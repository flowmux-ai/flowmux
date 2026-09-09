<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent completion detection

Native hooks own session/turn activity, process inspection owns identity and
liveness, and terminal content changes provide fallback evidence. A live CLI
process is not evidence that its turn is still working. Silence is not evidence
of completion. No inactivity timeout is used to declare a turn finished.

## September 2026 regression

A live Codex pane showed its final response, `Worked for 6m 15s`, and its input
composer while Agents still showed Working. The session recorded `task_complete`;
the public presence's last hook sequence was also from the final-response period.
That does not establish which hook was delivered or accepted. Historical hook
delivery cannot be reconstructed from the public presence alone.

A controlled live PTY replay reproduced Working after TurnStarted and the exact
captured completion screen, with no observed spinner and no Stop. The existing
fallback required earlier spinner evidence. The regression test failed before
the change. A separate fixed app running that replay displayed Completed/Idle.
The original window and its Codex sessions remained running throughout.

An actual Codex smoke test ran `pwd` and replied `LIFECYCLE_DONE`; its native
completion also reached Idle. The original stuck entry was separately reconciled
once from its unchanged, verified completion screen, without restarting its app.
This manual correction is not evidence of hot-loading the code change.

The new fallback requires a completed-duration footer immediately above a
matching Codex/Claude composer. A bare prompt, arbitrary response text, and the
continued existence of a process are insufficient to override a hook's Working.
An unchanged old completion frame cannot finish a newly started turn. Explicit
completion/interruption no longer restores an older screen Working/Blocked base.
Variable progress labels are recognized by their decorated clock/control or
token-counter shape, including Claude's changing spinner verbs.

A second stuck Working came from the opposite direction. Codex's Stop hook was
delivered and settled to Idle after the ingress grace, but the TUI repainted its
last `Working (44s • esc to interrupt)` frame after the hook returned. That
stale spinner reopened Working on the hook-owned presence, and the final frame
could not close it: it had no completion footer, and `?? .claude/` from a git
status above the composer made the idle-name heuristic call the pane Claude, so
the frame was discarded as an identity conflict. A settled Codex turn now
ignores screen Working until a new native turn starts, and idle-name detection
prefers the line nearest the composer.

A separate running app replayed TurnStarted, native Stop, a repainted stale
spinner, the final prompt containing `?? .claude/`, and a second turn. Both the
public presence and the visible Agents entry stayed Idle/Completed after Stop
and returned to Working on the second turn. Core/daemon checks passed 320 tests
(3 ignored), format checks, and Clippy. The verified fast-profile binaries were
installed; existing windows need a restart to load the change.

## Case coverage

| Case | Evidence and behavior | Verification |
| --- | --- | --- |
| Plain text, Markdown, code blocks, tables, Korean, tool output, images | Response body is irrelevant to native Stop; no answer keyword declares completion | CLI payload tests; captured-screen replay |
| TUI, noninteractive text, JSON/streaming output | Hooks remain primary; JSON response text is not parsed as TUI activity | Existing hook parser/lifecycle tests; no claim of testing every CLI mode live |
| Native Stop, repeated Stop, blocked Stop retry | Correlated session/turn settlement; retries do not duplicate completion | Daemon lifecycle tests |
| Missing Stop / spinner missed | Matching completion footer plus composer recovers Idle/Done | New core/daemon tests and live Codex screen replay |
| Bare/empty composer while working | Progress takes precedence; a prompt alone cannot clear native Working | Core tests |
| Variable progress verb / compaction / Stop-hook progress | Decorated duration and live controls preserve Working | Core tests |
| New turn before repaint | Unchanged completion frame cannot finish the new turn | Daemon regression |
| Stale spinner after Stop settlement | Settled Codex turn ignores screen Working until a new native turn; idle name follows the line nearest the composer | Daemon/core regression |
| Permission, AskUserQuestion, API/quota/session waits | Correlated waits remain Blocked despite completion-looking output | Existing and new daemon tests |
| Parallel tools, reordered events | Tool identities, scopes, and boundary sequences retain independent waits | Existing daemon/CLI tests |
| Active/reused child, child/root Stop race | Observed child ledger defers root completion; screen Idle cannot clear active children | Existing and new daemon tests |
| User interrupt | Codex Interrupt and Claude's explicit interruption fallback; process remains present | Existing hook/core/daemon tests |
| SessionEnd, crash/dead PID, resume/replacement | Teardown tombstones prevent stale reports/screens from resurrecting an agent | Existing daemon tests |
| Foreground/background tab | Acknowledged completion is Idle; unseen completion is Done; identity/session stay intact | Core/daemon visibility tests |

## Limits and sources

This is not a guarantee for every future output format. Hooks can be untrusted,
missing, delayed, or blocked by other handlers. A terminal snapshot can contain
copied UI text and cannot prove semantic completion. Unknown/missing footers,
narrow or heavily customized layouts, and unobserved/reused child activity still
depend on native events and the existing fallback. An observed child ledger is
not complete ground truth, so a missing child-stop event can keep work pending.
The change does not read entire transcripts in production or modify hook trust.

Codex goal mode can leave a `Conversation recap` above the composer and
`Goal achieved (16m)` in the model/cwd row below it. That adjacent status row
now qualifies as completion evidence, with the same duration validation,
live-progress precedence, and native wait/child guards as the other footers.
Recap prose alone never declares completion. Core/daemon regression cases and
a live PTY replay of the captured screen verified Working → Idle without Stop;
starting another turn against the unchanged screen retained Working. The
original stale session was separately reconciled from its verified live PID,
session ID, and completed turn; the running app did not hot-load this change.

- [Codex hooks](https://developers.openai.com/codex/hooks): native stdin JSON,
  turn identities, concurrent handlers, trust, Stop continuation, Interrupt;
  transcript format is explicitly not stable.
- [Claude hooks](https://code.claude.com/docs/en/hooks): Stop vs user interrupt,
  StopFailure, parallel tool and permission events, background work.
- [Codex status row source](https://github.com/openai/codex/blob/main/codex-rs/tui/src/status_indicator_widget.rs):
  variable header, elapsed clock, interrupt hint, background-process context.

Checks: `cargo test -p flowmux-core -p flowmux-daemon -p flowmux-cli`,
`cargo clippy -p flowmux-core -p flowmux-daemon --all-targets -- -D warnings`,
`cargo build --profile fast -p flowmux -p flowmux-cli`, and isolated live app replay.
The run passed 564 tests (9 ignored), format/diff checks, and Clippy. The GUI build
retained the existing unused `workspace_root` warning in `editor_pane_macos.rs`.
