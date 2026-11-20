// SPDX-License-Identifier: GPL-3.0-or-later
//! Host-authorized surfaces, kept outside window snapshots so late saves cannot undo a move.

use crate::{State, StateError, WindowOwner};
use flowmux_core::{
    AgentViewBinding, CloseSurfaceOutcome, Pane, PaneContent, RemoveOutcome, SurfaceId,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use uuid::Uuid;

// Bound admission file buffers without restricting ordinary GUI state restoration.
pub(crate) const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
pub(crate) const MAX_JSON_STRUCTURAL_TOKENS: usize = 128 * 1024;

pub(crate) fn read(path: &Path) -> std::io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    if file.metadata()?.len() > MAX_FILE_BYTES as u64 {
        return Err(file_too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(file_too_large());
    }
    if !flowmux_core::json::JsonStructureBudget::new(MAX_JSON_STRUCTURAL_TOKENS).check(&bytes) {
        return Err(json_too_complex());
    }
    Ok(bytes)
}

pub(crate) fn json_too_complex() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "Agent view state exceeds the JSON structure limit; existing execution is unchanged",
    )
}

pub(crate) fn file_too_large() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        "Agent view state exceeds the 32 MiB file limit; existing execution is unchanged",
    )
}

fn load_state(path: &Path) -> Result<State, StateError> {
    match read(path) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            crate::load_text(path, text)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(error) => Err(error.into()),
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PreparedView {
    surface: SurfaceId,
    lease: (Uuid, u64),
    owner: WindowOwner,
    window_save_id: Option<Uuid>,
    snapshot: State,
}

impl PreparedView {
    fn validate(&self, binding: AgentViewBinding) -> Result<(), StateError> {
        let mut matches = 0;
        for workspace in &self.snapshot.workspaces {
            crate::for_each_workspace_tab(workspace, &mut |tab| {
                if tab.id == self.surface
                    && tab.agent_binding.or(tab.shell_binding) == Some(binding)
                    && workspace.local_root().is_some()
                {
                    matches += 1;
                }
            });
        }
        if matches != 1
            || self.owner.pid == 0
            || self.owner.instance_id.is_nil()
            || self.lease.0.is_nil()
            || self.lease.1 == 0
        {
            return Err(StateError::Invalid(
                "invalid Agent destination checkpoint".into(),
            ));
        }
        crate::validate_locations(&self.snapshot)?;
        Ok(())
    }
}

fn prepared_path(state: &Path, binding: AgentViewBinding) -> PathBuf {
    state.with_extension(format!(
        "agent-view-prepared.{}.{}.json",
        binding.epoch, binding.instance.0
    ))
}

/// A checkpoint is not authority. The Host commits it only for the exact admitted lease.
pub fn prepare(
    state: &Path,
    binding: AgentViewBinding,
    surface: SurfaceId,
    lease: (Uuid, u64),
    owner: WindowOwner,
    snapshot: State,
) -> Result<(), StateError> {
    let _lock = crate::instance_lock::try_acquire_for_state(state)?;
    let disk = load_state(state)?;
    let prepared = PreparedView {
        surface,
        lease,
        owner,
        window_save_id: window_save_id(&disk, owner),
        snapshot,
    };
    prepared.validate(binding)?;
    let path = prepared_path(state, binding);
    match read(&path) {
        Ok(bytes) => {
            let previous: PreparedView = serde_json::from_slice(&bytes)?;
            if previous.lease.1 > lease.1
                || (previous.lease.1 == lease.1 && previous.lease != lease)
            {
                return Err(StateError::Invalid(
                    "Agent destination checkpoint was superseded".into(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    crate::atomic_write_with_limit(&path, &prepared, MAX_FILE_BYTES)
}

fn window_save_id(state: &State, owner: WindowOwner) -> Option<Uuid> {
    state
        .windows
        .iter()
        .find(|window| window.instance_id == owner.instance_id)
        .map(|window| window.save_id)
}

fn path(state: &Path, epoch: Uuid) -> PathBuf {
    state.with_extension(format!("agent-views.{epoch}.json"))
}

fn load(
    state: &Path,
    epoch: Uuid,
    remaining: &mut usize,
) -> Result<BTreeMap<Uuid, SurfaceId>, StateError> {
    match read(&path(state, epoch)) {
        Ok(bytes) => {
            *remaining = remaining.checked_sub(bytes.len()).ok_or_else(|| {
                StateError::Invalid(
                    "Agent view authority exceeds the 32 MiB total read budget".into(),
                )
            })?;
            Ok(serde_json::from_slice(&bytes)?)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(error) => Err(error.into()),
    }
}

/// The window saved after preparing this checkpoint. It is refused so it cannot replace the newer
/// save; a new one can be admitted.
pub const SAVED_AFTER_PREPARATION: &str =
    "Agent destination was saved after preparation; retry the move";

/// Commit before publishing ready/input ownership. Failure leaves the original lease active.
pub fn commit(
    state: &Path,
    binding: AgentViewBinding,
    surface: SurfaceId,
) -> Result<(), StateError> {
    commit_inner(state, binding, surface, None)
}

pub fn commit_gui(
    state: &Path,
    binding: AgentViewBinding,
    surface: SurfaceId,
    lease: (Uuid, u64),
    gui_pid: u32,
) -> Result<(), StateError> {
    commit_inner(state, binding, surface, Some((lease, gui_pid)))
}

pub fn discard_prepared(
    state: &Path,
    binding: AgentViewBinding,
    lease: (Uuid, u64),
    gui_pid: Option<u32>,
) -> Result<(), StateError> {
    let _lock = crate::instance_lock::try_acquire_for_state(state)?;
    let path = prepared_path(state, binding);
    match read(&path) {
        Ok(bytes) => {
            let prepared: PreparedView = serde_json::from_slice(&bytes)?;
            if prepared.lease == lease && gui_pid.is_none_or(|pid| prepared.owner.pid == pid) {
                std::fs::remove_file(path)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn commit_inner(
    state: &Path,
    binding: AgentViewBinding,
    surface: SurfaceId,
    checkpoint: Option<((Uuid, u64), u32)>,
) -> Result<(), StateError> {
    if binding.instance.0.is_nil() || binding.epoch.is_nil() || surface.0.is_nil() {
        return Err(StateError::Invalid("invalid Agent view authority".into()));
    }
    let _lock = crate::instance_lock::try_acquire_for_state(state)?;
    let mut remaining = MAX_FILE_BYTES;
    let mut owners = load(state, binding.epoch, &mut remaining)?;
    if let Some((lease, gui_pid)) = checkpoint {
        let prepared: PreparedView =
            serde_json::from_slice(&read(&prepared_path(state, binding))?)?;
        if prepared.surface != surface || prepared.lease != lease || prepared.owner.pid != gui_pid {
            return Err(StateError::Invalid(
                "Agent destination checkpoint changed".into(),
            ));
        }
        prepared.validate(binding)?;
        let disk = load_state(state)?;
        if window_save_id(&disk, prepared.owner) != prepared.window_save_id {
            return Err(StateError::Invalid(SAVED_AFTER_PREPARATION.into()));
        }
        let disk = crate::merge_window_snapshot(prepared.owner, prepared.snapshot, disk);
        // Save the destination first under the same lock as authority; a late source save cannot remove it.
        crate::save_owned_with_agent_view_to(state, disk, Some((binding, surface)))?;
    }
    owners.insert(binding.instance.0, surface);
    crate::atomic_write_with_limit(&path(state, binding.epoch), &owners, MAX_FILE_BYTES)?;
    if checkpoint.is_some() {
        let _ = std::fs::remove_file(prepared_path(state, binding));
    }
    Ok(())
}

/// Restoration must not depend on authority files: an unreadable one only
/// skips its fence, and the Host still rejects a stale tab when it attaches.
pub(crate) fn reconcile(path: &Path, state: &mut State) -> Result<(), StateError> {
    reconcile_inner(path, state, None, false)
}

pub(crate) fn reconcile_with_owner(
    path: &Path,
    state: &mut State,
    owner: Option<(AgentViewBinding, SurfaceId)>,
) -> Result<(), StateError> {
    reconcile_inner(path, state, owner, true)
}

fn reconcile_inner(
    path: &Path,
    state: &mut State,
    owner: Option<(AgentViewBinding, SurfaceId)>,
    strict: bool,
) -> Result<(), StateError> {
    let mut bindings = HashSet::new();
    for workspace in &state.workspaces {
        crate::for_each_workspace_tab(workspace, &mut |tab| {
            if let Some(binding) = tab.agent_binding {
                bindings.insert(binding.epoch);
            }
        });
    }
    let mut remaining = MAX_FILE_BYTES;
    let epochs = bindings
        .into_iter()
        .map(|epoch| match load(path, epoch, &mut remaining) {
            Ok(owners) => Ok((epoch, owners)),
            Err(error) if strict => Err(error),
            Err(_) => Ok((epoch, BTreeMap::new())),
        })
        .collect::<Result<HashMap<_, _>, StateError>>()?;
    let mut removed = HashSet::new();
    state.workspaces.retain_mut(|workspace| {
        let had_surfaces = !workspace.surfaces.is_empty();
        workspace.surfaces.retain_mut(|surface| {
            fn stale(
                pane: &Pane,
                epochs: &HashMap<Uuid, BTreeMap<Uuid, SurfaceId>>,
                prepared: Option<(AgentViewBinding, SurfaceId)>,
                result: &mut Vec<(flowmux_core::PaneId, SurfaceId)>,
            ) {
                match pane {
                    Pane::Leaf {
                        id,
                        content: PaneContent::Tabs { surfaces, .. },
                    } => {
                        for tab in surfaces {
                            if tab.agent_binding.is_some_and(|binding| {
                                if prepared == Some((binding, tab.id)) {
                                    return false;
                                }
                                epochs[&binding.epoch]
                                    .get(&binding.instance.0)
                                    .is_some_and(|winner| *winner != tab.id)
                            }) {
                                result.push((*id, tab.id));
                            }
                        }
                    }
                    Pane::Split { first, second, .. } => {
                        stale(first, epochs, prepared, result);
                        stale(second, epochs, prepared, result);
                    }
                    _ => {}
                }
            }
            let mut obsolete = Vec::new();
            stale(&surface.root_pane, &epochs, owner, &mut obsolete);
            for (pane, tab) in obsolete {
                if surface.root_pane.close_surface_in_leaf(pane, tab)
                    == CloseSurfaceOutcome::LastSurfaceRemoved
                {
                    match surface.root_pane.clone().remove_leaf(pane) {
                        RemoveOutcome::EntirelyRemoved => return false,
                        RemoveOutcome::Replaced(root) | RemoveOutcome::NotFound(root) => {
                            surface.root_pane = root
                        }
                    }
                }
            }
            true
        });
        let keep = !had_surfaces || !workspace.surfaces.is_empty();
        if !keep {
            removed.insert(workspace.id);
        }
        keep
    });
    state.workspace_order.retain(|id| !removed.contains(id));
    state.workspace_owners.retain(|id, _| !removed.contains(id));
    if state
        .active_workspace
        .is_some_and(|id| removed.contains(&id))
    {
        state.active_workspace = state.workspace_order.first().copied();
    }
    for window in &mut state.windows {
        window.workspace_order.retain(|id| !removed.contains(id));
        if window
            .active_workspace
            .is_some_and(|id| removed.contains(&id))
        {
            window.active_workspace = window.workspace_order.first().copied();
        }
    }
    Ok(())
}

const STALE_FILE_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Remove authority and checkpoint files of Host epochs no saved tab names.
// ponytail: age stands in for Host liveness; a week-old unreferenced file only
// loses a fence the Host re-checks on attach. Ask the Host if that ever matters.
pub(crate) fn remove_stale_files(state_path: &Path, state: &State) {
    let mut referenced = HashSet::new();
    for workspace in &state.workspaces {
        crate::for_each_workspace_tab(workspace, &mut |tab| {
            if let Some(binding) = tab.agent_binding.or(tab.shell_binding) {
                referenced.insert(binding.epoch.to_string());
            }
        });
    }
    let (Some(directory), Some(stem)) = (
        state_path.parent(),
        state_path.file_stem().and_then(|stem| stem.to_str()),
    ) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(rest) = name
            .to_str()
            .and_then(|name| name.strip_prefix(stem))
            .and_then(|rest| {
                rest.strip_prefix(".agent-views.")
                    .or_else(|| rest.strip_prefix(".agent-view-prepared."))
            })
            .and_then(|rest| rest.strip_suffix(".json"))
        else {
            continue;
        };
        // Authority: `<epoch>`; checkpoint: `<epoch>.<instance>`.
        let epoch = rest.split('.').next().unwrap_or(rest);
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= STALE_FILE_AGE);
        if old && Uuid::parse_str(epoch).is_ok() && !referenced.contains(epoch) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Turn every managed Agent tab into an ordinary terminal tab. For a start with
/// no Agent Host at all: its executions are gone, so the tabs can only resume
/// their sessions the ordinary way. Returns how many tabs changed.
pub fn clear_bindings(state: &mut State) -> usize {
    fn clear(pane: &mut Pane, cleared: &mut usize) {
        match pane {
            Pane::Leaf {
                content: PaneContent::Tabs { surfaces, .. },
                ..
            } => {
                for tab in surfaces {
                    *cleared += usize::from(tab.agent_binding.take().is_some());
                }
            }
            Pane::Split { first, second, .. } => {
                clear(first, cleared);
                clear(second, cleared);
            }
            _ => {}
        }
    }
    let mut cleared = 0;
    for workspace in &mut state.workspaces {
        for surface in &mut workspace.surfaces {
            clear(&mut surface.root_pane, &mut cleared);
        }
    }
    cleared
}
