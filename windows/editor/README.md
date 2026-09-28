<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows editor assets

`npm ci --ignore-scripts` and `npm run build` in this directory rebuild
`../assets/editor`. The build reads the shared editor frontend without modifying
its source or its existing `dist` directory. A build-time append adds
`adapter.js` in the frontend's lexical scope. Monaco and esbuild resolve from this
package's pinned dependencies; every worker, font and notice is bundled offline.

`initialize.js` is embedded by the Windows WebView wrapper and runs before the
bundle. It supplies the compatibility bridge, exact-page credentials and input
barrier. Debug background hosts additionally suppress `HTMLElement.focus` and
`window.focus`, and replace modal-dialog opening with its nonmodal counterpart.
The native wrapper also keeps those views hidden and refuses native focus calls.
These guards support background model verification; they do not establish real
keyboard, Korean IME, modal-dialog focus, clipboard or accessibility behavior.
Normal hosts keep the shared editor's focus and dialog behavior.

`window.flowmuxWindowsEditor.command({id, action, text?, path?, overwrite?})`
targets the active Monaco document. Actions are `read`, `replace_text`, `undo`,
`redo`, `save`, `save_as`, `save_all`, `close_document`, `discard_document`,
`compare`, `keep_mine`, `reload`, `recover`, and `discard_recovery`.
Commands reply through the authenticated bridge with `kind: "command_result"`,
the request `id`, and either `result` or `error`. Reads report actual model text,
language and document metadata; returned content is capped at 128 KiB of UTF-8
without splitting a Unicode scalar. Empty reads have null document/content
fields. Dirty close requests retain the document and open the ordinary close
confirmation; explicit `discard_document` uses the existing discard-close path.
Save-as overwrites only when the caller explicitly passes `overwrite: true`.

The adapter waits for the existing document-change/save/replace/close messages.
`discard_recovery` has no shared acknowledgment, so the host must follow its
command result with the usual flush barrier and worker snapshot. Timeouts do not
cancel or retry an operation. Host-side disk responses remain authoritative.

`barrier(id, seal)` rejects active Monaco composition, flushes all model changes,
and reports the shared `flush_completed` acknowledgment. A sealed barrier makes
the main and diff editors read-only and blocks keyboard/pointer editing until
`releaseBarrier(id)` receives the matching identifier (or zero to release a
failed native close operation). It does not cancel IME
composition. Static tests cover bridge/adapter contracts using stubs; they are
not a substitute for native Monaco verification.
