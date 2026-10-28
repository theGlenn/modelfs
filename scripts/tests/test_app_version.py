#!/usr/bin/env python3
"""Release version inputs must not produce invalid Apple bundle fields."""

import importlib.util
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("app_version", Path(__file__).resolve().parents[1] / "app" / "version.py")
app_version = importlib.util.module_from_spec(spec)
spec.loader.exec_module(app_version)


class BundleVersions(unittest.TestCase):
    def test_prerelease_and_build_metadata_keep_numeric_apple_fields(self):
        self.assertEqual(app_version.bundle_versions("0.2.0-rc.1+build.25", "142"), ("0.2.0", "142"))
        self.assertEqual(app_version.bundle_versions("1.0.0"), ("1.0.0", "1"))
        self.assertEqual(app_version.bundle_versions("0.2.0-rc.1", "10000"), ("0.2.0", "10000"))

    def test_unsafe_or_ambiguous_versions_are_rejected(self):
        for version in ["v0.2.0", "0.2", "01.2.0", "0.2.0-rc.01", "0.2.0&bad", "0.2.0\n"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                app_version.bundle_versions(version)

    def test_build_numbers_follow_apples_numeric_constraints(self):
        for build in ["0", "01", "-1", "1-beta", "1.0.0", "1\n"]:
            with self.subTest(build=build), self.assertRaises(ValueError):
                app_version.bundle_versions("0.2.0-rc.1", build)


if __name__ == "__main__":
    unittest.main()
