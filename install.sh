#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build flowmux (fast profile: release optimization without LTO, for quicker
# local installs) and install the binaries, the .desktop entry and the app
# icons to the host, so flowmux shows up in the app launcher / dock.
#
# flowmux uses the system GTK4/libadwaita/WebKitGTK/VTE libraries. The image
# viewer discovers ThorVG at runtime with libloading, so the core app builds
# without it and enables image rendering when the shared library is present.
#
# Usage: ./install.sh [--check] [--yes]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$REPO_ROOT"

# Prefer rustup over an older distro Rust when both are installed.
if [ -d "$HOME/.cargo/bin" ]; then
    PATH="$HOME/.cargo/bin:$PATH"
    export PATH
fi

CHECK_ONLY=false
ASSUME_YES=false

usage() {
    sed -n '3,12p' "$0" >&2
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --check)
            CHECK_ONLY=true
            ;;
        -y|--yes)
            ASSUME_YES=true
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            exit 2
            ;;
    esac
    shift
done

confirm() {
    local answer
    if [ "$ASSUME_YES" = true ]; then
        return 0
    fi
    if [ ! -t 0 ]; then
        echo "error: dependency installation needs confirmation; rerun with --yes" >&2
        exit 1
    fi
    read -r -p "$1 [Y/n] " answer
    case "$answer" in
        ""|y|Y|yes|YES) ;;
        *) echo "==> installation cancelled"; exit 1 ;;
    esac
}

if [ "$(uname -s)" != "Linux" ]; then
    echo "error: install.sh is for Linux. On macOS, use scripts/install-macos.sh." >&2
    exit 1
fi

if ! command -v apt-get >/dev/null 2>&1 || ! command -v dpkg-query >/dev/null 2>&1; then
    echo "error: automatic dependency installation currently supports apt-based Linux." >&2
    echo "       Install the packages under 'Build prerequisites' in README.md first." >&2
    exit 1
fi

if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    if [ "${ID:-}" = ubuntu ] \
        && dpkg --compare-versions "${VERSION_ID:-0}" lt 24.04; then
        echo "error: Ubuntu ${VERSION_ID:-unknown} is below flowmux's native-build floor (24.04)." >&2
        echo "       Use the Flatpak build on Ubuntu 22.04." >&2
        exit 1
    fi
fi

REQUIRED_APT_PACKAGES=(
    ca-certificates curl git build-essential pkg-config
    libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev
    libwebkitgtk-6.0-dev libssl-dev libdbus-1-dev libsecret-1-dev
)
MISSING_APT_PACKAGES=()
for package in "${REQUIRED_APT_PACKAGES[@]}"; do
    if ! dpkg-query -W -f='${Status}' "$package" 2>/dev/null \
        | grep -q '^install ok installed$'; then
        MISSING_APT_PACKAGES+=("$package")
    fi
done

MSRV="$(sed -n 's/^rust-version = "\(.*\)"$/\1/p' Cargo.toml)"
rust_is_ready() {
    local tool version
    for tool in rustc cargo; do
        command -v "$tool" >/dev/null 2>&1 || return 1
        version="$($tool --version 2>/dev/null | awk '{print $2}')"
        [ -n "$version" ] || return 1
        [ "$(printf '%s\n' "$MSRV" "$version" | sort -V | head -n1)" = "$MSRV" ] \
            || return 1
    done
}

RUST_READY=true
rust_is_ready || RUST_READY=false

if [ "$CHECK_ONLY" = true ]; then
    status=0
    if [ "${#MISSING_APT_PACKAGES[@]}" -gt 0 ]; then
        echo "missing system packages: ${MISSING_APT_PACKAGES[*]}" >&2
        status=1
    fi
    if [ "$RUST_READY" = false ]; then
        echo "missing Rust toolchain: rustc/cargo $MSRV or newer" >&2
        status=1
    fi
    if [ "$status" -eq 0 ]; then
        echo "==> prerequisites OK"
    fi
    exit "$status"
fi

if [ "${#MISSING_APT_PACKAGES[@]}" -gt 0 ]; then
    echo "The following required system packages are missing:"
    printf '  %s\n' "${MISSING_APT_PACKAGES[@]}"
    confirm "Install them with apt now?"

    APT=(apt-get)
    if [ "$(id -u)" -ne 0 ]; then
        if ! command -v sudo >/dev/null 2>&1; then
            echo "error: sudo is required to install system packages" >&2
            exit 1
        fi
        APT=(sudo apt-get)
    fi
    "${APT[@]}" update
    "${APT[@]}" install -y --no-install-recommends "${MISSING_APT_PACKAGES[@]}"
fi

check_module() {
    local module="$1"
    local minimum="${2:-}"
    if ! pkg-config --exists "$module" \
        || { [ -n "$minimum" ] && ! pkg-config --atleast-version="$minimum" "$module"; }; then
        echo "error: required pkg-config module '$module${minimum:+ >= $minimum}' is unavailable." >&2
        echo "       Your distribution may be below flowmux's native-build floor." >&2
        exit 1
    fi
}

check_module gtk4 4.12
check_module libadwaita-1 1.5
check_module vte-2.91-gtk4 0.76
check_module webkitgtk-6.0
check_module openssl
check_module dbus-1
check_module libsecret-1

if [ "$RUST_READY" = false ]; then
    confirm "Install or update Rust $MSRV+ with rustup now?"
    if command -v rustup >/dev/null 2>&1; then
        rustup toolchain install stable --profile minimal
    else
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --profile minimal
    fi
    if [ -f "$HOME/.cargo/env" ]; then
        # shellcheck disable=SC1091
        . "$HOME/.cargo/env"
    fi
    rust_is_ready || {
        echo "error: rustup did not provide rustc/cargo $MSRV or newer" >&2
        exit 1
    }
fi

# ThorVG is an optional runtime dependency: flowmux builds and installs without
# it. If it is missing, everything works except the inline image viewer, which
# shows a "ThorVG is not installed" message until the library is present.
if ! ldconfig -p 2>/dev/null | grep -q 'libthorvg-1\.so' \
    && ! pkg-config --exists thorvg-1 2>/dev/null; then
    echo "note: ThorVG not detected — the image viewer will be disabled until" >&2
    echo "      you install it:" >&2
    echo "        sudo apt install libthorvg-dev   # where packaged (e.g. Debian)" >&2
    echo "        scripts/install-thorvg.sh        # build from source (Ubuntu)" >&2
fi

echo "==> building flowmux (fast profile)"
cargo build --profile fast -p flowmux -p flowmux-cli -p flowmux-md-viewer

# The first directory installed to is the one the .desktop entry points at, so
# launching from the dock runs the same binary a shell on PATH would.
install -d "$HOME/.local/bin"
PRIMARY_BIN_DIR=""
for dir in "$HOME/.local/bin" "$HOME/.cargo/bin"; do
    if [ -d "$dir" ]; then
        install -m755 \
            target/fast/flowmux \
            target/fast/flowmuxctl \
            target/fast/flowmux-md-viewer \
            "$dir/"
        echo "==> installed to $dir"
        [ -n "$PRIMARY_BIN_DIR" ] || PRIMARY_BIN_DIR="$dir"
    fi
done

# Desktop entry + icons. A .desktop file alone is not enough — the launcher
# resolves `Icon=com.flowmux.App` through the hicolor theme, so the PNG/SVG have
# to land under the matching per-size apps/ directories with that basename.
DATA_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
APP_ID="com.flowmux.App"

# `Exec=flowmux` relies on the session PATH, which does not always include
# ~/.local/bin (and never includes ~/.cargo/bin) for launcher-started apps.
# Rewrite it to the absolute path so the dock entry works regardless.
install -d "$DATA_DIR/applications"
sed "s|^Exec=flowmux$|Exec=$PRIMARY_BIN_DIR/flowmux|" \
    "resources/desktop/$APP_ID.desktop" \
    > "$DATA_DIR/applications/$APP_ID.desktop"
chmod 644 "$DATA_DIR/applications/$APP_ID.desktop"
echo "==> installed desktop entry to $DATA_DIR/applications/$APP_ID.desktop"

install -Dm644 resources/icons/flowmux.svg \
    "$DATA_DIR/icons/hicolor/scalable/apps/$APP_ID.svg"
for size in 16 24 32 48 64 96 128 256 512; do
    install -Dm644 "resources/icons/flowmux-${size}.png" \
        "$DATA_DIR/icons/hicolor/${size}x${size}/apps/$APP_ID.png"
done
echo "==> installed icons to $DATA_DIR/icons/hicolor"

# Refresh the launcher caches. Both tools are optional: GNOME rescans on login
# anyway, so a missing binary only delays the icon showing up.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$DATA_DIR/applications" 2>/dev/null || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -qtf "$DATA_DIR/icons/hicolor" 2>/dev/null || true
fi

echo "==> done. Fully restart the running flowmux GUI to pick up the new binary."
