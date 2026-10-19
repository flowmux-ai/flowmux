<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux macOS preview

This archive contains `Flowmux.app` for Apple Silicon (arm64), built and
tested on macOS 15. Intel Macs and older macOS releases are not validated.

This is an initial binary distribution, not a standalone or notarized app.
It requires Homebrew at `/opt/homebrew` and these runtime libraries:

```sh
brew install gtk4 libadwaita vte3
```

The app is ad-hoc signed, not signed with an Apple Developer ID or notarized.
Gatekeeper may block the downloaded app. Signing and notarization are pending;
this preview does not promise unattended installation. Homebrew library
upgrades may require a newer Flowmux build. The optional ThorVG image backend
is not included.

Verify the download with its adjacent checksum file:

```sh
shasum -a 256 -c flowmux-VERSION-aarch64-apple-darwin.zip.sha256
```

Quit an existing Flowmux instance before manually replacing its app bundle.
Copy `Flowmux.app` to your Applications folder. Both commands are included in
`Flowmux.app/Contents/MacOS`: `flowmux` and `flowmuxctl`. They are not
automatically added to your shell's PATH.

For this preview, update by downloading a later release and replacing the app
after quitting. The in-app updater currently uses the source installer and
requires Rust and build tools; it does not install this ZIP. Homebrew Cask
installation and updates are not yet available.

License texts and third-party notices are in `Flowmux.app/Contents/Resources`.
