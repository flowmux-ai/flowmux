<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Team runtime reference

Read the relevant section when selecting models, diagnosing context or recovering
a worker. Routine follow-ups do not require rereading this reference. The
[skill](../SKILL.md) owns delegation and review policy.

## Choose provider, model and scope

Choose a route only after the delegation decision in SKILL.md. Honor explicit
provider choices; use --no-fallback when a particular or different provider is required.

Classify each assignment by scope, uncertainty and consequence, not prompt
length or role name. Start with the smallest tier that can meet its checks:

| Complexity | Assignment | Codex | Claude | Gemini | agy |
|---|---|---|---|---|---|
| `simple` | Fact extraction, focused search, mechanical small edits with clear checks | GPT-6 Luna / high | Haiku / low | Flash Lite | Gemini Flash / low |
| `standard` | Bounded implementation, debugging with reproduction, ordinary review | GPT-6.1 Sol / medium | Sonnet / medium | Flash | Gemini Argon / medium |
| `complex` | Unclear root cause, cross-module design, concurrency, security or data-loss-sensitive review | GPT-6 Astra / high | Opus / high | Pro | Claude Opus / high |

Pass `--complexity` on every start; use scope, expected work and uncertainty to
choose it. `--agent auto` (the default) chooses an eligible installed CLI, preferring
Codex, then Claude, Gemini and agy. Latest matching-profile Claude/Codex usage
observations exclude exhausted routes before a pane is created. Model-scoped
limits only exclude matching models; Claude web-only limits do not apply to CLI workers.
With fallback enabled, this can also replace
an explicitly selected exhausted provider; `--no-fallback` instead returns a
diagnostic before launching. `--model sol|terra|opus|sonnet` accepts short names
(`sonet` is also accepted); Terra maps to `gpt-5.6-terra`. Gemini CLI has no
`--effort` flag; choose its model tier instead. agy's model IDs depend on account
access; use `agy models` when choosing an override.

The CLI defaults to `standard` if omitted.
The helper sets `--model` and effort for that session and records the selection
in its receipt. Claude names are CLI aliases, so their versions depend on the
installed provider. Account access is still required. Preview without launching:

```bash
python3 /path/to/flowmux-team/scripts/team.py route --agent codex --complexity simple
```

`--model MODEL` overrides the tier's model and preserves provider effort defaults
unless `--effort` is also supplied. Honor a request to preserve an existing model
by explicitly selecting that model. Tell the user the provider, model and brief
reason when delegating. If a completed lower-tier attempt misses its checks,
use its findings to make one targeted higher-tier assignment. Automatic recovery
is bounded as described below; authentication and uncertain launches require inspection.
An ordinary workspace stays in the current agent/model; never spawn to change it.

## Worker lifecycle and context diagnostics

`start` first looks for a completed, idle worker from the same lead, Team workspace,
working directory and provider profile. A compatible provider, model, effort and
edit policy continues through a tracked follow-up in the existing pane and conversation.
The new assignment gets its own report and requested limits; earlier reports remain intact.
If an owned completed worker needs a different configuration, the helper retires
that worker before opening its replacement. Busy workers, input waits, changed
sessions and panes with extra tabs are preserved. Concurrent work can still need
more than one worker; repeated sequential assignments should not keep adding panes.
Older compatible managed workers can still be reused. Automatic retirement requires
a worker started with this helper version; older helpers cannot acknowledge shutdown
and their panes are preserved.

Use `--fresh-session` only when the task needs an independent conversation, such
as a separate reviewer. It explicitly keeps existing workers and opens another pane.
The sample and multi-role examples use this option to keep their reviewers independent.
For a related follow-up to a known worker, continue to use `followup` directly.

When a new worker is needed, the helper uses the Flowmux CLI's `team-spawn` to split
the **lead's** pane. The split's first and only tab launches the provider with the task prompt.
Claude/Codex stay interactive after their answer. Gemini/agy display their native
stream and resume the exact conversation on `followup`. Creating or replacing a
worker preserves the user's active workspace and keyboard focus. Codex workers use a dedicated local
TUI (`--no-daemon`) with `--dangerously-bypass-approvals-and-sandbox`.
Claude workers use `--allow-dangerously-skip-permissions --permission-mode bypassPermissions`;
the allow flag alone does not activate bypass mode. Gemini uses `--approval-mode yolo`; agy uses
`--dangerously-skip-permissions`. YOLO also applies after replacement and restoration. Claude retains its normal
tools and MCP configuration, so it can run shell checks. `--allow-edits` changes
the task prompt only: read-only assignments are instructions, not a sandbox or
tool restriction. Keep assignments within the user's authorized scope.

Jobs retain provider paths and profiles for collection and restoration;
credentials are not copied. Restoration resumes the conversation, not the task.

Retain the JSON receipt: `job`, `pane`, `surface`, `workspace`, `socket` and
`root_job` when present. An automatically reused assignment returns a turn's
`job`; use `root_job` for later explicit `followup` calls. Launch subsequent workers from the lead's origin, never a prior
worker. Explicit pane IDs keep routing independent of focus. Working directory
changes do not change the workspace that receives the split.

The managed Flowmux Codex shim uses `--no-daemon` when supported, preserving
explicit remote connections and provider permission/profile settings. Run
`flowmux hooks refresh-shims` after a binary-only update to refresh existing
wrappers without changing provider settings. The Linux/macOS installers do this
automatically. This affects new invocations, not new chats inside an already-running
shared TUI; keep those sessions open.

Codex source discovery uses `identify --session ID`. A direct Codex process can
prove its source through its agent and Flowmux process ancestry, including on
the first turn before a lifecycle hook. Shared daemons require an exact live
session binding. Do not clear a lead's session ID, replace its pane/socket or
fall back to inherited shared-daemon variables when resolution fails. Outside
Flowmux, use only the user's intended `--socket PATH --pane UUID`.

The lead must have permission to connect to the originating window's Unix socket
and its `.ctl` companion. A socket permission error is not a missing session or
an old CLI. Use the provider's normal permission approval flow for authorized
Flowmux commands when available. If approval is denied, stop and report it;
do not disable the lead's sandbox, silently change network policy, or use another
process to evade the denial. Any socket allowlist change requires the user's
authorization.

## Limits and recovery

New jobs automatically substitute an installed provider/model after a native quota
or capacity error, unavailable model/client, unexpected exit, or stalled reasoning. `--max-attempts` defaults to 4, with no repeated
provider/model pair. A quota-exhausted provider is skipped for the rest of that
assignment. Available alternatives and their effort are recorded in the receipt;
`route` previews them. A catalog or installed CLI does not prove remaining credit:
the native provider enforces its current account limits. No account/credential
rotation or provider-default changes are made.

The existing Flowmux usage collector publishes credential-free observations under
the state directory's `team-usage/`. `route` and launch receipts show `available`,
`exhausted`, `unavailable`, `stale` or `unknown` for each candidate. Only observations at most five
minutes old, from the exact configuration root, affect initial selection; missing,
failed, expired or unsupported observations retain native error-based fallback.
No new account request is made by the helper. Refresh **AI Usage** to renew data.
The existing collector also refreshes while a Team workspace exists, even with
the usage bar hidden; hiding the bar controls display, not Team routing freshness.
Gemini/agy currently have no preflight usage collector.

Native quota errors are shared across jobs under `team-quota/`, scoped to the
provider's configuration root. The hold lasts until a known reset, or five minutes
when no reset is known. A newer available observation releases it early; the old
hold stays released if that observation later expires or refresh fails. A new
native quota error can establish a new hold. Concurrent
jobs update the record under a lock; profiles never borrow each other's limits.
This reduces repeated failed launches, but cannot reserve quota against concurrent
provider usage. Successful work and the native provider remain authoritative.

Native `model_not_found` and `unsupported_client` failures are shared under
`team-unavailable/` for five minutes, scoped to the provider configuration root.
A missing model excludes only that model; an unsupported client excludes all
models of that provider. Other profiles remain eligible. Expiry allows a retry,
including after a client update. Authentication/permission errors are never cached
as unavailability. `route` reports these holds before a pane is created.

`--stall-timeout` bounds absence of native transcript/stream progress (default
180 seconds). Matching native tool start/result IDs grant up to five times this
quiet interval while a tool is pending (15 minutes by default), without resetting
the clock on polling. Codex/Claude tool calls, Gemini tool events and agy tool
steps supply this evidence; a working label or live process alone does not.
Hung tools still reach the extended stall deadline. Increase `--stall-timeout`
for longer expected quiet tools or reasoning. There is
no total time limit by default (`--timeout 0`), so continued progress can outlive
five minutes. An explicit positive `--timeout` bounds an attempt; expiry is
`time_budget`, closes the owned worker and stops without a replacement. It is
distinct from `stalled` recovery and takes priority when both deadlines expire.
Verified native input waits pause the total budget without resetting time already spent.
Native authentication/permission error codes take priority over quota or model
wording in their messages. Authentication, trust/permission prompts, changed session identity,
extra/user-owned tabs and uncertain IPC delivery require inspection, not a retry.

Before replacement, the helper saves evidence, stops its owned process tree,
verifies it stopped, then closes that failed worker's single-terminal pane. A
separate recovery process opens the next pane from the recorded original lead;
it does not depend on focus. Generation guards prevent old launchers replaying a
task. The replacement reads the assignment and prior artifacts, and must inspect
partial edits before continuing. Never close the lead or another user's pane.
If ownership, shutdown or close cannot be verified, recovery stops with a reason.

`--no-fallback` disables substitution; infrastructure failures still use owned-pane cleanup.
When alternatives or the attempt budget are exhausted, the failed pane is closed
and the final failure is retained. Successful and valid task-level `blocked` or `failed` reports remain available
for inspection and explicit follow-up. Task failures and malformed reports do not
substitute a model or close the pane automatically. Malformed reports retain their
answer and diagnostics but cannot authorize follow-up; inspect them first. Initial assignments and helper
`followup` assignments are monitored; manual TUI input is not a managed assignment.
Jobs launched by an older copied helper keep their original behavior.

Every `attempt-N/` retains its prompt, provider/model, answer and evidence.
`status`/`wait` report current `pane`, `surface`, `attempts`, `closed_panes` and
`fallback_used`; the original receipt's pane may have been replaced. Report
substitutions to the user. If researcher and reviewer use the same provider,
call it independent-session review, not cross-provider review.

## Follow-up delivery and restoration

`followup` requires the original lead pane and Team workspace, then checks
the exact worker session, active surface and idle state before input. It records the assignment, paste evidence and a unique marker, and collects
only the matching turn from the pinned transcript. Every follow-up returns its own
job receipt. The original report and earlier turns stay unchanged. Always pass the
original worker job to `followup`; pass a turn's receipt to `status`/`wait`.
Existing jobs can continue if their session is still uniquely identified and idle.
Managed jobs also require a live supervisor before creating a turn and immediately
before sending input. A live agent alone is insufficient; after supervisor loss,
inspect the retained pane and reports rather than sending an unmonitored task.
`followup --timeout SECONDS --stall-timeout SECONDS` overrides the recorded limits
for the next assignment and are then inherited by later follow-ups; zero disables
the total limit. These overrides require
a job created by this updated helper. Old copied helpers retain their time policy.

Only one unresolved assignment per worker is allowed. `dispatch_unknown` means
receipt is not yet proven by the saved user prompt; `running` with `received: true`
means it was received, not completed. A wait timeout does not cancel or resend.
Inspect `dispatch-error.json` and the pane on uncertainty; never automatically
resend or send Enter. Busy/blocked UI, inactive tabs and changed/missing session
bindings reject input. Missing hooks/session identity must be diagnosed, not
replaced with an invented idle event. Valid task-level `blocked` or `failed` reports can be followed up with
missing information or a corrected assignment once the real agent is idle; a permission dialog cannot.
Managed follow-ups use the same bounded recovery policy after confirmed receipt.
Gemini/agy follow-ups resume through native CLI arguments instead of terminal input.

Keep a bounded wait/inspect loop active while work is outstanding. Retain pending
user changes until an idle worker can receive them. This helper does not interrupt
a busy agent or wake an orchestrator after its final response. If manual input or
session replacement interferes, inspect the retained evidence before proceeding.
Never approve dialogs or send slash commands to force progress.

For runnable multi-turn examples, expected evidence and troubleshooting, read the
[communication cookbook](cookbook.md). The examples cover review and
revision, missing input, and a real failing-check → correction → passing-check loop.

Keep reports and artifacts for review. Completed worker panes are available for
reuse or retirement on a later `start`; do not request a fresh pane for every task.
Infrastructure failures use the owned-pane cleanup policy above; never close
the lead or another user's session. Restoring a launcher does not rerun its
assignment. No automatic merge or push.
