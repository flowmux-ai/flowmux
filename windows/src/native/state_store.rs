// SPDX-License-Identifier: GPL-3.0-or-later
//! An OS-held per-window lease survives PID reuse and releases after a crash.
use super::{checked, data_dir, wide};
use crate::state::{WindowState, MAX_STATE_BYTES};
use anyhow::Context;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::{
    MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
};

pub struct Store {
    pub id: Uuid,
    pub path: PathBuf,
    _lease: File,
}
impl Store {
    fn claim(directory: &Path, id: Uuid) -> anyhow::Result<Self> {
        std::fs::create_dir_all(directory)?;
        let lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(directory.join(format!("{id}.lock")))
            .context("saved window is in use or its lock is inaccessible")?;
        Ok(Self {
            id,
            path: directory.join(format!("{id}.json")),
            _lease: lease,
        })
    }
    fn read(&self) -> anyhow::Result<WindowState> {
        let mut bytes = Vec::new();
        File::open(&self.path)?
            .take((MAX_STATE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        let state = WindowState::decode(&bytes)?;
        anyhow::ensure!(
            state.window == self.id,
            "saved window identity does not match filename"
        );
        Ok(state)
    }
    pub fn write(&self, state: &WindowState) -> anyhow::Result<()> {
        anyhow::ensure!(
            state.window == self.id,
            "cannot overwrite another window's state"
        );
        let bytes = state.encode()?;
        let temporary = self.path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| -> anyhow::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            unsafe {
                checked(MoveFileExW(
                    wide(&temporary).as_ptr(),
                    wide(&self.path).as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                ))?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temporary);
        }
        result.context("could not atomically save Windows window state")
    }
}

pub fn open(
    new_window: bool,
    restore: Option<Uuid>,
    background: bool,
) -> anyhow::Result<(Option<Arc<Store>>, Option<WindowState>)> {
    // Every automated background host is isolated from real state by default.
    let test_directory = if cfg!(debug_assertions) {
        std::env::var_os("FLOWMUX_TEST_STATE_DIR").map(PathBuf::from)
    } else {
        None
    };
    if test_directory.is_none()
        && (background
            || (cfg!(debug_assertions) && std::env::var_os("FLOWMUX_TEST_NO_PERSIST").is_some()))
    {
        anyhow::ensure!(
            restore.is_none(),
            "restoring test state requires FLOWMUX_TEST_STATE_DIR"
        );
        return Ok((None, None));
    }
    let directory = test_directory.unwrap_or(data_dir()?.join("state"));
    open_directory(&directory, new_window, restore)
}
fn open_directory(
    directory: &Path,
    new_window: bool,
    restore: Option<Uuid>,
) -> anyhow::Result<(Option<Arc<Store>>, Option<WindowState>)> {
    if let Some(id) = restore {
        let store = Store::claim(directory, id)?;
        let state = store.read()?;
        return Ok((Some(Arc::new(store)), Some(state)));
    }
    if !new_window {
        if let Ok(entries) = std::fs::read_dir(directory) {
            let mut entries: Vec<_> = entries
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|e| e == "json"))
                .collect();
            entries
                .sort_by_key(|e| std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()));
            for entry in entries {
                let Some(id) = entry
                    .path()
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
                else {
                    continue;
                };
                let Ok(store) = Store::claim(directory, id) else {
                    continue;
                };
                match store.read() {
                    Ok(state) => return Ok((Some(Arc::new(store)), Some(state))),
                    Err(error) => super::host::report(&format!(
                        "Preserved unreadable state {}: {error:#}",
                        entry.path().display()
                    )),
                }
            }
        }
    }
    Ok((
        Some(Arc::new(Store::claim(directory, Uuid::new_v4())?)),
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_lock_atomic_replace_failure_and_corrupt_state_preservation() {
        let directory = std::env::temp_dir().join(format!("flowmux-state-test-{}", Uuid::new_v4()));
        let state = crate::state::sample();
        let store = Store::claim(&directory, state.window).unwrap();
        assert!(Store::claim(&directory, state.window).is_err());
        store.write(&state).unwrap();
        let original = std::fs::read(&store.path).unwrap();
        let guard = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&store.path)
            .unwrap();
        let mut changed = state.clone();
        changed.workspaces[0].name = "changed".into();
        assert!(store.write(&changed).is_err());
        assert_eq!(std::fs::read(&store.path).unwrap(), original);
        drop(guard);
        store.write(&changed).unwrap();
        assert_eq!(store.read().unwrap().workspaces[0].name, "changed");
        assert!(open_directory(&directory, false, Some(state.window)).is_err());
        drop(store);
        let (lease, restored) = open_directory(&directory, false, Some(state.window)).unwrap();
        assert_eq!(restored.unwrap().window, state.window);
        let path = lease.as_ref().unwrap().path.clone();
        drop(lease);
        std::fs::write(&path, b"future or corrupt state").unwrap();
        let (fresh, restored) = open_directory(&directory, false, None).unwrap();
        assert!(restored.is_none());
        assert_ne!(fresh.as_ref().unwrap().id, state.window);
        assert_eq!(std::fs::read(path).unwrap(), b"future or corrupt state");
        drop(fresh);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
