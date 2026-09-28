<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Native Windows host

The separate Windows Rust workspace uses Win32, WebView2, xterm.js and ConPTY.
The installed application runs Windows shells without WSL. Linux/macOS feature
parity is still in development; physical Korean IME and desktop acceptance
remain unverified.

## Build and run

Install Rust MSVC, the Windows SDK, Python and WebView2 Runtime 109+:

```powershell
cargo build --manifest-path windows/Cargo.toml --locked
Copy-Item windows/target/debug/flowmux-command.exe windows/target/debug/flowmux.com
python windows/scripts/fetch-conpty.py --output windows/target/debug
windows/target/debug/flowmux.exe
windows/target/debug/flowmuxctl.exe doctor
```

Keep the bundled ConPTY DLLs beside the binaries. Their versions and hashes are
pinned in `conpty.lock.json`; the host requires these DLLs.

To cross-build on Linux, install cargo-xwin and Clang/lld:

```sh
cargo xwin build --manifest-path windows/Cargo.toml --target x86_64-pc-windows-msvc --locked
cp windows/target/x86_64-pc-windows-msvc/debug/flowmux-command.exe windows/target/x86_64-pc-windows-msvc/debug/flowmux.com
python3 windows/scripts/fetch-conpty.py --output windows/target/x86_64-pc-windows-msvc/debug
```

Committed frontend assets allow Rust builds without Node.js. After changing
terminal code, run `npm ci`, `npm test` and `npm run build` in `windows/terminal`.
For Monaco assets, use the [editor build instructions](editor/README.md).

## CLI and user data

`flowmux.com` is the console launcher; `flowmux.exe` is the GUI and
`flowmuxctl.exe` sends commands to a running window. Consult `--help` and each
subcommand's `--help` for the current command set.

```powershell
flowmux --new-window --shell powershell
flowmuxctl --json tree
flowmuxctl --json identify
flowmuxctl browser open https://example.com
flowmuxctl editor open C:\work\project\README.md --root C:\work\project
```

A pane and its tab surface have different UUIDs. Commands accept explicit
`--pane` or surface arguments where supported. Each window has its own named
pipe. Terminals inherit `FLOWMUX_PIPE_NAME`; external callers should select the
intended window with `--pipe`. A command timeout can have an uncertain outcome:
inspect the current state before repeating a mutation.

Settings, saved windows and WebView/editor data live under
`%LOCALAPPDATA%\flowmux\windows`. Settings use `config.json`; instances publish
per-process discovery records under `instances`. `--temporary` disables window
checkpoint persistence. Closing a dirty editor requires a save/discard decision.

## Verify

Use the [bounded background workflow](scripts/VERIFICATION.md). Run only the
checks affected by a change. Success results print to the terminal and temporary
files are deleted; failures retain diagnostics for investigation. Do not create
or update progress reports, evidence documents or source-hash snapshots per edit.

```powershell
.\windows\scripts\run-check.ps1 -Name native -TimeoutSeconds 60 `
  -Path .\windows\scripts\verify-native.ps1 `
  -ArgumentList @('-BuildDirectory', 'windows/target/debug')
```

```sh
python3 windows/scripts/run-check.py --name tests --timeout-seconds 120 -- \
  cargo test --manifest-path windows/Cargo.toml --locked --lib
```

Windows UI checks use hidden debug hosts and isolated state. They must not
interrupt desktop input or replace a running application. Hidden checks do not
establish physical keyboard, Korean IME, clipboard, DPI or accessibility behavior.

## Installer

Install NSIS and run:

```powershell
.\windows\scripts\build.ps1 -Installer
```

The per-user installer uses a statically linked CRT and installs WebView2 when
needed. Uninstall preserves user data. Building an installer does not update an
already running application or establish installation/upgrade acceptance.
