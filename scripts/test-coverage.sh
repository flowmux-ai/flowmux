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
# Exercise SSH lifecycle code through the existing isolated live GUI suite.
# Keep the same instrumentation environment so its profiles join the unit tests.
cargo build --workspace --locked --target-dir "$CARGO_LLVM_COV_TARGET_DIR"
python3 scripts/ssh-workspace-fixture.py \
  --gui "$CARGO_LLVM_COV_TARGET_DIR/debug/flowmux" \
  --cli "$CARGO_LLVM_COV_TARGET_DIR/debug/flowmuxctl"
