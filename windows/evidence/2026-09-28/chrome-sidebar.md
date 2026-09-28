<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Sidebar and Hangul caption rendering

This stage is a validated foundation for the structural UI work described in
[UI_PARITY.md](../../UI_PARITY.md). It does not establish Linux screen or feature
parity. The separately running user application was not changed or installed.

The sidebar has a 36-DIP icon footer, a header notification bell, actual native
tooltips, and a 160–640-DIP preferred width. Resize/clamping preserves that
preference; a click without movement cannot accidentally save the clamped width.
Old state loads with the default 260-DIP width. Old binaries reject a saved
nondefault-width field, so downgrade checkpoint compatibility is limited.

Caption drawing normalizes only a temporary copy with Win32 NormalizationC.
The model, HWND caption, input, paths and saved text retain original codepoints.
Memory-only GDI/Uniscribe probes with Segoe UI and Malgun Gothic showed separate
raw decomposed jamo; a normalized drawing copy produced equal NFC/NFD pixels.
The actual hidden host also produces identical full native-chrome PNG hashes for
NFC/NFD workspace and tab captions. Compatibility jamo remain visually distinct.
This is caption rendering evidence, not physical IME composition acceptance.

Final debug build passed in 82.542 seconds and final Clippy in 24.674 seconds.
The focused state compatibility test passed (48.738 seconds including compilation).
Final hidden native cases passed:

| Case | Checks | Runner duration |
|---|---:|---:|
| Details, tooltips, NFC/NFD, themes and stable terminal identities | 5 | 11.785 s |
| Overflow pager and bounds | 1 | 6 s |
| Resize, cancellation, clamp/no-motion and restart | 2 | 7.494 s |
| Workspace regression | 6 | 16.205 s |

All runs used owned hidden windows, isolated state and bounded CLI requests.
No desktop input, clipboard, foreground focus, visible test window or user-app
replacement was used. Native PNGs call production drawing on an offscreen DIB;
they omit WebView/GPU pixels. The exercised monitor was 96 DPI. Physical pointer,
keyboard/IME, activation, high contrast, accessibility and DPI transitions remain
unverified. No release installer was produced for this small foundation stage;
packaging is batched with the following structural UI changes.

Failures are preserved in the [diagnostic manifest](chrome-sidebar-diagnostics/manifest.json).
An early compiler run required a closure type; Clippy then found duplicate
branches. Both were fixed. Initial tooltip checks failed because the v3
TTTOOLINFO size was rejected by the unmanifested common-controls v5 host; a native
ABI probe confirmed that the v2 size accepts and retrieves the same Korean text.
The size was corrected, followed by a fresh debug build and Clippy. A later
details verifier incorrectly expected an unchanged Settings label after a theme
update; it now resolves the control by its action identity, and all five checks
pass. Collection initially needed CP949 decoding for an old console log; a
subsequent array-shaped probe was correctly collected as an artifact rather than
a native-suite result. Neither collection issue altered product data.

Source snapshots distinguish the original and tooltip-fixed runtime, then the
final verifier correction. Subsequent structural UI edits are outside this
stage. Historical chrome-details evidence and its installer remain unchanged.
