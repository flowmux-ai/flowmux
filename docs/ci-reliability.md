<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# CI reliability

CI must fail on regressions. The goal is reproducible failures and reliable
oracles, not a green result obtained by retries, ignored assertions, longer
blanket timeouts, or lower coverage floors.

## Reproduced failures

The Test run `37207522195` exposed two different problems:

- Linux: editing stacked comments near line 20,000 could leave the composer
  outside the viewport. TextView validates line heights incrementally. The
  previous code overwrote GTK's scroll adjustment and treated an unchanged
  (temporarily clamped) value as completion, before the scroll extent was ready.
  `scroll_to_cursor` now lets GTK validate/scroll to the mark and completes only
  when the allocated card is actually visible. A newer render cancels the old
  request through the existing generation counter.
- macOS: the multi-pane handoff test compared the source's complete terminal
  screen before/after resizing. Bash redrew its long host prompt as the pane
  resized, so correct input routing looked like a changed input buffer. The
  source now remains a live raw-mode receiver. The assertion compares actual
  PTY bytes, including after the receiver exits, while the destination still
  verifies bracketed paste, submit, review content, scope, and focus.

The scroll regression test holds the adjustment range at its viewport size
through the initial allocation frames, then releases it. This deterministically
models delayed range validation; the old implementation times out with the card
still hundreds of thousands of pixels below the viewport. Both the first and
last stacked comment must become fully visible with the fix. This runs in the
Linux coverage test and the macOS main-thread native smoke.

Meta-review also reproduced a near-full-height composer ending below the viewport:
the default 5% scroll margin left a 583px card at y=38 in a 607px viewport. Such
cards now center without that margin; cards taller than the viewport align at
their top. The geometry test requires the requested height to be allocated and
position/range to stabilize, so a briefly visible animation frame cannot pass.

PTY receipts are published atomically and waits require the full paste/submit
terminator. File existence alone is not a receipt-completion signal.

The macOS run `37272406730` timed out waiting for the second (commit-review)
PTY receipt; the same scenario passed locally, so the log alone does not prove
its cause. The fixture now requires a launch-specific marker immediately above
the raw-mode prompt, waits for the shell to acknowledge the prior receiver's
exit, and uses separate worktree/commit receipt files. This prevents a stale
prompt or receipt from acknowledging a new receiver. Commit-handoff timeouts
include the received bytes, focus, review status, and terminal screen. Timeouts
and the actual paste/content/focus assertions remain unchanged. A native
regression renders the prior receiver's empty prompt and requires the next
launch to reject it; reverting to the old prompt-only check fails that assertion.

Run `37309769011` passed Linux but the macOS handoff fixture timed out waiting
for `REVIEW_DONE:source`. Its captured terminal showed the receiver exit and
shell prompt, with the expected marker absent. Exit acknowledgement now uses
a phase-specific file written by the shell after Python exits and restores
termios. It does not depend on terminal text surviving a redraw. The native
regression clears the terminal after exit and checks acknowledgement again;
the previous screen-based wait fails this case. Paste bytes, destination,
session, review scope, and the 10-second exit deadline remain checked.

The remaining macOS timeout was traced in run `37313952560`: workspace model
creation took less than a millisecond, but its GTK command waited 12.56 seconds.
A process sample captured all 679 main-thread samples in GSK rendering, mostly
Apple's software OpenGL shader compiler (`GLRendererFloat`). The macOS gate now
uses Cairo for native UI/IPC tests and logs the renderer, overriding inherited
GPU settings. Direct native harness runs still allow GL checks. The application
renderer, RPC deadlines, native UI assertions, and coverage floors are unchanged.

## One execution path

`.github/workflows/test.yml` invokes `scripts/test-ci.sh linux|macos`, exactly as
a developer can locally. The runner owns private XDG/runtime/temp directories,
clears inherited Flowmux pane/socket identity, keeps fatal GTK criticals, and
records toolchain/native-library versions and the complete output. It does not
restart the installed app or disable WebKit sandboxing.

`rust-toolchain.toml` pins Rust; the Test workflow reads that file through rustup.
Ubuntu is pinned to 24.04 with an explicit C.UTF-8 test locale, and the native
runner remains macOS 15. Updating the
compiler/OS is an explicit change to validate on both platforms. Homebrew/apt
package updates and hosted-runner hardware can still vary; logs record relevant
versions rather than pretending these environments are bit-for-bit identical.

The Linux gate runs instrumented workspace tests (all test executables finish
even if one fails, so later failures are not hidden) and the live SSH fixture,
exports JSON/HTML coverage even after a test failure, and enforces the existing
79% line / 78% region / 78% function floors. Coverage environment setup must
succeed before any test starts; a failed `show-env` cannot silently fall through
to uninstrumented tests. The macOS gate builds the workspace, checks IPC/bridge
contracts, and requires the main-thread native suite's completion marker.

`scripts/test-ci-runner.py` verifies failure-code propagation through log pipes,
no retry of failed suites, state isolation/cleanup, report export after failure,
and rejection of failed coverage setup. These checks run in both gates.

Evidence is uploaded even on failure from `target/ci/{linux,macos}` and Linux
`target/llvm-cov`. Do not describe a local uninstrumented pass as CI verification;
run the shared gate, push, then check both Test and Sanitizers for that exact SHA.

For a new timing-sensitive regression: capture the failed state, reproduce the
same state transition with a controlled fixture, prove failure before the fix,
and verify the actual mapped UI/PTY outcome. Fixed sleeps and whole-screen
snapshots are not substitutes for the condition being tested.

## Validation of this change

- The delayed-range regression failed before the scroll fix and passed after it;
  five additional Linux runs under LLVM instrumentation passed consecutively.
- The shared macOS gate passed: workspace build, 54 IPC tests, 4 bridge tests,
  and the complete isolated native suite, including comment geometry and actual
  multi-pane PTY handoff for working-tree and commit reviews.
- Runner contract tests, shell syntax, Rust formatting, and workflow actionlint
  passed. Coverage thresholds and GTK fatal-critical handling remain enabled.

The Linux browser integration fixture now prints the failed IPC request and the
child GUI log before its temporary directory is removed. This was verified with
a denied nested sandbox (failure included the bubblewrap/dbus-proxy cause), then
with namespace/mount support enabled (both sandbox-on and opt-out tests passed).
Local Docker validation needs an init/reaper (`docker run --init`), nested
namespace support, unmasked `/proc` mounts, and a Bash login shell for the
SSH fixture account (its input test exercises readline); the
hosted Ubuntu job uses the packaged AppArmor profile directly. Do not disable or
skip the sandbox assertion to make a restricted container pass.

The follow-up run `37283968929` confirmed that the receiver was ready but Send
rejected its session as ended/restarted. The fixture injected `AgentPresence`
without a source: one bare-prompt refresh claimed it as `flowmux:screen`, and
a subsequent refresh with no agent name removed it. Explicitly refreshing the
screen twice reproduces the session loss locally. Receiver identities now use
`flowmux:hook`, matching the authoritative sessions they simulate; the native
scenario requires the same session to survive those refreshes before Send.
Production agent-lifecycle and stale-session rejection rules are unchanged.
