<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows workspace and tab visual alignment — 2026-09-28

This Windows-only continuation adds a 260-DIP sidebar, matching the Linux default in `crates/flowmux/src/ui/window/mod.rs`, with 58-DIP workspace row spacing. Rows paint the workspace name and active surface cwd on separate lines, round the background, retain custom color stripes and unread counts, and ellipsize long paths. Terminal, browser and editor tabs have distinct geometric glyphs. The focused pane's selected tab has a stronger accent than selected tabs in other panes. Linux/macOS and shared sources are unchanged.

Metadata and same-layout pane/tab focus changes update existing native BUTTON handles. They do not recreate terminals. The standard native controls remain responsible for keyboard, accessibility and text input; this drawing change does not replace their input machinery. Physical behavior still requires acceptance below.

## Actual hidden Windows checks

[Unicode name and changed cwd](chrome-details-diagnostics/e5224231729a-unicode-cwd.png), [light theme](chrome-details-diagnostics/f8d3b6ee90d7-light.png), [mixed surface tabs](chrome-details-diagnostics/925fe7f57fa8-terminal-browser-editor.png) and [sidebar overflow](chrome-details-diagnostics/b0620ddc51c6-overflow-last.png) are artifacts from the production native renderer using actual owned HWND labels and geometry. They exclude WebView/GPU contents and are not composed desktop screenshots. Hosts remained hidden; no desktop input, clipboard, installation or user-session termination was used.

| Check | Result and scope |
| --- | --- |
| Details, three checks | [9.597 s](chrome-details-diagnostics/3fab437da257-result.json): Korean/NFD/emoji/ampersand caption codepoints, real CMD cwd change, model color stripe and painted second line; stable HWNDs/PIDs on metadata and pane focus; exact focused-tab accents; light/dark rendering and terminal settings in a two-terminal split; later mixed surface render |
| Overflow, one check | [6.431 s](chrome-details-diagnostics/ccd0bf66519d-result.json): hidden host resized to 900×400, five workspaces/four visible rows, first/last active visibility, disabled pager endpoints, bounds and no overlap |
| Workspace regression, six checks | [16.076 s](chrome-details-diagnostics/7cabb9463922-result.json): Unicode/title lock, invalid requests leave state unchanged, 41 reorders preserve PIDs, hidden calling context, targeted descendant cleanup, restart/Korean history, color clear and final-workspace protection |
| Browser regression, six checks | [16.869 s](chrome-details-diagnostics/744a9546d805-result.json): Unicode DOM/address, navigation, popup lifecycle, mixed-pane move/reuse, inactive caller cwd, browser-only restart |
| Final Windows Clippy, all targets | [24.729 s](chrome-details-diagnostics/b4dd0c7f8fd1-result.json), passed |
| Debug build | [83.345 s](chrome-details-diagnostics/f079bd8289ba-result.json), passed |
| Staged release entrypoints | [1.063 s](chrome-details-diagnostics/29cfb0cef75d-result.json): GUI, CLI and `.com` doctor pass; debug background switch disabled |
| NSIS package | [11.358 s](chrome-details-diagnostics/81a3487f5c4c-result.json), passed; installation not performed |

The workspace verifier now uses the existing hidden `CliProbe` helper instead of unbounded direct commands: five-second commands/conditions, eight-second startup, 110-second inner budget under a 120-second Job, bounded exit/kill cleanup, and no automatic mutation retry. Chrome details and overflow are separate cases with 55-second inner/60-second outer budgets. Every native runner reports zero processes after Job cleanup.

The [frozen manifest](chrome-details-diagnostics/manifest.json) preserves 45 artifacts, 11 runner records, four native records and five binary hashes. Its [source snapshot](chrome-details-diagnostics/44c8d31bff22-chrome-details-source.json) contains 212 hashes, all unchanged after validation. The initial Clippy pass preceded the final small high-contrast text/minimum-row-height adjustment; the final Clippy and debug build include that adjustment.

## Build and installer boundary

The [release Cargo invocation](chrome-details-diagnostics/58c61072cbf8-result.json) failed at publication of the canonical `release/flowmux.exe` with OS error 5 after linking. The running application's file was left untouched. The [staging record](chrome-details-diagnostics/fe7ce7349a7b-chrome-details-release-staging.json) verifies that all three `release/deps` PE files were freshly produced inside that invocation and match the validated source snapshot. Copies in `windows/dist/chrome-details-release` passed native diagnostics and supplied NSIS. This is not reported as a successful whole Cargo build.

Installer: `windows/dist/flowmux-windows-0.10.1-dev-chrome-details-x64-setup.exe`, **5,830,069 bytes**, SHA-256 `ddc25c7c38249b76968aadaf54908e0ebd1249d090e3114b8fd4dd43aed5fee8`. The currently running/installed application was not replaced.

## Remaining limits

Agents, broader workspace status metadata, the full Linux icon set and some popup/panel styling still differ. The mixed render supports visual inspection of three glyphs; the script does not assert each icon's pixel identity. Light/dark transitions were checked before opening the browser/editor. It does not establish mixed-surface theme transitions or native-handle preservation during theme changes.

NFD Hangul codepoints are preserved, but the native capture shows separate Jamo glyphs; this is not proof of composed Hangul shaping. Physical Korean IME, pointer/keyboard/focus, pager clicks/wheel, high-contrast rendering, per-monitor DPI, accessibility and composed WebView visual acceptance remain open. Existing partial acceptance statuses are retained. The [previous chrome evidence](chrome.md) and installer remain historical and unchanged.
