<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Setup and troubleshooting

Flowmux supports Linux and macOS. See [Install](../README.md#install) for
Ubuntu packages and the [macOS guide](../packaging/macos/README.md) for the
Apple Silicon DMG, runtime dependencies, and updates.
Run source build commands and helper scripts from the repository root.

## Build from source

On Ubuntu 24.04+ or macOS (Apple Silicon), run from the repository root:

```sh
./install.sh
```

The installer checks dependencies and asks to install missing packages and
Rust. On macOS, it also offers Homebrew and Apple's command-line tools;
if the Apple installer opens, finish it and rerun `./install.sh`.
Press Enter at `[Y/n]` to install, or type `n` to cancel.
Use `--check` to check dependencies without installing.

Ubuntu installs binaries to `~/.local/bin` (and `~/.cargo/bin` when present),
plus desktop entries and icons. macOS installs `~/Applications/Flowmux.app`
and CLI commands in `~/.local/bin`. You can run `./install.sh` from a Flowmux
terminal while the app is running. The new version is installed immediately;
existing sessions keep running their current version until you quit and reopen
the app. The installer prints the installed executable's version and retains
the previous app in a hidden `.Flowmux.app.previous.*` directory beside the app.
These backups can be removed after all sessions using them have exited.
Use `./install.sh --release --launch` for a macOS release build.

To enable agent hooks, run `flowmux fix` and restart agent sessions.
Codex users approve changed hooks in `/hooks`.

### Development checks

On Linux, the display-backed tests also need `xvfb` and a D-Bus session runner.
Install `dbus-x11`, `openssh-server`, `tmux`, and `python3-xlib` to run the
SSH and GUI integration checks used by CI. The SSH autofill check also uses
`gnome-keyring`, `python3-gi`, `gir1.2-atspi-2.0`, `at-spi2-core`, and
`python3-pil`; it starts a password-protected keyring on a private D-Bus session.
Browser recording and its integration check need `ffmpeg` and `ffprobe` with
the H.264 encoder: `sudo apt install ffmpeg` on Ubuntu or `brew install ffmpeg`
on macOS. Other browser controls do not require these tools.

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

Release packaging provides Linux packages and an Apple Silicon macOS preview
DMG with a SHA-256 sidecar. The macOS app requires Homebrew runtime libraries
and is ad-hoc signed, not notarized; see the
[macOS package README](../packaging/macos/README.md) for installation and update
limitations. CI builds the workspace on macOS 15
with Homebrew `pkg-config`, `gtk4`, `libadwaita`, and `vte3`, and runs an
isolated main-thread GTK/WKWebView smoke test. It checks browser state,
native focus, dirty-editor close cancellation, and persistence failure/retry.

Run the same complete gate as CI locally:

```sh
bash scripts/test-ci.sh macos
```

On Ubuntu 24.04, install `cargo-llvm-cov` 0.6.24 and the dependencies
listed in `.github/workflows/test.yml`, including the ThorVG backend and
WebKit AppArmor profile, then run:

```sh
cargo install cargo-llvm-cov --version 0.6.24 --locked
aa-exec -p flowmux-webkit -- bash scripts/test-ci.sh linux
```

Both gates use the pinned repository Rust toolchain, private state/runtime
directories, and fatal GTK criticals. Logs and environment versions are in
`target/ci/{linux,macos}`; Linux coverage is in `target/llvm-cov`.
`CARGO_TARGET_DIR` can relocate both build output and evidence. The Linux
gate includes instrumented workspace tests, the live SSH fixture, and the
same coverage floors as CI.

The smoke executable owns temporary state/configuration and its own window;
it does not restart an installed Flowmux. A logged-in macOS desktop is
required. [`install-macos.sh`](../scripts/install-macos.sh) builds a local
application bundle. Use `./install.sh --release --bundle-only` on macOS to
build `target/macos-bundle/Flowmux.app` with the locked dependencies without
installing, launching, or refreshing agent wrappers. Package the bundle with
`bash scripts/package-macos.sh target/macos-bundle/Flowmux.app dist/Flowmux.dmg`.
The disk image contains the app and an Applications shortcut for drag-and-drop
installation; see the [macOS distribution notes](../packaging/macos/README.md)
for runtime prerequisites. The Release workflow creates this DMG only after
the macOS gate passes and publishes the release
only after both Linux and macOS builds succeed.
There is no native Windows build or installer in this tree.

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

The v1.0.6 helper applies a JerryScript enum-range fix needed by Lottie
expressions when built as C++. Existing source installations need to rerun the
helper to receive it; the compatibility probe checks the API and loaders, not
this patch. System packages need an equivalent upstream or distribution fix.
Native regression checks are available with
`bash scripts/test-thorvg-sanitizers.sh` (Clang 18, Meson, Ninja and pkg-config).

Raster previews have a 64 MiB decoding budget. Images exceeding it show an error
and can be opened in the default application explicitly. The limit applies to
decoded pixels and decoder buffers, not the compressed file size.

Restart flowmux afterwards. Other builds must expose the compatible ThorVG
C API and loaders; a package name alone does not establish compatibility.

On macOS, the installer checks compatibility and offers to build ThorVG with a
`Y/n` prompt. Accept to enable images and Lottie (`.json`/`.lottie`); decline to
install Flowmux without the viewer. The library goes in `~/.local/lib`.
Ordinary JSON files still open in the editor.

### GStreamer (Linux browser media)

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
flowmux hooks refresh-shims  # update only wrappers already managed by Flowmux
```

`doctor` and `fix` accept `--json`. `fix` is idempotent: hook entries without a flowmux
marker are preserved, and flowmux-managed SKILL copies are re-synced to the
version embedded in the binary after backing up differing content. Symlinked
skills remain user-managed. See [skill management](agent-skills.md) for target
paths, skill-only installation/removal, backups, and agent activation controls.
Reload hook configuration at a convenient session boundary; do not interrupt
active agent sessions just to apply an update.

The Linux/macOS source installers refresh existing Flowmux agent wrappers,
including legacy wrappers under `~/.local/bin`, without installing missing
integrations or changing provider hooks/settings. Binary-only updates can run
`flowmux hooks refresh-shims` explicitly. Changes apply to new agent processes;
a new chat inside an existing Codex TUI still uses that TUI's connection.

Codex asks you to approve changed user hooks in `/hooks`; flowmux does not
bypass that. Codex configurations with `allow_managed_hooks_only = true`
cannot load these hooks, and `doctor` reports that policy.

### Troubleshooting

Each GUI/daemon has a regular socket (64 connections) and a companion
`<socket>.ctl` endpoint (16 connections) reserved for native agent hooks,
notifications and status reads. The CLI selects the companion automatically;
older clients can still use the regular socket. Browser operations stay on the
regular socket so long browser waits cannot exhaust the hook reserve. Each
connection processes one request at a time. Custom socket paths too long for
the companion endpoint retain regular admission only. Excess connections are
closed; idle/incomplete reads and blocked writes expire after 30 seconds.

At most 32 regular mutations enter the handler concurrently. Further mutations
receive `busy` with “request not started”; they are not queued and cannot take
effect later. Admission is the handler boundary, not a claim that a queued GUI
command has run. Once admitted, changes may wait for I/O or confirmation and
are never cancelled by server deadlines or client disconnects. Hooks and status
reads can still reach GTK while a layout change waits for confirmation.

Read-only queries have a 10-second response budget. Browser waits retain the
requested duration plus 10 seconds. Mutations include raw JavaScript and
screenshot file writes. A lost connection does not prove a mutation failed;
inspect its result before retrying. The client never automatically replays a
sent request. It falls back to an older server's regular socket only when the
companion is unavailable before transmission. Persistent clients must reconnect
after their idle connection closes.

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
