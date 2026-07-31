// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, in-memory Agent activity history for the side-panel popover.

use flowmux_core::{AgentStatus, PaneId, SurfaceId, WorkspaceId};
use flowmux_daemon::LocatedAgentPresence;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

const MAX_RETAINED: usize = 50;
const DUP_WINDOW: chrono::Duration = chrono::Duration::seconds(8);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityEntry {
    pub agent: String,
    pub status: Option<AgentStatus>,
    pub summary: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub workspace: WorkspaceId,
    pub pane: PaneId,
    pub surface: SurfaceId,
    pub workspace_label: String,
    pub surface_label: String,
    pub color: String,
    pub session_id: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityNowEntry {
    pub agent: String,
    pub status: AgentStatus,
    pub status_text: String,
    pub seen: bool,
    pub workspace: WorkspaceId,
    pub pane: PaneId,
    pub surface: SurfaceId,
    pub workspace_label: String,
    pub surface_label: String,
    pub color: String,
}

pub fn current_activity_entries(state: &flowmux_state::State) -> Vec<ActivityNowEntry> {
    let mut workspaces = Vec::new();
    for workspace_id in &state.workspace_order {
        if let Some(workspace) = state
            .workspaces
            .iter()
            .find(|workspace| workspace.id == *workspace_id)
        {
            workspaces.push(workspace);
        }
    }
    for workspace in &state.workspaces {
        if !state.workspace_order.contains(&workspace.id) {
            workspaces.push(workspace);
        }
    }

    workspaces
        .into_iter()
        .flat_map(|workspace| {
            workspace
                .collect_agent_bar_items()
                .into_iter()
                .map(move |item| {
                    let surface_label = workspace
                        .surfaces
                        .iter()
                        .find_map(|surface| {
                            surface.root_pane.surface_title(item.pane, item.surface)
                        })
                        .unwrap_or("tab")
                        .to_string();
                    ActivityNowEntry {
                        agent: item.agent_name,
                        status: item.status,
                        status_text: item.status_text,
                        seen: item.seen,
                        workspace: item.workspace,
                        pane: item.pane,
                        surface: item.surface,
                        workspace_label: workspace.display_title().to_string(),
                        surface_label,
                        color: item.color,
                    }
                })
        })
        .collect()
}

impl ActivityEntry {
    pub fn from_hook_presence(located: LocatedAgentPresence) -> Option<Self> {
        let status_text = located
            .presence
            .status_text()
            .unwrap_or_else(|| located.presence.status.as_str());
        if located.presence.name == "claude"
            && (status_text == "Working" || status_text.starts_with("Using "))
        {
            return None;
        }
        let (status, summary) = if status_text == "Ready" {
            (Some(located.presence.status), "Session started".to_string())
        } else if status_text == "Completed" || status_text.starts_with("Completed:") {
            (Some(AgentStatus::Done), status_text.to_string())
        } else {
            (Some(located.presence.status), status_text.to_string())
        };
        Some(Self {
            agent: located.presence.name,
            status,
            summary,
            created_at: chrono::Utc::now(),
            workspace: located.workspace,
            pane: located.pane,
            surface: located.surface,
            workspace_label: located.workspace_label,
            surface_label: located.surface_label,
            color: located.color,
            session_id: located.presence.session_id,
            source: "flowmux:hook".into(),
        })
    }

    pub fn session_ended(located: LocatedAgentPresence, source: &str) -> Self {
        Self {
            agent: located.presence.name,
            status: None,
            summary: "Session ended".into(),
            created_at: chrono::Utc::now(),
            workspace: located.workspace,
            pane: located.pane,
            surface: located.surface,
            workspace_label: located.workspace_label,
            surface_label: located.surface_label,
            color: located.color,
            session_id: located.presence.session_id,
            source: source.into(),
        }
    }
}

#[derive(Clone, Default)]
pub struct ActivityStore {
    entries: Rc<RefCell<VecDeque<ActivityEntry>>>,
    active_sessions: Rc<RefCell<HashMap<SurfaceId, (String, Option<String>)>>>,
}

impl ActivityStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, entry: ActivityEntry) -> bool {
        if entry.source != "flowmux:hook"
            && !(entry.source == "flowmux:proc" && entry.status.is_none())
        {
            return false;
        }
        let session_started = entry.summary == "Session started";
        if session_started && !self.begin_session(&entry) {
            return false;
        }
        if entry.status.is_none() {
            self.active_sessions.borrow_mut().remove(&entry.surface);
        }

        let mut entries = self.entries.borrow_mut();
        if !session_started
            && entries
                .iter()
                .rev()
                .find(|existing| existing.surface == entry.surface)
                .is_some_and(|existing| {
                    existing.status == entry.status
                        && existing.summary == entry.summary
                        && entry.created_at.signed_duration_since(existing.created_at) < DUP_WINDOW
                })
        {
            return false;
        }
        if entries.len() >= MAX_RETAINED {
            entries.pop_front();
        }
        entries.push_back(entry);
        true
    }

    fn begin_session(&self, entry: &ActivityEntry) -> bool {
        let mut sessions = self.active_sessions.borrow_mut();
        let Some(current) = sessions.get_mut(&entry.surface) else {
            sessions.insert(
                entry.surface,
                (entry.agent.clone(), entry.session_id.clone()),
            );
            return true;
        };
        if current.0 != entry.agent {
            *current = (entry.agent.clone(), entry.session_id.clone());
            return true;
        }
        if current.1.is_some() && entry.session_id.is_some() && current.1 != entry.session_id {
            current.1 = entry.session_id.clone();
            return true;
        }
        if current.1.is_none() && entry.session_id.is_some() {
            current.1 = entry.session_id.clone();
            if let Some(started) = self.entries.borrow_mut().iter_mut().rev().find(|existing| {
                existing.surface == entry.surface && existing.summary == "Session started"
            }) {
                started.session_id = entry.session_id.clone();
            }
        }
        false
    }

    pub fn entries(&self) -> Vec<ActivityEntry> {
        self.entries.borrow().iter().cloned().collect()
    }

    pub fn clear(&self) -> bool {
        let mut entries = self.entries.borrow_mut();
        let changed = !entries.is_empty();
        entries.clear();
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::{
        AgentPresence, Pane, PaneContent, PaneSurface, Surface, SurfaceKind, Workspace,
    };
    use std::path::PathBuf;

    fn entry(surface: SurfaceId, status: Option<AgentStatus>, summary: &str) -> ActivityEntry {
        ActivityEntry {
            agent: "claude".into(),
            status,
            summary: summary.into(),
            created_at: chrono::Utc::now(),
            workspace: WorkspaceId::new(),
            pane: PaneId::new(),
            surface,
            workspace_label: "flowmux".into(),
            surface_label: "zsh".into(),
            color: "#abcdef".into(),
            session_id: None,
            source: "flowmux:hook".into(),
        }
    }

    fn located(status: AgentStatus, status_text: &str) -> LocatedAgentPresence {
        let surface = SurfaceId::new();
        let mut presence = AgentPresence::new("claude", status.to_activity(), Some(42));
        presence.status = status;
        presence.source = Some("flowmux:hook".into());
        presence.custom_status = Some(status_text.into());
        LocatedAgentPresence {
            workspace: WorkspaceId::new(),
            pane: PaneId::new(),
            surface,
            workspace_label: "flowmux".into(),
            surface_label: "zsh".into(),
            color: "#abcdef".into(),
            presence,
        }
    }

    #[test]
    fn hook_entry_maps_completion_and_drops_tool_updates() {
        let completed =
            ActivityEntry::from_hook_presence(located(AgentStatus::Idle, "Completed: fixed tests"))
                .unwrap();
        assert_eq!(completed.status, Some(AgentStatus::Done));
        assert_eq!(completed.summary, "Completed: fixed tests");
        assert!(
            ActivityEntry::from_hook_presence(located(AgentStatus::Working, "Using Bash"))
                .is_none()
        );
    }

    #[test]
    fn now_entries_use_live_state_and_display_labels() {
        let workspace = WorkspaceId::new();
        let pane = PaneId::new();
        let mut tab = PaneSurface::terminal("zsh", Some(PathBuf::from("/tmp/flowmux")));
        let surface = tab.id;
        let mut presence =
            AgentPresence::new("codex", AgentStatus::Working.to_activity(), Some(42));
        presence.status = AgentStatus::Working;
        presence.custom_status = Some("Running tests".into());
        tab.agent = Some(presence);
        let state = flowmux_state::State {
            workspace_order: vec![workspace],
            workspaces: vec![Workspace {
                id: workspace,
                name: "automatic".into(),
                custom_title: Some("flowmux-terminal".into()),
                root_dir: PathBuf::from("/tmp/flowmux"),
                git: None,
                listening_ports: vec![],
                surfaces: vec![Surface {
                    id: SurfaceId::new(),
                    kind: SurfaceKind::Terminal {
                        shell: None,
                        cwd: Some(PathBuf::from("/tmp/flowmux")),
                    },
                    title: "main".into(),
                    root_pane: Pane::Leaf {
                        id: pane,
                        content: PaneContent::Tabs {
                            active: surface,
                            surfaces: vec![tab],
                        },
                    },
                }],
                color: Some("#123456".into()),
            }],
            ..Default::default()
        };

        let entries = current_activity_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent, "codex");
        assert_eq!(entries[0].status_text, "Running tests");
        assert_eq!(entries[0].workspace_label, "flowmux-terminal");
        assert_eq!(entries[0].surface_label, "zsh");
        assert_eq!(entries[0].color, "#123456");
    }

    #[test]
    fn store_retains_only_the_latest_fifty_entries() {
        let store = ActivityStore::new();
        for index in 0..1000 {
            assert!(store.push(entry(
                SurfaceId::new(),
                Some(AgentStatus::Working),
                &format!("event {index}")
            )));
        }
        let entries = store.entries();
        assert_eq!(entries.len(), MAX_RETAINED);
        assert_eq!(entries.first().unwrap().summary, "event 950");
        assert_eq!(entries.last().unwrap().summary, "event 999");
    }

    #[test]
    fn repeated_session_updates_enrich_instead_of_adding_rows() {
        let store = ActivityStore::new();
        let surface = SurfaceId::new();
        assert!(store.push(entry(
            surface,
            Some(AgentStatus::Unknown),
            "Session started"
        )));
        let mut update = entry(surface, Some(AgentStatus::Unknown), "Session started");
        update.session_id = Some("session-1".into());
        assert!(!store.push(update));
        assert_eq!(store.entries().len(), 1);
        assert_eq!(store.entries()[0].session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn a_changed_session_id_records_a_new_start() {
        let store = ActivityStore::new();
        let surface = SurfaceId::new();
        for session_id in ["session-1", "session-2"] {
            let mut started = entry(surface, Some(AgentStatus::Unknown), "Session started");
            started.session_id = Some(session_id.into());
            assert!(store.push(started));
        }
        assert_eq!(store.entries().len(), 2);
    }

    #[test]
    fn a_changed_agent_records_a_new_start_without_a_session_id() {
        let store = ActivityStore::new();
        let surface = SurfaceId::new();
        assert!(store.push(entry(
            surface,
            Some(AgentStatus::Unknown),
            "Session started"
        )));
        let mut codex = entry(surface, Some(AgentStatus::Unknown), "Session started");
        codex.agent = "codex".into();
        assert!(store.push(codex));
        assert_eq!(store.entries().len(), 2);
    }

    #[test]
    fn ending_a_session_allows_the_next_start() {
        let store = ActivityStore::new();
        let surface = SurfaceId::new();
        assert!(store.push(entry(
            surface,
            Some(AgentStatus::Unknown),
            "Session started"
        )));
        assert!(store.push(entry(surface, None, "Session ended")));
        assert!(store.push(entry(
            surface,
            Some(AgentStatus::Unknown),
            "Session started"
        )));
        assert_eq!(store.entries().len(), 3);
    }

    #[test]
    fn clear_drops_recent_entries_idempotently() {
        let store = ActivityStore::new();
        assert!(store.push(entry(
            SurfaceId::new(),
            Some(AgentStatus::Working),
            "Starting turn"
        )));
        assert!(store.clear());
        assert!(store.entries().is_empty());
        assert!(!store.clear());
    }

    #[test]
    fn store_ignores_screen_and_process_status_updates() {
        let store = ActivityStore::new();
        for source in ["flowmux:screen", "flowmux:proc"] {
            let mut update = entry(SurfaceId::new(), Some(AgentStatus::Working), "Working");
            update.source = source.into();
            assert!(!store.push(update));
        }

        let mut ended = entry(SurfaceId::new(), None, "Session ended");
        ended.source = "flowmux:screen".into();
        assert!(!store.push(ended.clone()));
        ended.source = "flowmux:proc".into();
        assert!(store.push(ended));
        assert_eq!(store.entries().len(), 1);
    }
}
