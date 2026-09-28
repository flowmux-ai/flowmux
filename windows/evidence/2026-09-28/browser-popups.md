<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows browser popup tabs — partial acceptance

The final hidden Windows popup suite passed **14 groups in 45.107 seconds** on
2026-09-28. It exercised actual native opener/WindowProxy behavior, tab routing,
Unicode payloads, closure and live capacity. Another 49 related regression groups,
pure Linux tests, Windows native unit tests, formatting, release Clippy and the
release build passed as recorded below. NSIS packaging, three release entrypoint
doctor probes, artifact hashes and the final owned-process snapshot are recorded
separately; this is not full browser or Windows acceptance. Linux/macOS
and shared implementations are outside this Windows-only change.

## Native attachment and routing

WebView2 `NewWindowRequested` events are routed into a new browser tab in the
opener's existing pane. The source must still be that pane's active, logically
visible browser in the active workspace. The new tab becomes active, retaining
the other tabs and terminal sessions. Requests from hidden or closed sources are
rejected. HTTP, HTTPS and `about:blank` use the existing browser URL policy;
local files, data/script URLs and the reserved terminal origin are rejected.
Native URIs are bounded to 16 KiB before queueing, with normal encoded-URL
validation applied before preparing the tab.

The event is marked handled immediately to suppress uncontrolled native popup
windows. Its deferral and event arguments remain in a UI-thread-owned queue. The
opener's actual environment is obtained from `ICoreWebView2_2::Environment` and
passed to Wry's `with_environment` for the child. The child uses the same default
browser profile. Sharing a filesystem profile directory or `WebContext` alone
does not establish that the COM environments are the same.

The popup child has never been navigated: its constructor sets neither `with_url`
nor `with_html`, including no preliminary `about:blank` navigation. Its handlers
and settings are installed, its model/native entry is committed, then
`SetNewWindow(child)` and deferral `Complete()` bind the actual native child to
the request. There is no separate `Navigate(request_uri)` or URL-only fallback.
This retains the runtime's WindowProxy/opener mechanism where native web security
and opener policy permit it. Microsoft requires the child to share the opener's
environment/profile and forbids prior navigation; the native content is installed
through `NewWindow`. See the official
[NewWindowRequested arguments reference](https://learn.microsoft.com/en-us/microsoft-edge/webview2/reference/win32/icorewebview2newwindowrequestedeventargs?view=webview2-1.0.4129.50#put_newwindow).

Child creation is dispatched after the original COM callback returns. Every COM
object remains on the host UI thread; only the wake event crosses the host event
channel. `Rc` ownership is used without unsafe `Send` implementations. This follows
Microsoft's [WebView2 threading and deferral guidance](https://learn.microsoft.com/en-us/microsoft-edge/webview2/concepts/threading-model),
which requires UI-thread access and postponing work that would create a nested
message loop inside a WebView2 callback.

Wry's custom new-window callback is omitted. In the installed Wry 0.57.0 adapter,
omitting it makes the existing handler synchronously set `Handled`; the flowmux
native handler then owns the deferral and attachment. Keeping the former custom
`Deny` callback would introduce a separate asynchronous decision. The relevant
adapter is in [Wry's WebView2 source](https://github.com/tauri-apps/wry/blob/792d0359ba6501a4fc360ece17de2ae42329a47c/src/webview2/mod.rs).

## Bounds, cancellation and failure handling

A host permits eight pending native requests, including requests drained from
the queue but still being constructed, and at most 16 live popup-created tabs.
Each request captures its source surface, navigation generation and exact logical
visibility revision. Source validity is checked after child creation as well as
before attachment because Wry creation pumps native messages. Requests older than
12 seconds are rejected when checked. This validity budget does not interrupt a
blocked COM creation call or guarantee that the UI can process cancellation at
exactly 12 seconds.

Queue rejection and host routing failures contribute to `rejected` and bounded
`last_error` diagnostics. Popup status also reports `pending`, `opened`, live
`active`, `limit` and `pending_limit`. Browser entries record their source surface
in `popup_opener` and native `popup_user_initiated` metadata. The latter is
informational: the implementation does not require a trusted user gesture or
provide a popup-permission prompt. The existing capacity, visibility and URL
checks still apply to programmatic requests.

Dropping a rejected request completes its handled deferral without a child.
Surface cancellation and host shutdown also reach requests already drained into
local variables, through tracked native handles. No `RefCell` borrow is held
across deferral completion. Popup/download controllers precede browser views in
the host's drop order, covering early-error unwind as well as explicit shutdown.

Construction, navigation and pre-attachment controller failures cancel registered
surface state before dropping an unpublished view. Attachment failure removes
the child and restores the previous workspace. Once attachment succeeds, a layout
failure retains the native child and reports an error, since a WindowProxy may
already refer to it. Native failures are not automatically retried and do not
prove that a partially completed action had no effect.

## `window.close` and final-tab preservation

The installed Wry adapter registers a `WindowCloseRequested` handler that destroys
its native container HWND. An additional flowmux handler cannot safely undo that
destruction, and this implementation does not guess event-registration tokens to
remove Wry's handler. The behavior is visible in the
[same pinned Wry source](https://github.com/tauri-apps/wry/blob/792d0359ba6501a4fc360ece17de2ae42329a47c/src/webview2/mod.rs).

The flowmux callback immediately marks the browser natively closed, clears its
shared visibility and advances its generation so earlier script/wait/capture
results become stale. Later navigation starts on that closed instance are denied.
Model cleanup is queued with both the surface ID and a per-browser instance UUID.
This prevents an old native close event from closing a different instance.

Cleanup removes the exact browser tab and collapses its pane only when empty,
preserving other tabs and a surviving focused pane. If it was the workspace's
final tab, the destroyed browser is replaced with a fresh `about:blank` browser
using a **new SurfaceId**, while preserving the pane and workspace. The destroyed
controller is dropped before another HWND is allocated. Replacement creation
failure is reported; a later explicit structural action can retry, without an
automatic retry loop. These rules apply to actual native close events, including
events for ordinary browser tabs where the engine permits script closure.

## Measured native behavior

The [final native result](native-browser-popups-background.json) contains all 14
passing groups. Its [bounded runner](popup-diagnostics/popups-native-final-runner.json)
records 45.107 seconds, exit code 0 and `activeAfterCleanup: 0`. The owned host
also exited 0 with empty stderr. The final process snapshot below separately
covers recorded owned processes from the later regression and packaging checks.

| Case | Observed result |
| --- | --- |
| URL popup | A script call returned a live WindowProxy, created one active tab in its source pane, and let the child write through its native opener. Source browser identity and the original terminal process survived. |
| Target link | DOM `.click()` on `target="_blank" rel="opener"` created the child tab with native opener access. This was not a physical mouse gesture or a test of implicit `noopener`. |
| Blank popup | `window.open('about:blank')` returned a usable WindowProxy for synchronous `document.write`; the populated document and opener were retained. |
| History | Child navigation, back and forward retained the native opener. |
| Source routing | A server response gate delayed the script call until another pane was logically focused; the child still entered the original source pane. |
| Hidden source | Gated calls fired only after the source tab or workspace was confirmed hidden. Both were rejected without changing the active tab/workspace; diagnostics recorded `popup opener is hidden`. |
| Move | Opening after a source move retained the same source document and native opener, and used the source's current pane. Native deferral during the move was not forced. |
| Source close | The source disappeared while the child kept its exact SurfaceId and document token, with `native_closed:false`. This runtime cleared `window.opener` to null. |
| Child close | Native child `window.close()` removed that exact tab, restored the remaining source tab and made the retained child WindowProxy report `closed:true`. |
| Ordinary browser close | `nativeCloseObserved:true`: this runtime allowed closure of the manually opened fixture tab and retained the terminal. Other documents/runtime policies may refuse such closure. |
| URL policy | File, JavaScript and reserved terminal-origin requests created no tabs. Terminal targets rejected browser eval. The per-request delivery distinction is described below. |
| Live capacity | Sixteen popup tabs were created; a seventeenth was rejected with the live-limit error. Closing one freed capacity for another child. |
| Flood | Of 32 script requests, one child was created and 31 were rejected; pending requests drained to zero. The final rejection reason was hidden source. This does not prove eight requests were held simultaneously. |
| Final tab close | With only the popup child remaining, native closure replaced it with a fresh-ID `about:blank` browser in the same pane/workspace, with no remaining popup ownership. |

The URL and blank-document cases preserved the exact Unicode string
`새 탭 한글 한 é 😀 + &`, including decomposed Hangul and combining accents.
The child had no terminal bridge globals, and document focus remained false.
These are ordinal DOM/string observations, not glyph, clipboard or real Korean
IME verification. Native `IsUserInitiated` metadata is informational: the
source-close observation recorded true even though the test invoked JavaScript.
The suite does not establish delivery of a request whose native flag is false.

The file URL returned no WindowProxy and added zero to the host rejection counter,
consistent with engine filtering before host delivery. JavaScript and the HTTP
and HTTPS reserved terminal origins each added one host rejection and returned
no WindowProxy. The source's JavaScript marker remained undefined. Negative
policy checks include a bounded 200 ms observation interval; they are not an
unlimited guarantee against arbitrarily delayed native events.

The focus/hide fixture holds an owned loopback image response until the verifier
confirms the requested logical focus or hidden state. Releasing the response fires
the image load handler, which calls `window.open` and sends a completion beacon.
This establishes ordering before native request delivery. It does not keep a
native deferral pending while the source moves, hides, navigates or closes.

## Failed checks and corrected assumptions

The initial pure routing test build failed because a test compared
`Option<String>` directly with `&str`; this was an assertion type error, not a
runtime failure. The corrected eight-test routing subset passed before the full
111-test Linux run. Both the [initial log](popup-diagnostics/popups-linux-routing.txt)
and [corrected log](popup-diagnostics/popups-linux-routing-fixed.txt) are retained.

The [first full popup run](popup-diagnostics/popups-native-all.json) passed seven
groups and then failed the source-close assertion in
[19.837 seconds](popup-diagnostics/popups-native-all-runner.json). It had required
the surviving child to retain an opener whose `closed` property was true. The
[diagnostic rerun](popup-diagnostics/popups-source-close-diagnostic.json) instead
showed unchanged child ID/token and `native_closed:false`, with `hasOpener:false`
and null opener fields. The engine had cleared the opener; it had not closed
the child. The verifier now requires the original source to be absent and the
child identity/document to survive, accepting either a cleared opener or an
opener reporting closed, with no opener-access error. No Rust fix was needed for
this test assumption. The [source-close subset](popup-diagnostics/popups-source-close-fixed.json)
then passed in [4.740 seconds](popup-diagnostics/popups-source-close-fixed-runner.json).

The original fixed-delay focus/hide scheduling was replaced with the server gate
to remove timing-dependent false failures. Changed subsets passed before the
final full run: [source routing, 3.892 seconds](popup-diagnostics/popups-source-pin-gated-runner.json),
[hidden source, 4.787 seconds](popup-diagnostics/popups-hidden-gated-runner.json),
and [policy, 4.908 seconds](popup-diagnostics/popups-policy-gated-runner.json).
The final run also used the corrected 200 ms policy observation timing.

The [initial existing-browser regression](popup-diagnostics/popups-regression-browser-initial-failed.json)
failed after five groups with `Browser-only layout differs` in
[13.151 seconds](popup-diagnostics/popups-regression-browser-initial-failed-runner.json).
Its old popup-denial assumption left the newly accepted child in the model, so
the later browser-only layout count was stale. The base verifier was corrected
to positively assert native popup creation and closure while retaining the
original browser and terminal identities. The corrected six-group rerun passed
in 13.644 seconds without a Rust change. Both runs are preserved; the initial
failure is not counted as a passing regression.

## Limits and verification status

The native runtime controls same-origin access, opener isolation, `noopener`,
named-target reuse and whether a script can close a window. Complete behavior
across those policies, frames, redirects and runtime versions is not established
by attaching a child successfully. Requested popup position/size/chrome features
are not reproduced as independent desktop windows: the child follows the pane's
tab geometry. Popup ancestry and counters are runtime metadata; saved browser
URLs do not reconstruct an opener relationship after restart.

Hidden verification must use owned hosts, explicit owned IPC pipes, isolated
config/state and loopback fixtures. It must not inject desktop input, request
foreground/focus changes, use the clipboard, manipulate user windows, install
software or visit external sites. Logical tab activation in a hidden host is not
physical foreground-window or Korean IME verification. The measured suite covers
startup and the specific lifecycle cases above. Native deferral during
navigation/close/move/hide-and-re-show, exact eight-request saturation, twelve-second
expiry, failure rollback, nested-popup chains, renderer crash and shutdown during
attachment remain unverified. The HTTP gate and flood case do not prove those
races or limits.

| Check | Result |
| --- | --- |
| Pure Linux tests | 111 passed; [runner, 10.214 s](popup-diagnostics/popups-linux-all-runner.json), [log](popup-diagnostics/popups-linux-all.txt). This runs the separate Windows workspace's platform-independent sources, not a native desktop. |
| Rust formatting | Passed; [runner, 9.910 s](popup-diagnostics/popups-format-runner.json). |
| Windows debug build | Passed; [runner, 62.216 s](popup-diagnostics/popups-debug-runner.json). |
| Windows release Clippy | All targets passed with `-D warnings`; [runner, 19.711 s](popup-diagnostics/popups-clippy-runner.json), [log](popups-clippy.txt). |
| Windows release build | Passed; [runner, 87.384 s](popup-diagnostics/popups-release-runner.json), [log](popups-release.txt). |
| Windows native Rust tests | 141 passed; [runner, 12.613 s](popup-diagnostics/popups-native-tests-runner.json), [log](popups-native-tests.txt). |
| Hidden native popup suite | 14 groups passed in 45.107 s; [result](native-browser-popups-background.json), [runner](popup-diagnostics/popups-native-final-runner.json). Specific outcomes and limits are above. |
| Related browser/terminal regressions | 49 groups passed on the same final Rust source; separate results are below. |
| Pending-request navigation/closure/visibility and failure rollback | Not deterministically forced; exhaustive races remain open. |
| NSIS package | Passed in [5.556 s](popup-diagnostics/popups-nsis-runner.json); [build log](popups-nsis.txt). The installer was not executed. |
| Release entrypoints | Three `doctor` probes passed in [0.911 s](popup-diagnostics/popups-release-doctor-runner.json); [results](native-browser-popups-release-entrypoints.json). |
| Artifact hashes | Seven debug/release/installer artifacts recorded in the [SHA256 manifest](artifacts-browser-popups.json). |
| Owned-process and fixture cleanup | [Read-only snapshot](native-browser-popups-cleanup.json): all 49 recorded host/shell/native-test/entrypoint PIDs absent; [runner, 1.571 s](popup-diagnostics/popups-cleanup-runner.json). Native runners also recorded zero active owned descendants after cleanup. |
| Physical UI, Korean IME, DPI and accessibility | Not performed; acceptance remains open |

| Related regression | Groups | Bounded runner duration |
| --- | --- | --- |
| [Base browser, corrected popup contract](popup-diagnostics/popups-regression-browser-fixed.json) | 6 | [13.644 s](popup-diagnostics/popups-regression-browser-fixed-runner.json) |
| [DOM queries](popup-diagnostics/popups-regression-browser-dom.json) | 6 | [8.517 s](popup-diagnostics/popups-regression-browser-dom-runner.json) |
| [DOM actions](popup-diagnostics/popups-regression-browser-actions.json) | 8 | [12.242 s](popup-diagnostics/popups-regression-browser-actions-runner.json) |
| [PNG capture](popup-diagnostics/popups-regression-browser-capture.json) | 6 | [15.003 s](popup-diagnostics/popups-regression-browser-capture-runner.json) |
| [Page find](popup-diagnostics/popups-regression-browser-find.json) | 8 | [14.399 s](popup-diagnostics/popups-regression-browser-find-runner.json) |
| [Extended browser wait](popup-diagnostics/popups-regression-browser-wait.json) | 6 | [41.385 s](popup-diagnostics/popups-regression-browser-wait-runner.json), including an intentional 27-second wait |
| [Downloads](popup-diagnostics/popups-regression-browser-downloads.json) | 8 | [14.973 s](popup-diagnostics/popups-regression-browser-downloads-runner.json) |
| [Active-download host shutdown](popup-diagnostics/popups-regression-downloads-shutdown.json) | 1 | [3.452 s](popup-diagnostics/popups-regression-downloads-shutdown-runner.json) |

The release `flowmux.exe`, `flowmuxctl.exe` and `flowmux.com` doctor commands all
returned exit code 0 and `status:ok`, reporting WebView2 `112.0.1722.48` and
`background_testing:false`. They are read-only CLI checks, not a rerun of the
hidden debug popup suite against release GUI hosts. No installer execution or
physical desktop acceptance is implied. The setup artifact is **2,378,201 bytes**
with SHA256 `27c42cc0ba15f1c7b4c7c1bc04014031d4b9c5d2f85b9643d8e1ac9272b23eab`.

The final process query was read-only and found every one of its 49 recorded
Windows process IDs absent. Its scope explicitly covers owned hosts/shells,
the Windows native-test runner and release doctor entrypoints; ordinary client
PID arrays and other runner IDs are excluded from that snapshot. It does not
claim a machine-wide process audit. Bounded native runner cleanup is recorded
separately, and the fixture lives within its owned verifier process.

B11 and G09 remain partial; 114 features retain 51 partial/63 pending and 13 gates
retain nine partial/four pending. Native popup events add no CLI operations; the
Windows browser command count remains 34. The unverified physical and lifecycle
limits above remain open. No result here completes the Windows goal.
