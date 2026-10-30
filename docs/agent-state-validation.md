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
five to three seconds. That installer compatibility adjustment is covered in the
separate validation section below.

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

## Native interruption hook deadline — 2026-10-30

The installed native-hook agent B version 0.166.0 displayed a startup warning
that its Interrupt timeout was clamped to three seconds. Flowmux had installed
five seconds. The installer now emits three seconds, and the existing drift
comparison recognizes and repairs the old entry.

A failing test first demonstrated that the old entry was incorrectly considered
current. The repaired entry retains an adjacent user handler and its position;
repeating installation does not change the result. The actual installer was then
run twice against a private test configuration. Only its managed hook changed;
the user handler and configuration file were preserved.

A new real-agent session in isolated application PID 53463 started without the
timeout warning. A long response entered working, Escape produced the native
interrupted outcome, and the preserved user handler wrote its private test
marker. A subsequent prompt completed normally. Existing application sessions
were not restarted. The original application PID 72789 remained alive.

Verified CLI SHA-256:
`590844d3edfc70ea3afa2df93e9550c63852d8a7a987a8facec3b67d80958541`.

The full agent unit runner, focused all-targets CLI Clippy, CLI build, formatting,
and diff checks passed. Evidence logs are `/tmp/fm-hook-timeout-unit.log` and
`/tmp/fm-hook-timeout-clippy.log`. The private hook configuration and marker are
under `/tmp/fm-atn-fx9y65bg`; no authentication material is included here.

## Status evidence separate from identity — 2026-10-30

Live state now carries optional `status_evidence` with its source and `since`
timestamp. Identity ownership retains its existing source. A screen fallback
therefore reports screen evidence without taking ownership from a hook or live
process. Process-only detection explicitly leaves activity unconfirmed.

The timestamp describes when the current state basis was established; it is not
a heartbeat, last poll, turn identifier, or proof of successful work. Identical
polling and same-state screen refinements retain native evidence. A restored
pending request recovers its original evidence, while a newly screen-detected
completion cannot borrow an earlier idle hook. Missing fields remain compatible
with old peers; an unknown future source is displayed as unavailable evidence.

Both live Agent lists use the same evidence description in their tooltips.
`flowmux --json agents` exposes `identity_source` and `status_evidence`; tree JSON
retains its existing ownership `source` and adds the optional evidence object.

Two regression tests failed before implementation because the evidence contract
was absent. Coverage now checks stale reports, ownership reclamation, repeated
polls, misleading screen identity, pending-request restoration, screen completion,
legacy JSON, and propagation through the live UI model and CLI projection.

Actual-agent verification used application PID 66246 in
`/tmp/fm-atn-xl9d6bnb`, with separate configuration, state, and socket directories.
The original application PID 72789 remained running throughout.

| Scenario | Observed result |
| --- | --- |
| Native-hook agent A 2.1.314, plain response and question | Hook-backed Working, Blocked, resumed Working, and Idle/Done |
| Native-hook agent B 0.166.0, shell command and one-time approval | Hook-backed Working/Blocked; reading retained the visible warning; approval completed without reopening the wait |
| Agent A, immediate interruption | Hook ownership retained; screen evidence explained the fallback to Idle/Done |
| Agent B, native interruption | Hook evidence and the interrupted status remained visible |
| Both agents, recovery prompt | A fresh Working state followed by a completed response |
| Both agents, unsubmitted request/progress/quota wording | Idle/Done and evidence timestamp remained unchanged |
| Agent B before native hooks started | Process evidence explicitly identified unconfirmed activity |

Agent A's early-interruption label still says Completed. The evidence change
makes its screen basis inspectable but does not add cancellation inference.
Question/permission labels and typed outcomes remain subsequent work. A background
shell may outlive a response; response completion does not claim all child work
or verification succeeded.

Verified application SHA-256:
`8a4a29ebc976969eae936dfcfb062785d728824fa3240927045b48d03e1cb2ac`.

Verified CLI SHA-256:
`737fc3c4f0f479a54201206849ef2e9c59eb317aab205d65298c0cf2274d2c9a`.

The agent unit runner passed, including 166 core and 188 daemon tests. Focused
all-targets Clippy, build, formatting, and diff checks passed. The expanded native
replay passed with hook-versus-screen evidence assertions in visible and hidden
workspaces. Logs: `/tmp/fm-evidence-unit.log`, `/tmp/fm-evidence-clippy.log`,
`/tmp/fm-evidence-native.log`. A subsequent native replay also observed both
evidence descriptions attached to live GTK rows and passed with
`MACOS_AGENT_EVIDENCE_TOOLTIPS_OK` in `/tmp/fm-evidence-agent-native.log`.

The first full macOS gate failed in the code-review history fixture when Git
could not create a temporary commit-object file (`Invalid argument`). An isolated
110-commit probe and the repeated full native scenario passed. A review-only
diagnostic attempt instead timed out at its earlier footer-overflow check and
did not reproduce the Git operation. Neither failed run is passing evidence;
the original Git error remains unexplained. The subsequent full macOS gate passed
all 45 phases with `CI_GATE_EXIT=0` in `/tmp/fm-evidence-macos-ci2.log`, including
the evidence-tooltip marker. IPC tests and native-smoke Clippy also passed in
`/tmp/fm-evidence-ipc.log` and `/tmp/fm-evidence-native-clippy.log`.

The preceding Linux snapshot gate failed while generating coverage reports
because its VM ran out of disk space (`CI_GATE_EXIT=1`). It is not passing
coverage evidence. Rebuildable sanitizer artifacts were cleared, and the next
coverage run removed its previous raw profiles through the normal coverage
cleanup. The VM then had 26 GiB free. The final snapshot for the preceding three
commits is running in `/tmp/fm-attention-final-63e4fb42`; it does not include this
evidence change. The Linux agent unit runner for this change passed (166 core
and 188 daemon tests), as did five Agent Bar tests and the live activity-model
projection test. Full Linux coverage for this change remains pending. Logs are
`/tmp/fm-evidence-linux-unit.log`, `/tmp/fm-evidence-linux-bar.log`, and
`/tmp/fm-evidence-linux-activity.log`.


## Typed pending waits and partial resolution — 2026-10-30

Pending wait records now extend the existing tool-ID, permission-scope, and
session-scope ledgers. Resolved tool IDs remain tombstones; sequence and root/child
boundaries retain their existing authority. A partial resolution publishes the
remaining reasons even when the public state stays Blocked. It does not emit a
completion notification or restart the status-evidence timestamp.

The read-only projection groups records by scope and reason, reporting a count
and the earliest still-pending start time. Tool counts refer to distinct tool
IDs. Permission/session counts refer to scopes, not an exact number of prompts.
One question can appear in both tool and permission scopes; those counts must
not be added together as a total request count. Generic notifications preserve
an already known scope reason and start time.

Question, approval, usage-limit, and API-failure events receive typed reasons.
Unknown or legacy events retain a fallback; no authentication or automatic-retry
state is inferred from missing data. The data model can represent authentication
when a future adapter has explicit evidence. Mixed reasons display Multiple
waits. Both live Agent lists share scope/reason/count details in their tooltips;
tree JSON and `flowmux --json agents` expose `pending_waits` and the derived
`status_text` while preserving raw `custom_status` for existing consumers.

The new regression failed against the parent commit because pending waits were
not inspectable (`/tmp/fm-wait-reasons-baseline-red.log`). The final isolated agent
unit runner passed 166 core and 190 daemon tests, including parallel partial
resolution, replay suppression, duplicate age/reason preservation, read state,
screen restoration, checkpoint rollback, and child ordering for multiple reasons.
IPC tests cover absent and future reason values. Full all-targets Clippy with the
native-smoke feature passed. The final test changes also passed the isolated
runner and Clippy. Logs: `/tmp/fm-waits-unit-last.log`,
`/tmp/fm-waits-ipc.log`, `/tmp/fm-waits-clippy-final.log`.

The expanded native hook replay passed with typed question/approval/quota
assertions, parallel request counts, and clearing after resolution. It also
verified evidence descriptions attached to live GTK rows. Log:
`/tmp/fm-waits-native.log` (`LIVE_NATIVE_HOOK_MATRIX_OK`,
`MACOS_AGENT_EVIDENCE_TOOLTIPS_OK`, `MACOS_NATIVE_AGENT_HOOKS_OK`). Quota coverage
is synthetic; no real rate limit was induced. The later Linux replay passed
the added API-failure and plan-approval assertions, both provider matrices,
hidden/focused completion, interrupted drafts, and stale/ambiguous Stop cases:
`/tmp/fm-waits-linux-replay.log` (`LINUX_TYPED_WAITS_REPLAY_OK`). API failures
were synthetic; no real API outage was induced.

Actual-agent verification used application PID 61046 in
`/tmp/fm-atn-rf8dbxa2`, separate Flowmux state/socket directories, private hook
settings, and dedicated terminal sessions. Native-hook agent B also used a
private agent home. The original application PID 72789 remained running.

| Scenario | Observed result |
| --- | --- |
| Native-hook agent A 2.1.314, question | Waiting for answer in both visible lists; one tool question and one permission question scope |
| Native-hook agent B 0.166.0, command approval | Waiting for approval; one permission scope; reading retained the warning |
| Answer/one-time approval | Working then Idle/Done; pending waits cleared |
| Both agents, plain response and foreground shell | Hook-backed Working and completed response |
| Agent A, immediate interruption | Hook identity retained with screen-backed Idle/Done; no old wait reopened |
| Agent B, native interruption | Turn interrupted with hook evidence and no pending wait |
| Both agents, recovery | Fresh Working followed by the expected response marker |
| Both agents, unsubmitted approval/quota/progress/interruption wording | Idle/Done, evidence timestamp, and empty pending waits remained unchanged |

Actual JSON evidence and draft observations are in `pending-waits.json`,
`draft-evidence.json`, and the two state logs under the isolated root. Visual
inspection confirmed the distinct question/approval labels and read warnings.
The question's two scopes are evidence of the same native interaction, not two
user questions. Agent A's immediate-interruption label still says Completed;
explicit outcome semantics remain subsequent work.

Verified application SHA-256:
`4e2fa80feeba631cdc9ac00c0e9f6c8724ac418977ba74728ec2ae4d4c2564b9`.

Verified CLI SHA-256:
`30158bc033cb90aaf9909e92534bf4606859b9b8c579903c1c646393816d15c8`.

The Linux agent unit runner, five Agent Bar tests, and the activity projection
test passed. Logs: `/tmp/fm-waits-linux-unit.log` and
`/tmp/fm-waits-linux-ui-result.log`.

Full macOS verification failed in native browser pointer delivery in
`/tmp/fm-waits-macos-ci.log`. A focused repeat and the previous verified binary
both reproduced pointer-completion timeouts. Read-only failure capture observed
a hidden, unfocused document and no delivered canvas events in
`/tmp/fm-waits-browser-diagnostic.log`. This narrows the failure to the native
visibility/input path; it does not establish its cause or a product fix. The
temporary diagnostic logging was removed, and the canonical binary rebuilt.
Full macOS verification remains outstanding while foreground availability is
being coordinated; no failed run is counted as passing.

The earlier three-commit Linux gate finished with `CI_GATE_EXIT=1` after the
editor I/O GUI scenario timed out waiting for `surface_focus`; coverage thresholds
passed, but the gate did not. Its reports were archived before the next run.
The evidence-only Linux gate also finished with `CI_GATE_EXIT=1`. Editor I/O
passed there, but the remote review Send fixture failed: its long placeholder
wrapped, leaving the cursor on a continuation row. The existing input guard
correctly refused to overwrite that state. A separate test correction draws a
bare empty prompt and retains the draft/busy/blocked rejection cases. The same
binary then passed all remote Send scenarios in
`/tmp/fm-waits-ssh-send-recheck.log`; product input protection was unchanged.
Full Linux validation of this pending-wait change is still outstanding.

## Response outcome separate from activity — 2026-10-30

The current response carries optional `turn_outcome`: completed, failed,
canceled, or unknown. A normal native Stop records response completion; the
native failure hook records failure without discarding its unresolved wait.
The explicit interruption boundary records cancellation only when the existing
root/child settlement permits it. The field does not assert successful work,
passing tests, child-process termination, or session teardown.

Screen-observed response endings report an unknown outcome unless the existing
interruption detector identifies an explicit interruption. They cannot borrow
the previous response's result. Same-state screen refinements preserve a native
outcome, temporary screen Working restores a pending failure, and a fresh turn
clears the old result. Stale boundaries still pass through the existing sequence
and ownership checks. Legacy peers can omit the field; future values deserialize
as unknown. Live labels use Response completed, Turn interrupted, or Response
ended; outcome unconfirmed. Unread response boundaries use a neutral stop marker
instead of a success checkmark. Activity history retains its existing detailed
completion summary while the live lists show the typed outcome label.

The new outcome regression failed against the preceding implementation with a
missing completed value (`/tmp/fm-outcomes-red.log`). The isolated agent runner
then passed 167 core and 191 daemon tests, including stale boundaries, new turns,
read state, screen restoration, and preservation of native cancellation. IPC
tests passed, including legacy/future outcome values. Full-workspace all-targets
Clippy with native-smoke passed. Ten Linux activity tests passed, including typed
completion projection. The Linux agent runner also passed, and final UI changes
passed focused Clippy and the ten activity tests. Logs: `/tmp/fm-outcomes-unit2.log`,
`/tmp/fm-outcomes-ipc.log`, `/tmp/fm-outcomes-clippy.log`, and
`/tmp/fm-outcomes-linux-unit.log`, `/tmp/fm-outcomes-ui-clippy-history.log`,
`/tmp/fm-outcomes-linux-history.log`. History tests also ensure typed question
labels remain distinct while completed-response details are preserved.

The expanded macOS native replay passed both provider matrices with explicit
outcome assertions and the live GTK evidence-tooltip check:
`/tmp/fm-outcomes-native.log` (`LIVE_NATIVE_HOOK_MATRIX_OK`,
`MACOS_AGENT_EVIDENCE_TOOLTIPS_OK`, `MACOS_NATIVE_AGENT_HOOKS_OK`). API failures
and quotas remain synthetic coverage; no real outage or rate limit was induced.

Final actual-agent verification used isolated application PID 89151 in
`/tmp/fm-atn-wqyxhit2`, private Flowmux state/socket directories and hook settings,
and dedicated terminal sessions. Native-hook agent B also used a private agent
home. Only the ten validated private test hooks were trusted. The original
application PID 72789 remained running.

| Scenario | Observed result |
| --- | --- |
| Native-hook agents A 2.1.314 and B 0.166.0, plain response | Working with no old outcome, then hook-backed completed outcome |
| Agent A, question and answer | Typed question wait without old completion; answer resumed work and completed |
| Agent B, one-time command approval | Typed approval wait; the exact harmless printf ran and completed |
| Agent A, immediate Escape with restored draft | Hook identity retained; screen-backed unknown outcome replaced the former Completed label |
| Agent B, Escape during response | Hook-backed canceled outcome and Turn interrupted label |
| Both agents, recovery prompt | Old outcome cleared during Working, then completed with the expected response marker |
| Both agents, unsubmitted approval/quota/interruption/completion/error wording | Activity, evidence timestamp, pending waits, and outcome remained unchanged |

Visual inspection confirmed both question/approval labels, then the distinct
interrupted and unconfirmed-result labels with neutral stop markers in both live
lists. Outcome and draft
evidence is in the two `*-outcomes.jsonl` files and `outcome-drafts.json` under
the isolated root; `agents-outcomes.json` confirms the CLI projection.
`/tmp/fm-outcomes-final-matrix.log` records `FINAL_LIVE_OUTCOME_MATRIX_OK`. Both
sessions ended with completed recovery responses and
empty composers. Verified application and CLI SHA-256 values are respectively:

`68f205f2783e3be47d2a12a02c9730c77e9a00cfb7dfda440d158e78df69fa32`

`2fa462a11b6374ea845a18db7bed14182bdb3a75421385c7d355024197d8a5c8`

The full Linux gate for the preceding pending-wait commit is running from a
frozen snapshot; it does not include this outcome change. Full cross-platform
verification remains outstanding, including the macOS foreground/input blocker
recorded above. These focused results do not replace those final gates.
