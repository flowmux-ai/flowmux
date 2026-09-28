<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Options automatic application and scrolling

Linux Options applies supported value changes immediately and keeps its footer
outside a scrolling General page. Windows now removes per-row Apply buttons,
uses native Terminal/Minimap/Shell groups in a scroll viewport, and preserves
the owned nonmodal General/Theme window. Text fields save after 250 ms; combo
selections save immediately through the existing worker. This is structural
progress, not complete Linux Options or visual parity.

At most ten rows hold one latest draft each, with one submitted save at a time.
Inputs stay enabled during ordinary saves. A late successful reply advances its
comparison baseline without replacing newer typing. Invalid or conflicting
drafts remain visible; invalid numeric drafts never become a disk/config error.
Composition guards defer saves and prevent programmatic replacement. Hidden
composition-end messages retain the latest committed text across close/reopen.
The complete default shell (program and arguments) is compared under the
existing file writer lock. Old CLI shell requests keep their original wire
shape and unconditional-update behavior.

## Verification

The hidden native workflow passed **nine groups in 8.673 seconds** (9.569 seconds
including the outer runner): owned window/viewport/footer geometry and scrolling,
raw Unicode automatic application, invalid intermediate values and repair,
same-row/multiple-row bursts, composition guard with unrelated external change,
composition close/hidden end/reopen, same-field conflict and Reload, arguments-only
external shell conflict, and Theme/Reset/close/reopen. Terminal acknowledgments,
active surface and terminal PIDs were checked. All observed settings used the
isolated real persistent config path; the resulting config file is retained.
External updates in this scenario use the same owned host's CLI, not a second
window. The existing writer-lock tests cover separate Store instances.

The native store test verifies successful merging, arguments-only stale shell
rejection, exact file-byte preservation, held locks, denied replacement and
corrupt-file behavior. Eight CLI tests include the unchanged legacy shell wire
shape. These **nine unit tests** passed through the checked-in counted runner
in 0.952 seconds (0.07 seconds store and 0.06 seconds CLI test execution).
An earlier correctly counted run also passed; it is not counted twice.

Final Windows Clippy with warnings denied passed in 25.715 seconds. Debug unit
linking passed in 80.606 seconds. PowerShell parsing and fixture compilation
passed in 0.974 seconds. The ConPTY test runtime checksum check passed in
0.307 seconds. Formatting and whitespace checks were also run.

## Corrections and evidence limits

Read-only review caught hidden composition-end draft loss and premature shell
baseline advancement after external arguments-only changes. Both were fixed
before the runtime source freeze and exercised in the native workflow. Static
verifier review also corrected an out-of-range minimap width from 140 to 64
before execution.

Two initial unit invocations passed a filter and harness option as one argv
entry. Both processes exited zero after running **zero tests**; neither is test
acceptance. Their logs remain preserved. The new `verify-options-unit.ps1`
passes separate argv entries and requires the expected nonzero counts, then
records the actual executable hash and output.

WM_IME_START/END messages target only exact owned hidden EDIT HWNDs. They prove
application guard behavior, not real Korean OS IME/TSF, candidate positioning,
keyboard focus, accessibility, per-monitor DPI or composed desktop/GPU pixels.
Full font pickers, General/Theme options, Keybindings and Update pages remain
incomplete. No feature was promoted to complete: 61 partial / 53 pending remain.

## Build provenance

Debug GUI linking took 82.336 seconds and static-CRT release linking took
113.233 seconds. Both Cargo invocations failed publishing the canonical GUI
because the user's running app holds it open (OS error 5). Those failures remain
failed. Fresh linked deps entrypoints were checked against the build interval,
hashed and separately staged. Hidden UI tests used `options-live-debug`;
unit tests used the separately staged debug test executable. Release GUI/CLI/
console doctor entrypoints passed in 0.819 seconds.

Source snapshots, staging records, all check logs, native observations and
binary hashes are preserved in [the diagnostics manifest](options-live-diagnostics/manifest.json).
The running user application was not replaced, installed over, moved or focused.

NSIS packaging passed in 12.570 seconds. Installer:
`windows/dist/flowmux-windows-0.10.1-dev-options-live-x64-setup.exe`
(5,927,866 bytes), SHA-256
`d9ad25b1387439ebffd4b60223fca2a20ed45063c6ab3c5a25b910ac1051bba6`. The installer was not executed.

The manifest is frozen with 43 preserved artifacts, 13 process-check
records, one native UI record and 8 binary hashes. All preserved/original
artifact hashes, 140 runtime source hashes and five verifier source hashes were
verified. All 7 Windows runner Jobs reported zero owned processes after cleanup.
The two zero-test process results are explicitly excluded from acceptance.
