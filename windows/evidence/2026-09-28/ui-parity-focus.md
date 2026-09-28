<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Final pane-menu and overview focus routing

This follows the [20-group UI structure verification](ui-parity-next.md).
Final read-only review found two input-continuity gaps. Closing an inactive
pane through More left native focus on the main window because only the active
pane close restored it. The menu success path now calls the existing
`focus_active` helper; the CLI/helper still preserves inactive-pane focus.
Rebuilding overview cards after workspace close/reorder destroyed the focused
HWND without requesting focus on a new card. The rebuild now sets the existing
`reveal_focus` flag before layout.

Only `native/workspaces.rs` and `native/overview.rs` differ from the earlier
runtime source snapshot, with one changed line in each file. The existing
background-test guards remain in place. Hidden tests can verify retained state,
card replacement, geometry, target routing and guarded closure. They deliberately
do not activate a desktop window or establish physical keyboard-focus recovery.
Actual focus recovery, IME composition/candidates and desktop interaction remain
unverified under the user's background-only requirement.

The two affected hidden native scenarios passed again: pane tools and guarded
pane close (two groups, 9.140 seconds) and overview/card/keyboard-target workflow
(five groups, 5.066 seconds). These repeat seven of the earlier 20 unique groups;
they do not add new acceptance features. Both runner Jobs report zero owned
processes after cleanup. Final Windows Clippy with warnings denied passed in
24.319 seconds; formatting was checked for both changed Rust files.

The debug build linked fresh entrypoints in 81.129 seconds but failed canonical
GUI publication because the running app holds that executable open (OS error 5).
That Cargo result stays failed. Fresh deps entrypoints were separately staged
with verified link times and hashes as `ui-parity-focus-debug`, then used for the
two native scenarios. Earlier immutable binaries, installer and diagnostics
remain historical records.

[The follow-up diagnostics](ui-parity-focus-diagnostics/manifest.json) preserve
this source boundary, native results, logs and staged-binary hashes.

The final static-CRT release linked in 117.725 seconds and encountered the same
locked canonical GUI publication failure. Separately staged fresh release GUI,
CLI and console doctor entrypoints all passed (0.891 seconds). NSIS packaging
passed in 11.761 seconds. The latest installer is
`windows/dist/flowmux-windows-0.10.1-dev-ui-parity-focus-x64-setup.exe`
(5,920,563 bytes), SHA-256
`d6d23c78b13d4ecdb2a8a549ad5da311aa788a14710dda35f7140c9d4f241982`. It was not installed; the running user app is unchanged.

The follow-up manifest is frozen after verifying all preserved/source artifact
hashes, 7 binary hashes and 140 current runtime source hashes. It contains
25 artifacts, 7 check records (five passed, two failed canonical publications)
and two passing native records. Physical focus recovery remains a stated
verification boundary, not an accepted result.
