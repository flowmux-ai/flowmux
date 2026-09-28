<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows editor automatic-refresh evidence — 2026-09-28

Eight new hidden automatic-refresh cases and nine related editor cases pass on the frozen runtime following commit `3f43fe2`. All 184 Windows unit tests, final debug/release builds, Clippy, format, three release doctor entrypoints and NSIS packaging pass. Final read-only cleanup distinguishes 725 absent recorded PIDs and three reused IDs. F04 and overall Windows acceptance remain partial; no physical desktop or IME readiness is claimed.

The [manifest](editor-refresh-diagnostics/manifest.json) maps 135 copied artifacts to exact source paths, raw SHA-256 and normalized SHA-256. Text copies use UTF-8 without BOM, LF, and no trailing whitespace or extra EOF blank lines; JSON values are unchanged. The [frozen snapshot](editor-refresh-diagnostics/frozen-source-hashes.json) covers 151 runtime, asset and verifier inputs, unchanged through final verification. Earlier [editor](editor.md) and [Open/picker](editor-open.md) evidence remains separate.

## Runtime boundary

Each editor owns a recursive `ReadDirectoryChangesW` watcher. Directory setup and notification I/O run off the UI thread. Initial readiness requires reconciliation; notifications and overflow are hints for forced-byte checks of open documents. One outstanding notice plus coalesced generations bounds delivery, with exact watcher epoch/generation acknowledgment. The host admits one new refresh per tick, one per editor, after 250 ms of editor quiet; due checkpoints take priority.

Refresh defers during commands, replacements, close/checkpoint/picker ownership, composition, pending edits, dialogs, diff state and focused Monaco widgets. Quick Open and workspace search share the guarded search dialog. It seals input, flushes ordered content/view state, then submits the existing bounded document worker. Clean advancing replacements update existing models directly without activating another document or calling focus. The frontend must acknowledge actual application before native bookkeeping releases the matching guard.

Surface/instance ownership survives moves and rejects stale callbacks after removal. The twelve-second deadline does not cancel filesystem calls: late admitted work retains its guard until reconciliation, or stays quarantined on synchronization failure. Watcher Drop signals stop without joining on the UI thread; the handle, buffer and OVERLAPPED remain alive until cancellation completes. Its two-second grace is diagnostic, not permission to free pending I/O.

`completed_count` and `applied_generation` describe delivered application, including partial errors. They do not prove all files were readable. A queued notice can leave `generation > applied_generation` while `pending` is false between operations.

## Eight automatic-refresh cases

Each case ran separately in a 120-second Windows Job with ordinary five-second CLI limits, explicit owned pipes and isolated fixture/profile/recovery roots. Actual hidden WebView2/Monaco reads follow application acknowledgment. None calls `editor check-disk` to satisfy its automatic-refresh wait. Every case records one passing check and Job `activeAfterCleanup: 0`.

| Case | Observed boundary | Runner time |
| --- | --- | --- |
| [Clean](editor-refresh-diagnostics/passed-auto-refresh-clean-native.json) | Unicode model advances; document/view identity, BOM/CRLF, external bytes and timestamp remain correct. | [6.457 s](editor-refresh-diagnostics/passed-auto-refresh-clean-runner.json) |
| [Inactive](editor-refresh-diagnostics/passed-auto-refresh-inactive-native.json) | Updating A preserves active B/tree; closing B reveals A's already-updated model without reopening its file. | [6.358 s](editor-refresh-diagnostics/passed-auto-refresh-inactive-runner.json) |
| [Conflict](editor-refresh-diagnostics/passed-auto-refresh-conflict-native.json) | Dirty text/version survive; Save refuses external changes; explicit Keep Mine/Save and a separate Reload resolve conflicts. | [9.278 s](editor-refresh-diagnostics/passed-auto-refresh-conflict-runner.json) |
| [Delete/recreate](editor-refresh-diagnostics/passed-auto-refresh-delete-recreate-native.json) | Deleted file is not recreated; old model survives, then external recreation updates the same identity. | [6.141 s](editor-refresh-diagnostics/passed-auto-refresh-delete-recreate-runner.json) |
| [Restored timestamp](editor-refresh-diagnostics/passed-auto-refresh-stamp-native.json) | Changed 67-byte content with identical restored UTC ticks still updates the actual model. | [6.298 s](editor-refresh-diagnostics/passed-auto-refresh-stamp-runner.json) |
| [Partial error](editor-refresh-diagnostics/passed-auto-refresh-partial-error-native.json) | Clean A advances despite locked B; B's old model/error/unknown status remain; A supports a following edit/save. | [6.757 s](editor-refresh-diagnostics/passed-auto-refresh-partial-error-runner.json) |
| [Move/close](editor-refresh-diagnostics/passed-auto-refresh-move-close-native.json) | Inactive moved view keeps identity/logical focus; post-close external write does not recreate it during observation. | [7.930 s](editor-refresh-diagnostics/passed-auto-refresh-move-close-runner.json) |
| [Coalescing/own save](editor-refresh-diagnostics/passed-auto-refresh-coalescing-native.json) | A 32-write burst reaches final bytes; own-save notification causes no false conflict or extra version bump. | [9.178 s](editor-refresh-diagnostics/passed-auto-refresh-coalescing-runner.json) |

The timestamp case records identical ticks `639261669885835472` and model version 2. The burst observed 64 generation advances and two completed refreshes for 32 writes, without assuming one event per write. Own-save remained clean at version 3. Move/close observed absence for 1,553 ms and records `forcedQueuedCallback: false`.

## Partial errors remain explicit

The shared poll can reload A and advance its version, then lose its accumulated messages when B fails. The Windows worker now preserves confirmed clean replacements alongside the original error, without forwarding InitializeEditor/OpenDocument or adding SetActiveDocument. The native locked-file case recorded OS error 32, A version 2 and B version 1 with `disk_status_known: false`. Closing B after unlock exposed A's existing model for an acknowledged edit/save; the case does not force every polling order or treat unlock as proof of health.

The public snapshot accessor may trust size/mtime even after a failed forced-byte poll consumed a dirty Modified transition. Recovery therefore never emits Unchanged from that weak snapshot. Previously known warnings remain, and ambiguous status is marked unknown. An empty successful retry or reactivation of an existing file may not replay that transition; uncertainty can persist until an authoritative status, replacement, save or genuinely fresh open. Repeated failed polls may mark a previously repaired document unknown again. Save still compares current bytes against its saved base before writing. An unreadable file can block polling later files.

Five new worker tests cover six variants: partial clean BOM/CRLF reload, dirty conflict, deletion, inactive selection, and equal-length/restored-timestamp changes with and without a previously known warning. Their deterministic unreadable fixture replaces an owned file with a directory, avoiding administrator/root permission assumptions.

## Nine related editor regressions

| Case | Runner time |
| --- | --- |
| [Open](editor-refresh-diagnostics/regression-open-native.json) | [5.124 s](editor-refresh-diagnostics/regression-open-runner.json) |
| [Edit/undo/redo/save](editor-refresh-diagnostics/regression-edit-native.json) | [5.010 s](editor-refresh-diagnostics/regression-edit-runner.json) |
| [Encoding](editor-refresh-diagnostics/regression-encoding-native.json) | [9.428 s](editor-refresh-diagnostics/regression-encoding-runner.json) |
| [Explicit conflict](editor-refresh-diagnostics/regression-conflict-native.json) | [4.938 s](editor-refresh-diagnostics/regression-conflict-runner.json) |
| [Move/undo identity](editor-refresh-diagnostics/regression-move-native.json) | [5.005 s](editor-refresh-diagnostics/regression-move-runner.json) |
| [Dirty-close guards](editor-refresh-diagnostics/regression-close-native.json) | [6.643 s](editor-refresh-diagnostics/regression-close-runner.json) |
| [Mixed restore](editor-refresh-diagnostics/regression-restore-native.json) | [8.007 s](editor-refresh-diagnostics/regression-restore-runner.json) |
| [Missing-root restore](editor-refresh-diagnostics/regression-missing-root-native.json) | [7.181 s](editor-refresh-diagnostics/regression-missing-root-runner.json) |
| [Failed checkpoint/unseal](editor-refresh-diagnostics/regression-checkpoint-failure-native.json) | [8.453 s](editor-refresh-diagnostics/regression-checkpoint-failure-runner.json) |

Each records one passing check: nine checks in 59.789 seconds, or 17 new native checks including refresh. No native case failed in this stage. These do not repeat every earlier browser/terminal/IPC/editor case. Checkpoint-failure proves failed replacement and later unsealing, not forced overlap with an earlier checkpoint.

## Builds, tests and package

| Check | Result |
| --- | --- |
| Linux worker/recovery/watcher subset | [30 passed; 41.097 s runner](editor-refresh-diagnostics/linux-worker-watch-pure-tests-runner.json), 0.11 s test body; no Windows API execution. |
| Adapter VM | [28 passed; 0.308 s](editor-refresh-diagnostics/final-adapter-tests-runner.json); spies/state checks, not physical IME. |
| Assets / verifier static | [Build 11.268 s](editor-refresh-diagnostics/final-assets-build-runner.json); [PowerShell/C# static 0.947 s](editor-refresh-diagnostics/final-verifier-static-runner.json). |
| Format / Windows Clippy | [1.109 s](editor-refresh-diagnostics/final-format-runner.json) / [17.285 s](editor-refresh-diagnostics/final-clippy-runner.json), passed. |
| Debug / release cross-build | [79.917 s](editor-refresh-diagnostics/final-debug-build-runner.json) / [100.480 s](editor-refresh-diagnostics/final-release-build-runner.json), passed. |
| Native unit build / execution | [Build 96.135 s](editor-refresh-diagnostics/final-native-unit-build-runner.json); [184 passed, 13.084 s runner](editor-refresh-diagnostics/final-native-tests-runner.json), [12.99 s body](editor-refresh-diagnostics/final-native-tests-stdout.txt), zero failed/ignored. |
| Release doctor | [flowmux.exe, flowmuxctl.exe, flowmux.com passed](editor-refresh-diagnostics/release-entrypoints.json); [0.985 s](editor-refresh-diagnostics/release-entrypoints-runner.json). |
| NSIS | [Passed, 11.055 s](editor-refresh-diagnostics/final-package-runner.json); installer 5,130,083 bytes. No installation ran. |

Windows unit execution includes real recursive Unicode watcher delivery/exact ACK/cancellation drain, missing-root failure without false readiness, and replacement notification beneath a root longer than 300 UTF-16 units. Four pure coalescer tests cover notification ownership. The [12-entry artifact inventory](editor-refresh-diagnostics/artifacts.json) includes executables, packaged documentation and main.js; setup SHA-256 is `0e7395a7d4f6fc3f9c97a83d956448b5dc7d057a9f53ee5067bc699b2c6e85d9`.

Two historical Clippy failures remain: [private method visibility, 14.989 s](editor-refresh-diagnostics/historical-clippy-private-method-runner.json), then [denied boolean lints, 14.800 s](editor-refresh-diagnostics/historical-clippy-lint-runner.json). Both precede the final freeze. The copied [verifier](editor-refresh-diagnostics/verifier-verify-editor.ps1), [fixture](editor-refresh-diagnostics/verifier-EditorFixture.cs), helpers and Job runner have raw hashes matching the frozen inputs.

## Cleanup and limitations

[Read-only cleanup](editor-refresh-diagnostics/final-cleanup.json) passed in [10.987 s](editor-refresh-diagnostics/final-cleanup-runner.json) at `2026-09-28T04:42:51.3409157Z`: 728 recorded IDs, 725 absent, three timestamp-proven reused. Two reuse checks used direct process timestamps; one used bounded Win32_Process.CreationDate fallback. A reused number happened to identify the cleanup runner, but its later creation proved it was not the earlier client. POSIX runner PIDs and unrecorded renderer descendants are excluded; Job cleanup counts are separate evidence. Existing WSL flowmux PID 787 was observed alive read-only.

The [inventory metadata](editor-refresh-diagnostics/cleanup-metadata.json), [script](editor-refresh-diagnostics/cleanup-script.ps1) and [generator](editor-refresh-diagnostics/cleanup-generator.py) preserve provenance. Initial preparation rejected a doctor runner object caught by a broad entrypoints filename pattern; excluding `*-runner.json` from that array pass fixed generation before native cleanup. This was not a native failure, and preparation was not failure-free.

Unicode checks compare Hangul/NFD/combining accents/emoji ordinally; actual model reads report `document_focused: false`. Logical editor visibility is separate from the verifier's hidden/nonforeground HWND checks. Physical IME, keyboard/mouse, actual focus transitions, glyph fidelity, dialogs, clipboard, accessibility, DPI and power-loss/interrupted-write behavior remain unverified. There is no automatic watcher restart, periodic fallback or watched-root rename detection; noisy recursive roots trigger all-open-document scans. Concurrent writers, network/UNC/reparse-point paths, exhaustive ACL/overflow behavior and permanently stalled driver cancellation are not fully covered. Move/close uses idle transitions and bounded observation, not every queued-callback race.
