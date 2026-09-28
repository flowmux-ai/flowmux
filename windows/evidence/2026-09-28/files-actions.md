<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows file actions evidence — 2026-09-28

Five affected file-action cases and six related hidden native cases pass. F08 remains partial. This boundary verifies the debug implementation; no new release build, installer or installation is claimed. The installer in the earlier [Files evidence](files.md) belongs to F07 and does not contain this change. Physical keyboard/IME/focus and visual parity remain pending.

The [manifest](files-actions-diagnostics/manifest.json) preserves 108 normalized text artifacts, 28 runner records, 11 native cases, five historical source snapshots and three debug binary hashes. Source/copy SHA-256 values and original paths are retained; JSON normalization preserves values. The [collector](files-actions-diagnostics/d12dfc800baa-collect-files-actions-evidence.py) refuses changes to historical copies and skips nonterminal runs. Final source review remains separate from these recorded historical boundaries.

## Implemented boundary

Copy/Rename/Move target one retained regular file and one existing destination parent. Open source editor documents are rejected, including clean documents before Copy. Copy preserves raw bytes up to 16 MiB; rename/publication never replaces an existing destination. The worker pins source/parent handles; `NtCreateFile` and `NtSetInformationFile` keep mutations relative to those handles. One host-wide operation remains admitted through prepared/running/posted states, with four-second preparation and 30-second cooperative commit budgets. The host checks owner, deadline and editor state before acceptance; a retired IPC reply cannot submit commit.

Accepted operation IDs expose status/cancel and the latest 64 terminal receipts. Cancellation stays active until the worker returns; blocked OS calls are not forcefully interrupted. Close/editor operations reject while active. Worker cancellation may finish as `failed` with `cancel_requested: true`; cancellation is not assumed to undo successful publication. Matching-owner completion refreshes the tree; terminal receipts survive owner changes.

## Five affected native cases

Each case uses an isolated hidden owned host, explicit IPC, bounded probes and a 120-second Job. Every passing case records clean host exit and `activeAfterCleanup: 0`. LISTBOX observations are read-only system messages; Monaco reads inspect the actual document.

| Case | Observed behavior | Runner time |
| --- | --- | --- |
| [roundtrip](files-actions-diagnostics/6ee036044d39-native-files-actions-background.json) | Copy→Rename→Move under a root longer than 300 UTF-16 units; nested Korean/NFD/emoji paths, exact BOM/CRLF bytes, native labels and final Monaco Open. | [9.124 s](files-actions-diagnostics/b01422d538f4-result.json) |
| [copy-bytes](files-actions-diagnostics/541804b8f0d1-native-files-actions-background.json) | Long nested Unicode paths preserve both UTF-8 BOM/CRLF text and binary NUL bytes without opening an editor. | [3.737 s](files-actions-diagnostics/8a4158232e43-result.json) |
| [rejections](files-actions-diagnostics/02e5ed712368-native-files-actions-background.json) | Collisions, missing parents, invalid names and owned reparse paths reject without source/destination/outside-sentinel changes. | [4.611 s](files-actions-diagnostics/62e33b6554e7-result.json) |
| [editor-guard](files-actions-diagnostics/d4e30421bbc8-native-files-actions-background.json) | All three actions reject clean and dirty open source documents; disk bytes and Monaco identity/version remain unchanged. | [6.050 s](files-actions-diagnostics/e3eb17625502-result.json) |
| [stale-token](files-actions-diagnostics/e33dfbd7fe0b-native-files-actions-background.json) | Expired retained tokens and out-of-range indices cannot mutate files. | [3.240 s](files-actions-diagnostics/cbf16f723ea1-result.json) |

## Six related native cases

| Case | Observed behavior | Runner time |
| --- | --- | --- |
| [auto-refresh-clean](files-actions-diagnostics/3b5a5c07a263-native-editor-background.json) | Native watcher refreshes clean Unicode Monaco content. | [5.564 s](files-actions-diagnostics/1033a612d634-result.json) |
| [open](files-actions-diagnostics/3f62b521f6ae-native-editor-background.json) | Unicode Open and duplicate document identity. | [5.831 s](files-actions-diagnostics/b5f7f04f6e06-result.json) |
| [move](files-actions-diagnostics/05f8c2b2daff-native-editor-background.json) | Moving an editor preserves its view/document/undo history. | [5.787 s](files-actions-diagnostics/ff401eedd197-result.json) |
| [close](files-actions-diagnostics/420fb341ed8d-native-editor-background.json) | Dirty close guards and explicit save/discard. | [7.979 s](files-actions-diagnostics/43265363ea7c-result.json) |
| [search-open](files-actions-diagnostics/193ea729fd72-native-editor-search-background.json) | Guarded search result Open and actual UTF-16 selection. | [6.130 s](files-actions-diagnostics/540cf43b09db-result.json) |
| [selection](files-actions-diagnostics/01da4993d447-native-files-background.json) | Logical and native Files selection across collapse/range changes. | [3.968 s](files-actions-diagnostics/6940dc6c290d-result.json) |

## Source and verification boundaries

Four intermediate 187-file snapshots retain the [initial source](files-actions-diagnostics/6cad8df3e2c1-files-actions-source-initial.json), [native build source](files-actions-diagnostics/9770da53603c-files-actions-source-native.json), [NT rename correction](files-actions-diagnostics/b5870a15649d-files-actions-source-nt-fix.json) and [test fixture correction](files-actions-diagnostics/c716f57f1ca2-files-actions-source-test-fix.json). Initial pure tests preceded later target-specific fixes. The NT correction is the runtime used by the five file-action cases; the later Rust change is confined to the session test warmup. Verifier error matching and documentation also changed. The [final staged F08 snapshot](files-actions-diagnostics/27e53a2c768c-files-actions-source-final.json) is preserved as historical because subsequent visual-host edits may already be in progress. Shared Linux/macOS sources were not edited for F08.

| Check | Result |
| --- | --- |
| Pure Files subset | [20 passed; 65.669 s runner / 1.38 s body](files-actions-diagnostics/4bb1d676c89c-result.json) on the initial boundary. |
| Corrected native filesystem subset | [3 passed; 0.720 s](files-actions-diagnostics/5168b7eb15b7-result.json) including long paths, pinned handles, collision and precommit cancellation. |
| Debug build / corrected debug unit build | [112.322 s](files-actions-diagnostics/b6d8ff0f7b4b-result.json) / [94.195 s](files-actions-diagnostics/15cbdad29184-result.json), passed. |
| Final session test build / targeted session tests | [83.443 s](files-actions-diagnostics/438c8706773b-result.json) / [5 passed; 9.616 s runner / 9.25 s body](files-actions-diagnostics/dfe7837b61ca-result.json). |
| Verifier static / final Windows Clippy | [0.997 s](files-actions-diagnostics/d2336de98727-result.json) / [35.110 s](files-actions-diagnostics/8f9fc29dad57-result.json), passed. |

The complete 235-test invocation did **not** pass as one run: 232 passed and three session tests failed because the debug test directory lacked bundled ConPTY dependencies, followed by poisoned test-lock failures. After staging dependencies, an isolated cold-process handle-count assertion failed; warming the runtime before the baseline corrected that test, and all five session tests pass. These targeted corrections are not described as a fresh complete-suite pass.

## Preserved failures and limits

The first target Clippy attempt failed on `EM_SETLIMITTEXT` import and integer-bound lints (24.359 s); the next found large enum variants and a boolean lint (29.916 s). The corrected check passed (28.398 s). The final invocation initially failed before Cargo because an unquoted environment path treated `Files` as a command (0.108 s); the corrected invocation passes (35.110 s). All runner logs remain indexed.

The first native Files subset reported 25 passed/two failed in 0.951 s: the Win32 rename wrapper rejected handle-relative publication with OS error 87. Replacing that call with `NtSetInformationFile` corrected the production path; the three native filesystem tests and the five live file-action cases then passed. The 7.441-second complete unit failure and 1.593-second cold session rerun failure remain preserved alongside the final 9.616-second session pass. Failed checks are not hidden or relabeled.

All recorded native Job cleanup counts are zero; no separate stage-wide PID cleanup audit or new release/package was run at this boundary. Clipboard cut/paste, batch/directory/remote/reparse/case-sensitive paths, overwrite, physical input/IME, DPI/accessibility and visual parity remain unsupported or unverified. Tests do not force every budget, in-flight cancellation, permanently blocked filesystems, crash recovery or hostile concurrent reparse changes. Unicode comparisons are ordinal and do not establish NFC/NFD equivalence.
