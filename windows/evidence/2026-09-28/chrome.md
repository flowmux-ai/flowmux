<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Native Windows shell visual alignment — 2026-09-28

The Windows-only shell now uses a compact Workspaces header/list, lower sidebar actions, pane-local tabs with close/add/menu controls, themed native controls and DPI-scaled Segoe UI. The former global row of nine raised buttons and the 248-DIP gap above workspace rows are removed. Dark terminal background/foreground and the 16 ANSI colors match `crates/flowmux/src/theme.rs` defaults. Shared Linux/macOS sources remain unchanged. This is a partial visual alignment, not full Linux parity.

## Actual hidden native verification

[Dark chrome](chrome-diagnostics/0d7bf71af74e-dark.png) and [light chrome](chrome-diagnostics/c99716922e1e-light.png) show live HWND geometry and labels painted by the same production background/button renderer into an owned in-process DIB. These are **native chrome render artifacts**, not composed desktop screenshots: WebView terminal/editor/browser contents are absent. No desktop window, input, clipboard or physical IME test was used.

The [6.07 s](chrome-diagnostics/e5f03f6c281b-result.json) case checks actual HWND ownerdraw styles/fonts, workspace rows at 40 DIP, 28-DIP tab strips, per-pane right-edge controls, bounds/no overlap after splitting, Korean labels, and exact dark/light background and workspace selection pixels. Theme changes preserve terminal identities and PIDs; live xterm settings acknowledge the Linux dark background. The final [source snapshot](chrome-diagnostics/439169c77519-chrome-source-final.json) contains 212 verified hashes.

| Related check | Result |
| --- | --- |
| Final Windows Clippy, all targets | [20.712 s](chrome-diagnostics/2d2b37655c7e-result.json), passed |
| Final debug build | [80.492 s](chrome-diagnostics/f33dab8e8777-result.json), passed |
| Composition/restore settings deferral | [0.207 s](chrome-diagnostics/2de2a2d60996-result.json), passed |
| Native Files Copy→Rename→Move | [7.87 s](chrome-diagnostics/dff4779f7ee3-result.json), passed; Unicode paths/bytes, LISTBOX and real Monaco |
| Native editor move | [9.374 s](chrome-diagnostics/85c24730f79a-result.json), passed; same view/document and undo history |
| Native browser suite | [16.493 s](chrome-diagnostics/f62f7ead7bfc-result.json), six checks passed; Unicode address/DOM, navigation, mixed pane moves and restore |
| Three staged release doctor entrypoints | [0.952 s](chrome-diagnostics/415542770429-result.json), passed; GUI, CLI and `.com`, background switch disabled |
| NSIS packaging | [11.575 s](chrome-diagnostics/dfa1ba49b020-result.json), passed; no installation |

## Release boundary and preserved failures

The release Cargo invocation **failed** after successful linking because it could not remove the existing canonical `release/flowmux.exe` (OS error 5). That file was left unchanged; no running app was terminated. All three freshly linked `release/deps` PE files were produced within that invocation, after the final source changes. Their hashes, freshness checks and the independently verified source snapshot are in the [staging record](chrome-diagnostics/3518b0eafcd2-chrome-release-staging.json). Copies in `windows/dist/chrome-release` passed the three native release diagnostics and supplied NSIS. This is not reported as a successful whole Cargo invocation.

Installer: `windows/dist/flowmux-windows-0.10.1-dev-chrome-x64-setup.exe`, **5,825,764 bytes**, SHA-256 `6a8ebe32812c9f580bcaf3b52539b2e6da9c853947cf752bbf6a35c2cb50246a`. It contains both the preceding F08 file operations and this visual change. Installation and replacement of the current running application were not performed.

The [manifest](chrome-diagnostics/manifest.json) preserves 73 artifacts, 21 runner records, eight native records and five binary hashes. Native Job cleanup counts are zero. Earlier missing `WM_MOUSELEAVE` / `IsWindowEnabled` imports and the corrected checks remain recorded. External hidden-window paint attempts returned magenta/black images or failed child PrintWindow calls; those captures are explicitly excluded from visual acceptance, even where a capture-only runner exited successfully. The production renderer on the owning UI thread resolved this verification limitation. An optional local PNG reread using Python Pillow was unavailable; pixel checks already run through the Windows fixture and rendered artifacts were inspected directly.

## Remaining parity limits

Workspace path/status metadata, Agents, the full symbolic icon set, rounded Linux row styling and some native popup/panel styling still differ. Physical pointer/keyboard/IME, high-contrast rendering, per-monitor DPI transitions, accessibility and full composed WebView visual acceptance remain pending. Hidden rendering does not prove those gates. This stage does not claim the complete Windows implementation or all 114 feature rows are finished.
