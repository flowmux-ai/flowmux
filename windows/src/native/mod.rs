// SPDX-License-Identifier: GPL-3.0-or-later
mod conpty;
pub mod entry;
pub mod host;
pub mod ipc;
mod session;
mod settings_store;
mod shell;
mod state_store;
mod usage_collect;
mod worktree_git;

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

pub fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain(Some(0)).collect()
}

pub fn checked(success: i32) -> anyhow::Result<()> {
    if success == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

pub fn data_dir() -> anyhow::Result<std::path::PathBuf> {
    let path = std::path::PathBuf::from(
        std::env::var_os("LOCALAPPDATA")
            .ok_or_else(|| anyhow::anyhow!("LOCALAPPDATA is unavailable"))?,
    )
    .join("flowmux")
    .join("windows");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
