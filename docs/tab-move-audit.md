<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Tab movement audit

Baseline: `d4e5e56a`. Findings and execution results are updated with the fixes.

## Acceptance rules

- A successful move has exactly one owner for the surface ID, model, widget and live backend.
- The destination is the explicitly requested pane/workspace/window, regardless of focus changes.
- Terminal PID/output, browser document state and unsaved editor buffers survive live moves.
- A rejected or cancelled drop leaves the source intact and does not open another window.
- A workspace remains in its owning window. Empty source panes collapse; a source window closes only when it owns no workspaces.
- Every supported route and rejection is covered by model/widget checks; native input separately exercises actual drag targets and event ordering.

## Partitions to exercise

| Dimension | Cases |
| --- | --- |
| Surface | Terminal, browser, dirty editor; SSH restrictions |
| Source/destination | Same pane, another pane in the same workspace, another workspace in the same window, main to detached, detached to main, detached to another detached window |
| Drop target | Before/after a tab, pane body as a tab, right/down split, workspace row, empty main window, Move menu, outside all targets |
| Source lifetime | Other tabs remain, final tab in a split pane, final pane in a workspace, final workspace in a window, another workspace remains in that window |
| Workspace creation | Shortcut, header button, local/team creation menu, request followed by a focus change |
| Ordering | Repeated moves, rename then drag without right-click, hover another workspace then leave/cancel, rejected/stale target, mount recovery |
| Process boundary | Same GUI process live move; separately launched GUI process investigated separately |
| Platform | Linux/X11 native input available here; macOS and native Wayland require their own runtime and must not be reported as executed here |

## Findings and fixes

1. At baseline, `NewWorkspace` had no requesting-window identity and consults `GtkApplication.active_window()` during dispatch. Queued input can therefore create a workspace in another window.
2. The main side panel included detached-window workspaces, although each detached side panel shows only its own workspaces. Row location and window ownership can disagree.
3. Sidebar hover called workspace activation, which can present another window during the drag. Sidebar drag flags are not shared by all detached sidebars.
4. Pane drops from an unknown source could import a serialized model with a new ID/backend, whereas sidebar drops reject the same unknown source. This is not a live transfer across processes.

5. A same-position move was reported as a missing source. Reorder commands now share the live-move handler and acknowledge a valid no-op.
6. Detached windows lacked the overlay required by workspace creation menus. The detached shell now includes that overlay.
7. Escape did not reliably cancel GTK tab drags. Cancelling the source can still be followed by a target drop on GTK 4.14. The window captures Escape, cancels the live source and marks the sequence consumed; every tab target checks that marker. An async target returning false must not also call `Drop.finish`, which would finish twice.
8. Workspace row reorder targets accepted tab strings as well as workspace payloads. They now accept only the workspace MIME type.

`NewWorkspace` now carries the requesting GtkApplicationWindow ID, captured before queueing for shortcuts, the header button and both local/team menu actions. Dispatch rejects a vanished host, and resolves cwd in the requesting window. The main side panel retains global title metadata for Move menus while showing only main-owned rows; selection updates only the owning panel. Detached panels share drag-sequence flags with pane/tab targets. Hovering a workspace row no longer switches workspaces or presents windows; a successful acknowledged drop activates its explicit destination.

Unknown tab sources are rejected instead of importing their serialized snapshot into a new backend. Explicit file-to-editor imports use a reserved nil source pane, preserving file drops. Terminal/browser/editor live objects cannot be reparented between independent GUI processes; these drops are verified safe rejections, **not** independent-process live-transfer support.

## Executed route matrix

The matrix runs mapped GTK windows with isolated state and real terminal/WebKit backends. It is a finite partition of the applicable routes, not a claim to enumerate every possible timing or platform event.

| Coverage | Execution | Result |
| --- | --- | --- |
| 3 surface kinds × 4 source workspaces × 4 destination workspaces × 5 routes | `tab_move_tests::tab_move_matrix_preserves_all_backends_across_three_windows` | **240 cases passed** |
| Hosts | Two main-window workspaces and one workspace in each of two detached windows | Same pane, same-window workspace changes, main↔detached and detached↔detached |
| Routes | Front/end tab insertion (native reorder command when same pane), workspace target, right/down split | Every surface/host combination above |
| Backend and ownership proof after every matrix move | Exactly one model owner; registry ownership; model/tab-bar/stack order and count; same live widget in actual destination window | PID unchanged, browser JS marker preserved, editor stays dirty; disk unchanged until explicit save; final saved text contains the edit |
| Creation followed immediately by focus change | 2 requesting windows × shortcut/header/local-menu/team-menu | **8 cases passed**, including detached menu rendering |
| Foreign/stale rejection | 3 serialized kinds × pane/split; missing pane/split/workspace targets | **9 cases passed**, unchanged source widget/PID, no imported backend/window |
| Native input scenario | `scripts/test-tab-windows-gui.py`, X11 XTest and AT-SPI | **18 drags, 14 assertion groups passed**, `TAB_WINDOWS_GUI_OK` |
| Native lifecycle | Tear-off, return while sibling workspaces remain, final tab to empty main closes only empty source window | Passed with same surface ID and original shell PID |
| Native ordering | Double-click rename → immediate drag, repeated tear-off, Ctrl+Alt+R, both tabs selectable | Passed without right-click workaround |
| Native targets | Body, right/down split across detached windows, tab before/after, new non-active main workspace row, Move to main window menu | Passed |
| Native cancellation | Outside drag + Escape; hover non-active workspace row + Escape, then another actual drop | Passed; original owner and visible main workspace unchanged on cancel |
| Independent processes | Drop onto foreign body and workspace row | Both models and original PID unchanged; no extra source window |
| Existing regression coverage | Surface lifecycle/SSH wrapper, live/deferred web tabs, file import, incremental split/collapse, render recovery and model SSH restrictions | Results recorded below |

The native script runs all ordering scenarios in one session. Fixtures wait for newly created tabs before sending title commands and wait for a loaded browser document before storing a JS marker. No failing user event sequence is replaced with a right-click or bypassed by rearranging its steps.

## Validation evidence

- Baseline ownership failure: `/tmp/fm-windows-u12763jp/test.log` (main requester, different focused window before dispatch).
- Final 240-case matrix and creation/rejection checks: `/tmp/fm-windows-nwal06rm/test.log`.
- Full native combined scenario: `/tmp/fm-gui-kx_rf7dn/` (event receipts and GUI logs, fatal GTK criticals enabled).
- DnD/renderer regression tests: 21 passed, `/tmp/fm-windows-3lb9wyco/test.log`.
- Workspace-wide clippy: passed with `-D warnings`; format and whitespace checks passed.
- Related final regressions passed: surface lifecycle 5, sidebar 29, keybindings 39, DnD/renderer 21, explicit file import 1, reorder dispatch 1, deferred web moves 1 and render recovery 1. The 4 audit tests include the 240-case matrix, 8 creation/focus cases, 9 move rejections and a queued request whose detached window has already closed.
- `cargo build --workspace --locked` passed. The CI runner fixture now includes the tab-window GUI suite in its expected suite list and phase counts; its regression/extended routing check passed.
- Final Linux gate's GUI binary: **946 passed, 0 failed, 4 ignored**. The subsequently added closed-window-request test passed separately. Gate prep, GUI harness and SSH fixture checks passed.
- Final gate finished with exit 101 after terminating the owned stalled CLI fixture. Its 57 completed test targets reported **2,121 passed, 0 failed, 13 ignored**; the CLI binary target did not complete and is excluded from those counts.
- Full gate is **not green**: unchanged CLI tests `codex_child_prompt_reactivates_the_observed_child_turn`, `codex_child_tool_progress_keeps_child_identity` and `codex_root_tool_progress_keeps_root_turn_identity` waited beyond 60 seconds for mock lifecycle traffic. They also stalled in a separate clean invocation. Only the owned stalled CLI test fixtures were terminated; the full-gate failure and logs are retained under `target/ci/linux/`.
- A clean CLI rerun excluding only those 3 stalled tests passed: 177 binary tests, 135 library tests (1 ignored), plus CLI integration tests. This filtered invocation is not a substitute for a successful complete gate; log: `/tmp/fm-windows-jum2h80o/test.log`.
- An earlier serial workspace run completed the other targets with 2,120 passing tests and 13 ignored tests; the GUI review handoff test timed out there. That review timeout was previously reproduced on an unchanged baseline; the final parallel gate's GUI binary passed it. Do not describe either complete invocation as all-green. The CLI/IPC/core/daemon sources are unchanged from `d4e5e56a`.

## Boundaries and remaining verification

- Executed platform: Linux/X11 under isolated Xvfb and D-Bus, separate runtime/config/state/data directories. The user's GUI and active agent session were protected throughout.
- Native Wayland was not executed: Xvfb does not provide a Wayland compositor or a consented input portal.
- macOS was not executed: there is no AppKit/main-thread native test host here. Its GestureDrag resolver is separate; sidebar/cancel parity needs native macOS verification and must not be inferred from the X11 result.
- Cross-process live session migration is unsupported. Safe rejection does not transfer a PTY, browser process or unsaved editor to another process.
- CLI `WorkspaceCreated` without a window target keeps its explicit main-window default; GUI creation routes now identify their requester.
