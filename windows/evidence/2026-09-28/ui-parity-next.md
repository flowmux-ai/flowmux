<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Search, pane tools and workspace overview

This stage follows the Linux screen structure. It does not establish complete
Windows/Linux visual or functional parity. All native verification uses owned
hidden hosts, isolated state and explicit IPC pipes. The user's running app is
not installed over, closed, moved or driven by desktop input.

## Changes

- Search is an owned 760×520 modal window. Native query/case/refresh controls,
  status and descriptive two-line rows lead to Show more and result activation.
  Closing or successfully opening a result releases the owner. IME composition
  postpones searches and native EDIT retains Enter/Escape handling. CLI pages
  remain 500 hits; UI Show more rescans up to 700 with fresh valid result IDs and
  an explicit display-limit message. Double-click activation and preview font
  still differ from Linux; physical IME is unverified.
- Pane headers follow maximize/restore, split right, split down, add tab, browser,
  More order. Narrow headers hide leftmost tools and retain More; tab context
  menus retain the hidden functions. Browser adds a tab to its source pane.
  More closes the whole pane through one editor barrier, preserving dirty
  documents and refusing the last pane. The CLI browser-open split contract
  remains unchanged.
- Workspace overview is a child overlay in the main window with real workspace
  cards, active selection, live names/order, clipped scrolling, selection,
  dismissal and guarded close. Footer entry and terminal Ctrl+Alt+K are wired.
  Previews use owned WebView2 CapturePreview and native GDI+ thumbnail drawing.
  Missing or failed previews are explicit. At most two native capture slots
  remain outstanding, with a two-second request deadline, five-second opening
  budget and 128 preview limit. Retired callbacks cannot populate a new opening.
- Overview debug capture routes the real production WM_DRAWITEM handlers into
  an offscreen DIB, including cached thumbnails. Native z-order and ancestor
  clipping are preserved. This is not a composed desktop/GPU screenshot.

## Verification

| Native subset | Unique grouped checks | Inner verifier time |
|---|---:|---:|
| Search dialog, Unicode query, 500/700 results, modal lifetime | 4 | 4.500 s |
| Direct pane tools, bounded headers, atomic guarded pane close | 2 | 7.920 s |
| Overview real model, names/order, clipping, exact keyboard targets, close | 5 | 6.138 s |
| Overview dirty editor refusal and explicit discard/close | 2 | 6.211 s |
| Native chrome, Unicode captions, stable IDs/PIDs and theme raster | 5 | 8.221 s |
| Overview browser thumbnail pixels and unchanged focus/scroll/PIDs | 2 | 4.912 s |

These are **20 unique groups**, not completed acceptance features. Search/pane/
chrome ran `ui-parity-next-verified-debug`; final overview cases ran
`ui-parity-next-final-debug`, which differs from the earlier verified stage only
in capture visibility/readiness guards and internal browser readiness-field
visibility.
An earlier overview dirty run (4.372 s) is preserved but not counted twice.
All corresponding Windows runner Jobs report zero active owned processes after
cleanup. Final native startup succeeded after the clap change; measured nested
builder frames fell from 932,880 to 827,832 bytes, with the same one-MiB reserve.

The hidden terminal capture produced a real PNG with background/cursor but no
verified terminal glyphs. It is **not** terminal raster acceptance. Inactive
workspace previews are explicitly unavailable. The additional browser case
verified 33,714 exact `#0c2238` pixels from the
owned fixture page inside the production card raster, and preserved DOM focus,
scroll and terminal PIDs. A later mixed browser/terminal image also showed
terminal text; freshness and complete terminal glyph preservation are still
unverified. Successful PNG callbacks alone do not establish these properties.

CLI/JSON compatibility: eight command tests passed (49.253 seconds including
compilation; 0.15 seconds test execution). The terminal shortcut's two existing
checks passed after adding Ctrl+Alt+K and composition/AltGraph guards. The CLI
stack-fix Windows Clippy run with warnings denied passed in 27.645 seconds; the
final capture-readiness Clippy run passed in 24.912 seconds. Rust formatting and
whitespace checks passed.

## Failures and corrections retained

- Initial Clippy found a wrong SS_NOPREFIX import; it was moved to
  SystemServices. A subsequent run found three unnecessary format-string
  borrows; these were corrected.
- One owned debug build was explicitly cancelled after 43.125 seconds when a
  late capture callback/overall capture-budget gap was found. Cancellation used
  the runner so its owned child process group was terminated and recorded.
- The first linked debug host overflowed its one-MiB main-thread stack on two
  native starts. Matching PE/PDB unwind data identified nested clap builders:
  Command 594,040 bytes and browser::Op 338,840 bytes before outer frames.
  Notify, Find and NotifyComplete arguments were separated using the existing
  clap::Args pattern; the stack reserve was not increased. The eight CLI/wire
  tests preserve the existing commands and flat JSON shape.
- The initial pane verifier completed direct toolbar/geometry checks but read
  expected error JSON from stdout. CLI errors are written to stderr; the
  verifier was corrected to inspect that stream and retain the exact rejection.
- Search control discovery initially traversed WebView subprocess HWNDs. The
  verifier now takes the exact owned Search HWND from Tree chrome diagnostics.
- Overview initially queued hidden workspace views before the active view. Two
  hidden CapturePreview callbacks stalled and blocked visible captures. The
  existing browser visibility/readiness contract is now applied to all preview
  sources; inactive previews are explicitly unavailable and do not occupy COM
  slots. A Clippy pass also caught sibling access to private browser readiness
  fields; their visibility was restricted to the parent module.
- Canonical debug/release GUI executables are held open by the running user app.
  Cargo publication can fail with OS error 5 after fresh deps binaries link.
  Such Cargo results remain failed. Separate staging requires fresh link times
  inside the recorded build interval and matching hashes. Other canonical CLI
  entrypoints can be updated by Cargo; the running GUI is untouched.

## Remaining boundaries

Native HWND commands establish handlers, geometry, actual retained state and
production native raster behavior. They do not establish physical IME candidate
placement/composition, activation/focus restoration, pointer/menu navigation,
accessibility, per-monitor DPI transitions or complete Linux pixel parity.
Inactive workspace/tab thumbnails are not captured; a future last-visible
preview cache is not implemented. Overview shortcut routing outside terminal
WebViews remains pending. Agents,
Worktrees, complete Options pages and the other gaps in [UI_PARITY.md](../../UI_PARITY.md)
remain separate work.

## Artifacts

Final debug linking took 83.681 seconds; static-CRT release linking took 116.059
seconds. Both Cargo invocations failed while publishing the locked canonical GUI,
then all three freshly linked deps entrypoints were verified and separately
staged. The release GUI/CLI/console doctor entrypoints passed in 0.968 seconds.
NSIS packaging passed in 13.901 seconds. Installer:
`windows/dist/flowmux-windows-0.10.1-dev-ui-parity-next-x64-setup.exe`
(5,919,133 bytes), SHA-256
`8b306f8452ecc82fa52b52fcaeffef124d592e3214a97a422efefc14ff59458b`.

Full results, failure logs, source snapshots, stack measurements, production
native PNGs and staged-binary hashes are preserved in
[the frozen diagnostics manifest](ui-parity-next-diagnostics/manifest.json):
112 preserved artifacts, 29 check records (15 passed, 13 failed, one cancelled),
13 related native records and 10 binary hashes. All copied/source hashes and
140 current runtime source hashes were verified before freezing. Failed earlier
attempts remain historical evidence, separate from the 20 passing native groups.
No running application was replaced and the installer was not executed.
The [final focus-routing follow-up](ui-parity-focus.md) records two later
source corrections, seven repeated affected checks and the newer installer.
