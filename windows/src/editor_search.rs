// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, read-only editor search, separate from ordered document/recovery I/O.
//!
//! Cancellation and deadlines are cooperative. A blocked filesystem call keeps
//! its fixed worker and admission permit until it returns; UI Drop never joins.
//! Callers must reject stale surface/instance/generation results and may report
//! a deadline before its eventual completion without releasing that permit.

use anyhow::{ensure, Context};
use flowmux_editor::{
    SearchCancellation, SearchDocument, SearchOptions, WorkspaceSearchMatch, WorkspaceSearchResult,
};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::{Regex, RegexBuilder};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const THREADS: usize = 2;
pub const MAX_ADMITTED: usize = 8;
pub const MAX_ADMITTED_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_INDEX_PATHS: usize = 2_000;
pub const MAX_VISITED_ENTRIES: usize = 20_000;
pub const MAX_SCANNED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RESULT_BYTES: usize = 1024 * 1024;
pub const MAX_QUERY_BYTES: usize = 4 * 1024;
pub const MAX_PATH_BYTES: usize = 16 * 1024;
pub const SEARCH_BUDGET: Duration = Duration::from_secs(4);
const RESPONSE_RESERVE_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Token {
    pub surface: Uuid,
    pub instance: Uuid,
    pub request_id: String,
    pub generation: u64,
}

#[derive(Debug)]
pub enum Kind {
    QuickOpen,
    Workspace {
        query: String,
        options: SearchOptions,
        /// Every open path overrides disk, including buffers too large to search.
        open_documents: Vec<SearchDocument>,
    },
}

#[derive(Debug)]
pub struct Request {
    pub token: Token,
    pub root: PathBuf,
    pub kind: Kind,
    /// Include time already spent flushing/capturing the document snapshot.
    pub deadline: Instant,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    QuickOpen { paths: Vec<String>, truncated: bool },
    Workspace { result: WorkspaceSearchResult },
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceStamp {
    pub path: String,
    /// SHA-256 of the exact LF text searched (one disk BOM removed).
    pub sha256: String,
    pub from_open_buffer: bool,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Diagnostics {
    pub visited_entries: usize,
    pub searched_files: usize,
    pub scanned_bytes: usize,
    pub skipped_binary: usize,
    pub skipped_encoding: usize,
    pub skipped_oversized: usize,
    pub skipped_unreadable: usize,
    pub skipped_path: usize,
    pub skipped_reparse: usize,
    pub cancelled: bool,
    pub deadline_exceeded: bool,
    pub entry_limit: bool,
    pub depth_limit: bool,
    pub ignore_bytes: usize,
    pub byte_limit: bool,
    pub result_byte_limit: bool,
    /// At most eight descriptions, each at most 512 Unicode scalar values.
    pub errors: Vec<String>,
}
impl Diagnostics {
    fn error(&mut self, error: impl std::fmt::Display) {
        if self.errors.len() < 8 {
            self.errors
                .push(error.to_string().chars().take(512).collect());
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Response {
    pub token: Token,
    pub outcome: Outcome,
    pub sources: Vec<SourceStamp>,
    pub diagnostics: Diagnostics,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub admitted: usize,
    pub queued: usize,
    pub running: usize,
    pub admitted_bytes: usize,
    pub completed: u64,
    pub rejected: u64,
    pub stopped: bool,
}

struct Control {
    token: Token,
    cancellation: SearchCancellation,
    deadline: Instant,
}
impl Control {
    fn interrupted(&self) -> bool {
        self.cancellation.is_cancelled() || Instant::now() >= self.deadline
    }
}

#[derive(Clone)]
pub struct Ticket {
    control: Arc<Control>,
}
impl Ticket {
    pub fn cancel(&self) {
        self.control.cancellation.cancel();
    }
    pub fn is_cancelled(&self) -> bool {
        self.control.cancellation.is_cancelled()
    }
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.control.deadline
    }
    pub fn token(&self) -> &Token {
        &self.control.token
    }
}

struct Job {
    id: Uuid,
    request: Request,
    control: Arc<Control>,
    bytes: usize,
}
struct Active {
    control: Arc<Control>,
    bytes: usize,
}
#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    active: HashMap<Uuid, Active>,
    status: Status,
}
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    emit: Arc<dyn Fn(Response) + Send + Sync>,
    execute: Arc<Executor>,
}
type Executor = dyn Fn(&Request, &Control) -> Response + Send + Sync;
impl Shared {
    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn emit(&self, response: Response) {
        // A callback is a nonblocking event send, never run while holding Queue.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.emit)(response)));
    }
    fn finish(&self, id: Uuid, running: bool) {
        let mut queue = self.queue();
        if let Some(active) = queue.active.remove(&id) {
            queue.status.admitted -= 1;
            queue.status.admitted_bytes -= active.bytes;
            queue.status.completed = queue.status.completed.saturating_add(1);
            if running {
                queue.status.running -= 1;
            }
        }
    }
}

pub struct Service {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
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
            emit: Arc::new(emit),
            execute: Arc::new(execute),
        });
        let mut service = Self {
            shared,
            threads: Vec::new(),
        };
        for index in 0..THREADS {
            let shared = service.shared.clone();
            service.threads.push(
                thread::Builder::new()
                    .name(format!("flowmux-editor-search-{index}"))
                    .spawn(move || worker(shared))
                    .context("cannot start editor search worker")?,
            );
        }
        Ok(service)
    }

    /// Pure validation/admission only; filesystem work is confined to workers.
    /// A new generation cancels this owner's older requests, retaining running
    /// permits until their calls finish. Queued old work is removed immediately.
    pub fn submit(&self, request: Request) -> anyhow::Result<Ticket> {
        let bytes = match request_bytes(&request) {
            Ok(bytes) => bytes,
            Err(error) => {
                let mut queue = self.shared.queue();
                queue.status.rejected = queue.status.rejected.saturating_add(1);
                return Err(error);
            }
        };
        let control = Arc::new(Control {
            token: request.token.clone(),
            cancellation: SearchCancellation::default(),
            deadline: request.deadline,
        });
        let id = Uuid::new_v4();
        let mut queue = self.shared.queue();
        if queue.status.stopped {
            queue.status.rejected = queue.status.rejected.saturating_add(1);
            anyhow::bail!("editor search service is stopped");
        }
        let same_owner = |token: &Token| {
            token.surface == request.token.surface && token.instance == request.token.instance
        };
        if queue.active.values().any(|active| {
            same_owner(&active.control.token)
                && active.control.token.generation >= request.token.generation
        }) {
            queue.status.rejected = queue.status.rejected.saturating_add(1);
            anyhow::bail!("editor search generation is stale");
        }
        for active in queue
            .active
            .values()
            .filter(|a| same_owner(&a.control.token))
        {
            active.control.cancellation.cancel();
        }
        let mut cancelled = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(job) = queue.jobs.pop_front() {
            if same_owner(&job.control.token) || job.control.interrupted() {
                queue.active.remove(&job.id);
                queue.status.admitted -= 1;
                queue.status.admitted_bytes -= job.bytes;
                queue.status.completed = queue.status.completed.saturating_add(1);
                cancelled.push(job);
            } else {
                retained.push_back(job);
            }
        }
        queue.jobs = retained;
        queue.status.queued = queue.jobs.len();
        let full = queue.status.admitted >= MAX_ADMITTED
            || queue.status.admitted_bytes.saturating_add(bytes) > MAX_ADMITTED_BYTES;
        if full {
            queue.status.rejected = queue.status.rejected.saturating_add(1);
        } else {
            queue.active.insert(
                id,
                Active {
                    control: control.clone(),
                    bytes,
                },
            );
            queue.jobs.push_back(Job {
                id,
                request,
                control: control.clone(),
                bytes,
            });
            queue.status.admitted += 1;
            queue.status.admitted_bytes += bytes;
            queue.status.queued = queue.jobs.len();
        }
        drop(queue);
        for job in cancelled {
            self.shared
                .emit(cancelled_response(&job.request, &job.control));
        }
        ensure!(
            !full,
            "editor search is busy (bounded admission or snapshot byte limit)"
        );
        self.shared.wake.notify_one();
        Ok(Ticket { control })
    }

    pub fn cancel_owner(&self, surface: Uuid, instance: Uuid) {
        let mut queue = self.shared.queue();
        for active in queue.active.values() {
            if active.control.token.surface == surface && active.control.token.instance == instance
            {
                active.control.cancellation.cancel();
            }
        }
        let mut cancelled = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(job) = queue.jobs.pop_front() {
            if job.control.token.surface == surface && job.control.token.instance == instance {
                cancelled.push(job);
            } else {
                retained.push_back(job);
            }
        }
        queue.jobs = retained;
        queue.status.queued = queue.jobs.len();
        drop(queue);
        for job in cancelled {
            self.shared.finish(job.id, false);
            self.shared
                .emit(cancelled_response(&job.request, &job.control));
        }
        self.shared.wake.notify_all();
    }
    pub fn status(&self) -> Status {
        self.shared.queue().status.clone()
    }
    pub fn stop(&self) {
        let cancelled = {
            let mut queue = self.shared.queue();
            queue.status.stopped = true;
            for active in queue.active.values() {
                active.control.cancellation.cancel();
            }
            let cancelled: Vec<_> = queue.jobs.drain(..).collect();
            queue.status.queued = 0;
            cancelled
        };
        for job in cancelled {
            self.shared.finish(job.id, false);
            self.shared
                .emit(cancelled_response(&job.request, &job.control));
        }
        self.shared.wake.notify_all();
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.stop();
        self.threads.clear(); // detach; never join filesystem work on UI.
    }
}

fn worker(shared: Arc<Shared>) {
    loop {
        let job = {
            let mut queue = shared.queue();
            loop {
                if let Some(job) = queue.jobs.pop_front() {
                    queue.status.queued = queue.jobs.len();
                    queue.status.running += 1;
                    break job;
                }
                if queue.status.stopped {
                    return;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            (shared.execute)(&job.request, &job.control)
        }))
        .unwrap_or_else(|_| failed_response(&job.request, "editor search worker panicked"));
        // Finishing means no filesystem work or snapshot allocation remains
        // owned by the permit; the bounded response alone goes to the host.
        let id = job.id;
        drop(job);
        shared.finish(id, true);
        shared.emit(response);
    }
}

fn request_bytes(request: &Request) -> anyhow::Result<usize> {
    crate::editor::validate_path(&request.root)?;
    ensure!(
        request.root.is_absolute(),
        "search root must be a local absolute path"
    );
    ensure!(
        request.token.generation > 0
            && !request.token.request_id.is_empty()
            && request.token.request_id.len() <= 128
            && request
                .token
                .request_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
        "invalid editor search identity"
    );
    ensure!(
        request.deadline > Instant::now(),
        "editor search deadline already expired"
    );
    let mut bytes = request.root.as_os_str().len().saturating_add(256);
    if let Kind::Workspace {
        query,
        options,
        open_documents,
    } = &request.kind
    {
        ensure!(
            !query.is_empty() && query.len() <= MAX_QUERY_BYTES && !query.contains('\0'),
            "search query is empty, contains NUL or exceeds 4 KiB"
        );
        ensure!(
            options.max_results > 0
                && options.max_results <= 500
                && options.max_file_bytes > 0
                && options.max_file_bytes <= 2 * 1024 * 1024,
            "invalid search result/file limits"
        );
        ensure!(
            options.include.len() <= 32 && options.exclude.len() <= 32,
            "search has too many include/exclude globs"
        );
        for pattern in options.include.iter().chain(&options.exclude) {
            ensure!(
                !pattern.is_empty() && pattern.len() <= MAX_QUERY_BYTES && !pattern.contains('\0'),
                "invalid search glob"
            );
            bytes = bytes.saturating_add(pattern.len());
        }
        ensure!(
            open_documents.len() <= crate::editor::MAX_DOCUMENTS,
            "too many open search buffers"
        );
        let mut snapshot_bytes = 0usize;
        for document in open_documents {
            snapshot_bytes = snapshot_bytes.saturating_add(document.content.len());
            ensure!(
                snapshot_bytes <= MAX_SNAPSHOT_BYTES,
                "open search snapshot exceeds 16 MiB"
            );
            crate::editor::validate_path(&crate::editor::display_path(document.path.clone()))?;
            ensure!(
                document.path.is_absolute(),
                "open search buffer path is not absolute"
            );
            crate::editor::validate_text(&document.content)?;
            bytes = bytes.saturating_add(document.path.as_os_str().len());
        }
        ensure!(
            snapshot_bytes <= MAX_SNAPSHOT_BYTES,
            "open search snapshot exceeds 16 MiB"
        );
        bytes = bytes
            .saturating_add(snapshot_bytes)
            .saturating_add(query.len());
    }
    ensure!(
        bytes <= MAX_ADMITTED_BYTES,
        "editor search request exceeds its byte budget"
    );
    Ok(bytes)
}

fn empty_response(request: &Request) -> Response {
    Response {
        token: request.token.clone(),
        outcome: match &request.kind {
            Kind::QuickOpen => Outcome::QuickOpen {
                paths: Vec::new(),
                truncated: false,
            },
            Kind::Workspace { .. } => Outcome::Workspace {
                result: WorkspaceSearchResult::default(),
            },
        },
        sources: Vec::new(),
        diagnostics: Diagnostics::default(),
        error: None,
    }
}
fn failed_response(request: &Request, error: impl std::fmt::Display) -> Response {
    let mut response = empty_response(request);
    response.error = Some(error.to_string().chars().take(4096).collect());
    response
}
fn cancelled_response(request: &Request, control: &Control) -> Response {
    let mut response = empty_response(request);
    interrupted(control, &mut response);
    response
}
fn interrupted(control: &Control, response: &mut Response) -> bool {
    if !control.interrupted() {
        return false;
    }
    response.diagnostics.cancelled = true;
    response.diagnostics.deadline_exceeded = Instant::now() >= control.deadline;
    if let Outcome::Workspace { result } = &mut response.outcome {
        result.cancelled = true;
    }
    response.error = Some(
        if response.diagnostics.deadline_exceeded {
            "editor search deadline exceeded"
        } else {
            "editor search cancelled"
        }
        .into(),
    );
    true
}
fn truncated(response: &mut Response) {
    match &mut response.outcome {
        Outcome::QuickOpen { truncated, .. } => *truncated = true,
        Outcome::Workspace { result } => result.truncated = true,
    }
}

struct Matcher {
    regex: Regex,
    include: Option<GlobSet>,
    exclude: Option<GlobSet>,
}
impl Matcher {
    fn new(query: &str, options: &SearchOptions) -> anyhow::Result<Self> {
        let pattern = if options.use_regex {
            query.to_owned()
        } else {
            regex::escape(query)
        };
        let pattern = if options.whole_word {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern
        };
        Ok(Self {
            regex: RegexBuilder::new(&pattern)
                .case_insensitive(!options.case_sensitive)
                .size_limit(1024 * 1024)
                .dfa_size_limit(1024 * 1024)
                .nest_limit(64)
                .build()
                .context("invalid or oversized search regex")?,
            include: globs(&options.include)?,
            exclude: globs(&options.exclude)?,
        })
    }
    fn includes(&self, path: &str) -> bool {
        self.include.as_ref().is_none_or(|g| g.is_match(path))
            && !self.exclude.as_ref().is_some_and(|g| g.is_match(path))
    }
}
fn globs(patterns: &[String]) -> anyhow::Result<Option<GlobSet>> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).context("invalid search glob")?);
    }
    Ok(Some(builder.build().context("invalid search glob set")?))
}

fn scan(request: &Request, control: &Control) -> Response {
    let mut response = empty_response(request);
    if interrupted(control, &mut response) {
        return response;
    }
    if let Err(error) = scan_inner(request, control, &mut response) {
        response.error = Some(format!("{error:#}").chars().take(4096).collect());
    }
    interrupted(control, &mut response);
    response
}

fn scan_inner(request: &Request, control: &Control, response: &mut Response) -> anyhow::Result<()> {
    let root = crate::editor::canonical_root(&request.root)?;
    ensure!(
        same_path(&root, &request.root),
        "editor search root identity changed"
    );
    let matcher = match &request.kind {
        Kind::Workspace { query, options, .. } => Some(Matcher::new(query, options)?),
        Kind::QuickOpen => None,
    };
    // Includes worst-case JSON escaping of the bounded Unicode error fields.
    let mut result_bytes = RESPONSE_RESERVE_BYTES;
    let mut overridden: Vec<PathBuf> = Vec::new();
    if let Kind::Workspace {
        options,
        open_documents,
        ..
    } = &request.kind
    {
        for document in open_documents {
            if interrupted(control, response) {
                return Ok(());
            }
            let path = crate::editor::display_path(document.path.clone());
            let relative =
                relative_path(&root, &path).context("open search buffer is outside root")?;
            if overridden.iter().any(|prior| same_path(prior, &path)) {
                anyhow::bail!("duplicate open search buffer identity");
            }
            overridden.push(path);
            // Even excluded/oversized open buffers override their stale disk copy.
            if document.content.len() as u64 > options.max_file_bytes {
                response.diagnostics.skipped_oversized += 1;
                continue;
            }
            if !matcher.as_ref().unwrap().includes(&relative) {
                continue;
            }
            if !collect(
                &relative,
                &document.content,
                true,
                matcher.as_ref().unwrap(),
                options,
                control,
                response,
                &mut result_bytes,
            )? {
                return Ok(());
            }
        }
    }
    let mut walk = OwnedWalk::new(&root, control, response)?;
    while let Some(path) = walk.next(control, response)? {
        let Some(relative) = relative_path(&root, &path) else {
            response.diagnostics.skipped_path += 1;
            continue;
        };
        match &request.kind {
            Kind::QuickOpen => {
                let cost = serde_json::to_vec(&relative)?.len() + 1;
                let Outcome::QuickOpen { paths, .. } = &mut response.outcome else {
                    unreachable!()
                };
                if paths.len() >= MAX_INDEX_PATHS
                    || result_bytes.saturating_add(cost) > MAX_RESULT_BYTES
                {
                    response.diagnostics.result_byte_limit =
                        result_bytes.saturating_add(cost) > MAX_RESULT_BYTES;
                    truncated(response);
                    break;
                }
                result_bytes += cost;
                paths.push(relative);
            }
            Kind::Workspace { options, .. } => {
                if !matcher.as_ref().unwrap().includes(&relative) {
                    continue;
                }
                let (mut file, identity) = match open_confined(&root, &path) {
                    Ok(opened) => opened,
                    Err(error) => {
                        response.diagnostics.skipped_unreadable += 1;
                        response.diagnostics.error(error);
                        continue;
                    }
                };
                if overridden.iter().any(|path| same_path(path, &identity)) {
                    continue;
                }
                let remaining =
                    MAX_SCANNED_BYTES.saturating_sub(response.diagnostics.scanned_bytes);
                if remaining == 0 {
                    response.diagnostics.byte_limit = true;
                    truncated(response);
                    break;
                }
                // Reserve the one-byte oversized probe inside the global budget.
                let limit = usize::try_from(options.max_file_bytes)
                    .unwrap()
                    .min(remaining.saturating_sub(1));
                let bytes = match read_bounded(&mut file, limit, control) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        response.diagnostics.scanned_bytes += error.consumed;
                        response.diagnostics.skipped_unreadable += 1;
                        response.diagnostics.error(error);
                        if response.diagnostics.scanned_bytes >= MAX_SCANNED_BYTES {
                            response.diagnostics.byte_limit = true;
                            truncated(response);
                            break;
                        }
                        continue;
                    }
                };
                response.diagnostics.scanned_bytes += bytes.len();
                if interrupted(control, response) {
                    break;
                }
                if bytes.len() > limit {
                    response.diagnostics.skipped_oversized += 1;
                    if limit < options.max_file_bytes as usize {
                        response.diagnostics.byte_limit = true;
                        truncated(response);
                        break;
                    }
                    continue;
                }
                let text = match decode(&bytes) {
                    Ok(text) => text,
                    Err(TextFailure::Binary) => {
                        response.diagnostics.skipped_binary += 1;
                        continue;
                    }
                    Err(TextFailure::Encoding) => {
                        response.diagnostics.skipped_encoding += 1;
                        continue;
                    }
                };
                if !collect(
                    &relative,
                    &text,
                    false,
                    matcher.as_ref().unwrap(),
                    options,
                    control,
                    response,
                    &mut result_bytes,
                )? {
                    break;
                }
            }
        }
    }
    if let Outcome::QuickOpen { paths, .. } = &mut response.outcome {
        paths.sort_by(|a, b| {
            a.split('/')
                .count()
                .cmp(&b.split('/').count())
                .then_with(|| a.cmp(b))
        });
    }
    Ok(())
}

const MAX_IGNORE_FILE_BYTES: usize = 64 * 1024;
const MAX_IGNORE_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 64;

struct Frame {
    entries: fs::ReadDir,
    rules: Arc<ignore::gitignore::Gitignore>,
}
struct OwnedWalk {
    root: PathBuf,
    frames: Vec<Frame>,
}
impl OwnedWalk {
    fn new(root: &Path, control: &Control, response: &mut Response) -> anyhow::Result<Self> {
        let rules = load_rules(root, root, true, control, response)?;
        Ok(Self {
            root: root.to_path_buf(),
            frames: vec![Frame {
                entries: fs::read_dir(root)?,
                rules,
            }],
        })
    }
    fn next(
        &mut self,
        control: &Control,
        response: &mut Response,
    ) -> anyhow::Result<Option<PathBuf>> {
        loop {
            if interrupted(control, response) {
                return Ok(None);
            }
            if response.diagnostics.visited_entries >= MAX_VISITED_ENTRIES {
                response.diagnostics.entry_limit = true;
                truncated(response);
                return Ok(None);
            }
            let Some(frame) = self.frames.last_mut() else {
                return Ok(None);
            };
            let Some(entry) = frame.entries.next() else {
                self.frames.pop();
                continue;
            };
            response.diagnostics.visited_entries += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    response.diagnostics.skipped_unreadable += 1;
                    response.diagnostics.error(error);
                    continue;
                }
            };
            let path = entry.path();
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) => {
                    response.diagnostics.skipped_unreadable += 1;
                    response.diagnostics.error(error);
                    continue;
                }
            };
            if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                response.diagnostics.skipped_reparse += 1;
                continue;
            }
            let directory = metadata.is_dir();
            if !directory && !metadata.is_file() {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                response.diagnostics.skipped_path += 1;
                continue;
            };
            if directory
                && [".git", "node_modules", "target"]
                    .iter()
                    .any(|generated| name.eq_ignore_ascii_case(generated))
            {
                continue;
            }
            let matched = self
                .frames
                .iter()
                .rev()
                .map(|frame| frame.rules.matched(&path, directory))
                .find(|matched| !matched.is_none());
            if matched.as_ref().is_some_and(|matched| matched.is_ignore())
                || (matched.is_none() && name.starts_with('.'))
            {
                continue;
            }
            match checked_entry(&path, &self.root) {
                Ok(()) => {}
                Err(PathFailure::Reparse) => {
                    response.diagnostics.skipped_reparse += 1;
                    continue;
                }
                Err(PathFailure::Invalid) => {
                    response.diagnostics.skipped_path += 1;
                    continue;
                }
            }
            if !directory {
                return Ok(Some(path));
            }
            if self.frames.len() >= MAX_DEPTH {
                response.diagnostics.depth_limit = true;
                truncated(response);
                continue;
            }
            let rules = load_rules(&self.root, &path, false, control, response)?;
            match fs::read_dir(&path) {
                Ok(entries) => self.frames.push(Frame { entries, rules }),
                Err(error) => {
                    response.diagnostics.skipped_unreadable += 1;
                    response.diagnostics.error(error);
                }
            }
        }
    }
}
fn load_rules(
    root: &Path,
    directory: &Path,
    first: bool,
    control: &Control,
    response: &mut Response,
) -> anyhow::Result<Arc<ignore::gitignore::Gitignore>> {
    let mut builder = ignore::gitignore::GitignoreBuilder::new(directory);
    let mut paths = Vec::new();
    if first {
        paths.push(root.join(".git/info/exclude"));
    }
    paths.push(directory.join(".gitignore"));
    paths.push(directory.join(".ignore"));
    for path in paths {
        if interrupted(control, response) {
            break;
        }
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("cannot inspect search ignore file"),
            Ok(metadata) if metadata.file_type().is_symlink() || is_reparse(&metadata) => {
                anyhow::bail!("search ignore file is a reparse point: {}", path.display());
            }
            Ok(_) => {}
        }
        let (mut file, _) = open_confined(root, &path)?;
        let remaining = MAX_IGNORE_BYTES.saturating_sub(response.diagnostics.ignore_bytes);
        ensure!(remaining > 0, "search ignore rules exceed 1 MiB");
        let limit = MAX_IGNORE_FILE_BYTES.min(remaining.saturating_sub(1));
        let bytes = match read_bounded(&mut file, limit, control) {
            Ok(bytes) => bytes,
            Err(error) => {
                response.diagnostics.ignore_bytes += error.consumed;
                return Err(error).context("cannot read search ignore file");
            }
        };
        response.diagnostics.ignore_bytes += bytes.len();
        ensure!(
            bytes.len() <= limit || limit == MAX_IGNORE_FILE_BYTES,
            "search ignore rules exceed 1 MiB"
        );
        ensure!(
            bytes.len() <= MAX_IGNORE_FILE_BYTES,
            "search ignore file exceeds 64 KiB"
        );
        ensure!(
            response.diagnostics.ignore_bytes <= MAX_IGNORE_BYTES,
            "search ignore rules exceed 1 MiB"
        );
        let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&bytes);
        let text = std::str::from_utf8(bytes).context("search ignore file is not UTF-8")?;
        for line in text.lines() {
            ensure!(
                line.len() <= MAX_QUERY_BYTES,
                "search ignore rule exceeds 4 KiB"
            );
            builder
                .add_line(Some(path.clone()), line)
                .context("invalid search ignore rule")?;
        }
    }
    Ok(Arc::new(
        builder.build().context("invalid search ignore rules")?,
    ))
}

enum PathFailure {
    Reparse,
    Invalid,
}
fn checked_entry(path: &Path, root: &Path) -> Result<(), PathFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|_| PathFailure::Invalid)?;
    if metadata.file_type().is_symlink() || is_reparse(&metadata) {
        return Err(PathFailure::Reparse);
    }
    let canonical =
        crate::editor::display_path(fs::canonicalize(path).map_err(|_| PathFailure::Invalid)?);
    crate::editor::validate_path(&canonical).map_err(|_| PathFailure::Invalid)?;
    if !paths_equal(root, &canonical) && relative_path(root, &canonical).is_none() {
        return Err(PathFailure::Invalid);
    }
    Ok(())
}
fn is_reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

/// Pure canonical identity comparison, including std's Windows verbatim spelling.
pub fn same_path(left: &Path, right: &Path) -> bool {
    let left = crate::editor::display_path(left.to_path_buf());
    let right = crate::editor::display_path(right.to_path_buf());
    let left: Vec<_> = left.components().collect();
    let right: Vec<_> = right.components().collect();
    left.len() == right.len()
        && left.iter().zip(&right).all(|(left, right)| {
            paths_equal(Path::new(left.as_os_str()), Path::new(right.as_os_str()))
        })
}
fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
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
    #[cfg(not(windows))]
    {
        left == right
    }
}
fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let root: Vec<_> = root.components().collect();
    let parts: Vec<_> = path.components().collect();
    if parts.len() <= root.len()
        || !root
            .iter()
            .zip(&parts)
            .all(|(a, b)| paths_equal(Path::new(a.as_os_str()), Path::new(b.as_os_str())))
    {
        return None;
    }
    let relative: PathBuf = parts[root.len()..].iter().collect();
    crate::editor::validate_relative_path(&relative).ok()?;
    let relative = relative.to_str()?.replace('\\', "/");
    (relative.len() <= MAX_PATH_BYTES).then_some(relative)
}

fn open_confined(root: &Path, path: &Path) -> anyhow::Result<(File, PathBuf)> {
    crate::editor::validate_path(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            GetFinalPathNameByHandleW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_SEQUENTIAL_SCAN,
        };
        // Retained search paths use '/' separators. Verbatim Win32 paths do not
        // normalize those, so normalize separators before adding the prefix.
        let extended = PathBuf::from(format!(
            "\\\\?\\{}",
            path.to_str()
                .context("search path is not Unicode")?
                .replace('/', "\\")
        ));
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_SEQUENTIAL_SCAN)
            .open(extended)?;
        let metadata = file.metadata()?;
        ensure!(
            metadata.is_file() && !is_reparse(&metadata),
            "search candidate is not a regular non-reparse file"
        );
        let mut wide = vec![0u16; crate::editor::MAX_PATH_UNITS + 1];
        let count = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                wide.as_mut_ptr(),
                wide.len() as u32,
                0,
            )
        };
        ensure!(
            count > 0 && (count as usize) < wide.len(),
            "cannot resolve opened search file: {}",
            io::Error::last_os_error()
        );
        wide.truncate(count as usize);
        let identity =
            crate::editor::display_path(PathBuf::from(std::ffi::OsString::from_wide(&wide)));
        crate::editor::validate_path(&identity)?;
        ensure!(
            relative_path(root, &identity).is_some(),
            "opened search file is outside root"
        );
        Ok((file, identity))
    }
    #[cfg(not(windows))]
    {
        checked_entry(path, root).map_err(|_| anyhow::anyhow!("invalid/reparse search file"))?;
        let file = File::open(path)?;
        ensure!(
            file.metadata()?.is_file(),
            "search candidate is not a regular file"
        );
        let identity = fs::canonicalize(path)?;
        ensure!(
            relative_path(root, &identity).is_some(),
            "opened search file is outside root"
        );
        Ok((file, identity))
    }
}

#[derive(Debug)]
struct ReadFailure {
    error: io::Error,
    consumed: usize,
}
impl std::fmt::Display for ReadFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} (after reading {} bytes)",
            self.error, self.consumed
        )
    }
}
impl std::error::Error for ReadFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}
fn read_bounded(
    file: &mut impl Read,
    limit: usize,
    control: &Control,
) -> Result<Vec<u8>, ReadFailure> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 32 * 1024];
    while bytes.len() <= limit && !control.interrupted() {
        let needed = chunk.len().min(limit.saturating_add(1) - bytes.len());
        let count = file
            .read(&mut chunk[..needed])
            .map_err(|error| ReadFailure {
                error,
                consumed: bytes.len(),
            })?;
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(bytes)
}
enum TextFailure {
    Binary,
    Encoding,
}
fn decode(bytes: &[u8]) -> Result<String, TextFailure> {
    if bytes.contains(&0) {
        return Err(TextFailure::Binary);
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let text = std::str::from_utf8(bytes).map_err(|_| TextFailure::Encoding)?;
    // Mixed LF/CRLF is not accepted by DocumentService. Bare CR is not a safe
    // Monaco range basis either; report it unsupported instead of false ranges.
    let mut lf = false;
    let mut crlf = false;
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            if index > 0 && bytes[index - 1] == b'\r' {
                crlf = true;
            } else {
                lf = true;
            }
        } else if *byte == b'\r' && bytes.get(index + 1) != Some(&b'\n') {
            return Err(TextFailure::Encoding);
        }
    }
    if lf && crlf {
        return Err(TextFailure::Encoding);
    }
    Ok(if crlf {
        text.replace("\r\n", "\n")
    } else {
        text.to_owned()
    })
}

pub fn content_sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// Revalidate a retained closed-file result before ordinary document Open. The
/// caller must also check the final opened payload, since its later read can race.
pub fn validate_result_source(
    root: &Path,
    relative: &Path,
    expected_sha256: &str,
) -> anyhow::Result<()> {
    crate::editor::validate_relative_path(relative)?;
    ensure!(
        expected_sha256.len() == 64 && expected_sha256.bytes().all(|c| c.is_ascii_hexdigit()),
        "invalid retained search content identity"
    );
    let control = Control {
        token: Token {
            surface: Uuid::nil(),
            instance: Uuid::nil(),
            request_id: "validate".into(),
            generation: 1,
        },
        cancellation: SearchCancellation::default(),
        deadline: Instant::now() + SEARCH_BUDGET,
    };
    let (mut file, _) = open_confined(root, &root.join(relative))?;
    let bytes = read_bounded(&mut file, 2 * 1024 * 1024, &control)?;
    ensure!(!control.interrupted(), "search result validation timed out");
    ensure!(
        bytes.len() <= 2 * 1024 * 1024,
        "search result file now exceeds 2 MiB"
    );
    let text = decode(&bytes)
        .map_err(|_| anyhow::anyhow!("search result file is no longer supported UTF-8 text"))?;
    ensure!(
        content_sha256(&text) == expected_sha256,
        "search result content changed; search again"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect(
    path: &str,
    content: &str,
    open: bool,
    matcher: &Matcher,
    options: &SearchOptions,
    control: &Control,
    response: &mut Response,
    result_bytes: &mut usize,
) -> anyhow::Result<bool> {
    response.diagnostics.searched_files += 1;
    let mut source = None;
    for (line_index, line) in content.split('\n').enumerate() {
        if interrupted(control, response) {
            return Ok(false);
        }
        let mut previous = 0;
        let mut column = 0;
        for found in matcher.regex.find_iter(line) {
            if interrupted(control, response) {
                return Ok(false);
            }
            let Outcome::Workspace { result } = &mut response.outcome else {
                unreachable!()
            };
            if result.matches.len() >= options.max_results {
                result.truncated = true;
                return Ok(false);
            }
            column += utf16_len(&line[previous..found.start()]);
            previous = found.start();
            let (preview, preview_column, preview_length) =
                preview(line, found.start(), found.end());
            let found = WorkspaceSearchMatch {
                path: path.into(),
                line: line_index as u32,
                column,
                length: utf16_len(found.as_str()),
                preview,
                preview_column,
                preview_length,
            };
            let mut cost = serde_json::to_vec(&found)?.len() + 1;
            let stamp = if source.is_none() {
                let stamp = SourceStamp {
                    path: path.into(),
                    sha256: content_sha256(content),
                    from_open_buffer: open,
                };
                cost += serde_json::to_vec(&stamp)?.len() + 1;
                Some(stamp)
            } else {
                None
            };
            if result_bytes.saturating_add(cost) > MAX_RESULT_BYTES {
                response.diagnostics.result_byte_limit = true;
                result.truncated = true;
                return Ok(false);
            }
            *result_bytes += cost;
            if let Some(stamp) = stamp {
                response.sources.push(stamp);
                source = Some(());
            }
            result.matches.push(found);
        }
    }
    Ok(true)
}
fn utf16_len(value: &str) -> u32 {
    value.encode_utf16().count() as u32
}
fn preview(line: &str, start: usize, end: usize) -> (String, u32, u32) {
    let before = line[..start]
        .char_indices()
        .rev()
        .nth(79)
        .map_or(0, |(i, _)| i);
    let matched_end = line[start..end]
        .char_indices()
        .nth(120)
        .map_or(end, |(i, _)| start + i);
    let after = line[matched_end..]
        .char_indices()
        .nth(80)
        .map_or(line.len(), |(i, _)| matched_end + i);
    let mut preview = String::new();
    if before > 0 {
        preview.push('…');
    }
    preview.push_str(&line[before..after]);
    if after < line.len() {
        preview.push('…');
    }
    (
        preview,
        u32::from(before > 0) + utf16_len(&line[before..start]),
        utf16_len(&line[start..matched_end]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("flowmux-search-{}-한글", Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(crate::editor::canonical_root(&path).unwrap())
        }
        fn write(&self, path: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
            let path = self.0.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            path
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn request(root: &Directory, kind: Kind) -> Request {
        Request {
            token: Token {
                surface: Uuid::new_v4(),
                instance: Uuid::new_v4(),
                request_id: "search-1".into(),
                generation: 1,
            },
            root: root.0.clone(),
            kind,
            deadline: Instant::now() + SEARCH_BUDGET,
        }
    }
    fn workspace(root: &Directory, query: &str, documents: Vec<SearchDocument>) -> Request {
        request(
            root,
            Kind::Workspace {
                query: query.into(),
                options: SearchOptions::default(),
                open_documents: documents,
            },
        )
    }
    fn run(request: &Request) -> Response {
        request_bytes(request).unwrap();
        let control = Control {
            token: request.token.clone(),
            cancellation: SearchCancellation::default(),
            deadline: request.deadline,
        };
        scan(request, &control)
    }
    fn matches(response: &Response) -> &[WorkspaceSearchMatch] {
        let Outcome::Workspace { result } = &response.outcome else {
            panic!("workspace result required")
        };
        &result.matches
    }

    #[test]
    fn bom_crlf_and_exact_unicode_use_monaco_utf16_ranges_and_stable_digest() {
        let root = Directory::new();
        let text = "😀 한글 한 e\u{301}\n한글\n";
        root.write(
            "NFD 한 😀.txt",
            format!("\u{feff}{}", text.replace('\n', "\r\n")),
        );
        let response = run(&workspace(&root, "한글", Vec::new()));
        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(matches(&response).len(), 2);
        assert_eq!(
            (
                matches(&response)[0].line,
                matches(&response)[0].column,
                matches(&response)[0].length
            ),
            (0, 3, 2)
        );
        assert_eq!(
            (matches(&response)[1].line, matches(&response)[1].column),
            (1, 0)
        );
        assert_eq!(response.sources[0].sha256, content_sha256(text));
        assert!(!response.sources[0].from_open_buffer);
        validate_result_source(
            &root.0,
            Path::new(&response.sources[0].path),
            &response.sources[0].sha256,
        )
        .unwrap();
        let nfd = run(&workspace(&root, "한", Vec::new()));
        assert_eq!(matches(&nfd).len(), 1);
        assert_eq!((matches(&nfd)[0].column, matches(&nfd)[0].length), (6, 3));
        root.write("NFD 한 😀.txt", "changed 한글\n");
        assert!(validate_result_source(
            &root.0,
            Path::new(&response.sources[0].path),
            &response.sources[0].sha256
        )
        .is_err());
    }

    #[test]
    fn open_buffers_override_disk_even_when_deleted_or_too_large_to_search() {
        let root = Directory::new();
        let path = root.write("buffer.txt", "disk needle\n");
        let documents = vec![SearchDocument {
            path: path.clone(),
            content: "buffer value\n".into(),
        }];
        assert!(matches(&run(&workspace(&root, "needle", documents.clone()))).is_empty());
        fs::remove_file(&path).unwrap();
        let response = run(&workspace(&root, "value", documents));
        assert_eq!(matches(&response).len(), 1);
        assert!(response.sources[0].from_open_buffer);
        root.write("buffer.txt", "disk needle\n");
        let oversized = vec![SearchDocument {
            path,
            content: "x".repeat(2 * 1024 * 1024 + 1),
        }];
        let response = run(&workspace(&root, "needle", oversized));
        assert!(matches(&response).is_empty());
        assert_eq!(response.diagnostics.skipped_oversized, 1);
    }

    #[test]
    fn ignores_generated_hidden_and_nested_rules_without_losing_unicode_paths() {
        let root = Directory::new();
        root.write(".gitignore", "*.ignored\n!keep.ignored\n");
        root.write(".hidden.txt", "needle");
        root.write("a.ignored", "needle");
        root.write("keep.ignored", "needle");
        root.write("node_modules/ignored.txt", "needle");
        root.write("target/ignored.txt", "needle");
        root.write("nested/.ignore", "skip.txt\n");
        root.write("nested/skip.txt", "needle");
        root.write("nested/한글 한 😀.txt", "needle");
        let response = run(&request(&root, Kind::QuickOpen));
        assert!(response.error.is_none(), "{:?}", response.error);
        let Outcome::QuickOpen { paths, truncated } = response.outcome else {
            unreachable!()
        };
        assert!(!truncated);
        assert_eq!(paths, ["keep.ignored", "nested/한글 한 😀.txt"]);
    }

    #[test]
    fn regex_word_case_globs_invalid_patterns_and_unreadable_text_are_explicit() {
        let root = Directory::new();
        root.write("include.txt", "Needle needles needle\n");
        root.write("exclude.txt", "needle\n");
        root.write("binary.bin", b"needle\0");
        root.write("invalid.txt", b"needle\xff");
        root.write("mixed.txt", b"needle\r\nneedle\n");
        let mut requested = workspace(&root, "needle", Vec::new());
        let Kind::Workspace { options, .. } = &mut requested.kind else {
            unreachable!()
        };
        options.whole_word = true;
        options.case_sensitive = true;
        options.include = vec!["*.txt".into()];
        options.exclude = vec!["exclude.txt".into()];
        let response = run(&requested);
        assert_eq!(matches(&response).len(), 1);
        assert_eq!(matches(&response)[0].column, 15);
        assert_eq!(response.diagnostics.skipped_encoding, 2);
        let Kind::Workspace { query, options, .. } = &mut requested.kind else {
            unreachable!()
        };
        options.use_regex = true;
        *query = "[".into();
        assert!(run(&requested).error.unwrap().contains("regex"));
        let Kind::Workspace { query, options, .. } = &mut requested.kind else {
            unreachable!()
        };
        *query = "needle".into();
        options.include = vec!["[".into()];
        assert!(run(&requested).error.unwrap().contains("glob"));
    }

    #[test]
    fn result_count_and_serialized_bytes_are_bounded_without_invalid_unicode() {
        let root = Directory::new();
        let path = root.write("many.txt", "needle\n".repeat(501));
        let response = run(&workspace(&root, "needle", Vec::new()));
        let Outcome::Workspace { result } = &response.outcome else {
            unreachable!()
        };
        assert!(result.truncated);
        assert_eq!(result.matches.len(), 500);
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_RESULT_BYTES);
        let request = workspace(
            &root,
            "needle",
            vec![SearchDocument {
                path,
                content: "needle\n".repeat(501),
            }],
        );
        let control = Control {
            token: request.token.clone(),
            cancellation: SearchCancellation::default(),
            deadline: request.deadline,
        };
        let options = SearchOptions::default();
        let matcher = Matcher::new("needle", &options).unwrap();
        let mut response = empty_response(&request);
        let mut bytes = RESPONSE_RESERVE_BYTES;
        let very_long_path = format!("{}.txt", "한".repeat(4_000));
        assert!(!collect(
            &very_long_path,
            &"needle\n".repeat(500),
            true,
            &matcher,
            &options,
            &control,
            &mut response,
            &mut bytes
        )
        .unwrap());
        assert!(response.diagnostics.result_byte_limit);
        assert!(serde_json::to_vec(&response).unwrap().len() <= MAX_RESULT_BYTES);
    }

    #[test]
    fn bounded_reader_never_trusts_metadata_and_cancel_or_deadline_stops_before_read() {
        let token = Token {
            surface: Uuid::nil(),
            instance: Uuid::nil(),
            request_id: "read".into(),
            generation: 1,
        };
        let control = Control {
            token,
            cancellation: SearchCancellation::default(),
            deadline: Instant::now() + SEARCH_BUDGET,
        };
        let mut growing = io::Cursor::new(vec![b'x'; 1_000_000]);
        assert_eq!(
            read_bounded(&mut growing, 128, &control).unwrap().len(),
            129
        );
        assert_eq!(growing.position(), 129);
        control.cancellation.cancel();
        let before = growing.position();
        assert!(read_bounded(&mut growing, 128, &control)
            .unwrap()
            .is_empty());
        assert_eq!(growing.position(), before);
        let expired = Control {
            deadline: Instant::now(),
            ..control
        };
        assert!(read_bounded(&mut growing, 128, &expired)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn partial_read_errors_retain_consumed_bytes_for_the_aggregate_budget() {
        struct FailingRead {
            sent: bool,
        }
        impl Read for FailingRead {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if self.sent {
                    return Err(io::Error::other("owned partial-read failure"));
                }
                self.sent = true;
                let count = output.len().min(12_345);
                output[..count].fill(b'x');
                Ok(count)
            }
        }
        let control = Control {
            token: Token {
                surface: Uuid::nil(),
                instance: Uuid::nil(),
                request_id: "partial-read".into(),
                generation: 1,
            },
            cancellation: SearchCancellation::default(),
            deadline: Instant::now() + SEARCH_BUDGET,
        };
        let mut consumed = 0;
        for _ in 0..3 {
            let error =
                read_bounded(&mut FailingRead { sent: false }, 64 * 1024, &control).unwrap_err();
            assert_eq!(error.consumed, 12_345);
            assert!(error.to_string().contains("owned partial-read failure"));
            consumed += error.consumed;
        }
        assert_eq!(consumed, 37_035);
        let remaining = 7;
        let bytes =
            read_bounded(&mut io::Cursor::new([b'x'; 20]), remaining - 1, &control).unwrap();
        assert_eq!(
            bytes.len(),
            remaining,
            "the oversized probe uses the aggregate budget too"
        );
    }

    #[test]
    fn path_boundaries_and_snapshot_cap_are_rejected_before_search() {
        let root = Directory::new();
        let outside = Directory::new();
        let external = outside.write("outside.txt", "secret needle");
        assert!(open_confined(&root.0, &external).is_err());
        assert!(relative_path(&root.0, &root.0.join("../outside.txt")).is_none());
        let requested = workspace(
            &root,
            "needle",
            vec![SearchDocument {
                path: external,
                content: "needle".into(),
            }],
        );
        assert!(run(&requested).error.unwrap().contains("outside root"));
        let too_large = workspace(
            &root,
            "needle",
            vec![SearchDocument {
                path: root.0.join("buffer.txt"),
                content: "x".repeat(MAX_SNAPSHOT_BYTES + 1),
            }],
        );
        assert!(request_bytes(&too_large)
            .unwrap_err()
            .to_string()
            .contains("snapshot"));
        assert!(validate_result_source(
            &root.0,
            Path::new("../outside.txt"),
            &content_sha256("secret needle")
        )
        .is_err());
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
    fn gated_service() -> (Service, Gate, mpsc::Receiver<()>, mpsc::Receiver<Response>) {
        let gate = Gate::new();
        let worker_gate = gate.0.clone();
        let (started, starts) = mpsc::channel();
        let (completed, completions) = mpsc::channel();
        let service = Service::start_with(
            move |response| {
                let _ = completed.send(response);
            },
            move |request, control| {
                started.send(()).unwrap();
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut opened = worker_gate.0.lock().unwrap();
                while !*opened {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    assert!(!remaining.is_zero(), "test gate was not released");
                    opened = worker_gate.1.wait_timeout(opened, remaining).unwrap().0;
                }
                cancelled_response(request, control)
            },
        )
        .unwrap();
        (service, gate, starts, completions)
    }

    #[test]
    fn fixed_workers_keep_executing_permits_after_cancel_and_stop_drains_only_queue() {
        let root = Directory::new();
        let (service, gate, starts, completions) = gated_service();
        let first = service.submit(request(&root, Kind::QuickOpen)).unwrap();
        service.submit(request(&root, Kind::QuickOpen)).unwrap();
        for _ in 0..THREADS {
            starts.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        for _ in THREADS..MAX_ADMITTED {
            service.submit(request(&root, Kind::QuickOpen)).unwrap();
        }
        assert_eq!(service.status().running, THREADS);
        assert_eq!(service.status().admitted, MAX_ADMITTED);
        assert!(service.submit(request(&root, Kind::QuickOpen)).is_err());
        first.cancel();
        assert_eq!(
            service.status().admitted,
            MAX_ADMITTED,
            "cancel is not completion"
        );
        service.stop();
        assert_eq!(service.status().admitted, THREADS);
        assert_eq!(service.status().queued, 0);
        gate.open();
        for _ in 0..MAX_ADMITTED {
            completions.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert_eq!(service.status().admitted, 0);
        assert_eq!(service.status().running, 0);
    }

    #[test]
    fn newer_generation_removes_queued_old_work_and_stale_cancel_cannot_target_new_owner() {
        let root = Directory::new();
        let (service, gate, starts, completions) = gated_service();
        for _ in 0..THREADS {
            service.submit(request(&root, Kind::QuickOpen)).unwrap();
        }
        for _ in 0..THREADS {
            starts.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        let old = request(&root, Kind::QuickOpen);
        let token = old.token.clone();
        let old_ticket = service.submit(old).unwrap();
        let mut new = request(&root, Kind::QuickOpen);
        new.token = Token {
            generation: token.generation + 1,
            request_id: "search-2".into(),
            ..token.clone()
        };
        let new_ticket = service.submit(new).unwrap();
        assert!(old_ticket.is_cancelled());
        assert_eq!(
            completions
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .token,
            token
        );
        assert_eq!(service.status().admitted, THREADS + 1);
        old_ticket.cancel();
        assert!(!new_ticket.is_cancelled());
        let mut stale = request(&root, Kind::QuickOpen);
        stale.token = token;
        assert!(service.submit(stale).is_err());
        service.stop();
        gate.open();
        for _ in 0..THREADS + 1 {
            completions.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert_eq!(service.status().admitted_bytes, 0);
    }

    #[test]
    fn real_service_completes_valid_search_after_invalid_regex_without_poisoning_worker() {
        let root = Directory::new();
        root.write("text.txt", "needle");
        let (send, receive) = mpsc::channel();
        let service = Service::start(move |response| {
            let _ = send.send(response);
        })
        .unwrap();
        let mut invalid = workspace(&root, "[", Vec::new());
        let Kind::Workspace { options, .. } = &mut invalid.kind else {
            unreachable!()
        };
        options.use_regex = true;
        service.submit(invalid).unwrap();
        assert!(receive
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .error
            .is_some());
        service
            .submit(workspace(&root, "needle", Vec::new()))
            .unwrap();
        let completed = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(completed.error.is_none());
        assert_eq!(matches(&completed).len(), 1);
        assert_eq!(service.status().admitted, 0);
    }

    #[cfg(unix)]
    #[test]
    fn scanner_does_not_follow_an_owned_symlink_outside_root() {
        let root = Directory::new();
        let outside = Directory::new();
        outside.write("secret.txt", "needle");
        std::os::unix::fs::symlink(&outside.0, root.0.join("linked")).unwrap();
        let response = run(&workspace(&root, "needle", Vec::new()));
        assert!(matches(&response).is_empty());
        assert_eq!(response.diagnostics.skipped_reparse, 1);
    }

    #[cfg(windows)]
    #[test]
    fn native_same_handle_unicode_long_paths_and_ordinal_identity() {
        let root = Directory::new();
        let mut nested = root.0.clone();
        use std::os::windows::ffi::OsStrExt;
        while nested.as_os_str().encode_wide().count() < 300 {
            nested.push("nested-한글-한-😀");
        }
        fs::create_dir_all(&nested).unwrap();
        let path = nested.join("file é.txt");
        fs::write(&path, "\u{feff}😀 needle\r\n").unwrap();
        let (mut file, identity) = open_confined(&root.0, &path).unwrap();
        assert!(same_path(
            &identity,
            &PathBuf::from(format!("\\\\?\\{}", path.display()))
        ));
        assert!(same_path(
            &identity,
            &PathBuf::from(path.to_str().unwrap().to_uppercase())
        ));
        assert!(same_path(
            &identity,
            &PathBuf::from(path.to_str().unwrap().replace('\\', "/"))
        ));
        let mut contents = String::new();
        file.read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "\u{feff}😀 needle\r\n");
        let relative = relative_path(&root.0, &path).unwrap();
        assert!(relative.contains('/'));
        let absolute_forward = path.to_str().unwrap().replace('\\', "/");
        let absolute_backward = absolute_forward.replace('/', "\\");
        let relative_backward = relative.replace('/', "\\");
        // Cover fully forward/backward absolute spellings and the mixed spelling
        // produced by joining a canonical root to a retained relative result.
        for candidate in [
            PathBuf::from(&absolute_forward),
            PathBuf::from(&absolute_backward),
            root.0.join(&relative),
            root.0.join(&relative_backward),
        ] {
            let (mut opened, opened_identity) = open_confined(&root.0, &candidate).unwrap();
            assert!(same_path(&opened_identity, &path));
            let mut actual = String::new();
            opened.read_to_string(&mut actual).unwrap();
            assert_eq!(actual, contents);
        }
        for candidate in [&relative, &relative_backward] {
            validate_result_source(
                &root.0,
                Path::new(candidate),
                &content_sha256("😀 needle\n"),
            )
            .unwrap();
        }
    }
}
