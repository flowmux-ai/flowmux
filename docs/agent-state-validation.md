<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent state validation

## Pending attention and completed-turn history — 2026-10-30

The change retains warning presentation for a read-but-unresolved input request.
Completion acknowledgement still clears result attention. Timestamped completed
turn footers delimit historical approval text so an already resolved request
does not become blocked again. Goal badges remain compatible with live controls.

Three failures were reproduced before their fixes:

- A blocked pane lost workspace attention after acknowledgement.
- A real approved command completed, then its transcript's earlier permission
  text changed the state back to blocked during screen polling.
- Editing a new draft changed the old completion fingerprint, allowing a new
  starting turn to settle against the previous completed frame.

Automated coverage includes hidden/visible requests, another completed tab,
request resumption/removal, completion acknowledgement, timestamp validation,
live controls following completion, stale completion after a new prompt,
correlated waits, and active children. The native hook replay includes the
completed approval frame in both hidden and focused workspaces.

Actual-agent verification used a separately built application, isolated data,
configuration, state, and socket directories. No existing application or agent
was closed or restarted. Original application PID 72789 remained alive. Test
application PIDs were 86793, 98980, and 39828. The final application included the
draft fingerprint fix and used the candidate hashes below.

| Provider | Version | Observed scenarios across validation passes |
| --- | --- | --- |
| Native-hook agent A | 2.1.312 initially; 2.1.314 on final candidate | Plain response, shell tool, question wait, focus/read while blocked, answer and completion, interruption, subsequent prompt |
| Native-hook agent B | 0.166.0 | Plain response, shell tool, explicit approval wait, focus/read while blocked, approval and completion, subsequent prompt, interruption and recovery |

The final candidate retained blocked state while input was outstanding. After
approval it stayed idle/done rather than reopening the resolved wait; a new
prompt entered working and completed independently. The actual window's warning
icon and side-panel text were inspected while a request was read but unresolved.
These observations are distinct from synthetic hook replay.

Both final-candidate agents also accepted a fresh prompt after interruption and
completed it. Agent B reported an interrupted outcome. Agent A returned to idle
but retained a generic completed label after early interruption; distinct
cancellation reporting remains part of the subsequent outcome work.

Candidate GUI SHA-256:
`faf35689ed22bd15b6569394924425f09c43aa359cc87dd1ff7271b2d24318e7`.

Candidate CLI SHA-256:
`e305bed4ead6ed272f807c19bd44082f33f81d94d17c34e6a6ebb2363aa26322`.

Verification status:

- Core suite: 161 unit tests and the normal pane-tree random walk passed; the
  opt-in deep-chain stress probe was not run.
- Shared status styling test passed.
- Workspace build and focused all-targets Clippy passed.
- Formatting and diff checks passed.
- The expanded native lifecycle matrix passed in the final shared macOS gate.
  All 45 shared phases passed, with `CI_GATE_EXIT=0` in
  `target/ci/macos/run.log`.
- The first shared macOS gate stopped at a browser native-pointer timeout while
  other GUI verification was occurring. That run is a failure, not passing
  evidence. The subsequent rerun passed all phases; the first failure remains
  recorded rather than being treated as passing evidence.
- Linux agent unit runner passed; five agent-bar tests passed under an isolated
  virtual display. The full Linux coverage gate has not yet been run.

One installed native-hook agent also reported an Interrupt hook timeout clamp from
five to three seconds. That installer compatibility adjustment is a separate
change, with its own configuration and real interruption checks.

A fresh session also briefly classified draft text mentioning approval as
blocked before submission. That separate screen-input boundary issue remains
for the next change; this change does not claim to fix every draft classifier.

Detailed evidence remains in the isolated test directories and local test logs;
authentication material is excluded from repository evidence. This record does
not claim that the remaining state-evidence, notification-correlation, attention
UI, or program-status work is complete.
