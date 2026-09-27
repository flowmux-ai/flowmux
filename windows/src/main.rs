// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use clap::Parser;
    use flowmux_windows::{
        command::Cli,
        native::{host, ipc, wide},
    };
    use windows_sys::Win32::{System::Console::*, UI::WindowsAndMessaging::*};
    let arguments: Vec<_> = std::env::args_os().collect();
    let result = if arguments.len() == 1 || arguments.get(1).is_some_and(|arg| arg == "--cwd") {
        host::run(arguments.get(2).map(std::path::PathBuf::from))
    } else {
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
        ipc::run(Cli::parse())
    };
    if let Err(error) = result {
        host::report(&format!("{error:#}"));
        unsafe {
            MessageBoxW(
                std::ptr::null_mut(),
                wide(format!("{error:#}")).as_ptr(),
                wide("flowmux").as_ptr(),
                MB_OK | MB_ICONERROR,
            );
        }
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This host targets Windows. Use the root workspace on Linux or macOS.");
    std::process::exit(1);
}
