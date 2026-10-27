#!/bin/bash
# Create a drag-to-Applications DMG from an existing universal ModelFS.app.
#
# Usage: scripts/package-app.sh (--ad-hoc | --release --sign ID)
#                              [--keychain PATH] [--app PATH] [--output-dir PATH]
#
# --release requires an optimized Developer ID app with a stapled notarization
# ticket. This script signs the DMG; release automation must then notarize and
# staple the DMG and regenerate its .sha256 after stapling changes the bytes.
# --ad-hoc is a local smoke package, named *-universal-adhoc.dmg.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
app="$root/dist/ModelFS.app"
output="$root/dist"
mode=
identity=-
keychain="${CODE_SIGN_KEYCHAIN:-}"
while [[ $# -gt 0 ]]; do
  case "$1" in
    --ad-hoc) [[ -z $mode ]] || { echo "choose exactly one packaging mode" >&2; exit 2; }; mode=adhoc ;;
    --release) [[ -z $mode ]] || { echo "choose exactly one packaging mode" >&2; exit 2; }; mode=release ;;
    --sign) identity="${2:?--sign needs an identity}"; shift ;;
    --keychain) keychain="${2:?--keychain needs a path}"; shift ;;
    --app) app="${2:?--app needs a path}"; shift ;;
    --output-dir) output="${2:?--output-dir needs a path}"; shift ;;
    -h | --help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done
[[ -n $mode ]] || { echo "choose --ad-hoc for a local smoke package or --release --sign ID" >&2; exit 2; }
verify=(python3 "$root/scripts/app/verify.py" "$app" --universal)
if [[ $mode == release ]]; then
  [[ $identity != - ]] || { echo "--release requires --sign with a Developer ID identity" >&2; exit 2; }
  verify+=(--developer-id --stapled)
elif [[ $identity != - ]]; then
  echo "--ad-hoc cannot use --sign; use --release for signed distribution" >&2; exit 2
fi
"${verify[@]}"
version="$(/usr/libexec/PlistBuddy -c 'Print :ModelFSVersion' "$app/Contents/Info.plist")"
name="ModelFS-$version-universal"
[[ $mode == adhoc ]] && name+="-adhoc"
mkdir -p "$output"
output="$(cd "$output" && pwd)"
dmg="$output/$name.dmg"
staging="$(mktemp -d "${TMPDIR:-/tmp}/modelfs-dmg.XXXXXX")"
trap 'rm -rf "$staging"' EXIT
# hdiutil preserves the source folder's permissions as the volume root.
# This is public app content, so other Mac accounts must be able to open it.
chmod 755 "$staging"
ditto "$app" "$staging/ModelFS.app"
ln -s /Applications "$staging/Applications"
hdiutil create -quiet -volname ModelFS -srcfolder "$staging" -format UDZO -fs HFS+ -ov "$dmg"
sign=(codesign --force --sign "$identity")
[[ $identity != - ]] && sign+=(--timestamp)
[[ -n $keychain ]] && sign+=(--keychain "$keychain")
"${sign[@]}" "$dmg"
codesign --verify --strict "$dmg"
if [[ $mode == release ]]; then
  # A valid signature alone can be ad-hoc or Apple Development. Public disk
  # images must use timestamped Developer ID code from the app's own team.
  dmg_signature="$(codesign --display --verbose=4 "$dmg" 2>&1)"
  dmg_team="$(printf '%s\n' "$dmg_signature" | sed -n 's/^TeamIdentifier=//p')"
  app_team="$(codesign --display --verbose=4 "$app" 2>&1 | sed -n 's/^TeamIdentifier=//p')"
  timestamp="$(printf '%s\n' "$dmg_signature" | sed -n 's/^Timestamp=//p')"
  if [[ $dmg_signature != *"Authority=Developer ID Application:"* || -z $timestamp || $timestamp == none ]]; then
    echo "release DMG needs a Developer ID Application signature and secure timestamp" >&2; exit 1
  fi
  if [[ ! $dmg_team =~ ^[A-Z0-9]{10}$ || $dmg_team != "$app_team" ]]; then
    echo "release DMG and app must be signed by the same Developer ID team" >&2; exit 1
  fi
fi
(cd "$output" && shasum -a 256 "$name.dmg" > "$name.dmg.sha256")
echo "Packaged $dmg"
