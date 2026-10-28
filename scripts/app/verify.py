#!/usr/bin/env python3
"""Inspect a built app without launching it or reading any model directories."""

import argparse
from pathlib import Path
import plistlib
import re
import subprocess
import sys

sys.dont_write_bytecode = True
from version import bundle_versions


def command(*args):
    result = subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    if result.returncode:
        raise ValueError("{} failed:\n{}".format(" ".join(map(str, args)), result.stdout.strip()))
    return result.stdout


def check(condition, message):
    if not condition:
        raise ValueError(message)


def version_tuple(text):
    numbers = tuple(int(part) for part in text.split("."))
    return numbers + (0,) * (3 - len(numbers))


def verify(app, universal=False, developer_id=False, stapled=False, engine=None):
    contents = app / "Contents"
    with (contents / "Info.plist").open("rb") as file:
        info = plistlib.load(file)
    short, build = bundle_versions(info["ModelFSVersion"], info["CFBundleVersion"])
    check(info["CFBundleShortVersionString"] == short, "bundle short version must be the numeric Cargo version")
    check(info["CFBundleVersion"] == build, "invalid numeric bundle build version")
    check(info["CFBundleIdentifier"] == "dev.modeld.app", "unexpected app bundle identifier")
    check(info["CFBundleExecutable"] == "ModelFS", "unexpected app executable")
    check(version_tuple(info["LSMinimumSystemVersion"]) == (15, 0, 0), "app must declare macOS 15.0")
    check(info["CFBundleIconFile"] == "ModelFS.icns", "app must declare the ModelFS icon")
    icon = (contents / "Resources" / "ModelFS.icns").read_bytes()
    check(len(icon) >= 8 and icon[:4] == b"icns" and int.from_bytes(icon[4:8], "big") == len(icon), "invalid or missing ModelFS.icns")
    command("codesign", "--verify", "--deep", "--strict", str(app))
    teams = []
    for binary in (contents / "MacOS" / "ModelFS", contents / "MacOS" / "modeld"):
        command("codesign", "--verify", "--strict", str(binary))
        architectures = set(command("lipo", "-archs", str(binary)).split())
        if universal:
            check(architectures == {"arm64", "x86_64"}, "{} must contain arm64 and x86_64".format(binary))
        minimums = re.findall(r"^\s*minos\s+([0-9.]+)\s*$", command("otool", "-arch", "all", "-l", str(binary)), re.MULTILINE)
        check(len(minimums) == len(architectures) and all(version_tuple(value) == (15, 0, 0) for value in minimums), "{} must target macOS 15.0 in every architecture".format(binary))
        if developer_id:
            signature = command("codesign", "--display", "--verbose=4", str(binary))
            check("Authority=Developer ID Application:" in signature, "{} needs a Developer ID Application signature".format(binary))
            check(re.search(r"\bflags=[^\n]*\bruntime\b", signature), "{} must use the hardened runtime".format(binary))
            check(re.search(r"^Timestamp=.+", signature, re.MULTILINE), "{} needs a secure signing timestamp".format(binary))
            team = re.search(r"^TeamIdentifier=([A-Z0-9]+)$", signature, re.MULTILINE)
            check(team is not None and team.group(1) != "notset", "{} has no signing team".format(binary))
            teams.append(team.group(1))
    if developer_id:
        check(info["ModelFSBuildConfiguration"] == "release", "Developer ID builds must be optimized release builds")
        check(len(set(teams)) == 1, "app and engine must be signed by the same team")
    if engine:
        check((contents / "MacOS" / "modeld").read_bytes() == engine.read_bytes(), "CLI engine and bundled engine must be byte-identical")
    if stapled:
        check(developer_id, "ticket checks require a Developer ID build")
        command("xcrun", "stapler", "validate", str(app))
        command("spctl", "--assess", "--type", "execute", "--verbose=2", str(app))
    return info


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("app", type=Path)
    parser.add_argument("--universal", action="store_true")
    parser.add_argument("--developer-id", action="store_true")
    parser.add_argument("--stapled", action="store_true")
    parser.add_argument("--engine", type=Path)
    args = parser.parse_args()
    try:
        info = verify(args.app, args.universal, args.developer_id, args.stapled, args.engine)
        print("Verified ModelFS {} (macOS 15.0{})".format(info["ModelFSVersion"], ", universal" if args.universal else ""))
    except (OSError, KeyError, ValueError, plistlib.InvalidFileException) as error:
        parser.exit(1, "app verification failed: {}\n".format(error))


if __name__ == "__main__":
    main()
