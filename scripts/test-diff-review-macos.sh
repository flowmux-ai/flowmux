#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ $(uname -s) != Darwin ]]; then
    echo "This runner requires a macOS desktop for the native Diff UI checks." >&2
    exit 1
fi

cargo test -p flowmux-vcs --test review --locked
cargo test -p flowmux-state review_drafts --locked
cargo build -p flowmux-cli --bin flowmuxctl --locked
FLOWMUX_REVIEW_SMOKE_ONLY=1 \
FLOWMUX_BUNDLED_CLI_PATH="$PWD/target/debug/flowmuxctl" \
    cargo test -p flowmux --test macos_native --features native-smoke --locked
