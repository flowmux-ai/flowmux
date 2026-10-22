#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Instrument the native dependency as well as its Flowmux-style C API caller.
set -euo pipefail
scripts=$(cd "$(dirname "$0")" && pwd)
cd "$scripts/.."
mkdir -p target
work=$(mktemp -d "$PWD/target/thorvg-sanitizers.XXXXXX")
echo "ThorVG sanitizer artifacts: $work"
git clone --depth 1 --branch v1.0.6 https://github.com/thorvg/thorvg.git "$work/source"
git -C "$work/source" apply "$scripts/patches/thorvg-1.0.6-object-base-type.patch"
export CC=clang-18 CXX=clang++-18 DEBUGINFOD_URLS=
export ASAN_OPTIONS=detect_leaks=1:halt_on_error=1:exitcode=99
export UBSAN_OPTIONS=halt_on_error=1:print_stacktrace=1
meson setup "$work/build" "$work/source" --buildtype=debugoptimized \
    -Db_sanitize=address,undefined -Db_lundef=false -Dloaders=all -Dsavers=all \
    -Dbindings=capi -Dengines=cpu -Dtools= -Dtests=true -Dextra=lottie_exp
ninja -C "$work/build" -j2
meson test -C "$work/build" --print-errorlogs --timeout-multiplier 3
"$CXX" -g -fsanitize=address,undefined "$scripts/test-thorvg-sanitizers.cpp" \
    -I"$work/source/src/bindings/capi" -L"$work/build/src" \
    -Wl,-rpath,"$work/build/src" -lthorvg-1 -o "$work/lottie-capi"
# test5.lot includes expressions that exercise JerryScript function object types.
for fixture in "$work/source"/test/resources/test{3..12}.lot; do
    "$work/lottie-capi" "$fixture"
done
