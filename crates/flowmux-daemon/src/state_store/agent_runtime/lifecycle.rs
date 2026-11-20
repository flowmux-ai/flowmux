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
    /// A ready screen settles display state, never whole-provider completion.
    pub(crate) fn provider_screen_settlement(
        &mut self,
        owner: Owner,
        presence: &AgentPresence,
        screen: &str,
        title: Option<&str>,
    ) -> Option<(Option<u64>, AgentLifecycleEvent)> {
        let session = presence.session_id.as_deref()?;
        let key = (owner, presence.name.clone(), session.to_string());
        if presence.source.as_deref() != Some("flowmux:hook") || lifecycle_has_waits(self, &key) {
            return None;
        }
        let turn = self.provider_turns.get_mut(&key)?;
        let status = detect_agent_status_from_signals(Some(screen), title);
        if status == Some(AgentStatus::Working) {
            turn.saw_working |= detect_agent_progress_text(Some(screen)).is_some();
            turn.idle_frame = None;
            return None;
        }
        let ready = screen
            .lines()
            .rev()
            .filter(|line| !line.trim().is_empty())
            .take(12)
            .any(|line| {
                let line = line.to_ascii_lowercase();
                line.contains("type your message")
                    || line.contains("ask anything")
                    || (presence.name == "antigravity" && line.trim() == "❯")
            });
        let observed = provider_terminal_outcome(&presence.name, screen);
        if status != Some(AgentStatus::Idle)
            || !ready
            || (turn.stop.is_none() && !turn.saw_working && observed.is_none())
        {
            turn.idle_frame = None;
            return None;
        }
        let fingerprint = agent_screen_fingerprint(Some(screen), title, false);
        match turn.idle_frame {
            Some((previous, since)) if previous == fingerprint => {
                if since.elapsed() < std::time::Duration::from_millis(600) {
                    return None;
                }
            }
            _ => {
                turn.idle_frame = Some((fingerprint, tokio::time::Instant::now()));
                return None;
            }
        }
        let outcome = observed.unwrap_or(if turn.stop.is_some() {
            AgentTurnOutcome::Completed
        } else {
            // shortcut: silent cancellation and lost Stop are indistinguishable until providers emit a terminal hook.
            AgentTurnOutcome::Unknown
        });
        let (message, status_text) = if outcome == AgentTurnOutcome::Completed {
            turn.stop.clone().expect("completed turns have a stop")
        } else {
            (None, outcome.status_text().to_string())
        };
        let seq = self.last_seq.get(&key).and_then(|seq| seq.checked_add(1));
        Some((
            seq,
            AgentLifecycleEvent::TurnFinished {
                outcome,
                message,
                status_text,
            },
        ))
    }

    pub(crate) fn has_provider_turn(&self, owner: Owner, presence: &AgentPresence) -> bool {
        presence.session_id.as_ref().is_some_and(|session| {
            self.provider_turns
                .contains_key(&(owner, presence.name.clone(), session.clone()))
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn native_session(
        &mut self,
        owner: Owner,
        agent: &str,
        pid: u32,
        session: &str,
        seq: u64,
        started: bool,
        current: Option<&AgentPresence>,
    ) -> bool {
        let key = (owner, agent.to_ascii_lowercase(), session.to_string());
        let floor = self
            .last_seq
            .get(&key)
            .copied()
            .into_iter()
            .chain(self.ended.get(&key).and_then(|ended| ended.seq))
            .chain(current.and_then(|presence| presence.seq))
            .max();
        if floor.is_some_and(|floor| seq <= floor) {
            return false;
        }
        let same_session = current.is_some_and(|presence| {
            presence.name.eq_ignore_ascii_case(agent)
                && presence.pid == Some(pid)
                && presence.session_id.as_deref() == Some(session)
        });
        // Repeated starts must not erase waits or in-flight child work.
        if started && same_session || !started && !same_session {
            return false;
        }
        if started {
            if let Some(previous) = current.and_then(|presence| {
                presence.session_id.as_ref().map(|session| {
                    (
                        (owner, presence.name.to_ascii_lowercase(), session.clone()),
                        presence.pid,
                        presence.seq,
                    )
                })
            }) {
                remember_ended_agent_lifecycle(self, previous.0, previous.1, previous.2, false);
            }
            clear_agent_lifecycle_runtime(self, owner, None, None, None);
            self.ended.remove(&key);
            self.ended_order.retain(|ended| ended != &key);
            self.last_seq.insert(key.clone(), seq);
            self.boundary_seq.insert(key, seq);
        } else {
            clear_agent_lifecycle_runtime(self, owner, Some(agent), Some(session), Some(pid));
            remember_ended_agent_lifecycle(self, key, Some(pid), Some(seq), false);
        }
        true
    }

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
        if agent == "codex" {
            let root_turn = match &lifecycle_event {
                AgentLifecycleEvent::TurnStarted { turn_id, .. } => turn_id.as_deref(),
                AgentLifecycleEvent::CodexRootProgressObserved { turn_id, .. } => {
                    Some(turn_id.as_str())
                }
                _ => None,
            };
            let settled = root_turn.is_some_and(|turn| {
                runtime
                    .codex_turns
                    .get(&(owner, session_id.to_string()))
                    .is_some_and(|ledger| {
                        ledger.current_parent_turn.as_deref() != Some(turn)
                            && ledger.settled_parent_turns.iter().any(|id| id == turn)
                    })
            });
            // Hook timestamps cannot reopen a settled turn; an active Stop retry can.
            // shortcut: Only retained IDs are deduplicated; revalidate provider work before retirement.
            if settled {
                tracing::debug!(target: "flowmux_agent", reason = "settled_root_event", changed = false, "agent decision");
                return None;
            }
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
                    settle_codex_after_grace = Some(CodexGraceSettlement {
                        turn_id: pending.turn_id,
                        stop_seq: pending.seq,
                    });
                    (!lifecycle_has_waits(runtime, &wait_key)).then_some((
                        Running,
                        None,
                        "Finishing Codex turn".into(),
                    ))
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
                let (superseded, already_settled, resumed, child_count) = {
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
                        ledger.current_parent_turn.as_ref() == Some(&turn_id),
                        ledger.active_children.len(),
                    )
                };
                if superseded || (already_settled && !stop_hook_active && !resumed) {
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
                    // Keep completion history when an explicit retry resumes the same ID.
                    ledger.current_parent_turn = Some(turn_id.clone());
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
                let (superseded, already_settled, resumed, child_count) = {
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
                        ledger.current_parent_turn.as_ref() == Some(&turn_id),
                        ledger.active_children.len(),
                    )
                };
                if superseded || (already_settled && !resumed) {
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
        if !waits_before.is_empty() || !pending_waits.is_empty() {
            if let Some(turn) = runtime.provider_turns.get_mut(&wait_key) {
                turn.idle_frame = None;
            }
        }
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

    #[tokio::test]
    async fn provider_ready_stability_restarts_across_permission_waits() {
        for agent in ["gemini", "antigravity"] {
            let owner = AgentInstanceId::new();
            let mut runtime = AgentLifecycleRuntime::default();
            let mut presence = None;
            apply(&mut runtime, owner, agent, 1, start("turn"), &mut presence);
            apply(
                &mut runtime,
                owner,
                agent,
                2,
                AgentLifecycleEvent::TurnStopRequested {
                    message: None,
                    status_text: "Completed".into(),
                },
                &mut presence,
            );
            assert!(runtime
                .provider_screen_settlement(
                    owner,
                    presence.as_ref().unwrap(),
                    "Type your message",
                    None
                )
                .is_none());
            let key = (owner, agent.to_string(), "session".to_string());
            runtime
                .provider_turns
                .get_mut(&key)
                .unwrap()
                .idle_frame
                .as_mut()
                .unwrap()
                .1 -= std::time::Duration::from_secs(1);
            apply(
                &mut runtime,
                owner,
                agent,
                3,
                AgentLifecycleEvent::PermissionWaitStarted {
                    reason: AgentWaitReason::Approval,
                    scope: None,
                    message: None,
                    status_text: "Approval required".into(),
                },
                &mut presence,
            );
            assert!(runtime
                .provider_screen_settlement(
                    owner,
                    presence.as_ref().unwrap(),
                    "Type your message",
                    None
                )
                .is_none());
            apply(
                &mut runtime,
                owner,
                agent,
                4,
                AgentLifecycleEvent::ToolBatchFinished {
                    status_text: "Working".into(),
                },
                &mut presence,
            );
            assert!(
                runtime
                    .provider_screen_settlement(
                        owner,
                        presence.as_ref().unwrap(),
                        "Type your message",
                        None
                    )
                    .is_none(),
                "{agent}: a wait must invalidate pre-wait screen stability"
            );
            runtime
                .provider_turns
                .get_mut(&key)
                .unwrap()
                .idle_frame
                .as_mut()
                .unwrap()
                .1 -= std::time::Duration::from_secs(1);
            assert!(runtime
                .provider_screen_settlement(
                    owner,
                    presence.as_ref().unwrap(),
                    "Type your message",
                    None
                )
                .is_some());
        }
    }

    fn settle(
        runtime: &mut AgentLifecycleRuntime<AgentInstanceId>,
        instance: AgentInstanceId,
        grace: CodexGraceSettlement,
        presence: &mut Option<AgentPresence>,
    ) -> LifecycleTransition {
        let transition = runtime
            .settle_codex(
                instance,
                Some(std::process::id()),
                grace.stop_seq,
                "session",
                &grace.turn_id,
                presence.as_ref(),
            )
            .expect("matching Stop settles");
        project(presence, transition.report.clone());
        transition
    }

    #[test]
    fn settled_codex_root_events_preserve_completion_and_newer_waits() {
        let mut runtime = AgentLifecycleRuntime::<AgentInstanceId>::default();
        let instance = AgentInstanceId::new();
        let mut presence = None;
        apply(
            &mut runtime,
            instance,
            "codex",
            10,
            start("old"),
            &mut presence,
        );
        let stopped = apply(
            &mut runtime,
            instance,
            "codex",
            20,
            stop("old"),
            &mut presence,
        );
        assert!(
            settle(
                &mut runtime,
                instance,
                stopped.settle_codex_after_grace.unwrap(),
                &mut presence,
            )
            .completed
        );
        let key = (instance, "codex".to_string(), "session".to_string());
        for with_new_turn in [false, true] {
            if with_new_turn {
                apply(
                    &mut runtime,
                    instance,
                    "codex",
                    30,
                    start("new"),
                    &mut presence,
                );
                apply(
                    &mut runtime,
                    instance,
                    "codex",
                    40,
                    AgentLifecycleEvent::PermissionWaitStarted {
                        reason: AgentWaitReason::Approval,
                        scope: Some("root:new".into()),
                        message: None,
                        status_text: "Approval required".into(),
                    },
                    &mut presence,
                );
            }
            let before = presence.clone();
            let last_seq = runtime.last_seq[&key];
            let boundary_seq = runtime.boundary_seq[&key];
            for event in [
                start("old"),
                AgentLifecycleEvent::CodexRootProgressObserved {
                    turn_id: "old".into(),
                    status_text: "Delayed progress".into(),
                },
            ] {
                assert!(runtime
                    .reduce(
                        instance,
                        "codex",
                        Some(std::process::id()),
                        Some(1000),
                        "session",
                        event,
                        presence.as_ref(),
                    )
                    .is_none());
                assert_eq!(presence, before);
                assert_eq!(runtime.last_seq[&key], last_seq);
                assert_eq!(runtime.boundary_seq[&key], boundary_seq);
            }
        }
        assert_eq!(presence.as_ref().unwrap().pending_waits.len(), 1);
        let stopped = apply(
            &mut runtime,
            instance,
            "codex",
            50,
            stop("new"),
            &mut presence,
        );
        assert!(
            settle(
                &mut runtime,
                instance,
                stopped.settle_codex_after_grace.unwrap(),
                &mut presence,
            )
            .completed
        );
        assert_eq!(presence.unwrap().status, AgentStatus::Idle);
    }

    fn root_progress(turn: &str) -> AgentLifecycleEvent {
        AgentLifecycleEvent::CodexRootProgressObserved {
            turn_id: turn.into(),
            status_text: "Working".into(),
        }
    }

    #[test]
    fn codex_vetoed_stop_continuation_within_grace_stays_working() {
        let mut runtime = AgentLifecycleRuntime::<AgentInstanceId>::default();
        let instance = AgentInstanceId::new();
        let mut presence = None;
        apply(
            &mut runtime,
            instance,
            "codex",
            10,
            start("t1"),
            &mut presence,
        );
        let stopped = apply(
            &mut runtime,
            instance,
            "codex",
            20,
            stop("t1"),
            &mut presence,
        );
        let grace = stopped.settle_codex_after_grace.unwrap();
        // Continuation tool hook lands before the 250 ms grace elapses.
        apply(
            &mut runtime,
            instance,
            "codex",
            30,
            root_progress("t1"),
            &mut presence,
        );
        assert!(runtime
            .settle_codex(
                instance,
                Some(std::process::id()),
                grace.stop_seq,
                "session",
                &grace.turn_id,
                presence.as_ref(),
            )
            .is_none());
        assert_eq!(presence.as_ref().unwrap().status, AgentStatus::Working);
        let retried = apply(
            &mut runtime,
            instance,
            "codex",
            40,
            AgentLifecycleEvent::CodexTurnStopped {
                turn_id: "t1".into(),
                message: None,
                status_text: "Completed".into(),
                stop_hook_active: true,
            },
            &mut presence,
        );
        let settled = settle(
            &mut runtime,
            instance,
            retried.settle_codex_after_grace.unwrap(),
            &mut presence,
        );
        assert!(settled.completed);
        assert_eq!(presence.unwrap().status, AgentStatus::Idle);
    }

    // Pins today's behaviour. Sequence A is a known display gap pending a decision.
    #[test]
    fn codex_vetoed_stop_continuation_after_grace_shows_idle_until_retry_stop() {
        let mut runtime = AgentLifecycleRuntime::<AgentInstanceId>::default();
        let instance = AgentInstanceId::new();
        let mut presence = None;
        apply(
            &mut runtime,
            instance,
            "codex",
            10,
            start("t1"),
            &mut presence,
        );
        let stopped = apply(
            &mut runtime,
            instance,
            "codex",
            20,
            stop("t1"),
            &mut presence,
        );
        // Stop(t1, stop_hook_active=false) settles and notifies although another
        // hook vetoed it and Codex keeps working on t1.
        let first = settle(
            &mut runtime,
            instance,
            stopped.settle_codex_after_grace.unwrap(),
            &mut presence,
        );
        assert!(first.completed);
        let idle = presence.clone();
        assert_eq!(idle.as_ref().unwrap().status, AgentStatus::Idle);
        // Continuation PreToolUse/PostToolUse(t1) are rejected; seq is newer than Stop.
        for seq in [30, 31] {
            assert!(runtime
                .reduce(
                    instance,
                    "codex",
                    Some(std::process::id()),
                    Some(seq),
                    "session",
                    root_progress("t1"),
                    presence.as_ref(),
                )
                .is_none());
            assert_eq!(presence, idle);
        }
        // A turn-scoped approval in the continuation is still accepted.
        apply(
            &mut runtime,
            instance,
            "codex",
            32,
            AgentLifecycleEvent::PermissionWaitStarted {
                reason: AgentWaitReason::Approval,
                scope: Some("root:t1".into()),
                message: None,
                status_text: "Approval required".into(),
            },
            &mut presence,
        );
        assert_eq!(presence.as_ref().unwrap().status, AgentStatus::Blocked);
        // Only the retry Stop ends it; it does not notify a second time.
        let retried = apply(
            &mut runtime,
            instance,
            "codex",
            40,
            AgentLifecycleEvent::CodexTurnStopped {
                turn_id: "t1".into(),
                message: None,
                status_text: "Completed".into(),
                stop_hook_active: true,
            },
            &mut presence,
        );
        let last = settle(
            &mut runtime,
            instance,
            retried.settle_codex_after_grace.unwrap(),
            &mut presence,
        );
        assert!(!last.completed);
        assert_eq!(presence.unwrap().status, AgentStatus::Idle);
    }

    #[test]
    fn codex_late_progress_after_real_completion_keeps_completion() {
        let mut runtime = AgentLifecycleRuntime::<AgentInstanceId>::default();
        let instance = AgentInstanceId::new();
        let mut presence = None;
        apply(
            &mut runtime,
            instance,
            "codex",
            10,
            start("t1"),
            &mut presence,
        );
        let stopped = apply(
            &mut runtime,
            instance,
            "codex",
            20,
            stop("t1"),
            &mut presence,
        );
        assert!(
            settle(
                &mut runtime,
                instance,
                stopped.settle_codex_after_grace.unwrap(),
                &mut presence,
            )
            .completed
        );
        let idle = presence.clone();
        // Same reducer input as the vetoed continuation above: only a later
        // Stop(stop_hook_active=true) tells the two apart, and B never sends one.
        assert!(runtime
            .reduce(
                instance,
                "codex",
                Some(std::process::id()),
                Some(30),
                "session",
                root_progress("t1"),
                presence.as_ref(),
            )
            .is_none());
        assert_eq!(presence, idle);
    }

    #[test]
    fn codex_explicit_retry_keeps_completion_dedup_across_progress_and_children() {
        let mut runtime = AgentLifecycleRuntime::<AgentInstanceId>::default();
        let instance = AgentInstanceId::new();
        let mut presence = None;
        apply(
            &mut runtime,
            instance,
            "codex",
            10,
            start("root"),
            &mut presence,
        );
        for (seq, event) in [
            (
                11,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "reused-child".into(),
                    turn_id: "old-child-turn".into(),
                },
            ),
            (
                12,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "reused-child".into(),
                    turn_id: "old-child-turn".into(),
                },
            ),
        ] {
            apply(&mut runtime, instance, "codex", seq, event, &mut presence);
        }
        let stopped = apply(
            &mut runtime,
            instance,
            "codex",
            20,
            stop("root"),
            &mut presence,
        );
        assert!(
            settle(
                &mut runtime,
                instance,
                stopped.settle_codex_after_grace.unwrap(),
                &mut presence,
            )
            .completed
        );
        let mut retry = stop("root");
        if let AgentLifecycleEvent::CodexTurnStopped {
            stop_hook_active, ..
        } = &mut retry
        {
            *stop_hook_active = true;
        }
        apply(&mut runtime, instance, "codex", 30, retry, &mut presence);
        apply(
            &mut runtime,
            instance,
            "codex",
            31,
            AgentLifecycleEvent::CodexRootProgressObserved {
                turn_id: "root".into(),
                status_text: "Explicit retry is working".into(),
            },
            &mut presence,
        );
        assert_eq!(presence.as_ref().unwrap().status, AgentStatus::Working);
        apply(
            &mut runtime,
            instance,
            "codex",
            100,
            AgentLifecycleEvent::CodexChildProgressObserved {
                agent_id: "reused-child".into(),
                turn_id: "new-child-turn".into(),
            },
            &mut presence,
        );
        let stopped = apply(
            &mut runtime,
            instance,
            "codex",
            32,
            stop("root"),
            &mut presence,
        );
        assert!(stopped.settle_codex_after_grace.is_none());
        let stopped = apply(
            &mut runtime,
            instance,
            "codex",
            101,
            AgentLifecycleEvent::CodexSubagentStopped {
                agent_id: "reused-child".into(),
                turn_id: "new-child-turn".into(),
            },
            &mut presence,
        );
        assert!(
            !settle(
                &mut runtime,
                instance,
                stopped.settle_codex_after_grace.unwrap(),
                &mut presence,
            )
            .completed
        );
        assert_eq!(presence.unwrap().status, AgentStatus::Idle);
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
