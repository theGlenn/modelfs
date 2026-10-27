"""Release-to-tap checks, isolated from the user's Homebrew installation."""

from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "update-homebrew.py"
OLD_SHA = "30b81ceaf578f9ad8b8107261159af4993a0171dde0f4ebe98e432821a58702b"
CLI_SHA = "a" * 64
APP_SHA = "b" * 64
OLD_URL = (
    "https://github.com/theGlenn/modelfs/releases/download/v0.1.0/"
    "modelfs-0.1.0-macos-universal.tar.gz"
)
FORMULA = f'''class Modelfs < Formula
  desc "Keep one copy of every local AI model, however many apps use it"
  homepage "https://glennso.dev/modelfs"
  url "{OLD_URL}"
  sha256 "{OLD_SHA}"
  license any_of: ["MIT", "Apache-2.0"]

  depends_on :macos

  def install
    bin.install "modeld"
  end

  def caveats
    <<~EOS
      Existing CLI instructions stay here.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/modeld --version")
  end
end
'''


class UpdateHomebrewTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="modelfs-tap-test-")
        self.addCleanup(self.directory.cleanup)
        self.tap = Path(self.directory.name)
        self.formula = self.tap / "Formula" / "modelfs.rb"
        self.formula.parent.mkdir()
        self.formula.write_text(FORMULA)

    def run_helper(self, version="0.2.0", cli_sha=CLI_SHA, app_sha=APP_SHA):
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--tap",
                str(self.tap),
                "--version",
                version,
                "--cli-sha256",
                cli_sha,
                "--app-sha256",
                app_sha,
            ],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_updates_cli_and_creates_app_only_cask_for_same_release(self):
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.formula.read_text(),
            FORMULA.replace(OLD_URL, OLD_URL.replace("0.1.0", "0.2.0"))
            .replace(OLD_SHA, CLI_SHA)
            .replace("depends_on :macos", "depends_on macos: :sequoia"),
        )
        cask = (self.tap / "Casks" / "modelfs.rb").read_text()
        self.assertIn('version "0.2.0"', cask)
        self.assertIn(f'sha256 "{APP_SHA}"', cask)
        self.assertIn('ModelFS-#{version}-universal.dmg', cask)
        self.assertIn('depends_on macos: :sequoia', cask)
        self.assertIn('app "ModelFS.app"', cask)
        self.assertIn('uninstall quit: "dev.modeld.app"', cask)
        self.assertIn('zap launchctl: "dev.modeld.daemon"', cask)
        self.assertNotIn("uninstall launchctl:", cask)
        self.assertNotIn("binary ", cask)
        self.assertNotIn("postflight", cask)
        self.assertNotIn("delete:", cask)
        self.assertNotIn("trash:", cask)
        self.assertNotIn("daemon install", cask)
        self.assertIn("Models and ~/.modeld are preserved", cask)

    def test_repeated_updates_are_identical_and_keep_other_tap_files(self):
        casks = self.tap / "Casks"
        casks.mkdir()
        unrelated = casks / "another-app.rb"
        unrelated.write_text("leave this cask unchanged\n")
        self.assertEqual(self.run_helper().returncode, 0)
        first = {path: path.read_bytes() for path in [self.formula, casks / "modelfs.rb"]}
        self.assertEqual(self.run_helper().returncode, 0)
        self.assertEqual(first, {path: path.read_bytes() for path in first})
        self.assertEqual(unrelated.read_text(), "leave this cask unchanged\n")

    def test_invalid_release_inputs_do_not_change_tap(self):
        for version, cli_sha, app_sha in [
            ("v0.2.0", CLI_SHA, APP_SHA),
            ("0.2.0-beta.1", CLI_SHA, APP_SHA),
            ("0.2", CLI_SHA, APP_SHA),
            ("01.2.3", CLI_SHA, APP_SHA),
            ('0.2.0"; system("false")', CLI_SHA, APP_SHA),
            ("0.2.0", "not-a-checksum", APP_SHA),
            ("0.2.0", CLI_SHA, "a" * 63),
        ]:
            with self.subTest(version=version, cli_sha=cli_sha, app_sha=app_sha):
                result = self.run_helper(version, cli_sha, app_sha)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.formula.read_text(), FORMULA)
                self.assertFalse((self.tap / "Casks").exists())

    def test_unexpected_formula_is_refused_before_any_write(self):
        for source in [
            FORMULA.replace(OLD_URL, "https://example.com/modeld.tar.gz"),
            FORMULA.replace("  sha256", "  # sha256"),
            FORMULA.replace("class Modelfs < Formula", "class Other < Formula"),
            FORMULA.replace(f'  sha256 "{OLD_SHA}"', f'  sha256 "{OLD_SHA}"\n  sha256 "{OLD_SHA}"'),
            FORMULA.replace("  depends_on :macos\n", ""),
            FORMULA.replace("  depends_on :macos", "  depends_on :macos\n  depends_on macos: :sonoma"),
        ]:
            with self.subTest(source=source):
                self.formula.write_text(source)
                result = self.run_helper()
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.formula.read_text(), source)
                self.assertFalse((self.tap / "Casks").exists())

    def test_existing_explicit_formula_version_and_line_endings_are_preserved(self):
        source = FORMULA.replace('  sha256', '  version "0.1.0" # existing version\n  sha256')
        self.formula.write_bytes(source.replace("\n", "\r\n").encode())
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        expected = source.replace(OLD_URL, OLD_URL.replace("0.1.0", "0.2.0"))
        expected = expected.replace(OLD_SHA, CLI_SHA).replace('version "0.1.0"', 'version "0.2.0"')
        expected = expected.replace("depends_on :macos", "depends_on macos: :sequoia")
        self.assertEqual(self.formula.read_bytes(), expected.replace("\n", "\r\n").encode())

    def test_formula_requires_the_engines_macos_version(self):
        self.formula.write_text(FORMULA.replace("depends_on :macos", 'depends_on macos: ">= :ventura"'))
        self.assertEqual(self.run_helper().returncode, 0)
        formula = self.formula.read_text()
        self.assertIn("  depends_on macos: :sequoia\n", formula)
        self.assertNotIn("ventura", formula)

    def test_symlinked_destination_is_refused_without_following_it(self):
        outside = self.tap / "outside.rb"
        outside.write_text("not a cask\n")
        casks = self.tap / "Casks"
        casks.mkdir()
        (casks / "modelfs.rb").symlink_to(outside)
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(outside.read_text(), "not a cask\n")
        self.assertEqual(self.formula.read_text(), FORMULA)


if __name__ == "__main__":
    unittest.main()
