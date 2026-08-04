// SPDX-License-Identifier: GPL-3.0-or-later
//! Atomic on-disk state for flowmux.
//!
//! Single source of truth lives at `$XDG_STATE_HOME/flowmux/state.json`.
//! Writes go through a tmp-file + rename so a crash mid-write never
//! leaves a half-serialized file.
//!
//! Schema is versioned (`schema_version`) so a future flowmux release can
//! migrate old state files from this format.

use flowmux_config::paths;
use flowmux_core::Workspace;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub mod agent_sessions;
pub mod instance_lock;
pub use agent_sessions::{default_agent_session_store, AgentSessionStore, SavedAgentSession};
pub use instance_lock::{try_acquire_state_lock, InstanceLock};

pub const SCHEMA_VERSION: u32 = 2;

/// Window size and maximized state, saved on exit and restored on next launch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowLayout {
    pub width: i32,
    pub height: i32,
    #[serde(default)]
    pub maximized: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowOwner {
    pub instance_id: Uuid,
    pub pid: u32,
}

impl WindowOwner {
    pub fn current() -> Self {
        Self {
            instance_id: Uuid::new_v4(),
            pid: std::process::id(),
        }
    }
}

/// Per-process window metadata stored alongside the workspace ownership map.
/// A later launch reclaims records whose owner PID is no longer alive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedWindow {
    pub instance_id: Uuid,
    pub owner_pid: u32,
    /// Kernel boot id at save time. A record from a different boot predates a
    /// reboot: every PTY it owned is dead and its pid may have been reused, so
    /// restart discards its workspaces instead of restoring dead panes.
    /// `None` (old state files, non-Linux) keeps the always-restore behavior.
    #[serde(default)]
    pub boot_id: Option<String>,
    #[serde(default)]
    pub layout: Option<WindowLayout>,
    #[serde(default)]
    pub sidebar_position: Option<i32>,
    #[serde(default)]
    pub workspace_order: Vec<flowmux_core::WorkspaceId>,
    #[serde(default)]
    pub active_workspace: Option<flowmux_core::WorkspaceId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub schema_version: u32,
    pub workspaces: Vec<Workspace>,
    /// Workspace IDs in the order they appear in the sidebar.
    #[serde(default)]
    pub workspace_order: Vec<flowmux_core::WorkspaceId>,
    /// Most-recently-active workspace, used to focus on launch.
    #[serde(default)]
    pub active_workspace: Option<flowmux_core::WorkspaceId>,
    /// Legacy v1 layout field. Load migrates it into [`State::windows`]; owned
    /// GUI saves leave it unset.
    #[serde(default)]
    pub window: Option<WindowLayout>,
    /// Legacy v1 side-panel divider position.
    #[serde(default)]
    pub sidebar_position: Option<i32>,
    #[serde(default)]
    pub windows: Vec<SavedWindow>,
    #[serde(default)]
    pub workspace_owners: HashMap<flowmux_core::WorkspaceId, Uuid>,
    pub last_saved: chrono::DateTime<chrono::Utc>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            workspaces: vec![],
            workspace_order: vec![],
            active_workspace: None,
            window: None,
            sidebar_position: None,
            windows: Vec::new(),
            workspace_owners: HashMap::new(),
            last_saved: chrono::Utc::now(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Json(#[from] serde_json::Error),
    #[error("state dir is unavailable")]
    NoStateDir,
    #[error("schema version {found} is newer than supported ({supported})")]
    SchemaTooNew { found: u32, supported: u32 },
}

pub fn default_path() -> Result<PathBuf, StateError> {
    paths::state_dir()
        .ok_or(StateError::NoStateDir)
        .map(|d| d.join("state.json"))
}

pub fn load() -> Result<State, StateError> {
    load_from(&default_path()?)
}

pub fn save(state: &State) -> Result<(), StateError> {
    save_to(&default_path()?, state)
}

/// Persist an owned snapshot without cloning the complete state first.
pub fn save_owned(state: State) -> Result<(), StateError> {
    save_owned_to(&default_path()?, state)
}

pub fn load_from(path: &Path) -> Result<State, StateError> {
    if !path.exists() {
        return Ok(State::default());
    }
    let text = std::fs::read_to_string(path)?;
    let mut state: State = serde_json::from_str(&text)?;
    if state.schema_version > SCHEMA_VERSION {
        return Err(StateError::SchemaTooNew {
            found: state.schema_version,
            supported: SCHEMA_VERSION,
        });
    }
    migrate_legacy_state(&mut state);
    Ok(state)
}

pub fn save_to(path: &Path, state: &State) -> Result<(), StateError> {
    save_owned_to(path, state.clone())
}

fn save_owned_to(path: &Path, mut state: State) -> Result<(), StateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    migrate_legacy_state(&mut state);
    state.last_saved = chrono::Utc::now();
    let json = serde_json::to_vec_pretty(&state)?;

    // Atomic replace: write to <name>.tmp, fsync, then rename.
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn migrate_legacy_state(state: &mut State) {
    let legacy_id = Uuid::nil();
    let needs_window = state.windows.is_empty()
        && (!state.workspaces.is_empty()
            || state.window.is_some()
            || state.sidebar_position.is_some());
    if needs_window {
        state.windows.push(SavedWindow {
            instance_id: legacy_id,
            owner_pid: 0,
            boot_id: None,
            layout: state.window.take(),
            sidebar_position: state.sidebar_position.take(),
            workspace_order: ordered_workspace_ids(state),
            active_workspace: state.active_workspace,
        });
    } else {
        state.window = None;
        state.sidebar_position = None;
    }
    for workspace in &state.workspaces {
        state
            .workspace_owners
            .entry(workspace.id)
            .or_insert(legacy_id);
    }
    state.schema_version = SCHEMA_VERSION;
}

fn ordered_workspace_ids(state: &State) -> Vec<flowmux_core::WorkspaceId> {
    let live = state
        .workspaces
        .iter()
        .map(|workspace| workspace.id)
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    let mut order = Vec::with_capacity(live.len());
    for id in &state.workspace_order {
        if live.contains(id) && seen.insert(*id) {
            order.push(*id);
        }
    }
    for workspace in &state.workspaces {
        if seen.insert(workspace.id) {
            order.push(workspace.id);
        }
    }
    order
}

/// Kernel boot id of the running system. `None` on non-Linux platforms
/// or when `/proc` is unavailable, which disables reboot detection and
/// keeps the always-restore behavior.
// ponytail: Linux-only via /proc; add a sysctl(kern.boottime) branch if
// macOS ever needs reboot detection.
fn current_boot_id() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Every terminal/browser tab id in the workspace's pane trees.
fn workspace_surface_ids(workspace: &Workspace, out: &mut Vec<flowmux_core::SurfaceId>) {
    fn rec(pane: &flowmux_core::Pane, out: &mut Vec<flowmux_core::SurfaceId>) {
        match pane {
            flowmux_core::Pane::Leaf { content, .. } => {
                if let flowmux_core::PaneContent::Tabs { surfaces, .. } = content {
                    out.extend(surfaces.iter().map(|surface| surface.id));
                }
            }
            flowmux_core::Pane::Split { first, second, .. } => {
                rec(first, out);
                rec(second, out);
            }
        }
    }
    for surface in &workspace.surfaces {
        rec(&surface.root_pane, out);
    }
}

/// Atomically claim every unowned or dead-process workspace for a new GUI
/// window. The on-disk ownership update happens before returning so two
/// concurrent launches cannot restore the same workspace set.
///
/// Workspaces owned by a window from a previous kernel boot are discarded,
/// not claimed: after a reboot their processes and agent sessions are gone,
/// so restoring the workspace would only show dead panes. Their saved agent
/// session bindings are forgotten too.
pub fn claim_window(owner: WindowOwner) -> Result<State, StateError> {
    let path = default_path()?;
    let (state, expired_surfaces) = claim_window_impl(&path, owner, current_boot_id())?;
    if !expired_surfaces.is_empty() {
        if let Some(store) = default_agent_session_store() {
            for surface in &expired_surfaces {
                let _ = store.forget_surface(*surface);
            }
        }
    }
    Ok(state)
}

pub fn claim_window_from(path: &Path, owner: WindowOwner) -> Result<State, StateError> {
    claim_window_impl(path, owner, current_boot_id()).map(|(state, _)| state)
}

fn claim_window_impl(
    path: &Path,
    owner: WindowOwner,
    current_boot: Option<String>,
) -> Result<(State, Vec<flowmux_core::SurfaceId>), StateError> {
    let _lock = instance_lock::acquire_for_state(path)?;
    let mut disk = load_from(path)?;
    let stale_boot = |window: &SavedWindow| {
        matches!(
            (&window.boot_id, &current_boot),
            (Some(saved), Some(now)) if saved != now
        )
    };
    // A pid from a previous boot can be reused by an unrelated process,
    // so the liveness check only applies within the current boot.
    let live_owners = disk
        .windows
        .iter()
        .filter(|window| !stale_boot(window) && flowmux_procmon::pid_alive(window.owner_pid))
        .map(|window| window.instance_id)
        .collect::<HashSet<_>>();
    let expired_owners = disk
        .windows
        .iter()
        .filter(|window| stale_boot(window))
        .map(|window| window.instance_id)
        .collect::<HashSet<_>>();
    let expired = disk
        .workspaces
        .iter()
        .filter_map(|workspace| {
            disk.workspace_owners
                .get(&workspace.id)
                .is_some_and(|owner| expired_owners.contains(owner))
                .then_some(workspace.id)
        })
        .collect::<HashSet<_>>();
    let mut expired_surfaces = Vec::new();
    for workspace in &disk.workspaces {
        if expired.contains(&workspace.id) {
            workspace_surface_ids(workspace, &mut expired_surfaces);
        }
    }
    disk.workspaces
        .retain(|workspace| !expired.contains(&workspace.id));
    disk.workspace_order
        .retain(|workspace| !expired.contains(workspace));
    disk.workspace_owners
        .retain(|workspace, _| !expired.contains(workspace));
    let claimed = disk
        .workspaces
        .iter()
        .filter_map(|workspace| {
            let existing = disk.workspace_owners.get(&workspace.id);
            existing
                .is_none_or(|owner| !live_owners.contains(owner))
                .then_some(workspace.id)
        })
        .collect::<HashSet<_>>();

    let prior_window = disk.windows.iter().rev().find(|window| {
        !live_owners.contains(&window.instance_id)
            && window
                .workspace_order
                .iter()
                .any(|workspace| claimed.contains(workspace))
    });
    let layout = prior_window.and_then(|window| window.layout.clone());
    let sidebar_position = prior_window.and_then(|window| window.sidebar_position);
    let active_workspace = prior_window
        .and_then(|window| window.active_workspace)
        .filter(|workspace| claimed.contains(workspace));
    let workspace_order = ordered_workspace_ids(&disk)
        .into_iter()
        .filter(|workspace| claimed.contains(workspace))
        .collect::<Vec<_>>();
    let workspaces = disk
        .workspaces
        .iter()
        .filter(|workspace| claimed.contains(&workspace.id))
        .cloned()
        .collect::<Vec<_>>();

    disk.windows
        .retain(|window| live_owners.contains(&window.instance_id));
    for workspace in &claimed {
        disk.workspace_owners.insert(*workspace, owner.instance_id);
    }
    disk.windows.push(SavedWindow {
        instance_id: owner.instance_id,
        owner_pid: owner.pid,
        boot_id: current_boot.clone(),
        layout: layout.clone(),
        sidebar_position,
        workspace_order: workspace_order.clone(),
        active_workspace,
    });
    disk.window = None;
    disk.sidebar_position = None;
    save_to(path, &disk)?;

    Ok((
        State {
            schema_version: SCHEMA_VERSION,
            workspaces,
            workspace_order,
            active_workspace,
            window: layout,
            sidebar_position,
            windows: Vec::new(),
            workspace_owners: HashMap::new(),
            last_saved: disk.last_saved,
        },
        expired_surfaces,
    ))
}

/// Merge one window's latest snapshot into the shared state while preserving
/// workspace sets and layout records owned by every other live process.
pub fn save_window(owner: WindowOwner, snapshot: &State) -> Result<(), StateError> {
    save_window_to(&default_path()?, owner, snapshot)
}

/// Merge an owned window snapshot without cloning its workspace tree.
pub fn save_window_owned(owner: WindowOwner, snapshot: State) -> Result<(), StateError> {
    save_window_owned_to(&default_path()?, owner, snapshot)
}

pub fn save_window_to(path: &Path, owner: WindowOwner, snapshot: &State) -> Result<(), StateError> {
    save_window_owned_to(path, owner, snapshot.clone())
}

fn save_window_owned_to(
    path: &Path,
    owner: WindowOwner,
    snapshot: State,
) -> Result<(), StateError> {
    let _lock = instance_lock::acquire_for_state(path)?;
    let mut disk = load_from(path)?;
    let previously_owned = disk
        .workspace_owners
        .iter()
        .filter_map(|(workspace, existing)| (*existing == owner.instance_id).then_some(*workspace))
        .collect::<HashSet<_>>();
    disk.workspaces
        .retain(|workspace| !previously_owned.contains(&workspace.id));
    disk.workspace_order
        .retain(|workspace| !previously_owned.contains(workspace));
    disk.workspace_owners
        .retain(|workspace, _| !previously_owned.contains(workspace));

    let owned_order = ordered_workspace_ids(&snapshot);
    let owned_ids = snapshot
        .workspaces
        .iter()
        .map(|workspace| workspace.id)
        .collect::<Vec<_>>();
    disk.workspaces.extend(snapshot.workspaces);
    disk.workspace_order.extend(owned_order.iter().copied());
    for workspace in owned_ids {
        disk.workspace_owners.insert(workspace, owner.instance_id);
    }
    disk.windows
        .retain(|window| window.instance_id != owner.instance_id);
    disk.windows.push(SavedWindow {
        instance_id: owner.instance_id,
        owner_pid: owner.pid,
        boot_id: current_boot_id(),
        layout: snapshot.window.clone(),
        sidebar_position: snapshot.sidebar_position,
        workspace_order: owned_order,
        active_workspace: snapshot.active_workspace,
    });
    disk.active_workspace = None;
    disk.window = None;
    disk.sidebar_position = None;
    save_owned_to(path, disk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::*;
    use std::path::PathBuf;

    fn sample_workspace() -> Workspace {
        Workspace {
            id: WorkspaceId::new(),
            name: "demo".into(),
            custom_title: None,
            root_dir: PathBuf::from("/tmp/demo"),
            git: None,
            listening_ports: vec![],
            surfaces: vec![],
            color: None,
        }
    }

    /// Workspace holding one terminal tab with a fixed surface id, owned by
    /// `owner` whose window record was saved under `boot_id`.
    fn state_with_owned_workspace(
        owner: WindowOwner,
        boot_id: Option<&str>,
        tab: SurfaceId,
    ) -> State {
        let mut workspace = sample_workspace();
        workspace.surfaces.push(Surface {
            id: SurfaceId::new(),
            kind: SurfaceKind::Terminal {
                shell: None,
                cwd: None,
            },
            title: "tab".into(),
            root_pane: Pane::Leaf {
                id: PaneId::new(),
                content: PaneContent::Tabs {
                    active: tab,
                    surfaces: vec![PaneSurface {
                        id: tab,
                        title: "tab".into(),
                        title_locked: false,
                        kind: SurfaceKind::Terminal {
                            shell: None,
                            cwd: None,
                        },
                        scrollback: Some("old output".into()),
                        agent: None,
                    }],
                },
            },
        });
        let id = workspace.id;
        let mut state = State::default();
        state.workspaces.push(workspace);
        state.workspace_order.push(id);
        state.workspace_owners.insert(id, owner.instance_id);
        state.windows.push(SavedWindow {
            instance_id: owner.instance_id,
            owner_pid: owner.pid,
            boot_id: boot_id.map(str::to_string),
            layout: None,
            sidebar_position: None,
            workspace_order: vec![id],
            active_workspace: Some(id),
        });
        state
    }

    #[test]
    fn dead_window_from_same_boot_is_still_restored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let dead = WindowOwner {
            instance_id: Uuid::new_v4(),
            pid: 999_999,
        };
        let tab = SurfaceId::new();
        save_to(
            &path,
            &state_with_owned_workspace(dead, Some("boot-a"), tab),
        )
        .unwrap();

        let (claimed, expired) =
            claim_window_impl(&path, WindowOwner::current(), Some("boot-a".into())).unwrap();
        assert_eq!(claimed.workspaces.len(), 1, "same-boot crash must restore");
        assert!(expired.is_empty());
    }

    #[test]
    fn reboot_expires_stale_boot_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let dead = WindowOwner {
            instance_id: Uuid::new_v4(),
            pid: 999_999,
        };
        let tab = SurfaceId::new();
        save_to(
            &path,
            &state_with_owned_workspace(dead, Some("boot-a"), tab),
        )
        .unwrap();

        let (claimed, expired) =
            claim_window_impl(&path, WindowOwner::current(), Some("boot-b".into())).unwrap();
        assert!(
            claimed.workspaces.is_empty(),
            "workspaces from a previous boot must not be restored"
        );
        assert_eq!(expired, vec![tab], "the dead tab is reported for cleanup");

        let disk = load_from(&path).unwrap();
        assert!(disk.workspaces.is_empty());
        assert!(disk.workspace_owners.is_empty());
        assert!(disk.workspace_order.is_empty());
    }

    #[test]
    fn window_without_boot_id_keeps_legacy_restore_behavior() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let dead = WindowOwner {
            instance_id: Uuid::new_v4(),
            pid: 999_999,
        };
        let tab = SurfaceId::new();
        save_to(&path, &state_with_owned_workspace(dead, None, tab)).unwrap();

        let (claimed, expired) =
            claim_window_impl(&path, WindowOwner::current(), Some("boot-b".into())).unwrap();
        assert_eq!(claimed.workspaces.len(), 1, "old state files must restore");
        assert!(expired.is_empty());
    }

    #[test]
    fn missing_file_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = load_from(&path).unwrap();
        assert!(state.workspaces.is_empty());
        assert_eq!(state.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = State::default();
        let ws = sample_workspace();
        let id = ws.id;
        state.workspaces.push(ws);
        state.workspace_order.push(id);
        state.active_workspace = Some(id);
        save_to(&path, &state).unwrap();

        let back = load_from(&path).unwrap();
        assert_eq!(back.workspaces.len(), 1);
        assert_eq!(back.workspaces[0].name, "demo");
        assert_eq!(back.active_workspace, Some(id));
    }

    #[test]
    fn owned_save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let mut state = State::default();
        let workspace = sample_workspace();
        let id = workspace.id;
        state.workspace_order.push(id);
        state.workspaces.push(workspace);

        save_owned_to(&path, state).unwrap();

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.workspace_order, vec![id]);
        assert_eq!(loaded.workspaces[0].id, id);
    }

    #[test]
    fn owned_window_save_records_workspace_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let owner = WindowOwner::current();
        let workspace = sample_workspace();
        let id = workspace.id;
        let state = State {
            workspaces: vec![workspace],
            workspace_order: vec![id],
            active_workspace: Some(id),
            ..Default::default()
        };

        save_window_owned_to(&path, owner, state).unwrap();

        let loaded = load_from(&path).unwrap();
        assert_eq!(loaded.workspace_order, vec![id]);
        assert_eq!(loaded.workspace_owners.get(&id), Some(&owner.instance_id));
        assert_eq!(loaded.windows[0].active_workspace, Some(id));
    }

    #[test]
    fn window_and_sidebar_position_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let state = State {
            window: Some(WindowLayout {
                width: 1600,
                height: 900,
                maximized: true,
            }),
            sidebar_position: Some(312),
            ..Default::default()
        };
        save_to(&path, &state).unwrap();

        let back = load_from(&path).unwrap();
        assert_eq!(back.window, None);
        assert_eq!(back.sidebar_position, None);
        assert_eq!(back.windows.len(), 1);
        assert_eq!(
            back.windows[0].layout,
            Some(WindowLayout {
                width: 1600,
                height: 900,
                maximized: true,
            })
        );
        assert_eq!(back.windows[0].sidebar_position, Some(312));
    }

    #[test]
    fn legacy_v1_state_migrates_without_workspace_or_layout_loss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let workspace = sample_workspace();
        let workspace_id = workspace.id;
        let fixture = serde_json::json!({
            "schema_version": 1,
            "workspaces": [workspace],
            "workspace_order": [workspace_id],
            "active_workspace": workspace_id,
            "window": {"width": 1440, "height": 900, "maximized": true},
            "sidebar_position": 280,
            "last_saved": "2026-01-01T00:00:00Z"
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&fixture).unwrap()).unwrap();

        let migrated = load_from(&path).unwrap();
        assert_eq!(migrated.schema_version, SCHEMA_VERSION);
        assert_eq!(migrated.workspaces.len(), 1);
        assert_eq!(migrated.workspaces[0].id, workspace_id);
        assert_eq!(migrated.workspace_order, vec![workspace_id]);
        assert_eq!(migrated.active_workspace, Some(workspace_id));
        assert_eq!(migrated.windows.len(), 1);
        assert_eq!(migrated.windows[0].layout.as_ref().unwrap().width, 1440);
        assert_eq!(migrated.windows[0].sidebar_position, Some(280));
        assert_eq!(
            migrated.workspace_owners.get(&workspace_id),
            Some(&Uuid::nil())
        );
    }

    #[test]
    fn missing_layout_fields_load_as_none() {
        // Older state.json files do not contain window / sidebar_position.
        // #[serde(default)] should load them as None.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{
                "schema_version": 1,
                "workspaces": [],
                "last_saved": "2026-01-01T00:00:00Z"
            }"#,
        )
        .unwrap();
        let state = load_from(&path).unwrap();
        assert_eq!(state.window, None);
        assert_eq!(state.sidebar_position, None);
    }

    #[test]
    fn rejects_newer_schema() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"schema_version": 9999, "workspaces": [], "last_saved": "2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let err = load_from(&path).unwrap_err();
        assert!(matches!(err, StateError::SchemaTooNew { .. }));
    }
}
