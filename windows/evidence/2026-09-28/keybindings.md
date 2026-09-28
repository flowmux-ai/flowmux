<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows terminal keybindings

Options now has General, Theme and Keybindings pages in the existing owned,
nonmodal window. General/Theme retain automatic application. Keybindings use a
searchable native list and an editor for the selected action, with Save shortcut,
Unbind, Use default and a separate Reset all keybindings action. Each line holds
one Ctrl+Shift+Key or GTK accelerator string; an empty saved list unbinds the
action. Save applies committed bindings immediately. This compact Win32 layout
is not identical to Linux's per-action editing dialogs.

The shared Linux catalogue is reused read-only: **37 actions are displayed,
28 have configurable Windows terminal bindings, seven are not configurable and
two clipboard actions are fixed**. A nonconfigurable binding does not imply the
underlying feature has no other Windows entrypoint. Clipboard defaults remain
fixed to preserve terminal copy/paste and Ctrl+C behavior. The product help
limits the new configuration to terminal input; browser, editor and native
control shortcuts are not covered.

Validation rejects malformed, duplicate, conflicting and reserved chords before
saving. Draft text remains separate from resolved physical key codes. Full-map
compare-and-swap checks run under the existing settings writer lock; external
changes cannot be silently overwritten by a stale edit. General Reset preserves
keybindings, while the separate bindings reset preserves General settings.
Composition guards prevent committing intermediate IME text. Worker replies
preserve subsequent typing, including edits made after Reset was submitted.

## Verification

The final strict hidden native workflow passed **eight groups in 6.598 seconds**:

- Owned Options tab, all catalogue rows, search, unsupported editing restrictions
  and control bounds.
- Default Ctrl+Shift+T through the actual renderer handler creating one terminal.
- Controlled composition guards and exact Unicode drafts; malformed, duplicate,
  conflicting and reserved bindings rejected without replacing the saved value.
- Native Save changing the binding, disabling the old default and creating one
  terminal through the new chord.
- Repeat, composition, keyCode 229, AltGraph, dead-key, Meta and right-Alt guards
  preventing dispatch through the renderer handler.
- Unbind stopping dispatch and Use default restoring the exact binding.
- An external binding change winning compare-and-swap while the local draft
  remains intact until explicit Reload.
- Separate binding reset and close/reopen preserving General settings and
  terminal process identities.

The final record is
`windows/dist/evidence/keybindings-518a4489-9e0a-4195-ace4-e8b4a7093227/native-keybindings-background.json`.
The earlier eight-group passing run at 6.907 seconds is preserved separately;
it is not added to the final eight groups. Existing Options regression coverage
also passed **nine groups in 8.265 seconds**. Windows unit coverage passed
**32 tests in 1.271 seconds** through the counted runner, and Node coverage passed
**62 tests in 1.020 seconds**. The additional nine-test root subset is overlapping
coverage and is not counted again.

Final Windows Clippy with warnings denied passed in **24.736 seconds** after
initial type/trait errors were corrected. These tests and native groups are not
completed acceptance features. U09 moves to partial; the latest acceptance
count is **62 partial / 52 pending**, with no feature marked complete.

## Failures and corrections retained

The first native run passed six groups, then failed after 11.048 seconds while
waiting for the stale-draft conflict scenario. Programmatic WM_SETTEXT on a
multiline EDIT did not emit EN_CHANGE. The fixture now sends that notification
to the exact owned parent after setting the text; the production input behavior
was not replaced with a test-only implementation. The original failure remains
preserved in `keybindings-b12a0918-fa54-4b93-952d-91e9b41964cd`.

A subsequent run passed, then the collision assertion was tightened to require
an actual duplicate-binding diagnostic instead of accepting a broader error.
The final 6.598-second run passed that stricter assertion. Review caught late
Reset replies replacing newer drafts. The final implementation snapshots
submitted drafts and also reads the live EDIT before applying replies, preserves
the composition comparison baseline and avoids replacing identical text
unnecessarily.

## Build and artifact provenance

Debug and static-CRT release Cargo runs took **88.667 seconds** and
**117.125 seconds**, respectively. Both failed publication of the canonical GUI
with Windows OS error 5 while the user's running executable remained open.
These Cargo results remain failed. Fresh linked deps entrypoints were checked
and separately staged against the same runtime source snapshot; no running
user application was closed, replaced, moved or focused.

Runtime/verifier source snapshots, the read-only shared catalogue hash, original
failure and passing records, staging metadata and binary hashes are collected
in [the frozen diagnostics manifest](keybindings-diagnostics/manifest.json):
47 artifacts, 15 process-check records, four native records and eight binary
hashes. All three release GUI/CLI/console doctor entrypoints passed in
[0.964 seconds](keybindings-diagnostics/0a54aaa4a161-native-keybindings-release-entrypoints.json).
All six Windows runner Jobs reported zero owned processes after cleanup. The
first failed native run and superseded intermediate run remain preserved; only
the final eight groups count as Keybindings acceptance.

NSIS packaging passed in **12.369 seconds**. Installer:
`windows/dist/flowmux-windows-0.10.1-dev-keybindings-x64-setup.exe`
(**5,980,244 bytes**), SHA-256
`503905258730eb70bd4c7d15955a3d19818d9fc82dc36f2aa2948e4e609f4b75`.
The installer was not executed over the user's running application. Its packaged
implementation notes retain the historical 6.907-second passing run; this final
evidence records the stricter 6.598-second run without counting both.

## Remaining boundaries

WM_IME_START/END messages establish application guards, not real Korean OS
IME/TSF composition or candidate behavior. The renderer hook supplies synthetic
events to the production handler: it does not establish physical keyboard,
focus, WebView/OS accelerator routing, accessibility, per-monitor DPI or
composed desktop rendering. The native dispatch scenario exercises new-terminal
creation; it does not establish physical-key execution of every configurable
action. Browser/editor/native-control rebinding and key capture remain pending.

Full Linux GUI/feature parity, the complete General/Theme options, font picker,
Update page and other gaps in [UI_PARITY.md](../../UI_PARITY.md) remain separate
work. No visible test windows, desktop input or clipboard access were required
for this hidden verification.
