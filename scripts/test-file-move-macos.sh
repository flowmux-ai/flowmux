#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Real cross-volume Cut, with cancellation/failure after the first source removal.
set -euo pipefail
cd "$(dirname "$0")/.."
test "$(uname -s)" = Darwin
fixture=$(mktemp -d /tmp/fm-move.XXXXXX)
mounted=false
cleanup() {
    if $mounted; then
        hdiutil detach "$fixture/mount" || return
    fi
    rm -rf "$fixture"
}
trap cleanup EXIT
cc -dynamiclib -Wall -Wextra -Werror scripts/test-file-move-failure.c -o "$fixture/failure.dylib"
hdiutil create -size 20m -fs HFS+ -volname FlowmuxMoveTest -type SPARSE -o "$fixture/volume.sparseimage"
hdiutil attach "$fixture/volume.sparseimage" -nobrowse -mountpoint "$fixture/mount"
mounted=true
cargo test -p flowmux --test macos_native --features native-smoke --locked --no-run \
    --message-format=json > "$fixture/build.jsonl"
binary=$(python3 - "$fixture/build.jsonl" <<'PY'
import json, sys
artifacts = [json.loads(line) for line in open(sys.argv[1])]
binaries = [row['executable'] for row in artifacts
            if row.get('reason') == 'compiler-artifact'
            and row.get('target', {}).get('name') == 'macos_native'
            and row.get('executable')]
assert len(binaries) == 1, binaries
print(binaries[0])
PY
)
FLOWMUX_FILE_MOVE_SMOKE_VOLUME="$fixture/mount" \
DYLD_INSERT_LIBRARIES="$fixture/failure.dylib" GSK_RENDERER=cairo "$binary"
