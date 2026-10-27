"""Release gates and credential cleanup, using temporary repos and macOS stubs."""

import base64
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]
SECRETS = {
    "APPLE_DEVELOPER_ID_P12_BASE64": base64.b64encode(b"private-certificate").decode(),
    "APPLE_DEVELOPER_ID_P12_PASSWORD": "private-password",
    "APPLE_DEVELOPER_ID_IDENTITY": "Developer ID Application: Test (TESTTEAM01)",
    "APPLE_NOTARY_KEY_P8": "-----BEGIN PRIVATE KEY-----\nprivate-key\n-----END PRIVATE KEY-----",
    "APPLE_NOTARY_KEY_ID": "TESTKEY001",
    "APPLE_NOTARY_ISSUER_ID": "00000000-0000-0000-0000-000000000001",
}


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / "repo"
        self.root.mkdir()
        self.env = {key: value for key, value in os.environ.items() if key not in SECRETS}
        self.env.update(SECRETS)
        self.env.update({"GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_SYSTEM": os.devnull})
        (self.root / "scripts").mkdir()
        for name in ["release-preflight.py", "sign-and-notarize.sh"]:
            source = SCRIPTS / name
            if source.exists():
                shutil.copy(source, self.root / "scripts" / name)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.1.0"\n')
        for name in ["README.md", "LICENSE-MIT", "LICENSE-APACHE"]:
            (self.root / name).write_text(name)
        self.git("init", "-q")
        self.git("config", "maintenance.auto", "false")
        self.git("config", "gc.auto", "0")
        self.git("config", "core.fsmonitor", "false")
        self.git("add", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "initial")
        self.git("update-ref", "refs/remotes/origin/master", "HEAD")
        self.git("tag", "v0.1.0")
        self.env["TAG"] = "v0.1.0"
        self.env["RUNNER_TEMP"] = str(Path(self.temporary.name) / "runner")
        Path(self.env["RUNNER_TEMP"]).mkdir()
        self.env["GITHUB_OUTPUT"] = str(Path(self.temporary.name) / "outputs")

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.root), *args], env=self.env, check=True, capture_output=True, text=True).stdout.strip()

    def preflight(self, *args):
        return subprocess.run(
            ["python3", "scripts/release-preflight.py", *args], cwd=self.root,
            env=self.env, capture_output=True, text=True,
        )

    def test_matching_tag_on_master_passes_and_outputs_version(self):
        result = self.preflight()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "0.1.0")
        self.assertEqual(Path(self.env["GITHUB_OUTPUT"]).read_text(), "version=0.1.0\n")

    def test_missing_signing_secret_fails_without_revealing_credentials(self):
        self.env.pop("APPLE_NOTARY_KEY_P8")
        result = self.preflight()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("APPLE_NOTARY_KEY_P8", result.stderr)
        self.assert_private(result)

    def test_wrong_tag_and_prerelease_are_rejected(self):
        for tag in ["v0.2.0", "v0.1.0-rc.1", "v0.1.0;echo injected"]:
            with self.subTest(tag=tag):
                self.env["TAG"] = tag
                self.assertNotEqual(self.preflight().returncode, 0)

    def test_tag_must_point_at_the_checkout_and_be_merged_to_master(self):
        (self.root / "unmerged").write_text("new work")
        self.git("add", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "unmerged")
        self.assertIn("checkout", self.preflight().stderr)
        self.git("tag", "-f", "v0.1.0")
        self.assertIn("master", self.preflight().stderr)

    def test_tap_token_can_be_required_before_publication(self):
        self.env.pop("HOMEBREW_TAP_TOKEN", None)
        result = self.preflight("--require-homebrew-token")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("HOMEBREW_TAP_TOKEN", result.stderr)

    def test_untracked_swift_sources_cannot_enter_a_tagged_release(self):
        source = self.root / "app/Sources/ModelFS/Untracked.swift"
        source.parent.mkdir(parents=True)
        source.write_text("// SwiftPM includes this source even though the tag does not.\n")
        result = self.preflight()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("uncommitted changes", result.stderr)

    def assert_private(self, result):
        for value in [SECRETS["APPLE_DEVELOPER_ID_P12_BASE64"], SECRETS["APPLE_DEVELOPER_ID_P12_PASSWORD"], SECRETS["APPLE_NOTARY_KEY_P8"]]:
            self.assertNotIn(value, result.stdout + result.stderr)

    def install_stubs(self):
        bin_dir = Path(self.temporary.name) / "bin"
        bin_dir.mkdir()
        self.env["PATH"] = str(bin_dir) + os.pathsep + self.env["PATH"]
        self.env["RELEASE_TEST_LOG"] = str(Path(self.temporary.name) / "commands")
        stub = '''#!/usr/bin/env python3
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
mode = os.environ.get("RELEASE_TEST_MODE", "success")
stage = name + ":" + (args[0] if args else "")
if name == "xcrun":
    stage += ":" + args[1]
    if args[1] == "submit":
        stage += ":" + ("dmg" if args[2].endswith(".dmg") else "app")
with open(os.environ["RELEASE_TEST_LOG"], "a") as log:
    log.write(stage + "\\n")
if name in ["cargo", "swift", "build-app.sh"]:
    assert "APPLE_NOTARY_KEY_P8" not in os.environ, "notary key leaked to build"
    assert "APPLE_DEVELOPER_ID_P12_BASE64" not in os.environ, "certificate leaked to build"
    mask = os.umask(0)
    os.umask(mask)
    assert mask == 0o022, "restrictive credential permissions leaked to public build artifacts"
if name == "security":
    if args[0] == "list-keychains" and "-s" not in args:
        print('    "/Users/runner/Library/Keychains/login.keychain-db"')
    if args[0] == "create-keychain":
        pathlib.Path(args[-1]).touch()
    if args[0] == "delete-keychain":
        if mode == "cleanup-fails": sys.exit(1)
        pathlib.Path(args[-1]).unlink(missing_ok=True)
    if args[0] == "import" and mode == "import-fails": sys.exit(1)
    if args[0] == "find-identity":
        print('1) AABB "Developer ID Application: Test (TESTTEAM01)"')
elif name == "xcrun":
    if args[:2] == ["notarytool", "history"] and mode == "credentials-invalid": sys.exit(1)
    if args[:2] == ["notarytool", "submit"]:
        artifact = "dmg" if args[2].endswith(".dmg") else "app"
        print(json.dumps({"id": "00000000-0000-0000-0000-000000000001", "status": "Invalid" if mode == artifact + "-rejected" else "Accepted"}))
    if args[:2] == ["stapler", "staple"]:
        path = pathlib.Path(args[2])
        if path.suffix == ".app": (path / ".stapled").touch()
        else:
            with path.open("ab") as dmg: dmg.write(b"stapled-ticket")
elif name == "ditto":
    pathlib.Path(args[-1]).write_bytes(b"app-zip")
elif name == "build-app.sh":
    root = pathlib.Path(__file__).resolve().parents[1]
    macos = root / "dist/ModelFS.app/Contents/MacOS"
    macos.mkdir(parents=True)
    (macos / "ModelFS").write_bytes(b"signed-app")
    (macos / "modeld").write_bytes(b"exact-signed-engine")
elif name == "package-app.sh":
    root = pathlib.Path(__file__).resolve().parents[1]
    assert (root / "dist/ModelFS.app/.stapled").exists(), "app not stapled before packaging"
    (root / "dist/ModelFS-0.1.0-universal.dmg").write_bytes(b"signed-dmg")
'''
        for name in ["cargo", "swift", "security", "xcrun", "codesign", "ditto", "spctl"]:
            path = bin_dir / name
            path.write_text(stub)
            path.chmod(0o755)
        for name in ["build-app.sh", "package-app.sh"]:
            path = self.root / "scripts" / name
            path.write_text(stub)
            path.chmod(0o755)
        # The signing helper only accepts exact tagged sources, including the
        # fixture's fake build/package scripts.
        self.git("add", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "stub macOS commands")
        self.git("update-ref", "refs/remotes/origin/master", "HEAD")
        self.git("tag", "-f", "v0.1.0")

    def sign(self, mode="success"):
        self.env["RELEASE_TEST_MODE"] = mode
        return subprocess.run(
            ["bash", "scripts/sign-and-notarize.sh"], cwd=self.root,
            env=self.env, capture_output=True, text=True,
        )

    def test_signing_failure_always_removes_credentials_and_keychain(self):
        self.install_stubs()
        for mode in ["import-fails", "credentials-invalid", "app-rejected", "dmg-rejected"]:
            with self.subTest(mode=mode):
                shutil.rmtree(self.root / "dist", ignore_errors=True)
                result = self.sign(mode)
                self.assertNotEqual(result.returncode, 0, result.stdout)
                self.assertEqual(list(Path(self.env["RUNNER_TEMP"]).iterdir()), [])
                self.assertIn("security:delete-keychain", Path(self.env["RELEASE_TEST_LOG"]).read_text())
                self.assert_private(result)
                if mode.endswith("-rejected"):
                    self.assertIn("Notarization submission ID:", result.stderr)
                    self.assertIn("notarytool log", result.stderr)

    def test_cleanup_failure_prevents_publication_even_after_successful_packaging(self):
        self.install_stubs()
        result = self.sign("cleanup-fails")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("credential cleanup failed", result.stderr)
        self.assertEqual(list(Path(self.env["RUNNER_TEMP"]).iterdir()), [])
        self.assert_private(result)

    def test_public_artifacts_use_signed_embedded_engine_and_post_staple_dmg_hash(self):
        import hashlib
        import tarfile

        self.install_stubs()
        result = self.sign()
        self.assertEqual(result.returncode, 0, result.stderr)
        archive = self.root / "dist/modelfs-0.1.0-macos-universal.tar.gz"
        with tarfile.open(archive) as package:
            engine = package.extractfile("modelfs-0.1.0-macos-universal/modeld").read()
        self.assertEqual(engine, (self.root / "dist/ModelFS.app/Contents/MacOS/modeld").read_bytes())
        dmg = self.root / "dist/ModelFS-0.1.0-universal.dmg"
        self.assertTrue(dmg.read_bytes().endswith(b"stapled-ticket"))
        self.assertEqual(dmg.with_suffix(".dmg.sha256").read_text().split()[0], hashlib.sha256(dmg.read_bytes()).hexdigest())
        self.assertEqual(list(Path(self.env["RUNNER_TEMP"]).iterdir()), [])
        self.assert_private(result)
        commands = Path(self.env["RELEASE_TEST_LOG"]).read_text().splitlines()
        self.assertLess(commands.index("xcrun:notarytool:history"), commands.index("cargo:fmt"))
        self.assertLess(commands.index("xcrun:notarytool:submit:app"), commands.index("package-app.sh:--release"))
        self.assertLess(commands.index("package-app.sh:--release"), commands.index("xcrun:notarytool:submit:dmg"))


if __name__ == "__main__":
    unittest.main()
