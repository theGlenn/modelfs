#!/usr/bin/env python3
"""Reject an unreleaseable checkout or missing credentials without printing secrets."""

import argparse
import base64
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib
import uuid


SIGNING_SECRETS = (
    "APPLE_DEVELOPER_ID_P12_BASE64",
    "APPLE_DEVELOPER_ID_P12_PASSWORD",
    "APPLE_DEVELOPER_ID_IDENTITY",
    "APPLE_NOTARY_KEY_P8",
    "APPLE_NOTARY_KEY_ID",
    "APPLE_NOTARY_ISSUER_ID",
)


def git(root, *args):
    result = subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True)
    if result.returncode:
        raise ValueError("Could not verify release checkout against origin/master and its tag.")
    return result.stdout.strip()


def validate_credentials(require_homebrew_token):
    names = SIGNING_SECRETS + (("HOMEBREW_TAP_TOKEN",) if require_homebrew_token else ())
    missing = [name for name in names if not os.environ.get(name, "").strip()]
    if missing:
        raise ValueError("Missing release secrets: " + ", ".join(missing))
    identity = os.environ["APPLE_DEVELOPER_ID_IDENTITY"]
    if not re.fullmatch(r"Developer ID Application: [^\r\n]+ \([A-Z0-9]{10}\)", identity):
        raise ValueError("APPLE_DEVELOPER_ID_IDENTITY must name a Developer ID Application certificate.")
    try:
        certificate = base64.b64decode("".join(os.environ["APPLE_DEVELOPER_ID_P12_BASE64"].split()), validate=True)
        if not certificate:
            raise ValueError
    except ValueError:
        raise ValueError("APPLE_DEVELOPER_ID_P12_BASE64 must be a base64-encoded certificate export.") from None
    key = os.environ["APPLE_NOTARY_KEY_P8"]
    if "-----BEGIN PRIVATE KEY-----" not in key or "-----END PRIVATE KEY-----" not in key:
        raise ValueError("APPLE_NOTARY_KEY_P8 must contain the complete App Store Connect API private key.")
    if not re.fullmatch(r"[A-Z0-9]{10}", os.environ["APPLE_NOTARY_KEY_ID"]):
        raise ValueError("APPLE_NOTARY_KEY_ID must be the API key's ten-character identifier.")
    try:
        uuid.UUID(os.environ["APPLE_NOTARY_ISSUER_ID"])
    except ValueError:
        raise ValueError("APPLE_NOTARY_ISSUER_ID must be the API key issuer UUID.") from None


def preflight(root, require_homebrew_token=False):
    validate_credentials(require_homebrew_token)
    with (root / "Cargo.toml").open("rb") as manifest:
        version = tomllib.load(manifest)["workspace"]["package"]["version"]
    tag = os.environ.get("TAG", "")
    if not re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", version) or tag != "v" + version:
        raise ValueError("Release tag must be vX.Y.Z and match the stable Cargo workspace version.")
    head = git(root, "rev-parse", "HEAD")
    if git(root, "rev-parse", f"refs/tags/{tag}^{{commit}}") != head:
        raise ValueError("Release tag does not point to this checkout.")
    master = git(root, "rev-parse", "refs/remotes/origin/master^{commit}")
    ancestor = subprocess.run(["git", "-C", str(root), "merge-base", "--is-ancestor", head, master])
    if ancestor.returncode:
        raise ValueError("Release checkout must already be merged into origin/master.")
    if git(root, "status", "--porcelain"):
        raise ValueError("Release checkout has uncommitted changes; build the exact tagged commit.")
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--require-homebrew-token", action="store_true")
    arguments = parser.parse_args()
    try:
        version = preflight(Path(__file__).resolve().parents[1], arguments.require_homebrew_token)
    except (ValueError, OSError, KeyError, tomllib.TOMLDecodeError) as error:
        print(f"Release preflight failed: {error}", file=sys.stderr)
        return 1
    if output := os.environ.get("GITHUB_OUTPUT"):
        with open(output, "a", encoding="utf-8") as stream:
            stream.write(f"version={version}\n")
    print(version)
    return 0


if __name__ == "__main__":
    sys.exit(main())
