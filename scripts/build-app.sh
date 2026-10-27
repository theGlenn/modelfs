#!/bin/bash
# Builds dist/ModelFS.app: the SwiftUI app with the modeld engine inside.
#
# Usage: scripts/build-app.sh [--release] [--universal] [--sign <identity>] [--keychain <path>]
#
#   --release     optimized build (default: debug)
#   --universal   Apple Silicon and Intel in one binary (default: this Mac's)
#   --sign ID     sign with a Developer ID identity (default: ad-hoc), with
#                 the hardened runtime and a secure timestamp; implies
#                 --release, since debug builds honor MODELD_BIN
#   --keychain P  use this signing keychain (or CODE_SIGN_KEYCHAIN)
#
# MODELFS_BUILD_NUMBER sets the numeric bundle build (default: 1 for local
# builds). Release automation uses its monotonically increasing run number.
# The full Cargo version, including prerelease labels, remains in ModelFSVersion.
#
# The engine sits at ModelFS.app/Contents/MacOS/modeld, so the app always
# runs the modeld it was built with. Needs Rust (rust-toolchain.toml) and
# Xcode or the Command Line Tools; nothing from Homebrew.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
profile=debug
universal=false
identity=-
keychain="${CODE_SIGN_KEYCHAIN:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) profile=release ;;
    --universal) universal=true ;;
    --sign) identity="${2:?--sign needs an identity}"; profile=release; shift ;;
    --keychain) keychain="${2:?--keychain needs a path}"; shift ;;
    -h | --help) sed -n '2,22p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

version="$(cargo metadata --no-deps --format-version 1 --locked --manifest-path "$root/Cargo.toml" \
  | python3 "$root/scripts/app/version.py" --cargo-metadata --field version)"
read -r short_version build_version <<< "$(python3 "$root/scripts/app/version.py" "$version")"
[[ -f "$root/scripts/app/ModelFS.icns" ]] || {
  echo "missing app icon: scripts/app/ModelFS.icns" >&2; exit 1;
}
# Keep the engine's load commands aligned with the app's declared support.
export MACOSX_DEPLOYMENT_TARGET=15.0

# 1. The engine.
# An explicit target dir: CARGO_TARGET_DIR must not send the build elsewhere
# while a stale modeld under target/ gets bundled.
cargo_flags=(build --locked -p modeld-cli --manifest-path "$root/Cargo.toml" --target-dir "$root/target")
[[ $profile == release ]] && cargo_flags+=(--release)
engine="$root/dist/engine/modeld"
mkdir -p "$(dirname "$engine")"
if $universal; then
  for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo "${cargo_flags[@]}" --target "$target"
  done
  lipo -create -output "$engine" \
    "$root/target/aarch64-apple-darwin/$profile/modeld" \
    "$root/target/x86_64-apple-darwin/$profile/modeld"
else
  cargo "${cargo_flags[@]}"
  cp "$root/target/$profile/modeld" "$engine"
fi

# 2. The app.
swift_flags=(-c "$profile" --package-path "$root/app")
$universal && swift_flags+=(--arch arm64 --arch x86_64)
swift build "${swift_flags[@]}"
app_binary="$(swift build "${swift_flags[@]}" --show-bin-path)/ModelFS"

# 3. The bundle.
app="$root/dist/ModelFS.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
sed -e "s/@VERSION@/$version/g" -e "s/@SHORT_VERSION@/$short_version/g" \
  -e "s/@BUILD_VERSION@/$build_version/g" -e "s/@CONFIGURATION@/$profile/g" \
  "$root/scripts/app/Info.plist" > "$app/Contents/Info.plist"
cp "$app_binary" "$app/Contents/MacOS/ModelFS"
cp "$root/scripts/app/ModelFS.icns" "$app/Contents/Resources/ModelFS.icns"

# 4. Signatures: nested code first, then the app that seals it.
sign=(codesign --force --sign "$identity")
[[ $identity != - ]] && sign+=(--options runtime --timestamp)
[[ -n $keychain ]] && sign+=(--keychain "$keychain")
# Sign the engine once. The CLI archive and bundle reuse these exact bytes.
"${sign[@]}" "$engine"
cp "$engine" "$app/Contents/MacOS/modeld"
"${sign[@]}" "$app"

# 5. Checks.
verify=(python3 "$root/scripts/app/verify.py" "$app" --engine "$engine")
$universal && verify+=(--universal)
[[ $identity != - ]] && verify+=(--developer-id)
"${verify[@]}"
"$app/Contents/MacOS/modeld" --version
echo "Built $app (version $version, $profile$($universal && echo ', universal' || true))"
