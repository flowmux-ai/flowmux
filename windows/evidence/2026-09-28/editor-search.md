<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows editor search evidence — 2026-09-28

Six distinct hidden native search cases pass across eight executions, alongside ten related editor cases: 18 passing native case checks. Results span three explicit source boundaries. Five original search runs and all ten regressions precede a deadline correction; two search runs follow it. A native unit failure then exposed a Windows nested-path defect. Its separator correction passes the targeted test, all 208 Windows unit tests and the affected nested-path Monaco rerun. Final debug/release, Clippy, format, three release doctor entrypoints, NSIS packaging and read-only cleanup pass. F06 and overall Windows acceptance remain partial. No physical keyboard, IME or foreground UI readiness is claimed.

The [manifest](editor-search-diagnostics/manifest.json) maps 188 copied artifacts to exact source paths, raw SHA-256 and normalized SHA-256. Text copies use UTF-8 without BOM, LF and no trailing whitespace or extra EOF blank lines; JSON values are preserved. The [initial 173-file snapshot](editor-search-diagnostics/pre-deadline-review-source-hashes.json) covers the first native search and regression runs. The [deadline-corrected snapshot](editor-search-diagnostics/post-deadline-pre-separator-source-hashes.json) changes three inputs: the native editor handler, native search handler and verifier. The [separator-corrected snapshot](editor-search-diagnostics/final-source-hashes.json) changes the scanner and native verifier from that boundary. Snapshots matched all current input hashes at their respective collection boundaries. Earlier [editor](editor.md), [Open/picker](editor-open.md) and [automatic-refresh](editor-refresh.md) evidence remains separate.

The [collector](editor-search-diagnostics/collector.py) preserves completed attempts and refuses changed historical copies. After the verifier advanced, its two older raw versions were reconstructed solely by reversing BOM/newline normalization and verified against their original recorded SHA-256 values; the manifest retains original and archived source paths. They are not presented as the final verifier.

## Implementation boundary

Literal Find, Replace Match and Replace All use the actual Monaco model. Query text is bounded to 4,096 UTF-8 bytes; the literal CLI path rejects regex. Case, whole-word and backward options are available. Results and passive `read.selection` observations use one-based UTF-16 positions. At most 500 ranges are returned, probing a 501st to report truncation; next/previous matching still searches the complete model. Replace All refuses truncated results. Replacements require a matching document ID and acknowledged version, preflight the resulting 16 MiB size limit, and use one undo transaction plus the existing native synchronization seal. Find does not call focus.

Quick Open indexes files without flushing editor buffers. Workspace search first flushes content and captures acknowledged buffers, then searches off the UI thread. Open buffers override disk content. Surface, native instance, request ID and generation pin each operation; cancellation does not pretend to stop blocked filesystem calls. Quick Open retains at most 2,000 paths and workspace search at most 500 matches. Paths are bounded to 16 KiB UTF-8. The scanner reserves 64 KiB within its one MiB serialized result/source budget for envelope and diagnostic overhead. Workspace protocol ranges remain zero-based UTF-16.

Native result retention uses a token and exact original index. The Windows frontend maps displayed Quick Open rankings back to that original entry. Workspace result opening validates containment, retained source bytes, and open-buffer identity/version on the worker before revealing. The shared reveal handler converts coordinates to Monaco's one-based positions. Its dialog remains open until native model application and final synchronization complete; errors retain results and display bounded plain text. The legacy path-only result-open message cannot bypass retained-token validation.

The adapter defers query/include/exclude work during composition and avoids handling Enter as a result-open request while composition is active. Quick Open keeps its query-independent index during composition. These paths have VM tests; the native cases below use CLI commands and do not physically exercise dialog input, clicks or IME. Hidden initialization suppresses HTMLElement focus and modal-dialog focus before the shared frontend loads.

## First five native search cases

Each case uses a 120-second Windows Job, isolated fixture/profile/recovery roots, an explicit owned pipe and actual hidden WebView2/Monaco reads. All five record one passing check, clean host exit 0 and Job `activeAfterCleanup: 0`. The total runner time is 27.034 seconds. These runs precede the SearchOpen deadline correction described below.

| Case | Observed behavior | Runner time |
| --- | --- | --- |
| [Find](editor-search-diagnostics/passed-find-native.json) | Ordinal Korean, emoji and NFD matches report actual one-based UTF-16 ranges; literal metacharacters do not become regex; content/version/disk bytes remain unchanged. | [4.625 s](editor-search-diagnostics/passed-find-runner.json) |
| [Replace](editor-search-diagnostics/passed-replace-native.json) | Version-pinned single/all replacement, literal `$1`, emoji, stale-version rejection, actual undo/redo and exact BOM/CRLF save. | [5.164 s](editor-search-diagnostics/passed-replace-runner.json) |
| [Quick Open](editor-search-diagnostics/passed-quick-open-native.json) | Unicode paths, hidden/generated/ignore exclusions, rejected token/index and actual opening of the retained entry. | [4.679 s](editor-search-diagnostics/passed-quick-open-runner.json) |
| [Workspace](editor-search-diagnostics/passed-workspace-native.json) | Dirty Monaco content overrides disk, zero-based UTF-16 ranges, regex/include/exclude behavior, invalid-pattern error and idle cancellation without mutation. | [5.195 s](editor-search-diagnostics/passed-workspace-runner.json) |
| [Guarded result open](editor-search-diagnostics/passed-search-open-native.json) | Actual `read.selection` proves reveal; changed disk/buffer and expired tokens are rejected while valid dirty-buffer content is preserved. | [7.371 s](editor-search-diagnostics/passed-search-open-runner.json) |

The guarded-open case records selection 1:4–8 for `GOLD` after a leading emoji/space and 1:1–5 in an edited buffer. NFD strings are compared ordinally; the verifier does not claim NFC/NFD equivalence. Stale disk changes happen before opening, rather than forcing a swap between validation and the shared read; that latter case has a deterministic worker test. Idle cancellation is not an in-flight cancellation proof.

## Ten related cases at the historical source

The [related run manifest](editor-search-diagnostics/run-741cc686-a7b2-4af9-a3e4-7a860e7512fb.json) records ten checks in 71.222 seconds. All observed host exits are clean and each Job records no active processes after cleanup. These checks were completed before the narrowly scoped SearchOpen deadline correction; they are not a claim that every prior Windows scenario was rerun.

| Case | Runner time |
| --- | --- |
| [Automatic clean refresh](editor-search-diagnostics/regression-auto-refresh-clean-native.json) | [7.039 s](editor-search-diagnostics/regression-auto-refresh-clean-runner.json) |
| [Automatic dirty conflict](editor-search-diagnostics/regression-auto-refresh-conflict-native.json) | [9.611 s](editor-search-diagnostics/regression-auto-refresh-conflict-runner.json) |
| [Automatic partial error](editor-search-diagnostics/regression-auto-refresh-partial-error-native.json) | [6.995 s](editor-search-diagnostics/regression-auto-refresh-partial-error-runner.json) |
| [Edit/undo/redo/save](editor-search-diagnostics/regression-edit-native.json) | [6.148 s](editor-search-diagnostics/regression-edit-runner.json) |
| [Encoding](editor-search-diagnostics/regression-encoding-native.json) | [8.916 s](editor-search-diagnostics/regression-encoding-runner.json) |
| [Explicit conflict](editor-search-diagnostics/regression-conflict-native.json) | [5.335 s](editor-search-diagnostics/regression-conflict-runner.json) |
| [Move](editor-search-diagnostics/regression-move-native.json) | [5.122 s](editor-search-diagnostics/regression-move-runner.json) |
| [Dirty close](editor-search-diagnostics/regression-close-native.json) | [6.514 s](editor-search-diagnostics/regression-close-runner.json) |
| [Restore](editor-search-diagnostics/regression-restore-native.json) | [7.617 s](editor-search-diagnostics/regression-restore-runner.json) |
| [Checkpoint failure](editor-search-diagnostics/regression-checkpoint-failure-native.json) | [7.925 s](editor-search-diagnostics/regression-checkpoint-failure-runner.json) |

## Static and build evidence collected so far

| Check | Result and source boundary |
| --- | --- |
| Adapter VM | [43 passed; 1.614 s](editor-search-diagnostics/editor-find-selection-adapter-tests-a2ba0456-1497-4646-ade7-6a64498fad6f-runner.json), including passive selection observation. |
| Linux editor/scanner/worker subset | [56 passed; 43.969 s runner](editor-search-diagnostics/editor-search-pure-beb4cf70-381e-4ec2-9792-69555ca61d1f-runner.json), 0.60 s test body. Does not compile Windows-only native handlers. |
| Offline assets | [Passed; 12.276 s](editor-search-diagnostics/editor-search-assets-final-0275fb29-c3ac-4729-ba70-908020cf9766-runner.json), including selection observation. |
| Initial native verifier static | [Passed; 0.683 s](editor-search-diagnostics/editor-search-verifier-static-final-1b63e033-9a85-4637-96bb-e0c6c8047a62-runner.json), before the later timeout case. |
| Debug build after enum correction | [Passed; 72.736 s](editor-search-diagnostics/editor-search-debug-match-fix-693d1456-7937-4fd9-b30c-5d43b62770ef-runner.json), before the later deadline correction. |
| Deadline-corrected debug / Windows Clippy | [83.520 s](editor-search-diagnostics/editor-search-debug-deadline-24ea5794-95f6-44a5-bf56-4e0f8d6205a2-runner.json) / [21.838 s](editor-search-diagnostics/editor-search-clippy-final-80899429-7d4a-46c8-8306-1fdac12f7b3b-runner.json), passed before the separator correction. |
| Six-case verifier static before nested-path extension | [Passed; 0.581 s](editor-search-diagnostics/editor-search-late-verifier-static-829d2649-4ab8-4472-86d3-499e50461d7b-runner.json). |
| Separator-corrected native unit build | [Passed; 99.633 s](editor-search-diagnostics/editor-search-unit-build-path-fix-78a756e8-f3b3-44f5-bbd6-4ec76f84d876-runner.json). |
| Corrected native path test / complete Windows unit suite | [1 passed; 0.138 s](editor-search-diagnostics/editor-search-native-path-test-c78fe37c-2b43-4bbc-b84f-adac13710809-runner.json), then [208 passed; 13.135 s runner](editor-search-diagnostics/editor-search-native-tests-path-final-dd67900b-9313-4877-9be4-8efd92038ba1-runner.json), 13.06 s test body; zero failed or ignored. |
| Final separator-corrected debug / Clippy / format | [82.702 s](editor-search-diagnostics/editor-search-debug-path-final-2580d04a-8f85-4bcc-a387-463b03b47753-runner.json) / [22.328 s](editor-search-diagnostics/editor-search-clippy-path-final-fe447e22-df8b-4a2b-ab1d-a6ae105c68f2-runner.json) / [1.210 s](editor-search-diagnostics/editor-search-format-path-final-e0c6fc89-a25f-4ee7-9c75-6d7fccad41dc-runner.json), all passed. |
| Final release build | [Passed; 108.749 s](editor-search-diagnostics/editor-search-release-path-final-73ee61f0-0a14-4020-8fb1-163cfa5a5fbf-runner.json). |
| Release doctor | [All three entrypoints passed](editor-search-diagnostics/release-entrypoints.json); [1.270 s](editor-search-diagnostics/editor-search-release-doctor-4e1fe5c6-6021-4619-a8e3-e8ba080d1ace-runner.json). |
| NSIS package | [Passed; 12.776 s](editor-search-diagnostics/editor-search-package-b4623788-c464-4567-bc8d-21a4c99b9ec3-runner.json); installer 5,652,833 bytes. No installation or foreground execution ran. |

The [12-entry artifact inventory](editor-search-diagnostics/artifacts.json) records current executables, packaged documentation and frontend hashes. Installer SHA-256 is `e1c34ec80fbcfb6b70637854fa96157d6fd6fa4480afc1568b16456325b1d836`.

The adapter suite imports Monaco's actual search parser/matching modules while stubbing the editor/host surface. It covers query validation, matching options, 500/501 bounds, result-byte preflight, undo grouping, stale document/version rejection, token/index remapping, UI completion ownership, bounded errors and composition deferral. It does not run WebView2 or prove physical input behavior.

## Preserved failures and later correction

The [first literal-search adapter run](editor-search-diagnostics/editor-find-adapter-tests-b8adde1d-ece0-4160-8048-401fd06fd2f2-runner.json) failed one assertion in 1.617 seconds: the test expected a Korean word at UTF-16 column 9, while its actual column was 8. Correcting that expectation led to the later passing suites; the original stdout/stderr are retained.

The [first Windows debug build](editor-search-diagnostics/editor-search-debug-d35e9660-a11b-4bfa-b17f-7648700482ad-runner.json) failed with E0004 after 33.543 seconds because ordinary Open submission lacked an arm for the new `Completion::SearchUi`. Read-only review identified the missing match arm; its correction explicitly rejects that completion on ordinary Open, keeping retained-result validation mandatory. The subsequent 72.736-second debug build passed.

A later lifecycle review found that SearchOpen could renew an IPC receipt deadline, submit worker work after its initial barrier expired, or apply a late reveal. The correction retains the receipt deadline and checks it before worker submission and late reveal. It is scoped to the new search-open operation. The original 173-file snapshot and earlier native results remain explicitly pre-correction.

After the deadline correction, the [guarded-open rerun](editor-search-diagnostics/post-deadline-search-open-native.json) passed in [5.990 s](editor-search-diagnostics/post-deadline-search-open-runner.json). The new [late-search-open case](editor-search-diagnostics/passed-late-search-open-native.json) passed in [20.321 s](editor-search-diagnostics/passed-late-search-open-runner.json). It suspended only the verified UI thread of its owned hidden host, leaving the IPC server running. The real IPC deadline replied after 15,051 ms; suspend/resume counts were 0/1 and the thread resumed after 15,060 ms. Draining the expired request preserved the original document and selection, one open model, no pending operation and no seal. A following fresh search/open succeeded with the expected range; the host exited cleanly. This forces queued admission expiry, not a blocked worker or a late renderer reveal, so those latter race boundaries are not claimed as native coverage.

The earlier [208-test native run](editor-search-diagnostics/editor-search-native-tests-8005d6f5-ef8c-489f-9492-3beeefaf52bb-runner.json) failed in 13.811 s: 207 passed and `native_same_handle_unicode_long_paths_and_ordinal_identity` failed with OS error 123 while validating a search result source. The scanner publishes relative paths with `/`; its verbatim Win32 path needed `\` separators. This is a production runtime defect, not a fixture-only failure. The correction normalizes separators before the verbatim prefix, extends the long-path unit matrix and makes the native guarded-open fixture use a nested Unicode path. The corrected targeted and complete Windows unit runs pass. The concurrent [release build](editor-search-diagnostics/editor-search-release-0a55d7d2-e3d0-45b3-ad0a-a85974948efd-runner.json) was intentionally stopped with SIGTERM after the unit failure at 47.564 s (exit -15); it is not a successful release build or an unexplained compiler failure.

On the separator-corrected source, [nested Unicode SearchOpen](editor-search-diagnostics/final-search-open-native.json) passed in [6.265 s](editor-search-diagnostics/final-search-open-runner.json). It opens `nested 한글/선택 한 😀.txt` and observes the actual revealed range, while retaining the changed-disk, stale-buffer and expired-token assertions. This is the same guarded-open case with stronger nested-path coverage, not an additional distinct case. Its host exits cleanly and the Job reports no active processes after cleanup. The earlier ten regressions were not rerun after this separator-only correction.

## Cleanup and remaining limitations

[Read-only cleanup](editor-search-diagnostics/final-cleanup.json) passed in [10.082 s](editor-search-diagnostics/editor-search-cleanup-90ac427e-82d3-469e-83f1-b85ac2b02576-runner.json) at `2026-09-28T05:31:21.3703300Z`: 691 recorded PIDs from 46 sources, 688 absent and three timestamp-proven reused. Two reuse checks used direct process start times and one used bounded Win32_Process.CreationDate fallback. No process was stopped or modified. POSIX runner IDs and the cleanup runner itself are excluded; unrecorded WebView renderer descendants are covered separately by Job cleanup counts, not individually inventoried. Every recorded native Job reports `activeAfterCleanup: 0`. Existing WSL flowmux PID 787 was observed alive read-only.

The [cleanup metadata](editor-search-diagnostics/cleanup-metadata.json), [script](editor-search-diagnostics/cleanup-script.ps1) and [generator](editor-search-diagnostics/cleanup-generator.py) preserve the inventory rules. The search dialog's physical keyboard/mouse/composition behavior, glyph fidelity, clipboard, DPI, accessibility and foreground focus transitions remain unverified. Hidden reads report `document_focused: false`; logical visibility and hidden HWND observations are separate checks.

The native cases do not force in-flight cancellation/supersession, permanently blocked filesystems, queued close/move races, network/UNC/reparse paths, hostile ACLs, directory floods or every scanner budget. Worker tests and static adapter tests cover narrower deterministic boundaries and must not be described as full native coverage.
