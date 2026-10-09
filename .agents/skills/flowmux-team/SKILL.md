---
name: flowmux-team
description: Delegate scoped tasks to new Claude Code or Codex agents in visible Flowmux panes, give each a role, and collect their results. Use when the user requests pane-based teamwork or agent orchestration.
---

<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux team

The current agent leads: choose useful independent tasks, give each worker a role
and scope, then inspect and integrate its result. Workers run as separate agent
processes in visible terminal panes. This is a file-based handoff, not Claude's
native Agent Teams task list or a cross-provider messaging protocol.

Use `scripts/team.py` relative to this skill directory. It needs Python 3.9+,
a running local Flowmux GUI and an authenticated `claude` or `codex` CLI.
The source directory must be trusted by the user. Keep the selected provider and
its configured model unless the user requests otherwise.

## Assign and launch

For each worker, specify a persona relevant to the task (for example reviewer,
implementer or researcher), the concrete task, permitted files, acceptance checks
and expected final output. Provide required context in the task file; workers do
not inherit this conversation. Avoid unnecessary secrets in prompts and logs.

Read-only tasks may share a checkout. Give concurrent editing workers separate
worktrees or disjoint file ownership; the helper does not create worktrees or
merge changes. Delegate only within the user's requested scope. Use
`--allow-edits` only for an assigned editing task. An isolated worktree is not
a security sandbox.

```bash
python3 /path/to/flowmux-team/scripts/team.py start \
  --agent codex --role 'CLI reviewer' \
  --cwd /absolute/project-or-worktree --task-file /absolute/task.txt
```

Inside a pane, the helper reads `FLOWMUX_SOCKET_PATH` and `FLOWMUX_PANE_ID`.
Outside Flowmux, supply the intended `--socket PATH --pane UUID` explicitly.
Socket aliases are resolved once and pinned to that window for the whole launch.
It checks the source in `tree`, splits to a fresh pane, and starts a new terminal
tab there with an executable launcher. It never types into a running agent.
Keep the unused initial shell tab in the new pane; it is harmless.

Launch workers sequentially because pane focus is shared window state. They can
then work concurrently. Do not switch focus during launch. The worker verifies
its pane/socket before starting the agent; a focus race fails without doing the
task. A launch error includes the private job directory: inspect its status,
`launch-error.json` and the Flowmux tree before deciding anything. Never retry an
uncertain launch automatically.

The JSON receipt includes `job`, `pane`, `surface`, `workspace` and `socket`.
Retain it. `--timeout` bounds agent execution (default 300 seconds); it does not
include GUI launch. Codex uses `read-only` by default or `workspace-write` with
`--allow-edits`. Claude permits Read/Glob/Grep, adding Edit/Write with
`--allow-edits`; it has no Bash or MCP tools in this helper. Claude tool denials
are failures. Neither path disables sandboxing, approves prompts, changes hooks,
or selects a cheaper/different model. Codex may still have configured connector
tools; the task must explicitly prohibit unrelated external actions.

## Collect and integrate

```bash
python3 /path/to/flowmux-team/scripts/team.py status /tmp/flowmux-team-JOB
python3 /path/to/flowmux-team/scripts/team.py wait /tmp/flowmux-team-JOB --timeout 60
```

`pending` means the launcher has not started; `running` means the worker started.
The helper asks for a JSON final answer with `task_status` (`completed`, `blocked`
or `failed`) and a nonempty string `report`. `completed` requires exit code zero
and a valid report declaring completion. Missing or malformed reports fail closed;
an agent exiting zero alone does not complete the task. `blocked`, `failed` and
`timed_out` never supply a successful result. A wait timeout exits 124 and leaves
the worker running; it is not an execution timeout. Status/wait exit 1 on worker
failure, blocked tasks or execution timeout and 0 on completion (`status` also
exits 0 for pending/running). `exit_code`, when present, describes the agent
process; `task_status` describes its reported task outcome.

Inspect `result.md` for any valid task report, including blocked/failed reports,
and `stdout.log` / `stderr.log` (plus Codex's `answer.txt`) on parsing failures.
The pane displays the role and artifact path while working and the final answer
when done. Logs carry detailed progress; `read-screen` is useful for debugging,
not a completion signal. Review evidence and any diff before accepting the
worker's claim. Supply the first worker's result as explicit context for a
dependent review task, then integrate and report from the lead session.

Quota, authentication, missing tools or denied permissions are blockers, not
reasons to relaunch with stronger permissions. Report them and keep artifacts.
A killed GUI/worker can leave stale pending/running state; inspect the exact
pane and process before concluding anything. A `started` marker prevents a
restored terminal from rerunning the job; do not delete it to force a retry.

Retain new panes and job files until results are reviewed. Close only recorded
worker panes when cleanup is requested, and only after completion; never close
the source pane or another session. Copy wanted results out of temporary storage
before deleting job artifacts. There is no automatic merge, push or cleanup.

For a runnable researcher → reviewer handoff, read
[the sample](references/sample.md). For general Flowmux operations, use its CLI
help or the separate `flowmux-browser` skill if available.
