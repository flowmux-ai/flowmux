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
blocked before submission. That separate screen-input boundary issue was left
for the follow-up below; this change did not fix every draft classifier.

Detailed evidence remains in the isolated test directories and local test logs;
authentication material is excluded from repository evidence. This record does
not claim that the remaining state-evidence, notification-correlation, attention
UI, or program-status work is complete.

## Editable input and display metadata — 2026-10-30

Real-agent testing reproduced three distinct false input requests:

- An unsubmitted draft contained the words `permission prompt`.
- After interruption, an older prompt containing those words remained in the
  recent transcript.
- The model/cwd row included an automatically generated session title containing
  those words, even after an approved command and its response had completed.

Each case failed a regression test before its fix. Screen fallback now excludes
editable text only when a recognized composer frame surrounds it. The model/cwd
row does not contribute arbitrary title/path text to status detection; its
explicit goal badge is retained for existing compatibility. A native interruption
marker directly above a recognized composer also delimits historical text. This
boundary is not itself a new lifecycle completion event.

Approval menus retain their controls, and progress/request signals outside the
composer remain visible to detection. Existing native waits, child-work guards,
and new-turn ordering are unchanged. The daemon tests include draft and
interrupted frames while correlated work is still outstanding.

The final isolated application is PID 53463, under `/tmp/fm-atn-fx9y65bg`.
Actual native-hook agent A version 2.1.314 and agent B version 0.166.0 were used.
Both displayed multiline drafts containing approval, progress, and quota wording
without leaving idle. Actual questions and one-time command approval requests
still entered blocked; reading them retained attention, and answering resolved
them. The final application also kept the completed approval idle with the
generated session title still visible. Earlier intermediate candidates exposed
the interruption/title failures; their results are not treated as passing proof.

Candidate GUI SHA-256:
`9b44dffd458f329d54209a1909c7328747f1eb82e026f0a606dc838501a36f84`.

Candidate CLI SHA-256:
`bbdc45c120943b0a3694659bbd0be4e81a1a08e89b3556c394aee62cc78f79a7`.

Final interruption/recovery passed for both real agents. Agent B remained idle
with its interrupted outcome despite the old transcript and session title;
the next submitted prompt entered working and completed independently. Agent A's
generic early-interruption label remains the outcome limitation noted above.
The final rendered application showed the completed response without a false
input warning. The expanded macOS native replay passed with
`LIVE_NATIVE_HOOK_MATRIX_OK` and `MACOS_NATIVE_AGENT_HOOKS_OK`.

The agent unit runner passed, including 163 core and 188 daemon tests. Workspace
build, focused all-targets Clippy, formatting, and diff checks passed. Logs are
`/tmp/fm-draft-unit3.log`, `/tmp/fm-draft-native-final.log`, and
`/tmp/fm-draft-clippy3.log`.

The full Linux gate is in progress on the snapshot preceding the final title-row
refinement; it is not final-candidate release evidence. Its initial attempt
lacked Git metadata in the isolated source copy; a second attempt omitted the
existing application profile and failed to launch the WebKit sandbox. The next
run uses the same `aa-exec -p flowmux-webkit` invocation as CI, without changing
system policy or disabling the web-process sandbox. Final Linux coverage remains
pending.
