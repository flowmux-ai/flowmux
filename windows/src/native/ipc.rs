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
    let response = response(cli)?;
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(response.as_bytes())
        .context("writing CLI stdout")?;
    stdout.flush().context("flushing CLI stdout")?;
    Ok(())
}

fn response(cli: Cli) -> anyhow::Result<String> {
    if matches!(cli.command, Command::ShellIntegration) {
        return Ok(include_str!("../../shell/powershell.ps1").into());
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
        return Ok(format!(
            "{}\n",
            json!({"platform":"windows","webview2":version,"status":"ok", "background_testing":cfg!(debug_assertions)})
        ));
    }
    let json = cli.json;
    let find_output = matches!(
        &cli.command,
        Command::Browser {
            op: crate::browser::Op::Find(..)
        }
    );
    let screenshot_output = matches!(
        &cli.command,
        Command::Browser {
            op: crate::browser::Op::Screenshot(..)
        }
    );
    let action_output = matches!(&cli.command,Command::Browser{op} if op.is_action());
    let dom_output = matches!(
        &cli.command,
        Command::Browser {
            op: crate::browser::Op::Text { .. }
                | crate::browser::Op::Value { .. }
                | crate::browser::Op::Attr { .. }
                | crate::browser::Op::IsVisible { .. }
                | crate::browser::Op::IsEnabled { .. }
                | crate::browser::Op::IsChecked { .. }
                | crate::browser::Op::Count { .. }
                | crate::browser::Op::Wait { .. }
        }
    );
    let snapshot_output = matches!(
        &cli.command,
        Command::Browser {
            op: crate::browser::Op::Snapshot { .. }
        }
    );
    let value = request(cli)?;
    if !json && find_output {
        return Ok(format!(
            "{}\n",
            value["found"]
                .as_bool()
                .context("invalid page find response")?
        ));
    }
    if !json && screenshot_output {
        return Ok(format!(
            "{}\n",
            value["path"]
                .as_str()
                .context("invalid screenshot response")?
        ));
    }
    if !json && action_output {
        anyhow::ensure!(value["ok"] == true, "invalid browser action response");
        return Ok("ok\n".into());
    }
    if !json && dom_output {
        let result = &value["result"];
        return Ok(format!(
            "{}\n",
            result
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| result.to_string())
        ));
    }
    if !json && snapshot_output {
        return Ok(value["markdown"]
            .as_str()
            .context("invalid snapshot response")?
            .to_string());
    }
    if !json && value["text"].is_string() {
        Ok(format!("{}\n", value["text"].as_str().unwrap()))
    } else {
        Ok(format!("{}\n", serde_json::to_string_pretty(&value)?))
    }
}

/// One IPC submission, with no stdout formatting and no retry after dispatch.
pub(super) fn request(cli: Cli) -> anyhow::Result<Value> {
    let reply_budget = match &cli.command {
        Command::Agents | Command::ReportAgent(_) => Duration::from_secs(3),
        Command::Browser {
            op: crate::browser::Op::Wait { options, .. },
        } => options.ipc_budget(Duration::from_secs(25), 10)?,
        _ => Duration::from_secs(25),
    };
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
    let mut command = cli.command;
    // Explicit relative paths belong to the CLI process, not the GUI's cwd.
    if let Command::NewTab {
        cwd: Some(path), ..
    }
    | Command::NewWorkspace {
        cwd: Some(path), ..
    } = &mut command
    {
        *path = std::path::absolute(&*path)?;
    }
    if let Command::Browser {
        op: crate::browser::Op::Screenshot(args),
    } = &mut command
    {
        args.path = std::path::absolute(&args.path)?;
    }
    if let Command::Editor {
        op: crate::editor::Op::Open(args),
    } = &mut command
    {
        args.path = std::path::absolute(&args.path)?;
        if let Some(root) = &mut args.root {
            *root = std::path::absolute(&*root)?;
        }
    }
    if let Command::Files {
        op: crate::files_model::Op::Show(args),
    } = &mut command
    {
        if let Some(root) = &mut args.root {
            *root = std::path::absolute(&*root)?;
        }
    }
    let mut bytes = serde_json::to_vec(&Request {
        command,
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
            Instant::now() + reply_budget,
            &stop,
        )
        .context("waiting for IPC reply; command may already have executed (not retried)")?;
    // Close before printing: slow redirected stdout must not hold a server slot.
    drop(file);
    let value: Value = serde_json::from_slice(&frame)?;
    if let Some(error) = value.get("error") {
        anyhow::bail!("{}", error.as_str().unwrap_or("IPC error"));
    }
    Ok(value)
}

// Validate the OS-reported owner before sending any command bytes. A syntactically
// valid discovery record or pipe name alone is not process identity evidence.
fn open_verified_pipe(name: &str) -> anyhow::Result<transport::Pipe> {
    open_verified_pipe_until(name, Instant::now() + Duration::from_secs(3))
}

fn open_verified_pipe_until(name: &str, deadline: Instant) -> anyhow::Result<transport::Pipe> {
    let expected = discovery::pipe_pid(name)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .security_qos_flags(SECURITY_IDENTIFICATION)
        .custom_flags(FILE_FLAG_OVERLAPPED);
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

/// Read other native windows before a destructive worktree operation. Discovery
/// is only a hint; verify the pipe's live OS owner, and fail closed on uncertainty.
pub(super) fn other_window_paths(
    cancel: &std::sync::atomic::AtomicBool,
) -> anyhow::Result<Vec<PathBuf>> {
    use std::sync::atomic::Ordering;
    fn paths(value: &Value, out: &mut Vec<PathBuf>) {
        match value {
            Value::Array(values) => values.iter().for_each(|value| paths(value, out)),
            Value::Object(object) => {
                for (key, value) in object {
                    if matches!(key.as_str(), "cwd" | "workspace_root") {
                        if let Some(path) = value.as_str() {
                            out.push(path.into());
                        }
                    } else {
                        paths(value, out);
                    }
                }
            }
            _ => {}
        }
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut used = Vec::new();
    let request = serde_json::to_vec(&Request {
        command: Command::Tree,
        caller_surface: None,
        caller_cwd: None,
    })?;
    for name in discovery::candidates(&data_dir()?.join("instances")) {
        let pid = discovery::pipe_pid(&name)?;
        if pid == std::process::id() {
            continue;
        }
        anyhow::ensure!(
            !cancel.load(Ordering::Acquire) && Instant::now() < deadline,
            "Worktree usage check was cancelled or exceeded five seconds"
        );
        // Discovery can outlive its process and that PID can be reused. Only
        // an existing, kernel-verified pipe identifies another flowmux window.
        let mut pipe = match open_verified_pipe_until(&name, deadline) {
            Ok(pipe) => pipe,
            Err(error) if error.downcast_ref::<std::io::Error>().is_some_and(|e| {
                matches!(e.raw_os_error(), Some(code) if code == ERROR_FILE_NOT_FOUND as i32 || code == ERROR_PATH_NOT_FOUND as i32)
            }) => continue,
            Err(error) => return Err(error.context("Cannot verify another flowmux window's worktree usage")),
        };
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        if raw.is_null() {
            if unsafe { GetLastError() } == ERROR_INVALID_PARAMETER {
                continue;
            }
            anyhow::bail!("Cannot verify another flowmux window before removing a worktree");
        }
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
            continue;
        }
        let result = (|| -> anyhow::Result<Value> {
            let stop = transport::Event::new()?;
            let mut bytes = request.clone();
            bytes.push(b'\n');
            pipe.write_all(&bytes, deadline, &stop)?;
            let frame = pipe.read_frame(server::MAX_REPLY_BYTES, deadline, &stop)?;
            Ok(serde_json::from_slice(&frame)?)
        })();
        let tree = match result {
            Ok(tree) => tree,
            Err(_)
                if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0 =>
            {
                continue
            }
            Err(error) => {
                return Err(error.context("Cannot verify another flowmux window's worktree usage"))
            }
        };
        let workspaces = tree["workspaces"]
            .as_array()
            .context("Another flowmux window returned no workspace state")?;
        for workspace in workspaces.iter().filter(|w| w["ssh"].is_null()) {
            paths(workspace, &mut used);
        }
    }
    Ok(used)
}
