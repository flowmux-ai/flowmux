<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# macOS terminal repaint flicker

Verified on macOS 27.0 (26A428), GTK 4.22.3, VTE 0.82.3 and Codex 0.160.0.
Baseline: `8868c86d`. All GUI probes used separate app bundles, bundle IDs,
processes, XDG config/data/state directories and per-process sockets under
`/tmp/fm-flicker`. The installed Flowmux process (PID 25627, started October 2
at 22:19:31) was neither closed nor restarted.

## Reproduction and diagnosis

In a fresh terminal, run `codex --no-alt-screen` and type without submitting.
At a 2560 × 1600 screenshot size, compare the unchanged welcome/help text above
the composer between successive window captures. Sending one character every
35 ms through that test pane's `send-keys` endpoint reproduced the problem:
most terminal text disappeared for individual frames, while part of the
composer and the surrounding app UI remained visible. The next frame restored
the text. A simpler alternating-row output fixture did not reproduce this.

| Same baseline binary, renderer setting | Captured frames | Frames losing unchanged welcome/help text |
| --- | ---: | ---: |
| Default (forced Cairo) | 120 | 2 |
| `GSK_RENDERER=gl` | 160 | 0 |
| Cairo with `GSK_DEBUG=full-redraw` | 160 | 0 |
| Patched binary, no renderer override | 300 | 0 |

The baseline failures are frames 11 and 109 in
`/tmp/fm-flicker/baseline/codex`; the static comparison rectangle was
`(564, 300, 2400, 700)`. Patched captures are in
`/tmp/fm-flicker/fixed/codex`. The temporary directory also contains the
launch/input/compare scripts, build/test logs and binary hashes. These are
local diagnostic artifacts, not portable repository fixtures.

The renderer-only A/B comparison and Cairo full-redraw control isolate the
reproduced artifact to macOS Cairo's partial repaint path. GTK's macOS Cairo
backend writes to IOSurface buffers and propagates damage to Core Animation
tiles ([upstream Cairo context](https://github.com/GNOME/gtk/blob/4.22.3/gdk/macos/gdkmacoscairocontext.c),
[upstream layer](https://github.com/GNOME/gtk/blob/4.22.3/gdk/macos/GdkMacosLayer.c)).
This investigation does not establish the exact faulty upstream instruction.
It does not claim to implement VTE synchronized output or fix every possible
PTY clear/redraw intermediate frame.

## Change and tradeoff

Default to `GSK_RENDERER=gl` on macOS before GTK initialization. Keep explicit
`GSK_RENDERER` choices intact and leave other platforms unchanged. The patched
startup logged `GskGLRenderer`; a separate patched process with an explicit
`GSK_RENDERER=cairo` logged `GskCairoRenderer`.

GL uses more memory. After the typing probe, `vmmap -summary` reported physical
footprints of 75.1 MiB for Cairo and 132.5 MiB for GL; observed process peaks
were 81.0 and 353.2 MiB respectively. These single-window observations are not
memory bounds. Cairo remains an explicit low-memory override, with the known
partial-repaint artifact. Forcing full-window Cairo redraws also suppressed
the artifact in this sample, but is a diagnostic setting rather than the
chosen default rendering path.

## Regression checks

- Patched app: 300 captured Codex frames retained unchanged text; another 50
  actual native typing/Backspace captures also retained it.
- Patched app: live terminal output, nine split-ratio changes, a new tab,
  return to the original Codex tab, and removal of the test split passed.
- Main-thread `macos_native` smoke with GL passed: WKWebView state retention,
  editor/terminal focus, saturated IPC, long socket paths, cancel close and
  save retry (`MACOS_NATIVE_SMOKE_OK`).
- GUI IPC tests: 54 passed. Bridge tests: 4 passed. Formatting and
  `git diff --check` passed. Clippy completed with existing warnings in
  `sessions.rs`, `editor_pane_macos.rs` and the daemon handler.
- Terminal, IPC, state and daemon test suites: 330 passed, 0 failed, 3 existing
  ignored tests. Configuration tests: 93 passed.
- The broader macOS suite is not entirely green: four PTY integration tests
  and one SSH bootstrap unit test failed. All five also failed when built
  from an untouched detached baseline checkout; the GUI renderer patch does
  not change their code. Do not count them as passing regression checks.

Baseline failures:

- `pty_tee_delivers_final_output_in_order_after_outer_eof`
- `pty_tee_outer_eof_kills_signal_ignoring_inner_group`
- `pty_tee_preserves_input_queued_before_startup`
- `pty_tee_restores_shared_stdout_flags`
- `ssh::tests::remote_bootstrap_preserves_literal_arguments_and_rejects_missing_cwd`

No installed application was replaced. The live verification used the patched
isolated bundle; the user's existing process retains its original renderer
until they choose to restart with an updated build.
