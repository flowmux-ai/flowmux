<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Setup and troubleshooting

For the Ubuntu release installer, see [Install](../README.md#install).
Run source build commands and helper scripts from the repository root.

## Build from source

Prerequisites on Ubuntu 24.04+ (Rust stable, MSRV 1.93):

```bash
sudo apt install build-essential pkg-config git curl ca-certificates \
  libgtk-4-dev libadwaita-1-dev libvte-2.91-gtk4-dev libwebkitgtk-6.0-dev \
  libssl-dev libdbus-1-dev libsecret-1-dev
```

Install to the host (builds with the `fast` profile, then installs
`flowmux`, `flowmuxctl`, and `flowmux-md-viewer` to `~/.local/bin` and also
to `~/.cargo/bin` when that directory exists, plus the desktop entry and icons):

```bash
./install.sh
```

`install.sh` offers to install missing apt packages and the Rust toolchain.
It leaves agent settings unchanged; run `flowmux fix` to enable hooks.
Restart any running flowmux GUI to pick up the new binary.

For development:

The display-backed tests also need `xvfb` and a D-Bus session runner.
Install `dbus-x11`, `openssh-server`, `tmux`, and `python3-xlib` to run the
SSH and GUI integration checks used by CI.

```bash
cargo build --release --workspace   # binaries under target/release/
cargo run -p flowmux                # debug GUI
cargo check --workspace             # type-check everything
GDK_BACKEND=x11 GTK_A11Y=test G_DEBUG=fatal-criticals xvfb-run -a dbus-run-session -- cargo test --workspace --locked
scripts/check-ubuntu-compat.sh      # Docker smoke check for 24.04 / 26.04
```

`GDK_BACKEND=x11` keeps GTK on Xvfb even in a Wayland session;
`GTK_A11Y=test` enables the accessibility assertions without a desktop service.
`G_DEBUG=fatal-criticals` turns invalid GTK calls into test failures.

The Monaco editor bundle under `editor/flowmux-editor-web/dist` is committed,
so builds do not need Node.js. Only changes to the editor frontend need
Node.js 20+; rebuild with `scripts/build-editor-assets.sh` and commit the
updated `dist` directory and `package-lock.json` together.

### Platform scope

Release packaging and CI target Linux. macOS-specific WebKit/IME code and
[`install-macos.sh`](../scripts/install-macos.sh) are retained for development;
the script's preflight does not establish that all workspace dependencies
build on macOS. There is no native Windows build or installer in this tree.

## Optional runtime dependencies

### ThorVG (image viewer)

The image viewer loads ThorVG with `dlopen` at runtime. flowmux builds and
runs without it; the viewer shows a "ThorVG is unavailable" message until a
build with the C API and image loaders is present. The helper script builds
the version used by the bindings (needs `meson` and `ninja-build`):

```bash
sudo scripts/install-thorvg.sh                 # ThorVG v1.0.6 → /usr/local
PREFIX=$HOME/.local scripts/install-thorvg.sh  # no sudo
```

Restart flowmux afterwards. Other builds must expose the compatible ThorVG
C API and loaders; a package name alone does not establish compatibility.

### GStreamer (browser media)

WebKitGTK plays media through GStreamer. Without these plugins pages still
load, but video sites may stall or miss subtitles:

```bash
sudo apt install gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
  gstreamer1.0-plugins-ugly gstreamer1.0-libav
```

## Verify & repair

flowmux checks agent hooks, agent SKILL files, the browser data dir,
host-browser profile detection, and the daemon socket. Detection of a
host profile does not mean its session can be imported. Firefox cookie
extraction exists, but insertion into the embedded browser is not implemented;
Chromium-family encrypted cookie extraction is also unavailable.

```bash
flowmux doctor   # read-only audit; non-zero exit if anything needs fixing
flowmux fix      # install / refresh what doctor flagged
```

Both accept `--json`. `fix` is idempotent: hook entries without a flowmux
marker are preserved, and flowmux-managed SKILL copies are re-synced to the
version embedded in the binary. Restart running agent sessions afterwards so
they reload hook configuration.

Codex asks you to approve changed user hooks in `/hooks`; flowmux does not
bypass that. Codex configurations with `allow_managed_hooks_only = true`
cannot load these hooks, and `doctor` reports that policy.

### Troubleshooting

WebKitGTK's web-process sandbox is enabled by default. If opening a browser
or editor fails with a `bwrap` / `uid map` permission error on Ubuntu, check
the AppArmor audit log and allow user namespaces for the installed flowmux
executable through an application-specific profile. See Ubuntu's
[AppArmor documentation](https://documentation.ubuntu.com/security/security-features/privilege-restriction/apparmor/#apparmor-unprivileged-user-namespace-restrictions).
Do not disable AppArmor or user-namespace restrictions system-wide.

The `.deb` package installs and loads the profile. `install.sh` offers to
install it when the host restricts unprivileged namespaces, preserving any
existing administrator policy. For a manual source install on Ubuntu 24.04+,
install the supplied profile and restart flowmux:

```bash
sudo install -m 0644 packaging/apparmor/flowmux-webkit /etc/apparmor.d/flowmux-webkit
sudo apparmor_parser -r /etc/apparmor.d/flowmux-webkit
```

Release tarballs include the same file as `flowmux-webkit.apparmor`.

The profile covers `flowmux` and `flowmux-md-viewer` in `/usr/bin`,
`/usr/local/bin`, and `/home/*/.local/bin` or `/home/*/.cargo/bin`. For a
development binary or a custom install path, launch it explicitly in the profile:

```bash
aa-exec -p flowmux-webkit -- ./target/debug/flowmux
```

This permits namespace creation for that application while retaining WebKit's
web-process sandbox. Linux CI uses the same profile for its test process and
checks the running web process's PID namespace and seccomp filter.

For a temporary compatibility diagnosis only, WebKitGTK supports an explicit
per-launch opt-out:

```bash
WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 flowmux
```

This removes WebKitGTK's web-process isolation for that launch, including the
embedded editor. Use only trusted content, and remove the override after
resolving the host policy. flowmux never enables this override automatically.
This setting does not control macOS WKWebView.

- `FLOWMUX_LOG=debug` (or any `tracing` filter) raises console log verbosity.
- Daily log files are written under `$XDG_STATE_HOME/flowmux/logs`
  (usually `~/.local/state/flowmux/logs`); crash reports go to
  `$XDG_STATE_HOME/flowmux/crash`.
