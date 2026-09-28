// SPDX-License-Identifier: GPL-3.0-or-later
use super::super::{checked, wide, worktree_git};
use super::{check, error, failed, field, Result, BODY_LIMIT};
use crate::usage::{self, Provider, ProviderRefresh, UsageError, UsageErrorKind};
use chrono::{Local, Utc};
use serde_json::{json, Value};
use std::{
    ffi::OsString,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::Instant,
};
use windows_sys::Win32::System::JobObjects::{AssignProcessToJobObject, TerminateJobObject};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::{ReadFile, WriteFile},
    System::{Pipes::PeekNamedPipe, Threading::*},
};

struct Executable {
    program: PathBuf,
    prefix: Vec<OsString>,
}

fn executable() -> Result<Executable> {
    let mut dirs = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|path| path.is_absolute())
        .collect::<Vec<_>>();
    if let Some(appdata) = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        dirs.push(appdata.join("npm"));
    }
    resolve(&dirs).ok_or_else(|| {
        error(
            UsageErrorKind::NotInstalled,
            "Codex CLI was not found on the absolute Windows PATH.",
        )
    })
}

fn resolve(dirs: &[PathBuf]) -> Option<Executable> {
    for dir in dirs.iter().filter(|path| path.is_absolute()) {
        let program = dir.join("codex.exe");
        if program.is_file() {
            return Some(Executable {
                program,
                prefix: Vec::new(),
            });
        }
        // Never interpret an npm .cmd wrapper as shell text. Use its known JS entry directly.
        let script = dir.join("node_modules/@openai/codex/bin/codex.js");
        if script.is_file() {
            let node = std::iter::once(dir)
                .chain(dirs.iter())
                .filter(|p| p.is_absolute())
                .map(|dir| dir.join("node.exe"))
                .find(|path| path.is_file());
            if let Some(program) = node {
                return Some(Executable {
                    program,
                    prefix: vec![script.into_os_string()],
                });
            }
        }
    }
    None
}

struct Process {
    job: OwnedHandle,
    process: OwnedHandle,
    input: OwnedHandle,
    output: OwnedHandle,
    error: OwnedHandle,
}
impl Drop for Process {
    fn drop(&mut self) {
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
            WaitForSingleObject(self.process.as_raw_handle(), 500);
        }
    }
}
fn io() -> UsageError {
    error(
        UsageErrorKind::Io,
        "The Codex usage process could not be read or written.",
    )
}

impl Process {
    fn start(executable: &Executable) -> Result<Self> {
        let _errors = super::super::session::ErrorMode::suppress_dialogs().map_err(|_| io())?;
        let job = worktree_git::job().map_err(|_| io())?;
        let (input, input_writer) = worktree_git::pipe().map_err(|_| io())?;
        let (output, output_writer) = worktree_git::pipe().map_err(|_| io())?;
        let (stderr, error_writer) = worktree_git::pipe().map_err(|_| io())?;
        let handles = [
            input.as_raw_handle(),
            output_writer.as_raw_handle(),
            error_writer.as_raw_handle(),
        ];
        for handle in handles {
            unsafe {
                checked(SetHandleInformation(
                    handle,
                    HANDLE_FLAG_INHERIT,
                    HANDLE_FLAG_INHERIT,
                ))
                .map_err(|_| io())?;
            }
        }
        let mut attributes = worktree_git::Attributes::new(&handles).map_err(|_| io())?;
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = attributes.ptr();
        let mut args = vec![executable.program.as_os_str().to_owned()];
        args.extend(executable.prefix.iter().cloned());
        args.push("app-server".into());
        let mut command = worktree_git::command(&args).map_err(|_| io())?;
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        unsafe {
            checked(CreateProcessW(
                wide(&executable.program).as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_NO_WINDOW | CREATE_SUSPENDED | EXTENDED_STARTUPINFO_PRESENT,
                std::ptr::null(),
                std::ptr::null(),
                &startup.StartupInfo,
                &mut process,
            ))
            .map_err(|_| {
                error(
                    UsageErrorKind::Io,
                    "Could not start the Codex usage collector.",
                )
            })?;
        }
        let process = unsafe {
            (
                OwnedHandle::from_raw_handle(process.hProcess),
                OwnedHandle::from_raw_handle(process.hThread),
            )
        };
        unsafe {
            if AssignProcessToJobObject(job.as_raw_handle(), process.0.as_raw_handle()) == 0 {
                TerminateProcess(process.0.as_raw_handle(), 1);
                WaitForSingleObject(process.0.as_raw_handle(), 500);
                return Err(io());
            }
            if ResumeThread(process.1.as_raw_handle()) == u32::MAX {
                return Err(io());
            }
        }
        drop(input);
        drop(output_writer);
        drop(error_writer);
        Ok(Self {
            job,
            process: process.0,
            input: input_writer,
            output,
            error: stderr,
        })
    }
    fn write(&self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value).map_err(|_| io())?;
        bytes.push(b'\n');
        // Three fixed request frames total less than the pipe's default buffer;
        // never write user data or unbounded input to a non-reading child.
        if bytes.len() > 1024 {
            return Err(io());
        }
        let mut written = 0;
        unsafe {
            checked(WriteFile(
                self.input.as_raw_handle(),
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            ))
            .map_err(|_| io())?;
        }
        if written as usize != bytes.len() {
            return Err(io());
        }
        Ok(())
    }
}

#[derive(Default)]
struct Responses {
    limits: Option<Value>,
    tokens: Option<Value>,
    failure: Option<UsageError>,
}
fn drain(
    pipe: &OwnedHandle,
    buffer: &mut Vec<u8>,
    remaining: &mut usize,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<bool> {
    check(cancel, deadline)?;
    let mut available = 0;
    unsafe {
        if PeekNamedPipe(
            pipe.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        ) == 0
        {
            if GetLastError() == ERROR_BROKEN_PIPE {
                return Ok(false);
            }
            return Err(io());
        }
    }
    if available == 0 {
        return Ok(false);
    }
    if *remaining == 0 {
        return Err(error(
            UsageErrorKind::InvalidData,
            "Codex usage output exceeded 1 MiB.",
        ));
    }
    let mut chunk = [0u8; 8192];
    let count = (available as usize).min(chunk.len()).min(*remaining);
    let mut read = 0;
    unsafe {
        checked(ReadFile(
            pipe.as_raw_handle(),
            chunk.as_mut_ptr(),
            count as u32,
            &mut read,
            std::ptr::null_mut(),
        ))
        .map_err(|_| io())?;
    }
    buffer.extend_from_slice(&chunk[..read as usize]);
    *remaining -= read as usize;
    Ok(read > 0)
}

fn exchange(executable: &Executable, cancel: &AtomicBool, deadline: Instant) -> Responses {
    let mut response = Responses::default();
    let result = (|| {
        check(cancel, deadline)?;
        let process = Process::start(executable)?;
        process.write(&json!({"id":0,"method":"initialize","params":{"clientInfo":{"name":"flowmux","title":"flowmux","version":env!("CARGO_PKG_VERSION")}}}))?;
        let mut initialized = false;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut remaining = BODY_LIMIT;
        loop {
            check(cancel, deadline)?;
            let read = drain(
                &process.output,
                &mut stdout,
                &mut remaining,
                cancel,
                deadline,
            )?;
            drain(
                &process.error,
                &mut stderr,
                &mut remaining,
                cancel,
                deadline,
            )?;
            stderr.clear(); // Never retain or expose provider stderr (it can contain credentials).
            while let Some(end) = stdout.iter().position(|byte| *byte == b'\n') {
                let line = stdout.drain(..=end).collect::<Vec<_>>();
                let Ok(value) = serde_json::from_slice::<Value>(&line) else {
                    continue;
                };
                match value.get("id").and_then(Value::as_u64) {
                    Some(0) if !initialized => {
                        if value.get("error").is_some_and(|v| !v.is_null()) {
                            return Err(usage::codex_limits(&value).err().unwrap_or_else(io));
                        }
                        if !value.get("result").is_some_and(Value::is_object) {
                            return Err(error(
                                UsageErrorKind::InvalidData,
                                "Codex initialization returned an invalid response.",
                            ));
                        }
                        process.write(&json!({"method":"initialized","params":{}}))?;
                        process.write(&json!({"id":1,"method":"account/rateLimits/read"}))?;
                        process.write(&json!({"id":2,"method":"account/usage/read"}))?;
                        initialized = true;
                    }
                    Some(1) if initialized => response.limits = Some(value),
                    Some(2) if initialized => response.tokens = Some(value),
                    _ => {}
                }
            }
            if response.limits.is_some() && response.tokens.is_some() {
                return Ok(());
            }
            let state = unsafe {
                WaitForSingleObject(process.process.as_raw_handle(), if read { 0 } else { 5 })
            };
            // An exiting child may have written after our previous pipe peek.
            if state == WAIT_OBJECT_0
                && !read
                && !drain(
                    &process.output,
                    &mut stdout,
                    &mut remaining,
                    cancel,
                    deadline,
                )?
            {
                return Err(error(
                    UsageErrorKind::InvalidData,
                    "Codex usage response was incomplete.",
                ));
            }
            if state != WAIT_OBJECT_0 && state != WAIT_TIMEOUT {
                return Err(io());
            }
        }
    })();
    response.failure = result.err();
    response
}

pub(super) fn collect(cancel: &AtomicBool, deadline: Instant) -> ProviderRefresh {
    let executable = match executable() {
        Ok(executable) => executable,
        Err(error) => return failed(Provider::Codex, error),
    };
    let response = exchange(&executable, cancel, deadline);
    let missing = || {
        response.failure.clone().unwrap_or_else(|| {
            error(
                UsageErrorKind::InvalidData,
                "Codex usage response was incomplete.",
            )
        })
    };
    let tokens = response
        .tokens
        .as_ref()
        .ok_or_else(missing)
        .and_then(|value| usage::codex_tokens(value, Local::now().date_naive()));
    let limits = response
        .limits
        .as_ref()
        .ok_or_else(missing)
        .and_then(usage::codex_limits);
    ProviderRefresh {
        provider: Provider::Codex,
        tokens: field(tokens),
        limits: field(limits),
        collected_at: Utc::now(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn mock(root: &Fixture, name: &str, body: &str) -> Executable {
        let path = root.0.join(name);
        // Windows PowerShell 5.1 otherwise reads the embedded Korean PID path
        // using the ANSI code page and exits before the timeout scenario starts.
        let mut script = vec![0xef, 0xbb, 0xbf];
        script.extend_from_slice(body.as_bytes());
        std::fs::write(&path, script).unwrap();
        let program = PathBuf::from(std::env::var_os("WINDIR").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        assert!(program.is_file());
        Executable {
            program,
            prefix: vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-File".into(),
                path.into_os_string(),
            ],
        }
    }

    #[test]
    fn owned_codex_jsonl_initialization_partial_errors_and_timeout_kill_descendants() {
        let root = Fixture(
            std::env::temp_dir().join(format!("flowmux-usage-jsonl-한글-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir_all(&root.0).unwrap();
        let handshake = r#"param([string]$Mode)
$ErrorActionPreference='Stop'
$init=[Console]::In.ReadLine()|ConvertFrom-Json
if($Mode -ne 'app-server' -or $init.id -ne 0 -or $init.method -ne 'initialize'){exit 11}
[Console]::Out.WriteLine('{"id":0,"result":{}}')
$ready=[Console]::In.ReadLine()|ConvertFrom-Json
$limits=[Console]::In.ReadLine()|ConvertFrom-Json
$tokens=[Console]::In.ReadLine()|ConvertFrom-Json
if($ready.method -ne 'initialized' -or $limits.id -ne 1 -or $limits.method -ne 'account/rateLimits/read' -or $tokens.id -ne 2 -or $tokens.method -ne 'account/usage/read'){exit 12}
[Console]::Out.WriteLine('{"id":1,"result":{"rateLimits":{"primary":{"usedPercent":12.5,"windowDurationMins":300}}}}')
"#;
        let script = format!("{handshake}\n[Console]::Out.WriteLine('{{\"id\":2,\"error\":{{\"code\":-32601,\"message\":\"unsupported\"}}}}')\nStart-Sleep -Seconds 30\n");
        let executable = mock(&root, "responses.ps1", &script);
        let cancel = AtomicBool::new(false);
        let response = exchange(
            &executable,
            &cancel,
            Instant::now() + Duration::from_secs(5),
        );
        assert!(response.failure.is_none(), "{:?}", response.failure);
        assert_eq!(
            usage::codex_limits(response.limits.as_ref().unwrap()).unwrap()[0].used_percent,
            12.5
        );
        assert_eq!(
            usage::codex_tokens(response.tokens.as_ref().unwrap(), Local::now().date_naive())
                .unwrap_err()
                .kind,
            UsageErrorKind::InvalidData
        );

        let pid_path = root.0.join("owned-child.pid");
        let child = format!(
            r#"
$start=New-Object Diagnostics.ProcessStartInfo
$start.FileName=Join-Path $PSHOME 'powershell.exe'
$start.Arguments='-NoLogo -NoProfile -NonInteractive -Command "Start-Sleep -Seconds 30"'
$start.UseShellExecute=$false
$start.CreateNoWindow=$true
$owned=[Diagnostics.Process]::Start($start)
[IO.File]::WriteAllText('{}',[string]$owned.Id)
Start-Sleep -Seconds 30
"#,
            pid_path.to_string_lossy().replace('\'', "''")
        );
        let executable = mock(&root, "timeout.ps1", &format!("{handshake}\n{child}"));
        let started = Instant::now();
        let response = exchange(&executable, &cancel, started + Duration::from_secs(3));
        assert!(
            pid_path.is_file(),
            "Owned mock did not reach descendant startup: {:?}",
            response.failure
        );
        let failure = response.failure.unwrap();
        assert_eq!(failure.kind, UsageErrorKind::Timeout, "{}", failure.message);
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(response.tokens.is_none());
        assert_eq!(
            usage::codex_limits(response.limits.as_ref().unwrap()).unwrap()[0].used_percent,
            12.5
        );
        let pid = std::fs::read_to_string(pid_path).unwrap().parse().unwrap();
        unsafe {
            let process = OpenProcess(PROCESS_SYNCHRONIZE, 0, pid);
            if !process.is_null() {
                let process = OwnedHandle::from_raw_handle(process);
                assert_eq!(
                    WaitForSingleObject(process.as_raw_handle(), 1000),
                    WAIT_OBJECT_0
                );
            } else {
                assert_eq!(GetLastError(), ERROR_INVALID_PARAMETER);
            }
        }
    }

    #[test]
    fn resolves_native_or_node_without_executing_cmd_wrappers() {
        let root =
            std::env::temp_dir().join(format!("flowmux-usage-resolve-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("node_modules/@openai/codex/bin")).unwrap();
        {
            std::fs::write(root.join("codex.cmd"), "must never execute").unwrap();
            assert!(resolve(std::slice::from_ref(&root)).is_none());
            std::fs::write(root.join("node.exe"), []).unwrap();
            std::fs::write(root.join("node_modules/@openai/codex/bin/codex.js"), []).unwrap();
            let node = resolve(std::slice::from_ref(&root)).unwrap();
            assert_eq!(node.program, root.join("node.exe"));
            assert_eq!(
                node.prefix,
                [root
                    .join("node_modules/@openai/codex/bin/codex.js")
                    .into_os_string()]
            );
            std::fs::write(root.join("codex.exe"), []).unwrap();
            let native = resolve(std::slice::from_ref(&root)).unwrap();
            assert_eq!(native.program, root.join("codex.exe"));
            assert!(native.prefix.is_empty());
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
