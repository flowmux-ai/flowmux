<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Team communication cookbook

These small examples demonstrate how the lead keeps responsibility after the
first worker reply. Each has a finite sequence, observable acceptance checks,
and preserved inputs/results. The scripted lead makes the sequence reproducible;
an agent orchestrator should use the same evidence to decide whether to revise,
clarify, stop or finish. Passing these examples does not prove arbitrary agent
judgment or unattended background operation.

## Before running

Run in a **Team workspace** with the intended lead pane identified by
`python3 "$TEAM_SKILL/scripts/team.py" context`. Use the absolute installed skill
directory as `TEAM_SKILL`. The full bundle includes `scripts/examples.py`.
An authenticated Claude Code or Codex CLI, available simple-tier model and live
session/activity reporting are required. The examples use real interactive TUIs
and consume account quota. They do not change model defaults or hook trust.
They disable provider substitution so the requested provider is actually tested.

Do not type into worker panes while a tracked assignment is in progress. A missing
session binding, trust prompt or permission dialog is a diagnostic stop. Follow-up
input requires the recorded tab to be active and the exact agent to be idle.

```bash
export TEAM_SKILL=/absolute/path/to/flowmux-team
python3 "$TEAM_SKILL/scripts/examples.py" --case review --agent codex
python3 "$TEAM_SKILL/scripts/examples.py" --case clarify --agent claude
python3 "$TEAM_SKILL/scripts/examples.py" --case repair --agent codex
```

The printed `ARTIFACTS` directory contains each request, dispatch/status logs,
`run.json`, source fixtures and actual check output. `run.json` links to the
worker job and each separate turn. Worker panes remain open for inspection.
An exception writes `failure.txt`; it never launches a duplicate automatically.
Each assignment waits up to `--timeout` seconds (default 300), in 15-second waits.

## 1. Research → review → changed requirement → review again

**Use:** evidence-based summaries, document comparison, bounded investigations.

Source: a three-line service specification: Flowmux, capacity 25, region Seoul.
Two read-only workers perform four turns:

1. Researcher reports product and capacity from the source.
2. Reviewer receives the actual result path and verifies its claims.
3. Lead adds the region requirement and sends the review path to the **same
   researcher**. The researcher rereads the source and revises its answer.
4. The **same reviewer** checks the revised report against the source.

Acceptance: capacity 25 appears in the initial report; both revised reports
include 25 and Seoul; pane, surface and session stay the same for each worker;
the original report remains unchanged. Reports must still be inspected for
quality: value checks alone cannot prove that a review was thoughtful.

For cross-provider review, append `--review-agent claude` to a Codex run, or the
reverse. The default uses two independent sessions of the same provider.
No cross-provider claim should be made for that default.

## 2. Missing input → clarification → completion

**Use:** incomplete requirements, ambiguous values, bounded planning.

One read-only planner receives capacity 25 with requested seats unspecified.
The required first result is `blocked`, naming the missing input. The lead then
updates the fixture to requested seats 9 and asks the **same planner** to reread
and calculate the remaining seats.

Acceptance: first task is genuinely `blocked`; the second is `completed` with
16 remaining seats; both belong to the same session; the blocked report remains.
This demonstrates application-level clarification, not approving a permission
request. In real work, ask the user for missing information instead of inventing it.

## 3. Inspect → failing test → correction → passing test

**Use:** a bounded bug fix with an independently executable acceptance check.

The runner creates a temporary `shipping.py` whose free-shipping condition is
`total > 50`, plus checks for totals 49, 50 and 51. One editing worker first
describes the current code without changing it. The lead executes the test and
records the actual failure at 50. It sends that evidence and the requirement
“50 or more” to the **same worker**, allowing edits to `shipping.py` only.
The lead executes the same checks again after receiving the correction.

Acceptance: the original test fails at 50; the corrected code passes all three
boundaries; the worker session is reused. The lead runs checks, so this works
with Claude's file-only tools as well as Codex. Editing is limited to disposable
example files; your repository is not changed by the assignment.
The temporary working directory may require provider trust. If a trust dialog
appears, stop and let the user handle it; never automate approval.

## Use the continuation contract directly

```bash
# Save the initial start receipt and its original job path as WORKER_JOB.
python3 "$TEAM_SKILL/scripts/team.py" followup "$WORKER_JOB" \
  --task-file /absolute/followup.txt > /absolute/followup-receipt.json
# Read the returned .job, then pass that exact turn directory:
python3 "$TEAM_SKILL/scripts/team.py" status /worker/job/turns/0001
python3 "$TEAM_SKILL/scripts/team.py" wait /worker/job/turns/0001 --timeout 60
```

The lead's follow-up should name the changed requirement, actual prior evidence,
allowed edits, and acceptance checks. Keep it short. A result's `completed`
status means the worker finished that assignment; the lead still checks it.

| Observation | Lead action |
|---|---|
| `dispatch_unknown` | Receipt is not proven yet; inspect/wait on the same turn. Do not resend. |
| `running`, `received: true` | The user prompt is in the pinned transcript; continue bounded waiting. |
| `completed` | Inspect evidence and acceptance checks; revise or finish. |
| Task report `blocked` | Supply actual missing information, then continue when idle. |
| UI busy / needs input / missing session | Diagnose the recorded pane; do not type or fabricate state. |
| Wait exits 124 | Deadline elapsed, task not cancelled. Keep evidence and decide whether to wait more. |
| Malformed report / changed transcript | Stop automated handoff; inspect the retained answer and session. |

`followup` rejects concurrent dispatch and unresolved earlier assignments.
It preserves uncertain delivery rather than risking a duplicate. It does not
queue into a busy TUI, interrupt a running task, replace a failed session, or
restart the lead after the lead has ended its own turn.

## Reproduce verification from the source checkout

```bash
python3 scripts/test-agent-team.py
cargo build -p flowmux -p flowmux-cli
python3 scripts/test-team-pingpong-gui.py
# Explicit live-account checks, same examples and real provider CLIs:
python3 scripts/test-team-pingpong-gui.py --real-agent claude
python3 scripts/test-team-pingpong-gui.py --real-agent codex
```

The GUI harness uses a separate Xvfb display, D-Bus and XDG state. It retains
terminal screens, workspace trees and logs and closes only its test-owned GUI.
Default provider fixtures validate deterministic transport and state handling in
real panes, not model quality or account access. The `--real-agent` runs validate
native CLI conversations and content checks; report those results separately.
Linux requires the Xvfb/python3-xlib dependencies of the existing GUI harness.

## Verification record — 2026-10-10, Linux

| Check | Observed result |
|---|---|
| Helper regression tests | 33 passed, including repeated follow-ups, immutable results, uncertain delivery recovery, concurrent dispatch rejection, missing identity, wrong/busy targets and provider substitution history. |
| Isolated GUI + deterministic providers | All 6 runs passed: 3 cases × Claude/Codex transcript formats, 16 total assignment turns. Actual panes received terminal input and returned newly appended transcript answers. |
| Skill bundle | Rust installation tests and the live Settings install/update/remove and session restoration suite passed with all five resources. |
| Native Claude Code 2.1.296 / Haiku | Attempted clarification case; stopped at initial theme/onboarding UI before producing a task result. Native ping-pong remains unverified. |
| Native Codex 0.162.1 / GPT-6 Luna | Attempted clarification case; the sandbox returned `bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted`, then the CLI requested escalation to read the task file. No approval was supplied; native ping-pong remains unverified. |

The native runs timed out without replaying their assignments. No sandbox or
trust setting was relaxed. Resolve native onboarding/sandbox prerequisites in the
user's environment before claiming end-to-end model validation. Mock-provider
passes establish the transport/state contract, not model behavior.

Local evidence retained by this run (temporary paths, not distributed assets):
`/tmp/fm-gui-spfx1rve` contains all six passing GUI logs, trees and screenshots;
`/tmp/fm-gui-yrakb528` contains the bundle/lifecycle checks;
`/tmp/fm-gui-__i2ndna` and `/tmp/fm-gui-by8sigiz` contain the respective native
Claude/Codex blocked-run logs and terminal screens.
