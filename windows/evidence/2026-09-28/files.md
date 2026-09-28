<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows Files evidence — 2026-09-28

Seven distinct Files cases and five related editor regressions pass in hidden native Windows hosts. F07 and overall Windows acceptance remain partial. These checks observe actual LISTBOX state and Monaco content through owned handles and explicit pipes; they do not prove physical keyboard, IME, foreground focus or visual layout behavior.

The [manifest](files-diagnostics/manifest.json) contains 145 copied artifacts, 14 native attempts, 19 standalone checks and five source boundaries; it is frozen as `reviewed_complete` after independent artifact, source, scope and article checks. It retains completed passing, failed, cancelled and evidence-error attempts with source paths, raw hashes and normalized hashes. Text copies use UTF-8 without BOM, LF and no trailing whitespace or extra EOF blank lines; JSON values are preserved. The [collector](files-diagnostics/collector.py) also preserves raw verifier revisions in `files-diagnostics/source-raw`. Earlier [editor search](editor-search.md) evidence is separate.

## Implementation and source boundaries

The native child panel provides expansion/collapse, selection, explicit refresh, 500-row More and retained file Open. It preserves selection by exact path and distinguishes collapsed selections from native visible selection. Hidden/generated entries remain visible. No filesystem mutation, external opener, clipboard, automatic tree watcher or persisted tree restoration is implemented in this stage.

Directory reads run on one worker with eight admitted requests, including posted responses. Each request retains its original four-second budget; cancellation and shutdown do not join a blocked filesystem call. Reads use an opened directory handle, validate final-path containment, preserve ordinal Unicode and treat reparse entries as unsupported leaves. Limits include 20,000 entries, 64 expanded directories/depth, 16 KiB per path, four MiB retained path/name bytes, one MiB serialized page, and 32 pane caches/16 MiB combined accounting. These implementation limits are not all forced in native integration cases.

Five 178-file input snapshots preserve the sequence: [initial](files-diagnostics/initial-source-hashes.json), [Win32 import correction](files-diagnostics/native-source-hashes.json), [nested verifier extension](files-diagnostics/live-source-hashes.json), [lint/output correction](files-diagnostics/final-source-hashes.json), and [owner verifier correction](files-diagnostics/verifier-final-source-hashes.json). The first live nested/paging attempts use the earlier runtime; later cases and final builds follow equivalent `.is_multiple_of()` predicate corrections. The final verifier-only change replaces unsupported Unix `close-pane` cleanup with Windows `close-tab`. No shared Linux/macOS sources were edited for this stage.

## Seven passing Files cases

Each passing case uses an isolated owned hidden host, a 120-second Job, bounded startup/CLI probes and read-only LISTBOX system messages. Every passing case records clean host exit and Job `activeAfterCleanup: 0`.

| Case | Observed behavior | Runner time |
| --- | --- | --- |
| [nested](files-diagnostics/files-nested-0b647f0f-516d-491b-95e1-ede63fff2a99-native.json) | Nested slash-relative Korean/NFD/emoji paths under a long root; actual LISTBOX text and retained Open into new and reused Monaco documents. | [6.016 s](files-diagnostics/files-nested-0b647f0f-516d-491b-95e1-ede63fff2a99-runner.json) |
| [paging](files-diagnostics/files-paging-2de174b1-e848-469c-b9c4-be27b4c42205-native.json) | 1,105 entries appear in 500-row increments; status paging and native rows retain original indices. | [13.571 s](files-diagnostics/files-paging-2de174b1-e848-469c-b9c4-be27b4c42205-runner.json) |
| [selection](files-diagnostics/files-selection-67647490-1871-413c-b491-9f68cee5bcba-native.json) | Replace, toggle and range selection match native LISTBOX selection; collapsed descendants remain selected by path. | [4.668 s](files-diagnostics/files-selection-67647490-1871-413c-b491-9f68cee5bcba-runner.json) |
| [refresh](files-diagnostics/files-refresh-04f40591-b1be-48f7-948b-228deaefc28a-native.json) | Refresh retains Unicode selection by path; stale tokens and out-of-range Open are rejected. | [6.633 s](files-diagnostics/files-refresh-04f40591-b1be-48f7-948b-228deaefc28a-runner.json) |
| [confinement](files-diagnostics/files-confinement-22f7c869-b203-487c-a5fb-17fb8818847a-native.json) | Owned junction remains a non-activatable leaf; invalid roots fail without outside-target listing or fixture mutation. | [6.110 s](files-diagnostics/files-confinement-22f7c869-b203-487c-a5fb-17fb8818847a-runner.json) |
| [owner](files-diagnostics/files-owner-f331c7c2-1adc-41a4-bdeb-d45110974652-native.json) | Show/hide and retained Open stay with the original pane; an unrelated terminal is preserved. | [7.154 s](files-diagnostics/files-owner-f331c7c2-1adc-41a4-bdeb-d45110974652-runner.json) |
| [late-show](files-diagnostics/files-late-show-09251371-db66-418e-8059-cc20905454a6-native.json) | Expired UI-queued Show preserves root, token, selection and controls; a fresh Show succeeds. | [18.151 s](files-diagnostics/files-late-show-09251371-db66-418e-8059-cc20905454a6-runner.json) |

The late-show case deliberately pauses only its owned UI thread, leaving the IPC server running. Its actual server expiry takes 15,059 ms; suspend/resume counts are 0/1 and the thread resumes after 15,065 ms. This verifies queued admission expiry, not a permanently blocked worker or queued-close race. The nested case compares Unicode ordinally; it does not claim NFC/NFD equivalence.

## Five related editor regressions

The final-source regression runners total 35.380 seconds. Each records its passing native assertion, clean host exit and Job cleanup. The scope is these five scenarios, not a rerun of every prior Windows gate.

| Case | Runner time |
| --- | --- |
| [auto-refresh-clean](files-diagnostics/regression-auto-refresh-clean-1439ad92-3fab-40c0-92f5-2eeef16e80c5-native.json) | [7.354 s](files-diagnostics/regression-auto-refresh-clean-1439ad92-3fab-40c0-92f5-2eeef16e80c5-runner.json) |
| [open](files-diagnostics/regression-open-4607e9d7-e731-4a87-9830-6a514666aec9-native.json) | [7.198 s](files-diagnostics/regression-open-4607e9d7-e731-4a87-9830-6a514666aec9-runner.json) |
| [move](files-diagnostics/regression-move-853faf39-4015-4695-a724-31e4b9569ae4-native.json) | [6.101 s](files-diagnostics/regression-move-853faf39-4015-4695-a724-31e4b9569ae4-runner.json) |
| [close](files-diagnostics/regression-close-6869c836-858b-49b7-b01c-fed11d26fd14-native.json) | [8.354 s](files-diagnostics/regression-close-6869c836-858b-49b7-b01c-fed11d26fd14-runner.json) |
| [search-open](files-diagnostics/regression-search-open-9115147d-6446-4891-a841-09c488980bcb-native.json) | [6.373 s](files-diagnostics/regression-search-open-9115147d-6446-4891-a841-09c488980bcb-runner.json) |

## Unit, build and verifier checks

| Check | Result |
| --- | --- |
| Pure Files model/service | [13 passed; 48.012 s runner, 1.02 s test body](files-diagnostics/files-pure-e175d0c6-d782-4a49-afad-2f063dbf7a17-runner.json) before Win32 import correction. |
| Native Files unit subset | [15 passed; 0.567 s runner](files-diagnostics/files-native-unit-subset-b57d56db-62b9-4212-af70-a7c8a353438b-runner.json) including same-handle path replacement and nested slash/Unicode/long paths. |
| Complete Windows unit suite | [223 passed; 13.958 s runner, 13.52 s body](files-diagnostics/files-native-all-units-c22f0eab-54e8-44f6-a43e-0f3c4b9bbb5a-runner.json); zero failed or ignored. |
| Final debug / unit build | [89.206 s](files-diagnostics/files-debug-final-502ffa5c-4248-4239-8daf-1ed673f62949-runner.json) / [126.887 s](files-diagnostics/files-unit-build-final-58b15f53-bbb4-49de-96bf-7efa09eae767-runner.json), passed. |
| Final Clippy / format | [23.125 s](files-diagnostics/files-clippy-lint-fix-269a8183-86ba-4aa0-b6e8-ef03f636f2f7-runner.json) / [1.426 s](files-diagnostics/files-format-90dc79e3-a7ca-4bd8-b796-250b705b457a-runner.json), passed. |
| Intermediate verifier syntax/C# fixture | [0.837 s](files-diagnostics/files-script-check-final-ce781a02-b9b8-4d35-b264-9a0b01923d2c-runner.json), passed before later nested/cleanup/output script extensions. Final live cases execute the newer script. |
| Resumed release / doctor / NSIS | [111.935 s](files-diagnostics/files-release-resumed-e44054d0-fd4e-4316-962d-9d5dbd40222a-runner.json) / [1.956 s](files-diagnostics/files-release-doctor-59fcf8e1-59da-4a62-b801-7a2d13f7765b-runner.json) / [28.805 s](files-diagnostics/files-package-dc4a56ff-b29a-46cd-8fd8-e26b03fee806-runner.json), all passed. |

## Preserved nonpassing attempts

The first [Windows unit build failed in 22.799 s](files-diagnostics/files-unit-build-955e7d44-6fb1-4411-bf53-6adf8d6919ca-runner.json) because the `EnableWindow` import was missing. Correcting the import produced the passing 96.640-second unit build. The first [Clippy check failed in 22.038 s](files-diagnostics/files-clippy-226516d6-1b56-4643-8e42-3db14d0334bd-runner.json) on two manual modulo predicates; equivalent `.is_multiple_of()` expressions pass.

The first [paging driver attempt](files-diagnostics/run-21c564ac-0a33-47d9-b535-03e33850b03e.json) is retained as `evidence_error`: its native assertion and [10.377-second Job](files-diagnostics/files-paging-4637361e-5f26-4947-b72e-b1c717b57a7e-runner.json) passed, but oversized stdout prevented the driver from discovering the runner result. Bounded verifier output fixes the evidence path; the separate 13.571-second paging rerun passes. This historical attempt is not counted as a passing driver execution.

The [first owner attempt](files-diagnostics/files-owner-261b044c-710e-4ac1-90d0-fadf9584713b-native.json) failed in 7.068 seconds during verifier cleanup because Windows does not support `close-pane`. After using `close-tab`, the owner case passes in 7.154 seconds. The prior [release build was cancelled by SIGTERM at 129.450 s](files-diagnostics/files-release-fbe688ab-35c6-477c-b6cd-5534b44f37f0-runner.json); this terminal result is preserved without inferring why the signal occurred. The separate 111.935-second resumed release build passes.

## Packaging, cleanup and remaining limits

The [three release doctor entrypoints](files-diagnostics/release-entrypoints.json) exit 0 with Windows/WebView2 status `ok`; the [doctor helper](files-diagnostics/release-doctor.ps1) preserves the invocation. The [40-entry inventory](files-diagnostics/artifacts.json) and [inventory helper](files-diagnostics/inventory.py) record binaries, bundled files and documentation. NSIS produced a 5,763,136-byte installer with SHA-256 `dd1f91d7a6a8a6253f4552a774d362a31c1c8a319623b585521ae51ea824f699`. No installation ran.

[Read-only cleanup](files-diagnostics/final-cleanup.json) passed in [17.073 s](files-diagnostics/files-final-cleanup-3666f9d1-0166-46be-83cb-e16dda4b5dec-runner.json) at `2026-09-28T06:14:24.5814093Z`: 516 recorded PIDs across 56 source records, 513 absent and three timestamp-proven reused. All 32 recorded Job cleanup entries report `activeAfterCleanup: 0`; source/copy duplicates mean these are not 32 distinct Jobs. No process was stopped or modified. POSIX runners and the cleanup runner itself are excluded; unrecorded WebView descendants are covered by Job counts. The [metadata](files-diagnostics/cleanup-metadata.json), [script](files-diagnostics/cleanup-script.ps1) and [generator](files-diagnostics/cleanup-generator.py) preserve these inventory rules.

No visible test window, desktop input, clipboard access, physical IME trial or installation was performed. Native cases do not force hostile concurrent reparse swaps, ACL denial, malformed UTF-16 filenames, network roots, blocked directory I/O, in-flight cancellation, every budget, queued-close races, DPI, accessibility or physical focus transitions. Unit tests cover narrower deterministic worker/parser/model boundaries. Functional hidden controls and ordinal Korean/NFD data preservation must not be presented as full Korean IME or visual acceptance.
