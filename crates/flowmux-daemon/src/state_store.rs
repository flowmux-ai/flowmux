// SPDX-License-Identifier: GPL-3.0-or-later
//! In-memory state with debounced disk persistence.
//!
//! Every mutation goes through this store, which writes to
//! `$XDG_STATE_HOME/flowmux/state.json` after a short debounce so we
//! never block the event loop on fsync. State load is synchronous on
//! boot.

use flowmux_core::{
    agent_bar_color_for_surface, collect_agent_bar_model, detect_agent_idle_name_from_signals,
    detect_agent_interruption, detect_agent_name_from_signals, detect_agent_progress_text,
    detect_agent_status_from_signals, detect_agent_usage_limit_text,
    select_process_agent_candidate, terminal_tab_title_for_cwd, AgentBarModel, AgentPresence,
    AgentStatus, AgentStatusReport, CloseSurfaceOutcome, EditorSessionState, Pane, PaneContent,
    PaneId, PaneSurface, RemoveOutcome, SplitDirection, Surface, SurfaceId, SurfaceKind,
    TerminalScrollback, Workspace, WorkspaceAgentBlock, WorkspaceId,
};
use flowmux_ipc::protocol::AgentLifecycleEvent;
use flowmux_state::{State, WindowLayout, WindowOwner};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Notify};
use tracing::{error, info};

#[derive(Debug, Clone, Copy)]
pub enum CloseOutcome {
    /// One leaf removed; the surface still exists.
    PaneRemoved { workspace: WorkspaceId },
    /// The leaf was the last in its surface; the surface was removed.
    SurfaceRemoved { workspace: WorkspaceId },
    /// That was the last surface; the entire workspace was removed.
    WorkspaceRemoved { workspace: WorkspaceId },
}

/// Result of relocating a tab via [`StateStore::move_surface_to_pane`] /
/// [`StateStore::move_surface_to_workspace`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveSurfaceOutcome {
    pub surface: SurfaceId,
    pub src_pane: PaneId,
    pub dst_pane: PaneId,
    /// Workspace the tab now lives in.
    pub dst_workspace: WorkspaceId,
    /// Workspace the tab came from.
    pub src_workspace: WorkspaceId,
    /// The source leaf emptied and was collapsed, so its pane no longer exists.
    pub src_pane_removed: bool,
    /// Collapsing the source removed its workspace entirely.
    pub src_workspace_removed: bool,
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
type AgentPermissionSeqKey = (SurfaceId, String, String, String);
type AgentSessionWaitSeqKey = (SurfaceId, String, String, String);
const SESSION_PERMISSION_SCOPE: &str = "session";
const SESSION_WAIT_SCOPE: &str = "session";

#[derive(Debug, Clone, Default)]
struct AgentLifecycleRuntime {
    /// Signed balances make start/resolve deltas commutative when parallel
    /// hook processes reach the daemon out of order.
    waits: HashMap<AgentLifecycleKey, HashMap<String, i64>>,
    permission_waits: HashMap<AgentLifecycleKey, HashSet<String>>,
    permission_event_seq: HashMap<AgentPermissionSeqKey, u64>,
    session_waits: HashMap<AgentLifecycleKey, HashSet<String>>,
    session_wait_event_seq: HashMap<AgentSessionWaitSeqKey, u64>,
    /// Highest non-child event observed, used to reject delayed terminal
    /// boundaries and native SessionStart. Codex child ordering is tracked by
    /// the child-specific sequence maps below so its start/Stop ingress race
    /// cannot invalidate a legitimate parent Stop.
    last_seq: HashMap<AgentLifecycleKey, u64>,
    /// Latest root/session boundary. Deltas older than this belong to a prior
    /// turn even when a different parallel item arrived later.
    boundary_seq: HashMap<AgentLifecycleKey, u64>,
    ended: HashMap<AgentLifecycleKey, EndedAgentLifecycle>,
    ended_order: VecDeque<AgentLifecycleKey>,
    codex_turns: HashMap<(SurfaceId, String), CodexTurnLedger>,
}

#[derive(Debug, Clone)]
struct EndedAgentLifecycle {
    pid: Option<u32>,
    seq: Option<u64>,
    /// A different native agent temporarily took ownership of the same pane.
    /// The displaced hook may resume only after process polling has restored
    /// its identity and the original live PID reports a newer event.
    reactivate_on_process_return: bool,
}

#[derive(Debug, Clone, Default)]
struct CodexTurnLedger {
    owner_pid: Option<u32>,
    current_parent_turn: Option<String>,
    active_children: HashMap<String, String>,
    child_event_seq: HashMap<(String, String), u64>,
    child_agent_event_seq: HashMap<String, u64>,
    pending_parent_stop: Option<PendingCodexStop>,
    settled_parent_turns: VecDeque<String>,
}

#[derive(Debug, Clone)]
struct PendingCodexStop {
    turn_id: String,
    message: Option<String>,
    status_text: String,
    notify_completion: bool,
    seq: Option<u64>,
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

impl MoveSurfaceOutcome {
    pub fn changed_workspaces(&self) -> Vec<WorkspaceId> {
        if self.src_workspace == self.dst_workspace {
            vec![self.src_workspace]
        } else {
            vec![self.src_workspace, self.dst_workspace]
        }
    }

    pub fn changed_panes(&self) -> Vec<PaneId> {
        if self.src_pane == self.dst_pane {
            vec![self.src_pane]
        } else {
            vec![self.src_pane, self.dst_pane]
        }
    }

    pub fn changed_surfaces(&self) -> [SurfaceId; 1] {
        [self.surface]
    }
}

/// Result of [`StateStore::split_surface_into_pane`]: like
/// [`MoveSurfaceOutcome`] but also reports the freshly created sibling pane the
/// tab was placed into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitMoveOutcome {
    pub surface: SurfaceId,
    pub src_pane: PaneId,
    pub dst_pane: PaneId,
    /// The new sibling pane that now holds the moved tab.
    pub new_pane: PaneId,
    pub dst_workspace: WorkspaceId,
    pub src_workspace: WorkspaceId,
    pub src_pane_removed: bool,
    pub src_workspace_removed: bool,
}

impl SplitMoveOutcome {
    pub fn changed_workspaces(&self) -> Vec<WorkspaceId> {
        if self.src_workspace == self.dst_workspace {
            vec![self.src_workspace]
        } else {
            vec![self.src_workspace, self.dst_workspace]
        }
    }

    pub fn changed_panes(&self) -> Vec<PaneId> {
        let mut panes = vec![self.src_pane];
        if self.dst_pane != self.src_pane {
            panes.push(self.dst_pane);
        }
        if self.new_pane != self.src_pane && self.new_pane != self.dst_pane {
            panes.push(self.new_pane);
        }
        panes
    }

    pub fn changed_surfaces(&self) -> [SurfaceId; 1] {
        [self.surface]
    }
}

/// Remove the leaf pane `target` from an already-locked [`State`], collapsing
/// the enclosing split / surface / workspace as needed. Mirrors the body of
/// [`StateStore::close_pane`] but operates on a held lock so it can be reused
/// by the tab-move path. Returns `None` if `target` is not found.
fn remove_pane_leaf_locked(s: &mut State, target: PaneId) -> Option<CloseOutcome> {
    for ws_idx in 0..s.workspaces.len() {
        let mut surface_to_drop = None;
        for surf_idx in 0..s.workspaces[ws_idx].surfaces.len() {
            let surface = &mut s.workspaces[ws_idx].surfaces[surf_idx];
            let root = std::mem::replace(
                &mut surface.root_pane,
                Pane::Leaf {
                    id: PaneId::new(),
                    content: PaneContent::tabbed_terminal("Terminal", None),
                },
            );
            match root.remove_leaf(target) {
                RemoveOutcome::EntirelyRemoved => {
                    surface_to_drop = Some(surf_idx);
                    break;
                }
                RemoveOutcome::Replaced(new_root) => {
                    surface.root_pane = new_root;
                    return Some(CloseOutcome::PaneRemoved {
                        workspace: s.workspaces[ws_idx].id,
                    });
                }
                RemoveOutcome::NotFound(unchanged) => {
                    surface.root_pane = unchanged;
                }
            }
        }
        if let Some(idx) = surface_to_drop {
            s.workspaces[ws_idx].surfaces.remove(idx);
            let ws_id = s.workspaces[ws_idx].id;
            if s.workspaces[ws_idx].surfaces.is_empty() {
                s.workspaces.remove(ws_idx);
                s.workspace_order.retain(|id| *id != ws_id);
                if s.active_workspace == Some(ws_id) {
                    s.active_workspace = s.workspace_order.first().copied();
                }
                return Some(CloseOutcome::WorkspaceRemoved { workspace: ws_id });
            }
            return Some(CloseOutcome::SurfaceRemoved { workspace: ws_id });
        }
    }
    None
}

struct TakenPaneSurface {
    surface: PaneSurface,
    workspace: WorkspaceId,
    leaf_empty: bool,
}

/// Remove one tab from a pane while the state lock is already held. Keeping
/// this search in one helper makes move and split-move use the same active-tab
/// fix-up and source-workspace bookkeeping.
fn take_surface_locked(
    s: &mut State,
    pane: PaneId,
    surface: SurfaceId,
) -> Option<TakenPaneSurface> {
    for workspace in &mut s.workspaces {
        for workspace_surface in &mut workspace.surfaces {
            if let Some((surface, leaf_empty)) = workspace_surface
                .root_pane
                .take_surface_from_leaf(pane, surface)
            {
                return Some(TakenPaneSurface {
                    surface,
                    workspace: workspace.id,
                    leaf_empty,
                });
            }
        }
    }
    None
}

/// Collapse a source pane after its last tab moved away. The booleans match
/// the public move outcomes: pane removed, then workspace removed.
fn collapse_empty_source_locked(s: &mut State, pane: PaneId, leaf_empty: bool) -> (bool, bool) {
    if !leaf_empty {
        return (false, false);
    }
    match remove_pane_leaf_locked(s, pane) {
        Some(CloseOutcome::WorkspaceRemoved { .. }) => (true, true),
        Some(_) => (true, false),
        None => (false, false),
    }
}

#[derive(Debug, Clone, Copy)]
enum PersistenceMode {
    Full,
    Window(WindowOwner),
    Disabled,
}

async fn save_snapshot_blocking(
    snapshot: State,
    mode: PersistenceMode,
) -> Result<u64, flowmux_state::StateError> {
    tokio::task::spawn_blocking(move || {
        match mode {
            PersistenceMode::Full => flowmux_state::save_owned(snapshot)?,
            PersistenceMode::Window(owner) => flowmux_state::save_window_owned(owner, snapshot)?,
            PersistenceMode::Disabled => return Ok(0),
        }
        let file_size = flowmux_state::default_path()
            .ok()
            .and_then(|path| std::fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        Ok(file_size)
    })
    .await
    .map_err(|err| {
        flowmux_state::StateError::Io(std::io::Error::other(format!(
            "state persistence worker failed: {err}"
        )))
    })?
}

/// Return every live workspace id exactly once in sidebar order. Persisted
/// states from older releases may have no `workspace_order`, while a damaged
/// state can contain stale or duplicate ids; append any missing live ids in
/// their stable `workspaces` order.
fn workspace_ids_in_display_order(s: &State) -> Vec<WorkspaceId> {
    let live: HashSet<WorkspaceId> = s.workspaces.iter().map(|ws| ws.id).collect();
    let mut seen = HashSet::with_capacity(live.len());
    let mut ordered = Vec::with_capacity(live.len());

    for id in &s.workspace_order {
        if live.contains(id) && seen.insert(*id) {
            ordered.push(*id);
        }
    }
    for ws in &s.workspaces {
        if seen.insert(ws.id) {
            ordered.push(ws.id);
        }
    }
    ordered
}

fn collect_pane_surface_ids(pane: &Pane, target: Option<PaneId>, out: &mut Vec<SurfaceId>) {
    match pane {
        Pane::Leaf {
            id,
            content: PaneContent::Tabs { surfaces, .. },
        } if target.is_none_or(|target| target == *id) => {
            out.extend(surfaces.iter().map(|surface| surface.id));
        }
        Pane::Leaf { .. } => {}
        Pane::Split { first, second, .. } => {
            collect_pane_surface_ids(first, target, out);
            collect_pane_surface_ids(second, target, out);
        }
    }
}

fn workspace_pane_surface_ids(workspace: &Workspace) -> Vec<SurfaceId> {
    let mut ids = Vec::new();
    for surface in &workspace.surfaces {
        collect_pane_surface_ids(&surface.root_pane, None, &mut ids);
    }
    ids
}

fn state_pane_surface_ids(state: &State, pane: PaneId) -> Vec<SurfaceId> {
    let mut ids = Vec::new();
    for workspace in &state.workspaces {
        for surface in &workspace.surfaces {
            collect_pane_surface_ids(&surface.root_pane, Some(pane), &mut ids);
        }
    }
    ids
}

#[derive(Clone)]
pub struct StateStore {
    inner: Arc<Mutex<State>>,
    cleared_agent_surfaces: Arc<Mutex<HashSet<SurfaceId>>>,
    last_agent_screen_fingerprints: Arc<Mutex<HashMap<SurfaceId, u64>>>,
    cleared_agent_screen_fingerprints: Arc<Mutex<HashMap<SurfaceId, Option<u64>>>>,
    cleared_agent_saw_no_signal: Arc<Mutex<HashSet<SurfaceId>>>,
    agent_lifecycle: Arc<Mutex<AgentLifecycleRuntime>>,
    dirty: Arc<Notify>,
    dirty_generation: Arc<AtomicU64>,
    persistence: PersistenceMode,
}

const PERSIST_DEBOUNCE: Duration = Duration::from_millis(250);

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
        .is_some_and(|waits| waits.values().any(|balance| *balance > 0))
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
    let seq_key = (key.0, key.1.clone(), key.2.clone(), scope.to_string());
    let newer = match (runtime.permission_event_seq.get(&seq_key).copied(), seq) {
        (Some(current), Some(incoming)) => incoming > current,
        (Some(_), None) => false,
        _ => true,
    };
    if !newer {
        return false;
    }
    if let Some(seq) = seq {
        runtime.permission_event_seq.insert(seq_key, seq);
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
    runtime
        .permission_event_seq
        .retain(|(surface, agent, session, _), _| {
            *surface != key.0 || agent != &key.1 || session != &key.2
        });
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
    runtime
        .session_wait_event_seq
        .retain(|(surface, agent, session, _), _| {
            *surface != key.0 || agent != &key.1 || session != &key.2
        });
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
        .retain(|(key_surface, key_agent, key_session, _), _| {
            keep_wait_key(&(*key_surface, key_agent.clone(), key_session.clone()))
        });
    lifecycle.session_waits.retain(|key, _| keep_wait_key(key));
    lifecycle
        .session_wait_event_seq
        .retain(|(key_surface, key_agent, key_session, _), _| {
            keep_wait_key(&(*key_surface, key_agent.clone(), key_session.clone()))
        });
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
    async fn forget_cleared_agent_surfaces(&self, surfaces: &[SurfaceId]) {
        if surfaces.is_empty() {
            return;
        }
        let mut cleared = self.cleared_agent_surfaces.lock().await;
        for surface in surfaces {
            cleared.remove(surface);
        }
        drop(cleared);
        let mut last = self.last_agent_screen_fingerprints.lock().await;
        for surface in surfaces {
            last.remove(surface);
        }
        drop(last);
        let mut baselines = self.cleared_agent_screen_fingerprints.lock().await;
        for surface in surfaces {
            baselines.remove(surface);
        }
        drop(baselines);
        let mut saw_no_signal = self.cleared_agent_saw_no_signal.lock().await;
        for surface in surfaces {
            saw_no_signal.remove(surface);
        }
        drop(saw_no_signal);
        let mut lifecycle = self.agent_lifecycle.lock().await;
        lifecycle
            .waits
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .permission_waits
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .permission_event_seq
            .retain(|(surface, _, _, _), _| !surfaces.contains(surface));
        lifecycle
            .session_waits
            .retain(|(surface, _, _), _| !surfaces.contains(surface));
        lifecycle
            .session_wait_event_seq
            .retain(|(surface, _, _, _), _| !surfaces.contains(surface));
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
        let mut lifecycle = self.agent_lifecycle.lock().await;
        clear_agent_lifecycle_runtime(&mut lifecycle, surface, agent, session_id, expected_pid);
    }

    async fn allow_agent_screen_restore(&self, surface: SurfaceId) {
        self.cleared_agent_surfaces.lock().await.remove(&surface);
        self.cleared_agent_screen_fingerprints
            .lock()
            .await
            .remove(&surface);
        self.cleared_agent_saw_no_signal
            .lock()
            .await
            .remove(&surface);
    }

    async fn suppress_agent_screen_restore(&self, surface: SurfaceId) {
        let baseline = self
            .last_agent_screen_fingerprints
            .lock()
            .await
            .get(&surface)
            .copied();
        self.cleared_agent_surfaces.lock().await.insert(surface);
        self.cleared_agent_screen_fingerprints
            .lock()
            .await
            .insert(surface, baseline);
        self.cleared_agent_saw_no_signal
            .lock()
            .await
            .remove(&surface);
    }

    /// Construct from inside a tokio runtime context. Spawns the
    /// persistence loop on the current runtime.
    pub fn new(initial: State) -> Self {
        let mut initial = initial;
        let normalized = normalize_state(&mut initial);
        let store = Self {
            inner: Arc::new(Mutex::new(initial)),
            cleared_agent_surfaces: Arc::new(Mutex::new(HashSet::new())),
            last_agent_screen_fingerprints: Arc::new(Mutex::new(HashMap::new())),
            cleared_agent_screen_fingerprints: Arc::new(Mutex::new(HashMap::new())),
            cleared_agent_saw_no_signal: Arc::new(Mutex::new(HashSet::new())),
            agent_lifecycle: Arc::new(Mutex::new(AgentLifecycleRuntime::default())),
            dirty: Arc::new(Notify::new()),
            dirty_generation: Arc::new(AtomicU64::new(0)),
            persistence: PersistenceMode::Full,
        };
        let bg = store.clone();
        tokio::spawn(async move { bg.persist_loop().await });
        if normalized {
            store.mark_dirty();
        }
        store
    }

    /// Construct without entering a tokio context. Caller must spawn
    /// [`StateStore::persist_loop`] on the runtime themselves. Useful
    /// from the GTK main thread before the runtime is fully wired.
    pub fn new_lazy(initial: State) -> Self {
        let mut initial = initial;
        let normalized = normalize_state(&mut initial);
        let store = Self {
            inner: Arc::new(Mutex::new(initial)),
            cleared_agent_surfaces: Arc::new(Mutex::new(HashSet::new())),
            last_agent_screen_fingerprints: Arc::new(Mutex::new(HashMap::new())),
            cleared_agent_screen_fingerprints: Arc::new(Mutex::new(HashMap::new())),
            cleared_agent_saw_no_signal: Arc::new(Mutex::new(HashSet::new())),
            agent_lifecycle: Arc::new(Mutex::new(AgentLifecycleRuntime::default())),
            dirty: Arc::new(Notify::new()),
            dirty_generation: Arc::new(AtomicU64::new(0)),
            persistence: PersistenceMode::Full,
        };
        if normalized {
            store.mark_dirty();
        }
        store
    }

    /// Same as [`new_lazy`], but the resulting store will never write
    /// to disk. Used by additional flowmux GUI windows that do not own
    /// the per-host `state.json` lock; their workspaces live and die
    /// with the window so they cannot stomp on the lock owner's file.
    pub fn new_lazy_ephemeral(initial: State) -> Self {
        let mut initial = initial;
        // Still normalize so any in-memory invariants the daemon
        // depends on hold, but do not flip the dirty bit — there is
        // nobody to flush to.
        let _ = normalize_state(&mut initial);
        Self {
            inner: Arc::new(Mutex::new(initial)),
            cleared_agent_surfaces: Arc::new(Mutex::new(HashSet::new())),
            last_agent_screen_fingerprints: Arc::new(Mutex::new(HashMap::new())),
            cleared_agent_screen_fingerprints: Arc::new(Mutex::new(HashMap::new())),
            cleared_agent_saw_no_signal: Arc::new(Mutex::new(HashSet::new())),
            agent_lifecycle: Arc::new(Mutex::new(AgentLifecycleRuntime::default())),
            dirty: Arc::new(Notify::new()),
            dirty_generation: Arc::new(AtomicU64::new(0)),
            persistence: PersistenceMode::Disabled,
        }
    }

    /// Construct a GUI-window store whose writes are merged into the shared
    /// state file under this window's ownership record.
    pub fn new_lazy_window(initial: State, owner: WindowOwner) -> Self {
        let mut initial = initial;
        let normalized = normalize_state(&mut initial);
        let store = Self {
            inner: Arc::new(Mutex::new(initial)),
            cleared_agent_surfaces: Arc::new(Mutex::new(HashSet::new())),
            last_agent_screen_fingerprints: Arc::new(Mutex::new(HashMap::new())),
            cleared_agent_screen_fingerprints: Arc::new(Mutex::new(HashMap::new())),
            cleared_agent_saw_no_signal: Arc::new(Mutex::new(HashSet::new())),
            agent_lifecycle: Arc::new(Mutex::new(AgentLifecycleRuntime::default())),
            dirty: Arc::new(Notify::new()),
            dirty_generation: Arc::new(AtomicU64::new(0)),
            persistence: PersistenceMode::Window(owner),
        };
        if normalized {
            store.mark_dirty();
        }
        store
    }

    /// True when this store is allowed to write to `state.json`.
    pub fn persist_enabled(&self) -> bool {
        !matches!(self.persistence, PersistenceMode::Disabled)
    }

    /// Spawn the persist loop on `handle`. Pair with [`new_lazy`].
    pub fn spawn_persist(&self, handle: &tokio::runtime::Handle) {
        let bg = self.clone();
        handle.spawn(async move { bg.persist_loop().await });
    }

    pub async fn snapshot(&self) -> State {
        self.inner.lock().await.clone()
    }

    /// Build the complete Agents UI model while holding the state lock, cloning
    /// only the small fields rendered by the bar/panel. Persisted terminal
    /// scrollback never leaves the store through this path.
    pub async fn agent_bar_model(&self) -> AgentBarModel {
        let s = self.inner.lock().await;
        let ordered_ids = workspace_ids_in_display_order(&s);
        let workspaces = ordered_ids
            .iter()
            .filter_map(|id| s.workspaces.iter().find(|workspace| workspace.id == *id));
        collect_agent_bar_model(workspaces)
    }

    pub async fn list_workspaces(&self) -> Vec<WorkspaceId> {
        let s = self.inner.lock().await;
        workspace_ids_in_display_order(&s)
    }

    /// Return the active workspace id without cloning the persisted state.
    pub async fn active_workspace(&self) -> Option<WorkspaceId> {
        self.inner.lock().await.active_workspace
    }

    /// Small workspace-navigation snapshot used by next/previous/number
    /// shortcuts. Terminal history and workspace metadata stay in the store.
    pub async fn workspace_order_and_active(&self) -> (Vec<WorkspaceId>, Option<WorkspaceId>) {
        let s = self.inner.lock().await;
        (workspace_ids_in_display_order(&s), s.active_workspace)
    }

    /// Clone workspace models in the same order shown in the sidebar, omitting
    /// terminal replay buffers that are not part of IPC tree responses. This
    /// avoids copying up to 256 KiB per terminal for every layout query.
    pub async fn ordered_workspaces(&self) -> Vec<Workspace> {
        let s = self.inner.lock().await;
        let by_id = s
            .workspaces
            .iter()
            .map(|workspace| (workspace.id, workspace))
            .collect::<HashMap<_, _>>();
        workspace_ids_in_display_order(&s)
            .into_iter()
            .filter_map(|id| {
                by_id
                    .get(&id)
                    .map(|workspace| workspace.clone_without_scrollback())
            })
            .collect()
    }

    pub async fn create_workspace(
        &self,
        name: Option<String>,
        root: std::path::PathBuf,
    ) -> WorkspaceId {
        let id = WorkspaceId::new();
        let surface_id = SurfaceId::new();
        let pane_id = PaneId::new();
        let tab_title = terminal_tab_title_for_cwd(Some(&root));
        // Even if caller supplies a name, treat it as the automatic value (`name`).
        // cmux semantics: customTitle is filled only after an explicit user
        // rename, and new workspaces always start with None in automatic mode.
        let auto_name = name.unwrap_or_else(|| {
            root.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace")
                .to_string()
        });
        let mut s = self.inner.lock().await;
        // Pick a palette color that other workspaces are not already wearing,
        // so sidebar color bars stay distinct; the UUID seeds the random choice
        // among the least-used colors.
        let used: Vec<String> = s
            .workspaces
            .iter()
            .filter_map(|w| w.color.clone())
            .collect();
        let color = flowmux_core::pick_workspace_color(&used, id.0.as_u128());
        let ws = Workspace {
            id,
            name: auto_name,
            custom_title: None,
            root_dir: root.clone(),
            git: None,
            listening_ports: vec![],
            surfaces: vec![Surface {
                id: surface_id,
                kind: SurfaceKind::Terminal {
                    shell: None,
                    cwd: Some(root.clone()),
                },
                title: "main".into(),
                root_pane: Pane::Leaf {
                    id: pane_id,
                    content: PaneContent::tabbed_terminal(tab_title, Some(root)),
                },
            }],
            color: Some(color),
        };
        s.workspaces.push(ws);
        s.workspace_order.push(id);
        if s.active_workspace.is_none() {
            s.active_workspace = Some(id);
        }
        drop(s);
        self.mark_dirty();
        id
    }

    pub async fn replace_git_info(
        &self,
        workspace: WorkspaceId,
        info: Option<flowmux_core::GitInfo>,
    ) {
        let mut s = self.inner.lock().await;
        if let Some(w) = s.workspaces.iter_mut().find(|w| w.id == workspace) {
            w.git = info;
        }
        drop(s);
        self.mark_dirty();
    }

    pub async fn replace_listening_ports(&self, workspace: WorkspaceId, ports: Vec<u16>) {
        let mut s = self.inner.lock().await;
        if let Some(w) = s.workspaces.iter_mut().find(|w| w.id == workspace) {
            w.listening_ports = ports;
        }
        drop(s);
        self.mark_dirty();
    }

    /// Split a target leaf and replace the new sibling with a
    /// browser pane carrying `url`. Used by `flowmux browser open` to
    /// drop a webview next to a terminal without touching the
    /// terminal's content. The new pane uses the tabbed-browser
    /// content shape so it slots into the pane-local surface-tab
    /// bar like any other browser pane.
    pub async fn split_pane_with_browser(
        &self,
        target: PaneId,
        direction: SplitDirection,
        url: String,
    ) -> Option<(WorkspaceId, PaneId)> {
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                if let Some(new_id) = surface.root_pane.split_leaf(
                    target,
                    direction,
                    0.5,
                    PaneContent::tabbed_browser("Browser", url.clone()),
                ) {
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some((ws_id, new_id));
                }
            }
        }
        None
    }

    /// Find the pane in any workspace and split it. Returns the new
    /// pane's id and the workspace it lives in so the GUI can rebuild
    /// the affected widget tree.
    pub async fn split_pane(
        &self,
        target: PaneId,
        direction: SplitDirection,
    ) -> Option<(WorkspaceId, PaneId)> {
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                let cwd = surface
                    .root_pane
                    .terminal_surface_cwd(target)
                    .or_else(|| Some(ws.root_dir.clone()));
                let title = terminal_tab_title_for_cwd(cwd.as_deref());
                if let Some(new_id) = surface.root_pane.split_leaf(
                    target,
                    direction,
                    0.5,
                    PaneContent::tabbed_terminal(title, cwd),
                ) {
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some((ws_id, new_id));
                }
            }
        }
        None
    }

    /// Remove the leaf pane and collapse its split. Returns the
    /// workspace it lived in. If the workspace's last surface becomes
    /// empty as a result, the surface is dropped; if the workspace's
    /// last surface is dropped, the workspace itself is removed too.
    /// Returns `None` if the pane wasn't found.
    pub async fn close_pane(&self, target: PaneId) -> Option<CloseOutcome> {
        let mut s = self.inner.lock().await;
        let closed_surfaces = state_pane_surface_ids(&s, target);
        let outcome = remove_pane_leaf_locked(&mut s, target);
        drop(s);
        if outcome.is_some() {
            self.forget_cleared_agent_surfaces(&closed_surfaces).await;
            self.mark_dirty();
        }
        outcome
    }

    /// Relocate the tab `surface_id` out of `src_pane` and into a brand-new
    /// sibling pane created by splitting `dst_pane` in `direction`. The moved
    /// tab becomes the new pane's only tab. If the source leaf empties it is
    /// collapsed. Returns `None` (state unchanged) if the surface or
    /// destination pane cannot be found, or if the pane's only tab is being
    /// split back into that same pane.
    pub async fn split_surface_into_pane(
        &self,
        src_pane: PaneId,
        surface_id: SurfaceId,
        dst_pane: PaneId,
        direction: SplitDirection,
    ) -> Option<SplitMoveOutcome> {
        let mut s = self.inner.lock().await;

        // Splitting the only tab back into its own pane would create a split and
        // immediately collapse the now-empty original leaf, so the returned pane
        // would not actually have the new sibling relationship callers expect.
        if src_pane == dst_pane
            && s.workspaces.iter().any(|ws| {
                ws.surfaces.iter().any(|sf| {
                    matches!(
                        sf.root_pane.find_leaf_content(src_pane),
                        Some(PaneContent::Tabs { surfaces, .. })
                            if surfaces.len() == 1 && surfaces[0].id == surface_id
                    )
                })
            })
        {
            return None;
        }

        // Capture the exact destination while holding the store lock. Taking a
        // tab mutates only a pane tree, so these vector indexes remain stable
        // until the destination split is complete.
        let (dst_ws_idx, dst_surface_idx) =
            s.workspaces.iter().enumerate().find_map(|(ws_idx, ws)| {
                ws.surfaces
                    .iter()
                    .position(|sf| sf.root_pane.find_leaf_content(dst_pane).is_some())
                    .map(|surface_idx| (ws_idx, surface_idx))
            })?;

        let taken = take_surface_locked(&mut s, src_pane, surface_id)?;
        let src_workspace = taken.workspace;

        // Split the prevalidated destination, placing the moved tab in the new
        // sibling. There is no second tree search or pending payload state that
        // can diverge after the source mutation.
        let dst_workspace = s.workspaces[dst_ws_idx].id;
        let content = PaneContent::Tabs {
            active: taken.surface.id,
            surfaces: vec![taken.surface],
        };
        let Some(new_pane) = s.workspaces[dst_ws_idx].surfaces[dst_surface_idx]
            .root_pane
            .split_leaf(dst_pane, direction, 0.5, content)
        else {
            error!(pane = %dst_pane, "split_surface_into_pane: verified destination rejected split");
            return None;
        };

        let (src_pane_removed, src_workspace_removed) =
            collapse_empty_source_locked(&mut s, src_pane, taken.leaf_empty);

        drop(s);
        self.mark_dirty();
        Some(SplitMoveOutcome {
            surface: surface_id,
            src_pane,
            dst_pane,
            new_pane,
            dst_workspace,
            src_workspace,
            src_pane_removed,
            src_workspace_removed,
        })
    }

    /// Insert a surface imported from another window/process into `dst_pane`.
    /// The surface gets a fresh id in this store; terminal/browser live widget
    /// state is rebuilt by the GUI from the model.
    pub async fn import_surface_to_pane(
        &self,
        dst_pane: PaneId,
        mut surface: PaneSurface,
        target_index: usize,
    ) -> Option<(WorkspaceId, SurfaceId)> {
        surface.id = SurfaceId::new();
        surface.agent = None;
        let surface_id = surface.id;

        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for sf in ws.surfaces.iter_mut() {
                if sf.root_pane.find_leaf_content(dst_pane).is_some() {
                    sf.root_pane
                        .insert_surface_into_leaf(dst_pane, surface, target_index)?;
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some((ws_id, surface_id));
                }
            }
        }

        None
    }

    /// Split `dst_pane` and place a surface imported from another
    /// window/process into the new sibling pane.
    pub async fn split_imported_surface_into_pane(
        &self,
        dst_pane: PaneId,
        mut surface: PaneSurface,
        direction: SplitDirection,
    ) -> Option<(WorkspaceId, PaneId, SurfaceId)> {
        surface.id = SurfaceId::new();
        surface.agent = None;
        let surface_id = surface.id;
        let content = PaneContent::Tabs {
            active: surface_id,
            surfaces: vec![surface],
        };

        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for sf in ws.surfaces.iter_mut() {
                if sf.root_pane.find_leaf_content(dst_pane).is_some() {
                    let new_pane = sf.root_pane.split_leaf(dst_pane, direction, 0.5, content)?;
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some((ws_id, new_pane, surface_id));
                }
            }
        }

        None
    }

    /// Return the workspace that owns leaf pane `target`, if any.
    pub async fn workspace_of_pane(&self, target: PaneId) -> Option<WorkspaceId> {
        let s = self.inner.lock().await;
        s.workspaces
            .iter()
            .find(|ws| {
                ws.surfaces
                    .iter()
                    .any(|sf| sf.root_pane.find_leaf_content(target).is_some())
            })
            .map(|ws| ws.id)
    }

    /// Relocate the tab `surface_id` out of leaf `src_pane` and into leaf
    /// `dst_pane` at `target_index` (clamped to the end). Works whether the
    /// destination is in the same workspace or a different one. If the source
    /// leaf empties it is collapsed like a pane close. Returns `None` if the
    /// surface or a tab-capable destination pane cannot be found, or a
    /// same-pane move is already at `target_index` (the state is left unchanged
    /// in those cases).
    pub async fn move_surface_to_pane(
        &self,
        src_pane: PaneId,
        surface_id: SurfaceId,
        dst_pane: PaneId,
        target_index: usize,
    ) -> Option<MoveSurfaceOutcome> {
        let mut s = self.inner.lock().await;

        // Treat a same-pane move as an in-place reorder. Without this guard,
        // moving the pane's only tab removes it, reinserts it, then collapses
        // the pane based on the stale `src_leaf_empty` value from before the
        // insertion, deleting the pane (and potentially its workspace).
        if src_pane == dst_pane {
            for ws in s.workspaces.iter_mut() {
                for sf in ws.surfaces.iter_mut() {
                    if sf
                        .root_pane
                        .find_surface_ref(src_pane, surface_id)
                        .is_some()
                    {
                        let changed = sf.root_pane.reorder_surface_in_leaf(
                            src_pane,
                            surface_id,
                            target_index,
                        );
                        if !changed {
                            return None;
                        }
                        let workspace = ws.id;
                        drop(s);
                        self.mark_dirty();
                        return Some(MoveSurfaceOutcome {
                            surface: surface_id,
                            src_pane,
                            dst_pane,
                            dst_workspace: workspace,
                            src_workspace: workspace,
                            src_pane_removed: false,
                            src_workspace_removed: false,
                        });
                    }
                }
            }
            return None;
        }

        // Destination must exist before we disturb the source, so a missing
        // target is a clean no-op rather than a lost tab.
        let (dst_ws_idx, dst_surface_idx) =
            s.workspaces.iter().enumerate().find_map(|(ws_idx, ws)| {
                ws.surfaces
                    .iter()
                    .position(|sf| {
                        matches!(
                            sf.root_pane.find_leaf_content(dst_pane),
                            Some(PaneContent::Tabs { .. })
                        )
                    })
                    .map(|surface_idx| (ws_idx, surface_idx))
            })?;

        let taken = take_surface_locked(&mut s, src_pane, surface_id)?;
        let src_workspace = taken.workspace;

        // Insert directly into the prevalidated destination. Capturing its
        // location before the take removes the old pending/destination expect
        // paths entirely.
        let dst_workspace = s.workspaces[dst_ws_idx].id;
        if s.workspaces[dst_ws_idx].surfaces[dst_surface_idx]
            .root_pane
            .insert_surface_into_leaf(dst_pane, taken.surface, target_index)
            .is_none()
        {
            error!(pane = %dst_pane, "move_surface_to_pane: verified destination rejected insert");
            return None;
        }

        let (src_pane_removed, src_workspace_removed) =
            collapse_empty_source_locked(&mut s, src_pane, taken.leaf_empty);

        drop(s);
        self.mark_dirty();
        Some(MoveSurfaceOutcome {
            surface: surface_id,
            src_pane,
            dst_pane,
            dst_workspace,
            src_workspace,
            src_pane_removed,
            src_workspace_removed,
        })
    }

    /// Convenience wrapper: move a tab to the **last** position of the first
    /// pane of `dst_workspace`. Used by the right-click "Move" menu and by a
    /// drop directly onto a workspace in the side panel.
    pub async fn move_surface_to_workspace(
        &self,
        src_pane: PaneId,
        surface_id: SurfaceId,
        dst_workspace: WorkspaceId,
    ) -> Option<MoveSurfaceOutcome> {
        let dst_pane = {
            let s = self.inner.lock().await;
            let ws = s.workspaces.iter().find(|w| w.id == dst_workspace)?;
            ws.surfaces.first()?.root_pane.first_leaf_id()?
        };
        self.move_surface_to_pane(src_pane, surface_id, dst_pane, usize::MAX)
            .await
    }

    /// Mark `id` as the focused workspace so the next launch starts
    /// there. No-op if the id isn't in the workspace list.
    pub async fn set_active_workspace(&self, id: Option<WorkspaceId>) {
        let mut s = self.inner.lock().await;
        let valid = id
            .map(|i| s.workspaces.iter().any(|w| w.id == i))
            .unwrap_or(true);
        let changed = valid && s.active_workspace != id;
        if changed {
            s.active_workspace = id;
        }
        if let Some(id) = id {
            if let Some(ws) = s.workspaces.iter_mut().find(|w| w.id == id) {
                Self::mark_all_agents_seen_locked(ws);
            }
        }
        drop(s);
        if changed {
            self.mark_dirty();
        }
    }

    /// Set a workspace's sidebar color. Returns true on success.
    pub async fn set_workspace_color(&self, id: WorkspaceId, color: String) -> bool {
        let mut s = self.inner.lock().await;
        let mut updated = false;
        if let Some(w) = s.workspaces.iter_mut().find(|w| w.id == id) {
            w.color = Some(color);
            updated = true;
        }
        drop(s);
        if updated {
            self.mark_dirty();
        }
        updated
    }

    /// Apply the value the user entered in the right-click "Change tab name"
    /// dialog to a workspace. Behavior matches cmux `setCustomTitle`:
    ///   * If trimming both ends yields an empty value, reset to
    ///     `custom_title = None` and return to automatic mode, showing `name`.
    ///   * Otherwise store `custom_title = Some(trimmed)`.
    ///
    /// The automatic value `name` is never modified here, so separate automatic
    /// signals such as folder rename or OSC can update it. Returns `false` when
    /// no workspace matches or nothing changes.
    pub async fn rename_workspace(&self, id: WorkspaceId, raw_input: String) -> bool {
        let trimmed = raw_input.trim();
        let new_custom = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        };
        let mut s = self.inner.lock().await;
        let Some(w) = s.workspaces.iter_mut().find(|w| w.id == id) else {
            return false;
        };
        if w.custom_title == new_custom {
            return false;
        }
        w.custom_title = new_custom;
        drop(s);
        self.mark_dirty();
        true
    }

    pub async fn surface_title(&self, pane: PaneId, surface_id: SurfaceId) -> Option<String> {
        let s = self.inner.lock().await;
        for ws in &s.workspaces {
            for surface in &ws.surfaces {
                if let Some(title) = surface.root_pane.surface_title(pane, surface_id) {
                    return Some(title.to_string());
                }
            }
        }
        None
    }

    pub async fn rename_surface(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        title: String,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                if surface
                    .root_pane
                    .rename_surface(pane, surface_id, title.clone())
                {
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some(ws_id);
                }
            }
        }
        None
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
        for ws in s.workspaces.iter_mut() {
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
        s.workspaces.iter().find_map(|workspace| {
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
        let mut lifecycle = self.agent_lifecycle.lock().await;
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
            if suppress_screen_restore {
                self.suppress_agent_screen_restore(surface_id).await;
            } else {
                self.allow_agent_screen_restore(surface_id).await;
            }
        }
        drop(lifecycle);
        removed
    }

    pub async fn clear_dead_agent_activity(&self, surface_id: SurfaceId) -> Option<WorkspaceId> {
        self.remove_agent_presence_if_current(surface_id, None, None, None, None, true)
            .await
            .map(|removed| removed.workspace)
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

        let agent = agent.to_ascii_lowercase();
        let mut runtime = self.agent_lifecycle.lock().await;
        let wait_key = (surface_id, agent.clone(), session_id.to_string());
        let current = self.located_agent_presence(surface_id).await;
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
            return AgentLifecycleResult::default();
        }
        let runtime_before = runtime.clone();
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
                let permission_seq_key = (
                    surface_id,
                    agent.clone(),
                    session_id.to_string(),
                    scope.clone(),
                );
                let newer = match (
                    runtime
                        .permission_event_seq
                        .get(&permission_seq_key)
                        .copied(),
                    seq,
                ) {
                    (Some(current), Some(incoming)) => incoming > current,
                    (Some(_), None) => false,
                    _ => true,
                };
                if newer {
                    if let Some(seq) = seq {
                        runtime.permission_event_seq.insert(permission_seq_key, seq);
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
                let session_wait_seq_key = (
                    surface_id,
                    agent.clone(),
                    session_id.to_string(),
                    scope.clone(),
                );
                let newer = match (
                    runtime
                        .session_wait_event_seq
                        .get(&session_wait_seq_key)
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
                            .insert(session_wait_seq_key, seq);
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
                let session_wait_seq_key = (
                    surface_id,
                    agent.clone(),
                    session_id.to_string(),
                    scope.clone(),
                );
                let newer = match (
                    runtime
                        .session_wait_event_seq
                        .get(&session_wait_seq_key)
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
                            .insert(session_wait_seq_key, seq);
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
                let permission_seq_key = (
                    surface_id,
                    agent.clone(),
                    session_id.to_string(),
                    SESSION_PERMISSION_SCOPE.to_string(),
                );
                let newer = match (
                    runtime
                        .permission_event_seq
                        .get(&permission_seq_key)
                        .copied(),
                    seq,
                ) {
                    (Some(current), Some(incoming)) => incoming > current,
                    (Some(_), None) => false,
                    _ => true,
                };
                if newer {
                    if let Some(seq) = seq {
                        runtime.permission_event_seq.insert(permission_seq_key, seq);
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
                let balance = runtime
                    .waits
                    .entry(wait_key.clone())
                    .or_default()
                    .entry(item_id.clone())
                    .or_default();
                *balance += 1;
                let active = *balance > 0;
                if *balance == 0 {
                    runtime
                        .waits
                        .get_mut(&wait_key)
                        .expect("wait map exists")
                        .remove(&item_id);
                }
                active.then_some((NeedsInput, message, status_text))
            }
            AgentLifecycleEvent::WaitResolved { item_id } => {
                let waits = runtime.waits.entry(wait_key.clone()).or_default();
                let balance = waits.entry(item_id.clone()).or_default();
                let resolved_active = *balance > 0;
                *balance -= 1;
                if *balance == 0 {
                    waits.remove(&item_id);
                }
                if runtime.waits.get(&wait_key).is_some_and(HashMap::is_empty) {
                    runtime.waits.remove(&wait_key);
                }
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
        if workspace.is_none() && current.is_none() {
            *runtime = runtime_before;
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

    pub async fn report_agent_status_with_visibility(
        &self,
        surface_id: SurfaceId,
        report: AgentStatusReport,
        surface_visible: bool,
    ) -> Option<(WorkspaceId, Option<AgentStatus>)> {
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
            let mut lifecycle = self.agent_lifecycle.lock().await;
            let session_id = report.session_id.as_deref().unwrap();
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
                return None;
            }
            let before = lifecycle.clone();
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
            clear_agent_lifecycle_runtime(&mut lifecycle, surface_id, None, None, None);
            if let Some((displaced_key, displaced_pid, seq)) = displaced {
                let distinct_nested_identity = displaced_key.1 != agent
                    || displaced_pid
                        .zip(report.pid)
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
            if let Some(seq) = report.seq {
                lifecycle.last_seq.insert(key.clone(), seq);
                lifecycle.boundary_seq.insert(key, seq);
            }
            if agent == "codex" {
                lifecycle.codex_turns.insert(
                    (surface_id, session_id.to_string()),
                    CodexTurnLedger {
                        owner_pid: report.pid,
                        ..CodexTurnLedger::default()
                    },
                );
            }
            let accepted = self
                .apply_agent_status_report(surface_id, report, surface_visible)
                .await;
            if accepted.is_none() {
                *lifecycle = before;
            }
            if accepted.is_some() {
                self.allow_agent_screen_restore(surface_id).await;
            }
            drop(lifecycle);
            accepted
        } else if let Some((agent, session_id)) = hook_session {
            // Serialize every session-bearing direct report with lifecycle
            // teardown. This closes the gap where SessionEnd could tombstone a
            // session and a delayed Stop/notification would recreate it.
            let mut lifecycle = self.agent_lifecycle.lock().await;
            let key = (surface_id, agent.clone(), session_id.clone());
            if lifecycle.ended.contains_key(&key) {
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
                return None;
            }
            let boundary = lifecycle.boundary_seq.get(&key).copied();
            if match (boundary, report.seq) {
                (Some(floor), Some(incoming)) => incoming <= floor,
                (Some(_), None) => true,
                _ => false,
            } {
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

        let mut runtime = self.agent_lifecycle.lock().await;
        let wait_key = (surface_id, "codex".to_string(), session_id.to_string());
        if runtime.ended.contains_key(&wait_key) {
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
            return AgentLifecycleResult::default();
        }
        let before = runtime.clone();
        let pending = {
            let Some(ledger) = runtime
                .codex_turns
                .get_mut(&(surface_id, session_id.to_string()))
            else {
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
            *runtime = before;
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
        for ws in s.workspaces.iter_mut() {
            let mut found = false;
            let mut changed = false;
            for surface in ws.surfaces.iter_mut() {
                if let Some(existing) = surface.root_pane.agent_presence_for_surface(surface_id) {
                    preserve_live_agent_pid(&mut report, &existing);
                }
                if let Some(applied) = surface.root_pane.report_surface_agent(
                    surface_id,
                    report.clone(),
                    surface_visible,
                ) {
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
        self.reconcile_process_agent_candidates_inner(detected, None)
            .await
    }

    /// Apply a process-tree snapshot only to surfaces whose agent slot still
    /// matches the value observed before the blocking process walk. A native
    /// SessionStart that lands during that walk must not be displaced and
    /// tombstoned by older process truth.
    pub async fn reconcile_process_agent_candidates_if_unchanged(
        &self,
        detected: &[(SurfaceId, Vec<&str>)],
        observed: &[(SurfaceId, Option<AgentPresence>)],
    ) -> Vec<(WorkspaceId, Option<AgentStatus>)> {
        self.reconcile_process_agent_candidates_inner(detected, Some(observed))
            .await
    }

    async fn reconcile_process_agent_candidates_inner(
        &self,
        detected: &[(SurfaceId, Vec<&str>)],
        observed: Option<&[(SurfaceId, Option<AgentPresence>)]>,
    ) -> Vec<(WorkspaceId, Option<AgentStatus>)> {
        let mut changed: Vec<(WorkspaceId, Option<AgentStatus>)> = Vec::new();
        let mut created_surfaces: Vec<SurfaceId> = Vec::new();
        // Process truth may replace a hook-owned identity. Serialize the whole
        // mutation with hook lifecycle work (lifecycle -> state lock order) so
        // a handler cannot validate the old owner and overwrite the replacement.
        let mut lifecycle = self.agent_lifecycle.lock().await;
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
                    continue;
                }
                for ws in s.workspaces.iter_mut() {
                    let mut applied = None;
                    for surface in ws.surfaces.iter_mut() {
                        let previous = surface.root_pane.agent_presence_for_surface(*surface_id);
                        let name = select_process_agent_candidate(previous.as_ref(), candidates);
                        if let Some(result) =
                            surface.root_pane.reconcile_process_agent(*surface_id, name)
                        {
                            applied = Some((result, previous, name));
                            break;
                        }
                    }
                    if let Some((result, previous, name)) = applied {
                        if result {
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

    pub async fn report_agent_screen_signals_with_visibility(
        &self,
        surface_id: SurfaceId,
        screen_text: Option<&str>,
        osc_title: Option<&str>,
        surface_visible: bool,
    ) -> Option<(WorkspaceId, Option<AgentStatus>)> {
        let fingerprint = agent_screen_fingerprint(screen_text, osc_title);
        self.last_agent_screen_fingerprints
            .lock()
            .await
            .insert(surface_id, fingerprint);
        let detected_status = detect_agent_status_from_signals(screen_text, osc_title);
        let status_text = match detected_status {
            Some(AgentStatus::Working) => detect_agent_progress_text(screen_text),
            Some(AgentStatus::Blocked) => detect_agent_usage_limit_text(screen_text),
            Some(AgentStatus::Idle) if detect_agent_interruption(screen_text) => {
                Some("Interrupted")
            }
            _ => None,
        };
        let idle_agent_name = if matches!(detected_status, None | Some(AgentStatus::Idle)) {
            detect_agent_idle_name_from_signals(screen_text, osc_title)
        } else {
            None
        };
        let status = detected_status.or_else(|| idle_agent_name.map(|_| AgentStatus::Idle));
        let agent_name = if status.is_some() {
            detect_agent_name_from_signals(screen_text, osc_title).or(idle_agent_name)
        } else {
            None
        };
        if status.is_none() {
            if self
                .cleared_agent_surfaces
                .lock()
                .await
                .contains(&surface_id)
            {
                // Seeing a plain shell/non-agent frame after teardown is strong
                // evidence that any later agent frame belongs to a new remote
                // or screen-only session, including one that first appears
                // blocked rather than working.
                self.cleared_agent_screen_fingerprints
                    .lock()
                    .await
                    .insert(surface_id, Some(fingerprint));
                self.cleared_agent_saw_no_signal
                    .lock()
                    .await
                    .insert(surface_id);
            }
            return self
                .clear_screen_agent_signal(surface_id, surface_visible)
                .await;
        }
        let status = status?;
        // Serialize the wait check, teardown latch check, and screen mutation
        // with SessionEnd/dead-PID teardown. Otherwise teardown can remove the
        // presence after this check but before the state lock below, allowing
        // the stale screen frame to recreate a ghost presence.
        let lifecycle = self.agent_lifecycle.lock().await;
        if status != AgentStatus::Blocked
            && (lifecycle.waits.iter().any(|((surface, _, _), waits)| {
                *surface == surface_id && waits.values().any(|balance| *balance > 0)
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
            return None;
        }
        if self
            .cleared_agent_surfaces
            .lock()
            .await
            .contains(&surface_id)
        {
            let baseline = self
                .cleared_agent_screen_fingerprints
                .lock()
                .await
                .get(&surface_id)
                .copied()
                .flatten();
            let screen_changed_after_clear = baseline.is_some_and(|old| old != fingerprint);
            let saw_no_agent_frame = self
                .cleared_agent_saw_no_signal
                .lock()
                .await
                .contains(&surface_id);
            if !screen_changed_after_clear || !saw_no_agent_frame {
                // A dead TUI can leave changing spinner/"Working" frames in
                // scrollback. Require a plain shell/non-agent frame before
                // treating later screen signals as a new remote agent.
                self.cleared_agent_screen_fingerprints
                    .lock()
                    .await
                    .insert(surface_id, Some(fingerprint));
                return None;
            }
            self.allow_agent_screen_restore(surface_id).await;
        }
        let mut s = self.inner.lock().await;
        let mut outcome = None;
        for ws in s.workspaces.iter_mut() {
            let mut found = false;
            let mut changed = false;
            for surface in ws.surfaces.iter_mut() {
                if let Some(applied) = surface.root_pane.report_surface_agent_signal(
                    surface_id,
                    status,
                    "flowmux:screen",
                    agent_name,
                    status_text,
                    surface_visible,
                ) {
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

    pub async fn workspace_agent_blocks(
        &self,
        workspace: WorkspaceId,
        mru: &[PaneId],
    ) -> Vec<WorkspaceAgentBlock> {
        let s = self.inner.lock().await;
        s.workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .map(|ws| ws.collect_agent_blocks(mru))
            .unwrap_or_default()
    }

    fn mark_all_agents_seen_locked(ws: &mut Workspace) -> bool {
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
        for ws in &s.workspaces {
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

    pub async fn update_surface_cwd(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        cwd: std::path::PathBuf,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.lock().await;
        let updated = update_surface_cwd_in_state(&mut s, pane, surface_id, cwd);
        drop(s);
        if updated.is_some() {
            self.mark_dirty();
        }
        updated
    }

    /// Store the last URL of a browser surface in state. Called in response to
    /// webview uri_notify so app exit/relaunch can restore the last viewed page.
    pub async fn update_browser_url(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        url: String,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.lock().await;
        let mut updated = None;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                if surface
                    .root_pane
                    .set_surface_browser_url(pane, surface_id, url.clone())
                {
                    updated = Some(ws.id);
                    break;
                }
            }
            if updated.is_some() {
                break;
            }
        }
        drop(s);
        if updated.is_some() {
            self.mark_dirty();
        }
        updated
    }

    /// Apply an automatic title from external signals, such as browser page title
    /// or terminal OSC 0/2, to a surface. User-renamed surfaces (title_locked)
    /// are left untouched.
    ///
    /// Applies cmux's single-panel auto-sync rule in the same call: if the
    /// workspace has no split (single Leaf), the updated surface is that leaf's
    /// active tab, and there is no user-provided `custom_title`, the workspace's
    /// automatic value (`name`) follows the same title. This lets
    /// [`Workspace::display_title`] naturally reflect active tab OSC titles such
    /// as "Claude Code". Splits or locked custom titles block automatic workspace
    /// label changes.
    pub async fn update_surface_auto_title(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        title: String,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.lock().await;
        let mut updated = None;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                if surface
                    .root_pane
                    .set_surface_title_auto(pane, surface_id, title.clone())
                {
                    updated = Some(ws.id);
                    break;
                }
            }
            if updated.is_some() {
                break;
            }
        }
        drop(s);
        if updated.is_some() {
            self.mark_dirty();
        }
        updated
    }

    /// Set the workspace's automatic value (`name`) directly. The GTK side knows
    /// the focused pane's active surface title, so the new design explicitly
    /// updates "current focused tab = workspace name" through this setter.
    /// `custom_title` is left untouched so user-locked labels remain. Returns
    /// `false` for no changes or missing workspaces.
    pub async fn set_workspace_name(&self, id: WorkspaceId, name: String) -> bool {
        let mut s = self.inner.lock().await;
        let Some(w) = s.workspaces.iter_mut().find(|w| w.id == id) else {
            return false;
        };
        if w.name == name {
            return false;
        }
        w.name = name;
        drop(s);
        self.mark_dirty();
        true
    }

    pub fn update_surface_cwd_blocking(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        cwd: std::path::PathBuf,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.blocking_lock();
        let updated = update_surface_cwd_in_state(&mut s, pane, surface_id, cwd);
        drop(s);
        if updated.is_some() {
            self.mark_dirty();
        }
        updated
    }

    pub async fn update_surface_scrollback(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        snapshot: impl Into<TerminalScrollback>,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.lock().await;
        let updated = update_surface_scrollback_in_state(&mut s, pane, surface_id, snapshot.into());
        drop(s);
        if updated.is_some() {
            self.mark_dirty();
        }
        updated
    }

    pub fn update_surface_scrollback_blocking(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        snapshot: impl Into<TerminalScrollback>,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.blocking_lock();
        let updated = update_surface_scrollback_in_state(&mut s, pane, surface_id, snapshot.into());
        drop(s);
        if updated.is_some() {
            self.mark_dirty();
        }
        updated
    }

    pub async fn update_editor_session(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        session: EditorSessionState,
    ) -> Option<WorkspaceId> {
        let mut state = self.inner.lock().await;
        let updated = update_editor_session_in_state(&mut state, pane, surface_id, session);
        drop(state);
        if updated.is_some() {
            self.mark_dirty();
        }
        updated
    }

    pub fn update_editor_session_blocking(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        session: EditorSessionState,
    ) -> Option<WorkspaceId> {
        let mut state = self.inner.blocking_lock();
        let updated = update_editor_session_in_state(&mut state, pane, surface_id, session);
        drop(state);
        if updated.is_some() {
            self.mark_dirty();
        }
        updated
    }

    /// Called when reordering terminal/browser tabs inside a pane by drag and
    /// drop. Moves `surface_id` within the same pane to `target_index`. The index
    /// is the final position after applying the move and clamps to the end when
    /// too large. Returns `None` for no changes or missing surfaces so callers
    /// leave GTK widgets untouched. Active SurfaceId is unaffected by reorder,
    /// so the same tab remains active after moving.
    pub async fn reorder_surface_in_pane(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
        target_index: usize,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                if surface
                    .root_pane
                    .reorder_surface_in_leaf(pane, surface_id, target_index)
                {
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some(ws_id);
                }
            }
        }
        None
    }

    /// Called when reordering workspaces in the side panel by drag and drop.
    /// Moves the workspace identified by `id` to `target_index` inside
    /// `workspace_order`. The index is the final position after applying the
    /// move and clamps to the end when too large. Same-position moves or missing
    /// workspaces return `false`.
    pub async fn reorder_workspace(&self, id: WorkspaceId, target_index: usize) -> bool {
        let mut s = self.inner.lock().await;
        let Some(current) = s.workspace_order.iter().position(|x| *x == id) else {
            return false;
        };
        let len = s.workspace_order.len();
        if len == 0 {
            return false;
        }
        let target = target_index.min(len - 1);
        if current == target {
            return false;
        }
        let removed = s.workspace_order.remove(current);
        s.workspace_order.insert(target, removed);
        drop(s);
        self.mark_dirty();
        true
    }

    /// Saved window size/maximized state. `None` on first launch.
    pub fn window_layout_blocking(&self) -> Option<WindowLayout> {
        self.inner.blocking_lock().window.clone()
    }

    /// Saved side-panel divider pixel position. `None` on first launch.
    pub fn sidebar_position_blocking(&self) -> Option<i32> {
        self.inner.blocking_lock().sidebar_position
    }

    /// Record window size/maximized state in state. This blocking variant is
    /// used because close handling calls it synchronously on the GTK main thread,
    /// where the async runtime is not guaranteed to still be alive.
    pub fn set_window_layout_blocking(&self, layout: WindowLayout) {
        let mut s = self.inner.blocking_lock();
        if s.window.as_ref() == Some(&layout) {
            return;
        }
        s.window = Some(layout);
        drop(s);
        self.mark_dirty();
    }

    /// Record the divider pixel position between side panel and content area.
    pub fn set_sidebar_position_blocking(&self, position: i32) {
        let mut s = self.inner.blocking_lock();
        if s.sidebar_position == Some(position) {
            return;
        }
        s.sidebar_position = Some(position);
        drop(s);
        self.mark_dirty();
    }

    /// Apply a pane split divider ratio to the model. `split_id` is the PaneId
    /// of a `Pane::Split` node in the tree. Returns `false` if no matching split
    /// exists or the ratio is unchanged so callers can skip dirty marking.
    pub fn set_pane_split_ratio_blocking(&self, split_id: PaneId, ratio: f32) -> bool {
        let mut s = self.inner.blocking_lock();
        let mut updated = false;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                if surface.root_pane.set_split_ratio(split_id, ratio) {
                    updated = true;
                    break;
                }
            }
            if updated {
                break;
            }
        }
        drop(s);
        if updated {
            self.mark_dirty();
        }
        updated
    }

    pub async fn set_pane_split_ratio(&self, split_id: PaneId, ratio: f32) -> bool {
        let mut s = self.inner.lock().await;
        let mut updated = false;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                if surface.root_pane.set_split_ratio(split_id, ratio) {
                    updated = true;
                    break;
                }
            }
            if updated {
                break;
            }
        }
        drop(s);
        if updated {
            self.mark_dirty();
        }
        updated
    }

    pub async fn parent_split_for_pane(&self, pane: PaneId) -> Option<PaneId> {
        let s = self.inner.lock().await;
        for ws in &s.workspaces {
            for surface in &ws.surfaces {
                if let Some(split_id) = surface.root_pane.parent_split_id(pane) {
                    return Some(split_id);
                }
            }
        }
        None
    }

    /// Remove an entire workspace. Used by the sidebar's X close
    /// button. Returns true if a workspace with that id existed.
    pub async fn remove_workspace(&self, id: WorkspaceId) -> bool {
        let mut s = self.inner.lock().await;
        let closed_surfaces = s
            .workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .map(workspace_pane_surface_ids)
            .unwrap_or_default();
        let before = s.workspaces.len();
        s.workspaces.retain(|w| w.id != id);
        s.workspace_order.retain(|x| *x != id);
        if s.active_workspace == Some(id) {
            s.active_workspace = s.workspace_order.first().copied();
        }
        let removed = s.workspaces.len() < before;
        drop(s);
        if removed {
            self.forget_cleared_agent_surfaces(&closed_surfaces).await;
            self.mark_dirty();
        }
        removed
    }

    /// Remove every workspace at once. Used by the sidebar context
    /// menu's "Close all tabs" item. Returns the ids that were removed
    /// (in their prior order) so the caller can tear down each
    /// workspace's GUI page. Clears the active-workspace pointer since
    /// nothing is left to activate.
    pub async fn remove_all_workspaces(&self) -> Vec<WorkspaceId> {
        let mut s = self.inner.lock().await;
        let closed_surfaces = s
            .workspaces
            .iter()
            .flat_map(workspace_pane_surface_ids)
            .collect::<Vec<_>>();
        let removed: Vec<WorkspaceId> = s.workspace_order.clone();
        let removed = if removed.is_empty() {
            s.workspaces.iter().map(|w| w.id).collect()
        } else {
            removed
        };
        s.workspaces.clear();
        s.workspace_order.clear();
        s.active_workspace = None;
        drop(s);
        if !removed.is_empty() {
            self.forget_cleared_agent_surfaces(&closed_surfaces).await;
            self.mark_dirty();
        }
        removed
    }

    pub async fn workspace_for_pane(&self, pane: PaneId) -> Option<WorkspaceId> {
        let s = self.inner.lock().await;
        for ws in &s.workspaces {
            for surface in &ws.surfaces {
                if pane_tree_contains(&surface.root_pane, pane) {
                    return Some(ws.id);
                }
            }
        }
        None
    }

    /// Find a leaf pane whose currently-active tab title starts with
    /// `needle` (ASCII case-insensitive). Used by the Notify dispatcher
    /// as a fallback when the hook source couldn't pass pane/surface
    /// info — e.g. the Flatpak OpenCode plugin path, where `flatpak
    /// run` resets env before the in-sandbox CLI can read
    /// `FLOWMUX_PANE_ID`, so the hook-driven Notify arrives with
    /// `pane=None`. Without recovery the daemon stores the entry with
    /// no workspace, so the sidebar can't blink (`mark_attention`
    /// needs a workspace id) and the bell click can't navigate
    /// (`focus_pane` needs a pane id). With this lookup the daemon
    /// rebuilds the routing context from the pane title flowmux
    /// already tracks (e.g. the active tab in pane 86ff5134 has title
    /// "OpenCode" once the agent attaches its PTY, which matches the
    /// "OpenCode" prefix of the Notify's `title="OpenCode ready"`).
    ///
    /// Returns the first matching `(workspace, pane, surface)` tuple.
    /// First-match policy is intentional: when only one pane runs the
    /// agent the answer is unambiguous, and when several do, blinking
    /// one of them still beats blinking none. We never invent
    /// associations across workspaces — the candidate must actually
    /// own a leaf whose active surface title matches.
    pub async fn find_pane_by_active_title_prefix(
        &self,
        needle: &str,
    ) -> Option<(WorkspaceId, PaneId, SurfaceId)> {
        if needle.is_empty() {
            return None;
        }
        let needle_lower = needle.to_ascii_lowercase();
        let s = self.inner.lock().await;
        for ws in &s.workspaces {
            for surface in &ws.surfaces {
                if let Some((pane_id, surface_id)) =
                    find_active_title_prefix(&surface.root_pane, &needle_lower)
                {
                    return Some((ws.id, pane_id, surface_id));
                }
            }
        }
        None
    }

    pub async fn get_workspace(&self, id: WorkspaceId) -> Option<Workspace> {
        let s = self.inner.lock().await;
        s.workspaces.iter().find(|w| w.id == id).cloned()
    }

    /// UI metadata clone that intentionally omits terminal replay buffers.
    pub async fn get_workspace_without_scrollback(&self, id: WorkspaceId) -> Option<Workspace> {
        let s = self.inner.lock().await;
        s.workspaces
            .iter()
            .find(|workspace| workspace.id == id)
            .map(Workspace::clone_without_scrollback)
    }

    /// Active workspace, falling back to the first one available.
    pub async fn active_or_first(&self) -> Option<WorkspaceId> {
        let s = self.inner.lock().await;
        s.active_workspace
            .or_else(|| s.workspaces.first().map(|w| w.id))
    }

    /// Add a fresh terminal surface to a workspace. Used by the
    /// "new surface" keyboard shortcut.
    pub async fn add_terminal_surface(
        &self,
        workspace: WorkspaceId,
        cwd: Option<std::path::PathBuf>,
    ) -> Option<SurfaceId> {
        let mut s = self.inner.lock().await;
        let w = s.workspaces.iter_mut().find(|w| w.id == workspace)?;
        let pane = w.surfaces.first()?.root_pane.first_leaf_id()?;
        let cwd = cwd
            .or_else(|| w.surfaces[0].root_pane.terminal_surface_cwd(pane))
            .or_else(|| Some(w.root_dir.clone()));
        let title = terminal_tab_title_for_cwd(cwd.as_deref());
        let surface = PaneSurface::terminal(title, cwd);
        let surface_id = w.surfaces[0].root_pane.add_surface_to_leaf(pane, surface)?;
        drop(s);
        self.mark_dirty();
        Some(surface_id)
    }

    pub async fn add_terminal_surface_to_pane(
        &self,
        pane: PaneId,
        cwd: Option<std::path::PathBuf>,
    ) -> Option<(WorkspaceId, SurfaceId)> {
        self.add_terminal_surface_to_pane_with_shell(pane, cwd, None)
            .await
    }

    pub async fn add_terminal_surface_to_pane_with_shell(
        &self,
        pane: PaneId,
        cwd: Option<std::path::PathBuf>,
        shell: Option<String>,
    ) -> Option<(WorkspaceId, SurfaceId)> {
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                let resolved_cwd = cwd
                    .clone()
                    .or_else(|| surface.root_pane.terminal_surface_cwd(pane))
                    .or_else(|| Some(ws.root_dir.clone()));
                let title = terminal_tab_title_for_cwd(resolved_cwd.as_deref());
                let mut tab = PaneSurface::terminal(title, resolved_cwd);
                if let SurfaceKind::Terminal {
                    shell: tab_shell, ..
                } = &mut tab.kind
                {
                    *tab_shell = shell.clone();
                }
                if let Some(surface_id) = surface.root_pane.add_surface_to_leaf(pane, tab) {
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some((ws_id, surface_id));
                }
            }
        }
        None
    }

    /// Add a browser surface to a workspace and return its id.
    pub async fn add_browser_surface(
        &self,
        workspace: WorkspaceId,
        url: String,
    ) -> Option<SurfaceId> {
        let mut s = self.inner.lock().await;
        let w = s.workspaces.iter_mut().find(|w| w.id == workspace)?;
        let pane = w.surfaces.first()?.root_pane.first_leaf_id()?;
        let tab = PaneSurface::browser("Browser", url);
        let surface_id = w.surfaces[0].root_pane.add_surface_to_leaf(pane, tab)?;
        drop(s);
        self.mark_dirty();
        Some(surface_id)
    }

    /// Walk every workspace's pane tree looking for a browser leaf
    /// that lives on the right side of `from`. cmux's
    /// `preferredBrowserTargetPane` policy: a `flowmux browser open`
    /// invoked from a terminal pane reuses an existing right-sibling
    /// browser pane instead of creating a new split. Returns the
    /// browser leaf's `PaneId` when found.
    pub async fn find_right_sibling_browser_leaf(&self, from: PaneId) -> Option<PaneId> {
        let s = self.inner.lock().await;
        for ws in s.workspaces.iter() {
            for surface in ws.surfaces.iter() {
                if let Some(p) = surface.root_pane.find_right_sibling_browser_leaf(from) {
                    return Some(p);
                }
            }
        }
        None
    }

    pub async fn add_browser_surface_to_pane(
        &self,
        pane: PaneId,
        url: String,
    ) -> Option<(WorkspaceId, SurfaceId)> {
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                let tab = PaneSurface::browser("Browser", url.clone());
                if let Some(surface_id) = surface.root_pane.add_surface_to_leaf(pane, tab) {
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some((ws_id, surface_id));
                }
            }
        }
        None
    }

    /// Add an editor surface to a specific pane using the supplied document root.
    pub async fn add_editor_surface_to_pane(
        &self,
        pane: PaneId,
        editor_root: std::path::PathBuf,
    ) -> Option<(WorkspaceId, SurfaceId)> {
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            for surface in ws.surfaces.iter_mut() {
                let tab = PaneSurface::editor("Editor", editor_root.clone());
                if let Some(surface_id) = surface.root_pane.add_surface_to_leaf(pane, tab) {
                    let ws_id = ws.id;
                    drop(s);
                    self.mark_dirty();
                    return Some((ws_id, surface_id));
                }
            }
        }
        None
    }

    pub async fn set_active_surface(
        &self,
        pane: PaneId,
        surface_id: SurfaceId,
    ) -> Option<WorkspaceId> {
        let mut s = self.inner.lock().await;
        for ws in s.workspaces.iter_mut() {
            let mut hit = false;
            for surface in ws.surfaces.iter_mut() {
                if surface.root_pane.set_active_surface(pane, surface_id) {
                    surface.root_pane.mark_surface_agent_seen(surface_id);
                    hit = true;
                    break;
                }
            }
            if hit {
                let ws_id = ws.id;
                drop(s);
                self.mark_dirty();
                return Some(ws_id);
            }
        }
        None
    }

    /// Peek-only: how many leaf panes the workspace containing
    /// `target` has, plus the workspace id. Used by the GUI to decide
    /// whether closing `target` would also close the workspace, so it
    /// can put up a confirmation dialog before the mutation runs.
    pub async fn workspace_pane_count_for(&self, target: PaneId) -> Option<(WorkspaceId, usize)> {
        let s = self.inner.lock().await;
        for ws in &s.workspaces {
            let mut count = 0usize;
            let mut found = false;
            for surf in &ws.surfaces {
                surf.root_pane.for_each_leaf(|id| {
                    count += 1;
                    if id == target {
                        found = true;
                    }
                });
            }
            if found {
                return Some((ws.id, count));
            }
        }
        None
    }

    /// Peek-only: number of tab surfaces inside `pane`. `None` when
    /// the pane is unknown or it is a non-tabbed leaf. Used together
    /// with `workspace_pane_count_for` to decide whether closing a
    /// surface (tab) ends up closing the whole workspace.
    pub async fn tab_count_in_pane(&self, pane: PaneId) -> Option<usize> {
        let s = self.inner.lock().await;
        for ws in &s.workspaces {
            for surf in &ws.surfaces {
                if let Some(count) = pane_tab_count(&surf.root_pane, pane) {
                    return Some(count);
                }
            }
        }
        None
    }

    pub async fn close_surface(&self, pane: PaneId, surface_id: SurfaceId) -> Option<CloseOutcome> {
        let mut s = self.inner.lock().await;
        let mut result = None;
        for ws_idx in 0..s.workspaces.len() {
            for surf_idx in 0..s.workspaces[ws_idx].surfaces.len() {
                let outcome = s.workspaces[ws_idx].surfaces[surf_idx]
                    .root_pane
                    .close_surface_in_leaf(pane, surface_id);
                match outcome {
                    CloseSurfaceOutcome::SurfaceRemoved => {
                        let ws_id = s.workspaces[ws_idx].id;
                        result = Some(CloseOutcome::SurfaceRemoved { workspace: ws_id });
                        break;
                    }
                    CloseSurfaceOutcome::LastSurfaceRemoved => {
                        result = remove_pane_leaf_locked(&mut s, pane);
                        break;
                    }
                    CloseSurfaceOutcome::NotFound => {}
                }
            }
            if result.is_some() {
                break;
            }
        }
        drop(s);
        if result.is_some() {
            self.forget_cleared_agent_surfaces(&[surface_id]).await;
            self.mark_dirty();
        }
        result
    }

    pub fn mark_dirty(&self) {
        self.dirty_generation.fetch_add(1, Ordering::Release);
        self.dirty.notify_one();
    }

    async fn wait_for_stable_dirty_generation(
        &self,
        persisted_generation: u64,
        debounce: Duration,
    ) -> u64 {
        let mut observed = loop {
            let notified = self.dirty.notified();
            let current = self.dirty_generation.load(Ordering::Acquire);
            if current != persisted_generation {
                break current;
            }
            notified.await;
        };

        loop {
            tokio::time::sleep(debounce).await;
            let current = self.dirty_generation.load(Ordering::Acquire);
            if current == observed {
                return current;
            }
            observed = current;
        }
    }

    pub async fn persist_loop(&self) {
        let mut persisted_generation = 0;
        loop {
            let stable_generation = self
                .wait_for_stable_dirty_generation(persisted_generation, PERSIST_DEBOUNCE)
                .await;
            // Ephemeral stores still observe the dirty bit so callers
            // do not need to special-case mutation paths, but they
            // never reach the disk.
            if !self.persist_enabled() {
                persisted_generation = stable_generation;
                continue;
            }
            let snapshot_generation = self.dirty_generation.load(Ordering::Acquire);
            let snap = self.snapshot().await;
            let workspaces = snap.workspaces.len();
            let started = Instant::now();
            match save_snapshot_blocking(snap, self.persistence).await {
                Ok(file_size) => info!(
                    generation = snapshot_generation,
                    workspaces,
                    file_size,
                    elapsed_ms = started.elapsed().as_millis(),
                    "state persisted"
                ),
                Err(e) => error!(error = %e, "state save failed"),
            }
            // A mutation after the snapshot keeps a higher generation and starts
            // a fresh debounce. Stale Notify permits from mutations already in
            // this snapshot are consumed without scheduling another write.
            persisted_generation = snapshot_generation;
        }
    }

    pub async fn save_now(&self) -> Result<(), flowmux_state::StateError> {
        if !self.persist_enabled() {
            return Ok(());
        }
        let snap = self.snapshot().await;
        save_snapshot_blocking(snap, self.persistence)
            .await
            .map(|_| ())
    }

    pub fn save_now_blocking(&self) -> Result<(), flowmux_state::StateError> {
        if !self.persist_enabled() {
            return Ok(());
        }
        let snap = self.inner.blocking_lock().clone();
        match self.persistence {
            PersistenceMode::Full => flowmux_state::save_owned(snap),
            PersistenceMode::Window(owner) => flowmux_state::save_window_owned(owner, snap),
            PersistenceMode::Disabled => Ok(()),
        }
    }
}

/// True when the pane tree has any leaf with id `target`. Walks the
/// tree with early-exit so a hit on the left subtree skips the right.
fn pane_tree_contains(tree: &Pane, target: PaneId) -> bool {
    match tree {
        Pane::Leaf { id, .. } => *id == target,
        Pane::Split { first, second, .. } => {
            pane_tree_contains(first, target) || pane_tree_contains(second, target)
        }
    }
}

/// Walk the pane tree and return the first leaf whose currently
/// active tab title (ASCII lowercased) starts with `needle_lower`.
/// Returns `(pane_id, active_surface_id)`. Used by
/// [`StateStore::find_pane_by_active_title_prefix`] as a fallback
/// route for Notify events that arrive with no pane info.
fn find_active_title_prefix(tree: &Pane, needle_lower: &str) -> Option<(PaneId, SurfaceId)> {
    match tree {
        Pane::Leaf { id, content } => match content {
            PaneContent::Tabs { active, surfaces } => surfaces
                .iter()
                .find(|s| s.id == *active)
                .filter(|s| active_title_matches_needle(&s.title, needle_lower))
                .map(|s| (*id, s.id)),
            // Legacy leaf shapes carry no per-tab title; they should
            // have been normalised on load, but stay defensive.
            PaneContent::Terminal { .. } | PaneContent::Browser { .. } => None,
        },
        Pane::Split { first, second, .. } => find_active_title_prefix(first, needle_lower)
            .or_else(|| find_active_title_prefix(second, needle_lower)),
    }
}

/// Count the tab surfaces inside the leaf identified by `target` in
/// the given pane tree. Returns `None` when `target` is not a
/// `PaneContent::Tabs` leaf (Terminal/Browser leaves with no tabs).
fn active_title_matches_needle(title: &str, needle_lower: &str) -> bool {
    let title_lower = title.to_ascii_lowercase();
    let Some(rest) = title_lower.strip_prefix(needle_lower) else {
        return false;
    };
    match rest.chars().next() {
        None => true,
        Some('|' | ':') => true,
        Some(ch) => ch.is_whitespace(),
    }
}

fn pane_tab_count(tree: &Pane, target: PaneId) -> Option<usize> {
    match tree {
        Pane::Leaf { id, content } if *id == target => match content {
            PaneContent::Tabs { surfaces, .. } => Some(surfaces.len()),
            PaneContent::Terminal { .. } | PaneContent::Browser { .. } => None,
        },
        Pane::Leaf { .. } => None,
        Pane::Split { first, second, .. } => {
            pane_tab_count(first, target).or_else(|| pane_tab_count(second, target))
        }
    }
}

fn update_surface_cwd_in_state(
    state: &mut State,
    pane: PaneId,
    surface_id: SurfaceId,
    cwd: std::path::PathBuf,
) -> Option<WorkspaceId> {
    for ws in state.workspaces.iter_mut() {
        for surface in ws.surfaces.iter_mut() {
            if surface
                .root_pane
                .set_surface_cwd(pane, surface_id, cwd.clone())
            {
                return Some(ws.id);
            }
        }
    }
    None
}

fn update_surface_scrollback_in_state(
    state: &mut State,
    pane: PaneId,
    surface_id: SurfaceId,
    snapshot: TerminalScrollback,
) -> Option<WorkspaceId> {
    for ws in state.workspaces.iter_mut() {
        for surface in ws.surfaces.iter_mut() {
            if surface
                .root_pane
                .find_surface_ref(pane, surface_id)
                .is_some()
            {
                return surface
                    .root_pane
                    .set_surface_scrollback_snapshot(pane, surface_id, snapshot)
                    .then_some(ws.id);
            }
        }
    }
    None
}

fn update_editor_session_in_state(
    state: &mut State,
    pane: PaneId,
    surface_id: SurfaceId,
    session: EditorSessionState,
) -> Option<WorkspaceId> {
    for workspace in &mut state.workspaces {
        for surface in &mut workspace.surfaces {
            if surface
                .root_pane
                .find_surface_ref(pane, surface_id)
                .is_some()
            {
                return surface
                    .root_pane
                    .set_surface_editor_session(pane, surface_id, session)
                    .then_some(workspace.id);
            }
        }
    }
    None
}

fn normalize_state(state: &mut State) -> bool {
    let normalized_order = workspace_ids_in_display_order(state);
    let mut changed = normalized_order != state.workspace_order;
    state.workspace_order = normalized_order;

    if state
        .active_workspace
        .is_some_and(|active| !state.workspace_order.contains(&active))
    {
        state.active_workspace = state.workspace_order.first().copied();
        changed = true;
    }

    for ws in &mut state.workspaces {
        for surface in &mut ws.surfaces {
            let fallback_cwd = match &surface.kind {
                SurfaceKind::Terminal { cwd, .. } => cwd.clone(),
                SurfaceKind::Browser { .. } | SurfaceKind::Editor { .. } => None,
            };
            changed |= surface.root_pane.normalize_leaf_tabs(fallback_cwd);
        }
        if ws.surfaces.len() > 1 {
            changed |= migrate_top_level_surfaces_to_first_pane(ws);
        }
        for surface in &mut ws.surfaces {
            changed |= surface.root_pane.normalize_leaf_tabs(None);
        }
    }
    changed
}

fn migrate_top_level_surfaces_to_first_pane(ws: &mut Workspace) -> bool {
    let Some(target_pane) = ws.surfaces[0].root_pane.first_leaf_id() else {
        return false;
    };
    let extra_surfaces = ws.surfaces.split_off(1);
    let changed = !extra_surfaces.is_empty();
    for surface in extra_surfaces {
        let Some(mut tab) = first_active_pane_surface(&surface.root_pane) else {
            continue;
        };
        tab.id = surface.id;
        if !surface.title.is_empty() {
            tab.title = surface.title;
        }
        ws.surfaces[0]
            .root_pane
            .add_surface_to_leaf(target_pane, tab);
    }
    changed
}

fn first_active_pane_surface(pane: &Pane) -> Option<PaneSurface> {
    match pane {
        Pane::Leaf { content, .. } => content.active_surface().cloned(),
        Pane::Split { first, second, .. } => {
            first_active_pane_surface(first).or_else(|| first_active_pane_surface(second))
        }
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
    state.workspaces.iter().find_map(|workspace| {
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
            surface
                .agent
                .take()
                .map(|presence| (*id, surface.title.clone(), presence))
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

    fn first_pane(ws: &Workspace) -> PaneId {
        ws.surfaces[0].root_pane.first_leaf_id().unwrap()
    }

    fn first_pane_tab_count(ws: &Workspace) -> usize {
        let Pane::Leaf { content, .. } = &ws.surfaces[0].root_pane else {
            panic!("expected single leaf")
        };
        match content {
            PaneContent::Tabs { surfaces, .. } => surfaces.len(),
            PaneContent::Terminal { .. } | PaneContent::Browser { .. } => 1,
        }
    }

    fn first_pane_active_surface(ws: &Workspace) -> SurfaceId {
        let pane = first_pane(ws);
        ws.surfaces[0]
            .root_pane
            .active_surface_id(pane)
            .expect("expected active surface")
    }

    async fn assert_agent_lifecycle(agent: &str, initial_status: AgentStatus) {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(
                Some(format!("{agent}-lifecycle")),
                std::path::PathBuf::from(format!("/tmp/{agent}-lifecycle")),
            )
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        let report = |status, activity, seq| AgentStatusReport {
            name: agent.into(),
            status: Some(status),
            activity,
            pid: None,
            source: Some("flowmux:hook".into()),
            seq: Some(seq),
            message: None,
            custom_status: None,
            session_id: Some(format!("{agent}-session")),
            session_name: None,
            messaging_socket: None,
        };

        store
            .report_agent_status_with_visibility(surface, report(initial_status, None, 1), true)
            .await;
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(initial_status),
            "{agent}: session start"
        );

        store
            .report_agent_status_with_visibility(
                surface,
                report(
                    AgentStatus::Working,
                    Some(flowmux_core::AgentActivity::Running),
                    2,
                ),
                true,
            )
            .await;
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working),
            "{agent}: running"
        );

        store
            .report_agent_status_with_visibility(
                surface,
                report(
                    AgentStatus::Blocked,
                    Some(flowmux_core::AgentActivity::NeedsInput),
                    3,
                ),
                false,
            )
            .await;
        assert_eq!(
            store.workspace_agent_attention_status(ws_id).await,
            Some(AgentStatus::Blocked),
            "{agent}: hidden input request raises attention"
        );

        store.set_active_workspace(Some(ws_id)).await;
        assert_eq!(
            store.workspace_agent_attention_status(ws_id).await,
            None,
            "{agent}: opening the workspace acknowledges the alert"
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Blocked),
            "{agent}: acknowledgement preserves the blocked state"
        );

        store
            .report_agent_status_with_visibility(
                surface,
                report(
                    AgentStatus::Working,
                    Some(flowmux_core::AgentActivity::Running),
                    4,
                ),
                true,
            )
            .await;
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working),
            "{agent}: resumed"
        );

        store.set_active_workspace(None).await;
        store
            .report_agent_status_with_visibility(
                surface,
                report(
                    AgentStatus::Idle,
                    Some(flowmux_core::AgentActivity::Idle),
                    5,
                ),
                false,
            )
            .await;
        assert_eq!(
            store.workspace_agent_attention_status(ws_id).await,
            Some(AgentStatus::Done),
            "{agent}: hidden completion raises a done alert"
        );

        store.set_active_workspace(Some(ws_id)).await;
        assert_eq!(
            store.workspace_agent_attention_status(ws_id).await,
            None,
            "{agent}: opening the completed workspace clears the alert"
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle),
            "{agent}: acknowledged completion settles to idle"
        );

        assert_eq!(store.set_agent_activity(surface, None).await, Some(ws_id));
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            None,
            "{agent}: exit clears presence"
        );
    }

    #[tokio::test]
    async fn create_workspace_sets_order_active_surface_and_color() {
        let store = StateStore::new_lazy(State::default());
        let root = std::path::PathBuf::from("/tmp/demo");
        let id = store.create_workspace(None, root.clone()).await;

        let state = store.snapshot().await;
        assert_eq!(state.workspace_order, vec![id]);
        assert_eq!(state.active_workspace, Some(id));
        assert_eq!(state.workspaces.len(), 1);
        let ws = &state.workspaces[0];
        assert_eq!(ws.name, "demo");
        assert!(ws.color.as_deref().is_some_and(|c| c.starts_with('#')));
        assert_eq!(ws.surfaces.len(), 1);
        assert!(matches!(
            &ws.surfaces[0].kind,
            SurfaceKind::Terminal { cwd: Some(cwd), .. } if cwd == &root
        ));
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected tabbed leaf")
        };
        assert_eq!(surfaces[0].title, "demo");
        assert!(!surfaces[0].title_locked);
    }

    #[tokio::test]
    async fn create_workspace_colors_stay_distinct_until_palette_exhausted() {
        let store = StateStore::new_lazy(State::default());
        let n = flowmux_core::WORKSPACE_PALETTE.len();
        for i in 0..n {
            store
                .create_workspace(None, std::path::PathBuf::from(format!("/tmp/ws{i}")))
                .await;
        }
        let state = store.snapshot().await;
        let colors: Vec<String> = state
            .workspaces
            .iter()
            .filter_map(|w| w.color.clone())
            .collect();
        let mut unique = colors.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            n,
            "first {n} workspaces should all differ: {colors:?}"
        );
    }

    #[tokio::test]
    async fn report_agent_status_surfaces_in_workspace_tree() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        let result = store
            .report_agent_status(
                surface,
                AgentStatusReport {
                    name: "claude".into(),
                    status: Some(AgentStatus::Working),
                    activity: Some(flowmux_core::AgentActivity::Running),
                    pid: Some(42),
                    source: Some("flowmux:hook".into()),
                    seq: Some(1),
                    message: None,
                    custom_status: None,
                    session_id: None,
                    session_name: None,
                    messaging_socket: None,
                },
            )
            .await;
        assert_eq!(result, Some((ws_id, Some(AgentStatus::Working))));

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "claude");
        assert_eq!(agent.status, AgentStatus::Working);
    }

    #[tokio::test]
    async fn workspace_activation_acknowledges_blocked_alert_but_keeps_blocked_status() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);
        store.set_active_workspace(None).await;

        store
            .report_agent_status(
                surface,
                AgentStatusReport {
                    name: "codex".into(),
                    status: Some(AgentStatus::Blocked),
                    activity: Some(flowmux_core::AgentActivity::NeedsInput),
                    pid: None,
                    source: Some("flowmux:hook".into()),
                    seq: Some(1),
                    message: Some("approval needed".into()),
                    custom_status: None,
                    session_id: None,
                    session_name: None,
                    messaging_socket: None,
                },
            )
            .await;

        assert_eq!(
            store.workspace_agent_attention_status(ws_id).await,
            Some(AgentStatus::Blocked)
        );
        store.set_active_workspace(Some(ws_id)).await;
        assert_eq!(store.workspace_agent_attention_status(ws_id).await, None);
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Blocked)
        );
    }

    #[tokio::test]
    async fn claude_lifecycle_start_running_blocked_resumed_completed_exit() {
        assert_agent_lifecycle("claude", AgentStatus::Idle).await;
    }

    #[tokio::test]
    async fn codex_lifecycle_unknown_running_blocked_resumed_completed_exit() {
        assert_agent_lifecycle("codex", AgentStatus::Unknown).await;
    }

    #[tokio::test]
    async fn newer_native_start_reopens_an_ended_same_pid_session_epoch() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let pid = std::process::id();
        let start = |seq| AgentStatusReport {
            name: "claude".into(),
            status: Some(AgentStatus::Idle),
            activity: Some(flowmux_core::AgentActivity::Idle),
            pid: Some(pid),
            source: Some("flowmux:hook".into()),
            seq: Some(seq),
            message: None,
            custom_status: Some("Ready".into()),
            session_id: Some("session-1".into()),
            session_name: None,
            messaging_socket: None,
        };

        assert!(store
            .report_agent_status_with_visibility(surface, start(1), true)
            .await
            .is_some());
        assert!(store
            .end_agent_session(surface, "claude", Some(2), Some("session-1"), Some(pid))
            .await
            .is_some());
        assert!(store
            .report_agent_status_with_visibility(surface, start(2), true)
            .await
            .is_none());
        assert!(store.located_agent_presence(surface).await.is_none());

        assert!(store
            .report_agent_status_with_visibility(surface, start(3), true)
            .await
            .is_some());
        let presence = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(presence.status, AgentStatus::Idle);
        assert_eq!(presence.custom_status.as_deref(), Some("Ready"));
        assert_eq!(presence.pid, Some(pid));
        assert_eq!(presence.session_id.as_deref(), Some("session-1"));

        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(pid),
                    Some(2),
                    "session-1",
                    AgentLifecycleEvent::ProgressObserved {
                        status_text: "stale work".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
    }

    #[tokio::test]
    async fn nested_agent_exit_allows_only_the_restored_outer_process_to_reactivate() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let outer_pid = std::process::id();

        store
            .report_agent_status_with_visibility(
                surface,
                AgentStatusReport {
                    name: "claude".into(),
                    status: Some(AgentStatus::Idle),
                    activity: Some(flowmux_core::AgentActivity::Idle),
                    pid: Some(outer_pid),
                    source: Some("flowmux:hook".into()),
                    seq: Some(1),
                    message: None,
                    custom_status: Some("Ready".into()),
                    session_id: Some("outer-session".into()),
                    session_name: None,
                    messaging_socket: None,
                },
                true,
            )
            .await;
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(outer_pid),
                Some(2),
                "outer-session",
                AgentLifecycleEvent::PermissionWaitStarted {
                    message: Some("outer approval".into()),
                    status_text: "Waiting for permission".into(),
                    scope: None,
                },
                true,
            )
            .await;

        // A process poll can observe the nested Codex before its SessionStart
        // hook arrives. Since the still-live outer hook identity also appears
        // in the subtree, process truth must not displace or tombstone it.
        assert!(store
            .reconcile_process_agent_candidates(&[(surface, vec!["codex", "claude"])])
            .await
            .is_empty());
        let before_inner_start = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(before_inner_start.name, "claude");
        assert_eq!(before_inner_start.source.as_deref(), Some("flowmux:hook"));
        assert_eq!(
            before_inner_start.session_id.as_deref(),
            Some("outer-session")
        );

        store
            .report_agent_status_with_visibility(
                surface,
                AgentStatusReport {
                    name: "codex".into(),
                    status: Some(AgentStatus::Unknown),
                    activity: None,
                    pid: None,
                    source: Some("flowmux:hook".into()),
                    seq: Some(3),
                    message: None,
                    custom_status: Some("Ready".into()),
                    session_id: Some("inner-session".into()),
                    session_name: None,
                    messaging_socket: None,
                },
                true,
            )
            .await;

        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(outer_pid),
                    Some(4),
                    "outer-session",
                    AgentLifecycleEvent::TurnStarted {
                        turn_id: None,
                        status_text: "must not replace Codex".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .name,
            "codex"
        );

        assert!(store
            .end_agent_session(surface, "codex", Some(5), Some("inner-session"), None)
            .await
            .is_some());
        assert_eq!(
            store
                .reconcile_process_agent_candidates(&[(surface, vec!["claude"])])
                .await,
            vec![(ws_id, Some(AgentStatus::Idle))]
        );
        let resumed = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(outer_pid),
                Some(6),
                "outer-session",
                AgentLifecycleEvent::TurnStarted {
                    turn_id: None,
                    status_text: "Outer resumed".into(),
                },
                true,
            )
            .await;
        assert_eq!(resumed.workspace, Some(ws_id));
        let presence = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(presence.name, "claude");
        assert_eq!(presence.status, AgentStatus::Working);
        assert_eq!(presence.source.as_deref(), Some("flowmux:hook"));
        assert_eq!(presence.pid, Some(outer_pid));
        assert_eq!(presence.session_id.as_deref(), Some("outer-session"));
    }

    #[tokio::test]
    async fn same_agent_nested_pid_switches_owner_then_reactivates_the_outer_process() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let outer_pid = std::process::id();
        let inner_pid = u32::MAX;
        let start = |session: &str, pid, seq| AgentStatusReport {
            name: "codex".into(),
            status: Some(AgentStatus::Unknown),
            activity: None,
            pid: Some(pid),
            source: Some("flowmux:hook".into()),
            seq: Some(seq),
            message: None,
            custom_status: Some("Ready".into()),
            session_id: Some(session.into()),
            session_name: None,
            messaging_socket: None,
        };

        store
            .report_agent_status_with_visibility(
                surface,
                start("outer-session", outer_pid, 1),
                true,
            )
            .await;
        store
            .report_agent_status_with_visibility(
                surface,
                start("inner-session", inner_pid, 2),
                true,
            )
            .await;
        let inner = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(inner.pid, Some(inner_pid));
        assert_eq!(inner.session_id.as_deref(), Some("inner-session"));

        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(outer_pid),
                    Some(3),
                    "outer-session",
                    AgentLifecycleEvent::TurnStarted {
                        turn_id: Some("outer-turn".into()),
                        status_text: "must not replace inner Codex".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );

        assert!(store
            .end_agent_session(
                surface,
                "codex",
                Some(4),
                Some("inner-session"),
                Some(inner_pid),
            )
            .await
            .is_some());
        assert_eq!(
            store
                .reconcile_process_agent_candidates(&[(surface, vec!["codex"])])
                .await,
            vec![(ws_id, Some(AgentStatus::Idle))]
        );
        let resumed = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(outer_pid),
                Some(5),
                "outer-session",
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("outer-turn".into()),
                    status_text: "Outer resumed".into(),
                },
                true,
            )
            .await;
        assert_eq!(resumed.workspace, Some(ws_id));
        let outer = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(outer.status, AgentStatus::Working);
        assert_eq!(outer.pid, Some(outer_pid));
        assert_eq!(outer.session_id.as_deref(), Some("outer-session"));
        assert_eq!(outer.source.as_deref(), Some("flowmux:hook"));
    }

    #[tokio::test]
    async fn correlated_claude_waits_resolve_only_the_matching_tool() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());

        for (seq, item_id) in [(1, "tool-a"), (2, "tool-b")] {
            let result = store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(seq),
                    "session-1",
                    AgentLifecycleEvent::WaitStarted {
                        item_id: item_id.into(),
                        message: None,
                        status_text: "Waiting for approval".into(),
                    },
                    false,
                )
                .await;
            assert_eq!(result.workspace, Some(ws_id));
        }
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Blocked
        );

        let first = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(3),
                "session-1",
                AgentLifecycleEvent::WaitResolved {
                    item_id: "tool-a".into(),
                },
                false,
            )
            .await;
        assert_eq!(first, AgentLifecycleResult::default());
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Blocked
        );

        let last = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(4),
                "session-1",
                AgentLifecycleEvent::WaitResolved {
                    item_id: "tool-b".into(),
                },
                false,
            )
            .await;
        assert_eq!(last.workspace, Some(ws_id));
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Working
        );
    }

    #[tokio::test]
    async fn non_resuming_session_wait_resolution_rejects_older_progress() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(10),
                "session-1",
                AgentLifecycleEvent::TurnStarted {
                    turn_id: None,
                    status_text: "Starting turn".into(),
                },
                true,
            )
            .await;
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(20),
                "session-1",
                AgentLifecycleEvent::SessionWaitStarted {
                    message: None,
                    status_text: "Waiting for quota".into(),
                    scope: Some("quota".into()),
                },
                true,
            )
            .await;
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(30),
                "session-1",
                AgentLifecycleEvent::SessionWaitResolved {
                    status_text: "Auto-resume disabled".into(),
                    resume: false,
                    scope: Some("quota".into()),
                },
                true,
            )
            .await;
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle)
        );

        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(25),
                    "session-1",
                    AgentLifecycleEvent::ProgressObserved {
                        status_text: "Delayed progress".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle)
        );
    }

    #[tokio::test]
    async fn terminal_boundaries_cannot_overwrite_newer_same_turn_activity() {
        for terminal in [
            AgentLifecycleEvent::TurnStopped {
                message: Some("stale stop".into()),
                status_text: "Completed".into(),
            },
            AgentLifecycleEvent::SessionWaitResolved {
                status_text: "Auto-resume disabled".into(),
                resume: false,
                scope: Some("quota".into()),
            },
        ] {
            let store = StateStore::new_lazy(State::default());
            let ws_id = store
                .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
                .await;
            let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());

            for (seq, event) in [
                (
                    10,
                    AgentLifecycleEvent::TurnStarted {
                        turn_id: None,
                        status_text: "Starting turn".into(),
                    },
                ),
                (
                    30,
                    AgentLifecycleEvent::ProgressObserved {
                        status_text: "Newer work".into(),
                    },
                ),
            ] {
                store
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        "claude",
                        Some(42),
                        Some(seq),
                        "session-1",
                        event,
                        true,
                    )
                    .await;
            }

            assert_eq!(
                store
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        "claude",
                        Some(42),
                        Some(20),
                        "session-1",
                        terminal,
                        true,
                    )
                    .await,
                AgentLifecycleResult::default()
            );
            assert_eq!(
                store.workspace_agent_status(ws_id).await,
                Some(AgentStatus::Working)
            );
        }
    }

    #[tokio::test]
    async fn stale_codex_stop_and_interrupt_cannot_cross_newer_root_activity() {
        for terminal in [
            AgentLifecycleEvent::CodexTurnStopped {
                turn_id: "root-1".into(),
                message: Some("stale stop".into()),
                status_text: "Completed".into(),
                stop_hook_active: false,
            },
            AgentLifecycleEvent::CodexTurnInterrupted {
                turn_id: "root-1".into(),
                status_text: "Interrupted".into(),
            },
        ] {
            let store = StateStore::new_lazy(State::default());
            let ws_id = store
                .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
                .await;
            let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());

            for (seq, event) in [
                (
                    10,
                    AgentLifecycleEvent::TurnStarted {
                        turn_id: Some("root-1".into()),
                        status_text: "Starting turn".into(),
                    },
                ),
                (
                    30,
                    AgentLifecycleEvent::ProgressObserved {
                        status_text: "Newer work".into(),
                    },
                ),
            ] {
                store
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        "codex",
                        Some(42),
                        Some(seq),
                        "session-1",
                        event,
                        true,
                    )
                    .await;
            }

            assert_eq!(
                store
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        "codex",
                        Some(42),
                        Some(20),
                        "session-1",
                        terminal,
                        true,
                    )
                    .await,
                AgentLifecycleResult::default()
            );
            assert_eq!(
                store.workspace_agent_status(ws_id).await,
                Some(AgentStatus::Working)
            );
        }
    }

    #[tokio::test]
    async fn newer_codex_progress_cancels_provisional_grace_settlement() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";
        let turn_id = "root-1";

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(10),
                session_id,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some(turn_id.into()),
                    status_text: "Starting turn".into(),
                },
                true,
            )
            .await;
        let provisional = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(20),
                session_id,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: turn_id.into(),
                    message: Some("provisional".into()),
                    status_text: "Completed".into(),
                    stop_hook_active: false,
                },
                true,
            )
            .await;
        assert!(provisional.settle_codex_after_grace.is_some());

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(30),
                session_id,
                AgentLifecycleEvent::ProgressObserved {
                    status_text: "Stop was blocked".into(),
                },
                true,
            )
            .await;
        assert_eq!(
            store
                .settle_codex_turn_after_grace(
                    surface,
                    Some(42),
                    Some(20),
                    session_id,
                    turn_id,
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working)
        );
    }

    #[tokio::test]
    async fn older_codex_grace_timer_cannot_consume_a_newer_pending_stop() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";
        let turn_id = "root-1";

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(1),
                session_id,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some(turn_id.into()),
                    status_text: "Starting turn".into(),
                },
                true,
            )
            .await;
        for seq in [2, 3] {
            let pending = store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(seq),
                    session_id,
                    AgentLifecycleEvent::CodexTurnStopped {
                        turn_id: turn_id.into(),
                        message: Some(format!("stop-{seq}")),
                        status_text: format!("Completed stop-{seq}"),
                        stop_hook_active: false,
                    },
                    true,
                )
                .await;
            assert!(pending.settle_codex_after_grace.is_some());
        }

        assert_eq!(
            store
                .settle_codex_turn_after_grace(
                    surface,
                    Some(42),
                    Some(2),
                    session_id,
                    turn_id,
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        let settled = store
            .settle_codex_turn_after_grace(surface, Some(42), Some(3), session_id, turn_id, true)
            .await;
        assert_eq!(settled.workspace, Some(ws_id));
        assert!(settled.completed);
        assert_eq!(settled.completion_message.as_deref(), Some("stop-3"));
    }

    #[tokio::test]
    async fn child_progress_cannot_make_a_delayed_parent_stop_stale() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(1),
                session_id,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-1".into()),
                    status_text: "Starting turn".into(),
                },
                true,
            )
            .await;

        // The parent Stop was created first but is delayed in IPC. Child tool
        // progress is both active-child evidence and independent of root order.
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(11),
                session_id,
                AgentLifecycleEvent::CodexChildProgressObserved {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                true,
            )
            .await;
        let parent_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(10),
                session_id,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: "root-1".into(),
                    message: Some("parent result".into()),
                    status_text: "Completed: parent result".into(),
                    stop_hook_active: false,
                },
                true,
            )
            .await;
        assert_eq!(parent_stop.workspace, Some(ws_id));
        assert!(!parent_stop.completed);
        assert!(parent_stop.settle_codex_after_grace.is_none());

        let child_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(12),
                session_id,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                true,
            )
            .await;
        assert_eq!(child_stop.workspace, Some(ws_id));
        assert!(!child_stop.completed);
        let settlement = child_stop
            .settle_codex_after_grace
            .expect("last child should schedule the parent Stop grace");
        assert_eq!(settlement.turn_id, "root-1");
        assert_eq!(settlement.stop_seq, Some(10));
        let settled = store
            .settle_codex_turn_after_grace(
                surface,
                Some(42),
                settlement.stop_seq,
                session_id,
                &settlement.turn_id,
                true,
            )
            .await;
        assert!(settled.completed);
        assert_eq!(settled.completion_message.as_deref(), Some("parent result"));
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle)
        );
    }

    #[tokio::test]
    async fn child_permission_cannot_make_a_delayed_parent_stop_stale() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        for (seq, event) in [
            (
                1,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-1".into()),
                    status_text: "Starting turn".into(),
                },
            ),
            (
                2,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
            ),
            (
                101,
                AgentLifecycleEvent::PermissionWaitStarted {
                    message: Some("child approval".into()),
                    status_text: "Waiting for child permission".into(),
                    scope: Some("child:child-1:child-turn-1".into()),
                },
            ),
        ] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(seq),
                    session_id,
                    event,
                    true,
                )
                .await;
        }

        let parent_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(100),
                session_id,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: "root-1".into(),
                    message: Some("parent result".into()),
                    status_text: "Completed: parent result".into(),
                    stop_hook_active: false,
                },
                true,
            )
            .await;
        assert!(!parent_stop.completed);
        assert!(parent_stop.settle_codex_after_grace.is_none());

        let child_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(102),
                session_id,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                true,
            )
            .await;
        let settlement = child_stop
            .settle_codex_after_grace
            .expect("last child should preserve the delayed parent Stop");
        assert_eq!(settlement.stop_seq, Some(100));
        let settled = store
            .settle_codex_turn_after_grace(
                surface,
                Some(42),
                settlement.stop_seq,
                session_id,
                &settlement.turn_id,
                true,
            )
            .await;
        assert!(settled.completed);
        assert_eq!(settled.completion_message.as_deref(), Some("parent result"));
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle)
        );
    }

    #[tokio::test]
    async fn child_stop_settlement_preserves_a_delayed_new_root_turn() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        for (seq, event) in [
            (
                1,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-1".into()),
                    status_text: "Starting root 1".into(),
                },
            ),
            (
                2,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
            ),
            (
                3,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: "root-1".into(),
                    message: Some("root 1 done".into()),
                    status_text: "Completed root 1".into(),
                    stop_hook_active: false,
                },
            ),
        ] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(seq),
                    session_id,
                    event,
                    true,
                )
                .await;
        }

        let child_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(5),
                session_id,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                true,
            )
            .await;
        assert!(!child_stop.completed);
        let settlement = child_stop
            .settle_codex_after_grace
            .expect("last child should defer settlement");
        assert_eq!(settlement.stop_seq, Some(3));

        // Root 2 started before the child Stop but its IPC arrived afterward.
        // The child sequence must not become the root boundary.
        let root_two = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(4),
                session_id,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-2".into()),
                    status_text: "Starting root 2".into(),
                },
                true,
            )
            .await;
        assert_eq!(root_two.workspace, Some(ws_id));
        assert_eq!(
            store
                .settle_codex_turn_after_grace(
                    surface,
                    Some(42),
                    settlement.stop_seq,
                    session_id,
                    &settlement.turn_id,
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working)
        );
        let runtime = store.agent_lifecycle.lock().await;
        let ledger = runtime
            .codex_turns
            .get(&(surface, session_id.to_string()))
            .unwrap();
        assert_eq!(ledger.current_parent_turn.as_deref(), Some("root-2"));
    }

    #[tokio::test]
    async fn newer_root_progress_cancels_pending_stop_while_children_remain() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        for (seq, event) in [
            (
                1,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-1".into()),
                    status_text: "Starting root 1".into(),
                },
            ),
            (
                2,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
            ),
            (
                3,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: "root-1".into(),
                    message: Some("stale result".into()),
                    status_text: "Completed root 1".into(),
                    stop_hook_active: false,
                },
            ),
        ] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(seq),
                    session_id,
                    event,
                    true,
                )
                .await;
        }

        let progress = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(4),
                session_id,
                AgentLifecycleEvent::CodexRootProgressObserved {
                    turn_id: "root-2".into(),
                    status_text: "Working".into(),
                },
                true,
            )
            .await;
        assert_eq!(progress.workspace, Some(ws_id));

        let child_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(5),
                session_id,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                true,
            )
            .await;
        assert!(!child_stop.completed);
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working)
        );
        let runtime = store.agent_lifecycle.lock().await;
        let ledger = runtime
            .codex_turns
            .get(&(surface, session_id.to_string()))
            .unwrap();
        assert!(ledger.pending_parent_stop.is_none());
        assert_eq!(ledger.current_parent_turn.as_deref(), Some("root-2"));
    }

    #[tokio::test]
    async fn session_wait_scopes_use_last_event_sequence_independently() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(1),
                session_id,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: None,
                    status_text: "Starting turn".into(),
                },
                true,
            )
            .await;
        let standalone_resolution = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(30),
                session_id,
                AgentLifecycleEvent::SessionWaitResolved {
                    status_text: "Quota resumed".into(),
                    resume: true,
                    scope: Some("quota".into()),
                },
                true,
            )
            .await;
        assert_eq!(standalone_resolution.workspace, Some(ws_id));
        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(20),
                    session_id,
                    AgentLifecycleEvent::SessionWaitStarted {
                        message: None,
                        status_text: "Delayed quota failure".into(),
                        scope: Some("quota".into()),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working)
        );

        for (seq, scope) in [(40, "ask_user"), (50, "quota")] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(seq),
                    session_id,
                    AgentLifecycleEvent::SessionWaitStarted {
                        message: None,
                        status_text: format!("Waiting for {scope}"),
                        scope: Some(scope.into()),
                    },
                    true,
                )
                .await;
        }
        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(60),
                    session_id,
                    AgentLifecycleEvent::SessionWaitResolved {
                        status_text: "Quota resumed".into(),
                        resume: true,
                        scope: Some("quota".into()),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        {
            let runtime = store.agent_lifecycle.lock().await;
            let key = (surface, "claude".to_string(), session_id.to_string());
            let scopes = runtime.session_waits.get(&key).unwrap();
            assert_eq!(scopes.len(), 1);
            assert!(scopes.contains("ask_user"));
        }
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Blocked)
        );

        let resolved = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(70),
                session_id,
                AgentLifecycleEvent::SessionWaitResolved {
                    status_text: "Working".into(),
                    resume: true,
                    scope: Some("ask_user".into()),
                },
                true,
            )
            .await;
        assert_eq!(resolved.workspace, Some(ws_id));
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working)
        );
    }

    #[tokio::test]
    async fn standalone_session_wait_resolutions_apply_fired_and_disabled_state() {
        for (index, resume, expected_before, expected_after) in [
            (0, true, AgentStatus::Idle, AgentStatus::Working),
            (1, false, AgentStatus::Working, AgentStatus::Idle),
        ] {
            let store = StateStore::new_lazy(State::default());
            let ws_id = store
                .create_workspace(
                    Some(format!("demo-{index}")),
                    std::path::PathBuf::from(format!("/tmp/demo-{index}")),
                )
                .await;
            let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
            let session_id = format!("session-{index}");

            store
                .report_agent_status_with_visibility(
                    surface,
                    AgentStatusReport {
                        name: "claude".into(),
                        status: Some(AgentStatus::Idle),
                        activity: Some(flowmux_core::AgentActivity::Idle),
                        pid: Some(42),
                        source: Some("flowmux:hook".into()),
                        seq: Some(1),
                        message: None,
                        custom_status: Some("Ready".into()),
                        session_id: Some(session_id.clone()),
                        session_name: None,
                        messaging_socket: None,
                    },
                    true,
                )
                .await;
            if !resume {
                store
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        "claude",
                        Some(42),
                        Some(2),
                        &session_id,
                        AgentLifecycleEvent::TurnStarted {
                            turn_id: None,
                            status_text: "Starting turn".into(),
                        },
                        true,
                    )
                    .await;
            }
            assert_eq!(
                store.workspace_agent_status(ws_id).await,
                Some(expected_before)
            );

            let resolution = store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(3),
                    &session_id,
                    AgentLifecycleEvent::SessionWaitResolved {
                        status_text: if resume {
                            "Working".into()
                        } else {
                            "Auto-resume disabled".into()
                        },
                        resume,
                        scope: Some("quota".into()),
                    },
                    true,
                )
                .await;
            assert_eq!(resolution.workspace, Some(ws_id));
            assert_eq!(
                store.workspace_agent_status(ws_id).await,
                Some(expected_after)
            );

            if !resume {
                assert_eq!(
                    store
                        .report_agent_lifecycle_with_visibility(
                            surface,
                            "claude",
                            Some(42),
                            Some(2),
                            &session_id,
                            AgentLifecycleEvent::ProgressObserved {
                                status_text: "Delayed progress".into(),
                            },
                            true,
                        )
                        .await,
                    AgentLifecycleResult::default()
                );
                assert_eq!(
                    store.workspace_agent_status(ws_id).await,
                    Some(AgentStatus::Idle)
                );
            }
        }
    }

    #[tokio::test]
    async fn claude_batch_completion_clears_only_the_permission_wait() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        for (seq, event) in [
            (
                1,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: None,
                    status_text: "Starting turn".into(),
                },
            ),
            (
                2,
                AgentLifecycleEvent::PermissionWaitStarted {
                    message: Some("Approve tool?".into()),
                    status_text: "Waiting for permission".into(),
                    scope: None,
                },
            ),
            (
                3,
                AgentLifecycleEvent::SessionWaitStarted {
                    message: Some("Answer question".into()),
                    status_text: "Waiting for input".into(),
                    scope: Some("ask_user".into()),
                },
            ),
        ] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(seq),
                    session_id,
                    event,
                    true,
                )
                .await;
        }

        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(4),
                    session_id,
                    AgentLifecycleEvent::ProgressObserved {
                        status_text: "Tool progress".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Blocked)
        );

        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "claude",
                    Some(42),
                    Some(5),
                    session_id,
                    AgentLifecycleEvent::ToolBatchFinished {
                        status_text: "Tool batch finished".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Blocked)
        );
        {
            let runtime = store.agent_lifecycle.lock().await;
            let key = (surface, "claude".to_string(), session_id.to_string());
            assert!(!runtime.permission_waits.contains_key(&key));
            assert!(runtime
                .session_waits
                .get(&key)
                .is_some_and(|scopes| scopes.contains("ask_user")));
        }

        let resolved = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "claude",
                Some(42),
                Some(6),
                session_id,
                AgentLifecycleEvent::SessionWaitResolved {
                    status_text: "Working".into(),
                    resume: true,
                    scope: Some("ask_user".into()),
                },
                true,
            )
            .await;
        assert_eq!(resolved.workspace, Some(ws_id));
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working)
        );
    }

    #[tokio::test]
    async fn codex_parent_stop_waits_for_matching_active_subagent() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(1),
                "session-1",
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-1".into()),
                    status_text: "Starting turn".into(),
                },
                false,
            )
            .await;
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(2),
                "session-1",
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                false,
            )
            .await;
        let parent_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(3),
                "session-1",
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: "root-1".into(),
                    message: Some("parent result".into()),
                    status_text: "Completed: parent result".into(),
                    stop_hook_active: false,
                },
                false,
            )
            .await;
        assert_eq!(parent_stop.workspace, Some(ws_id));
        assert!(!parent_stop.completed);
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Working
        );

        // Child tool progress can arrive after the parent Stop. It must not
        // cancel the deferred parent completion while that child is known live.
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(4),
                "session-1",
                AgentLifecycleEvent::ProgressObserved {
                    status_text: "Child still working".into(),
                },
                false,
            )
            .await;

        let stale_child_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(5),
                "session-1",
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "old-child-turn".into(),
                },
                false,
            )
            .await;
        assert_eq!(stale_child_stop, AgentLifecycleResult::default());

        let child_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(6),
                "session-1",
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                false,
            )
            .await;
        assert_eq!(child_stop.workspace, Some(ws_id));
        assert!(!child_stop.completed);
        let settlement = child_stop
            .settle_codex_after_grace
            .expect("last child should schedule parent settlement");
        let settled = store
            .settle_codex_turn_after_grace(
                surface,
                Some(42),
                settlement.stop_seq,
                "session-1",
                &settlement.turn_id,
                false,
            )
            .await;
        assert!(settled.completed);
        assert_eq!(settled.completion_message.as_deref(), Some("parent result"));
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Idle
        );

        let duplicate = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(7),
                "session-1",
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: "root-1".into(),
                    message: Some("parent result".into()),
                    status_text: "Completed: parent result".into(),
                    stop_hook_active: false,
                },
                false,
            )
            .await;
        assert_eq!(duplicate, AgentLifecycleResult::default());
    }

    #[tokio::test]
    async fn codex_active_stop_retry_resettles_without_duplicate_completion() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";
        let turn_id = "root-1";

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(1),
                session_id,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some(turn_id.into()),
                    status_text: "Starting turn".into(),
                },
                true,
            )
            .await;
        let first_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(2),
                session_id,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: turn_id.into(),
                    message: Some("first result".into()),
                    status_text: "Completed: first result".into(),
                    stop_hook_active: false,
                },
                true,
            )
            .await;
        assert!(first_stop.settle_codex_after_grace.is_some());
        let first_settlement = store
            .settle_codex_turn_after_grace(surface, Some(42), Some(2), session_id, turn_id, true)
            .await;
        assert!(first_settlement.completed);
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle)
        );

        let progress = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(3),
                session_id,
                AgentLifecycleEvent::ProgressObserved {
                    status_text: "Stop hook continued the turn".into(),
                },
                true,
            )
            .await;
        assert_eq!(progress.workspace, Some(ws_id));
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Working)
        );

        let retry = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(4),
                session_id,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: turn_id.into(),
                    message: Some("final result".into()),
                    status_text: "Completed: final result".into(),
                    stop_hook_active: true,
                },
                true,
            )
            .await;
        assert!(retry.settle_codex_after_grace.is_some());
        let retry_settlement = store
            .settle_codex_turn_after_grace(surface, Some(42), Some(4), session_id, turn_id, true)
            .await;
        assert_eq!(retry_settlement.workspace, Some(ws_id));
        assert!(!retry_settlement.completed);
        assert_eq!(retry_settlement.completion_message, None);
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle)
        );
    }

    #[tokio::test]
    async fn codex_stops_clear_only_their_matching_permission_scopes() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        for (seq, event) in [
            (
                1,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-1".into()),
                    status_text: "Starting turn".into(),
                },
            ),
            (
                2,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
            ),
            (
                3,
                AgentLifecycleEvent::PermissionWaitStarted {
                    message: Some("Approve root tool?".into()),
                    status_text: "Waiting for permission".into(),
                    scope: Some("root:root-1".into()),
                },
            ),
            (
                4,
                AgentLifecycleEvent::PermissionWaitStarted {
                    message: Some("Approve child tool?".into()),
                    status_text: "Waiting for permission".into(),
                    scope: Some("child:child-1:child-turn-1".into()),
                },
            ),
        ] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(seq),
                    session_id,
                    event,
                    true,
                )
                .await;
        }

        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(5),
                    session_id,
                    AgentLifecycleEvent::CodexTurnStopped {
                        turn_id: "root-1".into(),
                        message: Some("done".into()),
                        status_text: "Completed: done".into(),
                        stop_hook_active: false,
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        {
            let runtime = store.agent_lifecycle.lock().await;
            let key = (surface, "codex".to_string(), session_id.to_string());
            let scopes = runtime.permission_waits.get(&key).unwrap();
            assert_eq!(scopes.len(), 1);
            assert!(scopes.contains("child:child-1:child-turn-1"));
        }
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Blocked)
        );

        let child_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(6),
                session_id,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                true,
            )
            .await;
        assert_eq!(child_stop.workspace, Some(ws_id));
        assert!(!child_stop.completed);
        let settlement = child_stop
            .settle_codex_after_grace
            .expect("last child should schedule parent settlement");
        let settled = store
            .settle_codex_turn_after_grace(
                surface,
                Some(42),
                settlement.stop_seq,
                session_id,
                &settlement.turn_id,
                true,
            )
            .await;
        assert!(settled.completed);
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle)
        );
        let runtime = store.agent_lifecycle.lock().await;
        let key = (surface, "codex".to_string(), session_id.to_string());
        assert!(!runtime.permission_waits.contains_key(&key));
    }

    #[tokio::test]
    async fn codex_stops_tombstone_their_permission_scope_sequences() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        for (seq, event) in [
            (
                1,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-1".into()),
                    status_text: "Starting turn".into(),
                },
            ),
            (
                2,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
            ),
            (
                3,
                AgentLifecycleEvent::PermissionWaitStarted {
                    message: None,
                    status_text: "Waiting for child permission".into(),
                    scope: Some("child:child-1:child-turn-1".into()),
                },
            ),
            (
                4,
                AgentLifecycleEvent::PermissionWaitStarted {
                    message: None,
                    status_text: "Waiting for root permission".into(),
                    scope: Some("root:root-1".into()),
                },
            ),
        ] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(seq),
                    session_id,
                    event,
                    true,
                )
                .await;
        }

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(6),
                session_id,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                true,
            )
            .await;
        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(5),
                    session_id,
                    AgentLifecycleEvent::PermissionWaitStarted {
                        message: None,
                        status_text: "Delayed child permission".into(),
                        scope: Some("child:child-1:child-turn-1".into()),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );

        let root_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(8),
                session_id,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: "root-1".into(),
                    message: Some("done".into()),
                    status_text: "Completed: done".into(),
                    stop_hook_active: false,
                },
                true,
            )
            .await;
        assert!(root_stop.settle_codex_after_grace.is_some());
        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(7),
                    session_id,
                    AgentLifecycleEvent::PermissionWaitStarted {
                        message: None,
                        status_text: "Delayed root permission".into(),
                        scope: Some("root:root-1".into()),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );

        let runtime = store.agent_lifecycle.lock().await;
        let key = (surface, "codex".to_string(), session_id.to_string());
        assert!(!runtime.permission_waits.contains_key(&key));
    }

    #[tokio::test]
    async fn older_codex_child_start_cannot_revive_a_newer_stop() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(30),
                    session_id,
                    AgentLifecycleEvent::CodexSubagentStopped {
                        agent_id: "child-1".into(),
                        turn_id: "child-turn-1".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(20),
                    session_id,
                    AgentLifecycleEvent::CodexSubagentStarted {
                        agent_id: "child-1".into(),
                        turn_id: "child-turn-1".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        {
            let runtime = store.agent_lifecycle.lock().await;
            let ledger = runtime
                .codex_turns
                .get(&(surface, session_id.to_string()))
                .unwrap();
            assert!(ledger.active_children.is_empty());
        }
        assert!(store.located_agent_presence(surface).await.is_none());
    }

    #[tokio::test]
    async fn older_cross_turn_codex_child_events_cannot_replace_the_current_turn() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "session-1";

        let current_start = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(30),
                session_id,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "new-turn".into(),
                },
                true,
            )
            .await;
        assert_eq!(current_start.workspace, Some(ws_id));
        for (seq, event) in [
            (
                20,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "old-turn".into(),
                },
            ),
            (
                25,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "old-turn".into(),
                },
            ),
        ] {
            assert_eq!(
                store
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        "codex",
                        Some(42),
                        Some(seq),
                        session_id,
                        event,
                        true,
                    )
                    .await,
                AgentLifecycleResult::default()
            );
        }
        {
            let runtime = store.agent_lifecycle.lock().await;
            let ledger = runtime
                .codex_turns
                .get(&(surface, session_id.to_string()))
                .unwrap();
            assert_eq!(
                ledger.active_children.get("child-1").map(String::as_str),
                Some("new-turn")
            );
        }

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(40),
                session_id,
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "new-turn".into(),
                },
                true,
            )
            .await;
        let runtime = store.agent_lifecycle.lock().await;
        let ledger = runtime
            .codex_turns
            .get(&(surface, session_id.to_string()))
            .unwrap();
        assert!(ledger.active_children.is_empty());
    }

    #[tokio::test]
    async fn codex_new_root_turn_invalidates_pending_parent_stop() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());

        for (seq, lifecycle) in [
            (
                1,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-1".into()),
                    status_text: "Starting turn".into(),
                },
            ),
            (
                2,
                AgentLifecycleEvent::CodexSubagentStarted {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
            ),
            (
                3,
                AgentLifecycleEvent::CodexTurnStopped {
                    turn_id: "root-1".into(),
                    message: Some("obsolete".into()),
                    status_text: "Completed: obsolete".into(),
                    stop_hook_active: false,
                },
            ),
            (
                4,
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-2".into()),
                    status_text: "Starting turn".into(),
                },
            ),
        ] {
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    Some(42),
                    Some(seq),
                    "session-1",
                    lifecycle,
                    false,
                )
                .await;
        }
        let child_stop = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(5),
                "session-1",
                AgentLifecycleEvent::CodexSubagentStopped {
                    agent_id: "child-1".into(),
                    turn_id: "child-turn-1".into(),
                },
                false,
            )
            .await;
        assert!(!child_stop.completed);
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Working
        );
    }

    #[tokio::test]
    async fn opencode_lifecycle_unknown_running_blocked_resumed_completed_exit() {
        assert_agent_lifecycle("opencode", AgentStatus::Unknown).await;
    }

    #[tokio::test]
    async fn report_agent_status_keeps_hook_identity_over_stale_surface_title() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);
        let surface = first_pane_active_surface(&ws);
        assert_eq!(
            store.rename_surface(pane, surface, "Claude".into()).await,
            Some(ws_id)
        );

        let result = store
            .report_agent_status(
                surface,
                AgentStatusReport {
                    name: "codex".into(),
                    status: Some(AgentStatus::Idle),
                    activity: Some(flowmux_core::AgentActivity::Idle),
                    pid: None,
                    source: Some("flowmux:hook".into()),
                    seq: Some(1),
                    message: None,
                    custom_status: None,
                    session_id: Some("ses-codex".into()),
                    session_name: None,
                    messaging_socket: None,
                },
            )
            .await;
        assert_eq!(result, Some((ws_id, Some(AgentStatus::Idle))));

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "codex");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.source.as_deref(), Some("flowmux:hook"));
    }

    #[tokio::test]
    async fn report_agent_screen_signals_can_create_fallback_presence() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        let result = store
            .report_agent_screen_signals(surface, None, Some("Codex Action Required"))
            .await;
        assert_eq!(result, Some((ws_id, Some(AgentStatus::Blocked))));

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "codex");
        assert_eq!(agent.status, AgentStatus::Blocked);
        assert_eq!(agent.source.as_deref(), Some("flowmux:screen"));
    }

    #[tokio::test]
    async fn claude_usage_limit_screen_replaces_stale_working_hook_status() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        let mut report = AgentStatusReport::from_activity(
            "claude",
            Some(flowmux_core::AgentActivity::Running),
            Some(42),
        );
        report.source = Some("flowmux:hook".into());
        report.seq = Some(1);
        report.custom_status = Some("Using Bash".into());
        store.report_agent_status(surface, report).await;

        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some(
                        "⎿ You've hit your session limit · resets 10:20pm\n\
                         ❯\u{a0}\n\
                         ⚠ Usage limit reached · continuing automatically at 10:20pm",
                    ),
                    Some("flutter-tizen"),
                )
                .await,
            Some((ws_id, Some(AgentStatus::Blocked)))
        );

        let agent = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(agent.status, AgentStatus::Blocked);
        assert_eq!(agent.source.as_deref(), Some("flowmux:hook"));
        assert_eq!(
            agent.status_text(),
            Some("Usage limit reached · continuing automatically at 10:20pm")
        );

        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("$ printf 'Usage limit reached'"),
                    Some("flutter-tizen"),
                )
                .await,
            None
        );
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Blocked
        );
    }

    #[tokio::test]
    async fn reconcile_process_agents_creates_then_drops_proc_presence() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        // Agent process appears -> idle, process-owned presence created.
        let changed = store
            .reconcile_process_agents(&[(surface, Some("codex"))])
            .await;
        assert_eq!(changed, vec![(ws_id, Some(AgentStatus::Idle))]);
        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "codex");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.source.as_deref(), Some("flowmux:proc"));

        // Idempotent: same detection reports no change.
        assert!(store
            .reconcile_process_agents(&[(surface, Some("codex"))])
            .await
            .is_empty());

        // Process exits -> presence dropped.
        let changed = store.reconcile_process_agents(&[(surface, None)]).await;
        assert_eq!(changed, vec![(ws_id, None)]);
        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        assert!(tree[0].panes[0].tabs[0].agent.is_none());
    }

    #[tokio::test]
    async fn reconcile_process_candidates_preserves_nested_hook_identity() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                None,
                Some(6),
                "codex-session",
                AgentLifecycleEvent::TurnStarted {
                    turn_id: Some("root-turn".into()),
                    status_text: "Starting turn".into(),
                },
                true,
            )
            .await;
        store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                None,
                Some(7),
                "codex-session",
                AgentLifecycleEvent::PermissionWaitStarted {
                    message: Some("approval required".into()),
                    status_text: "Waiting for permission".into(),
                    scope: Some("root:root-turn".into()),
                },
                true,
            )
            .await;

        // Claude is the outer process and Codex is nested beneath it. The
        // native hook identity must survive even if the candidate order is not
        // trusted here; membership in the subtree is the relevant proof.
        assert!(store
            .reconcile_process_agent_candidates(&[(surface, vec!["claude", "codex"])])
            .await
            .is_empty());
        let agent = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(agent.name, "codex");
        assert_eq!(agent.status, AgentStatus::Blocked);
        assert_eq!(agent.source.as_deref(), Some("flowmux:hook"));
        assert_eq!(agent.session_id.as_deref(), Some("codex-session"));
        assert_eq!(agent.message.as_deref(), Some("approval required"));

        // The no-op identity reconciliation must preserve the lifecycle
        // ledger too: unrelated progress cannot clear the outstanding wait.
        assert_eq!(
            store
                .report_agent_lifecycle_with_visibility(
                    surface,
                    "codex",
                    None,
                    Some(8),
                    "codex-session",
                    AgentLifecycleEvent::ProgressObserved {
                        status_text: "Tool progress".into(),
                    },
                    true,
                )
                .await,
            AgentLifecycleResult::default()
        );
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Blocked)
        );
    }

    #[tokio::test]
    async fn stale_process_snapshot_cannot_displace_a_new_native_session() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        store
            .reconcile_process_agent_candidates(&[(surface, vec!["claude"])])
            .await;
        let observed = store
            .agent_process_reconciliation_snapshot(&[surface])
            .await;

        let started = store
            .report_agent_status_with_visibility(
                surface,
                AgentStatusReport {
                    name: "codex".into(),
                    status: Some(AgentStatus::Unknown),
                    activity: None,
                    pid: Some(42),
                    source: Some("flowmux:hook".into()),
                    seq: Some(1),
                    message: None,
                    custom_status: Some("Ready".into()),
                    session_id: Some("codex-session".into()),
                    session_name: None,
                    messaging_socket: None,
                },
                false,
            )
            .await;
        assert_eq!(started, Some((ws_id, Some(AgentStatus::Unknown))));

        // This was the true process tree before Codex started. Applying it
        // afterward would replace Codex with proc-owned Claude and tombstone
        // the just-created Codex epoch.
        assert!(store
            .reconcile_process_agent_candidates_if_unchanged(
                &[(surface, vec!["claude"])],
                &observed,
            )
            .await
            .is_empty());
        let presence = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(presence.name, "codex");
        assert_eq!(presence.source.as_deref(), Some("flowmux:hook"));
        assert_eq!(presence.session_id.as_deref(), Some("codex-session"));

        let progress = store
            .report_agent_lifecycle_with_visibility(
                surface,
                "codex",
                Some(42),
                Some(2),
                "codex-session",
                AgentLifecycleEvent::CodexRootProgressObserved {
                    turn_id: "root-turn".into(),
                    status_text: "Working".into(),
                },
                false,
            )
            .await;
        assert_eq!(progress.workspace, Some(ws_id));
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Working
        );
    }

    #[tokio::test]
    async fn reconcile_process_candidates_uses_deepest_without_hook_identity() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        let changed = store
            .reconcile_process_agent_candidates(&[(surface, vec!["codex", "claude"])])
            .await;
        assert_eq!(changed, vec![(ws_id, Some(AgentStatus::Idle))]);
        let agent = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(agent.name, "codex");
        assert_eq!(agent.source.as_deref(), Some("flowmux:proc"));
    }

    #[tokio::test]
    async fn codex_process_presence_survives_idle_after_a_working_turn() {
        // End-to-end for the reported Codex bug: a process-owned presence must
        // outlive a working->idle screen transition. Previously the idle scan
        // (Codex's title is `<spinner> <cwd>`, never "codex", and its composer
        // has no recognizable idle line) cleared the presence entirely.
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        store
            .reconcile_process_agents(&[(surface, Some("codex"))])
            .await;

        // Codex works: its spinner title raises status to Working.
        store
            .report_agent_screen_signals(surface, None, Some("\u{280b} scratchpad"))
            .await;
        let agent = {
            let state = store.snapshot().await;
            let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
            tree[0].panes[0].tabs[0].agent.clone().unwrap()
        };
        assert_eq!(agent.status, AgentStatus::Working);
        assert_eq!(agent.source.as_deref(), Some("flowmux:proc"));

        // Turn ends: idle codex emits no recognizable status signal.
        store
            .report_agent_screen_signals(surface, Some("~/work $"), Some("scratchpad"))
            .await;
        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        // Still present, settled to Idle, still process-owned — NOT cleared.
        assert_eq!(agent.name, "codex");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.source.as_deref(), Some("flowmux:proc"));
    }

    #[tokio::test]
    async fn reconcile_process_agents_drops_pidless_hook_presence() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        store
            .report_agent_status(
                surface,
                AgentStatusReport {
                    name: "opencode".into(),
                    status: Some(AgentStatus::Working),
                    activity: None,
                    pid: None,
                    source: Some("flowmux:hook".into()),
                    seq: None,
                    message: None,
                    custom_status: None,
                    session_id: None,
                    session_name: None,
                    messaging_socket: None,
                },
            )
            .await;

        assert_eq!(
            store.reconcile_process_agents(&[(surface, None)]).await,
            vec![(ws_id, None)]
        );
        assert!(store.located_agent_presence(surface).await.is_none());
    }

    #[tokio::test]
    async fn reconcile_process_agents_keeps_screen_presence_for_out_of_tree_agent() {
        // Regression: an agent inside a container / ssh session is invisible to
        // the process sweep, so the sweep kept deleting the presence the screen
        // scan had just created and the Agent Bar never showed it.
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("❯ Welcome to Claude Code"), None)
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );

        assert_eq!(
            store.reconcile_process_agents(&[(surface, None)]).await,
            vec![]
        );
        let agent = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(agent.name, "claude");
        assert_eq!(agent.source.as_deref(), Some("flowmux:screen"));
    }

    #[tokio::test]
    async fn report_agent_screen_signals_restores_idle_presence_from_agent_name() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        let result = store
            .report_agent_screen_signals(surface, Some("Codex\npress / for commands"), None)
            .await;
        assert_eq!(result, Some((ws_id, Some(AgentStatus::Idle))));

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "codex");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.source.as_deref(), Some("flowmux:screen"));
    }

    #[tokio::test]
    async fn report_agent_screen_signals_restores_idle_presence_from_blank_screen_title() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        let result = store
            .report_agent_screen_signals(surface, Some("  \n\n"), Some("Claude"))
            .await;
        assert_eq!(result, Some((ws_id, Some(AgentStatus::Idle))));

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "claude");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.source.as_deref(), Some("flowmux:screen"));
    }

    #[tokio::test]
    async fn repeated_agent_screen_signal_does_not_publish_an_unchanged_item() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("Codex\n• Working (0s • esc to interrupt)"),
                    Some("Codex"),
                )
                .await,
            Some((ws_id, Some(AgentStatus::Working)))
        );
        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("Codex\n• Working (0s • esc to interrupt)"),
                    Some("Codex"),
                )
                .await,
            None,
            "an unchanged screen-derived item must not trigger another UI rebuild"
        );

        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("Codex\n• Working (1s • esc to interrupt)"),
                    Some("Codex"),
                )
                .await,
            Some((ws_id, Some(AgentStatus::Working))),
            "changed progress text must trigger an event without a status transition"
        );
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status_text(),
            Some("Working (1s • esc to interrupt)")
        );
    }

    #[tokio::test]
    async fn report_agent_screen_signals_ignores_codegraph_installer_agent_list() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);
        let codegraph_screen = "\
Which agents should CodeGraph configure?
Claude Code (detected), Codex CLI (detected), opencode (detected)
Do you want to continue?";

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some(codegraph_screen), None)
                .await,
            None
        );

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        assert!(tree[0].panes[0].tabs[0].agent.is_none());
    }

    #[tokio::test]
    async fn report_agent_screen_signals_clears_stale_screen_presence_without_agent_name() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);
        let codegraph_screen = "\
Which agents should CodeGraph configure?
Claude Code (detected), Codex CLI (detected), opencode (detected)
Do you want to continue?";

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("OpenCode Action Required"), None)
                .await,
            Some((ws_id, Some(AgentStatus::Blocked)))
        );

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some(codegraph_screen), None)
                .await,
            Some((ws_id, None))
        );

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        assert!(tree[0].panes[0].tabs[0].agent.is_none());
    }

    #[tokio::test]
    async fn report_agent_screen_signals_clears_screen_presence_when_signal_disappears() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("Codex\npress / for commands"), None)
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );
        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("$ echo shell ready"), Some("demo"))
                .await,
            Some((ws_id, None))
        );

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        assert!(tree[0].panes[0].tabs[0].agent.is_none());
    }

    #[tokio::test]
    async fn screen_clear_restores_hook_status_after_screen_working_signal() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "codex".into(),
                        status: Some(AgentStatus::Idle),
                        activity: Some(flowmux_core::AgentActivity::Idle),
                        pid: Some(42),
                        source: Some("flowmux:hook".into()),
                        seq: Some(1),
                        message: None,
                        custom_status: None,
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );
        assert_eq!(
            store
                .report_agent_screen_signals(surface, None, Some("Codex ⠋ working"))
                .await,
            Some((ws_id, Some(AgentStatus::Working)))
        );
        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("$ echo shell ready"), Some("demo"))
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "codex");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.source.as_deref(), Some("flowmux:hook"));
        assert_eq!(
            store.live_agent_presences().await,
            vec![(ws_id, surface, 42)]
        );
    }

    #[tokio::test]
    async fn claude_lifecycle_tracks_stop_interrupt_restart_and_session_end() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let session_id = "claude-session";
        let report =
            |activity: flowmux_core::AgentActivity, seq, custom_status: &str| AgentStatusReport {
                name: "claude".into(),
                status: Some(activity.status()),
                activity: Some(activity),
                pid: Some(std::process::id()),
                source: Some("flowmux:hook".into()),
                seq: Some(seq),
                message: None,
                custom_status: Some(custom_status.into()),
                session_id: Some(session_id.into()),
                session_name: None,
                messaging_socket: None,
            };

        store
            .report_agent_status(
                surface,
                report(flowmux_core::AgentActivity::Idle, 1, "Ready"),
            )
            .await;
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle),
            "SessionStart must register Claude as idle"
        );

        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    report(flowmux_core::AgentActivity::Running, 2, "Starting turn",),
                )
                .await,
            Some((ws_id, Some(AgentStatus::Working))),
            "PromptSubmit must mark Claude working"
        );
        assert!(
            store
                .reconcile_process_agents(&[(surface, None)])
                .await
                .is_empty(),
            "process polling must not remove a PID-backed hook presence"
        );

        store
            .report_agent_status(
                surface,
                report(flowmux_core::AgentActivity::Idle, 3, "Completed"),
            )
            .await;
        assert_eq!(
            store.workspace_agent_status(ws_id).await,
            Some(AgentStatus::Idle),
            "Stop must settle Claude to idle"
        );

        let working = report(flowmux_core::AgentActivity::Running, 4, "Starting turn");
        store.report_agent_status(surface, working).await;
        let interrupted_screen = "⎿ Interrupted · What should Claude do instead?\n❯";
        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some(interrupted_screen), Some("demo"))
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );
        let interrupted = store
            .located_agent_presence(surface)
            .await
            .unwrap()
            .presence;
        assert_eq!(interrupted.status, AgentStatus::Idle);
        assert_eq!(interrupted.custom_status.as_deref(), Some("Interrupted"));
        assert_eq!(interrupted.source.as_deref(), Some("flowmux:hook"));

        store
            .report_agent_status(
                surface,
                report(flowmux_core::AgentActivity::Running, 5, "Starting turn"),
            )
            .await;
        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("⎿ Interrupted · What should Claude do instead?\n❯ do the next task\n❯"),
                    Some("demo"),
                )
                .await,
            None,
            "an older interruption must not stop a newly submitted turn"
        );
        assert_eq!(
            store
                .located_agent_presence(surface)
                .await
                .unwrap()
                .presence
                .status,
            AgentStatus::Working
        );

        assert!(
            store
                .end_agent_session(
                    surface,
                    "claude",
                    Some(6),
                    Some(session_id),
                    Some(std::process::id()),
                )
                .await
                .is_some(),
            "SessionEnd must remove the current Claude session"
        );
        assert!(store.located_agent_presence(surface).await.is_none());
    }

    #[tokio::test]
    async fn auto_approve_banner_does_not_block_hook_presence() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "cline".into(),
                        status: Some(AgentStatus::Idle),
                        activity: Some(flowmux_core::AgentActivity::Idle),
                        pid: Some(42),
                        source: Some("flowmux:hook".into()),
                        seq: Some(1),
                        message: None,
                        custom_status: None,
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );
        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("Ask anything...\nGPT-5.4  Plan  Act\nAuto-approve all enabled"),
                    Some("> hello"),
                )
                .await,
            None
        );

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "cline");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.source.as_deref(), Some("flowmux:hook"));
    }

    #[tokio::test]
    async fn duplicate_session_start_does_not_replace_live_hook_pid() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);
        let live_pid = std::process::id();
        let other_pid = live_pid.saturating_add(100_000);

        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "opencode".into(),
                        status: Some(AgentStatus::Idle),
                        activity: Some(flowmux_core::AgentActivity::Idle),
                        pid: Some(live_pid),
                        source: Some("flowmux:hook".into()),
                        seq: Some(1),
                        message: None,
                        custom_status: None,
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );
        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "opencode".into(),
                        status: Some(AgentStatus::Idle),
                        activity: Some(flowmux_core::AgentActivity::Idle),
                        pid: Some(other_pid),
                        source: Some("flowmux:hook".into()),
                        seq: Some(2),
                        message: None,
                        custom_status: None,
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );

        assert_eq!(
            store.live_agent_presences().await,
            vec![(ws_id, surface, live_pid)]
        );
    }

    #[tokio::test]
    async fn dead_pid_clear_restores_remote_agent_after_no_signal_boundary() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "opencode".into(),
                        status: Some(AgentStatus::Idle),
                        activity: Some(flowmux_core::AgentActivity::Idle),
                        pid: Some(42),
                        source: Some("flowmux:hook".into()),
                        seq: Some(1),
                        message: None,
                        custom_status: None,
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );
        assert_eq!(
            store
                .clear_dead_agent_presence(surface, 42)
                .await
                .map(|removed| removed.workspace),
            Some(ws_id)
        );
        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some(
                        "Ask anything... \"Fix broken tests\"\n\
                         Sisyphus - Ultraworker · GPT-5.5 OpenAI · medium\n\
                         tab agents  ctrl+p commands"
                    ),
                    Some("OpenCode"),
                )
                .await,
            None
        );
        assert!(store.located_agent_presence(surface).await.is_none());

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("$ echo shell ready"), Some("demo"))
                .await,
            None
        );

        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("OpenCode\n• Working (1s • esc to interrupt)"),
                    Some("OpenCode"),
                )
                .await,
            Some((ws_id, Some(AgentStatus::Working)))
        );

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "opencode");
        assert_eq!(agent.status, AgentStatus::Working);
        assert_eq!(agent.source.as_deref(), Some("flowmux:screen"));
    }

    #[tokio::test]
    async fn dead_pid_clear_requires_no_signal_before_restoring_changed_working_frame() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);
        let stale_frame = "Codex\n• Working (1s • esc to interrupt)";

        store
            .report_agent_status(
                surface,
                AgentStatusReport {
                    name: "codex".into(),
                    status: Some(AgentStatus::Working),
                    activity: Some(flowmux_core::AgentActivity::Running),
                    pid: Some(42),
                    source: Some("flowmux:hook".into()),
                    seq: Some(1),
                    message: None,
                    custom_status: Some("Working".into()),
                    session_id: Some("old-session".into()),
                    session_name: None,
                    messaging_socket: None,
                },
            )
            .await;
        store
            .report_agent_screen_signals(surface, Some(stale_frame), Some("Codex ⠋ working"))
            .await;
        assert!(store.clear_dead_agent_presence(surface, 42).await.is_some());

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some(stale_frame), Some("Codex ⠋ working"))
                .await,
            None
        );
        assert!(store.located_agent_presence(surface).await.is_none());

        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("Codex\n• Working (2s • esc to interrupt)"),
                    Some("Codex ⠙ working"),
                )
                .await,
            None
        );
        assert!(store.located_agent_presence(surface).await.is_none());

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("$ echo shell ready"), Some("demo"))
                .await,
            None
        );
        assert_eq!(
            store
                .report_agent_screen_signals(
                    surface,
                    Some("Codex\n• Working (3s • esc to interrupt)"),
                    Some("Codex ⠹ working"),
                )
                .await,
            Some((ws_id, Some(AgentStatus::Working)))
        );
    }

    #[tokio::test]
    async fn teardown_serializes_with_positive_screen_signal_reconciliation() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let pid = std::process::id();
        store
            .report_agent_status_with_visibility(
                surface,
                AgentStatusReport {
                    name: "codex".into(),
                    status: Some(AgentStatus::Unknown),
                    activity: None,
                    pid: Some(pid),
                    source: Some("flowmux:hook".into()),
                    seq: Some(1),
                    message: None,
                    custom_status: Some("Ready".into()),
                    session_id: Some("session-1".into()),
                    session_name: None,
                    messaging_socket: None,
                },
                true,
            )
            .await;

        // Hold state so the screen task stops after acquiring lifecycle. A
        // teardown queued behind it must then run last and leave no ghost.
        let state_guard = store.inner.lock().await;
        let screen_store = store.clone();
        let screen = tokio::spawn(async move {
            screen_store
                .report_agent_screen_signals_with_visibility(
                    surface,
                    Some("Codex\n• Working (1s • esc to interrupt)"),
                    Some("Codex ⠋ working"),
                    true,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if store.agent_lifecycle.try_lock().is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("screen reconciliation should hold lifecycle while waiting for state");

        let teardown_store = store.clone();
        let teardown = tokio::spawn(async move {
            teardown_store
                .end_agent_session(surface, "codex", Some(2), Some("session-1"), Some(pid))
                .await
        });
        drop(state_guard);

        assert!(screen.await.unwrap().is_some());
        assert!(teardown.await.unwrap().is_some());
        assert!(store.located_agent_presence(surface).await.is_none());
        assert!(store.cleared_agent_surfaces.lock().await.contains(&surface));
    }

    #[tokio::test]
    async fn stale_agent_name_scrollback_does_not_recreate_screen_presence() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("codex exited\n$ echo done"), None)
                .await,
            None
        );

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        assert!(tree[0].panes[0].tabs[0].agent.is_none());
    }

    #[tokio::test]
    async fn cleared_agent_presence_blocks_stale_screen_recreation_until_hook_report() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "codex".into(),
                        status: Some(AgentStatus::Working),
                        activity: Some(flowmux_core::AgentActivity::Running),
                        pid: Some(42),
                        source: Some("flowmux:hook".into()),
                        seq: Some(1),
                        message: None,
                        custom_status: None,
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await,
            Some((ws_id, Some(AgentStatus::Working)))
        );

        assert_eq!(store.set_agent_activity(surface, None).await, Some(ws_id));
        assert_eq!(
            store
                .report_agent_screen_signals(surface, Some("Codex\npress / for commands"), None)
                .await,
            None
        );
        assert_eq!(
            store
                .report_agent_screen_signals(surface, None, Some("Codex Action Required"))
                .await,
            None
        );
        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        assert!(tree[0].panes[0].tabs[0].agent.is_none());

        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "codex".into(),
                        status: Some(AgentStatus::Idle),
                        activity: Some(flowmux_core::AgentActivity::Idle),
                        pid: Some(43),
                        source: Some("flowmux:hook".into()),
                        seq: Some(2),
                        message: None,
                        custom_status: None,
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );
        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "codex");
        assert_eq!(
            store.live_agent_presences().await,
            vec![(ws_id, surface, 43)]
        );
    }

    #[tokio::test]
    async fn session_end_only_removes_the_current_agent_session() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(
                Some("activity-demo".into()),
                std::path::PathBuf::from("/tmp/activity-demo"),
            )
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);
        let pane = first_pane(&ws);
        store
            .report_agent_status(
                surface,
                AgentStatusReport {
                    name: "claude".into(),
                    status: Some(AgentStatus::Working),
                    activity: None,
                    pid: Some(42),
                    source: Some("flowmux:hook".into()),
                    seq: Some(20),
                    message: None,
                    custom_status: Some("Starting turn".into()),
                    session_id: Some("session-new".into()),
                    session_name: None,
                    messaging_socket: None,
                },
            )
            .await;

        assert!(store
            .end_agent_session(surface, "claude", Some(10), Some("session-new"), None)
            .await
            .is_none());
        assert!(store
            .end_agent_session(surface, "claude", Some(21), Some("session-old"), None)
            .await
            .is_none());
        assert!(store
            .end_agent_session(surface, "claude", None, Some("session-new"), None)
            .await
            .is_none());
        assert!(store
            .end_agent_session(surface, "claude", Some(21), None, None)
            .await
            .is_none());
        assert!(store.located_agent_presence(surface).await.is_some());

        let removed = store
            .end_agent_session(surface, "claude", Some(21), Some("session-new"), None)
            .await
            .expect("matching SessionEnd should remove presence");
        assert_eq!(removed.workspace, ws_id);
        assert_eq!(removed.pane, pane);
        assert_eq!(removed.surface, surface);
        assert_eq!(removed.workspace_label, "activity-demo");
        assert_eq!(removed.presence.session_id.as_deref(), Some("session-new"));
        assert!(store.located_agent_presence(surface).await.is_none());
    }

    #[tokio::test]
    async fn dead_pid_clear_cannot_remove_a_replacement_process() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        let old_pid = u32::MAX - 1;
        let replacement_pid = u32::MAX;
        for (pid, seq) in [(old_pid, 1), (replacement_pid, 2)] {
            assert!(store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "codex".into(),
                        status: Some(AgentStatus::Working),
                        activity: None,
                        pid: Some(pid),
                        source: Some("flowmux:hook".into()),
                        seq: Some(seq),
                        message: None,
                        custom_status: Some("Working".into()),
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await
                .is_some());
        }

        assert!(store
            .clear_dead_agent_presence(surface, old_pid)
            .await
            .is_none());
        assert_eq!(
            store.live_agent_presences().await,
            vec![(ws_id, surface, replacement_pid)]
        );
        assert_eq!(
            store
                .clear_dead_agent_presence(surface, replacement_pid)
                .await
                .unwrap()
                .presence
                .pid,
            Some(replacement_pid)
        );
    }

    #[tokio::test]
    async fn report_agent_screen_signals_rejects_mismatched_hook_status() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .report_agent_status(
                    surface,
                    AgentStatusReport {
                        name: "claude".into(),
                        status: Some(AgentStatus::Idle),
                        activity: Some(flowmux_core::AgentActivity::Idle),
                        pid: Some(42),
                        source: Some("flowmux:hook".into()),
                        seq: Some(1),
                        message: None,
                        custom_status: None,
                        session_id: None,
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await,
            Some((ws_id, Some(AgentStatus::Idle)))
        );

        let result = store
            .report_agent_screen_signals(surface, None, Some("Cline Action Required"))
            .await;
        assert_eq!(result, None);

        let state = store.snapshot().await;
        let tree = flowmux_ipc::protocol::describe_workspaces(&state.workspaces);
        let agent = tree[0].panes[0].tabs[0].agent.as_ref().unwrap();
        assert_eq!(agent.name, "claude");
        assert_eq!(agent.status, AgentStatus::Idle);
        assert_eq!(agent.source.as_deref(), Some("flowmux:hook"));
    }

    #[tokio::test]
    async fn split_and_close_pane_updates_tree_and_removes_empty_workspace() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let (split_ws, new_pane) = store
            .split_pane(original, SplitDirection::Vertical)
            .await
            .unwrap();
        assert_eq!(split_ws, ws_id);
        assert_eq!(store.workspace_for_pane(new_pane).await, Some(ws_id));
        let ws = store.get_workspace(ws_id).await.unwrap();
        let new_surface = ws.surfaces[0]
            .root_pane
            .active_surface_id(new_pane)
            .expect("expected active surface in new pane");
        assert_eq!(
            ws.surfaces[0]
                .root_pane
                .surface_title(new_pane, new_surface),
            Some("demo")
        );

        let (_, browser_surface) = store
            .add_browser_surface_to_pane(new_pane, "https://example.test".into())
            .await
            .expect("second tab should be added to the new pane");
        assert_eq!(store.tab_count_in_pane(new_pane).await, Some(2));

        let outcome = store.close_pane(new_pane).await.unwrap();
        assert!(matches!(outcome, CloseOutcome::PaneRemoved { workspace } if workspace == ws_id));
        assert_eq!(store.workspace_for_pane(new_pane).await, None);
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.surfaces[0].root_pane.first_leaf_id(), Some(original));
        assert_eq!(
            ws.surfaces[0]
                .root_pane
                .surface_title(new_pane, browser_surface),
            None,
            "closing a pane removes every tab it contained"
        );

        let outcome = store.close_pane(original).await.unwrap();
        assert!(matches!(
            outcome,
            CloseOutcome::WorkspaceRemoved { workspace } if workspace == ws_id
        ));
        let state = store.snapshot().await;
        assert!(state.workspaces.is_empty());
        assert!(state.workspace_order.is_empty());
        assert_eq!(state.active_workspace, None);
    }

    #[tokio::test]
    async fn workspace_mutators_only_mark_successful_changes() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("old".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let missing = WorkspaceId::new();

        // Rename follows cmux semantics: keep `name` as the automatic value and
        // update only custom_title.
        assert!(store.rename_workspace(ws_id, "new".into()).await);
        assert!(store.set_workspace_color(ws_id, "#112233".into()).await);
        assert!(!store.rename_workspace(missing, "missing".into()).await);
        assert!(!store.set_workspace_color(missing, "#445566".into()).await);
        store.set_active_workspace(Some(missing)).await;

        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.name, "old");
        assert_eq!(ws.custom_title.as_deref(), Some("new"));
        assert_eq!(ws.display_title(), "new");
        assert_eq!(ws.color.as_deref(), Some("#112233"));
        assert_eq!(store.snapshot().await.active_workspace, Some(ws_id));
    }

    #[tokio::test]
    async fn rename_workspace_clears_custom_title_on_empty_input() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("auto".into()), std::path::PathBuf::from("/tmp/auto"))
            .await;

        // User rename -> custom_title is filled.
        assert!(store.rename_workspace(ws_id, "MyName".into()).await);
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.custom_title.as_deref(), Some("MyName"));
        assert_eq!(ws.display_title(), "MyName");

        // Empty input -> return to automatic mode (custom_title = None).
        assert!(store.rename_workspace(ws_id, "".into()).await);
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.custom_title, None);
        assert_eq!(ws.display_title(), "auto");
        assert_eq!(ws.name, "auto");

        // Whitespace-only input has the same meaning.
        assert!(store.rename_workspace(ws_id, "Custom Again".into()).await);
        assert!(store.rename_workspace(ws_id, "   \t\n".into()).await);
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.custom_title, None);
    }

    #[tokio::test]
    async fn rename_workspace_trims_whitespace_around_input() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("auto".into()), std::path::PathBuf::from("/tmp/auto"))
            .await;
        assert!(
            store
                .rename_workspace(ws_id, "  Spaced Name  ".into())
                .await
        );
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.custom_title.as_deref(), Some("Spaced Name"));
    }

    #[tokio::test]
    async fn rename_workspace_idempotent_for_same_value() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("auto".into()), std::path::PathBuf::from("/tmp/auto"))
            .await;
        assert!(store.rename_workspace(ws_id, "Same".into()).await);
        // Re-entering the same value -> false (no change).
        assert!(!store.rename_workspace(ws_id, "Same".into()).await);
        // Same trimmed result returns false.
        assert!(!store.rename_workspace(ws_id, "  Same  ".into()).await);
        // Empty input twice returns false on the second call.
        assert!(store.rename_workspace(ws_id, "".into()).await);
        assert!(!store.rename_workspace(ws_id, "".into()).await);
    }

    #[tokio::test]
    async fn normalizes_legacy_top_level_surfaces_into_first_pane_tabs() {
        let ws_id = WorkspaceId::new();
        let first_surface = SurfaceId::new();
        let second_surface = SurfaceId::new();
        let mut state = State::default();
        state.workspaces.push(Workspace {
            id: ws_id,
            name: "legacy".into(),
            custom_title: None,
            root_dir: "/tmp/legacy".into(),
            git: None,
            listening_ports: vec![],
            surfaces: vec![
                Surface {
                    id: first_surface,
                    kind: SurfaceKind::Terminal {
                        shell: None,
                        cwd: Some("/tmp/legacy".into()),
                    },
                    title: "main".into(),
                    root_pane: Pane::Leaf {
                        id: PaneId::new(),
                        content: PaneContent::Terminal { pid: None },
                    },
                },
                Surface {
                    id: second_surface,
                    kind: SurfaceKind::Browser {
                        initial_url: Some("https://example.com".into()),
                    },
                    title: "Browser".into(),
                    root_pane: Pane::Leaf {
                        id: PaneId::new(),
                        content: PaneContent::Browser {
                            url: "https://example.com".into(),
                        },
                    },
                },
            ],
            color: None,
        });

        let store = StateStore::new_lazy(state);
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.surfaces.len(), 1);
        assert_eq!(first_pane_tab_count(&ws), 2);

        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, active },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected migrated tabbed root")
        };
        assert_eq!(*active, second_surface);
        assert!(surfaces.iter().any(|surface| surface.id == second_surface
            && matches!(&surface.kind, SurfaceKind::Browser { .. })));
    }

    #[tokio::test]
    async fn normalizes_legacy_terminal_number_titles_to_cwd_folder() {
        let ws_id = WorkspaceId::new();
        let pane_id = PaneId::new();
        let tab = PaneSurface::terminal("Terminal 3", Some("/tmp/project".into()));
        let tab_id = tab.id;
        let mut state = State::default();
        state.workspaces.push(Workspace {
            id: ws_id,
            name: "legacy".into(),
            custom_title: None,
            root_dir: "/tmp/legacy".into(),
            git: None,
            listening_ports: vec![],
            surfaces: vec![Surface {
                id: SurfaceId::new(),
                kind: SurfaceKind::Terminal {
                    shell: None,
                    cwd: Some("/tmp/legacy".into()),
                },
                title: "main".into(),
                root_pane: Pane::Leaf {
                    id: pane_id,
                    content: PaneContent::Tabs {
                        active: tab_id,
                        surfaces: vec![tab],
                    },
                },
            }],
            color: None,
        });

        let store = StateStore::new_lazy(state);
        let ws = store.get_workspace(ws_id).await.unwrap();
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected tabbed leaf")
        };
        assert_eq!(surfaces[0].title, "project");
        assert!(!surfaces[0].title_locked);
    }

    #[tokio::test]
    async fn resets_stale_terminal_titles_during_normalization_on_load() {
        // The persisted state captures whatever OSC 0/2 the program
        // running inside the tab last set ("Claude Code", "codex …",
        // "vim foo"). On the next launch that program is gone, so the
        // tab title must reset to the cwd-derived form. This test
        // pins the new behavior; a previous version of flowmux
        // auto-locked the stale title and it survived restarts.
        let ws_id = WorkspaceId::new();
        let pane_id = PaneId::new();
        let tab = PaneSurface::terminal("Claude Code", Some("/tmp/one".into()));
        let tab_id = tab.id;
        let mut state = State::default();
        state.workspaces.push(Workspace {
            id: ws_id,
            name: "legacy".into(),
            custom_title: None,
            root_dir: "/tmp/legacy".into(),
            git: None,
            listening_ports: vec![],
            surfaces: vec![Surface {
                id: SurfaceId::new(),
                kind: SurfaceKind::Terminal {
                    shell: None,
                    cwd: Some("/tmp/legacy".into()),
                },
                title: "main".into(),
                root_pane: Pane::Leaf {
                    id: pane_id,
                    content: PaneContent::Tabs {
                        active: tab_id,
                        surfaces: vec![tab],
                    },
                },
            }],
            color: None,
        });

        let store = StateStore::new_lazy(state);
        let ws = store.get_workspace(ws_id).await.unwrap();
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected tabbed leaf")
        };
        assert_eq!(
            surfaces[0].title,
            terminal_tab_title_for_cwd(Some(std::path::Path::new("/tmp/one")))
        );
        assert!(
            !surfaces[0].title_locked,
            "must not auto-lock; the title was never the user's intent"
        );
    }

    #[tokio::test]
    async fn add_surfaces_and_remove_workspace_keep_order_consistent() {
        let store = StateStore::new_lazy(State::default());
        let first = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let second = store
            .create_workspace(Some("two".into()), std::path::PathBuf::from("/tmp/two"))
            .await;

        let terminal = store
            .add_terminal_surface(first, Some("/tmp/one".into()))
            .await;
        let browser = store
            .add_browser_surface(first, "https://example.com".into())
            .await;
        assert!(terminal.is_some());
        assert!(browser.is_some());
        assert_eq!(store.get_workspace(first).await.unwrap().surfaces.len(), 1);
        assert_eq!(
            first_pane_tab_count(&store.get_workspace(first).await.unwrap()),
            3
        );

        assert!(store.remove_workspace(first).await);
        let state = store.snapshot().await;
        assert_eq!(state.workspace_order, vec![second]);
        assert_eq!(state.active_workspace, Some(second));
        assert!(!store.remove_workspace(first).await);
    }

    #[tokio::test]
    async fn remove_all_workspaces_clears_everything() {
        let store = StateStore::new_lazy(State::default());
        let first = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let second = store
            .create_workspace(Some("two".into()), std::path::PathBuf::from("/tmp/two"))
            .await;
        let third = store
            .create_workspace(Some("three".into()), std::path::PathBuf::from("/tmp/three"))
            .await;

        let removed = store.remove_all_workspaces().await;
        assert_eq!(removed, vec![first, second, third]);

        let state = store.snapshot().await;
        assert!(state.workspaces.is_empty());
        assert!(state.workspace_order.is_empty());
        assert_eq!(state.active_workspace, None);

        // Idempotent: a second call on an empty store removes nothing.
        assert!(store.remove_all_workspaces().await.is_empty());
    }

    #[tokio::test]
    async fn close_surface_removes_tab_then_last_pane() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let (tab_ws, second_surface) = store
            .add_terminal_surface_to_pane(pane, Some("/tmp/one".into()))
            .await
            .unwrap();
        assert_eq!(tab_ws, ws_id);
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(first_pane_tab_count(&ws), 2);
        assert_eq!(first_pane_active_surface(&ws), second_surface);

        let outcome = store.close_surface(pane, second_surface).await.unwrap();
        assert!(matches!(
            outcome,
            CloseOutcome::SurfaceRemoved { workspace } if workspace == ws_id
        ));
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(first_pane_tab_count(&ws), 1);

        let last_surface = first_pane_active_surface(&ws);
        let outcome = store.close_surface(pane, last_surface).await.unwrap();
        assert!(matches!(
            outcome,
            CloseOutcome::WorkspaceRemoved { workspace } if workspace == ws_id
        ));
        assert!(store.snapshot().await.workspaces.is_empty());
    }

    #[tokio::test]
    async fn close_paths_prune_cleared_agent_surface_latches() {
        let store = StateStore::new_lazy(State::default());

        let close_surface_workspace = store
            .create_workspace(
                Some("close-surface".into()),
                std::path::PathBuf::from("/tmp/close-surface"),
            )
            .await;
        let workspace = store.get_workspace(close_surface_workspace).await.unwrap();
        let pane = first_pane(&workspace);
        let surface = first_pane_active_surface(&workspace);
        store.cleared_agent_surfaces.lock().await.insert(surface);
        store.close_surface(pane, surface).await.unwrap();
        assert!(!store.cleared_agent_surfaces.lock().await.contains(&surface));

        let close_pane_workspace = store
            .create_workspace(
                Some("close-pane".into()),
                std::path::PathBuf::from("/tmp/close-pane"),
            )
            .await;
        let original = first_pane(&store.get_workspace(close_pane_workspace).await.unwrap());
        let (_, split) = store
            .split_pane(original, SplitDirection::Vertical)
            .await
            .unwrap();
        let workspace = store.get_workspace(close_pane_workspace).await.unwrap();
        let split_surface = workspace.surfaces[0]
            .root_pane
            .active_surface_id(split)
            .unwrap();
        store
            .cleared_agent_surfaces
            .lock()
            .await
            .insert(split_surface);
        store.close_pane(split).await.unwrap();
        assert!(!store
            .cleared_agent_surfaces
            .lock()
            .await
            .contains(&split_surface));

        let remove_workspace = store
            .create_workspace(
                Some("remove-workspace".into()),
                std::path::PathBuf::from("/tmp/remove-workspace"),
            )
            .await;
        let removed_surface =
            first_pane_active_surface(&store.get_workspace(remove_workspace).await.unwrap());
        store
            .cleared_agent_surfaces
            .lock()
            .await
            .insert(removed_surface);
        assert!(store.remove_workspace(remove_workspace).await);
        assert!(!store
            .cleared_agent_surfaces
            .lock()
            .await
            .contains(&removed_surface));

        let remaining = store
            .snapshot()
            .await
            .workspaces
            .iter()
            .flat_map(workspace_pane_surface_ids)
            .collect::<Vec<_>>();
        store
            .cleared_agent_surfaces
            .lock()
            .await
            .extend(remaining.iter().copied());
        store.remove_all_workspaces().await;
        assert!(store.cleared_agent_surfaces.lock().await.is_empty());
    }

    #[tokio::test]
    async fn terminal_tab_shell_override_is_stored_on_the_new_surface() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("shell".into()), std::path::PathBuf::from("/tmp"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let (_, surface) = store
            .add_terminal_surface_to_pane_with_shell(pane, None, Some("/bin/dash".to_string()))
            .await
            .unwrap();

        let workspace = store.get_workspace(ws_id).await.unwrap();
        let tab = workspace.surfaces[0]
            .root_pane
            .find_surface(pane, surface)
            .unwrap();
        assert!(matches!(
            &tab.kind,
            SurfaceKind::Terminal { shell: Some(shell), .. } if shell == "/bin/dash"
        ));
    }

    #[tokio::test]
    async fn rename_surface_updates_pane_tab_title() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store.surface_title(pane, surface).await.as_deref(),
            Some("one")
        );
        assert_eq!(
            store.rename_surface(pane, surface, "server".into()).await,
            Some(ws_id)
        );
        assert_eq!(
            store.surface_title(pane, surface).await.as_deref(),
            Some("server")
        );
    }

    #[tokio::test]
    async fn update_surface_cwd_persists_terminal_tab_cwd() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store
                .update_surface_cwd(pane, surface, "/tmp/two".into())
                .await,
            Some(ws_id)
        );

        let ws = store.get_workspace(ws_id).await.unwrap();
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected tabbed leaf")
        };
        assert!(matches!(
            &surfaces[0].kind,
            SurfaceKind::Terminal { cwd: Some(cwd), .. } if cwd == &std::path::PathBuf::from("/tmp/two")
        ));
        assert_eq!(surfaces[0].title, "two");
        assert!(!surfaces[0].title_locked);
    }

    #[tokio::test]
    async fn update_surface_scrollback_persists_bounded_history_and_skips_duplicates() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);
        let surface = first_pane_active_surface(&ws);
        let text = format!(
            "{}latest",
            "x".repeat(flowmux_core::TERMINAL_SCROLLBACK_MAX_BYTES)
        );

        assert_eq!(
            store
                .update_surface_scrollback(pane, surface, text.clone())
                .await,
            Some(ws_id)
        );
        assert_eq!(
            store.update_surface_scrollback(pane, surface, text).await,
            None
        );
        let ws = store.get_workspace(ws_id).await.unwrap();
        let saved = ws.surfaces[0]
            .root_pane
            .find_surface(pane, surface)
            .unwrap()
            .scrollback
            .unwrap();
        assert!(saved.content().len() <= flowmux_core::TERMINAL_SCROLLBACK_MAX_BYTES);
        assert!(saved.content().ends_with("latest"));
    }

    #[tokio::test]
    async fn update_editor_session_persists_view_and_active_title() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("project".into()), "/tmp/project".into())
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let (_, editor) = store
            .add_editor_surface_to_pane(pane, "/tmp/project".into())
            .await
            .unwrap();
        let session = EditorSessionState {
            open_files: vec![flowmux_core::EditorFileState {
                path: "/tmp/project/문서-日本語.rs".into(),
                cursor_line: 11,
                cursor_column: 5,
                scroll_top: 72.0,
            }],
            active_file: Some("/tmp/project/문서-日本語.rs".into()),
            zoom_percent: Some(135),
        };

        assert_eq!(
            store
                .update_editor_session(pane, editor, session.clone())
                .await,
            Some(ws_id)
        );
        assert_eq!(
            store.update_editor_session(pane, editor, session).await,
            None
        );
        assert_eq!(
            store.surface_title(pane, editor).await.as_deref(),
            Some("문서-日本語.rs")
        );
        let workspace = store.get_workspace(ws_id).await.unwrap();
        let surface = workspace.surfaces[0]
            .root_pane
            .find_surface(pane, editor)
            .unwrap();
        let SurfaceKind::Editor { session, .. } = &surface.kind else {
            panic!("expected editor surface");
        };
        assert_eq!(session.open_files[0].cursor_line, 11);
        assert_eq!(session.open_files[0].scroll_top, 72.0);
    }

    #[tokio::test]
    async fn update_surface_cwd_keeps_manually_renamed_tab_title() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);
        let surface = first_pane_active_surface(&ws);

        assert_eq!(
            store.rename_surface(pane, surface, "server".into()).await,
            Some(ws_id)
        );
        assert_eq!(
            store
                .update_surface_cwd(pane, surface, "/tmp/two".into())
                .await,
            Some(ws_id)
        );

        let ws = store.get_workspace(ws_id).await.unwrap();
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected tabbed leaf")
        };
        assert_eq!(surfaces[0].title, "server");
        assert!(surfaces[0].title_locked);
    }

    #[tokio::test]
    async fn add_terminal_surface_defaults_title_to_truncated_cwd_folder() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);

        store
            .add_terminal_surface_to_pane(pane, Some("/tmp/1234567890123456789".into()))
            .await
            .unwrap();

        let ws = store.get_workspace(ws_id).await.unwrap();
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, active },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected tabbed leaf")
        };
        let active = surfaces
            .iter()
            .find(|surface| surface.id == *active)
            .expect("expected active surface");
        assert_eq!(active.title, "12345678901234567...");
        assert!(!active.title_locked);
    }

    #[tokio::test]
    async fn add_terminal_surface_without_cwd_uses_pane_cwd() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);

        store
            .add_terminal_surface_to_pane(pane, None)
            .await
            .unwrap();

        let ws = store.get_workspace(ws_id).await.unwrap();
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, active },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected tabbed leaf")
        };
        let active = surfaces
            .iter()
            .find(|surface| surface.id == *active)
            .expect("expected active surface");
        assert_eq!(active.title, "one");
        assert!(matches!(
            &active.kind,
            SurfaceKind::Terminal { cwd: Some(cwd), .. } if cwd == &std::path::PathBuf::from("/tmp/one")
        ));
    }

    fn collect_leaves(p: &Pane) -> Vec<PaneId> {
        let mut v = Vec::new();
        p.for_each_leaf(|id| v.push(id));
        v
    }

    fn pane_split_direction(p: &Pane) -> Option<SplitDirection> {
        match p {
            Pane::Split { direction, .. } => Some(*direction),
            Pane::Leaf { .. } => None,
        }
    }

    fn find_browser_pane_url(p: &Pane, target: PaneId) -> Option<String> {
        match p {
            Pane::Leaf { id, content } if *id == target => match content {
                PaneContent::Tabs { surfaces, .. } => surfaces.iter().find_map(|s| match &s.kind {
                    SurfaceKind::Browser { initial_url } => initial_url.clone(),
                    _ => None,
                }),
                PaneContent::Browser { url } => Some(url.clone()),
                PaneContent::Terminal { .. } => None,
            },
            Pane::Leaf { .. } => None,
            Pane::Split { first, second, .. } => find_browser_pane_url(first, target)
                .or_else(|| find_browser_pane_url(second, target)),
        }
    }

    #[tokio::test]
    async fn split_pane_with_browser_creates_browser_sibling() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let (split_ws, new_pane) = store
            .split_pane_with_browser(
                original,
                SplitDirection::Vertical,
                "https://example.com".into(),
            )
            .await
            .expect("split should succeed for valid target");
        assert_eq!(split_ws, ws_id);
        assert_ne!(new_pane, original);

        let ws = store.get_workspace(ws_id).await.unwrap();
        let leaves = collect_leaves(&ws.surfaces[0].root_pane);
        assert_eq!(leaves.len(), 2);
        assert!(leaves.contains(&original));
        assert!(leaves.contains(&new_pane));

        let url = find_browser_pane_url(&ws.surfaces[0].root_pane, new_pane);
        assert_eq!(url.as_deref(), Some("https://example.com"));
    }

    #[tokio::test]
    async fn split_pane_with_browser_returns_none_for_unknown_target() {
        let store = StateStore::new_lazy(State::default());
        let _ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let bogus = PaneId::new();

        let result = store
            .split_pane_with_browser(bogus, SplitDirection::Vertical, "https://x.test".into())
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn split_pane_with_browser_returns_none_when_no_workspaces() {
        let store = StateStore::new_lazy(State::default());
        let bogus = PaneId::new();
        let result = store
            .split_pane_with_browser(bogus, SplitDirection::Horizontal, "https://x.test".into())
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn split_pane_with_browser_honors_vertical_direction() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());

        store
            .split_pane_with_browser(
                original,
                SplitDirection::Vertical,
                "https://example.com".into(),
            )
            .await
            .unwrap();

        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(
            pane_split_direction(&ws.surfaces[0].root_pane),
            Some(SplitDirection::Vertical)
        );
    }

    #[tokio::test]
    async fn split_pane_with_browser_honors_horizontal_direction() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());

        store
            .split_pane_with_browser(
                original,
                SplitDirection::Horizontal,
                "https://example.com".into(),
            )
            .await
            .unwrap();

        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(
            pane_split_direction(&ws.surfaces[0].root_pane),
            Some(SplitDirection::Horizontal)
        );
    }

    #[tokio::test]
    async fn split_pane_with_browser_finds_correct_workspace_among_many() {
        let store = StateStore::new_lazy(State::default());
        let _first = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let middle = store
            .create_workspace(Some("two".into()), std::path::PathBuf::from("/tmp/two"))
            .await;
        let _last = store
            .create_workspace(Some("three".into()), std::path::PathBuf::from("/tmp/three"))
            .await;

        let target = first_pane(&store.get_workspace(middle).await.unwrap());
        let (ws_id, new_pane) = store
            .split_pane_with_browser(
                target,
                SplitDirection::Vertical,
                "https://middle.test".into(),
            )
            .await
            .expect("split should succeed");
        assert_eq!(ws_id, middle);

        // The other workspaces stayed single-leaf.
        for id in [_first, _last] {
            let ws = store.get_workspace(id).await.unwrap();
            assert_eq!(collect_leaves(&ws.surfaces[0].root_pane).len(), 1);
        }

        let middle_ws = store.get_workspace(middle).await.unwrap();
        assert_eq!(collect_leaves(&middle_ws.surfaces[0].root_pane).len(), 2);
        assert_eq!(
            find_browser_pane_url(&middle_ws.surfaces[0].root_pane, new_pane).as_deref(),
            Some("https://middle.test")
        );
    }

    #[tokio::test]
    async fn split_pane_with_browser_preserves_target_leaf_id() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());

        store
            .split_pane_with_browser(
                original,
                SplitDirection::Vertical,
                "https://example.com".into(),
            )
            .await
            .unwrap();

        // After splitting, the original PaneId is still resolvable via
        // workspace_for_pane — it must still be a leaf in the tree.
        assert_eq!(store.workspace_for_pane(original).await, Some(ws_id));
    }

    #[tokio::test]
    async fn split_pane_with_browser_assigns_unique_pane_ids() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let (_, first_new) = store
            .split_pane_with_browser(original, SplitDirection::Vertical, "https://a.test".into())
            .await
            .unwrap();
        let (_, second_new) = store
            .split_pane_with_browser(
                first_new,
                SplitDirection::Horizontal,
                "https://b.test".into(),
            )
            .await
            .unwrap();

        assert_ne!(first_new, original);
        assert_ne!(second_new, original);
        assert_ne!(first_new, second_new);

        let ws = store.get_workspace(ws_id).await.unwrap();
        let leaves = collect_leaves(&ws.surfaces[0].root_pane);
        assert_eq!(leaves.len(), 3);
        assert!(leaves.contains(&original));
        assert!(leaves.contains(&first_new));
        assert!(leaves.contains(&second_new));
    }

    #[tokio::test]
    async fn split_pane_with_browser_browser_pane_uses_initial_url() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let url = "https://docs.example.org/path?q=1#frag";
        let (_, new_pane) = store
            .split_pane_with_browser(original, SplitDirection::Vertical, url.into())
            .await
            .unwrap();

        let ws = store.get_workspace(ws_id).await.unwrap();
        let Pane::Split { second, .. } = &ws.surfaces[0].root_pane else {
            panic!("expected split root after split_pane_with_browser")
        };
        let Pane::Leaf { id, content } = second.as_ref() else {
            panic!("expected new sibling to be a leaf")
        };
        assert_eq!(*id, new_pane);
        let PaneContent::Tabs { surfaces, active } = content else {
            panic!("browser pane content must be tabbed")
        };
        let active_surface = surfaces
            .iter()
            .find(|s| s.id == *active)
            .expect("active surface must exist");
        assert!(matches!(
            &active_surface.kind,
            SurfaceKind::Browser { initial_url: Some(u) } if u == url
        ));
    }

    #[tokio::test]
    async fn add_browser_surface_to_pane_appends_browser_tab_and_activates() {
        // Creating a workspace creates one terminal tab in the first pane. Pressing
        // the browser-tab add button should add a new browser tab to the same
        // pane and make it active.
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        assert_eq!(
            first_pane_tab_count(&store.get_workspace(ws_id).await.unwrap()),
            1
        );

        let (returned_ws, browser_surface) = store
            .add_browser_surface_to_pane(pane, "about:blank".into())
            .await
            .expect("browser tab should be added to existing pane");
        assert_eq!(returned_ws, ws_id);

        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(first_pane_tab_count(&ws), 2);
        assert_eq!(first_pane_active_surface(&ws), browser_surface);

        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } = &ws.surfaces[0].root_pane
        else {
            panic!("expected tabbed leaf after add_browser_surface_to_pane")
        };
        let added = surfaces
            .iter()
            .find(|s| s.id == browser_surface)
            .expect("new browser surface must be present");
        assert!(matches!(
            &added.kind,
            SurfaceKind::Browser { initial_url: Some(u) } if u == "about:blank"
        ));
    }

    #[tokio::test]
    async fn add_browser_surface_to_pane_returns_none_for_unknown_pane() {
        let store = StateStore::new_lazy(State::default());
        let _ = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let bogus = PaneId::new();

        let result = store
            .add_browser_surface_to_pane(bogus, "about:blank".into())
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn add_editor_surface_to_pane_uses_supplied_root_and_activates() {
        let store = StateStore::new_lazy(State::default());
        let workspace_root = std::path::PathBuf::from("/tmp/다국어-プロジェクト");
        let editor_root = std::path::PathBuf::from("/tmp/현재-ディレクトリ");
        let ws_id = store
            .create_workspace(Some("demo".into()), workspace_root)
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let (returned_ws, editor_surface) = store
            .add_editor_surface_to_pane(pane, editor_root.clone())
            .await
            .expect("editor tab should be added to existing pane");
        assert_eq!(returned_ws, ws_id);

        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(first_pane_tab_count(&ws), 2);
        assert_eq!(first_pane_active_surface(&ws), editor_surface);
        let added = pane_surfaces(&ws, pane)
            .into_iter()
            .find(|surface| surface.id == editor_surface)
            .expect("new editor surface must be present");
        assert!(matches!(
            added.kind,
            SurfaceKind::Editor {
                workspace_root,
                session
            } if workspace_root == editor_root && session.open_files.is_empty()
        ));
    }

    #[tokio::test]
    async fn repeated_editor_adds_create_separate_pane_tabs() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let root = std::path::PathBuf::from("/tmp/demo");
        let (_, first_editor) = store
            .add_editor_surface_to_pane(pane, root.clone())
            .await
            .unwrap();
        let (_, second_editor) = store.add_editor_surface_to_pane(pane, root).await.unwrap();

        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_ne!(first_editor, second_editor);
        assert_eq!(first_pane_tab_count(&ws), 3);
        assert_eq!(first_pane_active_surface(&ws), second_editor);
    }

    #[tokio::test]
    async fn add_editor_surface_to_pane_returns_none_for_unknown_pane() {
        let store = StateStore::new_lazy(State::default());
        let _ = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;

        assert!(store
            .add_editor_surface_to_pane(PaneId::new(), "/tmp/demo".into())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn add_browser_surface_to_pane_targets_correct_pane_after_split() {
        // A newly split sibling pane, not the original pane, should also accept
        // browser tabs without affecting tab counts in other panes.
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let (_, sibling) = store
            .split_pane(original, SplitDirection::Vertical)
            .await
            .unwrap();

        let (_, browser_surface) = store
            .add_browser_surface_to_pane(sibling, "https://example.com".into())
            .await
            .expect("browser tab should be added to sibling pane");

        let ws = store.get_workspace(ws_id).await.unwrap();
        let url = find_browser_pane_url(&ws.surfaces[0].root_pane, sibling);
        assert!(
            matches!(url.as_deref(), Some("https://example.com")),
            "sibling should contain the added browser tab"
        );

        let Pane::Split { first, second, .. } = &ws.surfaces[0].root_pane else {
            panic!("expected split after split_pane")
        };
        let leaf_for = |pane_id: PaneId| -> &Pane {
            for candidate in [first.as_ref(), second.as_ref()] {
                if let Pane::Leaf { id, .. } = candidate {
                    if *id == pane_id {
                        return candidate;
                    }
                }
            }
            panic!("pane {pane_id} not found in split tree")
        };
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, active },
            ..
        } = leaf_for(sibling)
        else {
            panic!("sibling pane should be tabbed leaf")
        };
        assert_eq!(*active, browser_surface);
        assert_eq!(surfaces.len(), 2);

        let Pane::Leaf {
            content:
                PaneContent::Tabs {
                    surfaces: orig_surfaces,
                    ..
                },
            ..
        } = leaf_for(original)
        else {
            panic!("original pane should be tabbed leaf")
        };
        assert_eq!(orig_surfaces.len(), 1, "original pane untouched");
    }

    /// Case: add a browser tab to pane A, then add a browser tab to pane B. A's
    /// existing browser surface must preserve id, title, and initial_url. GTK
    /// rerender previously recreated BrowserPane and returned to about:blank,
    /// but daemon state itself should never change, so lock that invariant here.
    /// If add_browser_surface_to_pane regresses and damages another pane, this
    /// catches it.
    #[tokio::test]
    async fn add_browser_to_one_pane_keeps_other_pane_browser_intact() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane_a = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let (_, pane_b) = store
            .split_pane(pane_a, SplitDirection::Vertical)
            .await
            .unwrap();

        // Add an https browser tab to pane A. A hypothetical user-navigated URL
        // lives in the GTK webview while state keeps only initial_url, so verify
        // the newly added surface metadata is preserved.
        let (_, browser_a) = store
            .add_browser_surface_to_pane(pane_a, "https://docs.a.test".into())
            .await
            .unwrap();
        let snap_before = store.get_workspace(ws_id).await.unwrap();
        let surfaces_a_before = pane_surfaces(&snap_before, pane_a);
        let surfaces_b_before = pane_surfaces(&snap_before, pane_b);

        // Add an about:blank browser tab to pane B.
        let (_, browser_b) = store
            .add_browser_surface_to_pane(pane_b, "about:blank".into())
            .await
            .unwrap();
        assert_ne!(browser_a, browser_b);

        let snap_after = store.get_workspace(ws_id).await.unwrap();
        let surfaces_a_after = pane_surfaces(&snap_after, pane_a);
        let surfaces_b_after = pane_surfaces(&snap_after, pane_b);

        // Pane A's surface list keeps the same idx, id, title, and kind.
        assert_eq!(
            fingerprints(&surfaces_a_before),
            fingerprints(&surfaces_a_after),
            "pane A surfaces must not change when pane B gets a new browser tab"
        );
        // Pane B should have exactly one new surface.
        assert_eq!(surfaces_b_before.len() + 1, surfaces_b_after.len());
        assert!(surfaces_b_after
            .iter()
            .any(|s| s.id == browser_b
                && matches!(&s.kind, SurfaceKind::Browser { initial_url: Some(u) } if u == "about:blank")));
    }

    /// Case: adding multiple browser tabs to the same pane preserves metadata
    /// for earlier surfaces and activates the newly added tab.
    #[tokio::test]
    async fn appending_browser_tabs_preserves_earlier_tabs_in_same_pane() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let (_, first_browser) = store
            .add_browser_surface_to_pane(pane, "https://one.test".into())
            .await
            .unwrap();
        let (_, second_browser) = store
            .add_browser_surface_to_pane(pane, "https://two.test".into())
            .await
            .unwrap();
        let (_, third_browser) = store
            .add_browser_surface_to_pane(pane, "https://three.test".into())
            .await
            .unwrap();

        let ws = store.get_workspace(ws_id).await.unwrap();
        let surfaces = pane_surfaces(&ws, pane);
        assert_eq!(surfaces.len(), 4); // initial terminal + 3 browsers
        let by_id: std::collections::HashMap<_, _> =
            surfaces.iter().map(|s| (s.id, s.clone())).collect();
        for (id, expected_url) in [
            (first_browser, "https://one.test"),
            (second_browser, "https://two.test"),
            (third_browser, "https://three.test"),
        ] {
            let s = by_id.get(&id).expect("browser surface must still exist");
            assert!(matches!(
                &s.kind,
                SurfaceKind::Browser { initial_url: Some(u) } if u == expected_url
            ));
        }
        // The most recently added tab should be the active surface.
        assert_eq!(first_pane_active_surface(&ws), third_browser);
    }

    /// Case: adding a browser tab must not touch surfaces in another workspace.
    #[tokio::test]
    async fn adding_browser_in_one_workspace_does_not_touch_other_workspaces() {
        let store = StateStore::new_lazy(State::default());
        let alpha = store
            .create_workspace(Some("alpha".into()), std::path::PathBuf::from("/tmp/alpha"))
            .await;
        let beta = store
            .create_workspace(Some("beta".into()), std::path::PathBuf::from("/tmp/beta"))
            .await;
        let pane_alpha = first_pane(&store.get_workspace(alpha).await.unwrap());
        let pane_beta = first_pane(&store.get_workspace(beta).await.unwrap());

        let beta_before = pane_surfaces(&store.get_workspace(beta).await.unwrap(), pane_beta);
        let _ = store
            .add_browser_surface_to_pane(pane_alpha, "https://alpha-only.test".into())
            .await
            .unwrap();
        let beta_after = pane_surfaces(&store.get_workspace(beta).await.unwrap(), pane_beta);
        assert_eq!(fingerprints(&beta_before), fingerprints(&beta_after));
    }

    /// Case: adding a new terminal tab to another pane preserves terminal surface
    /// metadata, especially cwd, in the existing pane.
    #[tokio::test]
    async fn adding_terminal_tab_to_other_pane_keeps_existing_terminal_cwd() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane_a = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let (_, pane_b) = store
            .split_pane(pane_a, SplitDirection::Vertical)
            .await
            .unwrap();

        let surface_a_id = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        // Simulate the user running cd in the shell by updating pane A terminal cwd.
        assert_eq!(
            store
                .update_surface_cwd(pane_a, surface_a_id, "/tmp/work/inner".into())
                .await,
            Some(ws_id)
        );

        // Now add a new terminal tab to pane B.
        let (_, _new_term) = store
            .add_terminal_surface_to_pane(pane_b, Some("/tmp/other".into()))
            .await
            .unwrap();

        // Pane A's surface should keep cwd /tmp/work/inner.
        let ws = store.get_workspace(ws_id).await.unwrap();
        let surfaces_a = pane_surfaces(&ws, pane_a);
        let s_a = surfaces_a
            .iter()
            .find(|s| s.id == surface_a_id)
            .expect("pane A's terminal surface must still exist");
        assert!(matches!(
            &s_a.kind,
            SurfaceKind::Terminal { cwd: Some(cwd), .. }
                if cwd == &std::path::PathBuf::from("/tmp/work/inner")
        ));
    }

    fn pane_surfaces(ws: &Workspace, pane: PaneId) -> Vec<PaneSurface> {
        fn walk(p: &Pane, target: PaneId) -> Option<Vec<PaneSurface>> {
            match p {
                Pane::Leaf { id, content } if *id == target => match content {
                    PaneContent::Tabs { surfaces, .. } => Some(surfaces.clone()),
                    PaneContent::Terminal { .. } | PaneContent::Browser { .. } => Some(vec![]),
                },
                Pane::Leaf { .. } => None,
                Pane::Split { first, second, .. } => {
                    walk(first, target).or_else(|| walk(second, target))
                }
            }
        }
        ws.surfaces
            .iter()
            .find_map(|s| walk(&s.root_pane, pane))
            .unwrap_or_default()
    }

    /// Browser navigation updates a surface's initial_url via update_browser_url,
    /// allowing the next launch to restore the same page. Also verify terminal
    /// surfaces and wrong (pane, surface) pairs are unaffected.
    #[tokio::test]
    async fn update_browser_url_persists_last_navigation_only_for_browser() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let (_, browser) = store
            .add_browser_surface_to_pane(pane, "https://one.test".into())
            .await
            .unwrap();

        // navigate -> reflected in state.
        assert_eq!(
            store
                .update_browser_url(pane, browser, "https://two.test/page?x=1".into())
                .await,
            Some(ws_id)
        );
        let ws = store.get_workspace(ws_id).await.unwrap();
        let updated = ws.surfaces[0]
            .root_pane
            .find_surface(pane, browser)
            .unwrap();
        assert!(matches!(
            &updated.kind,
            SurfaceKind::Browser { initial_url: Some(u) } if u == "https://two.test/page?x=1"
        ));

        // Same URL returns None as a no-op.
        assert_eq!(
            store
                .update_browser_url(pane, browser, "https://two.test/page?x=1".into())
                .await,
            None
        );

        // Terminal surface, the first active surface, is unaffected.
        let terminal_id = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        // Active may be browser, so find the terminal id explicitly.
        let ws = store.get_workspace(ws_id).await.unwrap();
        let terminal_id = match &ws.surfaces[0].root_pane {
            Pane::Leaf {
                content: PaneContent::Tabs { surfaces, .. },
                ..
            } => surfaces
                .iter()
                .find(|s| matches!(s.kind, SurfaceKind::Terminal { .. }))
                .map(|s| s.id)
                .unwrap(),
            _ => terminal_id,
        };
        assert_eq!(
            store
                .update_browser_url(pane, terminal_id, "https://nope.test".into())
                .await,
            None
        );
    }

    /// Browser page title signals automatically update surface.title. Surfaces
    /// locked by user rename do not update automatically.
    #[tokio::test]
    async fn update_surface_auto_title_respects_user_rename() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let (_, browser_a) = store
            .add_browser_surface_to_pane(pane, "https://a.test".into())
            .await
            .unwrap();
        let (_, browser_b) = store
            .add_browser_surface_to_pane(pane, "https://b.test".into())
            .await
            .unwrap();

        // A's page title arrives -> updated.
        assert_eq!(
            store
                .update_surface_auto_title(pane, browser_a, "Example A — Home".into())
                .await,
            Some(ws_id)
        );
        assert_eq!(
            store.surface_title(pane, browser_a).await.as_deref(),
            Some("Example A — Home")
        );

        // User names B directly -> automatic updates are ignored.
        store
            .rename_surface(pane, browser_b, "Pinned".into())
            .await
            .unwrap();
        assert_eq!(
            store
                .update_surface_auto_title(pane, browser_b, "B Page".into())
                .await,
            None
        );
        assert_eq!(
            store.surface_title(pane, browser_b).await.as_deref(),
            Some("Pinned")
        );

        // Empty title is ignored.
        assert_eq!(
            store
                .update_surface_auto_title(pane, browser_a, "   ".into())
                .await,
            None
        );
    }

    /// Updating a browser URL in another pane of another workspace must not
    /// change the first workspace's surface data.
    #[tokio::test]
    async fn update_browser_url_in_one_workspace_does_not_touch_others() {
        let store = StateStore::new_lazy(State::default());
        let alpha = store
            .create_workspace(Some("alpha".into()), std::path::PathBuf::from("/tmp/alpha"))
            .await;
        let beta = store
            .create_workspace(Some("beta".into()), std::path::PathBuf::from("/tmp/beta"))
            .await;
        let pane_alpha = first_pane(&store.get_workspace(alpha).await.unwrap());
        let pane_beta = first_pane(&store.get_workspace(beta).await.unwrap());

        let (_, alpha_browser) = store
            .add_browser_surface_to_pane(pane_alpha, "https://alpha.test".into())
            .await
            .unwrap();
        let (_, beta_browser) = store
            .add_browser_surface_to_pane(pane_beta, "https://beta.test".into())
            .await
            .unwrap();

        let _ = store
            .update_browser_url(pane_alpha, alpha_browser, "https://alpha.test/2".into())
            .await;

        let beta_surfaces = pane_surfaces(&store.get_workspace(beta).await.unwrap(), pane_beta);
        let beta_b = beta_surfaces.iter().find(|s| s.id == beta_browser).unwrap();
        assert!(matches!(
            &beta_b.kind,
            SurfaceKind::Browser { initial_url: Some(u) } if u == "https://beta.test"
        ));
    }

    /// `PaneSurface` comes from another crate and does not implement PartialEq.
    /// Extract only the key fields needed for unit-test preservation checks.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SurfaceFingerprint {
        id: SurfaceId,
        title: String,
        title_locked: bool,
        kind: SurfaceKindFingerprint,
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum SurfaceKindFingerprint {
        Terminal {
            shell: Option<String>,
            cwd: Option<std::path::PathBuf>,
        },
        Browser {
            initial_url: Option<String>,
        },
        Editor {
            workspace_root: std::path::PathBuf,
            open_files: Vec<std::path::PathBuf>,
            active_file: Option<std::path::PathBuf>,
        },
    }

    fn fingerprint(s: &PaneSurface) -> SurfaceFingerprint {
        let kind = match &s.kind {
            SurfaceKind::Terminal { shell, cwd } => SurfaceKindFingerprint::Terminal {
                shell: shell.clone(),
                cwd: cwd.clone(),
            },
            SurfaceKind::Browser { initial_url } => SurfaceKindFingerprint::Browser {
                initial_url: initial_url.clone(),
            },
            SurfaceKind::Editor {
                workspace_root,
                session,
            } => SurfaceKindFingerprint::Editor {
                workspace_root: workspace_root.clone(),
                open_files: session
                    .open_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect(),
                active_file: session.active_file.clone(),
            },
        };
        SurfaceFingerprint {
            id: s.id,
            title: s.title.clone(),
            title_locked: s.title_locked,
            kind,
        }
    }

    fn fingerprints(surfaces: &[PaneSurface]) -> Vec<SurfaceFingerprint> {
        surfaces.iter().map(fingerprint).collect()
    }

    async fn create_named_workspace(store: &StateStore, name: &str) -> WorkspaceId {
        store
            .create_workspace(
                Some(name.into()),
                std::path::PathBuf::from("/tmp").join(name),
            )
            .await
    }

    #[tokio::test]
    async fn normalization_repairs_workspace_order_and_stale_active_id() {
        let seed = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&seed, "a").await;
        let b = create_named_workspace(&seed, "b").await;
        let c = create_named_workspace(&seed, "c").await;
        let stale = WorkspaceId::new();
        let mut state = seed.snapshot().await;
        let first_workspace = state.workspaces.iter_mut().find(|ws| ws.id == a).unwrap();
        let first_pane = first_workspace.surfaces[0]
            .root_pane
            .first_leaf_id()
            .unwrap();
        let first_surface = first_workspace.surfaces[0]
            .root_pane
            .active_surface_id(first_pane)
            .unwrap();
        assert!(first_workspace.surfaces[0]
            .root_pane
            .set_surface_scrollback(first_pane, first_surface, "saved terminal history".into(),));
        state.workspace_order = vec![c, stale, c];
        state.active_workspace = Some(stale);

        let store = StateStore::new_lazy(state);
        let snapshot = store.snapshot().await;
        assert_eq!(snapshot.workspace_order, vec![c, a, b]);
        assert_eq!(snapshot.active_workspace, Some(c));
        assert_eq!(store.active_workspace().await, Some(c));
        assert_eq!(
            store.workspace_order_and_active().await,
            (vec![c, a, b], Some(c))
        );
        assert_eq!(store.list_workspaces().await, vec![c, a, b]);
        let ordered = store.ordered_workspaces().await;
        assert_eq!(
            ordered.iter().map(|ws| ws.id).collect::<Vec<_>>(),
            vec![c, a, b]
        );
        assert!(ordered
            .iter()
            .find(|workspace| workspace.id == a)
            .unwrap()
            .surfaces[0]
            .root_pane
            .find_surface(first_pane, first_surface)
            .unwrap()
            .scrollback
            .is_none());
    }

    #[tokio::test]
    async fn reorder_workspace_moves_first_to_last() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;
        let b = create_named_workspace(&store, "b").await;
        let c = create_named_workspace(&store, "c").await;

        assert!(store.reorder_workspace(a, 2).await);

        let order = store.snapshot().await.workspace_order;
        assert_eq!(order, vec![b, c, a]);
        assert_eq!(store.list_workspaces().await, vec![b, c, a]);
        assert_eq!(
            store
                .ordered_workspaces()
                .await
                .iter()
                .map(|ws| ws.id)
                .collect::<Vec<_>>(),
            vec![b, c, a]
        );
    }

    #[tokio::test]
    async fn reordered_workspace_order_survives_state_file_roundtrip() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;
        let b = create_named_workspace(&store, "b").await;
        let c = create_named_workspace(&store, "c").await;
        assert!(store.reorder_workspace(c, 0).await);

        let path = std::env::temp_dir().join(format!("flowmux-order-{a}.json"));
        flowmux_state::save_to(&path, &store.snapshot().await).unwrap();
        let restored = StateStore::new_lazy(flowmux_state::load_from(&path).unwrap());
        let _ = std::fs::remove_file(&path);

        assert_eq!(restored.list_workspaces().await, vec![c, a, b]);
        assert_eq!(restored.snapshot().await.workspace_order, vec![c, a, b]);
    }

    #[tokio::test]
    async fn reorder_workspace_moves_last_to_first() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;
        let b = create_named_workspace(&store, "b").await;
        let c = create_named_workspace(&store, "c").await;

        assert!(store.reorder_workspace(c, 0).await);

        let order = store.snapshot().await.workspace_order;
        assert_eq!(order, vec![c, a, b]);
    }

    #[tokio::test]
    async fn reorder_workspace_moves_middle_within_range() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;
        let b = create_named_workspace(&store, "b").await;
        let c = create_named_workspace(&store, "c").await;
        let d = create_named_workspace(&store, "d").await;

        // Move b to the end (a, c, d, b).
        assert!(store.reorder_workspace(b, 3).await);
        assert_eq!(store.snapshot().await.workspace_order, vec![a, c, d, b]);

        // Move d to the front (d, a, c, b).
        assert!(store.reorder_workspace(d, 0).await);
        assert_eq!(store.snapshot().await.workspace_order, vec![d, a, c, b]);
    }

    #[tokio::test]
    async fn reorder_workspace_target_beyond_len_clamps_to_end() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;
        let b = create_named_workspace(&store, "b").await;
        let c = create_named_workspace(&store, "c").await;

        // Even 100 should only move to the end.
        assert!(store.reorder_workspace(a, 100).await);

        let order = store.snapshot().await.workspace_order;
        assert_eq!(order, vec![b, c, a]);
    }

    #[tokio::test]
    async fn reorder_workspace_no_change_returns_false() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;
        let b = create_named_workspace(&store, "b").await;
        let c = create_named_workspace(&store, "c").await;

        // Move to its own position.
        assert!(!store.reorder_workspace(b, 1).await);
        assert_eq!(store.snapshot().await.workspace_order, vec![a, b, c]);

        // An out-of-range index that clamps to its own end position returns false.
        assert!(!store.reorder_workspace(c, 100).await);
        assert_eq!(store.snapshot().await.workspace_order, vec![a, b, c]);
    }

    #[tokio::test]
    async fn reorder_workspace_unknown_id_returns_false() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;
        let b = create_named_workspace(&store, "b").await;

        let missing = WorkspaceId::new();
        assert!(!store.reorder_workspace(missing, 0).await);
        assert_eq!(store.snapshot().await.workspace_order, vec![a, b]);
    }

    #[tokio::test]
    async fn reorder_workspace_single_channel_is_noop() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;

        assert!(!store.reorder_workspace(a, 0).await);
        assert!(!store.reorder_workspace(a, 5).await);
        assert_eq!(store.snapshot().await.workspace_order, vec![a]);
    }

    #[tokio::test]
    async fn reorder_workspace_empty_state_returns_false() {
        let store = StateStore::new_lazy(State::default());
        let any = WorkspaceId::new();
        assert!(!store.reorder_workspace(any, 0).await);
    }

    #[tokio::test]
    async fn reorder_workspace_does_not_change_active_workspace() {
        let store = StateStore::new_lazy(State::default());
        let a = create_named_workspace(&store, "a").await;
        let b = create_named_workspace(&store, "b").await;
        let _c = create_named_workspace(&store, "c").await;

        // Active starts as the first-created a.
        assert_eq!(store.snapshot().await.active_workspace, Some(a));

        // Moving a to the end should leave active as a.
        assert!(store.reorder_workspace(a, 2).await);
        assert_eq!(store.snapshot().await.active_workspace, Some(a));

        // Order is now [b, c, a]. Moving b to the end still leaves active as a.
        assert!(store.reorder_workspace(b, 2).await);
        assert_eq!(store.snapshot().await.active_workspace, Some(a));
    }

    /// Integrated test for pane-internal terminal/browser tab reorder:
    /// 1. returns the workspace_id in the normal case,
    /// 2. returns None for same-position or missing surfaces,
    /// 3. keeps the active tab on the same SurfaceId after moving.
    #[tokio::test]
    async fn reorder_surface_in_pane_moves_tab_and_keeps_active() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("ws".into()), std::path::PathBuf::from("/tmp"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
        let first = ws.surfaces[0].root_pane.active_surface_id(pane).unwrap();

        // Add the second terminal and third browser tab.
        let (_, second) = store
            .add_terminal_surface_to_pane(pane, Some("/tmp/two".into()))
            .await
            .unwrap();
        let (_, third) = store
            .add_browser_surface_to_pane(pane, "https://three.test".into())
            .await
            .unwrap();
        // Restore the active tab to first.
        store.set_active_surface(pane, first).await;

        // Move first to the last position.
        assert_eq!(
            store.reorder_surface_in_pane(pane, first, 2).await,
            Some(ws_id)
        );
        let snap = store.get_workspace(ws_id).await.unwrap();
        let flowmux_core::Pane::Leaf {
            content: flowmux_core::PaneContent::Tabs { active, surfaces },
            ..
        } = &snap.surfaces[0].root_pane
        else {
            panic!("expected tabs")
        };
        assert_eq!(
            surfaces.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![second, third, first]
        );
        // first moved but remains active.
        assert_eq!(*active, first);

        // Moving to the same end position again returns None.
        assert!(store
            .reorder_surface_in_pane(pane, first, 2)
            .await
            .is_none());

        // Missing SurfaceId returns None.
        assert!(store
            .reorder_surface_in_pane(pane, SurfaceId::new(), 0)
            .await
            .is_none());
    }

    /// target_index beyond length clamps to the end, safely handling callers
    /// that pass drop-position + 1 indexes.
    #[tokio::test]
    async fn reorder_surface_in_pane_clamps_target_index_beyond_len() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("ws".into()), std::path::PathBuf::from("/tmp"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
        let first = ws.surfaces[0].root_pane.active_surface_id(pane).unwrap();
        let (_, second) = store
            .add_terminal_surface_to_pane(pane, None)
            .await
            .unwrap();

        // Move first to 999 -> clamp to the end, index 1.
        assert_eq!(
            store.reorder_surface_in_pane(pane, first, 999).await,
            Some(ws_id)
        );
        let snap = store.get_workspace(ws_id).await.unwrap();
        let flowmux_core::Pane::Leaf {
            content: flowmux_core::PaneContent::Tabs { surfaces, .. },
            ..
        } = &snap.surfaces[0].root_pane
        else {
            panic!("expected tabs")
        };
        assert_eq!(
            surfaces.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![second, first]
        );
    }

    /// Reorder inside one channel/workspace must not affect another channel.
    #[tokio::test]
    async fn reorder_surface_in_pane_does_not_touch_other_workspaces() {
        let store = StateStore::new_lazy(State::default());
        let alpha = store
            .create_workspace(Some("alpha".into()), std::path::PathBuf::from("/tmp/alpha"))
            .await;
        let beta = store
            .create_workspace(Some("beta".into()), std::path::PathBuf::from("/tmp/beta"))
            .await;

        let ws_alpha = store.get_workspace(alpha).await.unwrap();
        let alpha_pane = ws_alpha.surfaces[0].root_pane.first_leaf_id().unwrap();
        let alpha_first = ws_alpha.surfaces[0]
            .root_pane
            .active_surface_id(alpha_pane)
            .unwrap();
        let (_, alpha_second) = store
            .add_terminal_surface_to_pane(alpha_pane, None)
            .await
            .unwrap();

        let ws_beta = store.get_workspace(beta).await.unwrap();
        let beta_pane = ws_beta.surfaces[0].root_pane.first_leaf_id().unwrap();
        let beta_first = ws_beta.surfaces[0]
            .root_pane
            .active_surface_id(beta_pane)
            .unwrap();
        let (_, beta_second) = store
            .add_terminal_surface_to_pane(beta_pane, None)
            .await
            .unwrap();

        // Move alpha pane's first tab to the end.
        assert_eq!(
            store
                .reorder_surface_in_pane(alpha_pane, alpha_first, 1)
                .await,
            Some(alpha)
        );

        // beta stays unchanged.
        let snap_beta = store.get_workspace(beta).await.unwrap();
        let flowmux_core::Pane::Leaf {
            content: flowmux_core::PaneContent::Tabs { surfaces, .. },
            ..
        } = &snap_beta.surfaces[0].root_pane
        else {
            panic!("expected tabs")
        };
        assert_eq!(
            surfaces.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![beta_first, beta_second]
        );
        // alpha was swapped.
        let snap_alpha = store.get_workspace(alpha).await.unwrap();
        let flowmux_core::Pane::Leaf {
            content: flowmux_core::PaneContent::Tabs { surfaces, .. },
            ..
        } = &snap_alpha.surfaces[0].root_pane
        else {
            panic!("expected tabs")
        };
        assert_eq!(
            surfaces.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![alpha_second, alpha_first]
        );
    }

    /// Window size and sidebar position setters are blocking, so call them with
    /// spawn_blocking inside a separate tokio runtime and verify there is no
    /// in-memory mutex conflict. Also verify semantic idempotence: repeating the
    /// same value does not trigger mark_dirty.
    #[tokio::test]
    async fn window_layout_setter_persists_value() {
        let store = StateStore::new_lazy(State::default());
        let store_for_blocking = store.clone();
        tokio::task::spawn_blocking(move || {
            store_for_blocking.set_window_layout_blocking(WindowLayout {
                width: 1440,
                height: 900,
                maximized: false,
            });
        })
        .await
        .unwrap();

        let snap = store.snapshot().await;
        assert_eq!(
            snap.window,
            Some(WindowLayout {
                width: 1440,
                height: 900,
                maximized: false,
            })
        );
    }

    #[tokio::test]
    async fn sidebar_position_setter_persists_value() {
        let store = StateStore::new_lazy(State::default());
        let store_for_blocking = store.clone();
        tokio::task::spawn_blocking(move || {
            store_for_blocking.set_sidebar_position_blocking(280);
        })
        .await
        .unwrap();
        assert_eq!(store.snapshot().await.sidebar_position, Some(280));
    }

    /// Pane split ratio setter scenario: normal case, split id missing from the
    /// tree, and same-ratio no-op.
    #[tokio::test]
    async fn pane_split_ratio_setter_updates_only_matching_split() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());
        // split_pane creates a new Split node whose PaneId is the root_pane of
        // the first surface in the workspace tree.
        let _ = store
            .split_pane(original, SplitDirection::Vertical)
            .await
            .unwrap();
        let ws = store.get_workspace(ws_id).await.unwrap();
        let split_id = match &ws.surfaces[0].root_pane {
            Pane::Split { id, .. } => *id,
            _ => panic!("expected split"),
        };

        let store_for_blocking = store.clone();
        let updated = tokio::task::spawn_blocking(move || {
            store_for_blocking.set_pane_split_ratio_blocking(split_id, 0.7)
        })
        .await
        .unwrap();
        assert!(updated);

        let ws = store.get_workspace(ws_id).await.unwrap();
        let Pane::Split { ratio, .. } = &ws.surfaces[0].root_pane else {
            unreachable!()
        };
        assert!((ratio - 0.7).abs() < 0.001);

        // Calling the same ratio again -> false.
        let store_for_blocking = store.clone();
        let again = tokio::task::spawn_blocking(move || {
            store_for_blocking.set_pane_split_ratio_blocking(split_id, 0.7)
        })
        .await
        .unwrap();
        assert!(!again);

        // Unknown split id -> false, tree unchanged.
        let store_for_blocking = store.clone();
        let unknown = tokio::task::spawn_blocking(move || {
            store_for_blocking.set_pane_split_ratio_blocking(PaneId::new(), 0.3)
        })
        .await
        .unwrap();
        assert!(!unknown);
    }

    /// set_workspace_name is the setter the GTK side uses to write the focused
    /// pane's active surface title explicitly into ws.name. Repeating the same
    /// value returns false (no-op).
    /// false (no-op).
    #[tokio::test]
    async fn set_workspace_name_updates_only_on_change() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("auto".into()), std::path::PathBuf::from("/tmp/auto"))
            .await;

        // Different value returns true.
        assert!(store.set_workspace_name(ws_id, "Claude Code".into()).await);
        assert_eq!(
            store.get_workspace(ws_id).await.unwrap().name,
            "Claude Code"
        );

        // Same value returns false.
        assert!(!store.set_workspace_name(ws_id, "Claude Code".into()).await);

        // Unknown workspace returns false.
        assert!(
            !store
                .set_workspace_name(WorkspaceId::new(), "ignored".into())
                .await
        );

        // Even with custom_title locked, set_workspace_name updates only ws.name.
        // custom_title stays as-is, and display_title gives custom priority.
        store.rename_workspace(ws_id, "MyName".into()).await;
        store.set_workspace_name(ws_id, "Updated Auto".into()).await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.name, "Updated Auto");
        assert_eq!(ws.custom_title.as_deref(), Some("MyName"));
        assert_eq!(ws.display_title(), "MyName");
    }

    /// Automatic synchronization is not the daemon's responsibility: surface
    /// updates touch only the surface, and ws.name changes only when GTK knows
    /// focus information and calls set_workspace_name. Regression guard.
    #[tokio::test]
    async fn surface_auto_title_does_not_touch_workspace_name() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("auto".into()), std::path::PathBuf::from("/tmp/auto"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);
        let active = first_pane_active_surface(&ws);

        store
            .update_surface_auto_title(pane, active, "Claude Code".into())
            .await;

        let ws = store.get_workspace(ws_id).await.unwrap();
        // Surface label updates, but ws.name stays unchanged until GTK calls
        // set_workspace_name.
        assert_eq!(ws.name, "auto");
    }

    /// Likewise, cwd changes do not let the daemon mutate ws.name by itself.
    #[tokio::test]
    async fn surface_cwd_does_not_touch_workspace_name() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(
                Some("origin".into()),
                std::path::PathBuf::from("/tmp/origin"),
            )
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        let pane = first_pane(&ws);
        let active = first_pane_active_surface(&ws);

        store
            .update_surface_cwd(pane, active, std::path::PathBuf::from("/tmp/elsewhere"))
            .await;
        let ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(ws.name, "origin");
    }

    // ----- right-sibling browser reuse (Phase 2) ----------------------

    /// Workspace with a single terminal pane → no right sibling exists,
    /// so `find_right_sibling_browser_leaf` must return `None`. This is
    /// the "first call" leg of the reuse-vs-split decision.
    #[tokio::test]
    async fn right_sibling_lookup_returns_none_for_unsplit_workspace() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let term = first_pane(&store.get_workspace(ws_id).await.unwrap());

        assert_eq!(store.find_right_sibling_browser_leaf(term).await, None);
    }

    /// After `flowmux browser open` once, the workspace looks like
    /// `term | browser`. The next call from the *terminal* pane must
    /// detect the browser as its right sibling.
    #[tokio::test]
    async fn right_sibling_lookup_finds_existing_browser_pane() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let term = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let (_, browser_pane) = store
            .split_pane_with_browser(term, SplitDirection::Vertical, "https://x".into())
            .await
            .expect("split should succeed");

        let found = store.find_right_sibling_browser_leaf(term).await;
        assert_eq!(found, Some(browser_pane));
    }

    /// Two-call scenario: first call hits split path, second call hits
    /// reuse path and adds a tab to the existing browser leaf.
    #[tokio::test]
    async fn two_browser_open_calls_first_splits_then_reuses_right_sibling() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let term = first_pane(&store.get_workspace(ws_id).await.unwrap());

        // First call: no right sibling → split path.
        assert!(store.find_right_sibling_browser_leaf(term).await.is_none());
        let (_, browser_pane) = store
            .split_pane_with_browser(term, SplitDirection::Vertical, "https://a".into())
            .await
            .unwrap();

        // Second call: reuse path. Daemon would call
        // add_browser_surface_to_pane on the right-sibling leaf.
        let reuse = store
            .find_right_sibling_browser_leaf(term)
            .await
            .expect("right sibling must exist after first split");
        assert_eq!(reuse, browser_pane);

        let added = store
            .add_browser_surface_to_pane(reuse, "https://b".into())
            .await;
        assert!(added.is_some(), "second URL should append a tab");

        // The browser pane now hosts two surface tabs (initial + new).
        let ws = store.get_workspace(ws_id).await.unwrap();
        let leaf_tabs = ws.surfaces[0]
            .root_pane
            .find_right_sibling_browser_leaf(term)
            .and_then(|p| {
                fn count_tabs(node: &Pane, target: PaneId) -> Option<usize> {
                    match node {
                        Pane::Leaf { id, content } if *id == target => match content {
                            PaneContent::Tabs { surfaces, .. } => Some(surfaces.len()),
                            _ => None,
                        },
                        Pane::Leaf { .. } => None,
                        Pane::Split { first, second, .. } => {
                            count_tabs(first, target).or_else(|| count_tabs(second, target))
                        }
                    }
                }
                count_tabs(&ws.surfaces[0].root_pane, p)
            });
        assert_eq!(leaf_tabs, Some(2));
    }

    #[tokio::test]
    async fn ephemeral_store_reports_persistence_disabled() {
        let store = StateStore::new_lazy_ephemeral(State::default());
        assert!(
            !store.persist_enabled(),
            "ephemeral stores must not persist to disk"
        );
        // save_now is a no-op on ephemeral stores: it returns Ok
        // without touching the on-disk state.json shared by the
        // lock-owning instance.
        assert!(store.save_now().await.is_ok());
        assert!(store.save_now_blocking().is_ok());

        let normal = StateStore::new_lazy(State::default());
        assert!(
            normal.persist_enabled(),
            "default constructor should persist"
        );
    }

    /// An ephemeral store still accepts mutations and lets them flow
    /// through `mark_dirty` so the rest of the daemon code path stays
    /// uniform — only the disk write is suppressed.
    #[tokio::test]
    async fn ephemeral_store_accepts_mutations_in_memory() {
        let store = StateStore::new_lazy_ephemeral(State::default());
        let id = store
            .create_workspace(Some("ghost".into()), std::path::PathBuf::from("/tmp/ghost"))
            .await;
        let snap = store.snapshot().await;
        assert_eq!(snap.workspaces.len(), 1);
        assert_eq!(snap.workspaces[0].id, id);
    }

    #[tokio::test]
    async fn persistence_debounce_waits_for_the_last_dirty_generation() {
        let store = StateStore::new_lazy_ephemeral(State::default());
        store.mark_dirty();
        let waiter = {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .wait_for_stable_dirty_generation(0, Duration::from_millis(20))
                    .await
            })
        };

        tokio::time::sleep(Duration::from_millis(10)).await;
        store.mark_dirty();
        tokio::time::sleep(Duration::from_millis(10)).await;
        store.mark_dirty();

        assert_eq!(waiter.await.unwrap(), 3);
    }

    #[tokio::test]
    async fn persistence_debounce_ignores_stale_notify_permits() {
        let store = StateStore::new_lazy_ephemeral(State::default());
        store.mark_dirty();
        store.mark_dirty();
        let persisted = store
            .wait_for_stable_dirty_generation(0, Duration::from_millis(5))
            .await;
        assert_eq!(persisted, 2);

        let no_new_generation = tokio::time::timeout(
            Duration::from_millis(20),
            store.wait_for_stable_dirty_generation(persisted, Duration::from_millis(5)),
        )
        .await;
        assert!(no_new_generation.is_err());
    }

    // --- Title-prefix fallback resolver -----------------------------
    //
    // Pins the Flatpak hook recovery path: when a Notify arrives with
    // `pane=None surface=None` because the host->sandbox transition
    // stripped FLOWMUX_PANE_ID, the daemon must rebuild the routing
    // context by matching the notification title against the pane's
    // active tab title (which flowmux flips to the agent name as
    // soon as the agent attaches its PTY).

    #[tokio::test]
    async fn title_prefix_resolver_finds_pane_after_rename() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        // workspace_view::terminal_title_notify renames the active
        // surface to the agent name. Re-create that pre-condition here.
        assert_eq!(
            store.rename_surface(pane, surface, "OpenCode".into()).await,
            Some(ws_id)
        );

        let hit = store
            .find_pane_by_active_title_prefix("OpenCode")
            .await
            .expect("rename must make the pane discoverable by title");
        assert_eq!(hit, (ws_id, pane, surface));
    }

    #[tokio::test]
    async fn title_prefix_resolver_is_case_insensitive() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        store.rename_surface(pane, surface, "OPENCODE".into()).await;

        assert!(store
            .find_pane_by_active_title_prefix("opencode")
            .await
            .is_some());
        assert!(store
            .find_pane_by_active_title_prefix("OPENcode")
            .await
            .is_some());
    }

    #[tokio::test]
    async fn title_prefix_resolver_rejects_hyphenated_path_prefix() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        store
            .rename_surface(pane, surface, "opencode-anycli".into())
            .await;

        assert!(store
            .find_pane_by_active_title_prefix("OpenCode")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn screen_signal_rejects_hyphenated_path_title_as_idle_agent() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(
                Some("opencode-anycli".into()),
                std::path::PathBuf::from("/tmp/opencode-anycli"),
            )
            .await;
        let pane = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let surface = first_pane_active_surface(&store.get_workspace(ws_id).await.unwrap());
        store
            .rename_surface(pane, surface, "opencode-anycli".into())
            .await;

        assert!(store
            .report_agent_screen_signals(surface, None, Some("opencode-anycli"))
            .await
            .is_none());
        assert!(store
            .get_workspace(ws_id)
            .await
            .unwrap()
            .collect_agent_bar_items()
            .is_empty());
    }

    #[tokio::test]
    async fn title_prefix_resolver_rejects_empty_needle() {
        let store = StateStore::new_lazy(State::default());
        let _ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        // A blank `title.split_whitespace().next()` must not collapse
        // into an everything-matches starts_with on every leaf, or the
        // daemon would attribute random Notifications to whatever pane
        // happens to be first in the workspace list.
        assert!(store.find_pane_by_active_title_prefix("").await.is_none());
    }

    #[tokio::test]
    async fn title_prefix_resolver_returns_none_when_no_pane_matches() {
        let store = StateStore::new_lazy(State::default());
        let _ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        // Title is the default "demo" — Notify("OpenCode ready")
        // must not match it, even after the lowercasing.
        assert!(store
            .find_pane_by_active_title_prefix("OpenCode")
            .await
            .is_none());
    }

    #[tokio::test]
    async fn title_prefix_resolver_walks_split_pane_trees() {
        // Split the workspace into two leaves, only one of which has
        // the agent attached. The resolver must walk into the split
        // tree (both halves) and pick the leaf whose active tab
        // matches.
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("demo".into()), std::path::PathBuf::from("/tmp/demo"))
            .await;
        let original = first_pane(&store.get_workspace(ws_id).await.unwrap());
        let (_, sibling) = store
            .split_pane(original, SplitDirection::Vertical)
            .await
            .expect("split must succeed");

        let original_surface = store.get_workspace(ws_id).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(original)
            .unwrap();
        let sibling_surface = store.get_workspace(ws_id).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(sibling)
            .unwrap();

        // Rename only the sibling pane's active tab. The resolver must
        // skip `original` and land on `sibling`.
        store
            .rename_surface(sibling, sibling_surface, "OpenCode".into())
            .await;
        let hit = store
            .find_pane_by_active_title_prefix("OpenCode")
            .await
            .unwrap();
        assert_eq!(hit, (ws_id, sibling, sibling_surface));

        // Sanity: `original` is still a valid pane and its active
        // surface title is unchanged.
        let original_ws = store.get_workspace(ws_id).await.unwrap();
        assert_eq!(
            original_ws.surfaces[0]
                .root_pane
                .surface_title(original, original_surface),
            Some("demo")
        );
    }

    /// Horizontal split (split_down) does not place the browser to the
    /// right — so even though we created one, reuse must not pick it.
    #[tokio::test]
    async fn right_sibling_lookup_ignores_horizontally_split_browser() {
        let store = StateStore::new_lazy(State::default());
        let ws_id = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let term = first_pane(&store.get_workspace(ws_id).await.unwrap());

        let _ = store
            .split_pane_with_browser(term, SplitDirection::Horizontal, "https://x".into())
            .await
            .unwrap();

        assert!(store.find_right_sibling_browser_leaf(term).await.is_none());
    }

    // ---- move_surface_to_pane / move_surface_to_workspace ----

    async fn pane_tab_ids(store: &StateStore, ws: WorkspaceId, pane: PaneId) -> Vec<SurfaceId> {
        let w = store.get_workspace(ws).await.unwrap();
        for surface in &w.surfaces {
            if let Some(PaneContent::Tabs { surfaces, .. }) =
                surface.root_pane.find_leaf_content(pane)
            {
                return surfaces.iter().map(|s| s.id).collect();
            }
        }
        Vec::new()
    }

    #[tokio::test]
    async fn move_surface_to_pane_appends_to_other_pane_in_same_workspace() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let src = first_pane(&store.get_workspace(ws).await.unwrap());
        let (_, dst) = store
            .split_pane(src, SplitDirection::Vertical)
            .await
            .unwrap();
        // src now has 2 tabs.
        let (_, moved) = store.add_terminal_surface_to_pane(src, None).await.unwrap();

        let out = store
            .move_surface_to_pane(src, moved, dst, usize::MAX)
            .await
            .unwrap();
        assert_eq!(out.dst_workspace, ws);
        assert_eq!(out.src_workspace, ws);
        assert_eq!(out.surface, moved);
        assert_eq!(out.src_pane, src);
        assert_eq!(out.dst_pane, dst);
        assert_eq!(out.changed_workspaces(), vec![ws]);
        assert_eq!(out.changed_panes(), vec![src, dst]);
        assert_eq!(out.changed_surfaces(), [moved]);
        assert!(!out.src_pane_removed);
        assert!(!out.src_workspace_removed);

        let src_tabs = pane_tab_ids(&store, ws, src).await;
        let dst_tabs = pane_tab_ids(&store, ws, dst).await;
        assert!(!src_tabs.contains(&moved));
        assert_eq!(dst_tabs.last().copied(), Some(moved));
        assert_eq!(dst_tabs.len(), 2);
    }

    #[tokio::test]
    async fn move_surface_to_same_pane_keeps_singleton_pane_and_workspace() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let pane = first_pane(&store.get_workspace(ws).await.unwrap());
        let only = store.get_workspace(ws).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(pane)
            .unwrap();

        assert!(store
            .move_surface_to_pane(pane, only, pane, usize::MAX)
            .await
            .is_none());
        assert_eq!(pane_tab_ids(&store, ws, pane).await, vec![only]);
        assert!(store.get_workspace(ws).await.is_some());
    }

    #[tokio::test]
    async fn move_surface_within_same_pane_reorders_without_detaching() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let pane = first_pane(&store.get_workspace(ws).await.unwrap());
        let first = store.get_workspace(ws).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(pane)
            .unwrap();
        let (_, second) = store
            .add_terminal_surface_to_pane(pane, None)
            .await
            .unwrap();
        let (_, third) = store
            .add_terminal_surface_to_pane(pane, None)
            .await
            .unwrap();
        assert!(store.set_active_surface(pane, second).await.is_some());

        let out = store
            .move_surface_to_pane(pane, first, pane, 2)
            .await
            .expect("different same-pane index must reorder");

        assert_eq!(out.dst_workspace, ws);
        assert_eq!(out.src_workspace, ws);
        assert_eq!(out.src_pane, pane);
        assert_eq!(out.dst_pane, pane);
        assert_eq!(out.changed_panes(), vec![pane]);
        assert!(!out.src_pane_removed);
        assert!(!out.src_workspace_removed);
        assert_eq!(
            pane_tab_ids(&store, ws, pane).await,
            vec![second, third, first]
        );
        assert_eq!(
            store.get_workspace(ws).await.unwrap().surfaces[0]
                .root_pane
                .active_surface_id(pane),
            Some(second)
        );
    }

    #[tokio::test]
    async fn move_surface_to_non_tab_leaf_rejects_before_taking_source() {
        let store = StateStore::new_lazy(State::default());
        let src_ws = store
            .create_workspace(Some("src".into()), std::path::PathBuf::from("/tmp/src"))
            .await;
        let dst_ws = store
            .create_workspace(Some("dst".into()), std::path::PathBuf::from("/tmp/dst"))
            .await;
        let src = first_pane(&store.get_workspace(src_ws).await.unwrap());
        let dst = first_pane(&store.get_workspace(dst_ws).await.unwrap());
        let moved = store.get_workspace(src_ws).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(src)
            .unwrap();

        // Simulate an invariant-corrupt legacy destination after constructor
        // normalization. The move API must reject it before taking the source.
        {
            let mut state = store.inner.lock().await;
            let destination = state
                .workspaces
                .iter_mut()
                .find(|ws| ws.id == dst_ws)
                .unwrap();
            let Pane::Leaf { content, .. } = &mut destination.surfaces[0].root_pane else {
                panic!("expected destination root leaf")
            };
            *content = PaneContent::Terminal { pid: None };
        }

        assert!(store
            .move_surface_to_pane(src, moved, dst, 0)
            .await
            .is_none());
        assert_eq!(pane_tab_ids(&store, src_ws, src).await, vec![moved]);
        assert!(store.get_workspace(src_ws).await.is_some());
    }

    #[tokio::test]
    async fn move_surface_to_pane_inserts_at_index() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let src = first_pane(&store.get_workspace(ws).await.unwrap());
        let (_, dst) = store
            .split_pane(src, SplitDirection::Vertical)
            .await
            .unwrap();
        // dst gets a second tab so it has [d0, d1].
        store.add_terminal_surface_to_pane(dst, None).await.unwrap();
        let dst_before = pane_tab_ids(&store, ws, dst).await;
        let (_, moved) = store.add_terminal_surface_to_pane(src, None).await.unwrap();

        store
            .move_surface_to_pane(src, moved, dst, 1)
            .await
            .unwrap();

        let dst_after = pane_tab_ids(&store, ws, dst).await;
        assert_eq!(dst_after, vec![dst_before[0], moved, dst_before[1]]);
    }

    #[tokio::test]
    async fn move_surface_collapses_emptied_source_pane_but_keeps_workspace() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let keep = first_pane(&store.get_workspace(ws).await.unwrap());
        let (_, src) = store
            .split_pane(keep, SplitDirection::Vertical)
            .await
            .unwrap();
        let only = store.get_workspace(ws).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(src)
            .unwrap();

        let out = store
            .move_surface_to_pane(src, only, keep, usize::MAX)
            .await
            .unwrap();
        assert!(out.src_pane_removed);
        assert!(!out.src_workspace_removed);

        let ws_after = store.get_workspace(ws).await.unwrap();
        // src pane is gone; keep pane absorbed the surface (2 tabs).
        assert_eq!(pane_tab_ids(&store, ws, keep).await.len(), 2);
        assert_eq!(ws_after.surfaces[0].root_pane.first_leaf_id(), Some(keep));
    }

    #[tokio::test]
    async fn move_surface_to_workspace_appends_and_removes_empty_source_workspace() {
        let store = StateStore::new_lazy(State::default());
        let ws1 = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let ws2 = store
            .create_workspace(Some("two".into()), std::path::PathBuf::from("/tmp/two"))
            .await;
        let src = first_pane(&store.get_workspace(ws1).await.unwrap());
        let moved = store.get_workspace(ws1).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(src)
            .unwrap();
        let dst = first_pane(&store.get_workspace(ws2).await.unwrap());

        let out = store
            .move_surface_to_workspace(src, moved, ws2)
            .await
            .unwrap();
        assert_eq!(out.dst_workspace, ws2);
        assert_eq!(out.src_workspace, ws1);
        assert!(out.src_workspace_removed);

        assert!(store.get_workspace(ws1).await.is_none());
        let dst_tabs = pane_tab_ids(&store, ws2, dst).await;
        assert_eq!(dst_tabs.len(), 2);
        assert_eq!(dst_tabs.last().copied(), Some(moved));
    }

    #[tokio::test]
    async fn move_surface_missing_surface_returns_none() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let src = first_pane(&store.get_workspace(ws).await.unwrap());
        let (_, dst) = store
            .split_pane(src, SplitDirection::Vertical)
            .await
            .unwrap();
        assert!(store
            .move_surface_to_pane(src, SurfaceId::new(), dst, 0)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn move_surface_to_missing_dest_keeps_surface() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let src = first_pane(&store.get_workspace(ws).await.unwrap());
        let moved = store.get_workspace(ws).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(src)
            .unwrap();
        assert!(store
            .move_surface_to_pane(src, moved, PaneId::new(), 0)
            .await
            .is_none());
        // Surface still present in the source pane.
        assert!(pane_tab_ids(&store, ws, src).await.contains(&moved));
    }

    #[tokio::test]
    async fn move_surface_to_pane_in_other_workspace_reassigns_dst_workspace() {
        // The destination location captured before the take must still point at
        // the other workspace's pane when the surface is inserted.
        let store = StateStore::new_lazy(State::default());
        let ws1 = store
            .create_workspace(Some("one".into()), std::path::PathBuf::from("/tmp/one"))
            .await;
        let ws2 = store
            .create_workspace(Some("two".into()), std::path::PathBuf::from("/tmp/two"))
            .await;
        let src = first_pane(&store.get_workspace(ws1).await.unwrap());
        let moved = store.get_workspace(ws1).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(src)
            .unwrap();
        let dst = first_pane(&store.get_workspace(ws2).await.unwrap());

        let out = store
            .move_surface_to_pane(src, moved, dst, usize::MAX)
            .await
            .expect("cross-workspace move must succeed");
        assert_eq!(out.dst_workspace, ws2);
        assert_eq!(out.src_workspace, ws1);
        assert!(pane_tab_ids(&store, ws2, dst).await.contains(&moved));
    }

    // ---- split_surface_into_pane ----

    #[tokio::test]
    async fn split_surface_into_pane_creates_sibling_with_moved_tab() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let dst = first_pane(&store.get_workspace(ws).await.unwrap());
        // Give the source its own pane (so dst is untouched) with a tab to move.
        let (_, src) = store
            .split_pane(dst, SplitDirection::Vertical)
            .await
            .unwrap();
        let (_, moved) = store.add_terminal_surface_to_pane(src, None).await.unwrap();

        let out = store
            .split_surface_into_pane(src, moved, dst, SplitDirection::Horizontal)
            .await
            .unwrap();
        assert_eq!(out.dst_workspace, ws);
        assert_eq!(out.surface, moved);
        assert_eq!(out.src_pane, src);
        assert_eq!(out.dst_pane, dst);
        assert_eq!(out.changed_workspaces(), vec![ws]);
        assert_eq!(out.changed_panes(), vec![src, dst, out.new_pane]);
        assert_eq!(out.changed_surfaces(), [moved]);
        assert!(!out.src_pane_removed);

        // The new sibling pane holds exactly the moved tab.
        assert_eq!(pane_tab_ids(&store, ws, out.new_pane).await, vec![moved]);
        assert!(!pane_tab_ids(&store, ws, src).await.contains(&moved));
        // dst keeps its original tab; the new split sits next to dst.
        let w = store.get_workspace(ws).await.unwrap();
        assert!(w.surfaces[0]
            .root_pane
            .parent_split_id(out.new_pane)
            .is_some());
    }

    #[tokio::test]
    async fn editor_and_browser_kinds_survive_move_and_split() {
        let root = std::path::PathBuf::from("/tmp/flowmux-dnd-kinds");
        let store = StateStore::new_lazy(State::default());
        let ws = store.create_workspace(Some("w".into()), root.clone()).await;
        let src = first_pane(&store.get_workspace(ws).await.unwrap());
        let (_, dst) = store
            .split_pane(src, SplitDirection::Vertical)
            .await
            .unwrap();
        let (_, editor) = store
            .add_editor_surface_to_pane(src, root.clone())
            .await
            .unwrap();
        let (_, browser) = store
            .add_browser_surface_to_pane(src, "https://example.test".into())
            .await
            .unwrap();

        store
            .move_surface_to_pane(src, editor, dst, usize::MAX)
            .await
            .expect("editor tab move should succeed");
        let split = store
            .split_surface_into_pane(src, browser, dst, SplitDirection::Horizontal)
            .await
            .expect("browser tab split should succeed");

        let workspace = store.get_workspace(ws).await.unwrap();
        let moved_editor = workspace
            .surfaces
            .iter()
            .find_map(|surface| surface.root_pane.find_surface(dst, editor));
        assert!(matches!(
            moved_editor.map(|surface| surface.kind),
            Some(SurfaceKind::Editor { workspace_root, .. }) if workspace_root == root
        ));
        let split_browser = workspace
            .surfaces
            .iter()
            .find_map(|surface| surface.root_pane.find_surface(split.new_pane, browser));
        assert!(matches!(
            split_browser.map(|surface| surface.kind),
            Some(SurfaceKind::Browser { initial_url: Some(url) })
                if url == "https://example.test"
        ));
    }

    #[tokio::test]
    async fn split_singleton_surface_into_its_own_pane_is_noop() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let before = store.get_workspace(ws).await.unwrap();
        let pane = first_pane(&before);
        let only = before.surfaces[0]
            .root_pane
            .active_surface_id(pane)
            .unwrap();

        assert!(store
            .split_surface_into_pane(pane, only, pane, SplitDirection::Horizontal)
            .await
            .is_none());

        let after = store.get_workspace(ws).await.unwrap();
        assert_eq!(after.surfaces[0].root_pane.first_leaf_id(), Some(pane));
        assert_eq!(pane_tab_ids(&store, ws, pane).await, vec![only]);
        assert!(after.surfaces[0].root_pane.parent_split_id(pane).is_none());
    }

    #[tokio::test]
    async fn import_surface_to_pane_assigns_new_id_and_activates() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("ws".into()), std::path::PathBuf::from("/tmp/ws"))
            .await;
        let pane = first_pane(&store.get_workspace(ws).await.unwrap());
        let imported = PaneSurface::browser("Remote", "https://remote.test".into());
        let old_id = imported.id;

        let (dst_ws, new_id) = store
            .import_surface_to_pane(pane, imported, usize::MAX)
            .await
            .unwrap();

        assert_eq!(dst_ws, ws);
        assert_ne!(new_id, old_id);
        assert_eq!(
            store.get_workspace(ws).await.unwrap().surfaces[0]
                .root_pane
                .active_surface_id(pane),
            Some(new_id)
        );
        assert_eq!(
            store.surface_title(pane, new_id).await.as_deref(),
            Some("Remote")
        );
    }

    #[tokio::test]
    async fn split_imported_surface_into_pane_creates_sibling() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("ws".into()), std::path::PathBuf::from("/tmp/ws"))
            .await;
        let dst = first_pane(&store.get_workspace(ws).await.unwrap());
        let imported = PaneSurface::terminal("Remote shell", Some("/tmp/remote".into()));

        let (dst_ws, new_pane, new_surface) = store
            .split_imported_surface_into_pane(dst, imported, SplitDirection::Horizontal)
            .await
            .unwrap();

        assert_eq!(dst_ws, ws);
        assert_eq!(pane_tab_ids(&store, ws, new_pane).await, vec![new_surface]);
        let w = store.get_workspace(ws).await.unwrap();
        assert!(w.surfaces[0].root_pane.parent_split_id(new_pane).is_some());
    }

    #[tokio::test]
    async fn split_surface_into_pane_collapses_emptied_source() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let dst = first_pane(&store.get_workspace(ws).await.unwrap());
        let (_, src) = store
            .split_pane(dst, SplitDirection::Vertical)
            .await
            .unwrap();
        let only = store.get_workspace(ws).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(src)
            .unwrap();

        let out = store
            .split_surface_into_pane(src, only, dst, SplitDirection::Horizontal)
            .await
            .unwrap();
        assert!(out.src_pane_removed);
        assert!(!out.src_workspace_removed);
        assert_eq!(pane_tab_ids(&store, ws, out.new_pane).await, vec![only]);
    }

    #[tokio::test]
    async fn split_surface_into_missing_dest_keeps_surface() {
        let store = StateStore::new_lazy(State::default());
        let ws = store
            .create_workspace(Some("w".into()), std::path::PathBuf::from("/tmp/w"))
            .await;
        let src = first_pane(&store.get_workspace(ws).await.unwrap());
        let moved = store.get_workspace(ws).await.unwrap().surfaces[0]
            .root_pane
            .active_surface_id(src)
            .unwrap();
        assert!(store
            .split_surface_into_pane(src, moved, PaneId::new(), SplitDirection::Vertical)
            .await
            .is_none());
        assert!(pane_tab_ids(&store, ws, src).await.contains(&moved));
    }
}
