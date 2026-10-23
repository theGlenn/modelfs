#!/bin/bash
# Builds dist/ModelFS.app: the SwiftUI app with the modeld engine inside.
#
# Usage: scripts/build-app.sh [--release] [--universal] [--sign <identity>]
#
#   --release     optimized build (default: debug)
#   --universal   Apple Silicon and Intel in one binary (default: this Mac's)
#   --sign ID     sign with a Developer ID identity (default: ad-hoc), with
#                 the hardened runtime and a secure timestamp; implies
#                 --release, since debug builds honor MODELD_BIN
#
# The engine sits at ModelFS.app/Contents/MacOS/modeld, so the app always
# runs the modeld it was built with. Needs Rust (rust-toolchain.toml) and
# Xcode or the Command Line Tools; nothing from Homebrew.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
profile=debug
universal=false
identity=-
while [[ $# -gt 0 ]]; do
  case "$1" in
    --release) profile=release ;;
    --universal) universal=true ;;
    --sign) identity="${2:?--sign needs an identity}"; profile=release; shift ;;
    -h | --help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)
[[ -n $version ]] || { echo "no version in Cargo.toml" >&2; exit 1; }

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
mkdir -p "$app/Contents/MacOS"
sed "s/@VERSION@/$version/g" "$root/scripts/app/Info.plist" > "$app/Contents/Info.plist"
cp "$app_binary" "$app/Contents/MacOS/ModelFS"
cp "$engine" "$app/Contents/MacOS/modeld"

# 4. Signatures: nested code first, then the app that seals it.
sign=(codesign --force --sign "$identity")
[[ $identity != - ]] && sign+=(--options runtime --timestamp)
"${sign[@]}" "$app/Contents/MacOS/modeld"
"${sign[@]}" "$app"

# 5. Checks.
codesign --verify --strict --verbose=1 "$app"
"$app/Contents/MacOS/modeld" --version
echo "Built $app (version $version, $profile$($universal && echo ', universal' || true))"
