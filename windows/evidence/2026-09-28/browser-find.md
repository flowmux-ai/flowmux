<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows browser page find — partial acceptance

The final debug native run passed all eight page-find groups; the separate startup
check also passed. Builds, native tests, related regressions and packaging passed
with the limits recorded below. Existing Linux/macOS and
shared sources, root manifests and lockfiles are outside this Windows-only change.
Full browser and Windows acceptance remain incomplete.

## Implemented behavior

`browser find pane:<id> <query>` supports `--backward`, `--case-sensitive` and
`--no-wrap`. The default is forward, case-insensitive search with wrap. Plain
output is `true`/`false`; JSON includes `surface`, `query`, `backward`,
`case_sensitive`, `wrap`, `found` and `selection`. No match is a successful false
result. `browser find-show` opens the native panel in normal runs, and
`browser find-close` returns `ok`, `surface` and `cleared`.

The browser toolbar has a Find button. Its owned native panel contains a Unicode
EDIT, Match case checkbox, Previous/Next/Close buttons and status text. Native
query editing receives Enter/Escape, character, key-up and composition messages
without translating them into find/close commands; Tab remains dialog navigation.
There is no search-as-you-type. Periodic status updates do not rewrite the edit.
Showing the normal panel focuses its edit. Background test hosts construct the
controls without showing or focusing the panel.

The bridge calls the real `window.find` engine. It does not implement a separate
DOM text matcher or alter page attributes. Input data is JSON encoded; empty,
NUL-containing and over-4,096-byte UTF-8 queries are rejected before dispatch.
Hangul, decomposed jamo, combining accents and emoji codepoints are passed without
normalization. This is a transport guarantee; the engine determines matching
equivalence. Returned selection text is limited to 8,192 UTF-16 units without
splitting a surrogate pair; truncation can split a combining/grapheme sequence.
Missing `window.find` produces an explicit error.

Requests retain their browser surface and native navigation generation. They
require an active, logically visible browser tab whose navigation has finished,
and serialize with other DOM actions on that surface. Callback validation checks
generation, surface existence, visibility revision, result shape/size and the
12-second deadline. The existing limit of 16 pending scripts remains. Navigation
or closure rejects stale callbacks and resets the panel; hiding and re-showing a
tab cannot validate a callback from its earlier visibility revision. Post-dispatch
errors report that the action may have executed; there is no automatic retry.
The panel can dismiss while an action is pending and defer owned-selection cleanup
until the pending request completes or expires, provided its document and
visibility still match. Expiry does not cancel the engine's running JavaScript.

Successful exposed ranges are tracked under a random per-browser window property.
Close checks the original Range identity, saved nodes/offsets, selection direction
and selected text before clearing it, and removes the owned tracking state.
Selection changes invalidate ownership. A failed find retains a still-matching
previous owned range; later user/page selections are preserved.

## Engine limits and primary sources

There is no standard DOM API equivalent to the full native browser find UI.
`window.find` is a legacy engine extension, and this implementation does not claim
full native Find API parity. Match counts, highlight-all and regex are not provided;
complete cross-frame or shadow-tree matching is not guaranteed. Chromium 112's
[LocalDOMWindow implementation](https://github.com/chromium/chromium/blob/112.0.5615.49/third_party/blink/renderer/core/frame/local_dom_window.cc)
delegates the options to its editor and leaves the search-in-frames argument
unimplemented. Its [editor search](https://github.com/chromium/chromium/blob/112.0.5615.49/third_party/blink/renderer/core/editing/editor.cc)
selects/reveals an engine result and skips results that cannot be represented by
a single DOM Range across tree scopes.

Some real matches inside shadow trees or text controls appear collapsed through
the document Selection API. Chromium's
[DOMSelection implementation](https://github.com/chromium/chromium/blob/112.0.5615.49/third_party/blink/renderer/core/editing/dom_selection.cc)
explicitly rescopes shadow positions and makes `isCollapsed` true in these cases.
The bridge does not claim such ranges for cleanup, so close can return
`cleared:false` and leave that engine selection visible. Replacing this guard with
`Selection.type === 'Range'` was rejected during review: DOM mutation paths in
[SelectionEditor](https://github.com/chromium/chromium/blob/112.0.5615.49/third_party/blink/renderer/core/editing/selection_editor.cc)
can change native selection positions without replacing the cached document Range.
The exposed identity/boundaries do not establish ownership of every hidden range.

Direct commands do not explicitly request OS/DOM focus, but the engine can focus
an editable match when its frame is already focused. This follows
[FrameSelection::SetFocusedNodeIfNeeded](https://github.com/chromium/chromium/blob/112.0.5615.49/third_party/blink/renderer/core/editing/frame_selection.cc).
No universal focus-preservation claim is made. Physical Korean IME composition,
commit, candidate selection, deletion, cancellation, glyph rendering, keyboard/
mouse controls, DPI and accessibility remain unverified. Unicode transport or
synthetic DOM events cannot substitute for those checks.

## Native verification results

[The final native evidence](native-browser-find-background.json) records eight
passing groups on the final debug binary, empty host stderr and host exit code 0.
The [bounded native runner](find-diagnostics/native-runner.json) passed in
17.443 seconds against a 120-second deadline; its owned job reported zero active
processes after cleanup. The separate [startup check](find-diagnostics/startup.json)
verified hidden debug doctor and owned-host readiness. Its
[runner](find-diagnostics/startup-runner.json) passed in 3.067 seconds against a
60-second deadline, also with zero active processes after job cleanup.

The eight native groups cover:

- Forward/backward search, wrap/no-wrap, case sensitivity, no match and plain
  boolean output.
- Exact Unicode query transport, observations of engine matching, and literal
  script-like query text that does not execute.
- Hidden native panel query text and close clearing only an owned selection,
  preserving a later manually changed DOM selection.
- Surface-pinned state and rejection of requests to a hidden workspace.
- Terminal targets, invalid queries and loading-document rejection.
- Closing the verified owned hidden panel with `WM_CLOSE` while find is queued:
  the find returns its result and eventual selected text is empty. The fixture
  queues a read-only eval first; it does not trace every callback ordering.
- Navigation cancellation of pending find followed by successful search in the
  new document.
- Browser-tab closure cancelling pending find while preserving the original
  terminal.

The query and response strings use ordinal comparison, but the engine's matching
equivalence is observed rather than required to be ordinal. This run produced:

| Query | Observed selected text |
| --- | --- |
| `한글` | `한글` |
| `한` (U+1112 U+1161 U+11AB) | `한` (U+D55C) |
| `é` (U+0065 U+0301) | The plain `e` in `needle` |
| `😀` | `😀` |
| `한글 한 é 😀` | The same full compound string, with exact codepoints retained |
| `한 é` | `한 é` |

These are native engine folding/equivalence observations, not input normalization
by flowmux. The response query and native query field retain the supplied
codepoints. A CRLF-containing query was retained exactly but returned `found:false`
with the preceding `needle` selection still present. No general newline-match or
Unicode-normalization guarantee follows from this run.

The initial attempt failed during owned debug startup with a main-thread stack
overflow, before any native group ran. The failure is preserved in
[initial-stack-failure.json](find-diagnostics/initial-stack-failure.json),
[initial-runner.json](find-diagnostics/initial-runner.json) and
[initial-stderr.txt](find-diagnostics/initial-stderr.txt). Extracting the new clap
arguments into separate `Args`/`PaneArgs` builders fixed the debug startup path;
the stack allocation was not increased. The subsequent startup and eight-group
runs above passed.

Evidence copies are UTF-8 with LF line endings and no BOM; JSON values are
unchanged. Native source run: `browser-find-95f712f7-8156-4d30-9df1-dffd7ebcb955`,
runner `page-find-native-bfd15200-c4b0-4d27-8e46-c41415251baf`. Startup source run:
`browser-find-3e41dfff-e91d-4c7d-9382-07b56da72c39`, runner
`page-find-startup-6feb40da-b64d-4c27-a932-dbc8cc51e8fe`. Their stdout/stderr copies
are in `find-diagnostics/native-stdout.txt`, `native-stderr.txt`,
`startup-stdout.txt` and `startup-stderr.txt`.

## Final verification status

| Check | Result |
| --- | --- |
| Rust validation/Unicode/escaping tests and formatting | 103 Windows-crate tests on Linux; fmt passed |
| Bounded Windows build and static checks | Debug/release builds, release all-targets Clippy with warnings denied, 133 actual Windows native tests passed |
| Hidden native page-find fixture and owned-range cleanup | Passed: eight groups, 17.443 seconds |
| Hidden debug doctor and owned-host readiness | Passed: 3.067 seconds |
| Pending-action, surface isolation, navigation/closure and deferred-close scenarios | Passed in the native subset; exhaustive races remain open |
| Existing browser regression checks | 32 groups: base 6, DOM queries 6, DOM actions 8, capture 6, extended waits 6 |
| Artifacts and owned process cleanup | NSIS packaged; three release doctor entrypoints passed; 28 recorded PIDs checked with reuse detection |
| Physical UI, Korean IME, DPI and accessibility | Not performed; acceptance remains open |

These native runs used owned hidden hosts, explicit owned IPC pipes, isolated
config/state and loopback fixtures. They did not inject OS keyboard/mouse input,
change focus, use the clipboard or visit external sites. `WM_CLOSE` targeted only
the verified owned hidden panel; it was not an Escape/IME simulation. The evidence
records `realImeTest:false`. Full lifecycle, physical UI/IME and Windows acceptance
remain open. Installer execution and physical desktop acceptance were not performed.


The final assertion review required all five known rendered Unicode fixture
queries to return `found:true`, without equating the query with the selected
codepoints. The strengthened Unicode subset passed in 5.185 seconds and the
panel subset in 4.283 seconds on the same final debug binary. Their evidence is
[Unicode](find-diagnostics/page-find-unicode-assertions.json) and
[panel](find-diagnostics/page-find-panel-assertions.json).

Final logs: [Linux tests](page-find-linux-tests.txt),
[Windows native tests](page-find-native-tests.txt),
[Clippy](page-find-clippy.txt), [release build](page-find-release.txt),
[NSIS packaging](page-find-nsis.txt). Related native evidence and per-step runner
results are in `find-diagnostics/`. Extended waits took 42.067 seconds including
the deliberate 27-second transport assertion; this was not an indefinite wait.
The CLI process-exit wait is five seconds with separate bounded output/cleanup
waits; the outer runner bounds the whole suite. No automatic test retry was used.

[Artifact hashes](artifacts-browser-find.json) record the final debug/release
GUI, CLI and console alias, plus the development installer. The installer is
2,358,655 bytes with SHA-256
`273129d1ba62dd4451854872b67b60eb9b7fd47b9dd7e53fd3718d7ccf487f3e`.
[Release entrypoints](native-browser-find-release-entrypoints.json) passed without
starting visible hosts. The installer itself was not executed.

[Cleanup observations](native-browser-find-cleanup.json) cover 28 recorded host,
shell, native-test and release-probe PIDs. One old fixture PID had been reused by
a Chrome process started after that fixture finished; the process was left alone.
In the final UTC-aware observation all 28 recorded PIDs were absent. Each native runner's owned job had zero
active processes after cleanup. The PID-only failure, the helper's timezone
comparison error and corrected UTC identity check are retained in
`find-diagnostics/`. The existing WSL flowmux PID 787 was still running; no
shared/Linux/macOS source was modified. Windows-crate tests do not constitute
full GTK/macOS runtime regression testing.

Remaining lifecycle cases include late callbacks after timeout, pending hide/show
or pane moves, dismissal versus navigation/reopening, concurrent actions,
client disconnect, renderer crash and host exit. Changed selection coverage uses
a replacement Range after completion; same-Range mutation, pending DOM changes
and shadow/text-control ownership are not claimed verified. B11 and G09 remain
partial; all 114 feature rows and 13 acceptance gates are retained.
