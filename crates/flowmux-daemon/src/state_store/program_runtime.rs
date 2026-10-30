// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime-only terminal reports. Native hook ledgers remain independent.

use super::StateStore;
use flowmux_core::{
    AgentActivity, AgentPresence, AgentRetryState, AgentStatus, AgentTerminalRecord,
    AgentTerminalReport, AgentTurnOutcome, AgentWaitReason, AgentWaitScope, AgentWaitSummary, Pane,
    PaneContent, SurfaceId, SurfaceKind,
};
use flowmux_ipc::protocol::TerminalProgramSnapshot;
use flowmux_notify::program_status::{
    parse_program_status, BlockedKind, ProgramRecords, ProgramState, ProgramStatus,
    MAX_PROGRAM_STATUS_RECORDS,
};
use std::collections::HashSet;

fn terminal_in(pane: &Pane, id: SurfaceId) -> bool {
    match pane {
        Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } => surfaces.iter().any(|surface| {
            surface.id == id
                && matches!(
                    surface.kind,
                    SurfaceKind::Terminal { .. } | SurfaceKind::SshTerminal { .. }
                )
        }),
        Pane::Split { first, second, .. } => terminal_in(first, id) || terminal_in(second, id),
        _ => false,
    }
}

fn terminal_exists(state: &flowmux_state::State, id: SurfaceId) -> bool {
    state.workspaces.iter().any(|workspace| {
        workspace
            .surfaces
            .iter()
            .any(|root| terminal_in(&root.root_pane, id))
    })
}

fn validate(snapshot: &mut TerminalProgramSnapshot) -> Result<(), String> {
    if snapshot.generation.is_nil() || snapshot.owner_pid == 0 || snapshot.revision == 0 {
        return Err("invalid terminal report identity or revision".into());
    }
    if snapshot.records.len() > MAX_PROGRAM_STATUS_RECORDS {
        return Err("too many terminal program records".into());
    }
    let mut ids = HashSet::new();
    let mut canonical = Vec::with_capacity(snapshot.records.len());
    for payload in &snapshot.records {
        let Some(ProgramStatus::Report(report)) = parse_program_status(payload) else {
            return Err("invalid terminal program record".into());
        };
        if report.state == ProgramState::Clear || !ids.insert(report.id.clone()) {
            return Err("terminal snapshot contains a command or duplicate id".into());
        }
        canonical.push(report.to_payload());
    }
    snapshot.records = canonical;
    Ok(())
}

fn project(
    pane: &mut Pane,
    surface: SurfaceId,
    snapshot: Option<&TerminalProgramSnapshot>,
    visible: bool,
) {
    match pane {
        Pane::Split { first, second, .. } => {
            project(first, surface, snapshot, visible);
            project(second, surface, snapshot, visible);
        }
        Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } => {
            let Some(tab) = surfaces.iter_mut().find(|tab| tab.id == surface) else {
                return;
            };
            let mut records = ProgramRecords::default();
            for payload in snapshot.into_iter().flat_map(|snapshot| &snapshot.records) {
                if let Some(ProgramStatus::Report(report)) = parse_program_status(payload) {
                    records.apply(report);
                }
            }
            if records.records().next().is_none() {
                if tab
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.source.as_deref() == Some("flowmux:program"))
                {
                    tab.agent = None;
                } else if let Some(agent) = &mut tab.agent {
                    agent.terminal_report = None;
                }
                return;
            }
            let agent = tab.agent.get_or_insert_with(|| {
                let name = records.app(None).unwrap_or("Program");
                let mut agent = AgentPresence::new(name, AgentActivity::Idle, None);
                agent.source = Some("flowmux:program".into());
                agent
            });
            let old = agent.terminal_report.as_ref();
            let now = chrono::Utc::now();
            let mut details = Vec::new();
            let mut waits: Vec<AgentWaitSummary> = Vec::new();
            let mut selected = (0, AgentStatus::Idle, None, "Idle");
            for record in records.records() {
                let (rank, status, outcome, label, state) = match record.state {
                    ProgramState::Blocked => (
                        5,
                        AgentStatus::Blocked,
                        None,
                        "Waiting for input",
                        "blocked",
                    ),
                    ProgramState::Error => (
                        4,
                        AgentStatus::Idle,
                        Some(AgentTurnOutcome::Failed),
                        "Error; review required",
                        "error",
                    ),
                    ProgramState::Working => (3, AgentStatus::Working, None, "Working", "working"),
                    ProgramState::Done => (
                        2,
                        AgentStatus::Idle,
                        Some(AgentTurnOutcome::Unknown),
                        "Finished; review result",
                        "done",
                    ),
                    ProgramState::Idle => (1, AgentStatus::Idle, None, "Idle", "idle"),
                    ProgramState::Clear => continue,
                };
                if rank > selected.0 {
                    selected = (rank, status, outcome, label);
                }
                let reason = (record.state == ProgramState::Blocked).then_some(match record.kind {
                    Some(BlockedKind::Permission) => AgentWaitReason::Approval,
                    Some(BlockedKind::Question) => AgentWaitReason::Question,
                    Some(BlockedKind::Auth) => AgentWaitReason::Authentication,
                    None => AgentWaitReason::Unknown,
                });
                if let Some(reason) = reason {
                    if let Some(wait) = waits.iter_mut().find(|wait| wait.reason == reason) {
                        wait.count += 1;
                    } else {
                        waits.push(AgentWaitSummary {
                            scope: AgentWaitScope::Session,
                            reason,
                            actor: reason.actor(),
                            retry: AgentRetryState::Unknown,
                            context_evidence: None,
                            count: 1,
                            since: old
                                .and_then(|old| {
                                    old.pending_waits.iter().find(|wait| wait.reason == reason)
                                })
                                .map_or(now, |wait| wait.since),
                        });
                    }
                }
                details.push(AgentTerminalRecord {
                    id: record.id.clone(),
                    app: records.app(record.id.as_deref()).map(str::to_owned),
                    state: state.into(),
                    progress: record.progress,
                    reason,
                });
            }
            waits.sort_by_key(|wait| wait.reason as u8);
            let same_state =
                old.is_some_and(|old| old.status == selected.1 && old.outcome == selected.2);
            let attention_state = match selected.0 {
                5 => "blocked",
                4 => "error",
                2 => "done",
                _ => "",
            };
            let new_attention = details
                .iter()
                .filter(|record| record.state == attention_state)
                .any(|record| {
                    old.is_none_or(|old| {
                        !old.records.iter().any(|previous| {
                            previous.id == record.id
                                && previous.state == record.state
                                && previous.reason == record.reason
                        })
                    })
                });
            let seen = visible
                || if same_state && !new_attention {
                    old.unwrap().seen
                } else {
                    selected.1 != AgentStatus::Blocked && selected.2.is_none()
                };
            let waits_label = (waits.len() == 1).then(|| waits[0].status_text()).flatten();
            agent.terminal_report = Some(AgentTerminalReport {
                status: selected.1,
                since: if same_state { old.unwrap().since } else { now },
                pending_waits: waits,
                outcome: selected.2,
                seen,
                label: waits_label.unwrap_or(selected.3).into(),
                records: details,
            });
        }
        _ => {}
    }
}

fn project_state(
    state: &mut flowmux_state::State,
    surface: SurfaceId,
    snapshot: Option<&TerminalProgramSnapshot>,
    visible: bool,
) {
    for workspace in &mut state.workspaces {
        for root in &mut workspace.surfaces {
            project(&mut root.root_pane, surface, snapshot, visible);
        }
    }
}

impl StateStore {
    /// The GUI must first match the wrapper PID and generation to its live PTY.
    /// State validation and revision admission are atomic with surface removal.
    pub async fn report_terminal_program_status(
        &self,
        surface: SurfaceId,
        snapshot: TerminalProgramSnapshot,
    ) -> Result<bool, String> {
        self.report_terminal_program_status_with_visibility(surface, snapshot, false)
            .await
    }

    pub async fn report_terminal_program_status_with_visibility(
        &self,
        surface: SurfaceId,
        mut snapshot: TerminalProgramSnapshot,
        visible: bool,
    ) -> Result<bool, String> {
        validate(&mut snapshot)?;
        let mut programs = self.agents.programs.lock().await;
        let mut state = self.inner.lock().await;
        if !terminal_exists(&state, surface) {
            return Err("terminal surface no longer exists".into());
        }
        if programs.get(&surface).is_some_and(|current| {
            current.generation == snapshot.generation && current.owner_pid != snapshot.owner_pid
        }) {
            return Err("terminal generation changed its owner".into());
        }
        if programs.get(&surface).is_some_and(|current| {
            current.generation == snapshot.generation && snapshot.revision <= current.revision
        }) {
            return Ok(false);
        }
        project_state(&mut state, surface, Some(&snapshot), visible);
        programs.insert(surface, snapshot);
        Ok(true)
    }

    pub async fn terminal_program_status(
        &self,
        surface: SurfaceId,
    ) -> Option<TerminalProgramSnapshot> {
        let programs = self.agents.programs.lock().await;
        let state = self.inner.lock().await;
        terminal_exists(&state, surface)
            .then(|| programs.get(&surface).cloned())
            .flatten()
    }

    /// A wrapper crash also ends working/blocked records, even if it could not
    /// send its last snapshot. Retained results belong to the ended generation.
    pub async fn terminal_program_process_exited(&self, surface: SurfaceId) {
        let mut programs = self.agents.programs.lock().await;
        if let Some(snapshot) = programs.get_mut(&surface) {
            snapshot.records.retain(|payload| {
                matches!(parse_program_status(payload), Some(ProgramStatus::Report(report))
                    if matches!(report.state, ProgramState::Done | ProgramState::Error))
            });
            project_state(
                &mut *self.inner.lock().await,
                surface,
                Some(snapshot),
                false,
            );
        }
    }

    pub async fn forget_terminal_program_status(&self, surface: SurfaceId) {
        let mut programs = self.agents.programs.lock().await;
        programs.remove(&surface);
        project_state(&mut *self.inner.lock().await, surface, None, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::AgentWaitReason;
    use flowmux_ipc::protocol::AgentLifecycleEvent;

    async fn fixture() -> (StateStore, SurfaceId) {
        let store = StateStore::new_lazy(flowmux_state::State::default());
        let workspace = store.create_workspace(None, "/tmp".into()).await;
        let workspace = store.get_workspace(workspace).await.unwrap();
        let surface = super::super::workspace_pane_surface_ids(&workspace)[0];
        (store, surface)
    }

    fn snapshot(records: &[&str]) -> TerminalProgramSnapshot {
        TerminalProgramSnapshot {
            generation: SurfaceId::new().0,
            owner_pid: 42,
            revision: 1,
            records: records
                .iter()
                .map(|record| format!("7501;{record}"))
                .collect(),
        }
    }

    #[tokio::test]
    async fn revisions_replace_atomically_and_old_acknowledgements_cannot_roll_back() {
        let (store, surface) = fixture().await;
        let first = snapshot(&["state=working", "state=blocked:id=child:kind=question"]);
        assert!(store
            .report_terminal_program_status(surface, first.clone())
            .await
            .unwrap());
        let mut next = first.clone();
        next.revision = 2;
        next.records = vec!["7501;state=done".into()];
        assert!(store
            .report_terminal_program_status(surface, next.clone())
            .await
            .unwrap());
        assert!(!store
            .report_terminal_program_status(surface, first.clone())
            .await
            .unwrap());
        assert!(!store
            .report_terminal_program_status(surface, next.clone())
            .await
            .unwrap());
        assert_eq!(store.terminal_program_status(surface).await, Some(next));
        let replacement = snapshot(&["state=idle"]);
        assert!(store
            .report_terminal_program_status(surface, replacement.clone())
            .await
            .unwrap());
        assert_eq!(
            store.terminal_program_status(surface).await,
            Some(replacement)
        );
    }

    #[tokio::test]
    async fn malformed_snapshot_does_not_partially_clear_existing_reports() {
        let (store, surface) = fixture().await;
        let before = snapshot(&["state=blocked:kind=permission"]);
        store
            .report_terminal_program_status(surface, before.clone())
            .await
            .unwrap();
        for records in [
            vec!["state=done", "state=done:msg=AA=="],
            vec!["state=clear"],
            vec!["state=done", "state=error"],
            vec!["?"],
            vec!["state=done"; 257],
        ] {
            let mut bad = before.clone();
            bad.revision = 2;
            bad.records = records
                .iter()
                .map(|record| format!("7501;{record}"))
                .collect();
            assert!(store
                .report_terminal_program_status(surface, bad)
                .await
                .is_err());
            assert_eq!(
                store.terminal_program_status(surface).await,
                Some(before.clone())
            );
        }
        let mut wrong_owner = before.clone();
        wrong_owner.owner_pid = 43;
        wrong_owner.revision = 2;
        assert!(store
            .report_terminal_program_status(surface, wrong_owner)
            .await
            .is_err());
        assert!(store
            .report_terminal_program_status(SurfaceId::new(), before.clone())
            .await
            .is_err());
        assert_eq!(store.terminal_program_status(surface).await, Some(before));
    }

    #[tokio::test]
    async fn reports_and_queries_cannot_resolve_or_acknowledge_native_waits() {
        let (store, surface) = fixture().await;
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                None,
                Some(1),
                "session",
                AgentLifecycleEvent::TurnStarted {
                    turn_id: None,
                    status_text: "Starting turn".into(),
                },
                false,
            )
            .await;
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                None,
                Some(2),
                "session",
                AgentLifecycleEvent::WaitStarted {
                    reason: AgentWaitReason::Question,
                    item_id: "question".into(),
                    message: None,
                    status_text: "Question".into(),
                },
                false,
            )
            .await;
        let before = store.located_agent_presence(surface).await.unwrap();
        for state in ["working", "idle", "done", "error"] {
            store
                .report_terminal_program_status(surface, snapshot(&[&format!("state={state}")]))
                .await
                .unwrap();
            assert!(store.terminal_program_status(surface).await.is_some());
            let mut after = store.located_agent_presence(surface).await.unwrap();
            let mut displayed = after.presence.display_presence().into_owned();
            displayed.terminal_report = None;
            assert_eq!(displayed, before.presence);
            after.presence.terminal_report = None;
            assert_eq!(after, before);
        }
        store.terminal_program_process_exited(surface).await;
        let mut after = store.located_agent_presence(surface).await.unwrap();
        let mut displayed = after.presence.display_presence().into_owned();
        displayed.terminal_report = None;
        assert_eq!(displayed, before.presence);
        after.presence.terminal_report = None;
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn wrapper_exit_keeps_results_and_surface_removal_forgets_everything() {
        let (store, surface) = fixture().await;
        let reports = snapshot(&[
            "state=done",
            "state=error:id=result",
            "state=working:id=work",
            "state=blocked:id=wait",
            "state=idle:id=idle",
        ]);
        store
            .report_terminal_program_status(surface, reports.clone())
            .await
            .unwrap();
        store.terminal_program_process_exited(surface).await;
        let ended = store.terminal_program_status(surface).await.unwrap();
        assert_eq!(
            ended.records,
            ["7501;state=done", "7501;state=error:id=result"]
        );
        assert_eq!(ended.revision, reports.revision);
        store.forget_cleared_agent_surfaces(&[surface]).await;
        assert!(store.terminal_program_status(surface).await.is_none());
    }

    #[tokio::test]
    async fn terminal_projection_is_shared_by_tree_bar_attention_and_read_only_details() {
        use flowmux_core::{AgentAttention, AgentEvidenceSource};
        let (store, surface) = fixture().await;
        let mut report = snapshot(&[
            "state=working:app=build",
            "state=blocked:id=child:kind=question",
        ]);
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        let detail = store.explain_agents(Some(surface)).await.remove(0);
        assert_eq!(detail.agent, "build");
        assert_eq!(detail.status, AgentStatus::Blocked);
        assert_eq!(detail.attention, AgentAttention::InputRequired);
        assert!(detail.terminal_report_primary);
        assert_eq!(
            detail.status_evidence.unwrap().source,
            AgentEvidenceSource::Program
        );
        assert_eq!(detail.pending_waits[0].scope, AgentWaitScope::Session);
        assert_eq!(
            detail.terminal_report.as_ref().unwrap().records[1]
                .app
                .as_deref(),
            Some("build")
        );
        for _ in 0..3 {
            assert_eq!(store.explain_agents(Some(surface)).await[0], detail);
        }
        let item = store.agent_bar_model().await.items.remove(0);
        assert_eq!(item.status, detail.status);
        assert_eq!(item.attention(), detail.attention);
        assert_eq!(item.status_evidence, detail.status_evidence);
        assert_eq!(
            store
                .workspace_agent_attention_status(detail.workspace)
                .await,
            Some(AgentStatus::Blocked)
        );
        let tree = flowmux_ipc::protocol::describe_workspaces(&[store
            .get_workspace(detail.workspace)
            .await
            .unwrap()])
        .remove(0);
        assert_eq!(
            tree.panes[0].tabs[0].agent.as_ref().unwrap().activity,
            AgentActivity::NeedsInput
        );
        store.set_active_workspace(Some(detail.workspace)).await;
        let read = store.explain_agents(Some(surface)).await.remove(0);
        assert!(read.seen);
        assert_eq!(read.pending_waits, detail.pending_waits);
        assert_eq!(read.attention, AgentAttention::InputRequired);
        report.revision += 1;
        report.records = vec!["7501;state=done:app=build".into()];
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        let done = store.explain_agents(Some(surface)).await.remove(0);
        assert_eq!(done.attention, AgentAttention::ResultReview);
        assert_eq!(done.turn_outcome, Some(AgentTurnOutcome::Unknown));
        assert!(done.pending_waits.is_empty());
        report.revision += 1;
        report.records.clear();
        store
            .report_terminal_program_status(surface, report)
            .await
            .unwrap();
        assert!(store.explain_agents(Some(surface)).await.is_empty());
    }

    #[tokio::test]
    async fn clear_restores_latest_screen_basis_without_renaming_process_identity() {
        use flowmux_core::{AgentEvidenceSource, AgentStatusEvidence, AgentStatusReport};
        let (store, surface) = fixture().await;
        let mut base = AgentPresence::new("codex", AgentActivity::Idle, None);
        base.source = Some("flowmux:proc".into());
        base.status_evidence = Some(AgentStatusEvidence {
            source: AgentEvidenceSource::Process,
            since: chrono::Utc::now(),
        });
        store.set_agent_activity(surface, Some(base)).await;
        let mut report = snapshot(&[
            "state=working:app=untrusted",
            "state=blocked:id=child:kind=permission",
        ]);
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        let mut screen =
            AgentStatusReport::from_activity("codex", Some(AgentActivity::Running), None);
        screen.source = Some("flowmux:screen".into());
        screen.custom_status = Some("New screen progress".into());
        store
            .report_agent_status_with_visibility(surface, screen, false)
            .await;
        let shown = store.explain_agents(Some(surface)).await.remove(0);
        assert_eq!(shown.agent, "codex");
        assert_eq!(shown.status, AgentStatus::Blocked);
        assert_eq!(shown.identity_source.as_deref(), Some("flowmux:proc"));
        assert_eq!(
            shown.status_evidence.unwrap().source,
            AgentEvidenceSource::Program
        );
        report.revision += 1;
        report.records.clear();
        store
            .report_terminal_program_status(surface, report)
            .await
            .unwrap();
        let shown = store.explain_agents(Some(surface)).await.remove(0);
        assert_eq!(shown.status, AgentStatus::Working);
        assert_eq!(
            shown.status_evidence.unwrap().source,
            AgentEvidenceSource::Screen
        );
        assert!(shown.terminal_report.is_none());
    }

    #[tokio::test]
    async fn accepted_native_event_retires_projection_and_old_snapshot_cannot_reattach() {
        use flowmux_core::AgentEvidenceSource;
        let (store, surface) = fixture().await;
        let report = snapshot(&["state=done:app=build"]);
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                None,
                Some(1),
                "new-session",
                AgentLifecycleEvent::TurnStarted {
                    turn_id: None,
                    status_text: "Starting turn".into(),
                },
                false,
            )
            .await;
        let native = store.explain_agents(Some(surface)).await.remove(0);
        assert_eq!(native.status, AgentStatus::Working);
        assert_eq!(
            native.status_evidence.unwrap().source,
            AgentEvidenceSource::Hook
        );
        assert!(native.terminal_report.is_none());
        assert!(!store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap());
        assert_eq!(store.explain_agents(Some(surface)).await[0], native);
        let mut fresh = report;
        fresh.revision += 1;
        store
            .report_terminal_program_status(surface, fresh)
            .await
            .unwrap();
        let current = store.explain_agents(Some(surface)).await.remove(0);
        assert_eq!(current.status, AgentStatus::Working);
        assert_eq!(current.status_evidence, native.status_evidence);
        assert!(current.terminal_report.is_some());
        assert!(!current.terminal_report_primary);
    }
    #[tokio::test]
    async fn clearing_one_read_request_keeps_remaining_request_read_but_new_request_alerts() {
        let (store, surface) = fixture().await;
        let mut report = snapshot(&[
            "state=blocked:id=one:kind=question",
            "state=blocked:id=two:kind=question",
        ]);
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        let original = store.explain_agents(Some(surface)).await.remove(0);
        store.set_active_workspace(Some(original.workspace)).await;
        report.revision += 1;
        report.records.remove(0);
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        let remaining = store.explain_agents(Some(surface)).await.remove(0);
        assert!(
            remaining.seen,
            "clearing one request must not reopen an old unread indication"
        );
        assert_eq!(remaining.pending_waits[0].count, 1);
        assert_eq!(remaining.status_evidence, original.status_evidence);
        report.revision += 1;
        report
            .records
            .push("7501;state=blocked:id=three:kind=question".into());
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        assert!(!store.explain_agents(Some(surface)).await[0].seen);
        report.revision += 1;
        report.records = vec![
            "7501;state=done:id=two".into(),
            "7501;state=done:id=three".into(),
        ];
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        store.set_active_workspace(Some(original.workspace)).await;
        report.revision += 1;
        report.records.pop();
        store
            .report_terminal_program_status(surface, report.clone())
            .await
            .unwrap();
        assert!(store.explain_agents(Some(surface)).await[0].seen);
        report.revision += 1;
        report.records.push("7501;state=error:id=three".into());
        store
            .report_terminal_program_status(surface, report)
            .await
            .unwrap();
        let failed = store.explain_agents(Some(surface)).await.remove(0);
        assert!(!failed.seen);
        assert_eq!(
            failed.attention,
            flowmux_core::AgentAttention::FailureReview
        );
    }
}
