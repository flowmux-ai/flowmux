// SPDX-License-Identifier: GPL-3.0-or-later
//! Short-lived read/merge/write lock for the on-disk `state.json`.
//!
//! GUI windows briefly take an exclusive `flock(2)` on the sibling
//! `state.lock` while they reload, merge their owned workspace slice, and
//! atomically replace the file. The kernel releases the lock on process exit,
//! so a stale lock file never blocks a later launch.
//!
//! See [`crate::StateError`] for the error variants reported here.

use crate::StateError;
use flowmux_config::paths;
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// RAII guard for the state file's exclusive lock.
#[derive(Debug)]
pub struct InstanceLock {
    // Keeping the file handle alive keeps the kernel-side lock alive.
    _file: File,
    path: PathBuf,
}

impl InstanceLock {
    /// Path of the lock file. Exposed for diagnostics/logging only.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

fn lock_path() -> Result<PathBuf, StateError> {
    paths::state_dir()
        .ok_or(StateError::NoStateDir)
        .map(|d| d.join("state.lock"))
}

fn lock_path_for_state(state_path: &Path) -> PathBuf {
    state_path.with_file_name("state.lock")
}

pub(crate) fn acquire_for_state(state_path: &Path) -> Result<InstanceLock, StateError> {
    let path = lock_path_for_state(state_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    file.lock_exclusive()?;
    Ok(InstanceLock { _file: file, path })
}

/// Try to take the exclusive `state.json` lock.
///
/// Return `Ok(None)` while another process owns the lock. Path-resolution
/// and I/O failures return `Err`; an unset `XDG_STATE_HOME` uses the normal
/// platform fallback. GUI persistence uses a blocking short-lived lock.
pub fn try_acquire_state_lock() -> Result<Option<InstanceLock>, StateError> {
    let path = lock_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(InstanceLock { _file: file, path })),
        Err(e) => {
            // fs2 surfaces `ErrorKind::WouldBlock` when another process
            // already holds the lock. Treat anything else as a real
            // I/O failure so the caller can decide how to recover.
            if e.kind() == std::io::ErrorKind::WouldBlock {
                Ok(None)
            } else {
                Err(StateError::Io(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // try_acquire_state_lock() touches a real file under $XDG_STATE_HOME
    // and can race with concurrent test threads, so serialize the test
    // body and isolate state by pointing $XDG_STATE_HOME at a tempdir.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn with_isolated_state_dir(f: impl FnOnce(&std::path::Path)) {
        let _g = SERIAL.lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let prev = std::env::var_os("XDG_STATE_HOME");
        std::env::set_var("XDG_STATE_HOME", dir.path());
        f(dir.path());
        match prev {
            Some(v) => std::env::set_var("XDG_STATE_HOME", v),
            None => std::env::remove_var("XDG_STATE_HOME"),
        }
    }

    #[test]
    fn first_caller_acquires_lock() {
        with_isolated_state_dir(|_| {
            let lock = try_acquire_state_lock().unwrap();
            assert!(lock.is_some(), "first acquirer must succeed");
        });
    }

    #[test]
    fn second_caller_gets_none_while_first_alive() {
        with_isolated_state_dir(|_| {
            let first = try_acquire_state_lock().unwrap();
            assert!(first.is_some());
            let second = try_acquire_state_lock().unwrap();
            assert!(
                second.is_none(),
                "second acquirer must observe lock contention"
            );
        });
    }

    #[test]
    fn lock_releases_on_drop() {
        with_isolated_state_dir(|_| {
            let first = try_acquire_state_lock().unwrap();
            assert!(first.is_some());
            drop(first);
            let again = try_acquire_state_lock().unwrap();
            assert!(again.is_some(), "lock must be re-acquirable after drop");
        });
    }
}
