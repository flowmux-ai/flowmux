// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-window local IPC. A protected DACL restricts clients to the owning user.
use super::{checked, data_dir, wide};
use crate::{
    command::{Cli, Command, Request},
    protocol::MAX_MESSAGE_BYTES,
};
use anyhow::Context;
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    sync::{mpsc, Arc},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
};

mod discovery;
mod server;
mod transport;
pub use server::{Reply, Server};

#[cfg(test)]
mod tests;

// Self-relative security descriptors can be copied as bytes after conversion.
fn user_descriptor() -> anyhow::Result<Vec<u8>> {
    unsafe {
        let mut raw_token = std::ptr::null_mut();
        checked(OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_QUERY,
            &mut raw_token,
        ))?;
        let token = OwnedHandle::from_raw_handle(raw_token);
        let mut size = 0;
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut size,
        );
        let mut storage = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        checked(GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            storage.as_mut_ptr().cast(),
            size,
            &mut size,
        ))?;
        let user = &*storage.as_ptr().cast::<TOKEN_USER>();
        let mut sid = std::ptr::null_mut();
        checked(ConvertSidToStringSidW(user.User.Sid, &mut sid))?;
        let length = (0..).take_while(|i| *sid.add(*i) != 0).count();
        let sid_text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, length));
        LocalFree(sid.cast());
        let sddl = wide(format!("D:P(A;;GA;;;SY)(A;;GA;;;{sid_text})"));
        let mut descriptor = std::ptr::null_mut();
        let mut length = 0;
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            &mut length,
        ))?;
        let bytes = std::slice::from_raw_parts(descriptor.cast::<u8>(), length as usize).to_vec();
        LocalFree(descriptor);
        Ok(bytes)
    }
}

fn make_pipe(name: &str, descriptor: &[u8], first: bool) -> anyhow::Result<OwnedHandle> {
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr() as *mut _,
        bInheritHandle: 0,
    };
    let flags = PIPE_ACCESS_DUPLEX
        | FILE_FLAG_OVERLAPPED
        | if first {
            FILE_FLAG_FIRST_PIPE_INSTANCE
        } else {
            0
        };
    let handle = unsafe {
        CreateNamedPipeW(
            wide(name).as_ptr(),
            flags,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            server::MAX_CLIENTS,
            65536,
            65536,
            5000,
            &security,
        )
    };
    anyhow::ensure!(
        handle != INVALID_HANDLE_VALUE,
        "CreateNamedPipe failed: {}",
        std::io::Error::last_os_error()
    );
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

pub fn run(cli: Cli) -> anyhow::Result<()> {
    if matches!(cli.command, Command::ShellIntegration) {
        print!("{}", include_str!("../../shell/powershell.ps1"));
        return Ok(());
    }
    if matches!(cli.command, Command::Doctor) {
        super::conpty::api()?;
        let version =
            wry::webview_version().context("Microsoft Edge WebView2 Runtime is not installed")?;
        let major: u32 = version.split('.').next().unwrap_or("").parse().unwrap_or(0);
        anyhow::ensure!(
            major >= 109,
            "WebView2 Runtime 109 or newer is required (found {version})"
        );
        println!(
            "{}",
            json!({"platform":"windows","webview2":version,"status":"ok", "background_testing":cfg!(debug_assertions)})
        );
        return Ok(());
    }
    let explicit = cli.pipe.or_else(|| std::env::var("FLOWMUX_PIPE_NAME").ok());
    let candidates = if let Some(name) = &explicit {
        // A stale explicit or inherited endpoint must never select another window.
        discovery::pipe_pid(name)?;
        vec![name.clone()]
    } else {
        discovery::candidates(&data_dir()?.join("instances"))
    };
    let mut connected = None;
    let mut last_error = None;
    for name in candidates {
        match open_verified_pipe(&name) {
            Ok(file) => {
                connected = Some((file, name));
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let (mut file, connected_name) = connected.ok_or_else(|| {
        let detail = last_error
            .map(|error| format!(": {error:#}"))
            .unwrap_or_default();
        if let Some(name) = explicit {
            anyhow::anyhow!("Could not connect to requested Windows flowmux pipe {name}{detail}")
        } else {
            anyhow::anyhow!("No reachable Windows flowmux window; launch flowmux.exe first{detail}")
        }
    })?;
    let caller_surface =
        if std::env::var("FLOWMUX_PIPE_NAME").as_deref() == Ok(connected_name.as_str()) {
            std::env::var("FLOWMUX_SURFACE_ID")
                .ok()
                .map(|value| uuid::Uuid::parse_str(&value))
                .transpose()
                .context("invalid FLOWMUX_SURFACE_ID")?
        } else {
            None
        };
    let mut bytes = serde_json::to_vec(&Request {
        command: cli.command,
        caller_cwd: caller_surface.and_then(|_| std::env::current_dir().ok()),
        caller_surface,
    })?;
    anyhow::ensure!(bytes.len() < MAX_MESSAGE_BYTES, "command is too large");
    bytes.push(b'\n');
    let stop = transport::Event::new()?;
    file.write_all(&bytes, Instant::now() + Duration::from_secs(5), &stop)
        .context("sending IPC request; command outcome may be unknown (not retried)")?;
    let frame = file
        .read_frame(
            server::MAX_REPLY_BYTES,
            Instant::now() + Duration::from_secs(25),
            &stop,
        )
        .context("waiting for IPC reply; command may already have executed (not retried)")?;
    // Close before printing: slow redirected stdout must not hold a server slot.
    drop(file);
    let value: Value = serde_json::from_slice(&frame)?;
    if let Some(error) = value.get("error") {
        anyhow::bail!("{}", error.as_str().unwrap_or("IPC error"));
    }
    if !cli.json && value["text"].is_string() {
        println!("{}", value["text"].as_str().unwrap());
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}

// Validate the OS-reported owner before sending any command bytes. A syntactically
// valid discovery record or pipe name alone is not process identity evidence.
fn open_verified_pipe(name: &str) -> anyhow::Result<transport::Pipe> {
    let expected = discovery::pipe_pid(name)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .custom_flags(FILE_FLAG_OVERLAPPED);
    let deadline = Instant::now() + Duration::from_secs(3);
    let file = loop {
        match options.open(name) {
            Ok(file) => break file,
            Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => {
                let left = deadline.saturating_duration_since(Instant::now());
                anyhow::ensure!(
                    !left.is_zero(),
                    "IPC connection capacity wait exceeded 3 seconds"
                );
                unsafe {
                    checked(WaitNamedPipeW(
                        wide(name).as_ptr(),
                        left.as_millis().max(1) as u32,
                    ))
                }
                .context("waiting for free IPC capacity (3-second limit)")?;
                // Another caller can take the released slot before CreateFile.
                // Retry only connection acquisition, never a transmitted request.
            }
            Err(error) => return Err(error).context("opening the requested pipe"),
        }
    };
    let mut actual = 0;
    unsafe {
        checked(GetNamedPipeServerProcessId(
            file.as_raw_handle(),
            &mut actual,
        ))
    }
    .context("querying the pipe server process")?;
    anyhow::ensure!(
        actual == expected,
        "pipe server PID mismatch: expected {expected}, found {actual}"
    );
    Ok(transport::Pipe::new(file.into())?)
}
