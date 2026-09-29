// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, read-only inspection of processes in one owned terminal Job.
use super::{checked, wide};
use crate::session_history::SessionAgent;
use anyhow::{Context, Result};
use std::{
    collections::BTreeMap,
    ffi::c_void,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};
use windows_sys::Win32::{
    Foundation::*,
    System::{
        Diagnostics::Debug::ReadProcessMemory, JobObjects::*, LibraryLoader::*, Threading::*,
    },
    UI::Shell::CommandLineToArgvW,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AgentProcess {
    pub agent: SessionAgent,
    pub pid: u32,
    pub started: u64,
    pub executable: PathBuf,
    pub argv: Vec<String>,
    pub prefix: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub home: PathBuf,
    pub arguments: Vec<String>,
    pub session_id: Option<String>,
}

const MAX_PIDS: usize = 256;
const MAX_ENVIRONMENT: usize = 256 * 1024;
const MAX_MEMORY: usize = 8 * 1024 * 1024;

struct Budget<'a> {
    cancel: &'a AtomicBool,
    deadline: Instant,
    remaining: usize,
}
impl Budget<'_> {
    fn check(&self) -> Result<()> {
        anyhow::ensure!(
            !self.cancel.load(Ordering::Acquire),
            "Agent discovery cancelled"
        );
        anyhow::ensure!(
            Instant::now() < self.deadline,
            "Agent discovery deadline exceeded"
        );
        Ok(())
    }
    fn read(&mut self, process: HANDLE, address: usize, length: usize) -> Result<Vec<u8>> {
        self.check()?;
        anyhow::ensure!(
            address >= 0x10000 && address.checked_add(length).is_some(),
            "Invalid process memory range"
        );
        anyhow::ensure!(
            length <= self.remaining,
            "Agent discovery memory limit exceeded"
        );
        self.remaining -= length;
        let mut data = vec![0; length];
        let mut read = 0;
        unsafe {
            checked(ReadProcessMemory(
                process,
                address as *const c_void,
                data.as_mut_ptr().cast(),
                length,
                &mut read,
            ))?;
        }
        anyhow::ensure!(read == length, "Incomplete process memory read");
        self.check()?;
        Ok(data)
    }
}

#[repr(C)]
struct Pids {
    assigned: u32,
    count: u32,
    ids: [usize; MAX_PIDS],
}
fn job_pids(job: HANDLE) -> Result<Vec<u32>> {
    let mut list = Pids {
        assigned: 0,
        count: 0,
        ids: [0; MAX_PIDS],
    };
    unsafe {
        checked(QueryInformationJobObject(
            job,
            JobObjectBasicProcessIdList,
            (&mut list as *mut Pids).cast(),
            std::mem::size_of_val(&list) as u32,
            std::ptr::null_mut(),
        ))?;
    }
    anyhow::ensure!(
        list.count as usize <= MAX_PIDS && list.assigned == list.count,
        "Owned Job exceeds process discovery limit"
    );
    list.ids[..list.count as usize]
        .iter()
        .map(|pid| u32::try_from(*pid).context("Invalid owned process ID"))
        .collect()
}
fn started(process: HANDLE, job: HANDLE) -> Result<u64> {
    let mut member = 0;
    let mut creation: FILETIME = unsafe { std::mem::zeroed() };
    let mut exit = creation;
    let mut kernel = creation;
    let mut user = creation;
    unsafe {
        checked(IsProcessInJob(process, job, &mut member))?;
        anyhow::ensure!(
            member != 0 && WaitForSingleObject(process, 0) == WAIT_TIMEOUT,
            "Owned process is no longer live"
        );
        checked(GetProcessTimes(
            process,
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        ))?;
    }
    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}
fn executable(process: HANDLE) -> Result<PathBuf> {
    let mut data = vec![0_u16; 32768];
    let mut length = data.len() as u32;
    unsafe {
        checked(QueryFullProcessImageNameW(
            process,
            0,
            data.as_mut_ptr(),
            &mut length,
        ))?;
    }
    Ok(PathBuf::from(
        String::from_utf16(&data[..length as usize]).context("Invalid executable Unicode")?,
    ))
}
fn native_agent(path: &Path) -> Option<SessionAgent> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    SessionAgent::from_name(name.strip_suffix(".exe")?)
}
fn node(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("node.exe"))
}

/// Do not inspect unrelated processes or infer agent identity from terminal output.
pub(super) fn discover(
    job: &OwnedHandle,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Option<AgentProcess>> {
    let _mode = super::session::ErrorMode::suppress_dialogs()?;
    let mut budget = Budget {
        cancel,
        deadline,
        remaining: MAX_MEMORY,
    };
    budget.check()?;
    let mut found: Vec<AgentProcess> = Vec::new();
    let mut unavailable = Vec::new();
    for pid in job_pids(job.as_raw_handle())? {
        budget.check()?;
        let raw = unsafe {
            OpenProcess(
                PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
                0,
                pid,
            )
        };
        if raw.is_null() {
            continue;
        }
        let process = unsafe { OwnedHandle::from_raw_handle(raw) };
        let Ok(time) = started(raw, job.as_raw_handle()) else {
            continue;
        };
        let Ok(image) = executable(raw) else {
            continue;
        };
        if native_agent(&image).is_none() && !node(&image) {
            continue;
        }
        let result = inspect(&process, &mut budget).and_then(|snapshot| {
            let Some((agent, prefix, first_argument)) =
                identity(&image, &snapshot.argv, &snapshot.cwd)
            else {
                return Ok(None);
            };
            let (environment, home, arguments) = home(
                agent,
                &snapshot.environment,
                &snapshot.cwd,
                &snapshot.argv[first_argument..],
            )?;
            anyhow::ensure!(
                started(raw, job.as_raw_handle())? == time,
                "Owned process identity changed"
            );
            anyhow::ensure!(executable(raw)? == image, "Owned process image changed");
            Ok(Some(AgentProcess {
                agent,
                pid,
                started: time,
                executable: image.clone(),
                session_id: session_id(agent, &snapshot.argv[first_argument..]),
                argv: snapshot.argv,
                prefix,
                cwd: snapshot.cwd,
                environment,
                home,
                arguments,
            }))
        });
        match result {
            Ok(Some(value)) => found.push(value),
            Ok(None) => {}
            Err(_) => unavailable.push(process),
        }
        budget.check()?;
    }
    // Like the Unix process source, prefer the earliest actual agent process.
    found.sort_by_key(|value| (value.started, value.pid));
    for value in found {
        budget.check()?;
        if live_handle(job, &value).is_some() {
            return Ok(Some(value));
        }
    }
    anyhow::ensure!(
        // A short-lived agent may exit while its parameters are being read.
        // Only a still-live unreadable process represents unsupported details.
        !unavailable
            .iter()
            .any(|process| started(process.as_raw_handle(), job.as_raw_handle()).is_ok()),
        "Agent process details are unavailable or unsupported"
    );
    Ok(None)
}

/// Retain this exact process incarnation so a deferred reply cannot report a
/// recycled PID or an agent that exited while another terminal was inspected.
pub(super) fn live_handle(job: &OwnedHandle, value: &AgentProcess) -> Option<OwnedHandle> {
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            value.pid,
        )
    };
    if raw.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    (started(raw, job.as_raw_handle()).ok() == Some(value.started)).then_some(process)
}

struct Snapshot {
    argv: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
}
#[repr(C)]
struct BasicInformation {
    exit: i32,
    peb: *mut c_void,
    affinity: usize,
    priority: i32,
    pid: usize,
    parent: usize,
}
type QueryProcess = unsafe extern "system" fn(HANDLE, u32, *mut c_void, u32, *mut u32) -> i32;
type QueryMachine = unsafe extern "system" fn(HANDLE, *mut u16, *mut u16) -> i32;
fn query_process() -> Result<QueryProcess> {
    unsafe {
        let ntdll = GetModuleHandleW(wide("ntdll.dll").as_ptr());
        let query = GetProcAddress(ntdll, c"NtQueryInformationProcess".as_ptr().cast())
            .context("Process query API unavailable")?;
        Ok(std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            QueryProcess,
        >(query))
    }
}

/// Hooks must come from a live descendant of this agent in its terminal Job.
pub(super) fn verify_hook_caller(
    job: &OwnedHandle,
    caller: &OwnedHandle,
    agent: &OwnedHandle,
) -> Result<()> {
    let query = query_process()?;
    let deadline = Instant::now() + std::time::Duration::from_millis(100);
    let target = unsafe { GetProcessId(agent.as_raw_handle()) };
    let target_started = started(agent.as_raw_handle(), job.as_raw_handle())?;
    let mut child = caller.as_raw_handle();
    let mut child_started = started(child, job.as_raw_handle())?;
    let mut parents = Vec::new();
    // ponytail: synchronous hook launch chains only, capped at 16 live parents;
    // detached helpers need an explicit authenticated registration protocol.
    for _ in 0..16 {
        anyhow::ensure!(
            Instant::now() < deadline,
            "Hook ancestry inspection expired"
        );
        let mut info: BasicInformation = unsafe { std::mem::zeroed() };
        let status = unsafe {
            query(
                child,
                0,
                (&mut info as *mut BasicInformation).cast(),
                std::mem::size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(status >= 0, "Cannot query hook parent");
        let pid = u32::try_from(info.parent).context("Invalid hook parent PID")?;
        if pid == target {
            anyhow::ensure!(
                target_started <= child_started,
                "Hook parent process was replaced"
            );
            for process in std::iter::once(caller)
                .chain(parents.iter())
                .chain(std::iter::once(agent))
            {
                started(process.as_raw_handle(), job.as_raw_handle())?;
            }
            return Ok(());
        }
        let raw = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid) };
        anyhow::ensure!(!raw.is_null(), "Hook parent is no longer available");
        let parent = unsafe { OwnedHandle::from_raw_handle(raw) };
        let parent_started = started(raw, job.as_raw_handle())?;
        anyhow::ensure!(
            parent_started <= child_started,
            "Hook parent process was replaced"
        );
        let image = executable(raw)?;
        anyhow::ensure!(
            native_agent(&image).is_none() && !node(&image),
            "Hook belongs to a nested agent or Node process"
        );
        child = raw;
        child_started = parent_started;
        parents.push(parent);
    }
    anyhow::bail!("Hook caller is not a supported descendant of this agent")
}

fn parameters(process: HANDLE, budget: &mut Budget<'_>) -> Result<(usize, bool)> {
    // These private layouts are supported only for x64 and WOW64 x86. Resolve
    // ntdll dynamically; a missing API or inconsistent prefix is an error.
    anyhow::ensure!(
        cfg!(target_arch = "x86_64"),
        "Agent memory discovery requires the x64 host"
    );
    let (query, machine): (QueryProcess, QueryMachine) = unsafe {
        let kernel = GetModuleHandleW(wide("kernel32.dll").as_ptr());
        let machine = GetProcAddress(kernel, c"IsWow64Process2".as_ptr().cast())
            .context("Process architecture API unavailable")?;
        (
            query_process()?,
            std::mem::transmute::<unsafe extern "system" fn() -> isize, QueryMachine>(machine),
        )
    };
    let (mut process_machine, mut native_machine) = (0, 0);
    unsafe {
        checked(machine(process, &mut process_machine, &mut native_machine))?;
    }
    let narrow = process_machine == 0x014c;
    anyhow::ensure!(
        narrow || process_machine == 0x8664 || (process_machine == 0 && native_machine == 0x8664),
        "Agent process architecture is unsupported"
    );
    let peb = if narrow {
        let mut peb = 0_usize;
        let status = unsafe {
            query(
                process,
                26,
                (&mut peb as *mut usize).cast(),
                std::mem::size_of::<usize>() as u32,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(status >= 0, "Cannot query WOW64 process parameters");
        peb
    } else {
        let mut info: BasicInformation = unsafe { std::mem::zeroed() };
        let status = unsafe {
            query(
                process,
                0,
                (&mut info as *mut BasicInformation).cast(),
                std::mem::size_of_val(&info) as u32,
                std::ptr::null_mut(),
            )
        };
        anyhow::ensure!(status >= 0, "Cannot query process parameters");
        info.peb as usize
    };
    let address = peb
        .checked_add(if narrow { 0x10 } else { 0x20 })
        .context("Invalid process parameters")?;
    let pointer = budget.read(process, address, if narrow { 4 } else { 8 })?;
    Ok((pointer_at(&pointer, 0, narrow)?, narrow))
}
fn pointer_at(data: &[u8], offset: usize, narrow: bool) -> Result<usize> {
    let length = if narrow { 4 } else { 8 };
    let bytes = data
        .get(offset..offset + length)
        .context("Invalid process parameter layout")?;
    let value = if narrow {
        u64::from(u32::from_le_bytes(bytes.try_into()?))
    } else {
        u64::from_le_bytes(bytes.try_into()?)
    };
    usize::try_from(value).context("Unsupported process pointer")
}
fn string_at(
    process: HANDLE,
    header: &[u8],
    offset: usize,
    narrow: bool,
    budget: &mut Budget<'_>,
) -> Result<String> {
    let length = u16::from_le_bytes(header[offset..offset + 2].try_into()?) as usize;
    let maximum = u16::from_le_bytes(header[offset + 2..offset + 4].try_into()?) as usize;
    anyhow::ensure!(
        length.is_multiple_of(2) && length <= maximum,
        "Invalid process string length"
    );
    if length == 0 {
        return Ok(String::new());
    }
    let address = pointer_at(header, offset + if narrow { 4 } else { 8 }, narrow)?;
    let bytes = budget.read(process, address, length)?;
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    anyhow::ensure!(!units.contains(&0), "Process string contains NUL");
    String::from_utf16(&units).context("Invalid process string Unicode")
}
fn inspect(process: &OwnedHandle, budget: &mut Budget<'_>) -> Result<Snapshot> {
    let process = process.as_raw_handle();
    let (address, narrow) = parameters(process, budget)?;
    // RTL_USER_PROCESS_PARAMETERS: CurrentDirectory, CommandLine, Environment.
    // x64 offsets: 0x38,0x70,0x80; WOW64: 0x24,0x40,0x48.
    let size = if narrow { 76 } else { 136 };
    let header = budget.read(process, address, size)?;
    let maximum = u32::from_le_bytes(header[0..4].try_into()?);
    let length = u32::from_le_bytes(header[4..8].try_into()?);
    let flags = u32::from_le_bytes(header[8..12].try_into()?);
    anyhow::ensure!(
        length >= size as u32 && maximum >= length && flags & 1 != 0,
        "Unsupported process parameter layout"
    );
    let cwd_offset = if narrow { 0x24 } else { 0x38 };
    let command_offset = if narrow { 0x40 } else { 0x70 };
    let cwd = string_at(process, &header, cwd_offset, narrow, budget)?;
    let command = string_at(process, &header, command_offset, narrow, budget)?;
    let environment = read_environment(
        process,
        pointer_at(&header, if narrow { 0x48 } else { 0x80 }, narrow)?,
        budget,
    )?;
    anyhow::ensure!(
        budget.read(process, address, size)? == header
            && string_at(process, &header, cwd_offset, narrow, budget)? == cwd
            && string_at(process, &header, command_offset, narrow, budget)? == command,
        "Process parameters changed during discovery"
    );
    let cwd = PathBuf::from(cwd);
    anyhow::ensure!(cwd.is_absolute(), "Agent working directory is not absolute");
    Ok(Snapshot {
        argv: command_line(&command)?,
        cwd,
        environment,
    })
}
fn command_line(command: &str) -> Result<Vec<String>> {
    anyhow::ensure!(!command.is_empty(), "Empty process command line");
    let mut count = 0;
    let pointer = unsafe { CommandLineToArgvW(wide(command).as_ptr(), &mut count) };
    anyhow::ensure!(!pointer.is_null(), "Cannot parse process command line");
    struct Args(*mut *mut u16);
    impl Drop for Args {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.0.cast());
            }
        }
    }
    let args = Args(pointer);
    anyhow::ensure!(
        (1..=1024).contains(&count),
        "Agent argument count exceeds limit"
    );
    let mut output = Vec::with_capacity(count as usize);
    for index in 0..count as usize {
        let value = unsafe { *args.0.add(index) };
        let mut length = 0;
        while length <= command.len() && unsafe { *value.add(length) } != 0 {
            length += 1;
        }
        anyhow::ensure!(length <= command.len(), "Invalid parsed process argument");
        output.push(
            String::from_utf16(unsafe { std::slice::from_raw_parts(value, length) })
                .context("Invalid argument Unicode")?,
        );
    }
    Ok(output)
}
fn retained_key(key: &str) -> bool {
    key == "PATH"
        || [
            SessionAgent::Claude,
            SessionAgent::Codex,
            SessionAgent::OpenCode,
            SessionAgent::Antigravity,
            SessionAgent::Cline,
        ]
        .iter()
        .any(|agent| agent.home_variables().contains(&key))
}
fn read_environment(
    process: HANDLE,
    address: usize,
    budget: &mut Budget<'_>,
) -> Result<BTreeMap<String, String>> {
    anyhow::ensure!(address.is_multiple_of(2), "Invalid environment alignment");
    let mut output = BTreeMap::new();
    let mut line = Vec::new();
    let mut offset = 0;
    while offset < MAX_ENVIRONMENT {
        let start = address
            .checked_add(offset)
            .context("Invalid environment address")?;
        // Never cross a page boundary in a single remote read.
        let length = (4096 - start % 4096).min(MAX_ENVIRONMENT - offset);
        let bytes = budget.read(process, start, length)?;
        offset += length;
        for pair in bytes.chunks_exact(2) {
            let unit = u16::from_le_bytes([pair[0], pair[1]]);
            if unit != 0 {
                line.push(unit);
                continue;
            }
            if line.is_empty() {
                return Ok(output);
            }
            if let Some(equal) = line
                .iter()
                .position(|unit| *unit == 61)
                .filter(|equal| *equal > 0)
            {
                if let Ok(key) = String::from_utf16(&line[..equal]) {
                    let key = key.to_ascii_uppercase();
                    if retained_key(&key) {
                        let value = String::from_utf16(&line[equal + 1..])
                            .context("Invalid allowed environment Unicode")?;
                        if !value.is_empty() {
                            output.insert(key, value);
                        }
                    }
                }
            }
            line.clear();
        }
    }
    anyhow::bail!("Agent environment exceeds discovery limit")
}
fn absolute(value: &str, cwd: &Path) -> Option<PathBuf> {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        return Some(path);
    }
    // Drive-relative and rooted-without-drive paths require hidden drive state.
    if path.has_root()
        || matches!(
            path.components().next(),
            Some(std::path::Component::Prefix(_))
        )
    {
        return None;
    }
    Some(cwd.join(path))
}
fn identity(
    image: &Path,
    argv: &[String],
    cwd: &Path,
) -> Option<(SessionAgent, Vec<String>, usize)> {
    if let Some(agent) = native_agent(image) {
        return Some((agent, Vec::new(), 1));
    }
    if !node(image) {
        return None;
    }
    let mut index = 1;
    let mut prefix = Vec::new();
    // Never scan task arguments or -e code for an agent-looking filename.
    while let Some(option) = argv.get(index).filter(|value| value.starts_with('-')) {
        if !matches!(
            option.as_str(),
            "--no-warnings" | "--enable-source-maps" | "--no-deprecation"
        ) {
            return None;
        }
        prefix.push(option.clone());
        index += 1;
    }
    let path = absolute(argv.get(index)?, cwd)?;
    let lower = path.to_str()?.replace('\\', "/").to_ascii_lowercase();
    let agent = [
        (
            "/node_modules/@anthropic-ai/claude-code/cli.js",
            SessionAgent::Claude,
        ),
        (
            "/node_modules/@openai/codex/bin/codex.js",
            SessionAgent::Codex,
        ),
        (
            "/node_modules/opencode-ai/bin/opencode",
            SessionAgent::OpenCode,
        ),
        ("/node_modules/cline/dist/cli.js", SessionAgent::Cline),
        ("/node_modules/cline/dist/index.js", SessionAgent::Cline),
        (
            "/node_modules/@cline/cli/dist/index.js",
            SessionAgent::Cline,
        ),
        (
            "/node_modules/antigravity-cli/bin/agy.js",
            SessionAgent::Antigravity,
        ),
    ]
    .iter()
    .find_map(|(suffix, agent)| lower.ends_with(suffix).then_some(*agent))?;
    prefix.push(path.to_str()?.to_string());
    Some((agent, prefix, index + 1))
}
fn home(
    agent: SessionAgent,
    raw: &BTreeMap<String, String>,
    cwd: &Path,
    argv: &[String],
) -> Result<(BTreeMap<String, String>, PathBuf, Vec<String>)> {
    let mut environment = BTreeMap::new();
    for (key, value) in raw {
        if key == "PATH" {
            environment.insert(key.clone(), value.clone());
        } else if agent.home_variables().contains(&key.as_str()) {
            let value = absolute(value, cwd)
                .context("Agent home override uses unsupported drive-relative path")?;
            environment.insert(
                key.clone(),
                value
                    .to_str()
                    .context("Invalid home path Unicode")?
                    .to_string(),
            );
        }
    }
    let mut arguments = Vec::new();
    if agent == SessionAgent::Cline {
        let mut index = 0;
        while index < argv.len() {
            let arg = &argv[index];
            let (flag, value) = if let Some((flag, value)) = arg.split_once('=') {
                (flag, Some(value))
            } else {
                (arg.as_str(), argv.get(index + 1).map(String::as_str))
            };
            if matches!(flag, "--config" | "--data-dir") {
                let raw = value
                    .filter(|value| !value.is_empty())
                    .context("Cline home option has no value")?;
                let value = absolute(raw, cwd)
                    .context("Cline home override uses unsupported drive-relative path")?;
                let string = value
                    .to_str()
                    .context("Invalid Cline home Unicode")?
                    .to_string();
                if flag == "--config" {
                    environment.insert("CLINE_DIR".into(), string.clone());
                } else {
                    environment.insert("CLINE_DATA_DIR".into(), string.clone());
                    environment.insert(
                        "CLINE_DB_DATA_DIR".into(),
                        value.join("db").to_string_lossy().into_owned(),
                    );
                }
                arguments.extend([flag.to_string(), string]);
                if !arg.contains('=') {
                    index += 1;
                }
            }
            index += 1;
        }
    }
    let home = agent
        .history_home(|key| environment.get(key).map(PathBuf::from))
        .context("Agent process has no history home")?;
    anyhow::ensure!(home.is_absolute(), "Agent history home is not absolute");
    Ok((environment, home, arguments))
}
fn session_id(agent: SessionAgent, argv: &[String]) -> Option<String> {
    let keys: &[&str] = match agent {
        SessionAgent::Claude => &["--resume", "-r", "--session-id"],
        SessionAgent::Codex => &["resume"],
        SessionAgent::OpenCode => &["--session", "-s"],
        SessionAgent::Antigravity => &["--conversation"],
        SessionAgent::Cline => &["--id"],
    };
    for (index, arg) in argv.iter().enumerate() {
        let (key, value) = arg
            .split_once('=')
            .map(|(key, value)| (key, Some(value)))
            .unwrap_or((arg, argv.get(index + 1).map(String::as_str)));
        if !keys.contains(&key) {
            continue;
        }
        let value = value?;
        return agent.canonical_session_id(value).ok();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::OsString, time::Duration};

    struct Fixture {
        root: PathBuf,
        job: Option<OwnedHandle>,
        process: Option<OwnedHandle>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(self.job.take());
            if let Some(process) = self.process.take() {
                unsafe {
                    WaitForSingleObject(process.as_raw_handle(), 2000);
                }
            }
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn fixture(program: &Path) -> Fixture {
        let mut fixture = Fixture {
            root: std::env::temp_dir().join(format!("flowmux-agent-한글-{}", uuid::Uuid::new_v4())),
            job: Some(super::super::worktree_git::job().unwrap()),
            process: None,
        };
        std::fs::create_dir_all(&fixture.root).unwrap();
        // A suspended copy tests native image/PEB/Job discovery without running
        // an agent, reading an account, or showing a console.
        let image = fixture.root.join("codex.exe");
        std::fs::copy(program, &image).unwrap();
        let arguments = [
            image.clone().into_os_string(),
            OsString::from("resume"),
            OsString::from("27bc8202-87a0-4ab6-a23c-9214e6a4a180"),
        ];
        let mut command = super::super::worktree_git::command(&arguments).unwrap();
        let mut environment: Vec<u16> = format!("CODEX_HOME=상태\\한글\0FLOWMUX_TEST_SECRET=must-not-be-retained\0PATH=C:\\owned-tools\0USERPROFILE={}\0\0", fixture.root.display()).encode_utf16().collect();
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of_val(&startup) as u32;
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        unsafe {
            checked(CreateProcessW(
                wide(&image).as_ptr(),
                command.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                environment.as_mut_ptr().cast(),
                wide(&fixture.root).as_ptr(),
                &startup,
                &mut info,
            ))
            .unwrap();
            fixture.process = Some(OwnedHandle::from_raw_handle(info.hProcess));
            let _thread = OwnedHandle::from_raw_handle(info.hThread);
            if let Err(error) = checked(AssignProcessToJobObject(
                fixture.job.as_ref().unwrap().as_raw_handle(),
                info.hProcess,
            )) {
                TerminateProcess(info.hProcess, 1);
                panic!("Cannot assign owned fixture: {error}");
            }
        }
        fixture
    }

    #[test]
    fn owned_job_agent_snapshot_unicode_home_membership_and_cancellation() {
        let _mode = super::super::session::ErrorMode::suppress_dialogs().unwrap();
        let windows = PathBuf::from(std::env::var_os("WINDIR").unwrap());
        // The WOW64 case is conditional only on that OS component existing.
        for program in [
            windows.join("System32/cmd.exe"),
            windows.join("SysWOW64/cmd.exe"),
        ] {
            if !program.is_file() {
                continue;
            }
            let fixture = fixture(&program);
            let cancel = AtomicBool::new(false);
            let value = discover(
                fixture.job.as_ref().unwrap(),
                &cancel,
                Instant::now() + Duration::from_secs(5),
            )
            .unwrap()
            .unwrap();
            assert_eq!(value.agent, SessionAgent::Codex);
            assert_eq!(value.cwd, fixture.root);
            assert_eq!(value.home, fixture.root.join("상태\\한글"));
            assert_eq!(
                value.session_id.as_deref(),
                Some("27bc8202-87a0-4ab6-a23c-9214e6a4a180")
            );
            assert!(value.prefix.is_empty());
            assert!(!value.environment.contains_key("FLOWMUX_TEST_SECRET"));
            assert_eq!(value.environment.len(), 3);
            assert!(value.started > 0);
            let other = super::super::worktree_git::job().unwrap();
            assert!(
                discover(&other, &cancel, Instant::now() + Duration::from_secs(1))
                    .unwrap()
                    .is_none()
            );
            cancel.store(true, Ordering::Release);
            assert!(discover(
                fixture.job.as_ref().unwrap(),
                &cancel,
                Instant::now() + Duration::from_secs(1)
            )
            .is_err());
        }
    }

    #[test]
    fn agent_identity_safe_node_prefix_and_cline_home_options() {
        let cwd = Path::new("C:\\작업");
        let node = Path::new("C:\\tools\\node.exe");
        let argv = vec![
            node.display().to_string(),
            "--enable-source-maps".into(),
            "node_modules\\@openai\\codex\\bin\\codex.js".into(),
            "resume".into(),
        ];
        let (agent, prefix, first) = identity(node, &argv, cwd).unwrap();
        assert_eq!(agent, SessionAgent::Codex);
        assert_eq!(first, 3);
        assert_eq!(
            prefix,
            vec![
                "--enable-source-maps",
                "C:\\작업\\node_modules\\@openai\\codex\\bin\\codex.js"
            ]
        );
        assert!(identity(
            node,
            &["node.exe".into(), "-e".into(), argv[2].clone()],
            cwd
        )
        .is_none());
        assert!(identity(
            node,
            &["node.exe".into(), "other.js".into(), argv[2].clone()],
            cwd
        )
        .is_none());
        assert!(identity(Path::new("C:\\tools\\bash.exe"), &argv, cwd).is_none());
        let raw = BTreeMap::from([
            ("USERPROFILE".into(), "C:\\사람".into()),
            ("PATH".into(), "C:\\tools".into()),
            ("SECRET".into(), "hidden".into()),
        ]);
        let (environment, home, arguments) = home(
            SessionAgent::Cline,
            &raw,
            cwd,
            &["--config=설정".into(), "--data-dir".into(), "자료".into()],
        )
        .unwrap();
        assert_eq!(home, cwd.join("자료/db"));
        assert_eq!(
            arguments,
            vec!["--config", "C:\\작업\\설정", "--data-dir", "C:\\작업\\자료"]
        );
        assert!(!environment.contains_key("SECRET"));
        assert!(absolute("D:relative", cwd).is_none());
        assert!(
            command_line("\"C:\\앱 폴더\\codex.exe\" resume \"한글 value\"").unwrap()[2]
                == "한글 value"
        );
    }
}
