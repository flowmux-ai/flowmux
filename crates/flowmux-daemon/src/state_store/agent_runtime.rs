// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime-only agent evidence and transitions owned by StateStore.
//!
//! Hook, process and screen observations share these ledgers. The lifecycle
//! lock still precedes the state lock; persistence and GUI effects stay with
//! their existing owners. StateStore clones share one runtime, not copies.

use super::StateStore;
use flowmux_core::{
    agent_bar_color_for_surface, detect_agent_completion, detect_agent_idle_name_from_signals,
    detect_agent_interruption, detect_agent_name_from_signals, detect_agent_progress_text,
    detect_agent_status_from_signals, detect_agent_usage_limit_text,
    select_process_agent_candidate, AgentPresence, AgentStatus, AgentStatusReport, Pane,
    PaneContent, PaneId, PaneSurface, SurfaceId, SurfaceKind, Workspace, WorkspaceId,
};
use flowmux_ipc::protocol::AgentLifecycleEvent;
use flowmux_state::State;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use tokio::sync::Mutex;

// Diagnostic IDs are opaque correlation tokens, never payload, prompt or title text.
fn trace_key(value: Option<&str>) -> Option<u64> {
    value.map(|value| {
        let mut hash = DefaultHasher::new();
        value.hash(&mut hash);
        hash.finish()
    })
}

fn lifecycle_trace_event(event: &AgentLifecycleEvent) -> (&'static str, Option<u64>) {
    use AgentLifecycleEvent::*;
    let (kind, key) = match event {
        TurnStarted { turn_id, .. } => ("turn_started", turn_id.as_deref()),
        ProgressObserved { .. } => ("progress", None),
        CodexRootProgressObserved { turn_id, .. } => ("root_progress", Some(turn_id.as_str())),
        PermissionWaitStarted { scope, .. } => ("permission_wait", scope.as_deref()),
        SessionWaitStarted { scope, .. } => ("session_wait", scope.as_deref()),
        SessionWaitResolved { scope, .. } => ("session_wait_resolved", scope.as_deref()),
        ToolBatchFinished { .. } => ("tool_batch_finished", None),
        WaitStarted { item_id, .. } => ("tool_wait", Some(item_id.as_str())),
        WaitResolved { item_id } => ("tool_resolved", Some(item_id.as_str())),
        TurnStopped { .. } => ("turn_stopped", None),
        CodexSubagentStarted { turn_id, .. } => ("child_started", Some(turn_id.as_str())),
        CodexChildProgressObserved { turn_id, .. } => ("child_progress", Some(turn_id.as_str())),
        CodexSubagentStopped { turn_id, .. } => ("child_stopped", Some(turn_id.as_str())),
        CodexTurnStopped { turn_id, .. } => ("codex_stopped", Some(turn_id.as_str())),
        CodexTurnInterrupted { turn_id, .. } => ("codex_interrupted", Some(turn_id.as_str())),
    };
    (kind, trace_key(key))
}

fn trace_agent_before(current: Option<&LocatedAgentPresence>) {
    if let Some(current) = current {
        let presence = &current.presence;
        let owner = match presence.source.as_deref() {
            Some("flowmux:hook") => "hook",
            Some("flowmux:screen") => "screen",
            Some(flowmux_core::AGENT_SOURCE_PROC) => "process",
            _ => "other",
        };
        let provider = match presence.name.as_str() {
            "claude" => "claude",
            "codex" => "codex",
            "gemini" => "gemini",
            "opencode" => "opencode",
            _ => "other",
        };
        tracing::debug!(target: "flowmux_agent", workspace = %current.workspace,
            pane = %current.pane, previous = ?presence.status, identity_source = owner,
            session_key = ?trace_key(presence.session_id.as_deref()),
            provider, "agent before");
    }
}

#[derive(Default)]
pub(super) struct AgentRuntime {
    pub(super) cleared_agent_surfaces: Mutex<HashSet<SurfaceId>>,
    last_agent_screen_fingerprints: Mutex<HashMap<SurfaceId, u64>>,
    cleared_agent_screen_fingerprints: Mutex<HashMap<SurfaceId, Option<u64>>>,
    cleared_agent_saw_no_signal: Mutex<HashSet<SurfaceId>>,
    pub(super) lifecycle: Mutex<AgentLifecycleRuntime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocatedAgentPresence {
    pub workspace: WorkspaceId,
    pub pane: PaneId,
    pub surface: SurfaceId,
    pub workspace_label: String,
    pub surface_label: String,
    pub color: String,
    pub presence: AgentPresence,
}

type AgentLifecycleKey = (SurfaceId, String, String);
type AgentScopeSequences = HashMap<AgentLifecycleKey, HashMap<String, u64>>;
const SESSION_PERMISSION_SCOPE: &str = "session";
const SESSION_WAIT_SCOPE: &str = "session";

#[derive(Debug, Default)]
pub(super) struct AgentLifecycleRuntime {
    /// Local process polls found neither an agent nor an out-of-tree transport.
    screen_absent: HashSet<SurfaceId>,
    /// One state per tool-use id: true while waiting, false once resolved.
    /// Keep resolved ids until the turn boundary so duplicate or reordered
    /// hook deliveries cannot reopen them. Distinct calls keep distinct ids.
    pub(super) waits: HashMap<AgentLifecycleKey, HashMap<String, bool>>,
    pub(super) permission_waits: HashMap<AgentLifecycleKey, HashSet<String>>,
    pub(super) permission_event_seq: AgentScopeSequences,
    pub(super) session_waits: HashMap<AgentLifecycleKey, HashSet<String>>,
    pub(super) session_wait_event_seq: AgentScopeSequences,
    /// Highest non-child event observed, used to reject delayed terminal
    /// boundaries and native SessionStart. Codex child ordering is tracked by
    /// the child-specific sequence maps below so its start/Stop ingress race
    /// cannot invalidate a legitimate parent Stop.
    pub(super) last_seq: HashMap<AgentLifecycleKey, u64>,
    /// Latest root/session boundary. Deltas older than this belong to a prior
    /// turn even when a different parallel item arrived later.
    pub(super) boundary_seq: HashMap<AgentLifecycleKey, u64>,
    pub(super) ended: HashMap<AgentLifecycleKey, EndedAgentLifecycle>,
    pub(super) ended_order: VecDeque<AgentLifecycleKey>,
    pub(super) codex_turns: HashMap<(SurfaceId, String), CodexTurnLedger>,
}

// A rejected report restores only the session this event can mutate. Other
// sessions and bounded tombstones never participate in a per-event copy.
struct LifecycleCheckpoint {
    key: AgentLifecycleKey,
    waits: Option<HashMap<String, bool>>,
    permission_waits: Option<HashSet<String>>,
    permission_event_seq: Option<HashMap<String, u64>>,
    session_waits: Option<HashSet<String>>,
    session_wait_event_seq: Option<HashMap<String, u64>>,
    last_seq: Option<u64>,
    boundary_seq: Option<u64>,
    codex_turn: Option<CodexTurnLedger>,
}

impl LifecycleCheckpoint {
    fn capture(runtime: &AgentLifecycleRuntime, key: &AgentLifecycleKey) -> Self {
        Self {
            key: key.clone(),
            waits: runtime.waits.get(key).cloned(),
            permission_waits: runtime.permission_waits.get(key).cloned(),
            permission_event_seq: runtime.permission_event_seq.get(key).cloned(),
            session_waits: runtime.session_waits.get(key).cloned(),
            session_wait_event_seq: runtime.session_wait_event_seq.get(key).cloned(),
            last_seq: runtime.last_seq.get(key).cloned(),
            boundary_seq: runtime.boundary_seq.get(key).cloned(),
            codex_turn: runtime.codex_turns.get(&(key.0, key.2.clone())).cloned(),
        }
    }

    fn restore(self, runtime: &mut AgentLifecycleRuntime) {
        restore_entry(&mut runtime.waits, &self.key, self.waits);
        restore_entry(
            &mut runtime.permission_waits,
            &self.key,
            self.permission_waits,
        );
        restore_entry(
            &mut runtime.permission_event_seq,
            &self.key,
            self.permission_event_seq,
        );
        restore_entry(&mut runtime.session_waits, &self.key, self.session_waits);
        restore_entry(
            &mut runtime.session_wait_event_seq,
            &self.key,
            self.session_wait_event_seq,
        );
        restore_entry(&mut runtime.last_seq, &self.key, self.last_seq);
        restore_entry(&mut runtime.boundary_seq, &self.key, self.boundary_seq);
        restore_entry(
            &mut runtime.codex_turns,
            &(self.key.0, self.key.2),
            self.codex_turn,
        );
    }
}

fn restore_entry<K: Eq + Hash + Clone, V>(map: &mut HashMap<K, V>, key: &K, previous: Option<V>) {
    if let Some(previous) = previous {
        map.insert(key.clone(), previous);
    } else {
        map.remove(key);
    }
}

#[derive(Debug, Clone)]
pub(super) struct EndedAgentLifecycle {
    pub(super) pid: Option<u32>,
    pub(super) seq: Option<u64>,
    /// A different native agent temporarily took ownership of the same pane.
    /// The displaced hook may resume only after process polling has restored
    /// its identity and the original live PID reports a newer event.
    pub(super) reactivate_on_process_return: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct CodexTurnLedger {
    pub(super) owner_pid: Option<u32>,
    pub(super) current_parent_turn: Option<String>,
    pub(super) active_children: HashMap<String, String>,
    pub(super) child_event_seq: HashMap<(String, String), u64>,
    pub(super) child_agent_event_seq: HashMap<String, u64>,
    pub(super) pending_parent_stop: Option<PendingCodexStop>,
    pub(super) settled_parent_turns: VecDeque<String>,
}

#[derive(Debug, Clone)]
pub(super) struct PendingCodexStop {
    pub(super) turn_id: String,
    pub(super) message: Option<String>,
    pub(super) status_text: String,
    pub(super) notify_completion: bool,
    pub(super) seq: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentLifecycleResult {
    pub workspace: Option<WorkspaceId>,
    pub completed: bool,
    pub completion_message: Option<String>,
    pub settle_codex_after_grace: Option<CodexGraceSettlement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexGraceSettlement {
    pub turn_id: String,
    pub stop_seq: Option<u64>,
}

fn agent_screen_fingerprint(screen_text: Option<&str>, osc_title: Option<&str>) -> u64 {
    let mut hasher = DefaultHasher::new();
    screen_text.hash(&mut hasher);
    osc_title.hash(&mut hasher);
    hasher.finish()
}

fn remember_settled_codex_turn(ledger: &mut CodexTurnLedger, turn_id: String) {
    if ledger
        .settled_parent_turns
        .iter()
        .any(|settled| settled == &turn_id)
    {
        return;
    }
    ledger.settled_parent_turns.push_back(turn_id);
    while ledger.settled_parent_turns.len() > 32 {
        ledger.settled_parent_turns.pop_front();
    }
}

fn set_codex_ledger_owner_if_missing(ledger: &mut CodexTurnLedger, pid: Option<u32>) {
    if let Some(pid) = pid {
        ledger.owner_pid.get_or_insert(pid);
    }
}

fn remember_ended_agent_lifecycle(
    runtime: &mut AgentLifecycleRuntime,
    key: AgentLifecycleKey,
    pid: Option<u32>,
    seq: Option<u64>,
    reactivate_on_process_return: bool,
) {
    runtime.ended_order.retain(|existing| existing != &key);
    runtime.ended.insert(
        key.clone(),
        EndedAgentLifecycle {
            pid,
            seq,
            reactivate_on_process_return,
        },
    );
    runtime.ended_order.push_back(key);
    while runtime.ended_order.len() > 128 {
        if let Some(oldest) = runtime.ended_order.pop_front() {
            runtime.ended.remove(&oldest);
        }
    }
}

fn lifecycle_has_waits(runtime: &AgentLifecycleRuntime, key: &AgentLifecycleKey) -> bool {
    runtime
        .waits
        .get(key)
        .is_some_and(|waits| waits.values().any(|waiting| *waiting))
        || runtime
            .permission_waits
            .get(key)
            .is_some_and(|scopes| !scopes.is_empty())
        || runtime
            .session_waits
            .get(key)
            .is_some_and(|scopes| !scopes.is_empty())
}

fn clear_permission_scope(
    runtime: &mut AgentLifecycleRuntime,
    key: &AgentLifecycleKey,
    scope: &str,
) -> bool {
    let removed = runtime
        .permission_waits
        .get_mut(key)
        .is_some_and(|scopes| scopes.remove(scope));
    if runtime
        .permission_waits
        .get(key)
        .is_some_and(HashSet::is_empty)
    {
        runtime.permission_waits.remove(key);
    }
    removed
}

fn clear_permission_scope_if_newer(
    runtime: &mut AgentLifecycleRuntime,
    key: &AgentLifecycleKey,
    scope: &str,
    seq: Option<u64>,
) -> bool {
    let newer = match (
        runtime
            .permission_event_seq
            .get(key)
            .and_then(|scopes| scopes.get(scope))
            .copied(),
        seq,
    ) {
        (Some(current), Some(incoming)) => incoming > current,
        (Some(_), None) => false,
        _ => true,
    };
    if !newer {
        return false;
    }
    if let Some(seq) = seq {
        runtime
            .permission_event_seq
            .entry(key.clone())
            .or_default()
            .insert(scope.to_string(), seq);
    }
    clear_permission_scope(runtime, key, scope);
    true
}

fn clear_codex_root_permission_scopes(
    runtime: &mut AgentLifecycleRuntime,
    key: &AgentLifecycleKey,
) {
    if let Some(scopes) = runtime.permission_waits.get_mut(key) {
        scopes.retain(|scope| !scope.starts_with("root:"));
        if scopes.is_empty() {
            runtime.permission_waits.remove(key);
        }
    }
}

fn clear_permission_event_seq_for_key(
    runtime: &mut AgentLifecycleRuntime,
    key: &AgentLifecycleKey,
) {
    runtime.permission_event_seq.remove(key);
}

fn clear_session_wait_scope(
    runtime: &mut AgentLifecycleRuntime,
    key: &AgentLifecycleKey,
    scope: &str,
) -> bool {
    let removed = runtime
        .session_waits
        .get_mut(key)
        .is_some_and(|scopes| scopes.remove(scope));
    if runtime
        .session_waits
        .get(key)
        .is_some_and(HashSet::is_empty)
    {
        runtime.session_waits.remove(key);
    }
    removed
}

fn clear_session_wait_event_seq_for_key(
    runtime: &mut AgentLifecycleRuntime,
    key: &AgentLifecycleKey,
) {
    runtime.session_wait_event_seq.remove(key);
}

fn clear_agent_lifecycle_runtime(
    lifecycle: &mut AgentLifecycleRuntime,
    surface: SurfaceId,
    agent: Option<&str>,
    session_id: Option<&str>,
    expected_pid: Option<u32>,
) {
    let keep_wait_key = |(key_surface, key_agent, key_session): &AgentLifecycleKey| {
        *key_surface != surface
            || agent.is_some_and(|agent| !key_agent.eq_ignore_ascii_case(agent))
            || session_id.is_some_and(|session| key_session != session)
    };
    lifecycle.waits.retain(|key, _| keep_wait_key(key));
    lifecycle
        .permission_waits
        .retain(|key, _| keep_wait_key(key));
    lifecycle
        .permission_event_seq
        .retain(|key, _| keep_wait_key(key));
    lifecycle.session_waits.retain(|key, _| keep_wait_key(key));
    lifecycle
        .session_wait_event_seq
        .retain(|key, _| keep_wait_key(key));
    lifecycle.last_seq.retain(|key, _| keep_wait_key(key));
    lifecycle.boundary_seq.retain(|key, _| keep_wait_key(key));
    lifecycle
        .codex_turns
        .retain(|(key_surface, key_session), ledger| {
            if *key_surface != surface {
                return true;
            }
            if session_id.is_some_and(|session| key_session != session) {
                return true;
            }
            if expected_pid.is_some_and(|pid| ledger.owner_pid != Some(pid)) {
                return true;
            }
            false
        });
}

impl StateStore {
    pub async fn is_ssh_surface(&self, surface: SurfaceId) -> bool {
        let state = self.inner.lock().await;
        state
            .workspaces
            .iter()
            .filter(|workspace| workspace.ssh_config().is_some())
            .any(|workspace| {
                workspace
                    .surfaces
                    .iter()
                    .any(|root| agent_presence_slot_in_pane(&root.root_pane, surface).is_some())
            })
    }

    /// Tab running Codex session `session_id`, with the session that tab last
    /// reported. A tab that reported this session wins. Otherwise, among Codex
    /// tabs opened in `cwd`: one whose title names the session's thread,
    /// preferring a tab no session has claimed, else the only tab whose title
    /// names no thread yet. `names` maps Codex sessions to thread names.
    /// Ambiguity returns `Err(true)` for duplicate reported session bindings,
    /// or `Err(false)` for tied heuristic candidates. Never pick by visit order.
    // ponytail: a Codex session started in `cwd` outside flowmux also lands
    // on that only unnamed tab until the tab's own thread has a name.
    pub async fn codex_session_tab(
        &self,
        session_id: &str,
        thread: Option<(&HashMap<String, String>, &Path)>,
    ) -> Result<Option<(PaneId, SurfaceId, Option<String>)>, bool> {
        fn for_each_tab(pane: &Pane, f: &mut impl FnMut(PaneId, &PaneSurface)) {
            match pane {
                Pane::Leaf {
                    id,
                    content: PaneContent::Tabs { surfaces, .. },
                } => surfaces.iter().for_each(|tab| f(*id, tab)),
                Pane::Leaf { .. } => {}
                Pane::Split { first, second, .. } => {
                    for_each_tab(first, f);
                    for_each_tab(second, f);
                }
            }
        }
        type Tab = (PaneId, SurfaceId, Option<String>);
        let state = self.inner.lock().await;
        let mut reported = Vec::new();
        let mut titled: Vec<Tab> = Vec::new();
        let mut unnamed: Vec<Tab> = Vec::new();
        let mut visit = |pane: PaneId, tab: &PaneSurface| {
            let Some(agent) = tab.agent.as_ref().filter(|agent| agent.name == "codex") else {
                return;
            };
            let found = (pane, tab.id, agent.session_id.clone());
            if agent.session_id.as_deref() == Some(session_id) {
                reported.push(found);
                return;
            }
            let (
                Some((names, cwd)),
                SurfaceKind::Terminal {
                    cwd: Some(tab_cwd), ..
                },
            ) = (thread, &tab.kind)
            else {
                return;
            };
            if !flowmux_state::session_history::same_directory(tab_cwd, cwd) {
                return;
            }
            let claimed = agent.session_id.as_ref();
            // A user-chosen title says nothing about the thread it shows.
            let shown = (!tab.title_locked)
                .then(|| flowmux_state::session_history::codex_title_thread(&tab.title));
            match shown {
                Some(Some(shown)) if names.get(session_id).is_some_and(|name| name == shown) => {
                    titled.push(found);
                }
                Some(Some(_)) => {}
                // A title without a thread still fits the session the tab
                // reported for as long as that session has no name.
                Some(None) if claimed.is_none_or(|session| names.contains_key(session)) => {
                    unnamed.push(found)
                }
                None if claimed.is_none() => unnamed.push(found),
                _ => {}
            }
        };
        for workspace in state
            .workspaces
            .iter()
            .filter(|ws| ws.local_root().is_some())
        {
            for root in &workspace.surfaces {
                for_each_tab(&root.root_pane, &mut visit);
            }
        }
        let unique = |mut tabs: Vec<Tab>, exact_session| match tabs.len() {
            0 => Ok(None),
            1 => Ok(tabs.pop()),
            _ => Err(exact_session),
        };
        if !reported.is_empty() {
            return unique(reported, true);
        }
        if !titled.is_empty() {
            if titled.iter().any(|tab| tab.2.is_none()) {
                titled.retain(|tab| tab.2.is_none());
            }
            return unique(titled, false);
        }
        unique(unnamed, false)
    }

    pub(super) async fn forget_cleared_agent_surfaces(&self, surfaces: &[SurfaceId]) {
        if surfaces.is_empty() {
            return;
        }
        let mut cleared = self.agents.cleared_agent_surfaces.lock().await;
        for surface in surfaces {
            cleared.remove(surface);
        }
        drop(cleared);
        let mut last = self.agents.last_agent_screen_fingerprints.lock().await;
        for surface in surfaces {
            last.remove(surface);
        }
        drop(last);
        let mut baselines = self.agents.cleared_agent_screen_fingerprints.lock().await;
        for surface in surfaces {
            baselines.remove(surface);
        }
        drop(baselines);
        let mut saw_no_signal = self.agents.cleared_agent_saw_no_signal.lock().await;
        for surface in surfaces {
            saw_no_signal.remove(surface);
        }
        drop(saw_no_signal);
        let mut lifecycle = self.agents.lifecycle.lock().await;
        lifecycle
            .screen_absent
            .retain(|surface| !surfaces.contains(surface));
        lifecycle
            .waits
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .permission_waits
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .permission_event_seq
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .session_waits
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .session_wait_event_seq
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .last_seq
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .boundary_seq
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .ended
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .ended_order
            .retain(|(surface, _, _)| !surfaces.contains(surface));
        lifecycle
            .codex_turns
            .retain(|(surface, _), _| !surfaces.contains(surface));
    }

    async fn clear_agent_lifecycle(
        &self,
        surface: SurfaceId,
        agent: Option<&str>,
        session_id: Option<&str>,
        expected_pid: Option<u32>,
    ) {
        let mut lifecycle = self.agents.lifecycle.lock().await;
        clear_agent_lifecycle_runtime(&mut lifecycle, surface, agent, session_id, expected_pid);
    }

    async fn allow_agent_screen_restore(&self, surface: SurfaceId) {
        self.agents
            .cleared_agent_surfaces
            .lock()
            .await
            .remove(&surface);
        self.agents
            .cleared_agent_screen_fingerprints
            .lock()
            .await
            .remove(&surface);
        self.agents
            .cleared_agent_saw_no_signal
            .lock()
            .await
            .remove(&surface);
    }

    async fn suppress_agent_screen_restore(&self, surface: SurfaceId) {
        let baseline = self
            .agents
            .last_agent_screen_fingerprints
            .lock()
            .await
            .get(&surface)
            .copied();
        self.agents
            .cleared_agent_surfaces
            .lock()
            .await
            .insert(surface);
        self.agents
            .cleared_agent_screen_fingerprints
            .lock()
            .await
            .insert(surface, baseline);
        self.agents
            .cleared_agent_saw_no_signal
            .lock()
            .await
            .remove(&surface);
    }

    async fn is_local_agent_surface(&self, surface: SurfaceId) -> bool {
        let state = self.inner.lock().await;
        agent_presence_slot_in_state(&state, surface).is_some()
    }

    /// Set (or clear, with `None`) the live AI-agent presence on the tab
    /// surface `surface_id`. Returns the owning workspace id so the
    /// caller can route a sidebar update. Deliberately does **not**
    /// `mark_dirty`: agent presence is runtime-only (`#[serde(skip)]`),
    /// so there is nothing to persist and we avoid disk churn on every
    /// status flip.
    pub async fn set_agent_activity(
        &self,
        surface_id: SurfaceId,
        agent: Option<AgentPresence>,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.lock().await;
        let mut found = None;
        for ws in s
            .workspaces
            .iter_mut()
            .filter(|ws| ws.local_root().is_some())
        {
            for surface in ws.surfaces.iter_mut() {
                if surface
                    .root_pane
                    .set_surface_agent(surface_id, agent.clone())
                {
                    found = Some(ws.id);
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        drop(s);
        if found.is_some() {
            if agent.is_some() {
                self.allow_agent_screen_restore(surface_id).await;
            } else {
                self.suppress_agent_screen_restore(surface_id).await;
                self.clear_agent_lifecycle(surface_id, None, None, None)
                    .await;
            }
        }
        found
    }

    pub async fn located_agent_presence(
        &self,
        surface_id: SurfaceId,
    ) -> Option<LocatedAgentPresence> {
        let s = self.inner.lock().await;
        s.workspaces
            .iter()
            .filter(|ws| ws.local_root().is_some())
            .find_map(|workspace| {
                workspace.surfaces.iter().find_map(|surface| {
                    located_agent_in_pane(&surface.root_pane, surface_id).map(
                        |(pane, surface_label, presence)| LocatedAgentPresence {
                            workspace: workspace.id,
                            pane,
                            surface: surface_id,
                            workspace_label: workspace.display_title().to_string(),
                            surface_label,
                            color: workspace
                                .color
                                .clone()
                                .unwrap_or_else(|| agent_bar_color_for_surface(surface_id)),
                            presence,
                        },
                    )
                })
            })
    }

    /// Remove a hook-owned presence only when the teardown still belongs to
    /// the current agent session. This keeps a delayed SessionEnd from clearing
    /// a newer session that reused the same tab.
    pub async fn end_agent_session(
        &self,
        surface_id: SurfaceId,
        agent: &str,
        seq: Option<u64>,
        session_id: Option<&str>,
        expected_pid: Option<u32>,
    ) -> Option<LocatedAgentPresence> {
        self.remove_agent_presence_if_current(
            surface_id,
            Some(agent),
            seq,
            session_id,
            expected_pid,
            true,
        )
        .await
    }

    /// Remove a presence whose recorded process has exited. PID liveness is the
    /// authority here, so no hook sequence/session constraint is needed.
    pub async fn clear_dead_agent_presence(
        &self,
        surface_id: SurfaceId,
        expected_pid: u32,
    ) -> Option<LocatedAgentPresence> {
        self.remove_agent_presence_if_current(
            surface_id,
            None,
            None,
            None,
            Some(expected_pid),
            true,
        )
        .await
    }

    async fn remove_agent_presence_if_current(
        &self,
        surface_id: SurfaceId,
        agent: Option<&str>,
        seq: Option<u64>,
        session_id: Option<&str>,
        expected_pid: Option<u32>,
        suppress_screen_restore: bool,
    ) -> Option<LocatedAgentPresence> {
        // Serialize teardown against lifecycle reports. Otherwise a delayed
        // pre-end event can recreate a presence in the gap between removal
        // and ledger cleanup.
        let mut lifecycle = self.agents.lifecycle.lock().await;
        let removed = {
            let mut s = self.inner.lock().await;
            s.workspaces.iter_mut().find_map(|workspace| {
                let workspace_id = workspace.id;
                let workspace_label = workspace.display_title().to_string();
                let color = workspace
                    .color
                    .clone()
                    .unwrap_or_else(|| agent_bar_color_for_surface(surface_id));
                workspace.surfaces.iter_mut().find_map(|surface| {
                    take_current_agent_from_pane(
                        &mut surface.root_pane,
                        surface_id,
                        agent,
                        seq,
                        session_id,
                        expected_pid,
                    )
                    .map(|(pane, surface_label, presence)| {
                        LocatedAgentPresence {
                            workspace: workspace_id,
                            pane,
                            surface: surface_id,
                            workspace_label: workspace_label.clone(),
                            surface_label,
                            color: color.clone(),
                            presence,
                        }
                    })
                })
            })
        };
        if let Some(removed_presence) = removed.as_ref() {
            let removed_agent = removed_presence.presence.name.to_ascii_lowercase();
            let removed_session = removed_presence.presence.session_id.as_deref();
            clear_agent_lifecycle_runtime(
                &mut lifecycle,
                surface_id,
                Some(&removed_agent),
                removed_session,
                None,
            );
            if let Some(removed_session) = removed_session {
                remember_ended_agent_lifecycle(
                    &mut lifecycle,
                    (surface_id, removed_agent, removed_session.to_string()),
                    removed_presence.presence.pid,
                    seq.or(removed_presence.presence.seq),
                    false,
                );
            }
        }
        if removed.is_some() {
            self.mark_dirty();
            if suppress_screen_restore {
                self.suppress_agent_screen_restore(surface_id).await;
            } else {
                self.allow_agent_screen_restore(surface_id).await;
            }
        }
        drop(lifecycle);
        removed
    }

    /// Merge a live agent status report into a tab surface. Returns the owning
    /// workspace and its rolled-up agent status when the report was accepted.
    /// Stale sequence numbers are ignored and return `None`.
    pub async fn report_agent_status(
        &self,
        surface_id: SurfaceId,
        report: AgentStatusReport,
    ) -> Option<(WorkspaceId, Option<AgentStatus>)> {
        let surface_visible = self.surface_is_in_active_workspace(surface_id).await;
        self.report_agent_status_with_visibility(surface_id, report, surface_visible)
            .await
    }

    /// Apply a correlated hook event under one daemon-side lock. This prevents
    /// independently spawned hook processes from resolving another parallel
    /// tool's permission wait or settling a Codex parent turn while an observed
    /// subagent is still active.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(target = "flowmux_agent", level = "debug", skip_all,
        fields(instance = std::process::id(), version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS, surface = %surface_id, evidence = "hook",
            session_key = ?trace_key(Some(session_id)), event = ?lifecycle_trace_event(&lifecycle_event),
            provider_key = ?trace_key(Some(agent)), seq = ?seq, surface_visible = surface_visible))]
    pub async fn report_agent_lifecycle_with_visibility(
        &self,
        surface_id: SurfaceId,
        agent: &str,
        pid: Option<u32>,
        seq: Option<u64>,
        session_id: &str,
        lifecycle_event: AgentLifecycleEvent,
        surface_visible: bool,
    ) -> AgentLifecycleResult {
        use flowmux_core::AgentActivity::{Idle, NeedsInput, Running};
        if !self.is_local_agent_surface(surface_id).await {
            tracing::debug!(target: "flowmux_agent", reason = "unsupported_surface", changed = false, "agent decision");
            return AgentLifecycleResult::default();
        }

        let agent = agent.to_ascii_lowercase();
        let mut runtime = self.agents.lifecycle.lock().await;
        let wait_key = (surface_id, agent.clone(), session_id.to_string());
        let current = self.located_agent_presence(surface_id).await;
        trace_agent_before(current.as_ref());
        // SessionEnd/dead-PID teardown leaves a bounded tombstone. A native
        // SessionStart normally establishes the next epoch. The one exception
        // is a live outer agent returning after a nested agent owned the pane:
        // process polling must first restore the same process identity, and the
        // returning hook must match the displaced live PID with a newer event.
        if let Some(ended) = runtime.ended.get(&wait_key) {
            let can_reactivate = ended.reactivate_on_process_return
                && current.as_ref().is_some_and(|located| {
                    located.presence.name.eq_ignore_ascii_case(&agent)
                        && located.presence.source.as_deref()
                            == Some(flowmux_core::AGENT_SOURCE_PROC)
                })
                && ended.pid.zip(pid).is_some_and(|(ended, incoming)| {
                    ended == incoming && flowmux_procmon::pid_alive(ended)
                })
                && seq.is_some_and(|incoming| ended.seq.is_none_or(|floor| incoming > floor));
            if !can_reactivate {
                tracing::debug!(target: "flowmux_agent", reason = "ended_session", changed = false, "agent decision");
                return AgentLifecycleResult::default();
            }
            runtime.ended.remove(&wait_key);
            runtime.ended_order.retain(|ended| ended != &wait_key);
        }
        if agent == "codex"
            && runtime
                .codex_turns
                .get(&(surface_id, session_id.to_string()))
                .and_then(|ledger| ledger.owner_pid)
                .zip(pid)
                .is_some_and(|(owner, incoming)| owner != incoming)
        {
            tracing::debug!(target: "flowmux_agent", reason = "owner_pid_mismatch", changed = false, "agent decision");
            return AgentLifecycleResult::default();
        }
        if current.as_ref().is_some_and(|located| {
            let presence = &located.presence;
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
            return AgentLifecycleResult::default();
        }
        let terminal_boundary = matches!(
            &lifecycle_event,
            AgentLifecycleEvent::TurnStopped { .. }
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
            return AgentLifecycleResult::default();
        }
        let runtime_before = current
            .is_none()
            .then(|| LifecycleCheckpoint::capture(&runtime, &wait_key));
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
        let mut completed = false;
        let mut turn_finished = false;
        let turn_boundary_seq = seq;
        let mut settle_codex_after_grace = None;
        let mut completion_message = None;
        let mut no_transition_reason = "ledger_only";
        let decision = match lifecycle_event {
            AgentLifecycleEvent::TurnStarted {
                turn_id,
                status_text,
            } => {
                runtime.waits.remove(&wait_key);
                if agent == "codex" {
                    clear_codex_root_permission_scopes(&mut runtime, &wait_key);
                    let ledger = runtime
                        .codex_turns
                        .entry((surface_id, session_id.to_string()))
                        .or_default();
                    set_codex_ledger_owner_if_missing(ledger, pid);
                    ledger.pending_parent_stop = None;
                    if turn_id.is_some() {
                        ledger.current_parent_turn = turn_id;
                    }
                } else {
                    runtime.permission_waits.remove(&wait_key);
                    clear_permission_event_seq_for_key(&mut runtime, &wait_key);
                    runtime.session_waits.remove(&wait_key);
                    clear_session_wait_event_seq_for_key(&mut runtime, &wait_key);
                }
                Some((Running, None, status_text))
            }
            AgentLifecycleEvent::ProgressObserved { status_text } => {
                if agent == "codex" {
                    if let Some(ledger) = runtime
                        .codex_turns
                        .get_mut(&(surface_id, session_id.to_string()))
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
                (!lifecycle_has_waits(&runtime, &wait_key)).then_some((Running, None, status_text))
            }
            AgentLifecycleEvent::CodexRootProgressObserved {
                turn_id,
                status_text,
            } => {
                if agent == "codex" {
                    if let Some(ledger) = runtime
                        .codex_turns
                        .get_mut(&(surface_id, session_id.to_string()))
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
                (!lifecycle_has_waits(&runtime, &wait_key)).then_some((Running, None, status_text))
            }
            AgentLifecycleEvent::PermissionWaitStarted {
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
                    runtime
                        .permission_waits
                        .entry(wait_key.clone())
                        .or_default()
                        .insert(scope);
                    Some((NeedsInput, message, status_text))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::SessionWaitStarted {
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
                    runtime
                        .session_waits
                        .entry(wait_key.clone())
                        .or_default()
                        .insert(scope);
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
                    clear_session_wait_scope(&mut runtime, &wait_key, &scope);
                    if !resume {
                        if let Some(seq) = seq {
                            runtime.boundary_seq.insert(wait_key.clone(), seq);
                        }
                    }
                    (!lifecycle_has_waits(&runtime, &wait_key)).then_some((
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
                    clear_permission_scope(&mut runtime, &wait_key, SESSION_PERMISSION_SCOPE);
                    (!lifecycle_has_waits(&runtime, &wait_key)).then_some((
                        Running,
                        None,
                        status_text,
                    ))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::WaitStarted {
                item_id,
                message,
                status_text,
            } => {
                let waits = runtime.waits.entry(wait_key.clone()).or_default();
                if let std::collections::hash_map::Entry::Vacant(wait) = waits.entry(item_id) {
                    wait.insert(true);
                    Some((NeedsInput, message, status_text))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::WaitResolved { item_id } => {
                let waits = runtime.waits.entry(wait_key.clone()).or_default();
                let resolved_active = waits.insert(item_id, false) == Some(true);
                if resolved_active && !lifecycle_has_waits(&runtime, &wait_key) {
                    Some((Running, None, "Working".into()))
                } else {
                    None
                }
            }
            AgentLifecycleEvent::TurnStopped {
                message,
                status_text,
            } if agent != "codex" => {
                turn_finished = true;
                completed = true;
                completion_message = message.clone();
                Some((Idle, message, status_text))
            }
            AgentLifecycleEvent::CodexSubagentStarted { agent_id, turn_id }
            | AgentLifecycleEvent::CodexChildProgressObserved { agent_id, turn_id }
                if agent == "codex" =>
            {
                let child_key = (agent_id.clone(), turn_id.clone());
                let has_waits = lifecycle_has_waits(&runtime, &wait_key);
                let ledger = runtime
                    .codex_turns
                    .entry((surface_id, session_id.to_string()))
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
                    return AgentLifecycleResult::default();
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
                        .entry((surface_id, session_id.to_string()))
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
                        return AgentLifecycleResult::default();
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
                        return AgentLifecycleResult::default();
                    }
                    ledger.active_children.remove(&agent_id);
                    let remaining = ledger.active_children.len();
                    let pending = (remaining == 0)
                        .then(|| ledger.pending_parent_stop.clone())
                        .flatten();
                    (remaining, pending)
                };
                clear_permission_scope_if_newer(
                    &mut runtime,
                    &wait_key,
                    &format!("child:{agent_id}:{turn_id}"),
                    seq,
                );
                if let Some(pending) = pending {
                    let ledger = runtime
                        .codex_turns
                        .get_mut(&(surface_id, session_id.to_string()))
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
                        (!lifecycle_has_waits(&runtime, &wait_key)).then_some((
                            Running,
                            None,
                            "Finishing Codex turn".into(),
                        ))
                    }
                } else if lifecycle_has_waits(&runtime, &wait_key) {
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
                let ledger_key = (surface_id, session_id.to_string());
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
                        &mut runtime,
                        &wait_key,
                        &format!("root:{turn_id}"),
                        seq,
                    );
                    let has_waits = lifecycle_has_waits(&runtime, &wait_key);
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
                let ledger_key = (surface_id, session_id.to_string());
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
                        &mut runtime,
                        &wait_key,
                        &format!("root:{turn_id}"),
                        seq,
                    );
                    let has_waits = lifecycle_has_waits(&runtime, &wait_key);
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
            // A root boundary is session-wide, but Codex shares the session id
            // with child threads. Preserve child waits while parent completion
            // is deferred; clear them only at actual turn settlement.
            runtime.waits.remove(&wait_key);
            runtime.permission_waits.remove(&wait_key);
            clear_permission_event_seq_for_key(&mut runtime, &wait_key);
            runtime.session_waits.remove(&wait_key);
            clear_session_wait_event_seq_for_key(&mut runtime, &wait_key);
            if let Some(seq) = turn_boundary_seq {
                runtime.boundary_seq.insert(wait_key.clone(), seq);
            }
        }

        let Some((activity, message, custom_status)) = decision else {
            let reason = if lifecycle_has_waits(&runtime, &wait_key) {
                "pending_waits"
            } else {
                no_transition_reason
            };
            tracing::debug!(target: "flowmux_agent", reason, changed = false,
                deferred = settle_codex_after_grace.is_some(), "agent decision");
            return AgentLifecycleResult {
                settle_codex_after_grace,
                ..AgentLifecycleResult::default()
            };
        };
        let report_seq = if seq
            .zip(current.as_ref().and_then(|located| located.presence.seq))
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
            session_id: Some(session_id.to_string()),
            session_name: None,
            messaging_socket: None,
        };
        let workspace = self
            .apply_agent_status_report(surface_id, report, surface_visible)
            .await
            .map(|(workspace, _)| workspace);
        if let Some(before) = runtime_before.filter(|_| workspace.is_none()) {
            before.restore(&mut runtime);
            completed = false;
            completion_message = None;
        }
        if workspace.is_some() {
            self.allow_agent_screen_restore(surface_id).await;
        }
        drop(runtime);
        AgentLifecycleResult {
            workspace,
            completed,
            completion_message,
            settle_codex_after_grace,
        }
    }

    #[tracing::instrument(target = "flowmux_agent", level = "debug", skip_all,
        fields(instance = std::process::id(), version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS, surface = %surface_id, evidence = "status_report",
            session_key = ?trace_key(report.session_id.as_deref()), seq = report.seq, surface_visible = surface_visible))]
    pub async fn report_agent_status_with_visibility(
        &self,
        surface_id: SurfaceId,
        report: AgentStatusReport,
        surface_visible: bool,
    ) -> Option<(WorkspaceId, Option<AgentStatus>)> {
        if !self.is_local_agent_surface(surface_id).await {
            tracing::debug!(target: "flowmux_agent", reason = "unsupported_surface", changed = false, "agent decision");
            return None;
        }
        // Only a native SessionStart carries both Ready and a session id.
        // Legacy wrapper starts are metadata-free and must not erase live
        // waits/children when they arrive late.
        let starts_native_session = report.source.as_deref() == Some("flowmux:hook")
            && report.custom_status.as_deref() == Some("Ready")
            && report.session_id.is_some();
        let hook_session = (report.source.as_deref() == Some("flowmux:hook"))
            .then(|| {
                report
                    .session_id
                    .as_ref()
                    .map(|session_id| (report.name.to_ascii_lowercase(), session_id.to_string()))
            })
            .flatten();
        let accepted = if starts_native_session {
            let mut lifecycle = self.agents.lifecycle.lock().await;
            let session_id = report.session_id.clone().unwrap();
            let agent = report.name.to_ascii_lowercase();
            let key = (surface_id, agent.clone(), session_id.to_string());
            let current = self.located_agent_presence(surface_id).await;
            let floor = lifecycle
                .last_seq
                .get(&key)
                .copied()
                .into_iter()
                .chain(lifecycle.ended.get(&key).and_then(|ended| ended.seq))
                .chain(current.as_ref().and_then(|located| located.presence.seq))
                .max();
            if match (floor, report.seq) {
                (Some(floor), Some(incoming)) => incoming <= floor,
                (Some(_), None) => true,
                _ => false,
            } {
                tracing::debug!(target: "flowmux_agent", reason = "stale_session_start", changed = false, "agent decision");
                return None;
            }
            let displaced = current.as_ref().and_then(|located| {
                let presence = &located.presence;
                presence.session_id.as_ref().and_then(|session| {
                    let displaced_key = (
                        surface_id,
                        presence.name.to_ascii_lowercase(),
                        session.clone(),
                    );
                    (displaced_key != key).then_some((displaced_key, presence.pid, presence.seq))
                })
            });
            let owner_pid = report.pid;
            let start_seq = report.seq;
            // A rejected SessionStart must leave the live waits untouched.
            // Commit the report before clearing ledgers, under the lifecycle lock.
            let accepted = self
                .apply_agent_status_report(surface_id, report, surface_visible)
                .await?;
            clear_agent_lifecycle_runtime(&mut lifecycle, surface_id, None, None, None);
            if let Some((displaced_key, displaced_pid, seq)) = displaced {
                let distinct_nested_identity = displaced_key.1 != agent
                    || displaced_pid
                        .zip(owner_pid)
                        .is_some_and(|(outer, inner)| outer != inner);
                let reactivates_after_process_return =
                    distinct_nested_identity && displaced_pid.is_some();
                remember_ended_agent_lifecycle(
                    &mut lifecycle,
                    displaced_key,
                    displaced_pid,
                    seq,
                    reactivates_after_process_return,
                );
            }
            lifecycle.ended.remove(&key);
            lifecycle.ended_order.retain(|ended| ended != &key);
            if let Some(seq) = start_seq {
                lifecycle.last_seq.insert(key.clone(), seq);
                lifecycle.boundary_seq.insert(key, seq);
            }
            if agent == "codex" {
                lifecycle.codex_turns.insert(
                    (surface_id, session_id.to_string()),
                    CodexTurnLedger {
                        owner_pid,
                        ..CodexTurnLedger::default()
                    },
                );
            }
            self.allow_agent_screen_restore(surface_id).await;
            drop(lifecycle);
            Some(accepted)
        } else if let Some((agent, session_id)) = hook_session {
            // Serialize every session-bearing direct report with lifecycle
            // teardown. This closes the gap where SessionEnd could tombstone a
            // session and a delayed Stop/notification would recreate it.
            let mut lifecycle = self.agents.lifecycle.lock().await;
            let key = (surface_id, agent.clone(), session_id.clone());
            if lifecycle.ended.contains_key(&key) {
                tracing::debug!(target: "flowmux_agent", reason = "ended_session", changed = false, "agent decision");
                return None;
            }
            if agent == "codex"
                && lifecycle
                    .codex_turns
                    .get(&(surface_id, session_id.clone()))
                    .and_then(|ledger| ledger.owner_pid)
                    .zip(report.pid)
                    .is_some_and(|(owner, incoming)| owner != incoming)
            {
                tracing::debug!(target: "flowmux_agent", reason = "owner_pid_mismatch", changed = false, "agent decision");
                return None;
            }
            let current = self.located_agent_presence(surface_id).await;
            if current.as_ref().is_some_and(|located| {
                let presence = &located.presence;
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
                    && presence
                        .pid
                        .zip(report.pid)
                        .is_some_and(|(current, incoming)| {
                            current != incoming && flowmux_procmon::pid_alive(current)
                        });
                authoritative_other_agent || other_session || live_other_pid
            }) {
                tracing::debug!(target: "flowmux_agent", reason = "identity_mismatch", changed = false, "agent decision");
                return None;
            }
            let boundary = lifecycle.boundary_seq.get(&key).copied();
            if match (boundary, report.seq) {
                (Some(floor), Some(incoming)) => incoming <= floor,
                (Some(_), None) => true,
                _ => false,
            } {
                tracing::debug!(target: "flowmux_agent", reason = "stale_sequence", changed = false, "agent decision");
                return None;
            }
            let terminal_idle = report.effective_status() == Some(AgentStatus::Idle);
            let report_seq = report.seq;
            let accepted = self
                .apply_agent_status_report(surface_id, report, surface_visible)
                .await;
            if accepted.is_some() {
                if let Some(seq) = report_seq {
                    lifecycle
                        .last_seq
                        .entry(key.clone())
                        .and_modify(|current| *current = (*current).max(seq))
                        .or_insert(seq);
                    if terminal_idle {
                        lifecycle.boundary_seq.insert(key.clone(), seq);
                    }
                }
                if terminal_idle {
                    lifecycle.waits.remove(&key);
                    lifecycle.permission_waits.remove(&key);
                    clear_permission_event_seq_for_key(&mut lifecycle, &key);
                    lifecycle.session_waits.remove(&key);
                    clear_session_wait_event_seq_for_key(&mut lifecycle, &key);
                }
                self.allow_agent_screen_restore(surface_id).await;
            }
            drop(lifecycle);
            accepted
        } else {
            let accepted = self
                .apply_agent_status_report(surface_id, report, surface_visible)
                .await;
            if accepted.is_some() {
                self.allow_agent_screen_restore(surface_id).await;
            }
            accepted
        };
        accepted
    }

    /// Settle a Codex root Stop after a short ingress grace period. A child is
    /// spawned before its SubagentStart hook is dispatched, so the Stop hook
    /// can otherwise observe an empty child set and publish a false completion.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(target = "flowmux_agent", level = "debug", skip_all,
        fields(instance = std::process::id(), version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS, surface = %surface_id, evidence = "hook_grace",
            session_key = ?trace_key(Some(session_id)), turn_key = ?trace_key(Some(turn_id)), seq = ?seq))]
    pub async fn settle_codex_turn_after_grace(
        &self,
        surface_id: SurfaceId,
        pid: Option<u32>,
        seq: Option<u64>,
        session_id: &str,
        turn_id: &str,
        surface_visible: bool,
    ) -> AgentLifecycleResult {
        use flowmux_core::AgentActivity::Idle;

        let mut runtime = self.agents.lifecycle.lock().await;
        let wait_key = (surface_id, "codex".to_string(), session_id.to_string());
        if runtime.ended.contains_key(&wait_key) {
            tracing::debug!(target: "flowmux_agent", reason = "ended_session", changed = false, "agent decision");
            return AgentLifecycleResult::default();
        }
        let current = self.located_agent_presence(surface_id).await;
        if !current.as_ref().is_some_and(|located| {
            located.presence.name.eq_ignore_ascii_case("codex")
                && located.presence.session_id.as_deref() == Some(session_id)
                && located
                    .presence
                    .pid
                    .zip(pid)
                    .is_none_or(|(current, incoming)| current == incoming)
        }) {
            tracing::debug!(target: "flowmux_agent", reason = "identity_mismatch", changed = false, "agent decision");
            return AgentLifecycleResult::default();
        }
        let before = LifecycleCheckpoint::capture(&runtime, &wait_key);
        let pending = {
            let Some(ledger) = runtime
                .codex_turns
                .get_mut(&(surface_id, session_id.to_string()))
            else {
                tracing::debug!(target: "flowmux_agent", reason = "no_turn_ledger", changed = false, "agent decision");
                return AgentLifecycleResult::default();
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
                return AgentLifecycleResult::default();
            }
            let pending = ledger.pending_parent_stop.take().unwrap();
            remember_settled_codex_turn(ledger, pending.turn_id.clone());
            ledger.current_parent_turn = None;
            pending
        };
        runtime.waits.remove(&wait_key);
        runtime.permission_waits.remove(&wait_key);
        clear_permission_event_seq_for_key(&mut runtime, &wait_key);
        runtime.session_waits.remove(&wait_key);
        clear_session_wait_event_seq_for_key(&mut runtime, &wait_key);
        if let Some(seq) = seq {
            runtime.boundary_seq.insert(wait_key.clone(), seq);
        }
        let report = AgentStatusReport {
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
        let workspace = self
            .apply_agent_status_report(surface_id, report, surface_visible)
            .await
            .map(|(workspace, _)| workspace);
        if workspace.is_none() {
            before.restore(&mut runtime);
            tracing::debug!(target: "flowmux_agent", reason = "report_rejected", changed = false, "agent decision");
            return AgentLifecycleResult::default();
        }
        self.allow_agent_screen_restore(surface_id).await;
        drop(runtime);
        AgentLifecycleResult {
            workspace,
            completed: pending.notify_completion,
            completion_message: pending
                .notify_completion
                .then_some(pending.message)
                .flatten(),
            settle_codex_after_grace: None,
        }
    }

    async fn apply_agent_status_report(
        &self,
        surface_id: SurfaceId,
        mut report: AgentStatusReport,
        surface_visible: bool,
    ) -> Option<(WorkspaceId, Option<AgentStatus>)> {
        let mut s = self.inner.lock().await;
        let mut accepted = None;
        for ws in s
            .workspaces
            .iter_mut()
            .filter(|ws| ws.local_root().is_some())
        {
            let mut found = false;
            let mut changed = false;
            for surface in ws.surfaces.iter_mut() {
                let previous = surface.root_pane.agent_presence_for_surface(surface_id);
                if let Some(existing) = &previous {
                    preserve_live_agent_pid(&mut report, existing);
                }
                if let Some(applied) = surface.root_pane.report_surface_agent(
                    surface_id,
                    report.clone(),
                    surface_visible,
                ) {
                    tracing::debug!(target: "flowmux_agent", surface = %surface_id,
                        workspace = %ws.id, previous = ?previous.as_ref().map(|p| p.status),
                        next = ?surface.root_pane.agent_presence_for_surface(surface_id).map(|p| p.status),
                        changed = applied, reason = "report_result", "agent decision");
                    found = true;
                    changed = applied;
                    break;
                }
            }
            if found {
                if changed {
                    accepted = Some((ws.id, ws.agent_status_rollup()));
                }
                break;
            }
        }
        accepted
    }

    /// Compatibility entry point for callers with at most one process-derived
    /// identity per surface. New process-tree scans should use
    /// [`Self::reconcile_process_agent_candidates`] so a nested hook identity
    /// is not discarded before reconciliation.
    pub async fn reconcile_process_agents(
        &self,
        detected: &[(SurfaceId, Option<&str>)],
    ) -> Vec<(WorkspaceId, Option<AgentStatus>)> {
        let candidates: Vec<_> = detected
            .iter()
            .map(|(surface, name)| (*surface, name.iter().copied().collect::<Vec<_>>()))
            .collect();
        self.reconcile_process_agent_candidates(&candidates).await
    }

    /// Snapshot the live agent slot for each terminal before process-tree
    /// inspection leaves the state lock. The process walk runs on a blocking
    /// worker, so callers must pair these tokens with
    /// [`Self::reconcile_process_agent_candidates_if_unchanged`] to avoid
    /// applying a result that predates a native hook transition.
    pub async fn agent_process_reconciliation_snapshot(
        &self,
        surfaces: &[SurfaceId],
    ) -> Vec<(SurfaceId, Option<AgentPresence>)> {
        let s = self.inner.lock().await;
        surfaces
            .iter()
            .filter_map(|surface| {
                agent_presence_slot_in_state(&s, *surface).map(|presence| (*surface, presence))
            })
            .collect()
    }

    /// Reconcile Agent Bar presence against all process-tree identities for a
    /// batch of terminal surfaces. Candidates are ordered deepest-first by the
    /// process monitor. A matching native hook identity is retained wherever
    /// it appears in the list; without one, the first candidate is selected.
    pub async fn reconcile_process_agent_candidates(
        &self,
        detected: &[(SurfaceId, Vec<&str>)],
    ) -> Vec<(WorkspaceId, Option<AgentStatus>)> {
        self.reconcile_process_agent_candidates_inner(detected, None, None)
            .await
    }

    /// Apply a process-tree snapshot only to surfaces whose agent slot still
    /// matches the value observed before the blocking process walk. A native
    /// SessionStart that lands during that walk must not be displaced and
    /// tombstoned by older process truth. `screen_fallback` contains surfaces
    /// with a live SSH/tmux/container transport; all other absent local agents
    /// must stay absent even when terminal history still shows their UI.
    pub async fn reconcile_process_agent_candidates_if_unchanged(
        &self,
        detected: &[(SurfaceId, Vec<&str>)],
        observed: &[(SurfaceId, Option<AgentPresence>)],
        screen_fallback: &[SurfaceId],
    ) -> Vec<(WorkspaceId, Option<AgentStatus>)> {
        self.reconcile_process_agent_candidates_inner(
            detected,
            Some(observed),
            Some(screen_fallback),
        )
        .await
    }

    async fn reconcile_process_agent_candidates_inner(
        &self,
        detected: &[(SurfaceId, Vec<&str>)],
        observed: Option<&[(SurfaceId, Option<AgentPresence>)]>,
        screen_fallback: Option<&[SurfaceId]>,
    ) -> Vec<(WorkspaceId, Option<AgentStatus>)> {
        let mut changed: Vec<(WorkspaceId, Option<AgentStatus>)> = Vec::new();
        let mut created_surfaces: Vec<SurfaceId> = Vec::new();
        // Process truth may replace a hook-owned identity. Serialize the whole
        // mutation with hook lifecycle work (lifecycle -> state lock order) so
        // a handler cannot validate the old owner and overwrite the replacement.
        let mut lifecycle = self.agents.lifecycle.lock().await;
        {
            let mut s = self.inner.lock().await;
            for (surface_id, candidates) in detected {
                let changed_during_scan = observed
                    .and_then(|observed| {
                        observed
                            .iter()
                            .find(|(observed_surface, _)| observed_surface == surface_id)
                    })
                    .is_some_and(|(_, observed_presence)| {
                        agent_presence_slot_in_state(&s, *surface_id).as_ref()
                            != Some(observed_presence)
                    });
                if changed_during_scan {
                    tracing::debug!(target: "flowmux_agent", surface = %surface_id,
                        evidence = "process", reason = "changed_during_scan", changed = false,
                        "agent decision");
                    continue;
                }
                if let Some(screen_fallback) = screen_fallback {
                    if candidates.is_empty() && !screen_fallback.contains(surface_id) {
                        lifecycle.screen_absent.insert(*surface_id);
                    } else {
                        lifecycle.screen_absent.remove(surface_id);
                    }
                }
                for ws in s
                    .workspaces
                    .iter_mut()
                    .filter(|ws| ws.local_root().is_some())
                {
                    let mut applied = None;
                    for surface in ws.surfaces.iter_mut() {
                        let previous = surface.root_pane.agent_presence_for_surface(*surface_id);
                        let name = select_process_agent_candidate(previous.as_ref(), candidates);
                        let cleared_screen = lifecycle.screen_absent.contains(surface_id)
                            && surface
                                .root_pane
                                .clear_surface_agent_from_source(*surface_id, "flowmux:screen")
                                == Some(true);
                        if let Some(result) =
                            surface.root_pane.reconcile_process_agent(*surface_id, name)
                        {
                            applied = Some((result || cleared_screen, previous, name));
                            break;
                        }
                    }
                    if let Some((result, previous, name)) = applied {
                        if result {
                            tracing::debug!(target: "flowmux_agent", surface = %surface_id,
                                workspace = %ws.id, evidence = "process",
                                previous = ?previous.as_ref().map(|p| p.status),
                                next = ?ws.surfaces.iter().find_map(|s| s.root_pane.agent_presence_for_surface(*surface_id)).map(|p| p.status),
                                candidate_present = name.is_some(), changed = true,
                                reason = "process_reconciled", "agent decision");
                            changed.push((ws.id, ws.agent_status_rollup()));
                            if name.is_some() {
                                created_surfaces.push(*surface_id);
                            }
                            if let Some(previous) = previous {
                                let displaced = name.is_none_or(|detected| {
                                    !previous.name.eq_ignore_ascii_case(detected)
                                });
                                if displaced {
                                    if let Some(session_id) = previous.session_id.as_deref() {
                                        let old_key = (
                                            *surface_id,
                                            previous.name.to_ascii_lowercase(),
                                            session_id.to_string(),
                                        );
                                        clear_agent_lifecycle_runtime(
                                            &mut lifecycle,
                                            *surface_id,
                                            Some(&previous.name),
                                            Some(session_id),
                                            previous.pid,
                                        );
                                        remember_ended_agent_lifecycle(
                                            &mut lifecycle,
                                            old_key,
                                            previous.pid,
                                            previous.seq,
                                            false,
                                        );
                                    } else {
                                        clear_agent_lifecycle_runtime(
                                            &mut lifecycle,
                                            *surface_id,
                                            None,
                                            None,
                                            None,
                                        );
                                    }
                                }
                            }
                        }
                        break;
                    }
                }
            }
        }
        // A live agent process is ground truth: unblock the screen-refinement
        // path that a prior hook SessionEnd may have latched for this surface,
        // so working/idle status can track again.
        for surface_id in created_surfaces {
            self.allow_agent_screen_restore(surface_id).await;
        }
        drop(lifecycle);
        if !changed.is_empty() {
            self.mark_dirty();
        }
        changed
    }

    pub async fn report_agent_screen_signals(
        &self,
        surface_id: SurfaceId,
        screen_text: Option<&str>,
        osc_title: Option<&str>,
    ) -> Option<(WorkspaceId, Option<AgentStatus>)> {
        let surface_visible = self.surface_is_in_active_workspace(surface_id).await;
        self.report_agent_screen_signals_with_visibility(
            surface_id,
            screen_text,
            osc_title,
            surface_visible,
        )
        .await
    }

    #[tracing::instrument(target = "flowmux_agent", level = "debug", skip_all,
        fields(instance = std::process::id(), version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS, surface = %surface_id, evidence = "screen", surface_visible = surface_visible))]
    pub async fn report_agent_screen_signals_with_visibility(
        &self,
        surface_id: SurfaceId,
        screen_text: Option<&str>,
        osc_title: Option<&str>,
        surface_visible: bool,
    ) -> Option<(WorkspaceId, Option<AgentStatus>)> {
        // Screen evidence is already scoped to a tab and carries no local PID.
        // SSH remains excluded from process scans and native hook reports.
        let lifecycle = self.agents.lifecycle.lock().await;
        if tracing::enabled!(target: "flowmux_agent", tracing::Level::DEBUG) {
            trace_agent_before(self.located_agent_presence(surface_id).await.as_ref());
        }
        if !self.is_local_agent_surface(surface_id).await && !self.is_ssh_surface(surface_id).await
        {
            tracing::debug!(target: "flowmux_agent", reason = "unsupported_surface", changed = false, "agent decision");
            return None;
        }
        if lifecycle.screen_absent.contains(&surface_id)
            && self.located_agent_presence(surface_id).await.is_none()
        {
            tracing::debug!(target: "flowmux_agent", reason = "process_absent", changed = false, "agent decision");
            return None;
        }
        let fingerprint = agent_screen_fingerprint(screen_text, osc_title);
        let previous_fingerprint = self
            .agents
            .last_agent_screen_fingerprints
            .lock()
            .await
            .insert(surface_id, fingerprint);
        let detected_status = detect_agent_status_from_signals(screen_text, osc_title);
        let completed_agent = (detected_status == Some(AgentStatus::Idle))
            .then(|| detect_agent_completion(screen_text))
            .flatten();
        let status_text = match detected_status {
            Some(AgentStatus::Working) => detect_agent_progress_text(screen_text),
            Some(AgentStatus::Blocked) => detect_agent_usage_limit_text(screen_text),
            Some(AgentStatus::Idle) if detect_agent_interruption(screen_text) => {
                Some("Interrupted")
            }
            Some(AgentStatus::Idle) if completed_agent.is_some() => Some("Completed"),
            _ => None,
        };
        // The composer stays visible while working: it identifies the agent
        // even when the progress row and tmux title do not contain its name.
        let prompt_agent_name = detect_agent_idle_name_from_signals(screen_text, osc_title);
        let status = detected_status.or_else(|| prompt_agent_name.map(|_| AgentStatus::Idle));
        let agent_name = if status.is_some() {
            completed_agent
                .or_else(|| detect_agent_name_from_signals(screen_text, osc_title))
                .or(prompt_agent_name)
        } else {
            None
        };
        if status.is_none() {
            if self
                .agents
                .cleared_agent_surfaces
                .lock()
                .await
                .contains(&surface_id)
            {
                // Seeing a plain shell/non-agent frame after teardown is strong
                // evidence that any later agent frame belongs to a new remote
                // or screen-only session, including one that first appears
                // blocked rather than working.
                self.agents
                    .cleared_agent_screen_fingerprints
                    .lock()
                    .await
                    .insert(surface_id, Some(fingerprint));
                self.agents
                    .cleared_agent_saw_no_signal
                    .lock()
                    .await
                    .insert(surface_id);
            }
            tracing::debug!(target: "flowmux_agent", reason = "no_screen_signal", "agent decision");
            drop(lifecycle);
            return self
                .clear_screen_agent_signal(surface_id, surface_visible)
                .await;
        }
        let status = status?;
        // Serialize the wait check, teardown latch check, and screen mutation
        // with SessionEnd/dead-PID teardown. Otherwise teardown can remove the
        // presence after this check but before the state lock below, allowing
        // the stale screen frame to recreate a ghost presence.
        if completed_agent.is_some()
            && previous_fingerprint == Some(fingerprint)
            && self
                .located_agent_presence(surface_id)
                .await
                .is_some_and(|located| {
                    located.presence.status == AgentStatus::Working
                        && located.presence.custom_status.as_deref() == Some("Starting turn")
                })
        {
            // A new hook turn must not be completed by its unchanged old footer.
            // Ordinary late tool hooks still allow the live footer to recover.
            tracing::debug!(target: "flowmux_agent", reason = "unchanged_completion_new_turn", changed = false, "agent decision");
            return None;
        }
        if status == AgentStatus::Idle
            && lifecycle.codex_turns.iter().any(|((surface, _), ledger)| {
                *surface == surface_id && !ledger.active_children.is_empty()
            })
        {
            tracing::debug!(target: "flowmux_agent", reason = "active_children", changed = false, "agent decision");
            return None;
        }
        if status != AgentStatus::Blocked
            && (lifecycle.waits.iter().any(|((surface, _, _), waits)| {
                *surface == surface_id && waits.values().any(|waiting| *waiting)
            }) || lifecycle
                .permission_waits
                .iter()
                .any(|((surface, _, _), scopes)| *surface == surface_id && !scopes.is_empty())
                || lifecycle
                    .session_waits
                    .iter()
                    .any(|((surface, _, _), scopes)| *surface == surface_id && !scopes.is_empty()))
        {
            // Hook waits are authoritative. A spinner or stale completion line
            // must not visually clear a real permission/input prompt.
            tracing::debug!(target: "flowmux_agent", reason = "pending_waits", changed = false, "agent decision");
            return None;
        }
        if self
            .agents
            .cleared_agent_surfaces
            .lock()
            .await
            .contains(&surface_id)
        {
            let baseline = self
                .agents
                .cleared_agent_screen_fingerprints
                .lock()
                .await
                .get(&surface_id)
                .copied()
                .flatten();
            let screen_changed_after_clear = baseline.is_some_and(|old| old != fingerprint);
            let saw_no_agent_frame = self
                .agents
                .cleared_agent_saw_no_signal
                .lock()
                .await
                .contains(&surface_id);
            if !screen_changed_after_clear || !saw_no_agent_frame {
                // A dead TUI can leave changing spinner/"Working" frames in
                // scrollback. Require a plain shell/non-agent frame before
                // treating later screen signals as a new remote agent.
                self.agents
                    .cleared_agent_screen_fingerprints
                    .lock()
                    .await
                    .insert(surface_id, Some(fingerprint));
                tracing::debug!(target: "flowmux_agent", reason = "session_teardown", changed = false, "agent decision");
                return None;
            }
            self.allow_agent_screen_restore(surface_id).await;
        }
        let mut s = self.inner.lock().await;
        let settled_codex_turn = status == AgentStatus::Working
            && s.workspaces.iter().any(|ws| {
                ws.surfaces.iter().any(|surface| {
                    surface
                        .root_pane
                        .agent_presence_for_surface(surface_id)
                        .is_some_and(|presence| {
                            presence.name == "codex"
                                && presence.source.as_deref() == Some("flowmux:hook")
                                && presence.session_id.as_deref().is_some_and(|session_id| {
                                    lifecycle
                                        .codex_turns
                                        .get(&(surface_id, session_id.to_string()))
                                        .is_some_and(|ledger| {
                                            !ledger.settled_parent_turns.is_empty()
                                                && ledger.current_parent_turn.is_none()
                                                && ledger.pending_parent_stop.is_none()
                                                && ledger.active_children.is_empty()
                                        })
                                })
                        })
                })
            });
        if settled_codex_turn {
            // The native lifecycle already settled this Codex turn and no new
            // turn has started. The TUI repaints its last spinner frame after
            // the Stop hook returns, so a screen Working here is stale and
            // must not reopen the turn: a bare prompt cannot clear hook
            // Working, so nothing would ever settle it again.
            tracing::debug!(target: "flowmux_agent", reason = "settled_turn_spinner", changed = false, "agent decision");
            return None;
        }
        let mut outcome = None;
        for ws in s.workspaces.iter_mut() {
            let mut found = false;
            let mut changed = false;
            for surface in ws.surfaces.iter_mut() {
                let previous = tracing::enabled!(target: "flowmux_agent", tracing::Level::DEBUG)
                    .then(|| surface.root_pane.agent_presence_for_surface(surface_id))
                    .flatten();
                if let Some(applied) = surface.root_pane.report_surface_agent_signal(
                    surface_id,
                    status,
                    "flowmux:screen",
                    agent_name,
                    status_text,
                    surface_visible,
                ) {
                    tracing::debug!(target: "flowmux_agent", workspace = %ws.id,
                        previous = ?previous.as_ref().map(|p| p.status), detected = ?status,
                        next = ?surface.root_pane.agent_presence_for_surface(surface_id).map(|p| p.status),
                        changed = applied, reason = "screen_result", "agent decision");
                    found = true;
                    changed = applied;
                    break;
                }
            }
            if found {
                if changed {
                    outcome = Some((ws.id, ws.agent_status_rollup()));
                }
                break;
            }
        }
        drop(s);
        drop(lifecycle);
        outcome
    }

    async fn clear_screen_agent_signal(
        &self,
        surface_id: SurfaceId,
        surface_visible: bool,
    ) -> Option<(WorkspaceId, Option<AgentStatus>)> {
        // A blank/unrecognized frame is weaker than an idle composer. It must
        // not bypass the correlated-work guards used by recognized frames.
        let lifecycle = self.agents.lifecycle.lock().await;
        if lifecycle.codex_turns.iter().any(|((surface, _), ledger)| {
            *surface == surface_id && !ledger.active_children.is_empty()
        }) || lifecycle.waits.iter().any(|((surface, _, _), waits)| {
            *surface == surface_id && waits.values().any(|waiting| *waiting)
        }) || lifecycle
            .permission_waits
            .iter()
            .any(|((surface, _, _), scopes)| *surface == surface_id && !scopes.is_empty())
            || lifecycle
                .session_waits
                .iter()
                .any(|((surface, _, _), scopes)| *surface == surface_id && !scopes.is_empty())
        {
            return None;
        }
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            let mut found = false;
            let mut changed = false;
            for surface in ws.surfaces.iter_mut() {
                if let Some(applied) = surface
                    .root_pane
                    .settle_screen_idle(surface_id, surface_visible)
                {
                    found = true;
                    changed = applied;
                    break;
                }
            }
            if found {
                if !changed {
                    return None;
                }
                return Some((ws.id, ws.agent_status_rollup()));
            }
        }
        None
    }

    pub async fn workspace_agent_status(&self, workspace: WorkspaceId) -> Option<AgentStatus> {
        let s = self.inner.lock().await;
        s.workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .and_then(Workspace::agent_status_rollup)
    }

    async fn surface_is_in_active_workspace(&self, surface_id: SurfaceId) -> bool {
        let s = self.inner.lock().await;
        let Some(active_workspace) = s.active_workspace else {
            return false;
        };
        s.workspaces
            .iter()
            .find(|workspace| workspace.id == active_workspace)
            .is_some_and(|workspace| {
                workspace
                    .surfaces
                    .iter()
                    .any(|surface| surface.root_pane.is_active_surface(surface_id))
            })
    }

    pub async fn workspace_agent_attention_status(
        &self,
        workspace: WorkspaceId,
    ) -> Option<AgentStatus> {
        let s = self.inner.lock().await;
        s.workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .and_then(Workspace::agent_attention_rollup)
    }

    pub(super) fn mark_all_agents_seen_locked(ws: &mut Workspace) -> bool {
        let mut changed = false;
        for surface in ws.surfaces.iter_mut() {
            changed |= surface.root_pane.mark_all_agents_seen();
        }
        changed
    }

    /// Collect `(workspace, surface, pid)` for every tab surface that
    /// currently has an agent presence with a known PID. The daemon's
    /// liveness sweep walks this list and clears entries whose process
    /// has died (hard kill / closed terminal where `SessionEnd` never
    /// fired).
    pub async fn live_agent_presences(&self) -> Vec<(WorkspaceId, SurfaceId, u32)> {
        let s = self.inner.lock().await;
        let mut out = Vec::new();
        for ws in s.workspaces.iter().filter(|ws| ws.local_root().is_some()) {
            let mut found = Vec::new();
            for surface in &ws.surfaces {
                surface.root_pane.collect_agent_presences(&mut found);
            }
            for (sid, presence) in found {
                if let Some(pid) = presence.pid {
                    out.push((ws.id, sid, pid));
                }
            }
        }
        out
    }
}

fn located_agent_in_pane(
    pane: &Pane,
    surface_id: SurfaceId,
) -> Option<(PaneId, String, AgentPresence)> {
    match pane {
        Pane::Leaf {
            id,
            content: PaneContent::Tabs { surfaces, .. },
        } => surfaces
            .iter()
            .find(|surface| surface.id == surface_id)
            .and_then(|surface| {
                surface
                    .agent
                    .clone()
                    .map(|presence| (*id, surface.title.clone(), presence))
            }),
        Pane::Leaf { .. } => None,
        Pane::Split { first, second, .. } => located_agent_in_pane(first, surface_id)
            .or_else(|| located_agent_in_pane(second, surface_id)),
    }
}

/// Return the agent slot for a surface while preserving the distinction
/// between an existing surface with no agent (`Some(None)`) and a surface that
/// is not in this pane tree (`None`).
fn agent_presence_slot_in_pane(
    pane: &Pane,
    surface_id: SurfaceId,
) -> Option<Option<AgentPresence>> {
    match pane {
        Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } => surfaces
            .iter()
            .find(|surface| surface.id == surface_id)
            .map(|surface| surface.agent.clone()),
        Pane::Leaf { .. } => None,
        Pane::Split { first, second, .. } => agent_presence_slot_in_pane(first, surface_id)
            .or_else(|| agent_presence_slot_in_pane(second, surface_id)),
    }
}

fn agent_presence_slot_in_state(
    state: &State,
    surface_id: SurfaceId,
) -> Option<Option<AgentPresence>> {
    state
        .workspaces
        .iter()
        .filter(|ws| ws.local_root().is_some())
        .find_map(|workspace| {
            workspace
                .surfaces
                .iter()
                .find_map(|surface| agent_presence_slot_in_pane(&surface.root_pane, surface_id))
        })
}

fn take_current_agent_from_pane(
    pane: &mut Pane,
    surface_id: SurfaceId,
    agent: Option<&str>,
    seq: Option<u64>,
    session_id: Option<&str>,
    expected_pid: Option<u32>,
) -> Option<(PaneId, String, AgentPresence)> {
    match pane {
        Pane::Leaf {
            id,
            content: PaneContent::Tabs { surfaces, .. },
        } => {
            let surface = surfaces
                .iter_mut()
                .find(|surface| surface.id == surface_id)?;
            let presence = surface.agent.as_ref()?;
            let invalid_hook_seq = agent.is_some()
                && match (presence.seq, seq) {
                    (Some(current), Some(incoming)) => incoming <= current,
                    (Some(_), None) => true,
                    _ => false,
                };
            let invalid_hook_session = agent.is_some()
                && match (presence.session_id.as_deref(), session_id) {
                    (Some(current), Some(incoming)) => current != incoming,
                    (Some(_), None) => true,
                    _ => false,
                };
            if agent.is_some_and(|agent| !presence.name.eq_ignore_ascii_case(agent))
                || invalid_hook_seq
                || invalid_hook_session
                || expected_pid.is_some_and(|pid| presence.pid != Some(pid))
            {
                return None;
            }
            let presence = surface.agent.take()?;
            let title = surface.title.clone();
            flowmux_core::normalize_unlocked_terminal_title(surface);
            Some((*id, title, presence))
        }
        Pane::Leaf { .. } => None,
        Pane::Split { first, second, .. } => {
            take_current_agent_from_pane(first, surface_id, agent, seq, session_id, expected_pid)
                .or_else(|| {
                    take_current_agent_from_pane(
                        second,
                        surface_id,
                        agent,
                        seq,
                        session_id,
                        expected_pid,
                    )
                })
        }
    }
}

fn preserve_live_agent_pid(report: &mut AgentStatusReport, existing: &AgentPresence) {
    let (Some(existing_pid), Some(incoming_pid)) = (existing.pid, report.pid) else {
        return;
    };
    if existing_pid == incoming_pid {
        return;
    }
    if existing.name != report.name {
        return;
    }
    if existing.source.as_deref() != Some("flowmux:hook")
        || report.source.as_deref() != Some("flowmux:hook")
    {
        return;
    }
    let sessions_compatible = existing.session_id.is_none()
        || report.session_id.is_none()
        || existing.session_id == report.session_id;
    if !sessions_compatible {
        return;
    }
    if flowmux_procmon::pid_alive(existing_pid) {
        report.pid = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn decision_trace_records_rejections_and_transitions_without_payload_text() {
        // Other parallel tests hit the same tracing callsites without a
        // subscriber. Capture in a child so global interest stays deterministic.
        const CHILD: &str = "FLOWMUX_TRACE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            assert!(std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "state_store::agent_runtime::tests::decision_trace_records_rejections_and_transitions_without_payload_text", "--nocapture"])
                .env(CHILD, "1").status().unwrap().success());
            return;
        }
        #[derive(Clone)]
        struct Writer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Writer {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let output = Writer(Default::default());
        let writer = output.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_env_filter("flowmux_agent=debug")
            .with_ansi(false)
            .without_time()
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber).unwrap();
        let store = StateStore::new_lazy_ephemeral(State::default());
        let workspace = store.create_workspace(None, std::env::temp_dir()).await;
        let ws = store.get_workspace(workspace).await.unwrap();
        let pane = &ws.surfaces[0].root_pane;
        let surface = pane
            .active_surface_id(pane.first_leaf_id().unwrap())
            .unwrap();
        for (seq, event) in [
            (
                10,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("private-turn".into()),
                    status_text: "private-status".into(),
                },
            ),
            (
                9,
                AgentLifecycleEvent::TurnStopped {
                    message: Some("private-message".into()),
                    status_text: "private-status".into(),
                },
            ),
            (
                11,
                AgentLifecycleEvent::TurnStopped {
                    message: Some("private-message".into()),
                    status_text: "private-status".into(),
                },
            ),
        ] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    None,
                    Some(seq),
                    "private-session",
                    event,
                    false,
                )
                .await;
        }
        let log = String::from_utf8(output.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("stale_sequence"), "{log}");
        assert!(log.contains("seq=Some(10)"), "{log}");
        assert!(log.contains("surface_visible=false"), "{log}");
        assert!(log.contains("next=Some(Working)"), "{log}");
        assert!(log.contains("next=Some(Idle)"), "{log}");
        assert!(log.contains("identity_source=\"hook\""), "{log}");
        assert!(log.contains(&surface.to_string()), "{log}");
        assert!(
            !log.contains("private-"),
            "diagnostics leaked payload: {log}"
        );
    }

    #[test]
    fn lifecycle_checkpoint_restores_one_session_without_rewinding_others() {
        let mut runtime = AgentLifecycleRuntime::default();
        let key = (SurfaceId::new(), "codex".into(), "target".into());
        let other = (SurfaceId::new(), "claude".into(), "other".into());
        runtime
            .waits
            .insert(key.clone(), HashMap::from([("tool".into(), true)]));
        runtime
            .permission_event_seq
            .insert(key.clone(), HashMap::from([("scope".into(), 7)]));
        runtime
            .session_wait_event_seq
            .insert(key.clone(), HashMap::from([("quota".into(), 8)]));
        runtime.last_seq.insert(key.clone(), 8);
        runtime.codex_turns.insert(
            (key.0, key.2.clone()),
            CodexTurnLedger {
                current_parent_turn: Some("turn".into()),
                active_children: HashMap::from([("child".into(), "child-turn".into())]),
                ..Default::default()
            },
        );
        let before = LifecycleCheckpoint::capture(&runtime, &key);
        clear_agent_lifecycle_runtime(&mut runtime, key.0, None, None, None);
        runtime
            .permission_waits
            .insert(key.clone(), HashSet::from(["new".into()]));
        runtime
            .session_waits
            .insert(key.clone(), HashSet::from(["new".into()]));
        runtime.boundary_seq.insert(key.clone(), 99);
        runtime
            .waits
            .insert(other.clone(), HashMap::from([("unrelated".into(), false)]));
        remember_ended_agent_lifecycle(&mut runtime, other.clone(), None, Some(12), false);
        before.restore(&mut runtime);
        assert_eq!(runtime.waits[&key], HashMap::from([("tool".into(), true)]));
        assert_eq!(runtime.permission_event_seq[&key]["scope"], 7);
        assert_eq!(runtime.session_wait_event_seq[&key]["quota"], 8);
        assert_eq!(runtime.last_seq[&key], 8);
        assert!(!runtime.boundary_seq.contains_key(&key));
        assert!(!runtime.permission_waits.contains_key(&key));
        assert!(!runtime.session_waits.contains_key(&key));
        let turn = &runtime.codex_turns[&(key.0, key.2.clone())];
        assert_eq!(turn.current_parent_turn.as_deref(), Some("turn"));
        assert_eq!(turn.active_children["child"], "child-turn");
        assert!(!runtime.waits[&other]["unrelated"]);
        assert_eq!(runtime.ended[&other].seq, Some(12));
        assert_eq!(runtime.ended_order.back(), Some(&other));
    }

    #[tokio::test]
    async fn rejected_session_start_preserves_runtime_waits() {
        let store = StateStore::new_lazy_ephemeral(State::default());
        let workspace = store.create_workspace(None, std::env::temp_dir()).await;
        let ws = store.get_workspace(workspace).await.unwrap();
        let pane = &ws.surfaces[0].root_pane;
        let surface = pane
            .active_surface_id(pane.first_leaf_id().unwrap())
            .unwrap();
        let key = (surface, "claude".into(), "session".into());
        store
            .agents
            .lifecycle
            .lock()
            .await
            .waits
            .insert(key.clone(), HashMap::from([("unresolved".into(), true)]));
        // Ready metadata with no activity/status cannot create a presence.
        let result = store
            .report_agent_status_with_visibility(
                surface,
                AgentStatusReport {
                    name: "claude".into(),
                    status: None,
                    activity: None,
                    pid: None,
                    source: Some("flowmux:hook".into()),
                    seq: Some(1),
                    message: None,
                    custom_status: Some("Ready".into()),
                    session_id: Some("session".into()),
                    session_name: None,
                    messaging_socket: None,
                },
                true,
            )
            .await;
        assert!(result.is_none());
        let runtime = store.agents.lifecycle.lock().await;
        assert!(runtime.waits[&key]["unresolved"]);
        assert!(!runtime.last_seq.contains_key(&key));
    }

    #[tokio::test]
    #[ignore = "transition cost probe; run with --ignored --nocapture"]
    async fn lifecycle_transition_cost_with_many_sessions() {
        for unrelated_sessions in [0, 100, 1000] {
            let store = StateStore::new_lazy_ephemeral(State::default());
            let workspace = store.create_workspace(None, std::env::temp_dir()).await;
            let ws = store.get_workspace(workspace).await.unwrap();
            let pane = &ws.surfaces[0].root_pane;
            let surface = pane
                .active_surface_id(pane.first_leaf_id().unwrap())
                .unwrap();
            {
                let mut runtime = store.agents.lifecycle.lock().await;
                for index in 0..unrelated_sessions {
                    runtime.waits.insert(
                        (SurfaceId::new(), "claude".into(), format!("other-{index}")),
                        (0..32).map(|item| (format!("tool-{item}"), true)).collect(),
                    );
                }
            }
            let started = std::time::Instant::now();
            for seq in 1..=200 {
                let result = store
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        "claude",
                        None,
                        Some(seq),
                        "measured",
                        AgentLifecycleEvent::TurnStarted {
                            turn_id: None,
                            status_text: "Working".into(),
                        },
                        true,
                    )
                    .await;
                assert_eq!(result.workspace, Some(workspace));
            }
            println!(
                "LIFECYCLE_COST unrelated_sessions={unrelated_sessions} events=200 elapsed_us={}",
                started.elapsed().as_micros()
            );
            assert_eq!(
                store.agents.lifecycle.lock().await.waits.len(),
                unrelated_sessions
            );
        }
    }
}
