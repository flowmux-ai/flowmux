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

The named-pipe listener now resets abandoned connections when a client closes
before `ConnectNamedPipe`. A deterministic native regression failed with error
232 before the fix and accepts the next client after it. Discovery publication
uses a synced temporary file and atomic replacement, with bounded retries for
transient sharing/access failures. Failed publication releases its newly bound
pipe; failed listener-thread startup removes its new record. Fatal listener
failures are logged and remove the matching discovery record.
The CLI ignores malformed, oversized or mismatched discovery records and checks
the OS-reported pipe server PID before sending bytes. Explicit/inherited targets
retain their window affinity and report underlying connection errors. Client
security quality of service permits identification without impersonation.
Hidden live hosts survived 640 empty/truncated disconnects, eight idle clients,
two-window routing, six fresh start/quit cycles and normal discovery cleanup.
The earlier workspace restart failure remains unexplained; these reproductions
do not establish its historical cause. See [IPC evidence](evidence/2026-09-28/ipc.md).

IPC now uses a fixed pool of 16 reusable overlapped pipes and workers. Request
reads and reply writes have 5-second whole-frame deadlines; the GUI command wait
is 15 seconds. A reply is retained until peer closure or a 2-second deadline,
replacing blocking flush. The CLI has bounded connection/write/reply waits and
never retransmits an already submitted request. Sixteen GUI command permits
remain attached to the UI's reply handles even when callers time out, preventing
an unresponsive UI queue from growing through repeated timeout/reconnect cycles.
Native host teardown cancels IPC before releasing terminal surfaces and joins all
workers. CancelIoEx is followed by completion observation before buffers or
OVERLAPPED values are released. Native tests cover cancellation in accept/read/
write/GUI waits, reused instances, oversized/fragmented Unicode frames, queue
permits, unread replies, and repeated pool lifetimes without handle growth.
Hidden live tests verify capacity, idle expiration, CLI reply timeout and quit
with an unread reply and 15 idle peers. Fault injection during partial startup,
automatic recovery after a fatal pool error, process-crash record cleanup,
sustained load/fairness, full discovery/multi-user/deployment coverage and real
IME remain pending. See [IPC limits evidence](evidence/2026-09-28/ipc-limits.md).
O02 remains partial.

Terminal settings now have a native menu/editor and CLI, with versioned shared
Windows config for font/fallback list, integer size, dark/light terminal theme,
scrollback and cursor blink/style. A dedicated worker validates and atomically
saves under an OS writer lock, re-reading before per-field updates. Stale edit
values and failed saves preserve existing data. External changes propagate to
other windows, including edits that preserve the revision; corrupt config retains
last-good runtime values and requires explicit reset. Existing xterm views apply
options without recreating shells or requesting focus, deferring while terminal
composition or history restoration is active. Per-view applied acknowledgements
are distinct from successful persistence. Hidden live tests verify Unicode values,
two-window updates, three-tab restart, actual ConPTY dimensions, history eviction,
invalid/stale/denied writes and corrupt-file recovery. Real menu/editor/IME/DPI,
font availability/width, custom themes, whole-app styling, per-tab settings,
configurable zoom keys and broader config/log/cache parity remain pending.
T07, T20, U10 and O06 remain partial; see
[settings evidence](evidence/2026-09-28/settings.md).

Shell choice now covers the shared default, individual new tabs/workspaces and
split inheritance, with built-in Windows PowerShell/CMD/PowerShell 7 discovery
and explicit executable/argv selection. Resolution uses full executable paths
before CreateProcessW; custom PATH/PATHEXT lookup is restricted to executable
extensions and never adds an implicit current-directory search. CMD reports
cwd through a child-only prompt prefix; PowerShell retains its prompt wrapper.
State stores each terminal's shell/argv; old state retains the original Windows
PowerShell behavior. Missing or invalid executables leave a saveable failed
terminal with explicit CMD recovery, preserving its surface/history and other
processes. Thread error-mode guards return native startup errors without system
error dialogs, and startup unwinding closes undrained output before ConPTY.
Hidden tests cover exact Unicode cwd/argv, failed-image recovery, mixed-shell
restart, unavailable restored executables and old-state compatibility. PowerShell
7 is unavailable on the validation machine; actual PS7, other interactive shells,
custom script parsers, physical menu/IME/DPI and broader failures remain pending.
T01 and T23 are partial. See [shell evidence](evidence/2026-09-28/shells.md).

The installed `flowmux.com` console entry point shares a unified launch/command
grammar with the GUI and executes CLI operations synchronously. `flowmuxctl.exe`
remains command-only; shortcuts still target `flowmux.exe`. Launch flags are no
longer inferred from the first argument, fixing leading `--shell` and `--cwd=`.
The console launcher starts its sibling GUI without inheriting any handles:
the initial redirected-pipe hang was reproduced and removed in a hidden native
run. CLI help, version, exit codes, JSON/text results, runtime stderr and closed
stdout behavior are consistent across entry points. GUI command mode retains
file/pipe/NUL redirection on console attachment and never opens error dialogs.
The installer includes/removes the console payload but has only been rebuilt
in this stage. Full command parity, custom shell resolution, clean-machine
installation/update and desktop acceptance remain open. O01 is partial; see
[entry-point evidence](evidence/2026-09-28/entrypoints.md).

New-window launch from a native terminal now delegates to its originating GUI
through the verified pipe instead of creating a GUI inside the terminal's job.
A hidden reproduction first showed both the new GUI and its shell dying when
the source tab closed. The corrected path retains both, also across clean and
forced source-host exit and a further launch from a retained CMD window.
The terminal job limits remain unchanged. A bounded UTF-16 launch context carries
caller argv/cwd/environment to the broker, preserving PATH and drive entries
while removing stale flowmux routing variables. Debug formatting redacts its
contents. Invalid requests and stale callers fail without fallback or replay.
Source and child context, Unicode paths/values/argv and normal descendant cleanup
are checked separately from real IME. External-job restrictions, caller timeout
after dispatch, detached existing tabs, window geometry, sleep/resume and full
multiwindow acceptance remain pending. See
[window lifetime evidence](evidence/2026-09-28/window-lifetime.md).

Windows explicit-text `paste` now waits for terminal output parsing before
using xterm's current bracketed-paste mode. Browser terminal paste events share
the bounded capture path and avoid duplicate input and the Shift+Enter mapping.
The host acknowledges one complete native queue submission; closed, busy,
expired and invalid requests fail without replay. Unicode is not normalized;
xterm converts LF/CRLF to CR. A hidden WebView2/ConPTY probe compares actual
UTF-8 console input with pre-ConPTY bytes, including mode on/off, 128 KiB text,
decomposed Hangul, emoji and inactive-tab routing. The verifier never uses the
desktop clipboard or OS input. Actual clipboard/selection/menu/IME acceptance,
all queue/timeout races and shell/TUI paste behavior remain open. T08 and T09
are partial. See [paste evidence](evidence/2026-09-28/paste.md).

Windows terminal selection now retains an exact bounded Unicode snapshot across
TUI redraws, tab moves and process exit, with explicit clearing for new selection,
search/input, normal/alternate-buffer transitions and reset. Parser-barrier CLI
operations read, select a cell range, select all or clear without OS clipboard
access or target activation. Copy/paste shortcuts and a keyboard-accessible
terminal context menu use the browser Clipboard API; delayed reads are discarded
when focus, visibility, composition or input changed. Background hosts disable
clipboard access in this controller and its automatic read permission grant.
Hidden native selection/Unicode/redraw tests and mocked clipboard/menu event
tests are separate from physical clipboard, mouse, accessibility and IME
acceptance, which remains open. T08 stays partial; see
[selection evidence](evidence/2026-09-28/selection.md).

Windows `capture-pane` now aliases the parser-barrier viewport read.
`read-screen --recent` extracts the latest 80 normal-buffer physical rows, or
all alternate-screen rows including footers below the cursor, without changing
selection, scrolling or focus. Responses preserve text/sequence compatibility
and add buffer/range/cursor metadata. Exact UTF-8 text is bounded to 128 KiB;
oversized results return an error instead of partial text or a bridge timeout.
Ten hidden native tests cover Unicode, wraps, redraws, old scroll positions,
hidden/moved/exited tabs and buffer transitions. Native oversized-grid and race
acceptance, sustained load and agent status polling/classification remain open.
T12/T13 are partial; see [screen evidence](evidence/2026-09-28/screen.md).

Windows terminals now include a cell-based canvas minimap with separate preview
wheel scrolling and viewport click/drag/keyboard navigation. Shared settings
persist enable/width/opacity and retain the gutter across alternate-screen
transitions. Hidden tabs release their raster while output parsing continues.
Public cell APIs preserve wide-cell alignment and styled foreground/background
colors within a bounded row window; updates retain their first 100 ms deadline.
Parser-barrier CLI operations inspect the actual raster or navigate without
keyboard focus. Nine hidden native checks cover pixels/cells, Unicode/color,
selection, settings, ConPTY dimensions and hidden/moved/exited surfaces.
Physical pointer/keyboard/IME/accessibility, high-DPI visual fidelity, sustained
multi-pane budgets and OSC palette mutation remain open. T14/T15 are partial;
see [minimap evidence](evidence/2026-09-28/minimap.md).

Windows notifications now reuse the existing pure Rust streaming OSC parser and
bounded notification store by read-only module inclusion. A native list and
workspace/tab captions expose unread notices; CLI operations inspect, acknowledge,
remove or reopen their stable source after moves and process exit. Explicit
notifications and simple OSC 9/99/777 share deduplication and priority rules.
Output counters/timestamps provide observations without declaring agent identity
or completion. Hidden native tests cover actual ConPTY Unicode delivery, native
control text, routing, bounded retention and independent window stores. Desktop
toasts, taskbar integration, real foreground/IME/menu acceptance, advanced OSC
protocols and agent hooks remain open. T19/U14/A07 are partial; see
[notification evidence](evidence/2026-09-28/notifications.md).

Windows named keys now wait for the target xterm parser before using its public
application cursor mode. Arrows/Home/End switch between normal and application
sequences, and an expanded named-key encoder is checked against 128 vectors from
the pinned xterm keyboard implementation plus flowmux's Shift+Enter contract.
Hidden native tests compare bytes before ConPTY and through ReadConsoleW, including
inactive/moved/exited targets. The new wire method avoids old hosts ignoring a
surface selector; legacy requests remain accepted by new hosts. Composition,
restore and pending-input guards preserve the existing text owner. Physical
keyboard, focus reporting/IME, advanced keyboard protocols and broader shell/TUI
acceptance remain pending. T21/O04 are partial; see
[named-key evidence](evidence/2026-09-28/keys.md).

The Windows changes are confined to this directory. During development, the
separate existing-platform change `15ee955` (WSL Shift+Tab) appeared in the
shared checkout; it was not edited or included as part of this Windows work.
The root manifest/lock and shared core remain unchanged by the Windows host.

Windows browser tabs now use separate WebView2 views/profile and the existing
pure placement model. Native address/navigation/status controls and CLI
open/navigate/history/stop/zoom/status/synchronous eval are implemented. Browser
pages have no terminal bridge or host objects. Native navigation IDs guard old
completion callbacks; failures surface in the status line. Mixed terminal/browser
moves preserve view/process identity, and browser-only state can save and restore
without waiting for nonexistent terminal history callbacks. Terminal-only actions
reject browser surfaces. Shared Linux/macOS code remains unchanged.

B01/B02/B03/B04/B05/B06/B07/B08/B09/B10/B11 and G09 are partial.
Remaining popup validation, full browser find acceptance, DevTools, private/profile controls, bookmarks, cookie import,
media/login/fullscreen and desktop IME/DPI/accessibility acceptance remain open.
The hidden loopback fixture checks Unicode DOM/control text, history, network
failure recovery, isolation, move identity, mixed checkpoints and browser-only
restart/profile persistence. It never synthesizes OS input or accesses clipboard.
See [browser evidence](evidence/2026-09-28/browser.md).

Windows DOM snapshots now share the existing headless Rust snapshot/ref types.
Top-document selectors must be unique, and ref numbers are not reused in a live
window. DOM revisions, navigation IDs and snapshot identities reject stale refs
and callbacks without stamping the DOM. Read-only text/value/attr/state/count
commands support structured JSON and plain scalar output. Output/traversal bounds,
Unicode excerpts, duplicate/deep selectors, reference lifetime and failure recovery
are checked in a hidden WebView2 fixture. Frames/shadow trees, full accessibility,
active-element type/press and actual browser IME remain pending. PNG capture is
described below. See
[DOM evidence](evidence/2026-09-28/browser-dom.md).

Windows browser waits now cover selector/text/URL/ready-state/synchronous JS
conditions through an independent native timer. Each wait pins the initial
surface, allows only one in-flight predicate, rejects old navigation callbacks
and checks the deadline again at completion. Timeout returns false; script errors
and browser close return errors. Only wait commands extend the IPC budget, up to
the validated 120-second wait plus transport margin. Existing terminal/state timer
frequency and non-wait IPC limits remain unchanged. B07/G09 remain partial because
client-disconnect cancellation, broader page semantics and complete race/physical
UI acceptance remain open. See [wait evidence](evidence/2026-09-28/browser-wait.md).

Windows now has ten ref-based DOM action commands. Ref binding validates the
current snapshot/DOM before execution, and per-surface admission prevents actions
and snapshots overtaking a pending action. Existing refs remain valid for explicit
repeats when the DOM is unchanged. Input values use native DOM setters with Unicode
preservation and beforeinput cancellation; OS input is never injected. Focus/blur
are blocked in background test hosts, so actual focus acceptance remains pending.
Clap action argument structs share their builders to avoid a reproduced Windows
debug main-thread stack overflow without changing CLI/JSON grammar or stack size.
B04/B05/G09 remain partial. See [action evidence](evidence/2026-09-28/browser-actions.md).


Windows browser viewport capture now uses native WebView2 CapturePreview/PNG,
with CLI-relative Unicode paths, original-surface/navigation/viewport validation,
size limits and two outstanding capture/save slots. COM stays on the UI thread;
a separate worker flushes a sibling temporary file and atomically replaces the
destination. Timeouts after file dispatch are explicitly ambiguous and never
trigger a retry. Hidden hosts capture actual page pixels without foreground,
keyboard, pointer or clipboard operations. B08/G09 remain partial: physical
DPI/minimized/multi-monitor behavior, remote filesystem guarantees, exhaustive
lifecycle races and real IME are not established by these tests. See
[capture evidence](evidence/2026-09-28/browser-capture.md).


Windows browser downloads now use native WebView2 transfers and a Downloads
manager/list, with CLI inspection/cancel/history controls. Each transfer receives
an isolated staging directory and a no-replacement final rename with collision
suffixes. UTF-8 extended filename metadata preserves decomposed Korean and other
Unicode codepoints while retaining the browser-selected extension. Native state
callbacks complement progress polling; reported interruptions request cancellation.
The runtime can retry internally before exposing an interrupted state.
B10/G09 remain partial. Save As/destination choice, pause/resume, restart recovery,
physical UI/security integration and broader lifecycle/network/filesystem coverage
remain open. See [download evidence](evidence/2026-09-28/browser-downloads.md).


Windows browser page find now calls the engine's `window.find` with bounded,
JSON-encoded Unicode queries, direction/case/wrap controls and a native panel.
The panel pins its browser surface, preserves native EDIT/IME messages, and
avoids automatic searches while typing. Deferred panel dismissal clears only
still-owned exposed selections after pending work completes. Navigation,
visibility revision and surface closure prevent stale result publication.
Eight hidden native groups cover engine matching, native query codepoints,
selection ownership, pending dismissal and navigation/closure. Engine Unicode
matching can differ from the original query, and shadow/text-control range
cleanup is deliberately conservative. Physical IME/UI, Find API parity and
remaining B11 browser controls remain pending. See
[page-find evidence](evidence/2026-09-28/browser-find.md).

Windows browser new-window requests now have native popup-to-tab routing. A
UI-thread-owned deferral attaches a fresh, never-navigated child through
`SetNewWindow`, using the opener's actual WebView2 environment and browser profile.
No URL/HTML initialization or separate navigation substitutes for the native
WindowProxy/opener connection. The original SurfaceId determines the destination
pane, where the child becomes active; terminal bridges remain unavailable.
Delivered script requests are accepted without a user-gesture restriction under
the URL, active-source and capacity policy. The host bounds live popup tabs to 16
and pending requests to eight, including native construction in progress. Epoch,
visibility-revision, closure and 12-second validity checks reject stale requests;
the deadline does not interrupt blocked COM creation.

Actual native close events remove the exact browser tab without cascading through
its children. A workspace's final closed browser is replaced by a new SurfaceId
and `about:blank` view in the same pane. Opener clearing/closed state and refusal
to close an ordinary browser remain engine behavior. Native creation/close
failure recovery, exhaustive deferral races, cross-origin/named-target policy and
physical UI/IME/DPI/accessibility acceptance remain open. The final hidden debug
fixture passed 14 groups in 45.107 seconds using owned loopback pages without
desktop input, foreground activation or clipboard access. Native opener access,
Unicode URL/blank content, routing, close behavior and bounded capacity have
[measured results](evidence/2026-09-28/native-browser-popups-background.json);
the ordinary-browser close case records whether the engine permits closure.
Release builds, 141 actual Windows unit tests, 49 related native regression
groups and installer packaging passed; results and remaining limits are in the
[popup record](evidence/2026-09-28/browser-popups.md).
B11/G09 remain partial and the browser CLI operation count remains 34. No shared
Linux/macOS source or acceptance status is changed by this Windows implementation.

Windows editor tabs now embed the existing Monaco frontend and offline workers
in a trusted WebView2 page. A separate Windows build appends its bridge adapter
without changing shared sources. CLI Open creates or reuses an editor tab;
actual model commands cover edits, undo/redo, saving, explicit Save As, conflicts,
recovery and document close. UI and CLI Save All use the same serial helper,
waiting for each document and native response before submitting the next file
to the bounded I/O worker. Main and diff models use LF internally while the
shared document service retains disk BOM/EOL metadata.

Authenticated surface/generation messages, content acknowledgments and flush
barriers order worker snapshots and dirty-close decisions. Pending mutations
seal editing; timed-out replacement work retains the seal until its response,
and synchronization failure keeps it read-only. Pane moves retain the WebView,
documents and undo history. Checkpoints retain editor file/session metadata;
recovery snapshots use a Windows-specific private temporary-file writer after
native testing exposed directory attributes copied onto a file by the shared
recovery fallback. Hidden hosts keep both editor profiles and recovery data
inside their explicit isolated state directory.

The initial editor stage passed fifteen unique hidden native checks, covering actual Monaco content,
Unicode/BOM/EOL bytes, ten-document Save All, dirty closure, conflict actions,
moves, restore failures, checkpoint failure and recovery after acknowledged
edits plus a completed checkpoint. A clean discard-state quit now completes even
when its IPC reply receiver expired before the editor barrier ran. The late-quit
case observed the real 15-second server deadline while only the owned hidden UI
thread was suspended, then verified balanced resume and clean host exit. The
close case also passed again after this fix. Twenty-seven related state, pane, workspace
and browser regression checks also passed. These are bounded case results,
not physical UI or complete editor acceptance. Final Windows debug and release
builds passed; source boundaries, repeated cases, native unit/package results
and preserved failures are tracked in the [editor record](evidence/2026-09-28/editor.md).
Open path preparation now runs on a bounded worker, retaining its original
source/pane/workspace/root and checking them again before publication. Admission
allows one preparation per pane and eight jobs per host, including cancelled
filesystem work and posted results. CLI Open keeps a twelve-second budget from
server receipt through preparation and initialization. WebView2 COM construction
still runs on the UI thread. The sidebar Open File button and `editor pick`
use an owned native dialog; the CLI returns an accepted-request receipt before
human interaction, not an editor surface. Background mode refuses the picker.

Accepted window closes cancel pending Opens and block mutations while replies
drain, including direct discard-state quits without an editor barrier. Window
checkpoint replacement now uses the canonical parent's extended-length Windows
path after a nested verifier directory exposed a 266-unit temporary filename
and a raw `MoveFileExW` path failure. The failed Restore run is preserved.

The [Open and picker record](evidence/2026-09-28/editor-open.md) separates three
initial hidden passes from seven final-source editor cases and 36 related native
regression checks. Final cases include deep-path Restore, expired queued Open,
accepted close during paused preparation, direct/editor late quit, dirty close
and failed-checkpoint unsealing. Controlled cases pause only exact owned hidden
threads and record balanced resumption; they do not cover every timing race or
exercise the physical picker. That stage's final Windows debug/release builds,
Clippy, format and all 172 actual Windows unit tests pass. Two earlier Restore
failures remain preserved; package, entrypoint and cleanup results are recorded
separately.

Automatic disk refresh is now implemented through one recursive native watcher
per editor root and the ordered document worker. Notifications coalesce while
one refresh is outstanding; each admitted scan checks all open files' bytes.
Existing clean Monaco models can update without activating an inactive document
or requesting focus. Dirty or deleted files retain their models and receive
conflict status. Scheduling defers for busy editors, recent activity, Monaco
composition, non-text Monaco widget focus and known editor HTML dialogs/diff
state. Input remains guarded through actual frontend application acknowledgment,
including after a timeout whose original I/O result is still pending. Close and
model commands during busy work fail explicitly rather than being queued for replay; tab moves retain
the editor/view identity without this readiness restriction.

A partial scan error preserves completed clean replacements and their advancing
versions alongside the error. `documents[].disk_status_known` exposes unresolved
status; uncertainty can remain sticky after unlock or an empty successful retry.
`automatic_refresh` distinguishes watch generations, applied acknowledgments,
pending phase and failures. These counters are not write counts or proof of a
fully successful scan. Unrelated root writes still cause full open-document
scans. Automatic watcher restart, periodic fallback and explicit watched-root
rename detection are not implemented. Known Monaco/HTML modal deferral is not
physical IME or general desktop-modal acceptance.

Eight final-debug-source hidden native refresh cases passed, with 58.397 seconds
of summed runner time: clean/inactive updates, dirty conflict actions,
deletion/recreation, equal-length restored-timestamp writes, partial denied reads,
move/close ownership and write-burst/own-save handling. Actual Monaco models were
read after apply acknowledgment without an explicit disk check. Partial-error
coverage retained both the repaired model and the blocked model/error, then
saved with the advanced version. The move/close case waited for idle and observed
absence for 1,553 ms after close; it did not force a stale callback race. A
32-write burst produced 64 observed generations and two completed refreshes,
without assuming one event per write. The current Clippy, debug build and format
checks passed in 17.285, 79.917 and 1.109 seconds respectively; 28 adapter tests
also passed. Nine related editor cases passed in 59.789 summed runner seconds:
Open, edit, encoding, explicit conflict, move, close, Restore, missing root and
checkpoint failure. All 184 actual Windows unit tests passed in 13.084 seconds
(12.99 seconds in the test binary). These include three native watcher tests
for extended Unicode roots/atomic replacement, recursive notices with exact
acknowledgment and cancellation drain, and missing-root failure. Source boundaries,
release/package/cleanup results and remaining limitations are recorded in the
[automatic-refresh evidence](evidence/2026-09-28/editor-refresh.md); the earlier
editor evidence remains historical.

The F06 stage implements Windows Quick Open, workspace search and literal
Monaco find/replace CLI commands. Six distinct hidden native cases passed across
eight executions. The initial five covered find (4.625s), replace (5.164s), Quick
Open (4.679s), workspace search (5.195s) and retained-result Open (7.371s); ten
related editor checks also passed on that initial source. The deadline correction
passed result Open again (5.990s) and a real IPC deadline case (20.321s). A later
native unit failure exposed incorrect separators in an extended Win32 result path.
After that correction, nested Unicode result Open passed (6.265s), all 208 Windows
unit tests passed (13.135s), and final Windows Clippy (22.328s) and debug build
(82.702s) passed. Earlier checks retain their source boundaries; the evidence also
preserves the unit failure and stopped release build. Full F06 acceptance remains
open. Release/package and cleanup results are recorded separately in the search
evidence. Quick Open indexes paths without a document flush/snapshot.
Workspace search uses acknowledged open buffers (including dirty/deleted-file
overrides), a 16 MiB snapshot cap, two fixed worker threads, eight admitted
requests and 32 MiB of admitted snapshots. It limits each request to a four-second
budget, 20,000 visited entries, 64 MiB of file reads, 2,000 indexed paths or 500
matches, and a 1 MiB result envelope. Cancellation retains a blocked operation's
worker/permit until that operation returns; the UI never joins search threads.

The scanner confines same-handle Windows reads to the captured local root,
skips reparse entries and honors bounded in-root ignore files. Individual skipped
read errors remain visible alongside available results. Queries/rules, expired
requests and stale snapshots fail explicitly. Workspace matching uses Rust regex
or literal queries over LF text after stripping one disk UTF-8 BOM; no NFC/NFD
normalization is applied. Shared workspace ranges use zero-based UTF-16 columns;
Monaco in-document ranges use one-based UTF-16 columns. Retained-result Open
checks source hashes and acknowledged buffer identities/versions before reveal.
Its deadline starts at IPC receipt, includes UI queue time and rejects expired
work before submission. Late submitted operations retain the synchronization guard
until model reconciliation completes, while their expired RevealRange is omitted.
The native deadline case pauses only the verified owned hidden host UI thread,
waits for the actual server deadline, resumes in finally, verifies unchanged model
and selection, then proves a fresh result open succeeds. It does not force timeout
after worker submission or cancellation during flush/snapshot capture.
In-document CLI replacement is literal, version-pinned, undoable and refuses
Replace All over truncated matches. Shared editor/platform sources stay read-only.
See the [command and limit reference](README.md#editor-search-partial); the
[search evidence record](evidence/2026-09-28/editor-search.md) tracks measured
results separately from this implementation description.

F01–F08, F17 and G08 remain partial, with 60 partial/54 pending features and ten
partial/three pending gates. Broader F06 acceptance, physical
IME/GUI/clipboard/fonts/DPI/accessibility, broader automatic-refresh acceptance
and the stated watcher limits, viewers, full ACL preservation, exhaustive
long-path behavior, unsynchronized edit/close races and renderer-crash recovery
remain pending.


The F07 stage adds a native Files child panel usable from terminal-only panes.
Tree state keeps exact Unicode relative paths, retained snapshot indices and tokens,
separate logical/visible selection, explicit refresh and 500-row increments. Directory
I/O runs on one worker with eight admitted requests, including posted responses.
Each scan retains its original four-second deadline, at most 20,000 visited entries,
four MiB of path/name storage, 64 expanded directories/levels and one-MiB pages.
The host bounds 32 pane caches with conservative 16-MiB accounting; selection
updates are transactional under that cap. Missing descendants of uninspected
collapsed folders remain unknown. Root failure invalidates old rows for Open.

Windows enumeration validates and reads the same directory handle, rejects reparse
traversal, confines final handle paths to the captured root and normalizes separators
before verbatim paths. Files ownership pins workspace, pane, source surface, instance
and generation. Hide/root replacement/source invalidation cancels pending work.
Open reuses the existing asynchronous editor preparation and synchronization path;
its owner survives preparation into editor barriers. Work not yet dispatched can
be cancelled; already-applied document responses remain reconciled and stale
operations report an error. No shared editor protocol or Linux/macOS source changed.

Seven distinct hidden native Files cases have passed, including actual LISTBOX
readback, long Unicode paths, editor reuse, paging, selection, stale-token rejection,
reparse leaves, source ownership and real queued IPC expiry. Physical UI/IME and
full F07 parity remain open. See the [Files evidence](evidence/2026-09-28/files.md)
for source boundaries, failures, measured checks and final packaging results, and
the [command reference](README.md#files-panel-partial) for supported operations.


F08 adds single-file Copy/Rename/Move with retained Files tokens, one host-wide
worker and 64 terminal receipts. Preparation pins source/parent handles; the host
rechecks owner, deadline and editor state before returning acceptance and
submitting mutation. Retired replies cannot submit commit. `NtCreateFile` and
`NtSetInformationFile` keep child creation and no-replacement rename bound to
those handles. Copy preserves raw bytes through a temporary file, up to 16 MiB.
All open source editor documents are rejected, including clean documents.
Cancellation remains cooperative; close/editor operations reject until the worker
returns. Shared Linux/macOS code stays unchanged.

Five affected hidden native cases pass: long nested Unicode roundtrip (9.124 s),
text/binary copy (3.737 s), invalid destinations (4.611 s), clean/dirty editor
guards (6.050 s), and stale tokens (3.240 s). These include actual LISTBOX and Monaco
observations. Final regression/build/package results are tracked separately in
[file-action evidence](evidence/2026-09-28/files-actions.md). F08 stays partial:
clipboard cut/paste, batch/directory/remote operations, overwrite and physical
keyboard/IME/focus acceptance remain open.
