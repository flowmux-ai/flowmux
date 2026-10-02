// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, in-memory Agent activity history for the side-panel popover.

use flowmux_config::options::AgentSortMode;
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
    pub surface_label: String,
    pub color: String,
}

pub fn current_activity_entries(model: &flowmux_core::AgentBarModel) -> Vec<ActivityNowEntry> {
    model
        .items
        .iter()
        .cloned()
        .map(|item| ActivityNowEntry {
            agent: item.agent_name,
            status: item.status,
            status_text: item.status_text,
            seen: item.seen,
            workspace: item.workspace,
            pane: item.pane,
            surface: item.surface,
            surface_label: item.surface_label,
            color: item.color,
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
    /// Per live agent: whether it is mid-turn and since when, as observed by
    /// the Agents list. Status changes are timed here rather than taken from
    /// hook entries so agents known only by process or screen detection sort too.
    turns: Rc<RefCell<HashMap<SurfaceId, (bool, chrono::DateTime<chrono::Utc>)>>>,
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

    #[cfg(test)]
    pub fn entries(&self) -> Vec<ActivityEntry> {
        self.entries.borrow().iter().cloned().collect()
    }

    pub fn forget_surfaces(&self, surfaces: &[SurfaceId]) {
        let mut sessions = self.active_sessions.borrow_mut();
        for surface in surfaces {
            sessions.remove(surface);
        }
    }

    /// Time shown on an Agents row and used to sort it: when the current turn
    /// was requested or the last one ended, else the latest activity entry.
    pub fn since(&self, surface: SurfaceId) -> Option<chrono::DateTime<chrono::Utc>> {
        self.turns
            .borrow()
            .get(&surface)
            .map(|turn| turn.1)
            .or_else(|| {
                let entries = self.entries.borrow();
                let latest = entries.iter().rev().find(|entry| entry.surface == surface);
                latest.map(|entry| entry.created_at)
            })
    }

    /// Record turn changes of the live agents, then order the Agents list.
    /// `entries` arrive in workspace order, which the stable sort keeps for ties.
    pub fn sort_entries(&self, entries: &mut [ActivityNowEntry], mode: AgentSortMode) {
        let in_progress = |status| matches!(status, AgentStatus::Working | AgentStatus::Blocked);
        {
            let now = chrono::Utc::now();
            let mut turns = self.turns.borrow_mut();
            turns.retain(|surface, _| entries.iter().any(|entry| entry.surface == *surface));
            for entry in entries.iter() {
                let busy = in_progress(entry.status);
                match turns.get(&entry.surface) {
                    Some(turn) if turn.0 == busy => {}
                    // An agent first seen idle has no known finish time.
                    None if !busy => {}
                    _ => {
                        turns.insert(entry.surface, (busy, now));
                    }
                }
            }
        }
        if mode == AgentSortMode::Workspace {
            return;
        }
        entries.sort_by_key(|entry| {
            let at = self
                .since(entry.surface)
                .and_then(|at| at.timestamp_nanos_opt());
            // Recently finished: working agents by request time, then the
            // latest finish first; an unknown finish counts as the oldest.
            let key = if in_progress(entry.status) {
                (0, at.unwrap_or(i64::MAX))
            } else {
                (1, -at.unwrap_or(-i64::MAX))
            };
            // Oldest finished is the exact reverse.
            match mode {
                AgentSortMode::RecentlyFinished => key,
                _ => (-key.0, -key.1),
            }
        });
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
    fn sort_entries_puts_working_agents_before_recent_finishes_and_mirrors_for_oldest() {
        let store = ActivityStore::new();
        let now = || ActivityNowEntry {
            agent: "codex".into(),
            status: AgentStatus::Idle,
            status_text: String::new(),
            seen: true,
            workspace: WorkspaceId::new(),
            pane: PaneId::new(),
            surface: SurfaceId::new(),
            surface_label: "zsh".into(),
            color: "#abcdef".into(),
        };
        // Workspace order. No hook entries exist: every time is observed.
        let mut entries = [now(), now(), now(), now(), now()];
        let [unknown, late, second, early, first] = entries.each_ref().map(|entry| entry.surface);
        let mut observe = |changes: &[(usize, AgentStatus)]| {
            for (index, status) in changes {
                entries[*index].status = *status;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
            store.sort_entries(&mut entries.clone(), AgentSortMode::Workspace);
            entries.clone()
        };
        observe(&[
            (1, AgentStatus::Working),
            (3, AgentStatus::Blocked),
            (4, AgentStatus::Working),
        ]);
        observe(&[(2, AgentStatus::Working), (3, AgentStatus::Idle)]);
        // `first` stays mid-turn across a permission wait and keeps its request time.
        let entries = observe(&[(1, AgentStatus::Done), (4, AgentStatus::Blocked)]);
        let sorted = |mode| {
            let mut entries = entries.clone();
            store.sort_entries(&mut entries, mode);
            entries.map(|entry| entry.surface)
        };

        assert_eq!(
            sorted(AgentSortMode::Workspace),
            [unknown, late, second, early, first]
        );
        assert_eq!(
            sorted(AgentSortMode::RecentlyFinished),
            [first, second, late, early, unknown]
        );
        assert_eq!(
            sorted(AgentSortMode::OldestFinished),
            [unknown, early, late, second, first]
        );
        assert!(store.since(unknown).is_none());
        assert!(store.since(early) < store.since(late));

        // A row first seen idle falls back to its latest activity entry.
        assert!(store.push(entry(unknown, Some(AgentStatus::Idle), "Session started")));
        assert_eq!(
            sorted(AgentSortMode::RecentlyFinished),
            [first, second, unknown, late, early]
        );

        // Agents that left the list are forgotten.
        store.sort_entries(&mut [], AgentSortMode::Workspace);
        assert!(store.since(first).is_none());
    }

    #[test]
    fn now_entries_use_live_state_and_agent_tab_label() {
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
                location: flowmux_core::WorkspaceLocation::Local {
                    root_dir: PathBuf::from("/tmp/flowmux"),
                },
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

        let model = flowmux_core::collect_agent_bar_model(state.workspaces.iter());
        let entries = current_activity_entries(&model);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent, "codex");
        assert_eq!(entries[0].status_text, "Running tests");
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
    fn forgetting_a_closed_surface_allows_the_next_start() {
        let store = ActivityStore::new();
        let surface = SurfaceId::new();
        assert!(store.push(entry(
            surface,
            Some(AgentStatus::Unknown),
            "Session started"
        )));

        store.forget_surfaces(&[surface]);

        assert!(store.push(entry(
            surface,
            Some(AgentStatus::Unknown),
            "Session started"
        )));
        assert_eq!(store.entries().len(), 2);
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
