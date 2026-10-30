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
when an adapter has explicit evidence. Mixed reasons display Multiple
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

## Waiting actor and retry context

Live waits now expose `actor` and `retry` alongside reason, scope, count, and
start time. Missing or future values remain unknown. Approval, question, and
explicit authentication failures identify user action. A usage-limit or generic
API failure alone does not establish who must act or whether a retry is scheduled.
An explicit stale auto-resume event reports user/manual and displays
`Usage limit reached; input required`. Ending auto-resume without continuing
clears the wait but preserves an already known failed response outcome.

Duplicate same-reason events retain explicit retry context and the original
start time. A changed known reason resets obsolete actor/retry context. Summary
groups include actor and retry so an automatic wait cannot hide a simultaneous
manual wait with the same reason. Tooltip details show the waiting actor and
known retry mode; public activity and the existing request-resolution rules are
unchanged.

Automatic retry is representable and covered with explicit lifecycle evidence
in reducer tests. The current native adapter does not receive an automatic-wait
start or reset deadline from the failure hook, so it does not fabricate either.
Automatic-wait screen refinement and its evidence boundary remain follow-up
work. No deadline is inferred from an ambiguous displayed clock.

The pre-change regression failed on the missing actor field in
`/tmp/fm-wait-context-red.log`. Final isolated unit runs passed 167 core and
193 daemon tests on macOS and Linux, with the existing CLI and process tests:
`/tmp/fm-wait-context-unit-verified.log` and
`/tmp/fm-wait-context-linux-unit.log`. IPC compatibility tests passed in
`/tmp/fm-wait-context-ipc.log`; full workspace/all-targets Clippy with native-smoke
passed in `/tmp/fm-wait-context-clippy.log`.

The expanded native replay passed authentication categories, unknown retry on
quota failure, manual resumption, automatic resumption, cancellation preserving
failure, and changing authentication to a generic API error. Live GTK tooltip
inspection also observed the waiting actor. Log:
`/tmp/fm-wait-context-native.log` (`LIVE_NATIVE_HOOK_MATRIX_OK`,
`MACOS_AGENT_EVIDENCE_TOOLTIPS_OK`, `MACOS_NATIVE_AGENT_HOOKS_OK`). Authentication
and quota events were synthetic; no real login failure or quota exhaustion was
induced.

Actual-agent verification used application PID 848 in `/tmp/fm-atn-jkp6usvc`,
separate application state and sockets, private hook settings, and a private
home for native-hook agent B. Agents A 2.1.314 and B 0.166.0 passed plain responses,
question/answer, one-time command approval, immediate/delayed interruption,
recovery, and unsubmitted status-like drafts. The actual question and approval
reported user/unknown context and cleared only after their corresponding
answers. Screenshots showed both request types and the later interrupted versus
unconfirmed-result states. Final recovery responses completed with empty waits.
Evidence: `/tmp/fm-wait-context-live.log` (`LIVE_WAIT_CONTEXT_MATRIX_OK`) and the
state/draft/CLI JSON files under the isolated root. Application and CLI hashes:

`43a6bf4476c9c5c7a046cbeab84abd7d07036fe58cd6721f71f3a88f38accce8`

`a4381724e87a3ce6d37bc6e6e2df2bc385d073e277ebcb78ae1df96d8de45b82`

A test-session vendor auto-update had redirected its executable link into a
temporary data directory. Only the owned test-agent processes were stopped;
the same binary was copied to permanent storage and the link restored with a
matching SHA-256. Subsequent live tests disabled auto-updating in their private
environment and verified the permanent link stayed unchanged. Original
application PID 72789 and the independent user agent remained alive.

The preceding full Linux gate failed because the editor I/O fixture dismissed
an error dialog before it appeared, leaving a later focus command queued behind
the still-open dialog. Screenshots established this ordering. The fixture now
waits for the named accessible dialog before dismissing it, then observes its
disappearance. The same failing binary passed the complete focused scenario
(`/tmp/fm-editor-io-recheck.log`), committed separately as `c4c4c486`.
The full gate is running from that frozen commit in
`/tmp/fm-outcomes-c4-linux-final.log`; it excludes this wait-context change.
Final full cross-platform verification remains outstanding.


## Lifecycle notification admission (2026-10-30)

A hook's notification now accompanies its lifecycle request when both carry the
same explicit pane and surface. The GUI emits that effect only after accepting
the lifecycle report and confirming the current session at its exact surface.
Rejected stale, duplicate-sequence, foreign-session, and missing-surface reports
produce an unapplied receipt and no notification. A stale pane hint cannot
redirect an accepted surface's notification.

The optional wire field preserves older request shapes. An explicit legacy
`Ok` response permits the CLI's former standalone notification path; an applied
or rejected receipt does not. Timeouts, transport errors, and unexpected replies
never trigger a replay because the server may already have applied the effect.
The bound request retains the former pair's combined delivery budget. This is
admission control, not causal deduplication: newer reports of the same pending
request can still produce repeated notifications with different message text.

The focused regression failed before the change with `Ok` instead of the
expected unapplied receipt (`/tmp/fm-notify-admission-red2.log`). The earlier
`red.log` used malformed test JSON and is not product regression evidence.
Final validation passed 68 GUI IPC handler tests on Linux, isolated agent unit
suites on both platforms, 78 IPC unit tests, five protocol samples, four IPC
stress tests, and full workspace/all-targets Clippy with native-smoke. Logs:
`/tmp/fm-notify-admission-linux-handler.log`,
`/tmp/fm-notify-admission-unit-final.log`,
`/tmp/fm-notify-admission-linux-unit.log`, `/tmp/fm-notify-admission-ipc.log`, and
`/tmp/fm-notify-admission-clippy-final.log`.

The native main-thread replay passed hook transitions, stale bound approval and
error rejection, and evidence tooltips (`/tmp/fm-notify-admission-native.log`,
`LIVE_NATIVE_HOOK_MATRIX_OK`, `MACOS_AGENT_EVIDENCE_TOOLTIPS_OK`, and
`MACOS_NATIVE_AGENT_HOOKS_OK`). These replayed events are synthetic.

Actual agents A 2.1.314 and B 0.166.0 ran against application PID 9278 in
`/tmp/fm-atn-tz8dqd_7`, with isolated state, socket, and private hook settings.
Both passed plain responses, question or one-time harmless command approval,
interruption, recovery, and unsubmitted status-like drafts. Question and
approval notifications retained their exact workspace/pane/surface. Native
screens showed the two wait reasons and later canceled versus unconfirmed
outcomes. A synthetic stale bound event injected into each actual waiting
session returned unapplied, created no notification, and left the wait intact.
This injection is distinct from actual native hook generation.

The visual checkpoint timer expired while inspection was pending; the test
continued from the existing waiting sessions without restarting either agent.
The continued matrix passed (`/tmp/fm-notify-admission-resume.log`,
`LIVE_NOTIFICATION_ADMISSION_MATRIX_OK`); initial evidence remains in
`/tmp/fm-notify-admission-live.log`. State, notification, draft, and final CLI
JSON are retained under the isolated root. Application and CLI SHA-256:

`ac25d746f6074474acb6bd10757e1bd7ef6b7c876fd59c85f51f09520cd7b8c5`

`939f529fd08259b0a0e022a7dc3f9d1b7b2643a9aa1dc7e66c25e48e545bbbb2`

Original application PID 72789 and independent user agent PID 1401 remained
alive; the permanent vendor executable link stayed unchanged. The frozen
`c4c4c486` Linux full gate is still running and excludes this change. Final full
cross-platform verification and causal notification deduplication remain open.


## Pending request notification identity (2026-10-30)

Pending tool, permission, and session waits now own an opaque notification ID
and a once-only claim. An accepted reminder can update state text without
creating another row or toast. The claim stays with the active request when
notification history is cleared or evicted; it ends with that request's existing
resolution/turn/session boundary. Delayed effects cannot claim a replacement
request's identity. Different identified requests keep separate rows even when
their text matches within eight seconds.

Unknown-to-known classification retains the same request identity. A change
between known reasons, or an explicit automatic-to-manual retry transition,
opens a new notification cause. Existing wait ownership, sequence checks,
resolution scopes, and public status are unchanged. The new claim is runtime
only and at-most-once: it does not introduce a durable delivery/retry queue.
Completion correlation and anonymous notification pairing remain separate
follow-up work.

The failing regression reproduced a second notification from a newer report of
the same pending permission (`/tmp/fm-pending-notify-red.log`). Final Linux GUI
checks passed 68 IPC handler and 31 notification-store tests
(`/tmp/fm-pending-notify-linux-gui-verified.log`). Isolated unit suites passed on
both platforms; the final Linux run included 195 daemon tests. macOS ran the
full earlier suite plus the two focused claim/identity tests after their
addition (`/tmp/fm-pending-notify-unit.log`,
`/tmp/fm-pending-notify-claim-unit.log`,
`/tmp/fm-pending-notify-linux-unit.log`). Full workspace/all-targets Clippy with
native-smoke passed (`/tmp/fm-pending-notify-clippy-verified.log`). Native GTK
hook replay and evidence tooltip checks passed
(`/tmp/fm-pending-notify-native.log`, `MACOS_NATIVE_SMOKE_OK`).

Actual agents A 2.1.314 and B 0.166.0 used isolated application PID 17652 in
`/tmp/fm-atn-ypcslfed`. A's actual permission reminder updated its hook metadata
while its question still had exactly one notification. Both agents passed plain
responses, question or one-time harmless command approval, interruption,
recovery, and unsubmitted status-like drafts. A later actual question produced
a different notification ID with the same text; both questions completed with
no remaining waits. Screenshots verified request and interruption states.

A synthetic history-clear probe initially omitted its reason and therefore
used the protocol's approval default, changing the known question reason and
correctly opening a new cause. The test restored the question reason and
repeated with explicit unknown classification: after clearing history, the
accepted reminder created no row and the question remained pending. This probe
is synthetic, separate from the real reminder observation. A hidden terminal
viewport also omitted B's approval menu; focusing the exact isolated workspace
made the menu observable, and only the displayed one-time command was approved.
Neither agent was restarted during these test continuations.

Evidence: `/tmp/fm-pending-notify-live.log`,
`/tmp/fm-pending-notify-resume.log` (`PENDING_CAUSE_RETAINED_AFTER_CLEAR`), and
`/tmp/fm-pending-notify-resume2.log` (`LIVE_PENDING_NOTIFICATION_MATRIX_OK`).
The isolated root retains wait/final notifications, draft checks, final agent
JSON, and `distinct-question-notifications.json`. Application and CLI SHA-256:

`4e65d1aa1dfe693145e8bf5ec841532086770e139f6ecd3684ef553c1e2edd2b`

`939f529fd08259b0a0e022a7dc3f9d1b7b2643a9aa1dc7e66c25e48e545bbbb2`

Original application PID 72789 and independent user agent PID 1401 remained
alive, and the permanent vendor executable link remained unchanged.
The frozen `c4c4c486` full Linux gate ended with failure while waiting for the
editor synchronization-error dialog, despite passing coverage thresholds.
Its reports are archived at `/tmp/fm-outcomes-c4c4c486-reports.tar.gz`.
Focused reruns of that exact binary passed; the full-gate fixture failure is
still under investigation. This is not a completed cross-platform gate.


## Completed-response notification identity (2026-10-30)

The most recently completed response now owns a runtime notification receipt.
Repeated accepted completion reports share that receipt until accepted new work
starts. Clearing or evicting notification history does not reset the claim.
Delayed effects cannot claim a superseded response. Existing lifecycle, screen
settlement, sequence, and ownership guards still decide whether work is accepted;
rejected spinner repaint cannot reopen a completed response. Notifications route
through the accepted surface's current pane instead of a stale pane hint.

Anonymous notifications retain eight-second suppression only for an exact
level/title/body repeat on the same pane and surface. Information and completion
entries no longer merge by timing alone, so unrelated information retains its
content and read state. Identified responses bypass that anonymous time window.
This remains runtime-only, at-most-once correlation. It cannot reconstruct a
missing native turn ID or identify an old completion newly stamped after an
unrelated accepted work boundary; it is not a durable delivery queue.

The focused regression failed before implementation because completion lacked
a notification identity (`/tmp/fm-completion-cause-red.log`). Final isolated
agent unit suites passed on both platforms, including 197 daemon tests. Full
workspace/all-targets Clippy with native-smoke and the canonical application
build passed. Logs: `/tmp/fm-completion-cause-unit-final2.log`,
`/tmp/fm-completion-cause-linux-unit.log`,
`/tmp/fm-completion-cause-clippy-final2.log`, and
`/tmp/fm-completion-cause-build-final2.log`. A test initially expected an old
screen spinner to override a hook-owned idle state; the existing guard correctly
rejected it. The test was corrected, and no status guard was weakened.

Linux passed 68 GUI IPC tests, 31 notification-store tests, and the complete
notification GUI fixture, including filters, read/delete actions, exact source
navigation, closed-source handling, and distinct information/completion content
(`/tmp/fm-completion-cause-linux-verified.log`). Native main-thread hook replay
and evidence tooltips passed (`/tmp/fm-completion-cause-native-final2.log`,
`MACOS_NATIVE_SMOKE_OK`). Replay events are synthetic.

Actual native-hook agents A 2.1.314 and B 0.166.0 ran against isolated application
PID 29802 in `/tmp/fm-atn-q9ghxxso`. Identical consecutive actual responses kept
two distinct completion IDs for each agent, with intervals of 3.109 and 2.654
seconds respectively. Both passed question or one-time harmless command
approval, interruption, recovery, and unsubmitted status-like drafts. A later
question opened a new request and completed with no remaining waits. Native
screens showed question versus approval and canceled versus unconfirmed result.

Synthetic completion retransmissions into A's actual completed session, after
the eight-second window and again after history clearing, created no new row
(`COMPLETION_REPLAY_AFTER_WINDOW_AND_CLEAR_OK`). Synthetic stale wait events
created no effect, and a repeated current wait remained claimed after clearing
history. These injections are separate from actual native hook observations.
The replacement-question hook arrived before its menu rendered, causing an
immediate screen assertion to fail. The driver now waits for the displayed
option; verification resumed in the same session without restarting agents and
completed successfully. Evidence: `/tmp/fm-completion-cause-live.log` and
`/tmp/fm-completion-cause-resume.log` (`LIVE_COMPLETION_CAUSE_MATRIX_OK`), plus
notification, draft, final-agent, and per-scenario JSON under the isolated root.
Application and CLI SHA-256:

`7efc37ed21d4fdf124d3ee56eb027dd23f33c16a3a8f6f17beb9e2426cdc322a`

`939f529fd08259b0a0e022a7dc3f9d1b7b2643a9aa1dc7e66c25e48e545bbbb2`

Original application PID 72789 and independent user agent PID 1401 stayed alive;
the permanent vendor executable link stayed unchanged. Final full Linux/macOS
gates remain outstanding, including the previously recorded editor-dialog and
browser-input fixture failures. Focused passing checks do not close those gates.


## Stable side-panel agent rows (2026-10-30)

The side-panel Agents list now reuses each surface's button and click handler.
Changed content updates inside that button, elapsed time updates its label, and
ordering changes move existing children without detaching them. A row's current
workspace and pane are updated independently of its persistent surface identity.
Ended surfaces release their rows, including when the final agent disappears.
Lifecycle detection, sorting policy, and the horizontal agent bar are unchanged.

A regression first failed because a status refresh replaced the button
(`/tmp/fm-stable-rows-red.log`). The shared native GTK check uses 24 rows and
verifies button identity, keyboard focus, scroll position, updated tooltip,
reordering, routing after a location change, and widget/handler release. The
release check waits for the next GTK frame because a mapped, focused widget can
still be retained during the current frame. Linux passed all 28 sidebar tests
(`/tmp/fm-stable-rows-linux2.log`). Full workspace/all-targets Clippy with
native-smoke and the application build passed (`/tmp/fm-stable-rows-clippy2.log`,
`/tmp/fm-stable-rows-build.log`).

The macOS main-thread harness passed the same row check, native hook replay,
and evidence tooltips (`/tmp/fm-stable-rows-native.log`,
`MACOS_STABLE_ACTIVITY_ROWS_OK`, `MACOS_NATIVE_SMOKE_OK`). Those hook events
are synthetic. The actual-agent application used PID 36782 and separate
state/socket/private hooks in `/tmp/fm-atn-x5jj1034`. Native-hook agent B 0.166.0
passed consecutive responses, one-time harmless command approval, interruption,
and recovery. Clicking the visibly updated A and B rows opened the correct
actual terminal tabs. Unsubmitted status-like drafts left both agents' state
and waits unchanged. Evidence: `/tmp/fm-stable-rows-b-live.log`
(`LIVE_STABLE_ROWS_B_MATRIX_AND_A_QUOTA_OK`), draft/final-agent JSON, and the
observed native screenshots. Application and CLI SHA-256:

`b8c841d4e23f1672c4b57fca25e5d3d5581f683b33012445d44f3b79c5940629`

`d82e62edc956b0c6932c5b0d144d08e5ae9ee8096c1095db4966de87b55a3940`

Agent A 2.1.314 reached its real five-hour quota on the first plain prompt;
the full two-agent scenario stopped at that boundary
(`/tmp/fm-stable-rows-live.log`). Its failure/quota wait remained visible, but
normal-response/question/recovery coverage for A on this build is incomplete.
The actual terminal advertised automatic continuation while structured retry
context remained unknown. Canceling that owned test's automatic continuation
with Escape changed the terminal footer without another hook sequence; the
quota wait remained. This confirms the separate screen-context improvement is
still needed. Raw screen and state evidence before/after cancellation is in
`actual-quota-screen.txt`, `actual-quota-state.json`,
`actual-quota-canceled-screen.txt`, and `actual-quota-canceled-state.json` under
the isolated root. Account limits, billing, and model settings were unchanged.

Original application PID 72789 and independent user agent PID 1401 stayed alive;
the permanent vendor executable link stayed unchanged. This validates the row
change with the stated actual-agent limitation. It does not complete automatic
retry classification, the horizontal bar, shared attention counts/priorities,
or the outstanding full cross-platform gates.
