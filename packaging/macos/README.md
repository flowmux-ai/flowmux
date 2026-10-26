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

An installed app can also update from **Options → Update**. It verifies the
release DMG and replaces the app in place; running windows keep their current
version and the next launch uses the new release. No source build tools are
needed.

For manual installation, install the runtime libraries with
`brew install gtk4 libadwaita vte3`, then drag **Flowmux.app → Applications**
from the DMG. [Build from source](../../docs/setup.md#build-from-source) if a
macOS release is not yet available.

Markdown files open in a separate viewer from Files. It supports Mermaid,
live reload, zoom, and PNG export with `flowmux-md-viewer --render-png out.png README.md`.
The executable is also available inside `Flowmux.app/Contents/MacOS`.

Allow notifications when macOS asks. Notification Center opens the originating
pane, and the Dock badge follows the unread count. Desktop notifications require
the installed app; permission can be changed in System Settings → Notifications.

For Selenium automation, start `FLOWMUX_WEBDRIVER_SERVER=127.0.0.1:4444 flowmux`.
See [browser automation](../../docs/webkit-automation.md#macos) for supported commands.

## Agent setup

Run this in a Flowmux terminal tab, then restart your agents:

```sh
flowmux fix
```

For a manual DMG install, use
`/Applications/Flowmux.app/Contents/MacOS/flowmuxctl fix`.
Codex users approve changed hooks in `/hooks`.

Local workspaces restore after an app restart within the same macOS boot.
After a system reboot, their old live-session state expires, matching Linux.
Older saved state without a boot ID still restores on its first upgraded launch.

To remove Flowmux, quit it and move the app to Trash; remove the
`~/.local/bin/flowmux` and `~/.local/bin/flowmuxctl` links if installed.

The preview is not notarized. Intel Macs and older macOS releases are not
validated. The optional ThorVG image backend is not included.
