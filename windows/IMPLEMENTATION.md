<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows implementation ledger

The goal is an installable Windows flowmux following the complete feature
review, while preserving the existing Rust Linux and macOS implementations.
This directory is a separate Cargo workspace with its own dependency lock.
The original workspace, platform code, installer and lockfile remain unchanged.
Shared pure domain code is consumed as a path dependency, without editing it.

The Windows host uses Rust/Win32 for windows, workspaces, panels, tabs and
commands, and WebView2 for terminal content. The terminal page uses xterm.js;
shells are native Windows processes attached to ConPTY. A whole-application
web frontend and a WSL launcher are not substitutes for this goal.

The architectural review allowed both a GTK host and a Windows Rust host.
The existing GTK application links VTE, WebKitGTK and Unix services directly.
The explicit requirement to leave both existing platforms unaffected makes a
separate native Rust host the implementation boundary here. GTK-on-Windows
embedding has not been demonstrated; the alternate native host still needs
all mixed-UI, IME, accessibility and lifecycle acceptance checks.

## Commit-sized stages

1. Preserve the architectural and Korean-input review, then establish an
   isolated workspace, origin/surface/generation validation, ordered byte
   transport and a real Rust host/WebView2/ConPTY window.
2. Native workspace/pane/tab UI, settings/persistence, process ownership,
   Named Pipe CLI and screen barriers. Retain live sessions across tab moves.
3. Terminal feature parity: search, selection, clipboard, scrollback,
   snapshots, minimap, background parsing, notifications, IME and keyboard.
4. Browser automation, Monaco editor, files/viewers, Git/worktrees, agents,
   hooks, history and usage services with Windows adapters.
5. SSH lifecycle/forwarding, installer/update/uninstall, deployment notices,
   Windows CI and full platform regression/acceptance evidence.

These are sequencing boundaries, not a reduced scope or a claim that the
first terminal window completes the task. `acceptance.json` tracks every one
of the review's 114 feature rows and 13 gates. A status only becomes passed
when linked evidence demonstrates that exact requirement. Native Windows
IME/installer/UI behavior cannot be established by Linux unit tests alone.

## Verification boundaries

- Baseline existing source: `91f9a0922736619fca9d514418f77f1d69865981`.
- Do not fix legacy Linux IME issues in this Windows implementation series.
- Check changes to `crates/`, root Cargo files and existing platform scripts
  against baseline at each stage; none is currently required.
- Run Windows host-independent tests on Linux as fast checks; cross-build and
  run the actual PE binaries on Windows for native behavior.
- Run existing required checks for any shared source change. macOS live
  evidence remains separate from cross-compilation or source isolation.
- No claim of absolute regression freedom is made from a narrow test pass.
- No release-complete label until all required feature/gate evidence exists.

## Current evidence

The first native implementation includes Win32 workspace/pane/tab controls,
trusted xterm.js views, ConPTY sessions, kill-on-close process jobs, per-window
Named Pipe CLI, parser-completion screen barriers, and a per-user NSIS installer.
Release binaries statically link the CRT. The official app-local ConPTY
package is pinned by SHA-256; neither WSL nor a system Rust installation is
required to run the installed application.

Actual Windows execution on build 22623 with PowerShell 5.1, Microsoft Korean
IME and WebView2 112.0.1722.48 established these **partial** results:

- Native PowerShell startup, Korean UTF-8 round trip, four pane splits keeping
  the original process alive, tab close, and 100 repeated create/close cycles.
- Eleven real IME cases, including hidden-cursor preedit, Backspace decomposition,
  consonant transfer, Enter, Shift+Enter, three rapid Enter presses, mixed input,
  and Shift+Left. CLI text injection is not used to establish those IME results.
- The system ConPTY leaked one process handle per closed session. A native
  test without any WebView reproduced 64 -> 84 handles over 20 cycles. Using
  Microsoft's app-local ConPTY 1.24.260710001 removed that growth. In the live
  window, process handles stayed at 13 and total handles were 325/325/324/324
  at cycles 25/50/75/100. This is not a full process-tree memory benchmark.
- Per-user installation, repeat installation, shortcuts, registration and
  uninstall preserving the exact PATH value and registry value type.
- Natural shell exit retains the last Korean output and exit code after EOF,
  releases the native session, and keeps the rendered screen readable. Tab
  close and host termination remove the root shell and three descendant
  processes. Twenty failed spawns and twenty direct PTY closes have bounded
  handle counts. Repeating 100 live tab cycles after this change kept process
  handles at 13 and total handles at 295/295/294/294.

Evidence is under [evidence/2026-09-27](evidence/2026-09-27/README.md). No complete
Windows acceptance gate has passed yet. Hanja candidates, focus transitions,
DPI, clipboard/Unicode-width coverage, broader TUI compatibility, additional
failure paths, full feature parity and clean-machine deployment remain.

The Windows changes are confined to this directory. During development, the
separate existing-platform change `15ee955` (WSL Shift+Tab) appeared in the
shared checkout; it was not edited or included as part of this Windows work.
The root manifest/lock and shared core remain unchanged by the Windows host.
