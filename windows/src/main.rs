// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    use clap::Parser;
    use flowmux_windows::{
        command::{Cli, Launch},
        native::{host, ipc, wide},
    };
    use windows_sys::Win32::{System::Console::*, UI::WindowsAndMessaging::*};
    let arguments: Vec<_> = std::env::args_os().collect();
    let result = if arguments.len() == 1
        || arguments.get(1).is_some_and(|arg| {
            ["--cwd", "--new-window", "--restore-window", "--temporary"]
                .iter()
                .any(|s| arg == s)
        }) {
        Launch::try_parse_from(arguments)
            .map_err(anyhow::Error::from)
            .and_then(host::run)
    } else {
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
        ipc::run(Cli::parse())
    };
    if let Err(error) = result {
        host::report(&format!("{error:#}"));
        if !(cfg!(debug_assertions)
            && std::env::var("FLOWMUX_TEST_BACKGROUND").as_deref() == Ok("1"))
        {
            unsafe {
                MessageBoxW(
                    std::ptr::null_mut(),
                    wide(format!("{error:#}")).as_ptr(),
                    wide("flowmux").as_ptr(),
                    MB_OK | MB_ICONERROR,
                );
            }
        }
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This host targets Windows. Use the root workspace on Linux or macOS.");
    std::process::exit(1);
}
