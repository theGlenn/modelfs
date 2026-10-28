#!/bin/bash
# Verifies an explicitly local package without launching the app or modeld.
# Build first: scripts/build-app.sh --release --universal
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
app="${1:-$root/dist/ModelFS.app}"
python3 "$root/scripts/app/verify.py" "$app" --universal --engine "$root/dist/engine/modeld"
test_root="$(mktemp -d "${TMPDIR:-/tmp}/modelfs-package-test.XXXXXX")"
mounted=false
cleanup() {
  if $mounted; then hdiutil detach -quiet "$test_root/mounted"; fi
  rm -rf "$test_root"
}
trap cleanup EXIT
"$root/scripts/package-app.sh" --ad-hoc --app "$app" --output-dir "$test_root/output"
version="$(/usr/libexec/PlistBuddy -c 'Print :ModelFSVersion' "$app/Contents/Info.plist")"
dmg="$test_root/output/ModelFS-$version-universal-adhoc.dmg"
(cd "$test_root/output" && shasum -a 256 -c "$(basename "$dmg").sha256")
mkdir "$test_root/mounted"
hdiutil attach -quiet -readonly -nobrowse -noautoopen -mountpoint "$test_root/mounted" "$dmg"
mounted=true
[[ "$(readlink "$test_root/mounted/Applications")" == /Applications ]]
python3 - "$test_root/mounted" <<'PY'
from pathlib import Path
import sys

volume = Path(sys.argv[1])
for relative in [".", "ModelFS.app", "ModelFS.app/Contents", "ModelFS.app/Contents/MacOS/modeld"]:
    path = volume / relative
    assert path.stat().st_mode & 0o555 == 0o555, f"Other Mac accounts cannot open {relative}"
PY
python3 "$root/scripts/app/verify.py" "$test_root/mounted/ModelFS.app" --universal --engine "$root/dist/engine/modeld"
if "$root/scripts/package-app.sh" --release --sign "Developer ID Application: Smoke Test" --app "$app" --output-dir "$test_root/refused"; then
  echo "release packaging unexpectedly accepted an ad-hoc build" >&2; exit 1
fi
[[ ! -e "$test_root/refused" ]]
echo "Package smoke checks passed (read-only mounted image, identical universal engine)."
