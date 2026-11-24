#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
set -euo pipefail

mirrors=${1:-/etc/apt/apt-mirrors.txt}
[[ -f "$mirrors" ]] || exit 0

# Keep the runner's regional mirrors available when the official archive fails.
fallbacks=$(awk '
    $1 ~ /^https?:\/\/archive\.ubuntu\.com\/ubuntu\/?$/ { next }
    $1 ~ /^https?:\/\// {
        sub(/[[:space:]]+priority:[0-9]+/, "")
        print $0 "\tpriority:2"
    }
' "$mirrors")
printf '%s\tpriority:1\n%s\n' 'https://archive.ubuntu.com/ubuntu/' "$fallbacks" > "$mirrors"
