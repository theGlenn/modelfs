# Releasing ModelFS for macOS

A signed release contains a universal `ModelFS.app` in a drag-to-Applications
DMG, and a universal `modeld` CLI archive. Both contain the **same signed engine
bytes** so the app, CLI, and background service agree about which engine is
current. Apple Silicon and Intel are supported; the app requires macOS 15+.

The first app release needs a new version: the existing `v0.1.0` release only
contains the CLI. Do not replace an existing release's assets or move its tag.

## Configure release credentials

Create a **Developer ID Application** signing identity in your Apple Developer
account. An **Apple Development** identity cannot sign a public notarized
release. Export the Developer ID certificate **and its private key** together
as a password-protected `.p12` from Keychain Access.

Configure these repository Actions secrets through GitHub's settings or
`gh secret set`. Keep their values out of source control and chat:

| Secret | Value |
| --- | --- |
| `APPLE_DEVELOPER_ID_P12_BASE64` | Base64-encoded `.p12`, including the private key |
| `APPLE_DEVELOPER_ID_P12_PASSWORD` | Export password for that `.p12` |
| `APPLE_DEVELOPER_ID_IDENTITY` | Full `Developer ID Application: … (TEAMID)` identity |
| `APPLE_NOTARY_KEY_P8` | Contents of the App Store Connect API `.p8` key used for notarization |
| `APPLE_NOTARY_KEY_ID` | API key ID |
| `APPLE_NOTARY_ISSUER_ID` | Issuer ID associated with the API key |
| `HOMEBREW_TAP_TOKEN` | Fine-grained token with Contents read/write on `theGlenn/homebrew-tap` |

Use an App Store Connect API key with access to notarization. See Apple's
[notarization documentation](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution).
The release scripts use a temporary keychain and credential files, remove them
on exit, and do not persist signing credentials in the app or release assets.
Tests and compilation run in a separate job that never receives these secrets;
the signing job signs that job's binaries without compiling or running them.

All seven secrets are required by the release workflow's preflight, including
the Homebrew token for the separate tap update. Missing credentials fail before
compilation or publication.

If only the tap update fails, re-run just that failed job: it reuses the
published assets without rebuilding. Re-running the whole workflow stops at
preflight, because the tag already has a release. If a run stopped after
creating its draft release, delete the draft with `gh release delete vX.Y.Z`,
then re-run.

## Prepare and publish

1. Merge the release changes into `master` and wait for CI.
2. Bump the workspace `version` in `Cargo.toml` to a new stable version (for
   example, `0.2.0`), update `Cargo.lock`, and merge that change into `master`.
3. Tag that exact commit and push the matching tag:

   ```sh
   git switch master
   git pull --ff-only
   git tag v0.2.0
   git push origin v0.2.0
   ```

4. Watch the **Release** workflow. It validates the version, source commit,
   and that the tag has no release yet. A job without credentials runs the Rust
   and Swift tests and builds the unsigned universal app. The signing job then
   signs its executables, notarizes and staples the app, then creates, signs,
   notarizes, and staples the DMG. It verifies the final artifacts before
   publishing.
5. The release contains:

   ```text
   ModelFS-0.2.0-universal.dmg
   ModelFS-0.2.0-universal.dmg.sha256
   modelfs-0.2.0-macos-universal.tar.gz
   modelfs-0.2.0-macos-universal.tar.gz.sha256
   ```

6. The Homebrew job updates both `Formula/modelfs.rb` and `Casks/modelfs.rb`
   with the published URLs and final SHA-256 checksums.

The release workflow accepts stable `vX.Y.Z` tags. App bundle versions remain
numeric even for local prerelease builds; release builds use the workflow run
number as their increasing bundle build number. A checksum is generated only
after the final artifact's signature and stapled ticket are in place.

## Local packaging check

Build and inspect a test DMG without release credentials:

```sh
scripts/build-app.sh --release --universal
scripts/package-app.sh --ad-hoc
```

The output has an explicit `-adhoc` suffix. It exercises the app layout,
architecture, icon, version, and disk-image contents; it is **not** a notarized
public release. Publishing uses the signed release path and refuses an ad-hoc
app. See each script's `--help` for paths and signing options.

Regenerate the checked-in icon from its original vector artwork with:

```sh
scripts/generate-app-icon.sh
```

Before the first public release, test a downloaded, quarantined DMG on a fresh
Mac: drag the app into Applications, open it through Gatekeeper, scan a temporary
model folder, and turn the background service on and off. Check both Apple
Silicon and Intel, including upgrading a CLI-installed service while preserving
its folder settings. Local ad-hoc tests cannot replace this signed-download test.

## Install, update, and remove

Users can download the DMG from [GitHub Releases](https://github.com/theGlenn/modelfs/releases)
and drag ModelFS into Applications. After an app release updates the tap:

```sh
brew install --cask theGlenn/tap/modelfs     # Mac app
brew install --formula theGlenn/tap/modelfs  # Standalone modeld CLI
```

The app includes its engine and does not need the CLI formula. The cask installs
only the app; the formula owns the `modeld` command on PATH. Both use the same
store, configuration, lock, and per-user background service. They share the
name `modelfs`, so pass `--formula` or `--cask` rather than let Homebrew guess.
Both need macOS 15+: the CLI archive carries the app's engine.

Update by replacing the app from a new DMG, or:

```sh
brew upgrade --cask theGlenn/tap/modelfs
```

The background service has its own installed engine copy. After updating the
app, use **Background service → Update** when offered. Installing or upgrading
the app does not automatically enable a service or replace an existing service.

The service is shared with the CLI and can outlive the app. To stop using it,
choose **Background service → Turn off** before removing the app, or run
`modeld daemon uninstall`. Normal cask uninstall preserves that shared service;
explicit cask `--zap` removes the service. Neither uninstall
path deletes `~/.modeld` or provider model files.
