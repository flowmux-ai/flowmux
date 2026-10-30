// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime-only terminal reports. Native hook ledgers remain independent.

use super::StateStore;
use flowmux_core::{Pane, PaneContent, SurfaceId, SurfaceKind};
use flowmux_ipc::protocol::TerminalProgramSnapshot;
use flowmux_notify::program_status::{
    parse_program_status, ProgramState, ProgramStatus, MAX_PROGRAM_STATUS_RECORDS,
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

impl StateStore {
    /// The GUI must first match the wrapper PID and generation to its live PTY.
    /// State validation and revision admission are atomic with surface removal.
    pub async fn report_terminal_program_status(
        &self,
        surface: SurfaceId,
        mut snapshot: TerminalProgramSnapshot,
    ) -> Result<bool, String> {
        validate(&mut snapshot)?;
        let mut programs = self.agents.programs.lock().await;
        let state = self.inner.lock().await;
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
        if let Some(snapshot) = self.agents.programs.lock().await.get_mut(&surface) {
            snapshot.records.retain(|payload| {
                matches!(parse_program_status(payload), Some(ProgramStatus::Report(report))
                    if matches!(report.state, ProgramState::Done | ProgramState::Error))
            });
        }
    }

    pub async fn forget_terminal_program_status(&self, surface: SurfaceId) {
        self.agents.programs.lock().await.remove(&surface);
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
            assert_eq!(store.located_agent_presence(surface).await.unwrap(), before);
        }
        store.terminal_program_process_exited(surface).await;
        assert_eq!(store.located_agent_presence(surface).await.unwrap(), before);
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
}
