#!/usr/bin/env python3
"""Pin the CLI formula and app cask to one published, stable ModelFS release.

Run against an already checked-out theGlenn/homebrew-tap. This script does not
clone, commit, push, install software, or read credentials. It prepares both
files before replacing either and preserves unrelated CLI formula content.
"""

import argparse
import os
from pathlib import Path
import re
import stat
import tempfile


RELEASES = "https://github.com/theGlenn/modelfs/releases/download"
STABLE_VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
TEMPLATE = Path(__file__).resolve().parent / "homebrew" / "modelfs.rb.template"


def stable_version(value):
    if re.fullmatch(STABLE_VERSION, value) is None:
        raise argparse.ArgumentTypeError("version must be a stable X.Y.Z release (no v prefix or prerelease suffix)")
    return value


def checksum(value):
    if re.fullmatch(r"[0-9a-fA-F]{64}", value) is None:
        raise argparse.ArgumentTypeError("checksum must contain exactly 64 hexadecimal characters")
    return value.lower()


def formula_field(source, name, replacement, required=True):
    # Only the top-level fields of this formula, not nested resources/bottles.
    pattern = re.compile(rf'^(  {name}[ \t]+")([^"\r\n]*)("[^\n]*)$', re.MULTILINE)
    matches = list(pattern.finditer(source))
    if not required and not matches:
        return source
    if len(matches) != 1:
        raise ValueError(f"Formula/modelfs.rb must contain exactly one top-level {name} field")
    return pattern.sub(lambda match: match[1] + replacement + match[3], source)


def updated_formula(source, version, cli_sha):
    if re.search(r"^class Modelfs < Formula\s*$", source, re.MULTILINE) is None:
        raise ValueError("Formula/modelfs.rb is not the expected Modelfs formula")
    url_pattern = re.compile(
        rf'^  url "{re.escape(RELEASES)}/v(?P<version>{STABLE_VERSION})/'
        rf'modelfs-(?P=version)-macos-universal\.tar\.gz"[^\n]*$',
        re.MULTILINE,
    )
    if len(url_pattern.findall(source)) != 1:
        raise ValueError("Formula/modelfs.rb must point at a known ModelFS CLI release asset")
    source = formula_field(source, "url", f"{RELEASES}/v{version}/modelfs-{version}-macos-universal.tar.gz")
    source = formula_field(source, "sha256", cli_sha)
    return formula_field(source, "version", version, required=False)


def checked_destination(tap, directory, filename, required=False):
    parent = tap / directory
    path = parent / filename
    if parent.is_symlink() or (parent.exists() and not parent.is_dir()):
        raise ValueError(f"{directory} must be a real directory in the tap")
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise ValueError(f"{directory}/{filename} must be a regular file, not a symlink")
    if required and not path.is_file():
        raise ValueError(f"{directory}/{filename} is missing from the tap")
    return path


def prepare_file(path, contents):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=f".{path.name}.", dir=path.parent)
    temporary = Path(temporary)
    try:
        with os.fdopen(descriptor, "wb") as output:
            mode = stat.S_IMODE(path.stat().st_mode) if path.exists() else 0o644
            os.fchmod(output.fileno(), mode)
            output.write(contents.encode("utf-8"))
            output.flush()
            os.fsync(output.fileno())
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise
    return temporary


def update_tap(tap, version, cli_sha, app_sha):
    tap = tap.resolve(strict=True)
    if not tap.is_dir():
        raise ValueError("tap path must be a directory")
    formula = checked_destination(tap, "Formula", "modelfs.rb", required=True)
    cask = checked_destination(tap, "Casks", "modelfs.rb")
    # read_bytes preserves CRLF and every byte outside the edited fields.
    source = formula.read_bytes().decode("utf-8")
    formula_contents = updated_formula(source, version, cli_sha)
    cask_contents = TEMPLATE.read_text(encoding="utf-8").replace("@VERSION@", version).replace("@APP_SHA256@", app_sha)
    prepared = []
    try:
        for destination, contents in [(formula, formula_contents), (cask, cask_contents)]:
            prepared.append((prepare_file(destination, contents), destination))
        for temporary, destination in prepared:
            os.replace(temporary, destination)
    finally:
        for temporary, _ in prepared:
            temporary.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tap", type=Path, required=True, help="checkout of theGlenn/homebrew-tap")
    parser.add_argument("--version", type=stable_version, required=True)
    parser.add_argument("--cli-sha256", type=checksum, required=True)
    parser.add_argument("--app-sha256", type=checksum, required=True)
    args = parser.parse_args()
    try:
        update_tap(args.tap, args.version, args.cli_sha256, args.app_sha256)
    except (OSError, UnicodeError, ValueError) as error:
        parser.exit(1, f"Homebrew update failed: {error}\n")
    print(f"Pinned CLI formula and app cask to ModelFS {args.version}")


if __name__ == "__main__":
    main()
