#!/bin/bash
# Release gate: validate credentials, test, build, sign, notarize and package.
# Requires the six APPLE_* secrets documented in release-preflight.py and a
# clean checkout of a stable version tag already merged to origin/master.
# No publishing occurs here. Credentials live only in a temporary keychain
# and private files; every normal exit and interrupt removes them.
set +x
set -euo pipefail
umask 077

root="$(cd "$(dirname "$0")/.." && pwd)"
version="$(python3 "$root/scripts/release-preflight.py")"

# Copy credentials into non-exported shell variables. Cargo, Swift, and the
# app packaging scripts must never inherit private signing/notary material.
certificate_base64=$APPLE_DEVELOPER_ID_P12_BASE64
certificate_password=$APPLE_DEVELOPER_ID_P12_PASSWORD
identity=$APPLE_DEVELOPER_ID_IDENTITY
notary_key=$APPLE_NOTARY_KEY_P8
notary_key_id=$APPLE_NOTARY_KEY_ID
notary_issuer=$APPLE_NOTARY_ISSUER_ID
unset APPLE_DEVELOPER_ID_P12_BASE64 APPLE_DEVELOPER_ID_P12_PASSWORD APPLE_DEVELOPER_ID_IDENTITY
unset APPLE_NOTARY_KEY_P8 APPLE_NOTARY_KEY_ID APPLE_NOTARY_ISSUER_ID

# A failed inventory must stop before creating any keychain. The original
# search list is restored even though every signing call selects ours explicitly.
original_keychain_list="$(security list-keychains -d user)"
original_keychains=()
while IFS= read -r line; do
  path="$(printf '%s\n' "$line" | sed 's/^[[:space:]]*"//; s/"[[:space:]]*$//')"
  [[ -z $path ]] || original_keychains+=("$path")
done <<< "$original_keychain_list"
[[ ${#original_keychains[@]} -gt 0 ]] || { echo 'Could not read the original user keychain search list.' >&2; exit 1; }
work="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/modelfs-release.XXXXXX")"
keychain="$work/release.keychain-db"

cleanup() {
  status=$?
  trap - EXIT
  cleanup_status=0
  if [[ -e $keychain ]]; then
    security delete-keychain "$keychain" >/dev/null 2>&1 || cleanup_status=1
  fi
  security list-keychains -d user -s "${original_keychains[@]}" >/dev/null 2>&1 || cleanup_status=1
  rm -rf "$work" || cleanup_status=1
  if [[ $cleanup_status -ne 0 ]]; then
    echo 'Release credential cleanup failed; no release will be published.' >&2
    [[ $status -ne 0 ]] || status=1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

printf '%s' "$certificate_base64" | base64 --decode > "$work/certificate.p12"
printf '%s\n' "$notary_key" > "$work/AuthKey.p8"
unset certificate_base64 notary_key
keychain_password="$(openssl rand -base64 32)"
security create-keychain -p "$keychain_password" "$keychain" >/dev/null
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$work/certificate.p12" -k "$keychain" -P "$certificate_password" \
  -T /usr/bin/codesign -T /usr/bin/security >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s \
  -k "$keychain_password" "$keychain" >/dev/null
unset certificate_password keychain_password
rm "$work/certificate.p12"

identities="$(security find-identity -v -p codesigning "$keychain")"
if [[ $identities != *"\"$identity\""* ]]; then
  echo 'The imported keychain does not contain the requested valid Developer ID Application identity.' >&2
  exit 1
fi
notary_auth=(--key "$work/AuthKey.p8" --key-id "$notary_key_id" --issuer "$notary_issuer")
# Authenticate before doing any compilation, so invalid credentials fail early.
xcrun notarytool history "${notary_auth[@]}" --output-format json > "$work/history.json"

# Private files are already mode 600 inside the mode 700 work directory.
# Build artifacts must retain standard permissions for other Mac users.
umask 022
cargo fmt --all --check --manifest-path "$root/Cargo.toml"
cargo clippy --workspace --all-targets --locked --manifest-path "$root/Cargo.toml" -- -D warnings
cargo test --workspace --locked --manifest-path "$root/Cargo.toml"
cargo build --locked -p modeld-cli --manifest-path "$root/Cargo.toml" --target-dir "$root/target"
MODELD_BIN="$root/target/debug/modeld" swift test --package-path "$root/app"
"$root/scripts/build-app.sh" --release --universal --sign "$identity" --keychain "$keychain"

app="$root/dist/ModelFS.app"
engine="$app/Contents/MacOS/modeld"
dmg="$root/dist/ModelFS-$version-universal.dmg"
notarize() {
  local artifact=$1
  local result=$2
  if ! xcrun notarytool submit "$artifact" "${notary_auth[@]}" \
    --wait --timeout 45m --output-format json > "$result" \
    || ! python3 -c 'import json,sys; sys.exit(0 if json.load(open(sys.argv[1])).get("status") == "Accepted" else 1)' "$result"; then
    echo 'Apple notarization failed or was not accepted; no release will be published.' >&2
    # Print only a UUID, never an arbitrary service response or private key.
    python3 - "$result" <<'NOTARY_ID' >&2
import json, re, sys
try:
    identifier = json.load(open(sys.argv[1])).get("id", "")
    if isinstance(identifier, str) and re.fullmatch(r"[0-9a-fA-F-]{36}", identifier):
        print("Notarization submission ID: " + identifier)
except (OSError, ValueError):
    pass
print("Inspect the submission using xcrun notarytool log <submission-id> with your API key.")
NOTARY_ID
    return 1
  fi
}

ditto -c -k --keepParent "$app" "$work/ModelFS.zip"
notarize "$work/ModelFS.zip" "$work/notary-app.json"
xcrun stapler staple "$app"
xcrun stapler validate "$app"
codesign --verify --strict "$engine"
codesign --verify --deep --strict "$app"
spctl --assess --type execute --verbose=2 "$app"

"$root/scripts/package-app.sh" --release --sign "$identity" --keychain "$keychain"
notarize "$dmg" "$work/notary-dmg.json"
xcrun stapler staple "$dmg"
xcrun stapler validate "$dmg"
codesign --verify --strict "$dmg"
spctl --assess --type open --context context:primary-signature --verbose=2 "$dmg"

# Use the exact notarized nested engine for Homebrew's CLI. Never rebuild or
# re-sign this copy: Apple's ticket recognizes its existing code signature.
name="modelfs-$version-macos-universal"
mkdir "$work/$name"
cp "$engine" "$work/$name/modeld"
cmp "$engine" "$work/$name/modeld"
cp "$root/README.md" "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$work/$name/"
COPYFILE_DISABLE=1 tar -czf "$root/dist/$name.tar.gz" -C "$work" "$name"
# Stapling changes DMG bytes, so hashes are always calculated last.
(
  cd "$root/dist"
  shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256"
  shasum -a 256 "$(basename "$dmg")" > "$(basename "$dmg").sha256"
)
archive_sha="$(cut -d ' ' -f 1 "$root/dist/$name.tar.gz.sha256")"
dmg_sha="$(cut -d ' ' -f 1 "$dmg.sha256")"
if [[ -n ${GITHUB_OUTPUT:-} ]]; then
  {
    printf 'archive=%s.tar.gz\nsha256=%s\n' "$name" "$archive_sha"
    printf 'dmg=%s\ndmg_sha256=%s\n' "$(basename "$dmg")" "$dmg_sha"
  } >> "$GITHUB_OUTPUT"
fi
echo "Validated release artifacts: dist/$name.tar.gz and dist/$(basename "$dmg")"
