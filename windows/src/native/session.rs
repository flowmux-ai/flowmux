// SPDX-License-Identifier: GPL-3.0-or-later
//! A suspended process joins its kill-on-close job before any user code runs.
//! Blocking pipe IO and ClosePseudoConsole never run on the window thread.
use super::{checked, wide};
use crate::protocol::{OutputWindow, OUTPUT_CHUNK_BYTES};
use anyhow::Context;
use flowmux_core::{PaneId, SurfaceId, WorkspaceId};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
    sync::{
        mpsc::{self, SyncSender},
        Arc, Condvar, Mutex,
    },
    thread,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{Console::*, JobObjects::*, Pipes::*, Threading::*},
};

#[derive(Debug)]
pub enum SessionEvent {
    Output { sequence: u64, bytes: Vec<u8> },
    Exit(u32),
    Error(String),
}
enum Input {
    Bytes(Vec<u8>),
    Resize(u16, u16),
}

#[derive(Default)]
struct Credit {
    window: OutputWindow,
    closed: bool,
}
type SharedCredit = Arc<(Mutex<Credit>, Condvar)>;

pub struct Session {
    pub pid: u32,
    job: OwnedHandle,
    input: Option<SyncSender<Input>>,
    credit: SharedCredit,
}

struct Console(HPCON, &'static super::conpty::Api);
// ConPTY handles may be resized/closed on a dedicated IO thread.
unsafe impl Send for Console {}
impl Drop for Console {
    fn drop(&mut self) {
        unsafe { (self.1.close)(self.0) }
    }
}

struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new(console: HPCON) -> anyhow::Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut bytes);
        }
        let mut attrs = Self {
            storage: vec![0; bytes.div_ceil(std::mem::size_of::<usize>())],
            initialized: false,
        };
        unsafe {
            checked(InitializeProcThreadAttributeList(
                attrs.ptr(),
                1,
                0,
                &mut bytes,
            ))?;
            attrs.initialized = true;
            checked(UpdateProcThreadAttribute(
                attrs.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const _,
                std::mem::size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ))?;
        }
        Ok(attrs)
    }
    fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { DeleteProcThreadAttributeList(self.ptr()) }
        }
    }
}

fn pipe() -> anyhow::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
    unsafe {
        checked(CreatePipe(&mut read, &mut write, std::ptr::null(), 0))?;
        Ok((
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        ))
    }
}

impl Session {
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        cwd: &Path,
        pane: PaneId,
        surface: SurfaceId,
        workspace: WorkspaceId,
        pipe_name: &str,
        cols: u16,
        rows: u16,
        emit: impl Fn(SessionEvent) + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let emit = Arc::new(emit);
        let (console_in, input_write) = pipe()?;
        let (output_read, console_out) = pipe()?;
        let mut console = 0;
        let api = super::conpty::api()?;
        let result = unsafe {
            (api.create)(
                COORD {
                    X: cols as i16,
                    Y: rows as i16,
                },
                console_in.as_raw_handle(),
                console_out.as_raw_handle(),
                0,
                &mut console,
            )
        };
        anyhow::ensure!(result >= 0, "CreatePseudoConsole failed: 0x{result:08x}");
        let console = Console(console, api);
        drop(console_in);
        drop(console_out);

        let job = unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            anyhow::ensure!(
                !handle.is_null(),
                "CreateJobObject failed: {}",
                std::io::Error::last_os_error()
            );
            OwnedHandle::from_raw_handle(handle)
        };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            checked(SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            ))?;
        }

        // Windows environment names are case-insensitive. Replace inherited pane/pipe context.
        let mut environment: BTreeMap<String, (std::ffi::OsString, std::ffi::OsString)> =
            std::env::vars_os()
                .map(|(k, v)| (k.to_string_lossy().to_uppercase(), (k, v)))
                .collect();
        for (key, value) in [
            ("TERM", "xterm-256color".to_owned()),
            ("COLORTERM", "truecolor".to_owned()),
            ("TERM_PROGRAM", "flowmux".to_owned()),
            ("FLOWMUX_PANE_ID", pane.to_string()),
            ("FLOWMUX_SURFACE_ID", surface.to_string()),
            ("FLOWMUX_WORKSPACE_ID", workspace.to_string()),
            ("FLOWMUX_TAB_ID", workspace.to_string()),
            ("FLOWMUX_PIPE_NAME", pipe_name.to_owned()),
            (
                "FLOWMUX_BUNDLED_CLI_PATH",
                std::env::current_exe()?
                    .with_file_name("flowmuxctl.exe")
                    .to_string_lossy()
                    .into_owned(),
            ),
        ] {
            environment.insert(key.to_owned(), (key.into(), value.into()));
        }
        environment.remove("FLOWMUX_SOCKET_PATH");
        let mut block = Vec::<u16>::new();
        for (key, value) in environment.values() {
            let mut entry = key.clone();
            entry.push("=");
            entry.push(value);
            block.extend(wide(entry));
        }
        block.push(0);

        let system_root = std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?;
        let executable = std::path::PathBuf::from(system_root)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let exe = wide(&executable);
        // No shell concatenation: executable is an absolute system path and arguments are constant.
        let mut command = wide(format!("\"{}\" -NoLogo -NoExit", executable.display()));
        let directory = wide(cwd);
        let mut attrs = Attributes::new(console.0)?;
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = attrs.ptr();
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        unsafe {
            checked(CreateProcessW(
                exe.as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | CREATE_SUSPENDED,
                block.as_ptr().cast(),
                directory.as_ptr(),
                &startup.StartupInfo,
                &mut process,
            ))
            .context("Cannot start PowerShell in ConPTY")?;
        }
        let process_handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess) };
        let thread_handle = unsafe { OwnedHandle::from_raw_handle(process.hThread) };
        // If assignment fails, kill the still-suspended process before releasing its handles.
        if let Err(error) = unsafe {
            checked(AssignProcessToJobObject(
                job.as_raw_handle(),
                process.hProcess,
            ))
        } {
            unsafe {
                TerminateProcess(process.hProcess, 1);
            }
            return Err(error.context("Cannot assign terminal process to its job"));
        }
        let credit: SharedCredit = Arc::new((Mutex::new(Credit::default()), Condvar::new()));
        let reader_credit = credit.clone();
        let reader_emit = emit.clone();
        thread::Builder::new()
            .name(format!("pty-read-{}", surface.0))
            .spawn(move || {
                let mut reader = File::from(output_read);
                let mut bytes = [0u8; OUTPUT_CHUNK_BYTES];
                loop {
                    match reader.read(&mut bytes) {
                        Ok(0) => break,
                        Ok(count) => {
                            let (lock, wake) = &*reader_credit;
                            let mut state = lock.lock().unwrap();
                            while !state.closed && !state.window.can_send(count) {
                                state = wake.wait(state).unwrap();
                            }
                            if state.closed {
                                continue;
                            } // Drain while ConPTY shuts down.
                            let sequence = state.window.sent(count).unwrap();
                            drop(state);
                            reader_emit(SessionEvent::Output {
                                sequence,
                                bytes: bytes[..count].to_vec(),
                            });
                        }
                        Err(error) if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) => {
                            break
                        }
                        Err(error) => {
                            reader_emit(SessionEvent::Error(error.to_string()));
                            break;
                        }
                    }
                }
            })?;
        let (input, input_receiver) = mpsc::sync_channel(256);
        let writer_emit = emit.clone();
        thread::Builder::new()
            .name(format!("pty-write-{}", surface.0))
            .spawn(move || {
                let mut writer = File::from(input_write);
                for message in input_receiver {
                    let result = match message {
                        Input::Bytes(bytes) => {
                            writer.write_all(&bytes).map_err(anyhow::Error::from)
                        }
                        Input::Resize(cols, rows) => {
                            let result = unsafe {
                                (console.1.resize)(
                                    console.0,
                                    COORD {
                                        X: cols as i16,
                                        Y: rows as i16,
                                    },
                                )
                            };
                            if result < 0 {
                                Err(anyhow::anyhow!("ConPTY resize failed: 0x{result:08x}"))
                            } else {
                                Ok(())
                            }
                        }
                    };
                    if let Err(error) = result {
                        writer_emit(SessionEvent::Error(error.to_string()));
                        break;
                    }
                }
                drop(writer);
                drop(console);
            })?;
        let session = Self {
            pid: process.dwProcessId,
            job,
            input: Some(input),
            credit,
        };
        if unsafe { ResumeThread(thread_handle.as_raw_handle()) } == u32::MAX {
            return Err(std::io::Error::last_os_error().into());
        }
        thread::Builder::new()
            .name(format!("pty-wait-{}", surface.0))
            .spawn(move || {
                unsafe {
                    WaitForSingleObject(process_handle.as_raw_handle(), INFINITE);
                }
                let mut code = 1;
                unsafe {
                    GetExitCodeProcess(process_handle.as_raw_handle(), &mut code);
                }
                emit(SessionEvent::Exit(code));
            })?;
        Ok(session)
    }

    pub fn input(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        // Explicitly opted-in debug builds only. Native IME tests compare this
        // pre-ConPTY boundary with ReadConsoleW, which can transform VT key codes.
        #[cfg(debug_assertions)]
        if let Some(path) = std::env::var_os("FLOWMUX_TEST_INPUT_TRACE") {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                let _ = writeln!(
                    file,
                    "{}",
                    serde_json::json!({"pid":self.pid,"bytes":bytes})
                );
            }
        }
        self.input
            .as_ref()
            .context("terminal has closed")?
            .try_send(Input::Bytes(bytes))
            .context("terminal input queue is full or closed")
    }
    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.input
            .as_ref()
            .context("terminal has closed")?
            .try_send(Input::Resize(cols, rows))
            .context("terminal resize queue is full or closed")
    }
    pub fn acknowledge(&self, sequence: u64) -> anyhow::Result<()> {
        let (lock, wake) = &*self.credit;
        lock.lock().unwrap().window.acknowledge(sequence)?;
        wake.notify_all();
        Ok(())
    }
    pub fn barrier(&self) -> u64 {
        self.credit.0.lock().unwrap().window.barrier()
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let (lock, wake) = &*self.credit;
        lock.lock().unwrap().closed = true;
        wake.notify_all();
        // Terminate descendants even if a pipe worker is temporarily blocked.
        unsafe {
            TerminateJobObject(self.job.as_raw_handle(), 1);
        }
        self.input.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn handles() -> u32 {
        let mut count = 0;
        unsafe {
            checked(GetProcessHandleCount(GetCurrentProcess(), &mut count)).unwrap();
        }
        count
    }

    #[test]
    fn repeated_native_pty_close_releases_process_handles() {
        let create = || {
            Session::spawn(
                &std::env::temp_dir(),
                PaneId::new(),
                SurfaceId::new(),
                WorkspaceId::new(),
                r"\\.\pipe\flowmux-test-unused",
                80,
                24,
                |_| {},
            )
            .unwrap()
        };
        drop(create());
        thread::sleep(std::time::Duration::from_millis(200));
        let before = handles();
        for _ in 0..20 {
            drop(create());
        }
        thread::sleep(std::time::Duration::from_millis(500));
        let after = handles();
        assert!(
            after <= before + 4,
            "native PTY handles grew from {before} to {after}"
        );
    }
}
