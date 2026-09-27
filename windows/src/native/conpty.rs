// SPDX-License-Identifier: GPL-3.0-or-later
//! Use the pinned app-local ConPTY implementation. The inbox implementation on
//! the validation machine leaked one process handle per closed pseudoconsole.
//! Never inspect opaque HPCON internals or close handles owned by the OS.
use super::wide;
use anyhow::Context;
use std::sync::OnceLock;
use windows_sys::Win32::{
    Foundation::*,
    System::{Console::*, LibraryLoader::*},
};

type Create = unsafe extern "system" fn(COORD, HANDLE, HANDLE, u32, *mut HPCON) -> i32;
type Resize = unsafe extern "system" fn(HPCON, COORD) -> i32;
type Close = unsafe extern "system" fn(HPCON);
type Release = unsafe extern "system" fn(HPCON) -> i32;
pub struct Api {
    _module: isize,
    pub create: Create,
    pub resize: Resize,
    pub close: Close,
    pub release: Release,
}
static API: OnceLock<Result<Api, String>> = OnceLock::new();

pub fn api() -> anyhow::Result<&'static Api> {
    API.get_or_init(|| load().map_err(|error| format!("{error:#}")))
        .as_ref()
        .map_err(|message| anyhow::anyhow!(message.clone()))
}

fn load() -> anyhow::Result<Api> {
    let directory = std::env::current_exe()?
        .parent()
        .context("Executable has no directory")?
        .to_owned();
    let path = directory.join("conpty.dll");
    anyhow::ensure!(directory.join("x64/OpenConsole.exe").is_file(),
        "ConPTY support files are missing. Reinstall flowmux or run windows/scripts/fetch-conpty.py for this build directory.");
    unsafe {
        // Restrict dependency lookup to the DLL's own directory and Windows System32.
        let module = LoadLibraryExW(
            wide(&path).as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
        anyhow::ensure!(
            !module.is_null(),
            "Cannot load {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        );
        let result = (|| -> anyhow::Result<Api> {
            let create = GetProcAddress(module, c"ConptyCreatePseudoConsole".as_ptr().cast())
                .context("ConPTY create export is missing")?;
            let resize = GetProcAddress(module, c"ConptyResizePseudoConsole".as_ptr().cast())
                .context("ConPTY resize export is missing")?;
            let close = GetProcAddress(module, c"ConptyClosePseudoConsole".as_ptr().cast())
                .context("ConPTY close export is missing")?;
            let release = GetProcAddress(module, c"ConptyReleasePseudoConsole".as_ptr().cast())
                .context("ConPTY release export is missing")?;
            Ok(Api {
                _module: module as isize,
                create: std::mem::transmute::<unsafe extern "system" fn() -> isize, Create>(create),
                resize: std::mem::transmute::<unsafe extern "system" fn() -> isize, Resize>(resize),
                close: std::mem::transmute::<unsafe extern "system" fn() -> isize, Close>(close),
                release: std::mem::transmute::<unsafe extern "system" fn() -> isize, Release>(
                    release,
                ),
            })
        })();
        if result.is_err() {
            FreeLibrary(module);
        }
        // A successful module is kept for the process lifetime, shared by all sessions.
        result
    }
}
