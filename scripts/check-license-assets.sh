#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later

set -euo pipefail

notice="THIRD_PARTY_ASSET_NOTICES.md"
test -s "$notice"

for source in \
    github.com/catppuccin/catppuccin \
    github.com/dracula/dracula-theme \
    github.com/primer/github-vscode-theme \
    github.com/morhetz/gruvbox \
    github.com/nordtheme/nord \
    github.com/atom/one-dark-syntax \
    github.com/altercation/solarized \
    github.com/tokyo-night/tokyo-night-vscode-theme \
    github.com/ghostty-org/ghostty \
    github.com/chriskempson/tomorrow-theme \
    github.com/lobehub/lobe-icons \
    github.com/Aider-AI/aider
do
    grep -Fq "$source" "$notice" || {
        echo "missing asset attribution: $source" >&2
        exit 1
    }
done

for theme in crates/flowmux-config/themes/*.theme; do
    IFS= read -r header < "$theme"
    if [ "$header" != "# SPDX-License-Identifier: MIT" ]; then
        echo "missing MIT SPDX header: $theme" >&2
        exit 1
    fi
done

grep -Fq "SPDX-License-Identifier: GPL-3.0-or-later" resources/themes/example.theme
grep -Fq "Ghostty contributors" resources/themes/example.theme
grep -Fq "Chris Kempson" resources/themes/example.theme
grep -Fq "CC BY-SA 4.0" crates/flowmux-md-viewer/tests/fixtures/README.md
grep -Fq "0.29.0.gfm.13/test/spec.txt" crates/flowmux-md-viewer/tests/fixtures/README.md

sha256sum --check --status <<'EOF'
d431b29d97b6f73e69d547109cf5081578fac931e72afe95639ebe766c1b2a20  crates/flowmux-md-viewer/tests/fixtures/commonmark-0.31.2-spec.json
7d8e5814befec287ac116786d81ff14e0adc9b13295b4494649e995408fd871c  crates/flowmux-md-viewer/tests/fixtures/gfm-spec.txt
EOF
