<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent status verification

Keep the user's running sessions intact. Use a rebuilt isolated GUI, separate
XDG state/config/data and a short `FLOWMUX_RUNTIME_DIR` under `/tmp` on macOS.

## Decision evidence

Enable `FLOWMUX_LOG=warn,flowmux_agent=debug` on the isolated GUI and hook CLI.
This uses existing console/daily logs and their bounded writer queue; it adds no
persistent event database. Debug capture is opt-in and is not a history of events
that happened before it was enabled. Retain logs with the tested Git revision and
binary checksum: the trace's package version alone does not identify a build.

Hook/screen spans identify the process, OS, version and surface. Session/turn/tool
identifiers are hashed correlation tokens, not raw payloads or a cryptographic
anonymization guarantee. Only known provider/identity-source labels are printed;
messages, status text, cwd, terminal text and titles are excluded. CLI routing
records candidate/selected surface IDs without logging the input payload.

`identity_source` describes presence ownership; `evidence` describes the input
being evaluated. `previous`/`next` describe the actual surface status, not the
workspace aggregate. `changed=false` is not necessarily rejection: repeated
reports and ledger-only events can be valid without changing the display.
Guard reasons distinguish stale sequence/turn, mismatched identity, ended
session, pending waits, active children, unchanged completion and settled-turn
spinner. `report_result`/`screen_result` record the underlying merge outcome;
an unchanged merge does not claim to identify a more specific core rejection.

Reproduce completion with and without Stop, both focused and hidden, while a
second session remains working. Then begin another turn and replay stale output.
`scripts/test-agent-hooks-gui.py` runs these cases through the actual CLI, IPC,
PTY and VTE. Never add a focus change just to make hidden completion pass.

## Platform gates

`bash scripts/test-ci.sh macos` runs core/daemon/CLI/process unit tests plus the
same hook replay on the AppKit main thread (`FLOWMUX_AGENT_SMOKE_ONLY=1`). The
Linux coverage gate invokes the replay through the isolated SSH GUI harness.
Both must exercise hidden and focused terminals without changing the scenario.

For a focused local unit run, use `python3 scripts/test-agent-unit.py`. It strips
pane/runtime context and reparents only its own test worker before launching
Cargo: merely removing environment variables or calling setsid still leaves a
managed Codex daemon in the test process ancestry. Native-daemon detection tests
continue to create their explicit daemon ancestor inside that isolated worker.
The normal runner returns Cargo's failure status; CI runner checks also inject
unit/native failures to prove that later gates cannot mask them.

The macOS gate records the Git revision, CLI checksum, unit results and native
hook marker in `target/ci/macos/`. A configured Linux gate is not evidence that it
ran on a macOS workstation; report unavailable platform execution separately.

## Transition contract and regression cases

`StateStore` owns session/turn/wait/child correlation. Core applies the existing
presence merge; the GUI supplies parsed terminal observations and visibility.
Do not copy lifecycle guards into CLI parsers or add a second state machine.

| Rule | Executable regression |
| --- | --- |
| Process evidence establishes identity/liveness, not whether a turn is working. A stale process scan cannot replace newer hook identity. | `stale_process_snapshot_cannot_displace_a_new_native_session` |
| Route to the unique strongest session binding before applying activity. Multiple equally strong candidates do not authorize choosing the first. | `codex_session_tab_rejects_duplicate_title_candidates`, `codex_routing_rejects_ties_and_prefers_unique_exact_session`, GUI ambiguous-session replay |
| Stop completes a response turn; SessionEnd tears down its session. Surviving background jobs/cron do not by themselves keep a Claude response working. | `claude_lifecycle_tracks_stop_interrupt_restart_and_session_end`, GUI Claude Stop replay |
| Tool wait delivery is idempotent by invocation ID. A resolved ID stays resolved until a boundary. Session/permission waits remain separate. | `correlated_wait_delivery_is_idempotent_and_resolution_is_final`, `claude_batch_completion_clears_only_the_permission_wait` |
| Parent Stop must respect observed active children. Old child events must not settle a reused child's newer turn. | `codex_parent_stop_waits_for_matching_active_subagent`, `older_cross_turn_codex_child_events_cannot_replace_the_current_turn`, GUI child replay |
| Older terminal events and grace timers cannot cross newer turn activity. A new root turn invalidates a pending old Stop. | `terminal_boundaries_cannot_overwrite_newer_same_turn_activity`, `older_codex_grace_timer_cannot_consume_a_newer_pending_stop`, `codex_new_root_turn_invalidates_pending_parent_stop` |
| Screen completion is fallback evidence; it cannot clear correlated waits/children or complete a new turn with its unchanged old footer. | `completion_screen_preserves_correlated_waits_and_children`, GUI permission/footer replay |
| A settled native turn cannot be reopened by a stale spinner. New native work still accepts live screen progress. | `stale_codex_spinner_after_grace_settlement_stays_idle`, GUI native completion/next-turn replay |
| Screen fallback reads the live terminal grid, excluding replaced frames in scrollback even when the user scrolls history. | `agent_status_text_excludes_replaced_frames_in_scrollback`, GUI hidden footer replay |
| Parsed output must refresh hidden terminals too. Raw PTY notification is not proof that VTE already parsed the final bytes. | `hidden_terminal_output_refreshes_after_vte_parses`, GUI hidden footer replay |
| A local process lookup missing a remote agent is not proof of exit. Restored terminal text alone is not proof of life. | `reconcile_process_agents_keeps_screen_presence_for_out_of_tree_agent`, `process_poll_rejects_replayed_agent_screen_after_restart` |
| Late SessionEnd/dead-PID observations cannot clear a replacement session/process. | `session_end_only_removes_the_current_agent_session`, `dead_pid_clear_cannot_remove_a_replacement_process` |

Idle is the runtime completed-turn activity. Done is its unseen-completion UI
projection, not a separate process lifecycle. No new status enum is needed.

The router keeps the established order: unique reported session, then matching
thread title (prefer an unclaimed tab), then the only eligible unnamed tab in
the directory. The window returns `AgentSurfaceAmbiguous` for ties. The CLI
checks all responding windows: a unique exact match beats heuristic ambiguity;
duplicate exact bindings or unresolved heuristic ties reject the hook without
mutating either target. Older servers that answer only the probe ping are still
skipped. A sole unnamed candidate remains a heuristic, not proof of ownership;
this change removes arbitrary tie-breaking, not that existing fallback limit.

`scripts/fixtures/agent-status/codex-goal.json` records the minimal reported
footer shape, origin/date/platform and whether the provider version is known.
It is a synthetic redacted sample, not a full transcript. Preserve the footer,
composer and hints together when adding a format regression. Do not substitute
a focus change for reproducing a hidden-workspace failure.

For the next incident, retain the failing event/screen sequence and connect it
to one contract row and the earlier relevant commits. Fix the responsible layer,
then verify both the symptom and its opposite error (early idle versus stuck
working). Commit only after the matching unit and isolated GUI checks pass;
repeat the affected checks after that commit before proceeding to another stage.
Report build, test, commit, installation and running-version verification
separately. A configured but unexecuted platform gate is not a pass.
