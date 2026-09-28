// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded Windows Files directory snapshots.
//! One worker; no UI/COM objects; cancellation never frees a running I/O owner.
//! A delivered Response retains admission until the host consumes/drops it.

use crate::files_model::{self as model, Owner, Row, Snapshot, Warning};
use anyhow::{ensure, Context};
use serde::Serialize;
#[cfg(windows)]
use std::io;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, MutexGuard,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const MAX_ADMITTED: usize = 8;
pub const MAX_INPUT_BYTES: usize = model::MAX_SNAPSHOT_BYTES;
pub const BUDGET: Duration = Duration::from_secs(4);

#[derive(Debug)]
pub struct Request {
    pub owner: Owner,
    /// Normalized absolute local root. Canonical identity must remain the same.
    pub root: PathBuf,
    pub expanded: Vec<String>,
    /// Original IPC receipt + BUDGET; never refreshed by a worker stage.
    pub deadline: Instant,
}
pub struct Response {
    pub owner: Owner,
    pub snapshot: Option<Snapshot>,
    pub error: Option<String>,
    // Bound posted-but-unconsumed snapshots as well as queued/running jobs.
    _permit: Option<Permit>,
}
impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilesResponse")
            .field("owner", &self.owner)
            .field("snapshot", &self.snapshot)
            .field("error", &self.error)
            .finish()
    }
}
impl Response {
    fn error(owner: Owner, error: impl std::fmt::Display) -> Self {
        Self {
            owner,
            snapshot: None,
            error: Some(model::short_error(error)),
            _permit: None,
        }
    }
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct Status {
    pub admitted: usize,
    pub queued: usize,
    pub running: usize,
    pub posted: usize,
    pub admitted_input_bytes: usize,
    pub completed: u64,
    pub rejected: u64,
    pub stopped: bool,
}
struct Control {
    owner: Owner,
    cancelled: AtomicBool,
    deadline: Instant,
}
impl Control {
    fn check(&self) -> anyhow::Result<()> {
        ensure!(
            !self.cancelled.load(Ordering::Acquire),
            "Files request was cancelled"
        );
        ensure!(
            Instant::now() < self.deadline,
            "Files request exceeded its original four-second deadline"
        );
        Ok(())
    }
}
#[derive(Clone)]
pub struct Ticket {
    control: Arc<Control>,
}
impl Ticket {
    pub fn cancel(&self) {
        self.control.cancelled.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.control.cancelled.load(Ordering::Acquire)
    }
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.control.deadline
    }
    pub fn owner(&self) -> &Owner {
        &self.control.owner
    }
}
#[derive(Default)]
struct Counters {
    admitted: AtomicUsize,
    input_bytes: AtomicUsize,
    completed: AtomicU64,
    rejected: AtomicU64,
}
struct Permit {
    counters: Arc<Counters>,
    bytes: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.counters
            .input_bytes
            .fetch_sub(self.bytes, Ordering::AcqRel);
        self.counters.admitted.fetch_sub(1, Ordering::AcqRel);
        self.counters.completed.fetch_add(1, Ordering::AcqRel);
    }
}
struct Job {
    id: Uuid,
    request: Request,
    control: Arc<Control>,
    permit: Permit,
}
#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    active: HashMap<Uuid, Arc<Control>>,
    running: usize,
    stopped: bool,
}
type Execute = dyn Fn(&Request, &Control) -> Response + Send + Sync;
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    counters: Arc<Counters>,
    emit: Box<dyn Fn(Response) + Send + Sync>,
    execute: Box<Execute>,
}
impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn emit(&self, response: Response) {
        // A callback failure must release its owned response/permit, not poison
        // the directory worker or strand the rest of the queue.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.emit)(response)));
    }
    fn cancelled(&self, job: Job, reason: &str) {
        let mut response = Response::error(job.request.owner.clone(), reason);
        response._permit = Some(job.permit);
        self.emit(response);
    }
}
pub struct Service {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
impl Service {
    pub fn start(emit: impl Fn(Response) + Send + Sync + 'static) -> anyhow::Result<Self> {
        Self::start_with(emit, scan)
    }
    fn start_with(
        emit: impl Fn(Response) + Send + Sync + 'static,
        execute: impl Fn(&Request, &Control) -> Response + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            wake: Condvar::new(),
            counters: Arc::new(Counters::default()),
            emit: Box::new(emit),
            execute: Box::new(execute),
        });
        let worker_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("flowmux-files".into())
            .spawn(move || worker(worker_shared))
            .context("cannot start Files directory worker")?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    /// Pure, nonblocking admission. New generations supersede this instance's
    /// queued/active work; caller still rejects stale results already posted.
    pub fn submit(&self, request: Request) -> anyhow::Result<Ticket> {
        let bytes = match validate_request(&request) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.shared
                    .counters
                    .rejected
                    .fetch_add(1, Ordering::Relaxed);
                return Err(error);
            }
        };
        let control = Arc::new(Control {
            owner: request.owner.clone(),
            cancelled: AtomicBool::new(false),
            deadline: request.deadline,
        });
        let mut queue = self.shared.queue();
        if queue.stopped
            || queue.active.values().any(|old| {
                old.owner.instance == request.owner.instance
                    && old.owner.generation >= request.owner.generation
            })
        {
            self.shared
                .counters
                .rejected
                .fetch_add(1, Ordering::Relaxed);
            anyhow::bail!("Files service is stopped or the request generation is stale");
        }
        for old in queue
            .active
            .values()
            .filter(|old| old.owner.instance == request.owner.instance)
        {
            old.cancelled.store(true, Ordering::Release);
        }
        let mut cancelled = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(job) = queue.jobs.pop_front() {
            if job.control.owner.instance == request.owner.instance || job.control.check().is_err()
            {
                queue.active.remove(&job.id);
                cancelled.push(job);
            } else {
                retained.push_back(job);
            }
        }
        queue.jobs = retained;
        // Reserve atomically while holding the queue lock. Permit drops can only
        // decrease admission concurrently, so this count cannot over-admit.
        let admitted = self
            .shared
            .counters
            .admitted
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_ADMITTED).then_some(count + 1)
            })
            .is_ok();
        if admitted {
            self.shared
                .counters
                .input_bytes
                .fetch_add(bytes, Ordering::AcqRel);
            let permit = Permit {
                counters: self.shared.counters.clone(),
                bytes,
            };
            let id = Uuid::new_v4();
            queue.active.insert(id, control.clone());
            queue.jobs.push_back(Job {
                id,
                request,
                control: control.clone(),
                permit,
            });
        } else {
            self.shared
                .counters
                .rejected
                .fetch_add(1, Ordering::Relaxed);
        }
        drop(queue);
        for job in cancelled {
            self.shared.cancelled(
                job,
                "Files request was superseded or expired before execution",
            );
        }
        ensure!(
            admitted,
            "Files is busy (eight admitted requests, including posted results)"
        );
        self.shared.wake.notify_one();
        Ok(Ticket { control })
    }
    pub fn cancel_owner(&self, instance: Uuid) {
        self.cancel_matching(Some(instance), false);
    }
    pub fn stop(&self) {
        self.cancel_matching(None, true);
    }
    fn cancel_matching(&self, instance: Option<Uuid>, stopped: bool) {
        let mut queue = self.shared.queue();
        queue.stopped |= stopped;
        for control in queue
            .active
            .values()
            .filter(|c| instance.is_none_or(|i| i == c.owner.instance))
        {
            control.cancelled.store(true, Ordering::Release);
        }
        let mut cancelled = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(job) = queue.jobs.pop_front() {
            if instance.is_none_or(|i| i == job.control.owner.instance) {
                queue.active.remove(&job.id);
                cancelled.push(job);
            } else {
                retained.push_back(job);
            }
        }
        queue.jobs = retained;
        drop(queue);
        for job in cancelled {
            self.shared
                .cancelled(job, "Files request was cancelled before execution");
        }
        self.shared.wake.notify_all();
    }
    pub fn status(&self) -> Status {
        let queue = self.shared.queue();
        let counters = &self.shared.counters;
        let admitted = counters.admitted.load(Ordering::Acquire);
        Status {
            admitted,
            queued: queue.jobs.len(),
            running: queue.running,
            posted: admitted.saturating_sub(queue.jobs.len() + queue.running),
            admitted_input_bytes: counters.input_bytes.load(Ordering::Acquire),
            completed: counters.completed.load(Ordering::Acquire),
            rejected: counters.rejected.load(Ordering::Acquire),
            stopped: queue.stopped,
        }
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
        drop(self.thread.take());
    } // detach; pending I/O retains all its owners
}
fn worker(shared: Arc<Shared>) {
    loop {
        let job = {
            let mut queue = shared.queue();
            while queue.jobs.is_empty() && !queue.stopped {
                queue = shared.wake.wait(queue).unwrap_or_else(|p| p.into_inner());
            }
            let Some(job) = queue.jobs.pop_front() else {
                return;
            };
            queue.running = 1;
            job
        };
        let mut response = match job.control.check() {
            Err(error) => Response::error(job.request.owner.clone(), error),
            Ok(()) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (shared.execute)(&job.request, &job.control)
            }))
            .unwrap_or_else(|_| {
                Response::error(job.request.owner.clone(), "Files directory worker panicked")
            }),
        };
        if let Err(error) = job.control.check() {
            response = Response::error(job.request.owner.clone(), error);
        }
        {
            let mut queue = shared.queue();
            queue.running = 0;
            queue.active.remove(&job.id);
        }
        response._permit = Some(job.permit);
        // Release potentially large request inputs before emitting completion.
        drop(job.request);
        drop(job.control);
        shared.emit(response);
    }
}
fn validate_request(request: &Request) -> anyhow::Result<usize> {
    crate::editor::validate_path(&request.root)?;
    let mut bytes = request
        .root
        .to_str()
        .context("Files root is not Unicode")?
        .len();
    ensure!(
        request.root.is_absolute() && bytes <= model::MAX_PATH_BYTES,
        "Files root must be a bounded local absolute path"
    );
    ensure!(
        request.expanded.len() <= model::MAX_EXPANDED,
        "Files expansion exceeds 64 directories"
    );
    let mut seen = HashSet::new();
    for path in &request.expanded {
        ensure!(
            model::relative(path)? == *path,
            "Files expansion paths must use forward slashes"
        );
        ensure!(
            seen.insert(path),
            "Files expansion contains duplicate paths"
        );
        bytes = bytes
            .checked_add(path.len())
            .context("Files request byte count overflow")?;
    }
    ensure!(
        bytes <= MAX_INPUT_BYTES,
        "Files request path inputs exceed four MiB"
    );
    ensure!(
        Instant::now() < request.deadline
            && request.deadline.saturating_duration_since(Instant::now()) <= BUDGET,
        "Files request must retain its original four-second budget"
    );
    Ok(bytes)
}

fn scan(request: &Request, control: &Control) -> Response {
    match snapshot(request, control) {
        Ok(snapshot) => Response {
            owner: request.owner.clone(),
            snapshot: Some(snapshot),
            error: None,
            _permit: None,
        },
        Err(error) => Response::error(request.owner.clone(), format!("{error:#}")),
    }
}
struct Scan<'a> {
    root: &'a Path,
    expanded: HashSet<&'a str>,
    control: &'a Control,
    snapshot: Snapshot,
    capped: bool,
}
fn snapshot(request: &Request, control: &Control) -> anyhow::Result<Snapshot> {
    control.check()?;
    let root = crate::editor::canonical_root(&request.root)?;
    ensure!(
        crate::editor_search::same_path(&root, &request.root),
        "Files root identity changed or traverses a reparse alias"
    );
    let mut scan = Scan {
        root: &root,
        expanded: request.expanded.iter().map(String::as_str).collect(),
        control,
        snapshot: Snapshot {
            owner: request.owner.clone(),
            root: root.clone(),
            rows: Vec::new(),
            warnings: Vec::new(),
            truncated: false,
            visited_entries: 0,
            path_bytes: root.to_str().unwrap().len(),
        },
        capped: false,
    };
    scan.visit("", 0)?;
    control.check()?;
    scan.snapshot.validate()?;
    Ok(scan.snapshot)
}
impl Scan<'_> {
    fn warning(&mut self, path: &str, error: impl std::fmt::Display) {
        if self.snapshot.warnings.len() < model::MAX_WARNINGS {
            self.snapshot.warnings.push(Warning {
                path: path.into(),
                message: model::short_error(error),
            });
        }
    }
    fn cap(&mut self, path: &str, message: &str) {
        self.capped = true;
        self.snapshot.truncated = true;
        self.warning(path, message);
    }
    fn visit(&mut self, relative: &str, depth: u16) -> anyhow::Result<()> {
        self.control.check()?;
        if self.capped {
            return Ok(());
        }
        if depth > model::MAX_DEPTH {
            self.snapshot.truncated = true;
            self.warning(relative, "Files depth limit reached");
            return Ok(());
        }
        let path = if relative.is_empty() {
            self.root.to_path_buf()
        } else {
            self.root.join(relative)
        };
        let mut directory = match Directory::open(self.root, &path) {
            Ok(directory) => directory,
            Err(error) if relative.is_empty() => {
                return Err(error).context("cannot read Files root")
            }
            Err(error) => {
                self.warning(relative, error);
                return Ok(());
            }
        };
        let mut rows = Vec::new();
        loop {
            self.control.check()?;
            if self.snapshot.visited_entries == model::MAX_ENTRIES {
                self.cap(relative, "Files entry limit reached; listing is incomplete");
                break;
            }
            let entry = match directory.next(self.control) {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) if relative.is_empty() => {
                    return Err(error).context("cannot finish reading Files root")
                }
                Err(error) => {
                    self.warning(relative, error);
                    break;
                }
            };
            self.snapshot.visited_entries += 1;
            let Some(name) = entry.name else {
                self.warning(
                    relative,
                    "Filename contains invalid Unicode and was skipped",
                );
                continue;
            };
            if name == "." || name == ".." {
                continue;
            }
            let child = if relative.is_empty() {
                name.clone()
            } else {
                format!("{relative}/{name}")
            };
            if model::relative(&child).is_err() {
                self.warning(relative, "Unsupported or oversized filename was skipped");
                continue;
            }
            let bytes = child.len() + name.len();
            if self.snapshot.path_bytes.saturating_add(bytes) > model::MAX_SNAPSHOT_BYTES {
                self.cap(
                    relative,
                    "Files path cache reached four MiB; listing is incomplete",
                );
                break;
            }
            self.snapshot.path_bytes += bytes;
            rows.push(Row {
                expanded: entry.directory
                    && !entry.reparse
                    && self.expanded.contains(child.as_str()),
                path: child,
                name,
                directory: entry.directory,
                reparse: entry.reparse,
                depth,
            });
        }
        // Holding this handle is unnecessary while child directories are read.
        drop(directory);
        rows.sort_by(|a, b| {
            b.directory
                .cmp(&a.directory)
                .then_with(|| name_order(&a.name, &b.name))
        });
        for row in rows {
            self.control.check()?;
            let child = row.expanded.then(|| row.path.clone());
            self.snapshot.rows.push(row);
            if let Some(child) = child {
                self.visit(&child, depth + 1)?;
            }
        }
        Ok(())
    }
}
fn name_order(left: &str, right: &str) -> std::cmp::Ordering {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Globalization::CompareStringOrdinal;
        let left: Vec<_> = left.encode_utf16().collect();
        let right: Vec<_> = right.encode_utf16().collect();
        let result = unsafe {
            CompareStringOrdinal(
                left.as_ptr(),
                left.len() as i32,
                right.as_ptr(),
                right.len() as i32,
                1,
            )
        };
        result.cmp(&2).then_with(|| left.cmp(&right))
    }
    #[cfg(not(windows))]
    {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then_with(|| left.encode_utf16().cmp(right.encode_utf16()))
    }
}
struct Entry {
    name: Option<String>,
    directory: bool,
    reparse: bool,
}

#[cfg(not(windows))]
struct Directory {
    entries: fs::ReadDir,
}
#[cfg(not(windows))]
impl Directory {
    fn open(root: &Path, path: &Path) -> anyhow::Result<Self> {
        ensure!(
            !fs::symlink_metadata(path)?.file_type().is_symlink(),
            "Files directory is a symbolic link"
        );
        let canonical = fs::canonicalize(path)?;
        ensure!(
            canonical.starts_with(root),
            "Files directory is outside root"
        );
        Ok(Self {
            entries: fs::read_dir(path)?,
        })
    }
    fn next(&mut self, control: &Control) -> anyhow::Result<Option<Entry>> {
        control.check()?;
        self.entries
            .next()
            .map(|entry| {
                let entry = entry?;
                let kind = entry.file_type()?;
                Ok(Entry {
                    name: entry.file_name().into_string().ok(),
                    directory: kind.is_dir(),
                    reparse: kind.is_symlink(),
                })
            })
            .transpose()
    }
}

// FILE_ID_BOTH_DIR_INFO layout, checked against native bindings by the Windows
// test. Parsing uses byte slices/read_u32, never an unaligned Rust struct borrow.
#[cfg(any(windows, test))]
const DIRECTORY_HEADER: usize = 104;
#[cfg(any(windows, test))]
fn read_u32(bytes: &[u8], at: usize) -> anyhow::Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .context("truncated directory record")?
            .try_into()
            .unwrap(),
    ))
}
#[cfg(any(windows, test))]
fn parse_directory_block(bytes: &[u8]) -> anyhow::Result<Vec<Entry>> {
    ensure!(bytes.len() <= 64 * 1024, "directory buffer exceeds 64 KiB");
    let mut entries = Vec::new();
    let mut at = 0;
    loop {
        ensure!(
            bytes.len().saturating_sub(at) >= DIRECTORY_HEADER,
            "truncated directory header"
        );
        let next = read_u32(bytes, at)? as usize;
        let attributes = read_u32(bytes, at + 56)?;
        let name_bytes = read_u32(bytes, at + 60)? as usize;
        ensure!(
            name_bytes > 0 && name_bytes.is_multiple_of(2),
            "invalid directory filename length"
        );
        let end = at
            .checked_add(DIRECTORY_HEADER)
            .and_then(|v| v.checked_add(name_bytes))
            .context("directory offset overflow")?;
        let record_end = if next == 0 {
            bytes.len()
        } else {
            ensure!(
                next.is_multiple_of(8) && next >= DIRECTORY_HEADER,
                "invalid directory next-entry offset"
            );
            at.checked_add(next).context("directory offset overflow")?
        };
        ensure!(
            end <= record_end && record_end <= bytes.len(),
            "directory record exceeds its buffer"
        );
        let units: Vec<_> = bytes[at + DIRECTORY_HEADER..end]
            .chunks_exact(2)
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .collect();
        entries.push(Entry {
            name: String::from_utf16(&units).ok(),
            directory: attributes & 0x10 != 0,
            reparse: attributes & 0x400 != 0,
        });
        if next == 0 {
            break;
        }
        at = record_end;
    }
    Ok(entries)
}

#[cfg(windows)]
struct Directory {
    file: fs::File,
    buffer: Vec<u64>,
    ready: VecDeque<Entry>,
    started: bool,
    ended: bool,
}
#[cfg(windows)]
fn extended_path(path: &Path) -> anyhow::Result<PathBuf> {
    crate::editor::validate_path(path)?;
    ensure!(path.is_absolute(), "Files native path is not absolute");
    // Required before verbatim prefix: Win32 no longer normalizes '/' afterwards.
    Ok(PathBuf::from(format!(
        "\\\\?\\{}",
        path.to_str()
            .context("Files path is not Unicode")?
            .replace('/', "\\")
    )))
}
#[cfg(windows)]
impl Directory {
    fn open(root: &Path, path: &Path) -> anyhow::Result<Self> {
        use std::os::windows::{
            ffi::OsStringExt,
            fs::{MetadataExt, OpenOptionsExt},
            io::AsRawHandle,
        };
        use windows_sys::Win32::Storage::FileSystem::*;
        let file = fs::OpenOptions::new()
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(extended_path(path)?)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_dir() && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
            "Files directory is not a regular non-reparse directory"
        );
        let mut wide = vec![0u16; crate::editor::MAX_PATH_UNITS + 1];
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                wide.as_mut_ptr(),
                wide.len() as u32,
                0,
            )
        } as usize;
        ensure!(
            length > 0 && length < wide.len(),
            "cannot identify opened Files directory: {}",
            io::Error::last_os_error()
        );
        wide.truncate(length);
        let identity =
            crate::editor::display_path(PathBuf::from(std::ffi::OsString::from_wide(&wide)));
        crate::editor::validate_path(&identity)?;
        ensure!(
            identity
                .ancestors()
                .any(|ancestor| crate::editor_search::same_path(ancestor, root)),
            "opened Files directory is outside root"
        );
        // Require the intended path identity too; aliases/renames must not silently
        // populate another directory under a retained row's name.
        ensure!(
            crate::editor_search::same_path(&identity, path),
            "opened Files directory identity changed"
        );
        Ok(Self {
            file,
            buffer: vec![0; 8192],
            ready: VecDeque::new(),
            started: false,
            ended: false,
        })
    }
    fn next(&mut self, control: &Control) -> anyhow::Result<Option<Entry>> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::ERROR_NO_MORE_FILES,
            Storage::FileSystem::{
                FileIdBothDirectoryInfo, FileIdBothDirectoryRestartInfo,
                GetFileInformationByHandleEx,
            },
        };
        control.check()?;
        if let Some(entry) = self.ready.pop_front() {
            return Ok(Some(entry));
        }
        if self.ended {
            return Ok(None);
        }
        self.buffer.fill(0);
        let class = if self.started {
            FileIdBothDirectoryInfo
        } else {
            FileIdBothDirectoryRestartInfo
        };
        let success = unsafe {
            GetFileInformationByHandleEx(
                self.file.as_raw_handle(),
                class,
                self.buffer.as_mut_ptr().cast(),
                (self.buffer.len() * 8) as u32,
            )
        };
        self.started = true;
        if success == 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                self.ended = true;
                return Ok(None);
            }
            return Err(error).context("Files directory enumeration failed");
        }
        control.check()?;
        let bytes = unsafe {
            std::slice::from_raw_parts(self.buffer.as_ptr().cast::<u8>(), self.buffer.len() * 8)
        };
        self.ready.extend(parse_directory_block(bytes)?);
        Ok(self.ready.pop_front())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("flowmux-files-{}", Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(crate::editor::display_path(fs::canonicalize(path).unwrap()))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn request(root: &Temp) -> Request {
        Request {
            owner: Owner {
                workspace: Uuid::new_v4(),
                pane: Uuid::new_v4(),
                instance: Uuid::new_v4(),
                generation: 1,
            },
            root: root.0.clone(),
            expanded: Vec::new(),
            deadline: Instant::now() + BUDGET,
        }
    }
    fn run(request: &Request) -> Response {
        scan(
            request,
            &Control {
                owner: request.owner.clone(),
                cancelled: AtomicBool::new(false),
                deadline: request.deadline,
            },
        )
    }
    fn record(name: &[u16], attrs: u32) -> Vec<u8> {
        let mut bytes = vec![0; DIRECTORY_HEADER + name.len() * 2];
        bytes[56..60].copy_from_slice(&attrs.to_le_bytes());
        bytes[60..64].copy_from_slice(&((name.len() * 2) as u32).to_le_bytes());
        for (index, unit) in name.iter().enumerate() {
            bytes[DIRECTORY_HEADER + index * 2..DIRECTORY_HEADER + index * 2 + 2]
                .copy_from_slice(&unit.to_le_bytes());
        }
        bytes
    }
    #[test]
    fn parser_preserves_exact_unicode_and_rejects_malformed_offsets() {
        let text = "한글-한-é-😀";
        let bytes = record(&text.encode_utf16().collect::<Vec<_>>(), 0x410);
        let result = parse_directory_block(&bytes).unwrap();
        assert_eq!(result[0].name.as_deref(), Some(text));
        assert!(result[0].directory && result[0].reparse);
        assert!(parse_directory_block(&record(&[0xd800], 0)).unwrap()[0]
            .name
            .is_none());
        for next in [1u32, 104, u32::MAX] {
            let mut bad = bytes.clone();
            bad[..4].copy_from_slice(&next.to_le_bytes());
            assert!(parse_directory_block(&bad).is_err());
        }
        let mut bad = bytes.clone();
        bad[60..64].copy_from_slice(&3u32.to_le_bytes());
        assert!(parse_directory_block(&bad).is_err());
        assert!(parse_directory_block(&bytes[..100]).is_err());
        let first_size = (bytes.len() + 7) & !7;
        let mut joined = bytes.clone();
        joined.resize(first_size, 0);
        joined[..4].copy_from_slice(&(first_size as u32).to_le_bytes());
        joined.extend(record(&"next.txt".encode_utf16().collect::<Vec<_>>(), 0));
        let two = parse_directory_block(&joined).unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[1].name.as_deref(), Some("next.txt"));
    }
    #[test]
    fn snapshot_expands_only_requested_directories_and_keeps_hidden_names() {
        let root = Temp::new();
        fs::create_dir_all(root.0.join("dir/sub")).unwrap();
        for path in [
            ".hidden",
            "z.txt",
            "dir/한.txt",
            "dir/한.txt",
            "dir/sub/not-read.txt",
        ] {
            fs::write(root.0.join(path), "text").unwrap();
        }
        let mut request = request(&root);
        request.expanded.push("dir".into());
        let result = run(&request);
        assert!(result.error.is_none(), "{:?}", result.error);
        let snapshot = result.snapshot.unwrap();
        snapshot.validate().unwrap();
        let paths: Vec<_> = snapshot.rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths[0], "dir");
        assert_eq!(paths[1], "dir/sub");
        assert!(
            paths.contains(&".hidden")
                && paths.contains(&"dir/한.txt")
                && paths.contains(&"dir/한.txt")
        );
        assert!(!paths.contains(&"dir/sub/not-read.txt"));
        request.expanded.push("dir/sub".into());
        assert!(run(&request)
            .snapshot
            .unwrap()
            .rows
            .iter()
            .any(|r| r.path == "dir/sub/not-read.txt"));
    }
    #[test]
    fn failures_and_original_deadlines_are_explicit() {
        let root = Temp::new();
        let mut request = request(&root);
        request.expanded.push("../outside".into());
        assert!(validate_request(&request).is_err());
        request.expanded.clear();
        request.deadline = Instant::now() - Duration::from_millis(1);
        assert!(run(&request).error.unwrap().contains("deadline"));
        request.deadline = Instant::now() + BUDGET;
        fs::remove_dir(&root.0).unwrap();
        assert!(run(&request).error.is_some());
    }
    struct Gate(Arc<(Mutex<bool>, Condvar)>);
    impl Gate {
        fn new() -> Self {
            Self(Arc::new((Mutex::new(false), Condvar::new())))
        }
        fn open(&self) {
            *self.0 .0.lock().unwrap() = true;
            self.0 .1.notify_all();
        }
    }
    impl Drop for Gate {
        fn drop(&mut self) {
            self.open();
        }
    }
    #[test]
    fn cancellation_and_posted_responses_keep_their_admission_permits() {
        let root = Temp::new();
        let gate = Gate::new();
        let waited = gate.0.clone();
        let (started_send, started_receive) = mpsc::channel();
        let (send, receive) = mpsc::channel();
        let service = Service::start_with(
            move |response| {
                send.send(response).unwrap();
            },
            move |request, _| {
                started_send.send(()).unwrap();
                let mut open = waited.0.lock().unwrap();
                while !*open {
                    open = waited.1.wait(open).unwrap();
                }
                Response::error(request.owner.clone(), "controlled completion")
            },
        )
        .unwrap();
        let running = service.submit(request(&root)).unwrap();
        started_receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        for _ in 1..MAX_ADMITTED {
            service.submit(request(&root)).unwrap();
        }
        assert!(service.submit(request(&root)).is_err());
        running.cancel();
        assert_eq!(service.status().running, 1);
        assert_eq!(service.status().admitted, MAX_ADMITTED);
        service.stop(); // queued cancellation completions still own their permits
        let queued: Vec<_> = (1..MAX_ADMITTED)
            .map(|_| receive.recv_timeout(Duration::from_secs(2)).unwrap())
            .collect();
        assert_eq!(service.status().admitted, MAX_ADMITTED);
        drop(queued);
        assert_eq!(service.status().admitted, 1);
        gate.open();
        let completed = receive.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(completed.error.as_deref().unwrap().contains("cancelled"));
        assert_eq!(service.status().running, 0);
        assert_eq!(service.status().admitted, 1);
        drop(completed);
        assert_eq!(service.status().admitted, 0);
    }
    #[test]
    fn drop_does_not_join_a_blocked_worker_and_response_remains_owned() {
        let root = Temp::new();
        let gate = Gate::new();
        let waited = gate.0.clone();
        let (start_send, start_receive) = mpsc::channel();
        let (send, receive) = mpsc::channel();
        let service = Service::start_with(
            move |response| {
                let _ = send.send(response);
            },
            move |request, _| {
                start_send.send(()).unwrap();
                let mut open = waited.0.lock().unwrap();
                while !*open {
                    open = waited.1.wait(open).unwrap();
                }
                Response::error(request.owner.clone(), "finished")
            },
        )
        .unwrap();
        service.submit(request(&root)).unwrap();
        start_receive.recv_timeout(Duration::from_secs(2)).unwrap();
        // If Drop joins, the test cannot advance to opening the owned gate.
        let (dropped_send, dropped_receive) = mpsc::channel();
        thread::spawn(move || {
            drop(service);
            dropped_send.send(()).unwrap();
        });
        dropped_receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        gate.open();
        assert!(receive
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .error
            .unwrap()
            .contains("cancelled"));
    }
    #[cfg(windows)]
    #[test]
    fn native_nested_forward_slash_unicode_long_path_enumeration() {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ID_BOTH_DIR_INFO;
        assert_eq!(
            std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileName),
            DIRECTORY_HEADER
        );
        assert_eq!(
            std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileAttributes),
            56
        );
        assert_eq!(
            std::mem::offset_of!(FILE_ID_BOTH_DIR_INFO, FileNameLength),
            60
        );
        let root = Temp::new();
        let mut base = root.0.clone();
        while base.as_os_str().encode_wide().count() < 300 {
            base.push("한글-한-é😀");
        }
        fs::create_dir_all(base.join("nested/child")).unwrap();
        fs::write(base.join("nested/child/한-é😀.txt"), "text").unwrap();
        let base = crate::editor::display_path(fs::canonicalize(base).unwrap());
        let request = Request {
            root: base.clone(),
            expanded: vec!["nested".into(), "nested/child".into()],
            ..request(&root)
        };
        let result = run(&request);
        assert!(result.error.is_none(), "{:?}", result.error);
        assert!(result
            .snapshot
            .unwrap()
            .rows
            .iter()
            .any(|row| row.path == "nested/child/한-é😀.txt"));
        let control = Control {
            owner: request.owner,
            deadline: Instant::now() + BUDGET,
            cancelled: AtomicBool::new(false),
        };
        for spelling in [
            base.join("nested/child"),
            PathBuf::from(
                base.join("nested/child")
                    .to_str()
                    .unwrap()
                    .replace('/', "\\"),
            ),
        ] {
            let mut directory = Directory::open(&base, &spelling).unwrap();
            let mut names = Vec::new();
            while let Some(entry) = directory.next(&control).unwrap() {
                if let Some(name) = entry.name {
                    names.push(name);
                }
            }
            assert!(names.contains(&"한-é😀.txt".into()));
        }
        let outside = Temp::new();
        assert!(Directory::open(&base, &outside.0).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn native_enumeration_keeps_the_opened_directory_after_path_replacement() {
        let root = Temp::new();
        let original = root.0.join("before");
        fs::create_dir(&original).unwrap();
        fs::write(original.join("original-한.txt"), "original").unwrap();
        let mut directory = Directory::open(&root.0, &original).unwrap();
        fs::rename(&original, root.0.join("moved")).unwrap();
        fs::create_dir(&original).unwrap();
        fs::write(original.join("replacement.txt"), "replacement").unwrap();
        let request = request(&root);
        let control = Control {
            owner: request.owner,
            deadline: Instant::now() + BUDGET,
            cancelled: AtomicBool::new(false),
        };
        let mut names = Vec::new();
        while let Some(entry) = directory.next(&control).unwrap() {
            if let Some(name) = entry.name {
                names.push(name);
            }
        }
        assert!(names.contains(&"original-한.txt".into()));
        assert!(!names.contains(&"replacement.txt".into()));
    }
}
