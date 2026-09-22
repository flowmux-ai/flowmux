#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail
cd "$(dirname "$0")/.."

mode=${1:-headless}
if (($#)); then shift; fi
case "$mode" in
    headless) packages=(); detect_leaks=1 ;;
    lottie) packages=(-p flowmux --bin flowmux lottie); detect_leaks=1 ;;
    gui) packages=(-p flowmux -p flowmux-md-viewer); detect_leaks=0 ;;
    gui-leaks) packages=(-p flowmux -p flowmux-md-viewer); detect_leaks=1 ;;
    *) echo "Usage: $0 [headless|lottie|gui|gui-leaks] [cargo test arguments...]" >&2; exit 2 ;;
esac

toolchain=nightly-2026-09-21
target=x86_64-unknown-linux-gnu
export CARGO_TARGET_DIR="$PWD/target/sanitizers"
export CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-2}
export CARGO_PROFILE_DEV_DEBUG=1 CARGO_PROFILE_TEST_DEBUG=1
export RUSTFLAGS='-Zsanitizer=address -Cforce-frame-pointers=yes'
export RUSTDOCFLAGS="$RUSTFLAGS"
export ASAN_OPTIONS="detect_leaks=$detect_leaks:halt_on_error=1:exitcode=99"
export DEBUGINFOD_URLS=
unset LSAN_OPTIONS CARGO_ENCODED_RUSTFLAGS CARGO_ENCODED_RUSTDOCFLAGS
export ASAN_SYMBOLIZER_PATH
ASAN_SYMBOLIZER_PATH=${ASAN_SYMBOLIZER_PATH:-$(command -v llvm-symbolizer || command -v llvm-symbolizer-18)}

# Use a private desktop/state even when launched from an existing flowmux pane.
for variable in ${!FLOWMUX_@}; do unset "$variable"; done
test_root=$(mktemp -d /tmp/flowmux-sanitizers.XXXXXX)
trap 'rm -rf "$test_root"' EXIT
export XDG_RUNTIME_DIR="$test_root/run" XDG_CONFIG_HOME="$test_root/config"
export XDG_STATE_HOME="$test_root/state" XDG_DATA_HOME="$test_root/data"
export XDG_CACHE_HOME="$test_root/cache"
mkdir -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_STATE_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
chmod 700 "$XDG_RUNTIME_DIR"

# Explicit --target keeps build scripts/proc macros out of the instrumented
# target graph; build-std instruments Rust's standard library as well.
command=(cargo +"$toolchain" test --locked -Zbuild-std --target "$target" "${packages[@]}" "$@")
if [[ "$mode" == gui* ]]; then
    export GDK_BACKEND=x11 GTK_A11Y=test G_DEBUG=fatal-criticals GSK_RENDERER=cairo
    xvfb-run -a dbus-run-session -- "${command[@]}"
else
    "${command[@]}"
fi
