#!/usr/bin/env python3
"""Translate Cargo SemVer into Apple's numeric bundle version fields."""

import argparse
import json
import os
import re
import sys


def bundle_versions(version, build_number="1"):
    """Keep prerelease identity separately; never put it in numeric plist fields."""
    match = re.fullmatch(
        r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
        r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
        r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?",
        version,
    )
    if not match:
        raise ValueError("Cargo version must be a complete SemVer such as 0.2.0-rc.1")
    prerelease = match.group(4)
    if prerelease and any(
        part.isdigit() and len(part) > 1 and part.startswith("0")
        for part in prerelease.split(".")
    ):
        raise ValueError("numeric SemVer prerelease components cannot have leading zeros")
    # A numeric CI sequence increments independently of the SemVer prerelease.
    # Use one component, which macOS interprets as [Major].0.0.
    if not re.fullmatch(r"[1-9][0-9]*", build_number):
        raise ValueError("MODELFS_BUILD_NUMBER must be a positive decimal integer")
    return ".".join(match.group(1, 2, 3)), build_number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", nargs="?")
    parser.add_argument("--cargo-metadata", action="store_true")
    parser.add_argument("--field", choices=["version", "short", "build"])
    parser.add_argument("--build-number", default=os.environ.get("MODELFS_BUILD_NUMBER", "1"))
    args = parser.parse_args()
    try:
        if args.cargo_metadata:
            if args.version:
                raise ValueError("pass a version or --cargo-metadata, not both")
            packages = json.load(sys.stdin)["packages"]
            versions = [package["version"] for package in packages if package["name"] == "modeld-cli"]
            if len(versions) != 1:
                raise ValueError("Cargo metadata must contain exactly one modeld-cli package")
            version = versions[0]
        else:
            version = args.version
        if not version:
            raise ValueError("a Cargo version is required")
        short, build = bundle_versions(version, args.build_number)
        if args.field:
            print({"version": version, "short": short, "build": build}[args.field])
        else:
            print(short, build)
    except (ValueError, KeyError, TypeError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    main()
