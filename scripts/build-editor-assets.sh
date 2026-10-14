#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Rebuild the vendored Monaco and Mermaid bundles. Normal flowmux builds use the committed
# output and do not require Node.js.

set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
editor_root="$repo_root/editor/flowmux-editor-web"

for command in node npm; do
    if ! command -v "$command" >/dev/null 2>&1; then
        echo "error: '$command' is required to rebuild editor assets" >&2
        exit 1
    fi
done

node_major=$(node -p 'Number(process.versions.node.split(".")[0])')
if [[ "$node_major" -lt 20 ]]; then
    echo "error: Node.js 20 or newer is required to rebuild editor assets" >&2
    exit 1
fi

cd "$editor_root"
npm ci
npm test
npm run build
npm run verify

echo "==> editor assets rebuilt under editor/flowmux-editor-web/dist"
