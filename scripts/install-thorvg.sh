#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build and install ThorVG with every image loader enabled and the C API
# exposed, which is what flowmux's image viewer links against.
#
# flowmux does NOT vendor ThorVG. Its hand-written `dlopen` shim loads the
# system ThorVG C API at runtime, so ThorVG must be installed first.
# Ubuntu (through 24.04) does not package ThorVG, so this script builds it
# from source with meson/ninja. ThorVG source is cloned into a temporary
# directory outside the repo and removed afterwards.
#
# Usage:
#   scripts/install-thorvg.sh              # build + install to /usr/local (sudo)
#   THORVG_VERSION=v1.0.6 scripts/install-thorvg.sh
#   PREFIX=$HOME/.local scripts/install-thorvg.sh   # no sudo

set -euo pipefail

# Match the C API version used by flowmux's hand-written bindings.
THORVG_VERSION="${THORVG_VERSION:-v1.0.6}"
PREFIX="${PREFIX:-/usr/local}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "error: '$1' not found; install it first" >&2; exit 1; }; }
need git
need meson
need ninja
need pkg-config

if ! command -v c++ >/dev/null 2>&1 && ! command -v g++ >/dev/null 2>&1; then
    echo "error: no C++ compiler found (install build-essential)" >&2
    exit 1
fi

# sudo only when installing into a prefix we cannot write to. Create a
# missing prefix first (e.g. a fresh $HOME/.local) so the -w test does not
# route a user-writable location through sudo and leave it root-owned.
SUDO=""
mkdir -p "$PREFIX" 2>/dev/null || true
if [ ! -w "$PREFIX" ]; then
    need sudo
    SUDO="sudo"
fi

src_dir="$(mktemp -d)"
trap 'rm -rf "$src_dir"' EXIT

echo "==> cloning ThorVG $THORVG_VERSION"
git clone --depth 1 --branch "$THORVG_VERSION" https://github.com/thorvg/thorvg.git "$src_dir"

echo "==> configuring (all loaders, C API, CPU/software engine)"
meson setup "$src_dir/build" "$src_dir" \
    --prefix="$PREFIX" \
    --libdir=lib \
    --buildtype=release \
    -Dloaders=all \
    -Dsavers=all \
    -Dbindings=capi \
    -Dengines=cpu \
    -Dtools="" \
    -Dtests=false

echo "==> building"
ninja -C "$src_dir/build"

echo "==> installing to $PREFIX"
$SUDO ninja -C "$src_dir/build" install

case "$PREFIX" in
    /usr|/usr/local)
        need ldconfig
        if [ "$(id -u)" -eq 0 ]; then
            ldconfig
        else
            need sudo
            sudo ldconfig
        fi
        ;;
esac

echo "==> verifying"
THORVG_PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:$PREFIX/lib/x86_64-linux-gnu/pkgconfig:${PKG_CONFIG_PATH:-}"
if ! PKG_CONFIG_PATH="$THORVG_PKG_CONFIG_PATH" pkg-config --exists thorvg-1; then
    echo "==> installed under $PREFIX, but pkg-config could not find thorvg-1." >&2
    echo "    Add its pkgconfig dir to PKG_CONFIG_PATH before building flowmux." >&2
    exit 1
fi

# Linking a C API call catches stale SONAME symlinks and ThorVG builds made
# without `-Dbindings=capi`; pkg-config alone reports both as installed.
printf '%s\n' '#include <thorvg_capi.h>' \
    'int main() {' \
    '  if (tvg_engine_init(0) != TVG_RESULT_SUCCESS) return 1;' \
    '  return tvg_engine_term() == TVG_RESULT_SUCCESS ? 0 : 1;' \
    '}' | c++ -x c++ - -o "$src_dir/verify-thorvg" \
        $(PKG_CONFIG_PATH="$THORVG_PKG_CONFIG_PATH" pkg-config --cflags --libs thorvg-1)
LD_LIBRARY_PATH="$PREFIX/lib:${LD_LIBRARY_PATH:-}" "$src_dir/verify-thorvg"

ver="$(PKG_CONFIG_PATH="$THORVG_PKG_CONFIG_PATH" pkg-config --modversion thorvg-1)"
echo "==> done. thorvg-1 $ver with C API installed under $PREFIX"
