#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."

case ${1:-true} in
    true|false) editor=${1:-true} ;;
    *) echo 'Usage: scripts/check-pre-push.sh [true|false (editor changed)]' >&2; exit 2 ;;
esac

echo 'pre-push: checking Rust formatting and all native targets'
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
echo 'pre-push: running headless workspace tests in isolated state'
python3 scripts/test-agent-unit.py --all

if $editor; then
    echo 'pre-push: testing and rebuilding editor assets'
    (
        cd editor/flowmux-editor-web
        npm ci
        npm run test:coverage
        npm run build
        npm run verify
    )
    if [[ -n $(git status --porcelain -- editor/flowmux-editor-web/dist) ]]; then
        echo 'pre-push: rebuilt editor assets differ; commit the regenerated dist files.' >&2
        exit 1
    fi
fi
