<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows Keybindings rows and owned editing dialogs

Options replaces the searchable list and permanent selected-action editor with
Linux's per-action arrangement: label, action ID, current bindings and Edit.
The scrolling viewport contains **35 rows: 28 configurable terminal actions and
seven disabled Edit controls**. Copy/Paste are fixed and explained in the help.
Reset all keybindings remains outside the scrolling viewport.

The Options-owned edit modal is **420×220 DIP**; its capture modal is **320×140
DIP**. Reset, Unbind and captured keys change only the draft. Cancel discards
unsubmitted work; OK validates and saves comma-separated shortcuts. Enter on
Cancel/Reset/Unbind/Capture now performs that button's action; Enter on OK or the
input confirms. General/Theme retain automatic application; shortcut help
correctly describes explicit OK saving without replacing real errors or Saving.

The existing worker and full-map compare-and-swap preserve external changes and
invalid/conflicting Unicode drafts. Replies identify the editor generation and
close it only when the submitted text is current and composition is inactive.
Owners disabled by a modal are restored when it closes. Capture tracks owned
window messages, with no global keyboard-state lookup or nested event loop.

## Final verification

The [final hidden native record](keybindings-dialog-diagnostics/4f1a383f8df0-native-keybindings-dialog-background.json)
passed **eight groups in 9.436 seconds** on the final debug source:

- 35 rows, 28 supported actions, seven actually disabled Edit buttons, current
  binding labels and different General/Keybindings saving hints.
- Edit/capture ownership and size, owner disabling/restoration, input/button
  bounds and nonoverlap, draft-only Reset/Unbind and Cancel.
- Posted Enter on each dialog button and the input through the real message
  loop, without desktop focus or physical keyboard input.
- Comma-separated bindings, renderer acknowledgement, old default removal and
  actual new-terminal creation through each new chord.
- Valid-draft OK blocked during controlled composition; exact Unicode draft
  retained through unrelated updates and invalid input.
- Stale full-map CAS preserving the external winner and local draft.
- Pure modifiers, valid Ctrl+Alt+Y under repeat/229/composition guards, capture
  Escape, captured draft and subsequent confirmed renderer dispatch.
- Confirmed unbind/default restoration without replacing existing terminal PIDs.

The [Options regression record](keybindings-dialog-diagnostics/f9c881dd36ad-native-options-live-background.json)
passed **nine groups in 8.486 seconds** against the same final staged executable.
Windows units passed **33 tests in 1.148 seconds**: 22 keybindings, one store,
eight CLI and two protocol tests. That binary predates the UI-only width/help/
Enter corrections; its tested core files are unchanged. Final Windows Clippy
passed in **24.83 seconds**. Changed Rust files pass rustfmt; whole-Windows
`cargo fmt --check` reports an existing untouched formatting difference in
`windows/src/state.rs:246`. Node tests were not repeated because no frontend
source changed. Acceptance stays **62 partial / 52 pending**, U09 partial.

## Images and preserved corrections

Final [rows](keybindings-dialog-diagnostics/f8d123e943a1-keybindings-rows.png),
[edit popup](keybindings-dialog-diagnostics/2d0b56d196f1-keybindings-edit.png) and
[capture popup](keybindings-dialog-diagnostics/d727ea39563d-keybindings-capture.png) PNGs match the
reviewed pixels exactly. Button labels and OK-based footer text are visible.
These use the production native BUTTON/STATIC painters on an owned offscreen
DIB. **EDIT pixels, title bars and composed GPU/desktop output are absent**;
actual EDIT text and geometry are checked separately. Capturing leaves the
model/dialog state unchanged.

The first 9.414-second pass preceded button-width/footer fixes and stronger
valid-chord guard assertions. The later 11.181-second pass preceded the Enter
fix. Both remain superseded evidence, not additional final acceptance groups.
Earlier Options passes are likewise retained separately.

A cross-process HFONT measuring helper failed in **6.266 seconds** (native work
5.189 seconds). Foreign process GDI font handles could not be selected; the
extra helper was removed, retaining bounds and actual PNG inspection. Review
then found Enter always confirming even on Cancel. The previous binary
reproduced that failure in **5.004 seconds** (native work 3.937 seconds). The
final eight-group run verifies the corrected message-loop route. Initial
needless-borrow and capture-method privacy compile failures also remain intact.

## Build and package provenance

Final debug and static-CRT release Cargo invocations failed canonical GUI
publication with Windows OS error 5 after linking, in **83.2 seconds** and
**118.065 seconds**. These remain failed Cargo results. Fresh deps executables
were checked against their build time and hashes, then copied into separate
`keybindings-dialog-debug-keys` and `keybindings-dialog-release-keys` stages.
The running user's GUI was not stopped, moved, overwritten or focused.

All three final release entrypoints (GUI, CLI and console alias) passed doctor
in **0.866 seconds**. NSIS packaging passed in **11.776 seconds**. Installer:
`windows/dist/flowmux-windows-0.10.1-dev-keybindings-dialog-x64-setup.exe`
(**5,990,078 bytes**), SHA-256
`3dd1562d2a1332fc7da7d2ab397e69101033dc8befd5c52b9241de53897dae08`. It was not installed over the running application.

The [frozen diagnostics manifest](keybindings-dialog-diagnostics/manifest.json)
contains 105 artifacts (including 12 PNGs), 28 process checks,
8 native records and 14 binary hashes, including superseded and failed
runs. Final snapshots cover 144 Windows runtime files, one read-only shared
catalogue, 11 verifier/installer/acceptance files and package documents. All
12 Windows runner Jobs report zero owned processes after cleanup.

## Remaining boundaries

Owned WM_IME_START/END and key messages establish application guards, not real
Korean IME/TSF composition, candidates or physical keyboard capture. Posted
messages verify routing for a target HWND, not physical Tab/focus behavior.
Renderer hooks use the production handler with synthetic events; physical OS/
WebView accelerator delivery and all configurable actions are not established.
Browser/editor/native-control rebinding remains unsupported. Accessibility,
high contrast, per-monitor DPI and full Linux visual equivalence are unverified.
Complete General/Theme settings, font picker, Update and remaining screens are
tracked separately in [UI_PARITY.md](../../UI_PARITY.md).
