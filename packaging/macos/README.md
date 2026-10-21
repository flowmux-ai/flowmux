<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux on macOS

Apple Silicon preview, tested on macOS 15.

## Install or update

```sh
curl -fsSL https://flowmux.org/install.sh | sh
```

The installer downloads the release DMG and asks to install missing Homebrew
and runtime libraries. If Flowmux is running, the update is applied after you
quit it. The app is installed in `~/Applications`, with CLI commands in
`~/.local/bin`.

For manual installation, install the runtime libraries with
`brew install gtk4 libadwaita vte3`, then drag **Flowmux.app → Applications**
from the DMG. [Build from source](../../docs/setup.md#build-from-source) if a
macOS release is not yet available.

## Agent setup

Run this in a Flowmux terminal tab, then restart your agents:

```sh
flowmux fix
```

For a manual DMG install, use
`/Applications/Flowmux.app/Contents/MacOS/flowmuxctl fix`.
Codex users approve changed hooks in `/hooks`.

To remove Flowmux, quit it and move the app to Trash; remove the
`~/.local/bin/flowmux` and `~/.local/bin/flowmuxctl` links if installed.

The preview is not notarized. Intel Macs and older macOS releases are not
validated. The optional ThorVG image backend is not included.
