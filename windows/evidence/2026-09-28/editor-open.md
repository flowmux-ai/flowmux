<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows editor Open and picker evidence — 2026-09-28

This stage follows the Monaco editor implementation committed as `83155e6`.
It adds asynchronous path preparation, a native Open File entry point and close
guards, and corrects long checkpoint replacement paths. Acceptance remains
partial: 57 partial/57 pending features and ten partial/three pending gates.
No feature or gate becomes passed. The [earlier editor record](editor.md) retains
its fifteen native cases and 27 related regression checks; those counts are not
new results for this stage.

Seven hidden editor cases and 36 related regression checks pass on the final
runtime source. Three additional editor passes precede the checkpoint correction;
they are historical evidence, giving ten unique editor cases across this stage.
Two earlier Restore failures remain preserved. Final-source debug/release,
Clippy, format, all 172 actual Windows unit tests, three release doctor
entrypoints and NSIS packaging pass. Final read-only cleanup verifies all 460
recorded process IDs: 457 absent and three reused. Physical picker, keyboard,
mouse, focus and IME behavior are unverified.

## Implementation boundary

Open captures its source surface, pane, workspace and root before submitting
filesystem work. A single preparer checks canonical roots, file existence and
containment off the UI thread. Before publication, the host rejects expired,
cancelled or closed/moved-source results. Prepared Open still creates or reuses
an editor in the captured pane. There is one pending preparation per pane and
eight admission slots per host; executing/queued work, cancelled jobs that have
not drained and posted results all retain slots. Cancelling a ticket cannot
interrupt an operating-system filesystem call already running.

CLI Open carries a twelve-second budget from server receipt through UI dispatch,
preparation and editor initialization. It does not acquire a fresh budget when
a modal dialog or WebView construction delayed dispatch. Expired preparation
cannot publish an editor. An operation already initializing an editor can still
finish after its caller times out; the existing pending sequence reconciles that
outcome. No automatic retry or forced filesystem-thread termination is added.

The sidebar **Open file…** action and `editor pick [--pane <pane>]` use an owned
`IFileOpenDialog`. It captures the workspace root/default folder, requires a
filesystem file, preserves valid Unicode and applies the normal root/path policy.
The dialog does not change process cwd. Windows may use its remembered folder
instead of the supplied default. A selected outside-root file is rejected.

`editor pick` acknowledges `picker_requested: true` with the captured pane/source
before entering the modal dialog. The receipt contains no editor surface and is
not an Open result. Human interaction therefore does not consume that IPC wait.
Cancellation leaves the layout alone; selection begins asynchronous Open with
a new operation budget and later failures reported through an owned desktop error
dialog. An expired requester is rejected before the dialog is queued. All picker
entry paths reject background mode, including the native wrapper's first check.

Native dialog interaction and WebView2 COM creation remain on the UI thread.
Application events queue during the modal dialog, so other IPC requests may
expire while the human is selecting a file. This is not complete asynchronous
desktop behavior or a bounded native API execution guarantee.

Accepted close state is retained while its reply drains. It blocks new mutations
and cancels pending Open preparations, including close paths without an editor
barrier or persistent state store. Direct accepted discard-state quit also exits
if its receiver has already expired. Pending preparations cannot publish into an
accepted close. These changes preserve dirty-editor guards; `--discard-state`
does not authorize discarding unsaved documents.

## Completed native checks before checkpoint correction

The [manifest](editor-open-diagnostics/manifest.json) maps each copy to its exact
source path, raw source SHA-256, normalized output SHA-256 and runner. Text copies
use UTF-8 without BOM and LF, with trailing whitespace/extra EOF blank lines
removed; JSON values are preserved. Earlier editor diagnostics are untouched.

| Case | Measured boundary | Runner time |
| --- | --- | --- |
| [async-open](editor-open-diagnostics/passed-async-open-native.json) | Two real CLI clients overlap; distinct captured roots/panes, actual Unicode Monaco values and original terminal identities are retained. Completion order is not forced. | [5.343 s](editor-open-diagnostics/passed-async-open-runner.json) |
| [picker-blocked](editor-open-diagnostics/passed-picker-blocked-native.json) | Exact background refusal; unchanged tree/editor identity, live terminal and zero visible owned top-level windows before/after. No native dialog runs. | [4.464 s](editor-open-diagnostics/passed-picker-blocked-runner.json) |
| [Open regression](editor-open-diagnostics/regression-open-native.json) | Actual Unicode model text/language, duplicate document identity and invalid-text isolation. | [6.458 s](editor-open-diagnostics/regression-open-runner.json) |

Each case records one passing check. The async case explicitly records
`forcedCompletionOrder: false`; it does not establish a specific preparation,
move, cancellation or close race. The picker case explicitly records
`dialogInteraction: false` and `continuousWindowObservation: false`; its snapshots
are not continuous proof that no transient HWND appeared. All three owned hosts
record clean exit code zero. This does not replace a final process inventory.

These runs used the frozen initial seventeen-case verifier, preserved as
[PowerShell](editor-open-diagnostics/initial-verifier-verify-editor.ps1),
[CLI probe](editor-open-diagnostics/initial-verifier-CliProbe.cs) and
[editor fixture](editor-open-diagnostics/initial-verifier-EditorFixture.cs).
Their exact raw source hashes match the copied
[snapshot manifest](editor-open-diagnostics/initial-verifier-snapshot-manifest.json).
Later additions to the tracked verifier do not retroactively extend these checks.

## Completed final-source native checks

| Case | Measured boundary | Runner time |
| --- | --- | --- |
| [Deep-path Restore](editor-open-diagnostics/passed-final-restore-native.json) | Checkpoint and mixed-editor restoration pass in the deeper immutable snapshot directory after path and startup-wait corrections. | [12.449 s](editor-open-diagnostics/passed-final-restore-runner.json) |
| [late-open](editor-open-diagnostics/passed-late-open-native.json) | UI-queued Open expires at the real IPC deadline, creates no editor, drains admission, preserves the terminal/tree and permits a following Open. | [19.416 s](editor-open-diagnostics/passed-late-open-runner.json) |
| [close-preparing](editor-open-diagnostics/passed-close-preparing-native.json) | Accepted quit cancels Open while its owned preparation worker is paused; after resumption the late result drains without a tab before the held quit peer is released. | [5.208 s](editor-open-diagnostics/passed-close-preparing-runner.json) |
| [late-quit-empty](editor-open-diagnostics/passed-late-quit-empty-native.json) | Direct terminal-only quit still exits cleanly after its IPC reply expires; no editor barrier supplies the close path. | [18.367 s](editor-open-diagnostics/passed-late-quit-empty-runner.json) |
| [late-quit](editor-open-diagnostics/passed-final-late-quit-native.json) | Clean editor quit exits after the real server deadline and balanced owned UI-thread resume. | [19.982 s](editor-open-diagnostics/passed-final-late-quit-runner.json) |
| [close](editor-open-diagnostics/passed-final-close-native.json) | Dirty tab/workspace/quit refusal retains focus; explicit save/discard permits closure. | [6.341 s](editor-open-diagnostics/passed-final-close-runner.json) |
| [checkpoint-failure](editor-open-diagnostics/passed-final-checkpoint-failure-native.json) | Failed replacement preserves the old checkpoint, releases the model seal and permits later clean quit. | [7.525 s](editor-open-diagnostics/passed-final-checkpoint-failure-runner.json) |

Late Open observed a **15.068 s** server deadline, and empty-editor quit observed
**15.069 s**. Each paused only the exact owned hidden UI thread, recording previous
suspend count **0** and resume count **1**, with no whole-process suspension.
Late Open then recorded zero pending/admitted/editor counts and a successful
following Open. Both cases recorded clean, unforced host exit.
The editor-present late-quit case observed **15.065 s**, also with suspend **0**,
resume **1** and clean unforced exit.

Close-preparing paused the exact owned thread named `flowmux-editor-open`, again
with suspend **0** and resume **1**. Pending/admitted counts changed from **1/1**
before quit to **0/1** at acceptance and **0/0** after resume. The real quit peer
remained held for **17 ms** while the cancelled result drained; the fixture
observed zero editors and the original terminal before host exit, then released
the peer and observed clean exit. This verifies that particular accepted-close
window, not arbitrary filesystem-call interruption or every cancellation timing.

## Preserved Restore failure and checkpoint correction

The [initial Restore attempt](editor-open-diagnostics/failed-snapshot-restore-native.json)
failed in [6.522 seconds](editor-open-diagnostics/failed-snapshot-restore-runner.json).
Its `save-state` request reported an atomic window-state save failure with Windows
OS error 3. It recorded no passing check. Its runner, native JSON and logs are
preserved without replacing the failure with a later summary.

The parent's source/path investigation found that the deeper snapshot directory
produced a temporary checkpoint filename of **266 UTF-16 units**, although the
destination was **230 units**. Rust created the temporary file, but the old
`MoveFileExW` call received unprefixed paths. The Windows checkpoint writer now
canonicalizes the existing parent and uses its extended-length path for both the
temporary and destination filenames. The same-directory replacement, previous
checkpoint preservation on failure and externally reported path are retained.
Microsoft documents a default `MAX_PATH` limit for `MoveFileExW` and support for
the `\\?\` prefix to extend it. [MoveFileExW documentation](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw).
The [next Restore attempt](editor-open-diagnostics/failed-restored-startup-native.json)
passed the earlier checkpoint-save step and reached restart, but failed in
[11.472 seconds](editor-open-diagnostics/failed-restored-startup-runner.json) when
its initial `identify` exceeded the verifier's nested five-second request wait.
The outer startup budget was eight seconds. The verifier was corrected to
give that request only the remaining startup budget, without changing ordinary
command deadlines or retrying the timed-out request. That verifier correction
has passed static checks. In the failed attempt, later cleanup reached the
restored editor's status; close still reported loading, so cleanup forcibly
terminated that exact owned host with exit code -1. This run contains no passing
Restore check and is not clean-exit evidence.

Subsequent source review also found an IPC deadline/reply admission race. The
reply sender and receiver now share a gate: retirement either consumes the
already-committed response or prevents a later sender from claiming success.
The editor also checks elapsed time when completion arrives, including a result
queued ahead of the periodic timeout tick. The final lifecycle cases above
exercise late Open/quit and accepted-close behavior without erasing either
earlier failure.

The final deep-path Restore run completed successfully. Its restored host was
discovered in **75 ms**; identity had **7,925 ms** remaining and answered in
**6,096 ms**. Total startup was **6,167 ms**, below the unchanged **8,000 ms**
budget. That identity duration exceeds the former nested five-second limit,
confirming the verifier mismatch in this scenario without increasing the global
budget. The other fresh hosts started in **1,037 ms** and **909 ms**. All three
exited cleanly without force. Exhaustive path/ACL or power-loss behavior remains
unverified.

## Build and static evidence

| Check | Result and source boundary |
| --- | --- |
| Linux Open-preparer domain subset | [Nine passed, 41.812 s](editor-open-diagnostics/linux-open-preparer-tests-runner.json); [test output](editor-open-diagnostics/linux-open-preparer-tests-stdout.txt). Does not run Windows COM or dialogs. |
| Initial Windows release Clippy | [Passed, 18.298 s](editor-open-diagnostics/before-receipt-close-fixes-clippy-runner.json), before server-receipt/accepted-close/direct-quit fixes and the checkpoint correction. |
| Initial Windows debug cross-build | [Passed, 72.492 s](editor-open-diagnostics/before-direct-quit-fix-debug-build-runner.json), before direct-quit and checkpoint corrections. |
| Debug cross-build after receipt/close fixes | [Passed, 69.240 s](editor-open-diagnostics/final-debug-build-runner.json), before checkpoint correction. Its original copied filename contains “final”; the manifest marks it historical. |
| Release cross-build after receipt/close fixes | [Passed, 93.386 s](editor-open-diagnostics/release-build-runner.json), before checkpoint correction. |
| Debug cross-build after checkpoint correction | [Passed, 71.689 s](editor-open-diagnostics/post-checkpoint-fix-debug-build-runner.json), before atomic IPC reply/deadline and completion-time checks. |
| Frozen initial verifier static check | [Passed, 0.920 s](editor-open-diagnostics/initial-verifier-static-runner.json). This is not native dialog evidence. |
| Lifetime helper static check | [Passed, 0.646 s](editor-open-diagnostics/lifetime-helper-verifier-static-runner.json); PowerShell parse and three C# helpers compiled, with no host/fixture execution. |
| Startup-budget verifier static check | [Passed, 0.852 s](editor-open-diagnostics/startup-budget-verifier-static-runner.json); the same static boundary after the startup wait correction. |
| Final-source Windows Clippy | [Passed, 41.060 s](editor-open-diagnostics/final-clippy-runner.json), after checkpoint and IPC/completion fixes; all Windows targets with warnings denied. |
| Final-source format check | [Passed, 8.266 s](editor-open-diagnostics/final-format-runner.json). |
| Final-source Windows debug cross-build | [Passed, 73.506 s](editor-open-diagnostics/final-source-debug-build-runner.json), after all runtime corrections described above. |
| Final-source Windows release cross-build | [Passed, 98.613 s](editor-open-diagnostics/final-source-release-build-runner.json). |
| Final native unit executable build | [Passed, 87.243 s](editor-open-diagnostics/final-native-unit-build-runner.json). |
| Actual Windows unit execution | [172 passed, 12.774 s runner](editor-open-diagnostics/final-native-tests-runner.json), [12.69 s test body](editor-open-diagnostics/final-native-tests-stdout.txt); zero failed/ignored. Includes nine preparer, five reply-handoff and one long-checkpoint-path additions. |
| Release entrypoints | [Three passed](editor-open-diagnostics/final-release-entrypoints.json); [runner, 0.923 s](editor-open-diagnostics/final-release-doctor-runner.json). `flowmux.exe`, `flowmuxctl.exe` and `flowmux.com` doctor exit zero; this does not launch a GUI or run the installer. |
| NSIS package | [Passed, 11.362 s](editor-open-diagnostics/final-package-runner.json); setup is 5,108,021 bytes. No installation or foreground execution was performed. |

The [artifact inventory](artifacts-editor-open.json) records SHA-256 and sizes for
three debug entrypoints, three release entrypoints, the native unit executable
and installer; all eight matched current files when collected. The setup SHA-256
is `1856bff8b7d33fe335249b59c1b60f7aaae58c07b4e806a4cfd3c557549d5056`.
The frozen EDITOR.md, IMPLEMENTATION.md and acceptance.json hashes remained
unchanged through packaging. Package creation does not establish installer or
physical desktop acceptance.

The final-v1 verifier is separately frozen in its
[snapshot manifest](editor-open-diagnostics/final-v1-verifier-snapshot-manifest.json),
with [PowerShell](editor-open-diagnostics/final-v1-verifier-verify-editor.ps1),
[CLI probe](editor-open-diagnostics/final-v1-verifier-CliProbe.cs),
[fixture](editor-open-diagnostics/final-v1-verifier-EditorFixture.cs) and
[lifetime helper](editor-open-diagnostics/final-v1-verifier-EditorOpenLifetime.cs).
All four raw hashes match the snapshot and its tracked sources at collection.
This preserves the startup-budget and advanced-case definitions used by the
final-source checks above.

The nine domain tests cover Unicode root/file resolution and containment, failed
admission, executing/queued/posted capacity, cancellation around resolution,
original deadlines, nonblocking shutdown and permit release. Their injected
resolver exists only in test code. Linux passes do not establish Windows path
or native integration behavior. POSIX runner PIDs must not be read as Windows
process IDs.

## Related final-source native regressions

| Suite | Checks | Runner time |
| --- | --- | --- |
| [State](editor-open-diagnostics/regression-state-native.json) | 8 | [40.041 s](editor-open-diagnostics/regression-state-runner.json) |
| [Panes](editor-open-diagnostics/regression-panes-native.json) | 7 | [13.802 s](editor-open-diagnostics/regression-panes-runner.json) |
| [Workspaces](editor-open-diagnostics/regression-workspaces-native.json) | 6 | [11.193 s](editor-open-diagnostics/regression-workspaces-runner.json) |
| [IPC](editor-open-diagnostics/regression-ipc-native.json) | 6 | [7.309 s](editor-open-diagnostics/regression-ipc-runner.json) |
| [IPC limits](editor-open-diagnostics/regression-ipc-limits-native.json) | 3 | [36.728 s](editor-open-diagnostics/regression-ipc-limits-runner.json) |
| [Extended browser wait](editor-open-diagnostics/regression-browser-wait-native.json) | 6 | [41.443 s](editor-open-diagnostics/regression-browser-wait-runner.json) |

The total is **36** related checks. State evidence has no top-level status field;
its bounded successful runner and eight recorded checks are retained as emitted.
Extended wait includes a **27,035 ms** wait beyond the default server/client
budgets without retransmission. These suites retain their physical/lifecycle
limits and are separate from the editor case count.

## Cleanup and remaining acceptance

The [final read-only inventory](native-editor-open-cleanup.json) passed in
[8.005 seconds](editor-open-diagnostics/final-cleanup-runner.json), after a
[0.533-second parser check](editor-open-diagnostics/cleanup-static-runner.json).
At `2026-09-28T04:14:12.5011125Z`, **457 of 460** recorded Windows PIDs were absent;
the other **three** were proven reused by creation timestamps later than their
latest recorded stage finishes. Two used `Get-Process.StartTime`, and one used
the bounded PID-filtered `Win32_Process.CreationDate` fallback. No entry remained
unverified or possibly alive, and no process was controlled by the inventory.
Names alone were never accepted as reuse evidence.

The [generator](editor-open-diagnostics/cleanup-generator.py),
[executed script](editor-open-diagnostics/cleanup-check.ps1) and
[input metadata](editor-open-diagnostics/cleanup-inputs.json) are preserved with
raw and normalized hashes. They cover explicitly recorded native hosts, shells,
clients, browser-wait clients, IPC-limit host/shell IDs, Windows runners and release
entrypoints. POSIX runners and the inventory's own cleanup runners are excluded.
WebView renderer descendants are not individually inventoried; native runner job
cleanup counts remain separate evidence. The earlier editor-stage cleanup record
was not overwritten.

Physical picker selection/cancellation, focus, keyboard/mouse/IME, glyph fidelity,
clipboard, DPI and accessibility remain open. WebView construction is still
UI-bound. Automatic file watching, Quick Open/workspace search, native clipboard
integration, renderer-crash recovery, unacknowledged edit/close races, concurrent
writers, network/UNC/reparse paths, full ACL preservation and power-loss durability
retain their earlier limits. F01–F05, F17 and G08 remain partial.
