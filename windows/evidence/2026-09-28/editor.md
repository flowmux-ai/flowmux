<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows Monaco editor evidence — 2026-09-28

This stage adds actual offline Monaco editor tabs to the native Windows host.
Fifteen unique hidden-native editor cases passed, with one recorded
check per case, plus 27 related native regression checks. These results make
F01–F05, F17 and G08 **partial**. No Windows feature or gate becomes passed.
The acceptance ledger now contains 57 partial/57 pending features and ten
partial/three pending gates. The original fourteen cases and 27 related regression
checks precede the one-line QuitDiscard close-state correction. The late-quit
case and a repeated close case passed afterward. Earlier results retain that
source boundary. Final debug/release builds, 157 native Windows unit tests,
Clippy, installer packaging and three release entrypoint probes passed. The final
read-only inventory found all 125 recorded Windows process IDs absent.

## Implementation and verification boundary

A Windows-only asset build reads the shared editor frontend unchanged and appends
a small bridge/command adapter in its lexical scope. Monaco, language workers,
fonts and notices are bundled offline. The tokenized loopback asset server uses
an allowlist and CSP; the WebView bridge validates its exact page, credential and
surface instance. The editor has its own profile and no terminal bridge.

Opening is currently through the CLI. Subsequent commands invoke real Monaco
models and the existing frontend save/conflict/recovery/close paths. The read
command reports actual model text, language and metadata; returned text is capped
at 128 KiB. Replacing text uses Monaco edits and undo stops. UI and CLI Save All
share a serial helper that waits for content, save and native completion before
sending the next document to the eight-job/32 MiB worker queue. The native
ten-document case reaches this helper through the CLI; it does not click a
physical button or shortcut. Seventeen static adapter tests separately exercise
both entry points, queued debounce handling, failure, timeout and overlap.

Content acknowledgments and barriers precede worker snapshots and dirty-close
decisions. Command/replacement work seals input; a timeout does not unlock late
disk work, and synchronization failure keeps the editor read-only. Main and diff
models use LF internally; the shared document service owns disk EOL/BOM encoding.
Native recovery writes create a temporary file with a protected current-user/
SYSTEM DACL before writing, flush it, then replace in the same directory. This
describes the implementation; it does not establish exhaustive ACL or power-loss
acceptance.

All current passing editor cases record the actual storage/profile/recovery
paths under the owned verifier state directory. The hidden WebViews suppress
focus and modal focus behavior before the bundle loads, and the native wrapper
keeps them hidden. Fixtures use owned processes, explicit IPC pipes and local
files, without desktop input, clipboard access or external sites. Unicode
assertions use ordinal comparisons and exact file bytes, not visual similarity.

## Measured native editor cases

The [collection manifest](editor-diagnostics/manifest.json) maps preserved native
JSON, runner results and stdout/stderr to their original runs. Durations below
are bounded runner wall times, including setup and cleanup. Each link describes
the specific tested behavior rather than broader feature acceptance.

| Case | Recorded behavior | Runner duration |
| --- | --- | --- |
| [open](editor-diagnostics/passed-open-native.json) | CLI Open, duplicate file identity, actual Monaco content/language and invalid-text isolation | [5.475 s](editor-diagnostics/passed-open-runner.json) |
| [edit](editor-diagnostics/passed-edit-native.json) | Actual model edit, undo/redo and acknowledged Save with exact Unicode bytes | [5.409 s](editor-diagnostics/passed-edit-runner.json) |
| [encoding](editor-diagnostics/passed-encoding-native.json) | UTF-8, BOM and LF/CRLF file round trips | [9.506 s](editor-diagnostics/passed-encoding-runner.json) |
| [encoding-defaults](editor-diagnostics/passed-encoding-defaults-native.json) | Empty/single-line model LF defaults and CRLF disk encoding without doubled CR | [12.962 s](editor-diagnostics/passed-encoding-defaults-runner.json) |
| [conflict](editor-diagnostics/passed-conflict-native.json) | Explicit disk check, clean reload, dirty conflict, compare, Keep Mine and Reload | [5.557 s](editor-diagnostics/passed-conflict-runner.json) |
| [save-as](editor-diagnostics/passed-save-as-native.json) | Unicode Save As, explicit overwrite, Save All, denied replacement and root boundary | [5.773 s](editor-diagnostics/passed-save-as-runner.json) |
| [save-all-many](editor-diagnostics/passed-save-all-many-native.json) | Ten dirty documents through the shared serial UI/CLI Save All helper; exact file/recovery bytes | [7.850 s](editor-diagnostics/passed-save-all-many-runner.json) |
| [close](editor-diagnostics/passed-close-native.json) | Dirty document/tab/workspace/quit refusal; explicit save/discard and logical-focus preservation | [6.725 s](editor-diagnostics/passed-close-runner.json) |
| [move](editor-diagnostics/passed-move-native.json) | Hidden/moved editor retains native view, document and actual Monaco undo history | [5.157 s](editor-diagnostics/passed-move-runner.json) |
| [restore](editor-diagnostics/passed-restore-native.json) | Mixed checkpoint restores editor files/active document without terminal metadata | [7.758 s](editor-diagnostics/passed-restore-runner.json) |
| [restore-errors](editor-diagnostics/passed-restore-errors-native.json) | Missing restored file is reported; later valid Open reuses the existing editor | [8.111 s](editor-diagnostics/passed-restore-errors-runner.json) |
| [missing-root](editor-diagnostics/passed-missing-root-native.json) | Unavailable root preserves checkpoint metadata and leaves other surfaces usable | [7.086 s](editor-diagnostics/passed-missing-root-runner.json) |
| [checkpoint-failure](editor-diagnostics/passed-checkpoint-failure-native.json) | Denied checkpoint replacement retains old state, releases the close seal and permits later edits/quit | [8.463 s](editor-diagnostics/passed-checkpoint-failure-runner.json) |
| [recovery](editor-diagnostics/passed-recovery-native.json) | Acknowledged dirty content survives owned-host termination; explicit Recover and Save preserve exact bytes | [8.685 s](editor-diagnostics/passed-recovery-runner.json) |
| [late-quit](editor-diagnostics/passed-late-quit-native.json) | Accepted clean discard-state quit exits after the real IPC deadline and verified resumption of the owned hidden UI thread | [21.356 s](editor-diagnostics/passed-late-quit-runner.json) |

The recovery case first waits for the edit acknowledgment, confirms the isolated
recovery snapshot and unchanged original file, completes a checkpoint, and then
terminates its exact owned host. Restart offers the recovery content; explicit
Recover restores it into Monaco, and explicit Save changes the original file.
This is recovery at that measured boundary. It does not force termination during
the 150 ms edit debounce, during an I/O write, or during a renderer-only crash.

The dirty-close case follows a replace-text command that already synchronized.
It therefore does not independently reproduce an unacknowledged keystroke racing
tab/workspace/host closure. The checkpoint-failure case proves failure unsealing
and subsequent actual editing; it does not force a close while an earlier
checkpoint is still in flight.

Source review found that a clean `quit --discard-state` accepted after its reply
receiver expired could leave the host alive with the editor sealed. The accepted
close now proceeds when sending that reply fails, matching regular quit. The
late-quit case suspends only the exact owned hidden host UI thread while its IPC
server remains running. It observes the actual server deadline in **15.084 s**,
then resumes the thread with previous suspend count **0** and resume count **1**.
The original queued close completes and the host exits cleanly with code **0**,
without forcing its termination. It does not cancel or retry the timed-out quit.

A separate [post-fix close rerun](editor-diagnostics/post-quit-fix-close-native.json)
passed in [8.949 s](editor-diagnostics/post-quit-fix-close-runner.json). This repeats
the close case and does not add a sixteenth unique case. Its outer runner was
misnamed late-quit; the preserved native JSON correctly identifies `case: close`,
so only the separate 21.356-second run counts as late-quit evidence.

## Related regressions and build checks

| Related native regression | Checks | Runner duration |
| --- | --- | --- |
| [state](editor-diagnostics/regression-state-native.json) | 8 | [41.570 s](editor-diagnostics/regression-state-runner.json) |
| [panes](editor-diagnostics/regression-panes-native.json) | 7 | [14.125 s](editor-diagnostics/regression-panes-runner.json) |
| [workspaces](editor-diagnostics/regression-workspaces-native.json) | 6 | [12.582 s](editor-diagnostics/regression-workspaces-runner.json) |
| [browser](editor-diagnostics/regression-browser-native.json) | 6 | [16.338 s](editor-diagnostics/regression-browser-runner.json) |

The total is 27 related checks: eight state, seven pane, six workspace and six
browser checks. Each suite retains its own pending physical/lifecycle limits.
The state JSON has no top-level status field; its successful bounded runner and
eight recorded checks are preserved without inventing one.

| Check | Result |
| --- | --- |
| Adapter tests | 17 passed; [runner, 0.410 s](editor-diagnostics/adapter-17-runner.json). Stubbed JavaScript contracts, not physical IME. |
| Offline editor assets | Built successfully; [runner, 12.386 s](editor-diagnostics/adapter-assets-build-runner.json). |
| Linux Windows-workspace library tests | 123 passed before the recovery adapter was added; [runner, 59.238 s](editor-diagnostics/linux-lib-123-runner.json). |
| Later Linux editor subset | 14 passed including portable recovery tests; [runner, 43.939 s](editor-diagnostics/linux-editor-14-runner.json). Windows-only tests are excluded. |
| Windows debug cross-build with recovery fix, before late-quit correction | Passed; [runner, 74.322 s](editor-diagnostics/native-debug-recovery-build-runner.json). |
| Final Windows debug cross-build | Passed after the late-quit correction; [runner, 73.799 s](editor-diagnostics/post-quit-fix-debug-build-runner.json). |
| Earlier release Clippy, all targets | Passed before the late-quit correction with warnings denied; [runner, 24.537 s](editor-diagnostics/clippy-fixed-runner.json). |
| Final release Clippy, all targets | Passed after the late-quit correction with warnings denied; [runner, 20.114 s](editor-diagnostics/final-clippy-runner.json). |
| Final Rust formatting | Passed after the late-quit correction; [runner, 10.060 s](editor-diagnostics/post-quit-fix-format-runner.json). |
| Earlier Windows release build | Passed before the late-quit correction; [runner, 109.902 s](editor-diagnostics/release-build-runner.json). |
| Final Windows release build | Passed after the late-quit correction; [runner, 115.556 s](editor-diagnostics/post-quit-fix-release-build-runner.json). |
| Earlier native Windows unit tests | 157 passed before the late-quit correction; [runner, 13.071 s](editor-diagnostics/native-tests-before-quit-fix-runner.json). |
| Final native Windows test executable | Built after the late-quit correction; [runner, 88.183 s](editor-diagnostics/final-native-lib-build-runner.json). |
| Final native Windows unit tests | 157 passed after the late-quit correction; [runner, 13.332 s](editor-diagnostics/final-native-tests-runner.json), test body 13.24 s, including recovery DACL, long-path and sharing-lock cases. |
| Final installer package | NSIS packaging passed; [runner, 12.166 s](editor-diagnostics/final-package-runner.json). Installer execution remains unverified. |
| Final release entrypoints | Three `doctor` probes passed; [runner, 1.192 s](editor-diagnostics/final-release-doctor-runner.json), [results](native-editor-release-entrypoints.json). |
| Artifact hashes | Eight debug/release/native-unit/installer artifacts recorded in the [SHA256 manifest](artifacts-editor.json). |
| Source/process isolation | [Read-only record](editor-isolation.json): no tracked stage changes outside `windows/`; the existing WSL flowmux PID 787 was alive without control actions. |
| Final owned-process inventory | [Read-only result](native-editor-cleanup.json): all 125 recorded Windows PIDs absent at 03:25:41.9468664 UTC; [runner, 2.500 s](editor-diagnostics/final-cleanup-runner.json). |

The final release `flowmux.exe`, `flowmuxctl.exe` and `flowmux.com` doctor
commands returned exit code 0 and `status:ok`, reporting WebView2
`112.0.1722.48` and `background_testing:false`. These are read-only CLI probes;
they do not rerun hidden editor cases in a release GUI or establish deployment.
The packaged setup is **5,086,195 bytes**, SHA256
`7d88924b5c6976bc4f3f77e0e137e8af2aae26c7f3d045b0f17440cbce946477`.
The installer includes the editor/Monaco notices and refreshed Rust notices; its
[six recorded package-input hashes](editor-diagnostics/package-inputs.json) cover
the installer script, acceptance/implementation documents and notice files. It
was built, not installed or uninstalled in this stage.

The isolation record compares this editor stage with its parent commit and finds
no tracked changes outside `windows/`. It does not reinterpret or revert earlier
unrelated changes, or claim Linux/macOS runtime regression coverage. The existing
WSL process was inspected only; no control action was taken against it.

## Preserved failures

The [initial Open failure](editor-diagnostics/failed-initial-status-null-native.json)
reported an IPC error. Source review traced this to the status response carrying
a top-level `error:null`, which collided with the IPC error discriminator; the
field is now `last_error`. The preserved diagnostic does not contain that original
status payload, so the diagnosis is not inferred from the error string alone.

The [first edit failure](editor-diagnostics/failed-recovery-permissions-native.json)
recorded Windows error 87 when the shared recovery fallback copied directory
attributes onto a file. A Windows-only recovery writer now handles file creation
and replacement while retaining the shared recovery format, names and reads.
That earlier run also used a non-isolated editor recovery path. The host now
requires the explicit test state root for both profile and recovery storage; the
current passing cases assert and record it. The earlier
[Open pass](editor-diagnostics/superseded-open-before-isolation-native.json) is
retained as a superseded diagnostic and is not storage-isolation evidence.

The [initial Clippy failure](editor-diagnostics/clippy-initial-failed-runner.json)
was a test initializer style lint. Its corrected run is linked above. Failures
and superseded results are not counted among the fifteen unique passing cases.

The [initial cleanup inventory](editor-diagnostics/cleanup-initial-failed.json)
recorded 123 absent PIDs and one unverified entry out of 124. PID 37496 belonged
to `WmiPrvSE`, but its unavailable start time caused a null-value error, so the
result remains failed and does not establish PID reuse. That read-only check
did not stop or modify any process; its failure is preserved separately from
any later inventory.

The final inventory found **125 of 125 recorded PIDs absent**, including PID
37496. The previously observed system process exited between the checks without
any control action. The corrected script includes a bounded CIM creation-time
fallback, but this successful run did not exercise it because every recorded PID
was absent; it does not retroactively prove the earlier process identity. The
expanded inventory includes recorded Windows runner IDs as well as hosts, shells
and descendants. It excludes POSIX runner IDs and editor client PID arrays, and
does not inventory every renderer or every process launched by the older
regression verifiers. Runner job cleanup is separate evidence. This is not a
machine-wide absence or leak claim.

## Still pending

Physical keyboard/mouse editing, Korean IME composition and candidates, native
Open UI, clipboard, font/glyph/width fidelity, DPI, accessibility, selection,
minimap appearance and normal modal-dialog focus remain unverified. Existing
terminal IME evidence does not establish Monaco IME behavior. Fully asynchronous
Open-path validation, automatic file watching, complete workspace search and
file/viewer parity also remain pending; the conflict suite uses explicit disk
checks.

Concurrent writers, network/UNC/reparse-point paths, full ACL/owner preservation,
exhaustive long paths, power-loss durability, renderer-crash recovery and
other deterministic late-response/close races remain open. Shared document saving uses
file attributes through `std::fs::Permissions`; it does not establish preservation
of an existing custom Windows DACL. Same-URL renderer reload is denied and
duplicate readiness fails closed, but renderer termination recovery is not
implemented or accepted by these results. No clean-machine deployment or full
Windows acceptance follows from the checks above.
