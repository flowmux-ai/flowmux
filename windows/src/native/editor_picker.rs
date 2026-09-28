// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned native Open File dialog. Never invoked by a background host.
use anyhow::{ensure, Context};
use std::{
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use windows::{
    core::{HRESULT, PCWSTR, PWSTR},
    Win32::{
        Foundation::{ERROR_CANCELLED, HWND as DialogOwner},
        System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER},
        UI::Shell::{
            FileOpenDialog, IFileOpenDialog, IShellItem, SHCreateItemFromParsingName,
            FOS_DONTADDTORECENT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_NOCHANGEDIR,
            FOS_PATHMUSTEXIST, SIGDN_FILESYSPATH,
        },
    },
};
use windows_sys::Win32::Foundation::HWND;

/// IShellItem::GetDisplayName transfers a COM-task-allocated UTF-16 string.
struct SelectedName {
    raw: PWSTR,
}
impl Drop for SelectedName {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.raw.0.cast_const().cast())) };
    }
}

/// Call on the host's initialized STA thread only for a foreground user action,
/// with its captured workspace cwd. The wrapper does not change process cwd.
/// The caller retains its original target across the modal message loop and
/// submits the selected path to the worker for canonical containment/file checks.
/// Physical dialog, keyboard and IME behavior still require interactive evidence.
pub(super) fn pick(
    owner: HWND,
    initial_dir: &Path,
    background: bool,
) -> anyhow::Result<Option<PathBuf>> {
    // Keep this first: even invalid inputs in a hidden verifier cannot reach a
    // native dialog, request focus or alter any desktop state through this API.
    ensure!(!background, "Open File is unavailable in background mode");
    ensure!(!owner.is_null(), "Open File requires an owning window");
    crate::editor::validate_path(initial_dir).context("invalid Open File starting directory")?;
    ensure!(
        initial_dir.is_absolute(),
        "Open File starting directory must be absolute"
    );

    let initial: Vec<u16> = initial_dir
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) }
            .context("cannot create Open File dialog")?;
    let options = unsafe { dialog.GetOptions() }.context("cannot read Open File options")?;
    unsafe {
        dialog.SetOptions(
            options
                | FOS_FORCEFILESYSTEM
                | FOS_FILEMUSTEXIST
                | FOS_PATHMUSTEXIST
                | FOS_NOCHANGEDIR
                | FOS_DONTADDTORECENT,
        )
    }
    .context("cannot configure Open File dialog")?;
    let folder: IShellItem = unsafe { SHCreateItemFromParsingName(PCWSTR(initial.as_ptr()), None) }
        .context("cannot resolve Open File starting directory")?;
    // This is the default when the shell has no remembered folder for the app;
    // it intentionally does not force a folder over the user's dialog history.
    unsafe { dialog.SetDefaultFolder(&folder) }
        .context("cannot set Open File starting directory")?;
    unsafe { dialog.SetTitle(windows::core::w!("Open File")) }
        .context("cannot set Open File title")?;

    // The owner is borrowed; the COM dialog owns its window. No hooks or other
    // window/focus/clipboard APIs run. COM references release on every return.
    if let Err(error) = unsafe { dialog.Show(Some(DialogOwner(owner))) } {
        if error.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
            return Ok(None);
        }
        return Err(error).context("Open File failed");
    }

    let selected = unsafe { dialog.GetResult() }.context("Open File returned no file")?;
    let filename = SelectedName {
        raw: unsafe { selected.GetDisplayName(SIGDN_FILESYSPATH) }
            .context("selected item has no filesystem path")?,
    };
    ensure!(!filename.raw.is_null(), "Open File returned no path");
    // The shell contract supplies a terminated allocation. Bound the scan and
    // Rust copy to 32,768 UTF-16 units including its terminator; reject invalid
    // surrogates rather than losing filename codepoints through replacement.
    let end = (0..=crate::editor::MAX_PATH_UNITS)
        .find(|index| unsafe { *filename.raw.0.add(*index) == 0 })
        .context("selected file path exceeds the Open File path limit")?;
    let units = unsafe { std::slice::from_raw_parts(filename.raw.0, end) };
    let text = String::from_utf16(units).context("selected file path is not valid Unicode")?;
    let path = PathBuf::from(text);
    crate::editor::validate_path(&path).context("invalid selected file path")?;
    ensure!(path.is_absolute(), "selected file path must be absolute");
    Ok(Some(path))
}
