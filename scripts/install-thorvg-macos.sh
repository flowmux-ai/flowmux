#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Optional viewer dependency, shared by source and downloaded-app installers.
set -euo pipefail
[ "$(uname -s)" = Darwin ] || { echo 'error: this helper requires macOS' >&2; exit 1; }
scripts=$(cd "$(dirname "$0")" && pwd)
if command -v python3 >/dev/null 2>&1 && python3 "$scripts/check-thorvg.py"; then
    echo '==> ThorVG image and Lottie support is ready'
    exit 0
fi
echo 'Image and Lottie (JSON) viewing requires ThorVG with compatible C API and loaders.'
if ! ( : </dev/tty ) 2>/dev/null; then
    echo '==> Image/Lottie viewer unavailable; rerun the installer in a terminal to install ThorVG.' >&2
    exit 0
fi
printf 'Install ThorVG v1.0.6 and its build tools into ~/.local for the image viewer? [Y/n] ' >/dev/tty
read -r answer </dev/tty || exit 1
case "$answer" in
    ''|y|Y|yes|YES) ;;
    *) echo '==> Skipping ThorVG; image/Lottie viewer unavailable.'; exit 0 ;;
esac
command -v brew >/dev/null 2>&1 || { echo 'error: install Homebrew, then rerun the installer' >&2; exit 1; }
if ! xcrun --find clang++ >/dev/null 2>&1; then
    xcode-select --install
    echo 'Finish installing Apple command-line tools, then rerun the installer.' >&2
    exit 1
fi
missing=()
for tool in meson ninja pkg-config python3 git; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        case "$tool" in python3) missing+=(python) ;; *) missing+=("$tool") ;; esac
    fi
done
if [ "${#missing[@]}" -gt 0 ]; then brew install "${missing[@]}"; fi
PREFIX="$HOME/.local" bash "$scripts/install-thorvg.sh"
python3 "$scripts/check-thorvg.py" || { echo 'error: installed ThorVG failed the image/Lottie compatibility check' >&2; exit 1; }
echo '==> ThorVG installed. Restart Flowmux to enable image/Lottie viewing.'
