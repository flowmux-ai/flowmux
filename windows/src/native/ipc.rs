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
    io::{BufRead, BufReader, Read, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::PathBuf,
    sync::{mpsc, Arc},
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::{Pipes::*, Threading::*},
};

mod discovery;

pub type Reply = mpsc::SyncSender<Value>;

pub struct Server {
    pub name: String,
    discovery: PathBuf,
}
impl Server {
    pub fn start(
        emit: impl Fn(Request, Reply) + Send + Sync + 'static,
        shutdown: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let name = format!(
            r"\\.\pipe\flowmux-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let descriptor = user_descriptor()?;
        // Create the first instance synchronously: startup cannot advertise an unbound endpoint.
        let first = make_pipe(&name, &descriptor, true)?;
        let discovery = discovery::publish(&data_dir()?.join("instances"), &name)?;
        // The guard removes the record if thread creation fails. The bound first
        // instance can already accept clients while the listener starts.
        let server = Self { name, discovery };
        let emit = Arc::new(emit);
        let shutdown = Arc::new(shutdown);
        let server_name = server.name.clone();
        let discovery = server.discovery.clone();
        std::thread::Builder::new()
            .name("flowmux-ipc".into())
            .spawn(move || {
                let mut instance = first;
                loop {
                    if let Err(error) = accept_connection(&instance) {
                        super::host::report(&format!("IPC listener stopped: {error:#}"));
                        break;
                    }
                    let next = match make_pipe(&server_name, &descriptor, false) {
                        Ok(value) => value,
                        Err(error) => {
                            super::host::report(&format!(
                                "IPC instance creation failed: {error:#}"
                            ));
                            break;
                        }
                    };
                    let emit = emit.clone();
                    let shutdown = shutdown.clone();
                    // Each client has its own instance; a stalled caller does not block acceptance.
                    let worker = std::thread::Builder::new()
                        .name("flowmux-ipc-client".into())
                        .spawn(move || {
                            let mut pipe = File::from(instance);
                            let mut quitting = false;
                            let response = (|| -> anyhow::Result<Value> {
                                let mut reader = BufReader::new(
                                    (&mut pipe).take((MAX_MESSAGE_BYTES + 1) as u64),
                                );
                                let mut line = String::new();
                                reader.read_line(&mut line)?;
                                anyhow::ensure!(
                                    line.len() <= MAX_MESSAGE_BYTES && line.ends_with('\n'),
                                    "invalid IPC frame"
                                );
                                let command: Request = serde_json::from_str(&line)?;
                                quitting = matches!(command.command, Command::Quit { .. });
                                let (send, receive) = mpsc::sync_channel(1);
                                emit(command, send);
                                receive
                                    .recv_timeout(Duration::from_secs(15))
                                    .context("window did not answer within 15 seconds")
                            })()
                            .unwrap_or_else(|error| json!({"error":error.to_string()}));
                            if let Ok(mut bytes) = serde_json::to_vec(&response) {
                                bytes.push(b'\n');
                                let _ = pipe.write_all(&bytes);
                                // Client reads the reply before closing. Flush waits only in this worker.
                                unsafe {
                                    FlushFileBuffers(pipe.as_raw_handle());
                                    DisconnectNamedPipe(pipe.as_raw_handle());
                                }
                            }
                            if quitting && response.get("error").is_none() {
                                shutdown();
                            }
                        });
                    if let Err(error) = worker {
                        super::host::report(&format!("IPC client thread creation failed: {error}"));
                    }
                    instance = next;
                }
                discovery::remove_if_current(&discovery, &server_name);
            })?;
        Ok(server)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        discovery::remove_if_current(&self.discovery, &self.name);
    }
}

fn accept_connection(instance: &OwnedHandle) -> anyhow::Result<()> {
    loop {
        let connected = unsafe { ConnectNamedPipe(instance.as_raw_handle(), std::ptr::null_mut()) };
        if connected != 0 {
            return Ok(());
        }
        let error = unsafe { GetLastError() };
        match error {
            ERROR_PIPE_CONNECTED => return Ok(()),
            // A client can open and close this instance before ConnectNamedPipe.
            // Reset that abandoned connection, preserving the listener and name.
            ERROR_NO_DATA => unsafe {
                checked(DisconnectNamedPipe(instance.as_raw_handle()))
                    .context("could not reset abandoned IPC connection")?;
            },
            _ => {
                return Err(std::io::Error::from_raw_os_error(error as i32))
                    .context("ConnectNamedPipe failed")
            }
        }
    }
}

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
            PIPE_UNLIMITED_INSTANCES,
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
    file.write_all(&bytes)?;
    let mut line = String::new();
    BufReader::new((&mut file).take(16 * MAX_MESSAGE_BYTES as u64)).read_line(&mut line)?;
    anyhow::ensure!(line.ends_with('\n'), "incomplete or oversized IPC reply");
    let value: Value = serde_json::from_str(&line)?;
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
fn open_verified_pipe(name: &str) -> anyhow::Result<File> {
    let expected = discovery::pipe_pid(name)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .security_qos_flags(SECURITY_IDENTIFICATION);
    let mut file = options.open(name);
    if file
        .as_ref()
        .is_err_and(|error| error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32))
    {
        unsafe { checked(WaitNamedPipeW(wide(name).as_ptr(), 3000)) }
            .context("waiting for the requested pipe")?;
        file = options.open(name);
    }
    let file = file.context("opening the requested pipe")?;
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
    Ok(file)
}
