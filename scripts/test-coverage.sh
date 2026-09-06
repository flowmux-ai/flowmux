#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."

export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
cargo llvm-cov clean --workspace
eval "$(CARGO_TARGET_DIR="$PWD/target/llvm-cov-target" cargo llvm-cov show-env --export-prefix)"
# Use cargo test itself: llvm-cov's --tests omits the normal example binary
# needed by cross_process_lock, and also omits stable doctests.
cargo test --workspace --locked --target-dir "$CARGO_LLVM_COV_TARGET_DIR" "$@"
