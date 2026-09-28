// SPDX-License-Identifier: GPL-3.0-or-later
//! Hidden, cancellable Git subprocesses with one deadline/output budget per operation.
use super::{checked, wide};
use crate::worktrees::{self as domain, List, RemoveError};
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::*,
    Storage::FileSystem::ReadFile,
    System::{JobObjects::*, Pipes::*, Threading::*},
};

const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const BUDGET: Duration = Duration::from_secs(30);

struct Runner<'a> {
    executable: PathBuf,
    cancel: &'a AtomicBool,
    deadline: Instant,
    remaining: usize,
}
struct Output {
    code: u32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
impl Output {
    fn error(&self) -> String {
        let message: String = String::from_utf8_lossy(&self.stderr)
            .trim()
            .chars()
            .take(2000)
            .collect();
        if message.is_empty() {
            format!("Git exited with status {}", self.code)
        } else {
            message
        }
    }
}
pub(super) struct Attributes {
    storage: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    pub(super) fn new(handles: &[HANDLE]) -> Result<Self> {
        let mut bytes = 0;
        unsafe {
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut bytes);
        }
        anyhow::ensure!(bytes != 0, "Cannot size Git process attributes");
        let mut value = Self {
            storage: vec![0; bytes.div_ceil(std::mem::size_of::<usize>())],
            initialized: false,
        };
        unsafe {
            checked(InitializeProcThreadAttributeList(
                value.ptr(),
                1,
                0,
                &mut bytes,
            ))?;
            value.initialized = true;
            checked(UpdateProcThreadAttribute(
                value.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handles.as_ptr().cast(),
                std::mem::size_of_val(handles),
                std::ptr::null_mut(),
                std::ptr::null(),
            ))?;
        }
        Ok(value)
    }
    pub(super) fn ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.storage.as_mut_ptr().cast()
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.ptr());
            }
        }
    }
}
pub(super) fn pipe() -> Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
    unsafe {
        checked(CreatePipe(&mut read, &mut write, std::ptr::null(), 0))?;
        Ok((
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        ))
    }
}
pub(super) fn job() -> Result<OwnedHandle> {
    unsafe {
        let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        anyhow::ensure!(
            !raw.is_null(),
            "Cannot create Git job: {}",
            std::io::Error::last_os_error()
        );
        let job = OwnedHandle::from_raw_handle(raw);
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        checked(SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        ))?;
        Ok(job)
    }
}
fn executable() -> Result<PathBuf> {
    let path = std::env::var_os("PATH").context("Git is not available on PATH")?;
    for directory in std::env::split_paths(&path).filter(|path| path.is_absolute()) {
        let candidate = directory.join("git.exe");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    anyhow::bail!("Git for Windows was not found on the absolute host PATH")
}
pub(super) fn command(arguments: &[OsString]) -> Result<Vec<u16>> {
    let mut out = Vec::new();
    for argument in arguments {
        if !out.is_empty() {
            out.push(32);
        }
        out.push(34);
        let mut slashes = 0;
        for unit in argument.encode_wide() {
            anyhow::ensure!(unit != 0, "Git argument contains NUL");
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
    anyhow::ensure!(out.len() < 32767, "Git command exceeds the Windows limit");
    out.push(0);
    Ok(out)
}
fn environment() -> Vec<u16> {
    let mut values = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        // The selected cwd, not a calling shell's Git environment, owns this operation.
        if matches!(
            upper.as_str(),
            "GIT_DIR"
                | "GIT_WORK_TREE"
                | "GIT_COMMON_DIR"
                | "GIT_INDEX_FILE"
                | "GIT_OBJECT_DIRECTORY"
                | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                | "GIT_CONFIG"
                | "GIT_CONFIG_COUNT"
                | "GIT_CONFIG_PARAMETERS"
                | "GIT_CEILING_DIRECTORIES"
        ) || upper.starts_with("GIT_CONFIG_KEY_")
            || upper.starts_with("GIT_CONFIG_VALUE_")
        {
            continue;
        }
        values.insert(upper, (key, value));
    }
    for (key, value) in [
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_OPTIONAL_LOCKS", "0"),
        ("GIT_PAGER", "cat"),
        ("PAGER", "cat"),
        ("LC_ALL", "C"),
    ] {
        values.insert(key.into(), (OsString::from(key), OsString::from(value)));
    }
    let mut block = Vec::new();
    for (key, value) in values.values() {
        let mut entry = key.clone();
        entry.push("=");
        entry.push(value);
        block.extend(wide(entry));
    }
    block.push(0);
    block
}

impl<'a> Runner<'a> {
    fn new(cancel: &'a AtomicBool) -> Result<Self> {
        let deadline = Instant::now() + BUDGET;
        let value = Self {
            executable: executable()?,
            cancel,
            deadline,
            remaining: OUTPUT_LIMIT,
        };
        value.check()?;
        Ok(value)
    }
    fn check(&self) -> Result<()> {
        anyhow::ensure!(
            !self.cancel.load(Ordering::Relaxed),
            "Worktree operation cancelled"
        );
        anyhow::ensure!(
            Instant::now() < self.deadline,
            "Worktree operation exceeded 30 seconds"
        );
        anyhow::ensure!(self.remaining != 0, "Worktree Git output reached 2 MiB");
        Ok(())
    }
    // Peek guarantees the synchronous ReadFile only consumes bytes already in this pipe.
    fn drain(&mut self, pipe: &OwnedHandle, bytes: &mut Vec<u8>) -> Result<bool> {
        self.check()?;
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
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
                    return Ok(false);
                }
                return Err(error.into());
            }
            if available == 0 {
                return Ok(false);
            }
            let mut buffer = [0u8; 65536];
            let mut read = 0;
            checked(ReadFile(
                pipe.as_raw_handle(),
                buffer.as_mut_ptr(),
                available.min(buffer.len() as u32),
                &mut read,
                std::ptr::null_mut(),
            ))?;
            if read as usize > self.remaining {
                self.remaining = 0;
                anyhow::bail!("Worktree Git output exceeded 2 MiB");
            }
            self.remaining -= read as usize;
            bytes.extend_from_slice(&buffer[..read as usize]);
            Ok(read != 0)
        }
    }
    fn run(&mut self, directory: &Path, args: &[&OsStr]) -> Result<Output> {
        let mut arguments = [
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "credential.interactive=false",
        ]
        .map(OsStr::new)
        .to_vec();
        arguments.extend_from_slice(args);
        self.run_process(directory, &arguments)
    }
    fn run_process(&mut self, directory: &Path, args: &[&OsStr]) -> Result<Output> {
        self.check()?;
        anyhow::ensure!(directory.is_absolute(), "Git directory must be absolute");
        let _errors = super::session::ErrorMode::suppress_dialogs()?;
        let job = job()?;
        let (input, input_writer) = pipe()?;
        let (output, output_writer) = pipe()?;
        let (error, error_writer) = pipe()?;
        drop(input_writer); // Noninteractive commands see EOF on stdin.
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
                ))?;
            }
        }
        let mut attributes = Attributes::new(&handles)?;
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = std::mem::size_of_val(&startup) as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[2];
        startup.lpAttributeList = attributes.ptr();
        let mut arguments = vec![self.executable.as_os_str().to_owned()];
        arguments.extend(args.iter().map(|arg| arg.to_os_string()));
        let mut command = command(&arguments)?;
        let block = environment();
        let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        unsafe {
            checked(CreateProcessW(
                wide(&self.executable).as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
                CREATE_NO_WINDOW
                    | CREATE_SUSPENDED
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT,
                block.as_ptr().cast(),
                wide(directory).as_ptr(),
                &startup.StartupInfo,
                &mut process,
            ))?;
        }
        let process_handle = unsafe { OwnedHandle::from_raw_handle(process.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(process.hThread) };
        unsafe {
            if let Err(error) = checked(AssignProcessToJobObject(
                job.as_raw_handle(),
                process_handle.as_raw_handle(),
            )) {
                TerminateProcess(process_handle.as_raw_handle(), 1);
                return Err(error.context("Cannot assign Git process to its owned job"));
            }
            anyhow::ensure!(
                ResumeThread(thread.as_raw_handle()) != u32::MAX,
                "Cannot resume Git process"
            );
        }
        drop(thread);
        drop(input);
        drop(output_writer);
        drop(error_writer);
        let mut result = Output {
            code: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };
        loop {
            self.check()?;
            self.drain(&output, &mut result.stdout)?;
            self.drain(&error, &mut result.stderr)?;
            match unsafe { WaitForSingleObject(process_handle.as_raw_handle(), 5) } {
                WAIT_OBJECT_0 => {
                    unsafe {
                        checked(GetExitCodeProcess(
                            process_handle.as_raw_handle(),
                            &mut result.code,
                        ))?;
                        // No Git helper may retain our pipes after the main command exits.
                        checked(TerminateJobObject(job.as_raw_handle(), 1))?;
                    }
                    while self.drain(&output, &mut result.stdout)? {}
                    while self.drain(&error, &mut result.stderr)? {}
                    return Ok(result);
                }
                WAIT_TIMEOUT => {}
                _ => return Err(std::io::Error::last_os_error().into()),
            }
        }
        // Every return drops the kill-on-close job, including cancellation/overflow/startup failure.
    }
    fn strings(&mut self, directory: &Path, args: &[&str]) -> Result<Output> {
        self.run(directory, &args.iter().map(OsStr::new).collect::<Vec<_>>())
    }
    fn list(&mut self, start: &Path) -> Result<List> {
        let root = self.strings(start, &["rev-parse", "--show-toplevel"])?;
        anyhow::ensure!(root.code == 0, "{}", root.error());
        let root_bytes = root.stdout.strip_suffix(b"\n").unwrap_or(&root.stdout);
        let root_bytes = root_bytes.strip_suffix(b"\r").unwrap_or(root_bytes);
        let root = PathBuf::from(
            std::str::from_utf8(root_bytes).context("Git repository path is not UTF-8")?,
        );
        anyhow::ensure!(
            root.is_absolute(),
            "Git returned a relative repository path"
        );
        let root = normalize(&root);
        self.check()?;
        let output = self.strings(&root, &["worktree", "list", "--porcelain", "-z"])?;
        anyhow::ensure!(output.code == 0, "{}", output.error());
        let mut items = domain::parse_porcelain(&output.stdout).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(!items.is_empty(), "Git returned no worktrees");
        for item in &mut items {
            anyhow::ensure!(
                item.path.is_absolute(),
                "Git returned a relative worktree path"
            );
            item.path = normalize(&item.path);
            item.is_current = same_path(&item.path, &root);
            if !item.is_bare {
                match self.strings(
                    &item.path,
                    &["status", "--porcelain=v2", "-z", "--untracked-files=normal"],
                ) {
                    Ok(output) if output.code == 0 => {
                        item.changes =
                            Some(domain::parse_status(&output.stdout).map_err(anyhow::Error::msg)?)
                    }
                    _ => {
                        self.check()?;
                    }
                }
                if !item.head.is_empty() {
                    match self.strings(
                        &root,
                        &[
                            "show",
                            "-s",
                            "--no-show-signature",
                            "--format=%s%x00%ct",
                            &item.head,
                            "--",
                        ],
                    ) {
                        Ok(output) if output.code == 0 => {
                            (item.commit_subject, item.commit_time) =
                                domain::parse_commit(&output.stdout)
                        }
                        _ => {
                            self.check()?;
                        }
                    }
                }
            }
        }
        self.check()?;
        items.sort_by(|a, b| {
            b.is_current
                .cmp(&a.is_current)
                .then_with(|| a.branch.cmp(&b.branch))
                .then_with(|| a.path.cmp(&b.path))
        });
        Ok(List {
            repository_root: root.clone(),
            current_worktree: root,
            items,
        })
    }
}
fn normalize(path: &Path) -> PathBuf {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    ordinary(path)
}
fn ordinary(path: PathBuf) -> PathBuf {
    // Git for Windows accepts ordinary drive/UNC paths; do not pass a verbatim
    // device prefix as a worktree operand. Preserve the remaining UTF-16 units.
    use std::os::windows::ffi::OsStringExt;
    let units: Vec<_> = path.as_os_str().encode_wide().collect();
    if units.starts_with(&[92, 92, 63, 92, 85, 78, 67, 92]) {
        let mut ordinary = vec![92, 92];
        ordinary.extend_from_slice(&units[8..]);
        PathBuf::from(OsString::from_wide(&ordinary))
    } else if units.starts_with(&[92, 92, 63, 92]) && units.get(5) == Some(&58) {
        PathBuf::from(OsString::from_wide(&units[4..]))
    } else {
        path
    }
}
/// Worker-only: canonicalize every path before containment checks. An unresolved
/// cwd cannot be treated as proof that a checkout is unused.
pub(super) fn ensure_unused(target: &Path, used: &[PathBuf]) -> Result<(), String> {
    let canonical = |path: &Path| {
        std::fs::canonicalize(path).map(ordinary).map_err(|error| {
            format!(
                "Cannot verify worktree usage for {}: {error}",
                path.display()
            )
        })
    };
    let target = canonical(target)?;
    for path in used {
        let path = canonical(path)?;
        if path
            .ancestors()
            .any(|ancestor| same_path(ancestor, &target))
        {
            return Err("Close tabs and workspaces using this worktree before removal".into());
        }
    }
    Ok(())
}
fn same_path(left: &Path, right: &Path) -> bool {
    use windows_sys::Win32::Globalization::{CompareStringOrdinal, CSTR_EQUAL};
    let left: Vec<_> = left.as_os_str().encode_wide().collect();
    let right: Vec<_> = right.as_os_str().encode_wide().collect();
    unsafe {
        CompareStringOrdinal(
            left.as_ptr(),
            left.len() as i32,
            right.as_ptr(),
            right.len() as i32,
            1,
        ) == CSTR_EQUAL
    }
}
pub(super) fn list(start: &Path, cancel: &AtomicBool) -> Result<List, String> {
    Runner::new(cancel)
        .and_then(|mut runner| runner.list(start))
        .map_err(|e| format!("{e:#}"))
}
pub(super) fn remove(
    root: &Path,
    path: &Path,
    force: bool,
    cancel: &AtomicBool,
) -> Result<(), RemoveError> {
    let failure = |error: anyhow::Error| RemoveError::Failed(format!("{error:#}"));
    let mut runner = Runner::new(cancel).map_err(failure)?;
    let current = runner.list(root).map_err(failure)?;
    let path = normalize(path);
    let row = current
        .items
        .iter()
        .find(|row| same_path(&row.path, &path))
        .ok_or_else(|| RemoveError::Failed("Worktree is no longer in this repository".into()))?;
    if row.is_main || row.is_current || row.is_bare {
        return Err(RemoveError::Failed(
            "Main, current and bare worktrees cannot be removed".into(),
        ));
    }
    if let Some(reason) = &row.lock_reason {
        return Err(RemoveError::Locked(reason.clone()));
    }
    let changes = row
        .changes
        .as_ref()
        .ok_or_else(|| RemoveError::Failed("Cannot verify worktree changes".into()))?;
    if !changes.is_clean() && !force {
        return Err(RemoveError::RequiresForce(
            "Worktree contains tracked or untracked changes".into(),
        ));
    }
    let mut args = vec![OsStr::new("worktree"), OsStr::new("remove")];
    if force && !changes.is_clean() {
        args.push(OsStr::new("--force"));
    }
    args.push(OsStr::new("--"));
    args.push(path.as_os_str());
    let output = runner
        .run(&current.repository_root, &args)
        .map_err(failure)?;
    if output.code == 0 {
        Ok(())
    } else {
        Err(RemoveError::Failed(output.error()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_git_runner_enforces_cancellation_deadline_and_output_budget() {
        let directory = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
        let executable = directory.join("WindowsPowerShell/v1.0/powershell.exe");
        let cancel = AtomicBool::new(false);
        let mut runner = Runner {
            executable,
            cancel: &cancel,
            deadline: Instant::now() + Duration::from_secs(2),
            remaining: OUTPUT_LIMIT,
        };
        let args = [
            OsStr::new("-NoProfile"),
            OsStr::new("-NonInteractive"),
            OsStr::new("-Command"),
            OsStr::new("[Console]::Out.WriteLine('bounded')"),
        ];
        let echoed = runner.run_process(&directory, &args).unwrap();
        assert_eq!(echoed.code, 0, "{}", echoed.error());
        assert_eq!(echoed.stdout, b"bounded\r\n");
        cancel.store(true, Ordering::Relaxed);
        let error = runner
            .run_process(&directory, &args)
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("cancelled"));
        cancel.store(false, Ordering::Relaxed);
        runner.deadline = Instant::now() - Duration::from_millis(1);
        assert!(runner
            .run_process(&directory, &args)
            .err()
            .unwrap()
            .to_string()
            .contains("30 seconds"));
        runner.deadline = Instant::now() + Duration::from_secs(1);
        runner.remaining = 1;
        assert!(runner
            .run_process(&directory, &args)
            .err()
            .unwrap()
            .to_string()
            .contains("2 MiB"));
        runner.deadline = Instant::now() + Duration::from_millis(100);
        runner.remaining = OUTPUT_LIMIT;
        let args = [
            OsStr::new("-NoProfile"),
            OsStr::new("-NonInteractive"),
            OsStr::new("-Command"),
            OsStr::new("while ($true) { }"),
        ];
        assert!(runner
            .run_process(&directory, &args)
            .err()
            .unwrap()
            .to_string()
            .contains("30 seconds"));
        runner.deadline = Instant::now() + Duration::from_secs(1);
        let error = std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(25));
                cancel.store(true, Ordering::Relaxed);
            });
            runner
                .run_process(&directory, &args)
                .err()
                .unwrap()
                .to_string()
        });
        assert!(error.contains("cancelled"));
    }
    #[test]
    fn usage_paths_are_resolved_before_component_comparison() {
        let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        assert!(ensure_unused(&root, &[root.join("System32")]).is_err());
        assert!(ensure_unused(&root.join("System32"), std::slice::from_ref(&root)).is_ok());
        let missing = root.join(format!("flowmux-missing-{}", uuid::Uuid::new_v4()));
        assert!(ensure_unused(&root, &[missing])
            .unwrap_err()
            .contains("Cannot verify"));
        assert_eq!(
            ordinary(PathBuf::from(r"\\?\UNC\server\share\tree")),
            PathBuf::from(r"\\server\share\tree")
        );
    }
}
