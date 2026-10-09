// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-workspace office choices survive view recreation without rewriting session state.

use crate::StateError;
use flowmux_core::WorkspaceId;
use std::{
    io::Write,
    path::{Path, PathBuf},
};

fn path(workspace: WorkspaceId) -> Result<PathBuf, StateError> {
    Ok(flowmux_config::paths::state_dir()
        .ok_or(StateError::NoStateDir)?
        .join("office-designs")
        .join(format!("{workspace}.json")))
}

pub fn load(workspace: WorkspaceId) -> Result<Option<u8>, StateError> {
    load_from(&path(workspace)?)
}

pub fn save(workspace: WorkspaceId, design: u8) -> Result<(), StateError> {
    save_to(&path(workspace)?, design)
}

fn load_from(path: &Path) -> Result<Option<u8>, StateError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn save_to(path: &Path, design: u8) -> Result<(), StateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let mut file = std::fs::File::create(&tmp)?;
    file.write_all(&serde_json::to_vec(&design)?)?;
    file.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_designs_survive_reload_and_reject_corrupt_data() {
        let root = tempfile::tempdir().unwrap();
        let first = root.path().join("a.json");
        let second = root.path().join("b.json");
        assert_eq!(load_from(&first).unwrap(), None);
        save_to(&first, 3).unwrap();
        save_to(&second, 19).unwrap();
        save_to(&first, 7).unwrap();
        assert_eq!(load_from(&first).unwrap(), Some(7));
        assert_eq!(load_from(&second).unwrap(), Some(19));
        std::fs::write(&first, "invalid").unwrap();
        assert!(load_from(&first).is_err());
    }
}
