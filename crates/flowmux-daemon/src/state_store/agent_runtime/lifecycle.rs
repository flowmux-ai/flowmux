// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared hook transitions; callers own validation, projection and effects.

use super::*;

#[derive(Default)]
pub(crate) struct LifecycleTransition {
    pub(crate) report: Option<AgentStatusReport>,
    pub(super) notification_scope: Option<(AgentWaitScope, String)>,
    pub(crate) completed: bool,
    pub(crate) completion_message: Option<String>,
    pub(crate) settle_codex_after_grace: Option<CodexGraceSettlement>,
}

impl<Owner: Copy + Eq + Hash> AgentLifecycleRuntime<Owner> {
    // The caller validates local surface or Host/process ownership first.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn reduce(
        &mut self,
        owner: Owner,
        agent: &str,
        pid: Option<u32>,
        seq: Option<u64>,
        session_id: &str,
        lifecycle_event: AgentLifecycleEvent,
        current: Option<&AgentPresence>,
    ) -> Option<LifecycleTransition> {
        use flowmux_core::AgentActivity::{Idle, NeedsInput, Running};
        let runtime = self;
        let agent = agent.to_ascii_lowercase();
        let wait_key = (owner, agent.clone(), session_id.to_string());
        // SessionEnd/dead-PID teardown leaves a bounded tombstone. A native
        // SessionStart normally establishes the next epoch. The one exception
        // is a live outer agent returning after a nested agent owned the pane:
        // process polling must first restore the same process identity, and the
        // returning hook must match the displaced live PID with a newer event.
        if let Some(ended) = runtime.ended.get(&wait_key) {
            let can_reactivate = ended.reactivate_on_process_return
                && current.is_some_and(|located| {
                    located.name.eq_ignore_ascii_case(&agent)
                        && located.source.as_deref() == Some(flowmux_core::AGENT_SOURCE_PROC)
                })
                && ended.pid.zip(pid).is_some_and(|(ended, incoming)| {
                    ended == incoming && flowmux_procmon::pid_alive(ended)
                })
                && seq.is_some_and(|incoming| ended.seq.is_none_or(|floor| incoming > floor));
            if !can_reactivate {
                tracing::debug!(target: "flowmux_agent", reason = "ended_session", changed = false, "agent decision");
                return None;
            }
            runtime.ended.remove(&wait_key);
            runtime.ended_order.retain(|ended| ended != &wait_key);
        }
        if agent == "codex"
            && runtime
                .codex_turns
                .get(&(owner, session_id.to_string()))
                .and_then(|ledger| ledger.owner_pid)
                .zip(pid)
                .is_some_and(|(owner, incoming)| owner != incoming)
        {
            tracing::debug!(target: "flowmux_agent", reason = "owner_pid_mismatch", changed = false, "agent decision");
            return None;
        }
        if current.is_some_and(|presence| {
            let authoritative_other_agent = !presence.name.eq_ignore_ascii_case(&agent)
                && matches!(
                    presence.source.as_deref(),
                    Some("flowmux:hook") | Some(flowmux_core::AGENT_SOURCE_PROC)
                );
            let other_session = presence.name.eq_ignore_ascii_case(&agent)
                && presence
                    .session_id
                    .as_deref()
                    .is_some_and(|current| current != session_id);
            let live_other_pid = presence.name.eq_ignore_ascii_case(&agent)
                && presence.pid.zip(pid).is_some_and(|(current, incoming)| {
                    current != incoming && flowmux_procmon::pid_alive(current)
                });
            authoritative_other_agent || other_session || live_other_pid
        }) {
            tracing::debug!(target: "flowmux_agent", reason = "identity_mismatch", changed = false, "agent decision");
            return None;
        }
        let terminal_boundary = matches!(
            &lifecycle_event,
            AgentLifecycleEvent::TurnStopped { .. }
                | AgentLifecycleEvent::TurnFinished { .. }
                | AgentLifecycleEvent::TurnStopRequested { .. }
                | AgentLifecycleEvent::CodexTurnStopped { .. }
                | AgentLifecycleEvent::CodexTurnInterrupted { .. }
                | AgentLifecycleEvent::SessionWaitResolved { resume: false, .. }
        );
        let mut sequence_floor = runtime.boundary_seq.get(&wait_key).copied();
        if terminal_boundary {
            sequence_floor = sequence_floor.max(runtime.last_seq.get(&wait_key).copied());
        }
        if match (sequence_floor, seq) {
            (Some(floor), Some(incoming)) => incoming <= floor,
            (Some(_), None) => true,
            _ => false,
        } {
            tracing::debug!(target: "flowmux_agent", reason = "stale_sequence", changed = false, "agent decision");
            return None;
        }
        let codex_child_event = agent == "codex"
            && (matches!(
                &lifecycle_event,
                AgentLifecycleEvent::CodexSubagentStarted { .. }
                    | AgentLifecycleEvent::CodexChildProgressObserved { .. }
                    | AgentLifecycleEvent::CodexSubagentStopped { .. }
            ) || matches!(
                &lifecycle_event,
                AgentLifecycleEvent::PermissionWaitStarted {
                    scope: Some(scope),
                    ..
                } if scope.starts_with("child:")
            ));
        if let Some(seq) = seq.filter(|_| !codex_child_event) {
            runtime
                .last_seq
                .entry(wait_key.clone())
                .and_modify(|current| *current = (*current).max(seq))
                .or_insert(seq);
            if matches!(&lifecycle_event, AgentLifecycleEvent::TurnStarted { .. }) {
                runtime.boundary_seq.insert(wait_key.clone(), seq);
            }
        }
        let notification_scope = match &lifecycle_event {
            AgentLifecycleEvent::WaitStarted { item_id, .. } => {
                Some((AgentWaitScope::Tool, item_id.clone()))
            }
            AgentLifecycleEvent::PermissionWaitStarted { scope, .. } => Some((
                AgentWaitScope::Permission,
                scope
                    .clone()
                    .unwrap_or_else(|| SESSION_PERMISSION_SCOPE.into()),
            )),
            AgentLifecycleEvent::SessionWaitStarted { scope, .. } => Some((
                AgentWaitScope::Session,
                scope.clone().unwrap_or_else(|| SESSION_WAIT_SCOPE.into()),
            )),
            _ => None,
        };
        let waits_before = pending_wait_summary(runtime, &wait_key);
        let mut completed = false;
        let mut turn_finished = false;
        let turn_boundary_seq = seq;
        let mut settle_codex_after_grace = None;
        let mut completion_message = None;
        let mut turn_outcome = None;
        let mut no_transition_reason = "ledger_only";
        let decision = match lifecycle_event {
            AgentLifecycleEvent::TurnStarted {
                turn_id,
                status_text,
            } => {
                if matches!(agent.as_str(), "gemini" | "antigravity" | "cline") {
                    runtime
                        .provider_turns
                        .insert(wait_key.clone(), ProviderTurn::default());
                }
                runtime.waits.remove(&wait_key);
                if agent == "codex" {
                    clear_codex_root_permission_scopes(runtime, &wait_key);
                    let ledger = runtime
                        .codex_turns
                        .entry((owner, session_id.to_string()))
                        .or_default();
                    set_codex_ledger_owner_if_missing(ledger, pid);
                    ledger.pending_parent_stop = None;
                    if turn_id.is_some() {
                        ledger.current_parent_turn = turn_id;
                    }
                } else {
                    runtime.permission_waits.remove(&wait_key);
                    clear_permission_event_seq_for_key(runtime, &wait_key);
                    runtime.session_waits.remove(&wait_key);
                    clear_session_wait_event_seq_for_key(runtime, &wait_key);
                }
                Some((Running, None, status_text))
            }
            AgentLifecycleEvent::ProgressObserved { status_text } => {
                if let Some(turn) = runtime.provider_turns.get_mut(&wait_key) {
                    turn.stop = None;
                    turn.idle_frame = None;
                }
                if agent == "codex" {
                    if let Some(ledger) = runtime
                        .codex_turns
                        .get_mut(&(owner, session_id.to_string()))
                    {
                        let supersedes_stop = ledger.active_children.is_empty()
                            && ledger.pending_parent_stop.as_ref().is_some_and(|pending| {
                                match (pending.seq, seq) {
                                    (Some(stop), Some(incoming)) => incoming > stop,
                                    (None, Some(_)) => true,
                                    _ => false,
                                }
                            });
                        if supersedes_stop {
                            ledger.pending_parent_stop = None;
                        }
                    }
                }
                (!lifecycle_has_waits(runtime, &wait_key)).then_some((Running, None, status_text))
            }
            AgentLifecycleEvent::CodexRootProgressObserved {
                turn_id,
                status_text,
            } => {
                if agent == "codex" {
                    if let Some(ledger) = runtime
                        .codex_turns
                        .get_mut(&(owner, session_id.to_string()))
                    {
                        let supersedes_stop =
                            ledger.pending_parent_stop.as_ref().is_some_and(|pending| {
                                match (pending.seq, seq) {
                                    (Some(stop), Some(incoming)) => incoming > stop,
                                    (None, Some(_)) => true,
                                    _ => false,
                                }
                            });
                        if supersedes_stop {
                            ledger.pending_parent_stop = None;
                        }
                        ledger.current_parent_turn = Some(turn_id);
                    }
                }
                (!lifecycle_has_waits(runtime, &wait_key)).then_some((Running, None, status_text))
            }
            AgentLifecycleEvent::PermissionWaitStarted {
                reason,
                message,
                status_text,
                scope,
            } => {
                let scope = scope.unwrap_or_else(|| SESSION_PERMISSION_SCOPE.to_string());
                let newer = match (
                    runtime
                        .permission_event_seq
                        .get(&wait_key)
                        .and_then(|scopes| scopes.get::<str>(scope.as_ref()))
                        .copied(),
                    seq,
                ) {
                    (Some(current), Some(incoming)) => incoming > current,
                    (Some(_), None) => false,
                    _ => true,
                };
                if newer {
                    if let Some(seq) = seq {
                        runtime
                            .permission_event_seq
                            .entry(wait_key.clone())
                            .or_default()
                            .insert(scope.to_string(), seq);
                    }
                    record_scope_wait(
                        runtime
                            .permission_waits
                            .entry(wait_key.clone())
                            .or_default(),
                        scope,
                        reason,
                        Some(AgentWaitContext {
                            actor: flowmux_core::AgentWaitActor::User,
                            ..Default::default()
                        }),
                    );
                    Some((NeedsInput, message, status_text))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::SessionWaitStarted {
                context,
                outcome,
                reason,
                message,
                status_text,
                scope,
            } => {
                let scope = scope.unwrap_or_else(|| SESSION_WAIT_SCOPE.to_string());
                let newer = match (
                    runtime
                        .session_wait_event_seq
                        .get(&wait_key)
                        .and_then(|scopes| scopes.get::<str>(scope.as_ref()))
                        .copied(),
                    seq,
                ) {
                    (Some(current), Some(incoming)) => incoming > current,
                    (Some(_), None) => false,
                    _ => true,
                };
                if newer {
                    if let Some(seq) = seq {
                        runtime
                            .session_wait_event_seq
                            .entry(wait_key.clone())
                            .or_default()
                            .insert(scope.to_string(), seq);
                    }
                    let baseline = runtime.retry_screen_fingerprints.get(&owner).copied();
                    let scopes = runtime.session_waits.entry(wait_key.clone()).or_default();
                    let before = scopes
                        .get(&scope)
                        .map(|wait| (wait.notification_id, wait.reason));
                    record_scope_wait(scopes, scope.clone(), reason, context);
                    let wait = scopes.get_mut(&scope).unwrap();
                    if before != Some((wait.notification_id, wait.reason)) {
                        wait.screen_baseline = baseline;
                    }
                    turn_outcome = outcome;
                    Some((NeedsInput, message, status_text))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::SessionWaitResolved {
                status_text,
                resume,
                scope,
            } => {
                let scope = scope.unwrap_or_else(|| SESSION_WAIT_SCOPE.to_string());
                let newer = match (
                    runtime
                        .session_wait_event_seq
                        .get(&wait_key)
                        .and_then(|scopes| scopes.get::<str>(scope.as_ref()))
                        .copied(),
                    seq,
                ) {
                    (Some(current), Some(incoming)) => incoming > current,
                    (Some(_), None) => false,
                    _ => true,
                };
                if !newer {
                    None
                } else {
                    if let Some(seq) = seq {
                        runtime
                            .session_wait_event_seq
                            .entry(wait_key.clone())
                            .or_default()
                            .insert(scope.to_string(), seq);
                    }
                    clear_session_wait_scope(runtime, &wait_key, &scope);
                    if !resume {
                        // Ending a wait is not evidence that a failed response succeeded.
                        turn_outcome = current.and_then(|c| c.turn_outcome);
                        if let Some(seq) = seq {
                            runtime.boundary_seq.insert(wait_key.clone(), seq);
                        }
                    }
                    (!lifecycle_has_waits(runtime, &wait_key)).then_some((
                        if resume { Running } else { Idle },
                        None,
                        status_text,
                    ))
                }
            }
            AgentLifecycleEvent::ToolBatchFinished { status_text } => {
                let scope = SESSION_PERMISSION_SCOPE;
                let newer = match (
                    runtime
                        .permission_event_seq
                        .get(&wait_key)
                        .and_then(|scopes| scopes.get::<str>(scope.as_ref()))
                        .copied(),
                    seq,
                ) {
                    (Some(current), Some(incoming)) => incoming > current,
                    (Some(_), None) => false,
                    _ => true,
                };
                if newer {
                    if let Some(seq) = seq {
                        runtime
                            .permission_event_seq
                            .entry(wait_key.clone())
                            .or_default()
                            .insert(scope.to_string(), seq);
                    }
                    clear_permission_scope(runtime, &wait_key, SESSION_PERMISSION_SCOPE);
                    (!lifecycle_has_waits(runtime, &wait_key)).then_some((
                        Running,
                        None,
                        status_text,
                    ))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::WaitStarted {
                reason,
                item_id,
                message,
                status_text,
            } => {
                let waits = runtime.waits.entry(wait_key.clone()).or_default();
                if let std::collections::hash_map::Entry::Vacant(wait) = waits.entry(item_id) {
                    wait.insert(Some(PendingAgentWait::new(reason)));
                    Some((NeedsInput, message, status_text))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::WaitResolved { item_id } => {
                let waits = runtime.waits.entry(wait_key.clone()).or_default();
                let resolved_active = waits.insert(item_id, None).flatten().is_some();
                if resolved_active && !lifecycle_has_waits(runtime, &wait_key) {
                    Some((Running, None, "Working".into()))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::TurnStopRequested {
                message,
                status_text,
            } if matches!(agent.as_str(), "gemini" | "antigravity") => {
                if current
                    .as_ref()
                    .is_some_and(|c| c.turn_outcome == Some(AgentTurnOutcome::Completed))
                    && !runtime.provider_turns.contains_key(&wait_key)
                {
                    None
                } else {
                    let turn = runtime.provider_turns.entry(wait_key.clone()).or_default();
                    turn.stop = Some((message, status_text));
                    turn.idle_frame = None;
                    (!lifecycle_has_waits(runtime, &wait_key)).then_some((
                        Running,
                        None,
                        "Finishing response".into(),
                    ))
                }
            }
            AgentLifecycleEvent::TurnFinished {
                outcome,
                message,
                status_text,
            } if agent != "codex" => {
                turn_finished = true;
                completed = outcome == AgentTurnOutcome::Completed;
                turn_outcome = Some(outcome);
                completion_message = message.clone();
                Some((Idle, message, status_text))
            }
            AgentLifecycleEvent::TurnStopped {
                message,
                status_text,
            } if agent != "codex" => {
                turn_finished = true;
                completed = true;
                turn_outcome = Some(AgentTurnOutcome::Completed);
                completion_message = message.clone();
                Some((Idle, message, status_text))
            }
            AgentLifecycleEvent::CodexSubagentStarted { agent_id, turn_id }
            | AgentLifecycleEvent::CodexChildProgressObserved { agent_id, turn_id }
                if agent == "codex" =>
            {
                let child_key = (agent_id.clone(), turn_id.clone());
                let has_waits = lifecycle_has_waits(runtime, &wait_key);
                let ledger = runtime
                    .codex_turns
                    .entry((owner, session_id.to_string()))
                    .or_default();
                set_codex_ledger_owner_if_missing(ledger, pid);
                let newer = seq.is_none_or(|incoming| {
                    ledger
                        .child_event_seq
                        .get(&child_key)
                        .is_none_or(|current| incoming > *current)
                        && ledger
                            .child_agent_event_seq
                            .get(&agent_id)
                            .is_none_or(|current| incoming > *current)
                });
                if !newer {
                    tracing::debug!(target: "flowmux_agent", reason = "stale_child_sequence", changed = false, "agent decision");
                    return None;
                }
                if let Some(seq) = seq {
                    ledger.child_event_seq.insert(child_key, seq);
                    ledger.child_agent_event_seq.insert(agent_id.clone(), seq);
                }
                ledger.active_children.insert(agent_id, turn_id);
                if has_waits {
                    None
                } else {
                    let count = ledger.active_children.len();
                    Some((
                        Running,
                        None,
                        format!(
                            "{count} active Codex subagent{}",
                            if count == 1 { "" } else { "s" }
                        ),
                    ))
                }
            }
            AgentLifecycleEvent::CodexSubagentStopped { agent_id, turn_id } if agent == "codex" => {
                let child_key = (agent_id.clone(), turn_id.clone());
                let (remaining_children, pending) = {
                    let ledger = runtime
                        .codex_turns
                        .entry((owner, session_id.to_string()))
                        .or_default();
                    set_codex_ledger_owner_if_missing(ledger, pid);
                    let newer = seq.is_none_or(|incoming| {
                        ledger
                            .child_event_seq
                            .get(&child_key)
                            .is_none_or(|current| incoming > *current)
                            && ledger
                                .child_agent_event_seq
                                .get(&agent_id)
                                .is_none_or(|current| incoming > *current)
                    });
                    if !newer {
                        tracing::debug!(target: "flowmux_agent", reason = "stale_child_sequence", changed = false, "agent decision");
                        return None;
                    }
                    if let Some(seq) = seq {
                        ledger.child_event_seq.insert(child_key, seq);
                        ledger.child_agent_event_seq.insert(agent_id.clone(), seq);
                    }
                    let matches_current_turn = ledger
                        .active_children
                        .get(&agent_id)
                        .is_some_and(|active_turn| active_turn == &turn_id);
                    if !matches_current_turn {
                        tracing::debug!(target: "flowmux_agent", reason = "child_turn_mismatch", changed = false, "agent decision");
                        return None;
                    }
                    ledger.active_children.remove(&agent_id);
                    let remaining = ledger.active_children.len();
                    let pending = (remaining == 0)
                        .then(|| ledger.pending_parent_stop.clone())
                        .flatten();
                    (remaining, pending)
                };
                clear_permission_scope_if_newer(
                    runtime,
                    &wait_key,
                    &format!("child:{agent_id}:{turn_id}"),
                    seq,
                );
                if let Some(pending) = pending {
                    let ledger = runtime
                        .codex_turns
                        .get_mut(&(owner, session_id.to_string()))
                        .expect("Codex ledger remains present");
                    if ledger
                        .settled_parent_turns
                        .iter()
                        .any(|settled| settled == &pending.turn_id)
                    {
                        None
                    } else {
                        settle_codex_after_grace = Some(CodexGraceSettlement {
                            turn_id: pending.turn_id,
                            stop_seq: pending.seq,
                        });
                        (!lifecycle_has_waits(runtime, &wait_key)).then_some((
                            Running,
                            None,
                            "Finishing Codex turn".into(),
                        ))
                    }
                } else if lifecycle_has_waits(runtime, &wait_key) {
                    None
                } else if remaining_children > 0 {
                    Some((
                        Running,
                        None,
                        format!(
                            "{remaining_children} active Codex subagent{}",
                            if remaining_children == 1 { "" } else { "s" }
                        ),
                    ))
                } else {
                    Some((Running, None, "Working".into()))
                }
            }
            AgentLifecycleEvent::CodexTurnStopped {
                turn_id,
                message,
                status_text,
                stop_hook_active,
            } if agent == "codex" => {
                let ledger_key = (owner, session_id.to_string());
                let (superseded, already_settled, child_count) = {
                    let ledger = runtime.codex_turns.entry(ledger_key.clone()).or_default();
                    set_codex_ledger_owner_if_missing(ledger, pid);
                    (
                        ledger
                            .current_parent_turn
                            .as_ref()
                            .is_some_and(|current| current != &turn_id),
                        ledger
                            .settled_parent_turns
                            .iter()
                            .any(|settled| settled == &turn_id),
                        ledger.active_children.len(),
                    )
                };
                if superseded || (already_settled && !stop_hook_active) {
                    no_transition_reason = "stale_or_settled_turn";
                    None
                } else {
                    clear_permission_scope_if_newer(
                        runtime,
                        &wait_key,
                        &format!("root:{turn_id}"),
                        seq,
                    );
                    let has_waits = lifecycle_has_waits(runtime, &wait_key);
                    let ledger = runtime
                        .codex_turns
                        .get_mut(&ledger_key)
                        .expect("Codex ledger remains present");
                    if stop_hook_active {
                        ledger
                            .settled_parent_turns
                            .retain(|settled| settled != &turn_id);
                    }
                    ledger.pending_parent_stop = Some(PendingCodexStop {
                        turn_id: turn_id.clone(),
                        message,
                        status_text,
                        notify_completion: !already_settled,
                        outcome: AgentTurnOutcome::Completed,
                        seq,
                    });
                    if child_count == 0 {
                        settle_codex_after_grace = Some(CodexGraceSettlement {
                            turn_id,
                            stop_seq: seq,
                        });
                        (!has_waits).then_some((Running, None, "Finishing Codex turn".into()))
                    } else if has_waits {
                        None
                    } else {
                        Some((
                            Running,
                            None,
                            format!(
                                "{child_count} active Codex subagent{}",
                                if child_count == 1 { "" } else { "s" }
                            ),
                        ))
                    }
                }
            }
            AgentLifecycleEvent::CodexTurnInterrupted {
                turn_id,
                status_text,
            } if agent == "codex" => {
                let ledger_key = (owner, session_id.to_string());
                let (superseded, already_settled, child_count) = {
                    let ledger = runtime.codex_turns.entry(ledger_key.clone()).or_default();
                    set_codex_ledger_owner_if_missing(ledger, pid);
                    (
                        ledger
                            .current_parent_turn
                            .as_ref()
                            .is_some_and(|current| current != &turn_id),
                        ledger
                            .settled_parent_turns
                            .iter()
                            .any(|settled| settled == &turn_id),
                        ledger.active_children.len(),
                    )
                };
                if superseded || already_settled {
                    no_transition_reason = "stale_or_settled_turn";
                    None
                } else {
                    clear_permission_scope_if_newer(
                        runtime,
                        &wait_key,
                        &format!("root:{turn_id}"),
                        seq,
                    );
                    let has_waits = lifecycle_has_waits(runtime, &wait_key);
                    let ledger = runtime
                        .codex_turns
                        .get_mut(&ledger_key)
                        .expect("Codex ledger remains present");
                    if child_count == 0 {
                        ledger.pending_parent_stop = Some(PendingCodexStop {
                            turn_id: turn_id.clone(),
                            message: None,
                            status_text,
                            notify_completion: false,
                            outcome: AgentTurnOutcome::Canceled,
                            seq,
                        });
                        settle_codex_after_grace = Some(CodexGraceSettlement {
                            turn_id,
                            stop_seq: seq,
                        });
                        (!has_waits).then_some((Running, None, "Finishing interrupted turn".into()))
                    } else {
                        ledger.pending_parent_stop = Some(PendingCodexStop {
                            turn_id,
                            message: None,
                            status_text,
                            notify_completion: false,
                            outcome: AgentTurnOutcome::Canceled,
                            seq,
                        });
                        if has_waits {
                            None
                        } else {
                            Some((
                                Running,
                                None,
                                format!(
                                    "{child_count} active Codex subagent{}",
                                    if child_count == 1 { "" } else { "s" }
                                ),
                            ))
                        }
                    }
                }
            }
            _ => None,
        };

        if turn_finished {
            runtime.provider_turns.remove(&wait_key);
            // A root boundary is session-wide, but Codex shares the session id
            // with child threads. Preserve child waits while parent completion
            // is deferred; clear them only at actual turn settlement.
            runtime.waits.remove(&wait_key);
            runtime.permission_waits.remove(&wait_key);
            clear_permission_event_seq_for_key(runtime, &wait_key);
            runtime.session_waits.remove(&wait_key);
            clear_session_wait_event_seq_for_key(runtime, &wait_key);
            if let Some(seq) = turn_boundary_seq {
                runtime.boundary_seq.insert(wait_key.clone(), seq);
            }
        }

        let pending_waits = pending_wait_summary(runtime, &wait_key);
        // A partial resolution changes the visible reasons even while activity
        // stays blocked. Duplicate deliveries leave both facts and UI untouched.
        let decision = decision.or_else(|| {
            (pending_waits != waits_before && !pending_waits.is_empty()).then(|| {
                (
                    NeedsInput,
                    current.and_then(|p| p.message.clone()),
                    current
                        .and_then(|p| p.custom_status.clone())
                        .unwrap_or_else(|| "Waiting for input".into()),
                )
            })
        });
        let Some((activity, message, custom_status)) = decision else {
            let reason = if lifecycle_has_waits(runtime, &wait_key) {
                "pending_waits"
            } else {
                no_transition_reason
            };
            tracing::debug!(target: "flowmux_agent", reason, changed = false,
                deferred = settle_codex_after_grace.is_some(), "agent decision");
            return Some(LifecycleTransition {
                settle_codex_after_grace,
                ..LifecycleTransition::default()
            });
        };
        let report_seq = if seq
            .zip(current.and_then(|located| located.seq))
            .is_some_and(|(incoming, current)| incoming <= current)
        {
            None
        } else {
            seq
        };
        let report = AgentStatusReport {
            name: agent,
            status: None,
            activity: Some(activity),
            pid,
            source: Some("flowmux:hook".into()),
            seq: report_seq,
            message,
            custom_status: Some(custom_status),
            pending_waits: Some(pending_waits),
            turn_outcome,
            session_id: Some(session_id.to_string()),
            session_name: None,
            messaging_socket: None,
        };
        Some(LifecycleTransition {
            report: Some(report),
            notification_scope,
            completed,
            completion_message,
            settle_codex_after_grace,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn settle_codex(
        &mut self,
        owner: Owner,
        pid: Option<u32>,
        seq: Option<u64>,
        session_id: &str,
        turn_id: &str,
        current: Option<&AgentPresence>,
    ) -> Option<LifecycleTransition> {
        use flowmux_core::AgentActivity::Idle;
        let runtime = self;
        let wait_key = (owner, "codex".to_string(), session_id.to_string());
        if runtime.ended.contains_key(&wait_key) {
            tracing::debug!(target: "flowmux_agent", reason = "ended_session", changed = false, "agent decision");
            return None;
        }
        if !current.is_some_and(|located| {
            located.name.eq_ignore_ascii_case("codex")
                && located.session_id.as_deref() == Some(session_id)
                && located
                    .pid
                    .zip(pid)
                    .is_none_or(|(current, incoming)| current == incoming)
        }) {
            tracing::debug!(target: "flowmux_agent", reason = "identity_mismatch", changed = false, "agent decision");
            return None;
        }
        let pending = {
            let Some(ledger) = runtime
                .codex_turns
                .get_mut(&(owner, session_id.to_string()))
            else {
                tracing::debug!(target: "flowmux_agent", reason = "no_turn_ledger", changed = false, "agent decision");
                return None;
            };
            if ledger
                .owner_pid
                .zip(pid)
                .is_some_and(|(owner, incoming)| owner != incoming)
                || !ledger.active_children.is_empty()
                || ledger
                    .pending_parent_stop
                    .as_ref()
                    .is_none_or(|pending| pending.turn_id != turn_id || pending.seq != seq)
            {
                tracing::debug!(target: "flowmux_agent", reason = "superseded_or_active_children", changed = false, "agent decision");
                return None;
            }
            let pending = ledger.pending_parent_stop.take().unwrap();
            remember_settled_codex_turn(ledger, pending.turn_id.clone());
            ledger.current_parent_turn = None;
            pending
        };
        runtime.waits.remove(&wait_key);
        runtime.permission_waits.remove(&wait_key);
        clear_permission_event_seq_for_key(runtime, &wait_key);
        runtime.session_waits.remove(&wait_key);
        clear_session_wait_event_seq_for_key(runtime, &wait_key);
        if let Some(seq) = seq {
            runtime.boundary_seq.insert(wait_key.clone(), seq);
        }
        let report = AgentStatusReport {
            pending_waits: None,
            turn_outcome: Some(pending.outcome),
            name: "codex".into(),
            status: None,
            activity: Some(Idle),
            pid,
            source: Some("flowmux:hook".into()),
            // The provisional Stop report already owns this sequence. The
            // aggregate settlement is the second phase of the same event.
            seq: None,
            message: pending.message.clone(),
            custom_status: Some(pending.status_text),
            session_id: Some(session_id.to_string()),
            session_name: None,
            messaging_socket: None,
        };
        Some(LifecycleTransition {
            report: Some(report),
            notification_scope: None,
            completed: pending.notify_completion,
            completion_message: pending
                .notify_completion
                .then_some(pending.message)
                .flatten(),
            settle_codex_after_grace: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::AgentInstanceId;

    fn apply(
        runtime: &mut AgentLifecycleRuntime<AgentInstanceId>,
        instance: AgentInstanceId,
        agent: &str,
        seq: u64,
        event: AgentLifecycleEvent,
        presence: &mut Option<AgentPresence>,
    ) -> LifecycleTransition {
        let transition = runtime
            .reduce(
                instance,
                agent,
                Some(std::process::id()),
                Some(seq),
                "session",
                event,
                presence.as_ref(),
            )
            .expect("valid lifecycle event");
        project(presence, transition.report.clone());
        transition
    }

    fn project(presence: &mut Option<AgentPresence>, report: Option<AgentStatusReport>) {
        if let Some(report) = report {
            if let Some(presence) = presence {
                assert!(presence.apply_report(report, false));
            } else {
                *presence = AgentPresence::from_report(report, false);
            }
        }
    }

    fn start(turn: &str) -> AgentLifecycleEvent {
        AgentLifecycleEvent::TurnStarted {
            turn_id: Some(turn.into()),
            status_text: "Working".into(),
        }
    }

    fn stop(turn: &str) -> AgentLifecycleEvent {
        AgentLifecycleEvent::CodexTurnStopped {
            turn_id: turn.into(),
            message: None,
            status_text: "Completed".into(),
            stop_hook_active: false,
        }
    }

    #[test]
    fn instance_ledgers_keep_children_and_waits_without_a_surface() {
        let mut runtime = AgentLifecycleRuntime::<AgentInstanceId>::default();
        let first = AgentInstanceId::new();
        let second = AgentInstanceId::new();
        let (mut a, mut b) = (None, None);
        apply(&mut runtime, first, "codex", 1, start("parent-a"), &mut a);
        apply(&mut runtime, second, "codex", 1, start("parent-b"), &mut b);
        apply(
            &mut runtime,
            first,
            "codex",
            100,
            AgentLifecycleEvent::CodexSubagentStarted {
                agent_id: "child".into(),
                turn_id: "child-turn".into(),
            },
            &mut a,
        );
        apply(
            &mut runtime,
            first,
            "codex",
            101,
            AgentLifecycleEvent::PermissionWaitStarted {
                reason: AgentWaitReason::Approval,
                scope: Some("child:child:child-turn".into()),
                message: None,
                status_text: "Approval required".into(),
            },
            &mut a,
        );
        // A child uses its own sequence and turn identity, not the parent's.
        let stopped = apply(&mut runtime, first, "codex", 2, stop("parent-a"), &mut a);
        assert!(stopped.settle_codex_after_grace.is_none());
        assert_eq!(a.as_ref().unwrap().status, AgentStatus::Blocked);
        let stopped = apply(&mut runtime, second, "codex", 2, stop("parent-b"), &mut b);
        let grace = stopped.settle_codex_after_grace.unwrap();
        let settled = runtime
            .settle_codex(
                second,
                Some(std::process::id()),
                grace.stop_seq,
                "session",
                &grace.turn_id,
                b.as_ref(),
            )
            .unwrap();
        assert!(settled.completed);
        project(&mut b, settled.report);
        assert_eq!(b.as_ref().unwrap().status, AgentStatus::Idle);
        assert_eq!(a.as_ref().unwrap().status, AgentStatus::Blocked);

        let child_stopped = apply(
            &mut runtime,
            first,
            "codex",
            102,
            AgentLifecycleEvent::CodexSubagentStopped {
                agent_id: "child".into(),
                turn_id: "child-turn".into(),
            },
            &mut a,
        );
        let stale = child_stopped.settle_codex_after_grace.unwrap();
        assert!(a.as_ref().unwrap().pending_waits.is_empty());
        apply(
            &mut runtime,
            first,
            "codex",
            3,
            AgentLifecycleEvent::CodexRootProgressObserved {
                turn_id: "parent-a".into(),
                status_text: "Working".into(),
            },
            &mut a,
        );
        assert!(runtime
            .settle_codex(
                first,
                Some(std::process::id()),
                stale.stop_seq,
                "session",
                &stale.turn_id,
                a.as_ref()
            )
            .is_none());
        assert_eq!(a.as_ref().unwrap().status, AgentStatus::Working);
        let stopped = apply(&mut runtime, first, "codex", 4, stop("parent-a"), &mut a);
        let grace = stopped.settle_codex_after_grace.unwrap();
        let settled = runtime
            .settle_codex(
                first,
                Some(std::process::id()),
                grace.stop_seq,
                "session",
                &grace.turn_id,
                a.as_ref(),
            )
            .unwrap();
        assert!(settled.completed);
        project(&mut a, settled.report);
        assert_eq!(a.as_ref().unwrap().status, AgentStatus::Idle);
    }

    #[test]
    fn rejected_identity_and_checkpoint_restore_do_not_erase_another_instance() {
        let mut runtime = AgentLifecycleRuntime::<AgentInstanceId>::default();
        let first = AgentInstanceId::new();
        let second = AgentInstanceId::new();
        let (mut a, mut b) = (None, None);
        apply(&mut runtime, first, "claude", 1, start("first"), &mut a);
        apply(&mut runtime, second, "claude", 1, start("second"), &mut b);
        let key = (first, "claude".to_string(), "session".to_string());
        let checkpoint = LifecycleCheckpoint::capture(&runtime, &key);
        for (instance, presence) in [(first, &mut a), (second, &mut b)] {
            apply(
                &mut runtime,
                instance,
                "claude",
                2,
                AgentLifecycleEvent::WaitStarted {
                    reason: AgentWaitReason::Question,
                    item_id: "same-tool-id".into(),
                    message: None,
                    status_text: "Question".into(),
                },
                presence,
            );
        }
        checkpoint.restore(&mut runtime);
        assert!(!lifecycle_has_waits(&runtime, &key));
        let other_key = (second, "claude".to_string(), "session".to_string());
        assert!(lifecycle_has_waits(&runtime, &other_key));
        assert!(runtime
            .reduce(
                second,
                "claude",
                Some(std::process::id()),
                Some(50),
                "different-session",
                start("wrong"),
                b.as_ref()
            )
            .is_none());
        assert_eq!(runtime.last_seq[&other_key], 2);
        assert!(lifecycle_has_waits(&runtime, &other_key));
        apply(
            &mut runtime,
            second,
            "claude",
            3,
            AgentLifecycleEvent::WaitResolved {
                item_id: "same-tool-id".into(),
            },
            &mut b,
        );
        apply(
            &mut runtime,
            second,
            "claude",
            4,
            AgentLifecycleEvent::WaitStarted {
                reason: AgentWaitReason::Question,
                item_id: "same-tool-id".into(),
                message: None,
                status_text: "Delayed question".into(),
            },
            &mut b,
        );
        assert!(!lifecycle_has_waits(&runtime, &other_key));
        assert_eq!(b.as_ref().unwrap().status, AgentStatus::Working);
        clear_agent_lifecycle_runtime(&mut runtime, second, Some("claude"), Some("session"), None);
        remember_ended_agent_lifecycle(
            &mut runtime,
            other_key.clone(),
            Some(std::process::id()),
            Some(5),
            false,
        );
        assert!(runtime
            .reduce(
                second,
                "claude",
                Some(std::process::id()),
                Some(6),
                "session",
                start("ended"),
                b.as_ref()
            )
            .is_none());
        assert!(!runtime.last_seq.contains_key(&other_key));
        assert!(runtime.last_seq.contains_key(&key));
    }

    #[tokio::test]
    async fn instance_projection_matches_existing_surface_path() {
        for agent in ["claude", "codex", "gemini"] {
            let store = StateStore::new_lazy_ephemeral(State::default());
            let workspace = store.create_workspace(None, std::env::temp_dir()).await;
            let ws = store.get_workspace(workspace).await.unwrap();
            let pane = &ws.surfaces[0].root_pane;
            let surface = pane
                .active_surface_id(pane.first_leaf_id().unwrap())
                .unwrap();
            let instance = AgentInstanceId::new();
            let mut runtime = AgentLifecycleRuntime::<AgentInstanceId>::default();
            let mut presence = None;
            for (index, event) in [
                start("turn"),
                AgentLifecycleEvent::PermissionWaitStarted {
                    reason: AgentWaitReason::Approval,
                    scope: None,
                    message: None,
                    status_text: "Approval required".into(),
                },
                AgentLifecycleEvent::ProgressObserved {
                    status_text: "Working".into(),
                },
                AgentLifecycleEvent::ToolBatchFinished {
                    status_text: "Working".into(),
                },
                if agent == "codex" {
                    stop("turn")
                } else if agent == "gemini" {
                    AgentLifecycleEvent::TurnStopRequested {
                        message: None,
                        status_text: "Completed".into(),
                    }
                } else {
                    AgentLifecycleEvent::TurnStopped {
                        message: None,
                        status_text: "Completed".into(),
                    }
                },
            ]
            .into_iter()
            .enumerate()
            {
                let seq = index as u64 + 1;
                let outcome = store
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        agent,
                        Some(std::process::id()),
                        Some(seq),
                        "session",
                        event.clone(),
                        false,
                    )
                    .await;
                let detached = apply(&mut runtime, instance, agent, seq, event, &mut presence);
                assert_eq!(outcome.completed, detached.completed);
                assert_eq!(
                    outcome.settle_codex_after_grace,
                    detached.settle_codex_after_grace
                );
                let actual = store
                    .located_agent_presence(surface)
                    .await
                    .unwrap()
                    .presence;
                let detached = presence.as_ref().unwrap();
                assert_eq!(actual.status, detached.status);
                assert_eq!(actual.activity, detached.activity);
                assert_eq!(actual.turn_outcome, detached.turn_outcome);
                assert_eq!(actual.custom_status, detached.custom_status);
                assert_eq!(actual.seq, detached.seq);
                let waits = |p: &AgentPresence| {
                    p.pending_waits
                        .iter()
                        .map(|w| (w.scope, w.reason, w.actor, w.retry, w.count))
                        .collect::<Vec<_>>()
                };
                assert_eq!(waits(&actual), waits(detached));
            }
        }
    }
}
