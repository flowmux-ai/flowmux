<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux macOS preview

For Apple Silicon (arm64), tested on macOS 15. Intel Macs and older macOS
releases are not validated.

## Install

1. Install [Homebrew](https://brew.sh/) at `/opt/homebrew`, then run:

   ```sh
   brew install gtk4 libadwaita vte3
   ```

2. Quit any running Flowmux instance. Open the `.dmg` and drag
   **Flowmux.app → Applications**.
3. Eject the DMG and open Flowmux from Applications.

The DMG needs the Homebrew libraries above; you do not need to build Flowmux
or install Rust. This preview has no Apple Developer ID signing or
notarization, so macOS may block it.

## Agent setup

The DMG does not add commands to your shell's PATH. Enable agent hooks with:

```sh
"/Applications/Flowmux.app/Contents/MacOS/flowmuxctl" fix
```

Restart agent sessions after setup. Codex users must approve changed hooks in
`/hooks`. Both `flowmux` and `flowmuxctl` are in the app's `Contents/MacOS` folder.

## Update or remove

- **Update:** quit Flowmux, download the newer DMG, and replace the app in
  Applications. The in-app updater currently builds from source and needs Rust
  and build tools. Homebrew Cask updates are not available.
- **Remove:** quit Flowmux and move the app from Applications to Trash.

Homebrew library upgrades may require a newer Flowmux build. The optional
ThorVG image backend is not included.

To verify a download, place the DMG and its `.sha256` file together and run
(replace `VERSION` with the release version):

```sh
shasum -a 256 -c flowmux-VERSION-aarch64-apple-darwin.dmg.sha256
```

License texts and third-party notices are in `Flowmux.app/Contents/Resources`.
