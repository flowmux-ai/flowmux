<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows installed terminal font picker

The hidden Windows picker, existing Options and Keybindings regressions passed.
An installer candidate is available; it was not installed over the user's app.

Options keeps the original font-family/fallback EDIT and adds Choose. It opens
an owned **460×420-DIP modal** with a native search EDIT, LISTBOX, Use font and
Cancel. Choices include installed curated monospace families, the exact current
custom value and Windows default. Search never writes settings. Use font, Enter
on the selection/input or a list double-click returns the chosen value through
the existing Options row and asynchronous settings writer; Enter on Cancel
performs cancellation.

The original comparison baseline stays fixed while the picker is open. A real
selection uses existing compare-and-swap validation; stale external changes or
newer/composing raw input are not silently overwritten. Cancel and an unchanged
Current choice reconcile an untouched row with the external winner without a
new settings write. Invalid/conflicting drafts remain available for correction.
Owners disabled by the modal are restored on close; hidden test mode neither
shows nor focuses the popup.

A new primary font preserves the existing fallback tail. Choosing the same
primary preserves the original expression. Unsupported or malformed CSS
expressions are not rewritten; the original Options EDIT remains available.
Current custom is preserved even when it is absent from the installed catalogue.
Raw search/input strings are not Unicode-normalized by the picker.

## Differences and bounded discovery

Linux uses an inline searchable DropDown; this Windows implementation uses a
separate owned modal. Linux's default selection removes the override and inherits
the theme font. **Windows default restores the existing Windows CSS font stack;
it is not theme inheritance.** Neither flow nor appearance is claimed identical.

One worker enumerates installed fonts once per Options lifetime and caches its
result. Callback checks limit discovery to **300 ms, 4096 visits or 256
candidates**. The UI independently discards results after **two seconds** before
accepting a queued result. There is no thread join or retry-driven thread growth;
the underlying native call cannot be forcibly stopped. Timeout or failure leaves
Current custom and Windows default usable, without presenting unverified names
as installed fonts.

GDI's fixed-pitch metadata and curated names do **not** establish WebView2/CSS
font availability, glyph coverage or correct Hangul fallback. Those rendering
properties require their own evidence.

## Final verification

The [picker record](font-picker-diagnostics/558ebe9fbc80-native-font-picker-background.json)
passed **nine groups in 8.829 seconds** on the final staged debug executable:

- Installed catalogue, exact custom fallback string, modal ownership and Cancel.
- Substring and no-result searches without saving.
- Unicode/NFD search preserved during controlled composition and other writes.
- Native list selection and posted Enter applying through the settings writer,
  with renderer acknowledgement and unchanged terminal process IDs.
- Cancel and unchanged Current choice revealing an external winner without writes.
- Original raw-input changes and composition blocking replacement.
- Stale selection preserving the external winner and conflicting local draft.
- Windows default restoring only the font chain.
- A conflicting choice equal to the old baseline retaining its error and draft
  through unrelated settings updates, until explicit Reload.

Actual GDI discovery visited **1399 faces in 2 ms**, returning 13 curated
families without truncation on this machine. This is an observation, not a
guarantee about another system or WebView glyph rendering.

The same executable passed the [Options regression](font-picker-diagnostics/4e49c5239bc7-native-options-live-background.json)
in **9.238 seconds / nine groups** and [Keybindings regression](font-picker-diagnostics/fb79266efd6b-native-keybindings-dialog-background.json)
in **9.384 seconds / eight groups**. The new [font contract unit](font-picker-diagnostics/84288274c693-font-picker-unit-evidence.json)
passed **one test** (Windows runner **1.045 seconds**) covering curated ordering,
deduplication, quoting/escaping, exact fallback preservation and malformed input.
Prior-stage tests are not added to these counts.

Final Clippy passed in **22.208 seconds**; changed Rust files passed rustfmt in
**0.212 seconds**. The existing untouched whole-Windows formatting difference
in `src/state.rs:246` remains outside this change. No frontend source changed.
Native suites used 60-second outer Windows Jobs, 50-second work budgets and
bounded CLI/startup/condition waits. Host exit/output completion was checked;
all six Windows runner Jobs reported zero owned processes after cleanup.

All runtime verification is **hidden-only**, using exact owned HWNDs, isolated
settings/state and explicit instance pipes. Application IME guards exercised by
messages are distinct from physical Korean OS IME/TSF correctness. Acceptance
remains **62 partial / 52 pending**, U09 partial, with no feature marked complete.

## Images and artifact provenance

The current debug capture reuses production BUTTON/STATIC painting and requests
same-process native EDIT/LISTBOX client raster through `WM_PRINTCLIENT`. This is
an owned offscreen client image, **not title-bar, GPU/WebView or desktop capture**.
Older stages' BUTTON/STATIC-only PNG limits remain unchanged.

The reviewed [font picker PNG](font-picker-diagnostics/25ea5ceecace-font-picker.png)
is **444×381 client pixels**: the installed names, selection highlight and button
labels appear. **The search EDIT remains blank despite the paint request.**
The [Keybindings rows](font-picker-diagnostics/5f988ffd4d6f-keybindings-rows.png),
[edit](font-picker-diagnostics/40c86ade73fe-keybindings-edit.png) and
[capture](font-picker-diagnostics/6dcbe50267d2-keybindings-capture.png) images were
also reviewed; shortcut EDIT pixels are absent there as well. Their exact text,
search behavior and live geometry passed separate HWND checks. Capture did not
change the model, query or dialog state. The internal cause of missing hidden
EDIT pixels is unconfirmed; these images do not approve visible EDIT glyphs,
borders or IME candidate rendering.

Native records preserve their original capture-scope strings. After image review,
the verifier strings were corrected to describe a paint request, not proven EDIT
raster output; behavior was unchanged. Both verifier snapshots are retained.

Debug and static-CRT release Cargo invocations failed canonical GUI publication
with OS error 5 after linking, in **88.907 seconds** and **117.233 seconds**.
Those remain failed results. Fresh deps binaries were checked against build
times and hashes and separately staged; the running user's GUI was not stopped,
overwritten, moved or focused. The unit build passed in **83.832 seconds**.

All [three staged release entrypoints](native-font-picker-release-entrypoints.json)
passed doctor in **0.886 seconds**. NSIS packaging passed in **11.678 seconds**.
Installer: `windows/dist/flowmux-windows-0.10.1-dev-font-picker-x64-setup.exe`,
**6,020,784 bytes**, SHA-256
`2d2f5960392e5fed08d529d0a14171dc0c61bc1beb3a33a80b93817b620ac832`.
No installation was performed.

The [diagnostics manifest](font-picker-diagnostics/manifest.json) preserves
**52 artifacts (four PNGs), 13 process checks, four native/unit records and
14 binary hashes**, including the failed Cargo publication records. Final source snapshots cover
**146 Windows runtime files plus one read-only shared file**; separate snapshots
cover tested/current verifier sources, installer inputs and package documents.

Physical keyboard/mouse and focus recovery, Korean IME candidates/composition,
accessibility, high contrast, per-monitor DPI and composed visual acceptance
remain unverified. Full Linux Options settings, theme inheritance, Update and
other screen/feature gaps remain tracked in [UI_PARITY.md](../../UI_PARITY.md).
