#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Package an existing app bundle as a drag-to-Applications disk image.
# Usage: bash scripts/package-macos.sh Flowmux.app output.dmg
set -euo pipefail

bundle="${1:?app bundle required}"
image="${2:?output DMG required}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
codesign --verify --deep --strict "$bundle"

stage=$(mktemp -d /tmp/fm-dmg.XXXXXX)
trap 'rm -rf "$stage"' EXIT
ditto "$bundle" "$stage/Flowmux.app"
cp "$repo_root/packaging/macos/README.md" "$stage/README.md"
ln -s /Applications "$stage/Applications"
mkdir -p "$(dirname "$image")"
hdiutil create -volname Flowmux -srcfolder "$stage" -fs HFS+ -format UDZO "$image"
hdiutil verify "$image"
(
    cd "$(dirname "$image")"
    filename="$(basename "$image")"
    shasum -a 256 "$filename" > "$filename.sha256"
    shasum -a 256 -c "$filename.sha256"
)
