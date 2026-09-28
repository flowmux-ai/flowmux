<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows editor tabs

The Windows host embeds Monaco in a separate WebView2 editor tab. It reuses the
unchanged `flowmux-editor` document, protocol, session and recovery domain through
a path dependency; Windows owns its host integration, ordered I/O worker, local
asset server and frontend adapter. This is a partial editor implementation toward
F01–F05/F17, not a claim of full Linux/macOS parity. Hidden native tests establish
[measured partial acceptance](evidence/2026-09-28/editor.md) for actual Monaco
models, file bytes and the recorded lifecycle cases. Physical UI/IME acceptance
and the remaining limits in that evidence record are still pending.

An editor tab can contain several documents. `editor open` uses an existing
editor tab with the same canonical root in the target pane, or adds a new editor
tab to that pane. Opening an already open file selects its existing document.
An omitted `--pane` follows the normal caller/active-tab target; an omitted
`--root` uses the workspace working directory. The CLI resolves relative file and root paths from its own working directory;
the resulting file must be contained in the editor root and already exist.

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
read-only state and external-change state. Historical per-file restore errors
remain diagnostic; they do not make a later valid Open fail. `ready` requires
both the frontend and backend and is false during unresolved replacement or
timed-out work. Inspect status after an uncertain result rather than retrying a
mutation automatically.

Files must be local Unicode paths contained in a canonical editor root. Windows
validation rejects UNC paths, device namespaces, alternate data streams,
drive-relative paths, reserved device names, control characters and ambiguous
trailing dots/spaces. General paths are limited to 32,767 UTF-16 code units; the
Save As bridge additionally bounds its path string to 16 KiB. Windows command-line
limits can be lower than the editor's data limits. Canonicalization checks
containment, but this is not a claim of race-proof handling of concurrently
changed reparse points.

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

Remaining scope includes the native Open File entry point/dialog, Windows native
editor clipboard/menu integration, Quick Open and workspace search, automatic
filesystem watchers/polling, physical IME validation and renderer-crash recovery.
Files are currently opened through the CLI. Quick Open/search requests receive
completion/error responses instead of hanging. Disk changes require explicit
`check-disk` or are detected by the save conflict check. Native clipboard requests
report that integration is unavailable; background verification never accesses
the OS clipboard. Concurrent-writer races, exhaustive ACL behavior, power-loss
durability and complete desktop usability remain separate acceptance work.

Implementation sources: [command validation](src/editor.rs),
[ordered worker](src/editor_worker.rs),
[Windows recovery writer](src/editor_recovery.rs), [native lifecycle](src/native/editor.rs),
[editor WebView](src/native/editor_view.rs), [asset server](src/editor_assets.rs),
[Windows adapter](editor/adapter.js) and [shared editor domain](../crates/flowmux-editor/).
The [background verifier](scripts/verify-editor.ps1) records both measured checks
and deferred scenarios. This document does not substitute for its native results.
