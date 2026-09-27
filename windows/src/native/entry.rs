// SPDX-License-Identifier: GPL-3.0-or-later
//! Console entry points never open diagnostic dialogs. The desktop shortcut
//! targets the GUI executable; flowmux.com provides normal shell wait semantics.
use crate::command::{parse_entry, Cli, Invocation};
use anyhow::Context;
use clap::Parser;
use std::io::Write;
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    Storage::FileSystem::{GetFileType, FILE_TYPE_CHAR, FILE_TYPE_DISK, FILE_TYPE_PIPE},
    System::Console::*,
    UI::WindowsAndMessaging::*,
};

fn attach_parent() {
    // AttachConsole may replace the process standard-handle table. Retain file
    // and pipe redirection (including NUL); never allocate a new console for
    // the GUI binary. A character device is not necessarily a console.
    unsafe {
        let redirected = [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE].map(|which| {
            let handle = GetStdHandle(which);
            let kind = GetFileType(handle);
            let mut console_mode = 0;
            let keep = !handle.is_null()
                && handle != INVALID_HANDLE_VALUE
                && (matches!(kind, FILE_TYPE_DISK | FILE_TYPE_PIPE)
                    || (kind == FILE_TYPE_CHAR && GetConsoleMode(handle, &mut console_mode) == 0));
            (which, handle, keep)
        });
        AttachConsole(ATTACH_PARENT_PROCESS);
        for (which, handle, keep) in redirected {
            if keep {
                SetStdHandle(which, handle);
            }
        }
    }
}

fn parse_error(error: clap::Error) -> i32 {
    let code = error.exit_code();
    if error.print().is_err() {
        1
    } else {
        code
    }
}

fn runtime_error(name: &str, json: bool, error: anyhow::Error) -> i32 {
    let message = format!("{error:#}");
    if json {
        let _ = writeln!(
            std::io::stderr().lock(),
            "{}",
            serde_json::json!({"error":message})
        );
    } else {
        let _ = writeln!(std::io::stderr().lock(), "{name}: {message}");
    }
    1
}

pub fn client(cli: Cli, name: &str) -> i32 {
    let json = cli.json;
    match super::ipc::run(cli) {
        Ok(()) => 0,
        Err(error) => runtime_error(name, json, error),
    }
}

pub fn control_main() -> i32 {
    match Cli::try_parse() {
        Ok(cli) => client(cli, "flowmuxctl"),
        Err(error) => parse_error(error),
    }
}

fn spawned(pid: u32) -> anyhow::Result<()> {
    writeln!(
        std::io::stdout().lock(),
        "{}",
        serde_json::json!({"spawned_pid":pid})
    )?;
    Ok(())
}

fn launch_command(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> anyhow::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut out = Vec::new();
    for value in arguments {
        if !out.is_empty() {
            out.push(32);
        }
        out.push(34);
        let mut slashes = 0;
        for unit in value.encode_wide() {
            anyhow::ensure!(unit != 0, "launch argument contains NUL");
            if unit == 92 {
                slashes += 1;
                continue;
            }
            out.extend(std::iter::repeat_n(
                92,
                if unit == 34 { slashes * 2 + 1 } else { slashes },
            ));
            slashes = 0;
            out.push(unit);
        }
        out.extend(std::iter::repeat_n(92, slashes * 2));
        out.push(34);
    }
    anyhow::ensure!(
        out.len() < 32767,
        "expanded launcher command exceeds Windows limit"
    );
    out.push(0);
    Ok(out)
}

fn launch_gui(arguments: &[std::ffi::OsString]) -> anyhow::Result<u32> {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::*;
    let executable = std::env::current_exe()?.with_file_name("flowmux.exe");
    let mut command = launch_command(
        std::iter::once(executable.as_os_str().to_owned()).chain(arguments.iter().cloned()),
    )?;
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of_val(&startup) as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    let _errors = super::session::ErrorMode::suppress_dialogs()?;
    unsafe {
        // No handle inheritance: Stdio::null alone still allowed the GUI to
        // retain the launcher's old redirected pipes and prevented EOF.
        super::checked(CreateProcessW(
            super::wide(&executable).as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            DETACHED_PROCESS,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        ))
        .with_context(|| format!("Cannot launch {}", executable.display()))?;
        let _process = OwnedHandle::from_raw_handle(process.hProcess);
        let _thread = OwnedHandle::from_raw_handle(process.hThread);
    }
    Ok(process.dwProcessId)
}

pub fn console_main() -> i32 {
    let arguments: Vec<_> = std::env::args_os().collect();
    match parse_entry(arguments.clone()) {
        Err(error) => parse_error(error),
        Ok(Invocation::Client(cli)) => client(cli, "flowmux"),
        Ok(Invocation::Launch { json, .. }) => {
            let launch = || -> anyhow::Result<()> {
                let pid = launch_gui(&arguments[1..])?;
                // Report process creation only: readiness and startup failures
                // belong to the new host. Never kill it if stdout is closed.
                if json {
                    spawned(pid)?;
                }
                Ok(())
            };
            match launch() {
                Ok(()) => 0,
                Err(error) => runtime_error("flowmux", json, error),
            }
        }
    }
}

pub fn gui_main() -> i32 {
    match parse_entry(std::env::args_os()) {
        Err(error) => {
            attach_parent();
            parse_error(error)
        }
        Ok(Invocation::Client(cli)) => {
            attach_parent();
            client(cli, "flowmux")
        }
        Ok(Invocation::Launch { options, json }) => {
            if json {
                // Redirected GUI invocations can observe their own process ID.
                // The console launcher supplies its own receipt independently.
                let _ = spawned(std::process::id());
            }
            if let Err(error) = super::host::run(options) {
                let message = format!("{error:#}");
                super::host::report(&message);
                let code = runtime_error("flowmux", json, error);
                let background = cfg!(debug_assertions)
                    && std::env::var("FLOWMUX_TEST_BACKGROUND").as_deref() == Ok("1");
                if !(json || background) {
                    unsafe {
                        MessageBoxW(
                            std::ptr::null_mut(),
                            super::wide(message).as_ptr(),
                            super::wide("flowmux").as_ptr(),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
                code
            } else {
                0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, os::windows::ffi::OsStringExt};
    #[test]
    fn launcher_preserves_utf16_empty_quotes_and_trailing_backslashes() {
        for value in ["", "한글 한 😀 & #", "a\\\"b\\", "C:\\한글 \\"] {
            assert_eq!(
                launch_command([OsString::from(value)]).unwrap(),
                super::super::wide(crate::shell::quote_arg(value))
            );
        }
        assert_eq!(
            launch_command([OsString::from_wide(&[0xd800, 92])]).unwrap(),
            vec![34, 0xd800, 92, 92, 34, 0]
        );
        assert!(launch_command([OsString::from_wide(&[0])]).is_err());
        assert!(launch_command([OsString::from("한".repeat(32767))]).is_err());
    }
}
