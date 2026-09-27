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
- Same-window tab reordering and moves between panes/workspaces preserve the
  existing surface, PTY and WebView. A hidden native host completed 40 workspace
  moves with the original PID, output and cwd retained. Commands invoked by the
  moved, hidden child resolve its current location from its stable surface ID.
- Background verification after tab moves also repeated all 100 create/close
  cycles (process handles 13; total 260/260/261/261) and the lifecycle checks.
  Test scripts now default to a hidden debug host; interactive input is opt-in.

The user is working on the Windows desktop and requested background-only work.
Native menu/IME move trials were interrupted by concurrent desktop input, so they
are not acceptance evidence. The focus path avoids repeating WebView2 MoveFocus
when the same view already owns focus, but the real IME move case still needs a
controlled interactive rerun. The earlier eleven IME cases validate the lifecycle
build, not this newer focus change. Moving to another window, drag/drop, broader
TUI/scrollback and all other pending gates remain separate work.

The hidden debug host also completed synchronized Korean/color output across
1/4/16 panes (10,000 lines per pane), with parser-complete reads taking about
0.98/2.52/5.99 seconds from the start gate through the final reads. A separate
inactive tab completed its output and remained readable without changing focus.
`read-screen --surface` exposes this direct inactive-tab read to the CLI.
These timings include probe generation and polling, not just terminal throughput.
Post-output private memory for the owned process tree was approximately
276/732/2303 MiB; the 16-pane sample included 16 PowerShell processes,
16 ConPTY hosts and 21 WebView2 processes. This is a point-in-time measurement,
not peak usage or a leak test. Memory efficiency, repeated sustained load,
minimize/lock/resume and release-build desktop evidence remain pending.

Windows state now uses versioned per-window files with exclusive OS leases and
atomic replacement on a worker thread. Parser barriers capture bounded styled
normal-buffer history for visible and hidden tabs. Clean close, explicit save
and 30-second checkpoints persist layout, tab identity, focus and recorded
local cwd. Restore renders history into scrollback before starting fresh
ConPTY processes; historical VT responses are suppressed while parsing it.
A hidden native test verified four surfaces over two workspaces, Korean and
green SGR round trips, duplicate-restore rejection, independent window saves,
forced termination/recovery, periodic checkpoints, temporary mode, and failed
atomic replacement retaining the old file and live window. Geometry/settings,
alternate-screen snapshots, live-process/agent
resumption, broader Unicode/wrap fidelity and restore-time real IME remain
pending. This is partial persistence coverage, not full session recovery.

Session-local PowerShell prompt integration now reports current local drive
directories through OSC 7. Native ConPTY testing found that raw OSC text passed
through code page 949 could replace combining marks with `?`; ASCII URI encoding
preserves the original path without changing console encoding or PSReadLine.
Hidden hosts verified Korean/NFD/punctuation paths, new-tab/split inheritance,
hidden and moved surface isolation, same-line `cd; flowmuxctl new-tab` context,
and restart into the latest reported directory. A custom prompt retains command
failure status, exit code, dynamic variables and current `$pwd`; reinstalling
the wrapper is idempotent. Historical OSC metadata cannot change restored cwd.
Read-only native control inspection also verifies locked tab captions and the
active marker survive title output. UNC/device/remote paths, other shells,
long-path startup, arbitrary prompt frameworks and interactive IME remain open.

Single-terminal find now has case/regex controls, previous/next with wrap, clear
errors, close and a native entry point. The `find` CLI shares its controller and
waits for parsed output without taking keyboard focus. Hidden Windows tests
verify retained Korean history, NFD/emoji selection, soft-wrap boundaries,
inactive/moved/exited tabs, and separation of normal/alternate buffers.
A real in-place rewrite exposed stale xterm-addon search lines when the cursor
returned to the same position. The host now invalidates search after parsed
output; the frontend recreates the addon through its public API before the next
query, retaining the terminal and process. The same native reproduction passes.
Synthetic event tests guard composition Enter/Escape, but search-field real IME,
buttons, keyboard navigation and DPI still need interactive verification.
JavaScript regex differs from VTE/PCRE2; expression performance and full
text/Unicode acceptance remain open.

Whole-window output search now has a native EDIT/LISTBOX window and asynchronous
CLI start/poll/cancel/open operations. Hidden tabs participate without activation;
logical lines join soft wraps and results page in groups of 500. Scanning reads
xterm's parsed grid in yielded batches, with generation checks rejecting mixed
or cancelled results. Normal-buffer markers and retained text permit appended
output and moved tabs while refusing rewritten, evicted and reflowed matches.
Opening validates before focus changes, then revalidates after activation.
Native hidden tests cover multiple workspaces, 503 matches, a logical line over
260 physical rows, Korean/NFD/emoji selections, case-fold coordinate mapping,
same-cursor rewrites, alternate-screen transitions and scrollback eviction.
Results and error text are also checked through the hidden native controls.
Search-window real IME, mouse/keyboard navigation, visual layout/DPI, SSH and
sustained-output stress remain pending. Evidence is recorded separately under
[evidence/2026-09-28](evidence/2026-09-28/README.md).

Evidence is under [evidence/2026-09-27](evidence/2026-09-27/README.md). No complete
Windows acceptance gate has passed yet. Hanja candidates, focus transitions,
DPI, clipboard/Unicode-width coverage, broader TUI compatibility, additional
failure paths, full feature parity and clean-machine deployment remain.

Nested panes now expose draggable dividers and a ratio-based CLI, directional
focus, and temporary maximize/restore. All operations retain existing terminal
views and sessions. A hidden native test compares calculated rectangles with
WebView bounds and actual PowerShell console dimensions, checks hidden Korean
output, repeats 40 zoom transitions, and restores persisted nested ratios.
Invalid ratios/targets preserve state; resizing inactive workspaces preserves
focus. Structural changes and navigation to another pane clear zoom. Fixed
Alt+Arrow and Ctrl+Alt+M bindings defer to composition and AltGr; event unit tests
do not establish physical keyboard or IME acceptance. Native divider drag,
interactive focus, minimum-size usability, DPI and accessibility remain pending.
See [pane evidence](evidence/2026-09-28/panes.md); U02 and U04 remain partial.

Workspace metadata/lifecycle now has native menus and a UTF-16 name/color editor,
plus list/current/focus/rename/color/reorder/close CLI operations. Tab rename uses
the existing shared title-lock semantics. Unicode names retain their codepoints;
Win32 button/menu captions escape literal ampersands. Metadata updates rebuild
controls without requesting terminal focus; workspace actions use stable IDs.
Native hidden tests verify 41 reorders with stable processes, hidden caller
context, close of three terminals plus three descendants without affecting
survivors, and restart preserving order/color/locked Unicode names/history.
The editor detects metadata changed elsewhere before applying a pending edit.
Native Rust tests construct its controls hidden and check UTF-16 values.
Real menus/IME/DPI/accessibility, drag reordering, side panel overflow,
automatic-name reset and an empty-window UI remain pending. The final workspace
is protected until empty-window behavior is implemented. See
[workspace evidence](evidence/2026-09-28/workspaces.md).

The Windows changes are confined to this directory. During development, the
separate existing-platform change `15ee955` (WSL Shift+Tab) appeared in the
shared checkout; it was not edited or included as part of this Windows work.
The root manifest/lock and shared core remain unchanged by the Windows host.
