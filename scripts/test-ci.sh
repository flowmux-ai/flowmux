#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# The same gates run locally and in .github/workflows/test.yml.
set -euo pipefail
cd "$(dirname "$0")/.."

mode=${1:-}
case "$mode:$(uname -s)" in
    linux:Linux|macos:Darwin) ;;
    *) echo "Usage: $0 linux|macos (on the matching OS)" >&2; exit 2 ;;
esac

export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-"$PWD/target"}
mkdir -p "$CARGO_TARGET_DIR"
CARGO_TARGET_DIR=$(cd "$CARGO_TARGET_DIR" && pwd)
report_dir="$CARGO_TARGET_DIR/ci/$mode"
mkdir -p "$report_dir"
exec > >(tee "$report_dir/run.log") 2>&1

# Never inherit the invoking pane's socket, identity, or persistent state.
for variable in ${!FLOWMUX_@}; do unset "$variable"; done
test_root=$(mktemp -d /tmp/fm-ci.XXXXXX)
trap 'result=$?; echo "CI_GATE_EXIT=$result"; rm -rf "$test_root"' EXIT
export TMPDIR="$test_root/tmp" FLOWMUX_RUNTIME_DIR="$test_root/run"
export XDG_RUNTIME_DIR="$test_root/run" XDG_CONFIG_HOME="$test_root/config"
export XDG_STATE_HOME="$test_root/state" XDG_DATA_HOME="$test_root/data"
export XDG_CACHE_HOME="$test_root/cache"
mkdir -p "$TMPDIR" "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"
export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2} RUST_BACKTRACE=1 G_DEBUG=fatal-criticals

printf 'CI_GATE=%s\n' "$mode"
uname -a
rustc -Vv
cargo --version
pkg-config --modversion gtk4 libadwaita-1
python3 scripts/test-ci-runner.py
cargo fmt --all -- --check

if [[ $mode == linux ]]; then
    cargo llvm-cov --version
    rustup component add llvm-tools-preview
    export GDK_BACKEND=x11 GTK_A11Y=test LANG=C.UTF-8 LC_ALL=C.UTF-8
    locale
    # Keep a fixed virtual desktop. AppArmor is provisioned by the host/CI;
    # this runner does not disable the WebKit sandbox or change host policy.
    result=0
    xvfb-run -a -s '-screen 0 1280x1024x24' dbus-run-session -- \
        bash scripts/test-coverage.sh -- --nocapture || result=$?
    # Preserve failure evidence even when a test fails; never turn it green.
    mkdir -p "$CARGO_TARGET_DIR/llvm-cov"
    cargo llvm-cov report --json --summary-only \
        --output-path "$CARGO_TARGET_DIR/llvm-cov/summary.json" || result=1
    cargo llvm-cov report --html || result=1
    cargo llvm-cov report --fail-under-lines 79 --fail-under-regions 78 \
        --fail-under-functions 78 || result=1
    exit "$result"
fi

cargo build --workspace --locked
cargo test -p flowmux --bin flowmux --locked ipc_handler::tests
cargo test -p flowmux --bin flowmux --locked bridge::tests
export FLOWMUX_BUNDLED_CLI_PATH="$CARGO_TARGET_DIR/debug/flowmuxctl"
cargo test -p flowmux --test macos_native --features native-smoke --locked \
    2>&1 | tee "$report_dir/native-smoke.log"
grep -q '^MACOS_NATIVE_SMOKE_OK$' "$report_dir/native-smoke.log"
