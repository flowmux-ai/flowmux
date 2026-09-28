<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows editor tabs

The Windows host embeds Monaco in a separate WebView2 editor tab. It reuses the
unchanged `flowmux-editor` document, protocol, session and recovery domain through
a path dependency; Windows owns its host integration, ordered I/O worker, local
asset server and frontend adapter. This is a partial editor implementation toward
F01–F05/F17, not a claim of full Linux/macOS parity. Hidden native tests establish
[measured partial acceptance](evidence/2026-09-28/editor.md) for actual Monaco
models, file bytes and the recorded lifecycle cases. Physical UI/IME acceptance
and the remaining limits in that evidence record are still pending. The later
[Open and picker record](evidence/2026-09-28/editor-open.md) separates current
implementation from completed checks and outstanding verification. It records
seven final-source editor cases, 36 related native checks and 172 Windows unit
tests, with three earlier passes and two failed Restore attempts kept separate.
Those results precede the automatic-refresh implementation described below.
The later [automatic-refresh record](evidence/2026-09-28/editor-refresh.md)
contains eight passing hidden native cases for actual Monaco updates, conflicts,
partial failures and ownership, plus nine related editor cases and 184 actual
Windows unit tests. Release, package and cleanup results are tracked separately
in that record. Physical IME and desktop acceptance remain unverified; these
results do not establish every notification or callback race.

An editor tab can contain several documents. `editor open` uses an existing
editor tab with the same canonical root in the target pane, or adds a new editor
tab to that pane. Opening an already open file selects its existing document.
An omitted `--pane` follows the normal caller/active-tab target; an omitted
`--root` uses the workspace working directory. The CLI resolves relative file and root paths from its own working directory;
the resulting file must be contained in the editor root and already exist.
Root/file canonicalization and validation run on a bounded background worker.
The host captures the original source surface, pane, workspace and root, then
rechecks source membership before publishing a prepared Open. A source that has
closed or moved to another pane/workspace cannot redirect that result into the
newly focused pane.

The sidebar's **Open file…** button opens an owned Windows file dialog. The
console command `editor pick [--pane <pane-uuid>]` requests the same dialog;
an omitted pane follows the caller/active-tab target. Both capture the workspace
working directory as the editor root and default starting folder. Windows may
restore a remembered dialog folder instead of that default. The selected file
must be inside the captured root; the picker does not adopt an outside file's
directory as a new root or change the process working directory.

`editor pick` returns `picker_requested: true`, `pane` and `source` before opening
the modal dialog. This is an accepted-request receipt, with no editor surface;
it is not an `editor_opened` result. Human time spent selecting a file is outside
the IPC reply wait. Cancellation leaves the tab layout unchanged. Selection
starts asynchronous Open, with later failures shown in an owned desktop error
dialog. Use `tree` or `editor status` to inspect the resulting editor. If the
requester expires before the host can acknowledge the picker request, no dialog
opens. Only one picker is admitted per host. Background mode rejects it before
any native dialog; physical picker selection, cancellation, focus and IME
behavior remain unverified.

These PowerShell examples use the console CLI and an explicit window pipe.
`--pipe` can be omitted when `FLOWMUX_PIPE_NAME` supplies the intended window.
Use the editor **surface** UUID returned by Open for subsequent editor commands;
the pane UUID is a different identity.

```powershell
$pipe = '<pipe-name>'
$pane = '<pane-uuid>'
$root = 'C:\work\project'
$opened = flowmuxctl.exe --pipe $pipe --json editor open (Join-Path $root 'README.md') --pane $pane --root $root | ConvertFrom-Json
$surface = $opened.editor_opened.surface

flowmuxctl.exe --pipe $pipe --json editor status $surface
flowmuxctl.exe --pipe $pipe --json editor command $surface read
flowmuxctl.exe --pipe $pipe --json editor command $surface replace-text --text "한글 한 é 😀`nsecond line`n"
flowmuxctl.exe --pipe $pipe --json editor command $surface undo
flowmuxctl.exe --pipe $pipe --json editor command $surface redo
flowmuxctl.exe --pipe $pipe --json editor command $surface save
flowmuxctl.exe --pipe $pipe --json editor check-disk $surface
flowmuxctl.exe --pipe $pipe --json editor flush $surface
```

Commands operate on the active document unless noted below. The `command`
response wraps its result in `result`; Open returns `editor_opened` with `pane`,
`surface` and `placement_strategy` (`new_tab` or `reuse_tab`).

| Command/action | Behavior |
| --- | --- |
| `editor pick [--pane <pane>]` | Requests the native Open File dialog and returns a receipt before human interaction. Background mode rejects it. |
| `editor status <surface>` | Reports readiness, logical visibility, document metadata, active document, dirty paths, saved session state, pending work and diagnostic fields. Does not return full document text. |
| `editor command <surface> read` | Reads the actual Monaco model and its document/version/encoding/EOL state. Text is limited to 128 KiB; check `content_truncated` and `total_bytes`. |
| `replace-text --text <text>`, `undo`, `redo` | Change the Monaco model and synchronize through the versioned document protocol. |
| `save` | Saves the active document after synchronizing its current model. |
| `save-as --path <relative-path>` | Saves under the editor root. An existing target requires explicit `--overwrite`; absolute paths and parent traversal are rejected. |
| `save-all` | Saves dirty documents in this editor tab, stopping on the first failure. Earlier successful saves remain saved. |
| `close-document` | Closes a clean document. A dirty document opens the shared editor confirmation and returns `closed: false, confirmation_required: true`. |
| `discard-document` | Explicitly discards the active document's edits and closes that document. |
| `compare`, `keep-mine`, `reload` | Resolve a reported disk conflict: show a diff, retain the buffer against the new disk base, or replace the buffer from disk. `keep-mine` does not itself save; use `save` afterward. |
| `recover`, `discard-recovery` | Answer the active document's pending recovery proposal. Recovery restores the buffer as dirty; it does not silently overwrite the file. |
| `editor check-disk <surface>` | Flushes edits, compares disk bytes and reports changes. A clean changed document can reload automatically; a dirty one retains its edits and reports a conflict. |
| `editor flush <surface>` | Waits for frontend changes, view state and ordered recovery work to reach the backend, then returns status. It neither saves the document nor creates a window checkpoint. |

`status` includes `ready`, `initialization_failed`, `synchronization_failed`,
`pending`, `last_error`, `restore_errors`, `documents`, `session` and the
`storage_root`/`profile_path`/`recovery_root` diagnostic paths. Each document
has its ID, display path, dirty state, version, encoding, EOL, active state,
read-only state, external-change state and `disk_status_known`. A false
`disk_status_known` means a failed scan could not establish that document's disk
status; it does not mean unchanged. Historical per-file restore errors
remain diagnostic; they do not make a later valid Open fail. `ready` requires
both the frontend and backend and is false during unresolved replacement or
timed-out work. Inspect status after an uncertain result rather than retrying a
mutation automatically.

Automatic refresh uses a native recursive directory watcher for each editor
root. Notifications are hints: the ordered document worker compares actual disk
bytes for all open documents, including writes with unchanged size and restored
timestamps. Clean changed documents reload into their existing Monaco models;
dirty documents retain their edits and report a conflict. A deleted file keeps
its existing model and reports the missing-file conflict without recreating the
file. Automatic replacements do not select an inactive document or request
focus. Explicit `check-disk` and the save conflict check remain available.

Refresh waits for editor work and recent activity to settle. It defers during
Monaco composition, focus in a non-text Monaco widget, pending edits, diff
display, and the editor's close, recovery, Save As or search dialogs. This covers
those known Monaco/HTML states, not arbitrary operating-system dialogs or
physical IME acceptance. An admitted refresh guards input until the frontend
acknowledges applying its result.
Close and model commands received while the editor is busy fail explicitly;
the host does not queue or replay them. A tab move retains the editor/view
identity and is not blocked by this readiness guard. Poll status for readiness
before a new close or model command. A timeout after disk work starts retains
the guard for the original result; it does not cancel the filesystem operation
or unlock editing early.

`automatic_refresh` reports watcher `mode`/`ready`, `pending`, `generation`,
`applied_generation`, `completed_count`, `deferred_count`, `phase`, `timed_out`
and `last_error`, plus nested watcher ownership and shutdown diagnostics.
Completion counters advance only after the frontend apply acknowledgment,
including an applied partial-error response. They do not count writes or prove
that every file was readable. If a later file fails after an earlier clean file
was reloaded, its advancing model/version is retained alongside the original
error. Ambiguous documents have `disk_status_known: false`; unlocking a file or
an empty successful retry may leave that uncertainty and its diagnostic sticky.

Notifications are coalesced while one refresh is outstanding. Every observed
root change currently prompts a scan of all open documents, so unrelated writes
in a noisy root can cause repeated scans. There is no automatic watcher restart,
periodic fallback, or explicit detection of the watched root directory itself
being renamed. Watcher startup/failure remains visible in status. Shutdown
signals cancellation without joining on the UI thread; the worker keeps pending
I/O resources until Windows reports completion. The eight hidden native cases
passed clean and inactive-document updates, dirty conflicts, deletion/recreation,
equal-length writes with restored timestamps, partial sharing-denied reads,
moved/closed ownership and write bursts followed by the editor's own save. They
read actual Monaco models after apply acknowledgment without using `check-disk`
to trigger refresh. The move/close case waited for idle before its mutations
and observed the closed editor's absence for 1,553 ms; it did not force a queued
callback race. The 32-write burst produced 64 observed generations and two
completed refreshes in that run, not a fixed event-to-write relationship.
Physical interaction, modal/composition deferral on a real desktop and exhaustive
watcher failure/cancellation timing remain unverified.

Files must be local Unicode paths contained in a canonical editor root. Windows
validation rejects UNC paths, device namespaces, alternate data streams,
drive-relative paths, reserved device names, control characters and ambiguous
trailing dots/spaces. General paths are limited to 32,767 UTF-16 code units; the
Save As bridge additionally bounds its path string to 16 KiB. Windows command-line
limits can be lower than the editor's data limits. Canonicalization checks
containment, but this is not a claim of race-proof handling of concurrently
changed reparse points.

One Open preparation may be pending per pane. Its worker admits eight jobs per
host, including executing/queued work, cancelled jobs that have not drained and
posted results awaiting the UI. Cancelling an operating-system filesystem call
does not refund its slot before it finishes. This is separate from each editor
tab's document-I/O queue. CLI Open has a twelve-second budget from server receipt,
including UI queue time, preparation and editor initialization; preparation does
not restart that budget. Picker-initiated Open starts its budget after selection.
Expired/cancelled preparation results cannot publish an editor tab. A timeout
after initialization has begun may still have an uncertain outcome; inspect
the tree/status before retrying.

WebView2 COM construction and native dialog calls remain on the UI thread. The
modal picker processes native messages while application events await its return;
other IPC requests can expire during that wait. Queued CLI Opens retain their
original receipt time. The budget cannot interrupt a blocked Windows API call,
and this implementation does not establish bounded WebView construction time.

Each editor session admits at most 128 documents. Document loading and
synchronized text have 16 MiB limits. The shared domain accepts UTF-8 with or
without a BOM and LF or CRLF line endings; binary/NUL content, invalid UTF-8 and
mixed LF/CRLF files are rejected. Monaco models use LF internally, while the
document's original BOM/EOL settings govern saves. Korean, decomposed Hangul,
combining marks and emoji are passed without Unicode normalization. Read-only
documents refuse editing/saving. Version checks reject stale edits, and saves
compare current disk bytes with the stored base before using the shared atomic
write path. A conflict or failed save retains unsaved buffer state.

Document I/O and recovery operations run in one ordered worker per editor tab.
Its queue admits eight pending operations and at most 32 MiB of queued text;
overflow is reported instead of blocking the UI thread. Recovery writes/removals
are applied before worker replies. Failed recovery operations are reported and
retained for retry on subsequent work. Recovery records are scoped to the
workspace and editor surface; restoring the persisted surface can offer its
unsaved snapshot. Undecided proposals are not silently discarded on an ordinary
document close. Window checkpoints store document paths, active document, cursor,
scroll and zoom metadata, separately from unsaved recovery text.

Windows recovery writes use a platform adapter because copying directory
permissions onto a temporary file fails on Windows. The adapter preserves the
shared snapshot names, JSON format, workspace identity and size validation;
reads and removals still use `RecoveryStore`. It creates each temporary file with
a protected DACL granting access to the process user and SYSTEM, writes and
flushes the snapshot, then replaces the destination in the same directory.
Replacement failure leaves the previous snapshot in place; cleanup targets only
the temporary file created by that write. Native creation and replacement use
the canonical parent's extended-length Windows path, including for deeply nested
evidence directories. This changes recovery-file creation, not the shared source
or ordinary document-save implementation. Power-loss durability and exhaustive
ACL behavior remain separate acceptance work.

Restoration and recovery-directory creation occur on the worker. An unavailable
root or recovery store produces an unready editor with `initialization_failed`
and an error, while preserving its original saved session metadata. A pristine
failed editor with no live documents, dirty state or pending work can be
checkpointed or closed without trapping other tabs in the window. A missing
individual file is reported in `restore_errors`; other restorable files can open.

Closing an editor tab, workspace or window first synchronizes its editor models.
Dirty documents prevent the container close until they have been saved or
explicitly discarded. `quit --discard-state` does not bypass this unsaved-edit
guard. Checkpointing is allowed with dirty documents after synchronization; it
does not mark them saved. Failed close/checkpoint operations release ordinary
close seals so editing can resume.

Once a window close is accepted, new mutations are refused and pending Open
preparations are cancelled while the IPC reply drains. The accepted-close guard
also applies when no editor barrier or persistent state store is needed, so a
late preparation cannot add a tab during that interval. A direct clean
`quit --discard-state` still exits when its reply receiver has expired.

Window checkpoints resolve their existing parent to Windows' extended-length
path before creating and atomically replacing the temporary state file. This
handles a long temporary filename even when the shorter destination is below
the traditional path limit. It preserves the externally reported checkpoint
path and same-directory replacement. The initial failure and correction's
verification status are recorded separately in the Open and picker evidence;
this is not exhaustive long-path, ACL or power-loss acceptance.

An accepted clean `quit --discard-state` still closes the host if its IPC reply
receiver has already expired. A client timeout does not cancel that queued quit;
do not retry it automatically. The measured late-quit case and its exact hidden
UI-thread suspension boundary are recorded in the linked acceptance evidence.

Mutating CLI commands and model-replacing disk/recovery operations suspend edits
until their original responses have been applied. The native command deadline
is 12 seconds. On expiry, the caller receives an uncertain-outcome error and the
host retains the original pending operation as a timeout tombstone. Its existing
callback/flush/snapshot sequence can still finish; editing resumes only after
that sequence reconciles. A filesystem timeout is not cancellation. No automatic
retry or forced filesystem-thread termination occurs. Dropping a worker lets its
current filesystem operation finish and cancels queued work without joining it
on the UI thread.

A synchronization failure instead quarantines the frontend: it remains read-only,
status reports `synchronization_failed`, and ordinary close-error release paths
do not unlock it. Unexpected frontend reloads also fail closed. Automatic repair
or renderer-crash reattachment is not implemented; the retained backend and
recovery data are not evidence that every renderer-crash timing is recoverable.

Normal desktop mode displays the real Monaco editor, uses its ordinary dialogs
and can focus a selected editor. Windows avoids repeating native focus transfer
when focus is already inside that editor. Commands and close barriers reject an
active composition instead of committing it. Background verification keeps the
native editor WebView hidden, suppresses DOM/window focus calls, substitutes
nonmodal dialog opening and disables the WebView clipboard option. Its
`visible` status is logical layout visibility, not proof of a shown HWND.
Background protocol edits exercise Monaco but do not establish physical IME,
keyboard, mouse, accessibility, DPI, font/glyph or native-dialog behavior.

Editor storage is isolated as well as its window behavior. Normal mode uses
`%LOCALAPPDATA%\flowmux\windows` as `storage_root`. Background mode requires
`FLOWMUX_TEST_STATE_DIR` and uses that supplied directory for editor storage;
it does not fall back to the normal profile/recovery directory. The background
verifier supplies its owned evidence/state directory. Both modes place the
WebView profile under `storage_root\editor-profile` and shared recovery records
under `storage_root\editor-recovery`, with workspace and surface/document hashes
below that recovery root. `editor status` exposes all three paths so verification
can check the actual storage location. Restoring a test window uses the same
isolated root to make its recovery records available.

Remaining scope includes physical acceptance of the native Open File dialog,
Windows native editor clipboard/menu integration, Quick Open and workspace
search, broader automatic-refresh acceptance and its watcher limitations,
physical IME validation and renderer-crash recovery. Quick Open/search requests
receive completion/error responses instead of hanging. Native clipboard requests
report that integration is unavailable; background verification never accesses
the OS clipboard. Concurrent-writer races, exhaustive ACL behavior, power-loss
durability and complete desktop usability remain separate acceptance work.

Implementation sources: [command validation](src/editor.rs),
[Open preparation](src/editor_open.rs), [native picker](src/native/editor_picker.rs),
[ordered worker](src/editor_worker.rs),
[directory watcher](src/editor_watch.rs), [automatic refresh](src/native/editor_refresh.rs),
[Windows recovery writer](src/editor_recovery.rs), [native lifecycle](src/native/editor.rs),
[editor WebView](src/native/editor_view.rs), [asset server](src/editor_assets.rs),
[Windows adapter](editor/adapter.js) and [shared editor domain](../crates/flowmux-editor/).
The [background verifier](scripts/verify-editor.ps1) records both measured checks
and deferred scenarios. This document does not substitute for its native results.
