#!/bin/bash
# Regenerates the checked-in macOS app icon from original vector artwork.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/modelfs-icon.XXXXXX")"
trap 'rm -rf "$work"' EXIT
swift "$root/scripts/app/render-icon.swift" "$work/ModelFS.iconset"
iconutil --convert icns --output "$root/scripts/app/ModelFS.icns" "$work/ModelFS.iconset"
