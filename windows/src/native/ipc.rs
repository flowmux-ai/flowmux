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
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
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
        let emit = Arc::new(emit);
        let shutdown = Arc::new(shutdown);
        let server_name = name.clone();
        std::thread::Builder::new()
            .name("flowmux-ipc".into())
            .spawn(move || {
                let mut instance = first;
                loop {
                    let connected =
                        unsafe { ConnectNamedPipe(instance.as_raw_handle(), std::ptr::null_mut()) };
                    if connected == 0 && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED {
                        break;
                    }
                    let next = match make_pipe(&server_name, &descriptor, false) {
                        Ok(value) => value,
                        Err(_) => break,
                    };
                    let emit = emit.clone();
                    let shutdown = shutdown.clone();
                    // Each client has its own instance; a stalled caller does not block acceptance.
                    std::thread::spawn(move || {
                        let mut pipe = File::from(instance);
                        let mut quitting = false;
                        let response = (|| -> anyhow::Result<Value> {
                            let mut reader =
                                BufReader::new((&mut pipe).take((MAX_MESSAGE_BYTES + 1) as u64));
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
                    instance = next;
                }
            })?;
        let directory = data_dir()?.join("instances");
        std::fs::create_dir_all(&directory)?;
        let discovery = directory.join(format!("{}.json", std::process::id()));
        std::fs::write(
            &discovery,
            serde_json::to_vec(&json!({"pid":std::process::id(),"pipe":name}))?,
        )?;
        Ok(Self { name, discovery })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.discovery);
    }
}

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
    let mut candidates = Vec::new();
    if let Some(name) = explicit {
        candidates.push(name);
    } else {
        let directory = data_dir()?.join("instances");
        if let Ok(entries) = std::fs::read_dir(directory) {
            let mut entries: Vec<_> = entries.flatten().collect();
            entries.sort_by_key(|entry| {
                std::cmp::Reverse(entry.metadata().and_then(|m| m.modified()).ok())
            });
            for entry in entries {
                if let Ok(bytes) = std::fs::read(entry.path()) {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        if let Some(name) = value["pipe"].as_str() {
                            candidates.push(name.to_owned());
                        }
                    }
                }
            }
        }
    }
    let mut connected = None;
    for name in candidates {
        anyhow::ensure!(
            name.starts_with(r"\\.\pipe\flowmux-"),
            "not a local flowmux pipe"
        );
        for _ in 0..2 {
            match OpenOptions::new().read(true).write(true).open(&name) {
                Ok(file) => {
                    connected = Some((file, name.clone()));
                    break;
                }
                Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) => unsafe {
                    WaitNamedPipeW(wide(&name).as_ptr(), 3000);
                },
                Err(_) => break,
            }
        }
        if connected.is_some() {
            break;
        }
    }
    let (mut file, connected_name) =
        connected.context("No running Windows flowmux window; launch flowmux.exe first")?;
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
