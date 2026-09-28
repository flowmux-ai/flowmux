// SPDX-License-Identifier: GPL-3.0-or-later
//! Ordered editor I/O. No WebView, COM, clipboard or UI object enters this worker.
//!
//! Drop rejects new work and detaches without joining the UI thread. The current
//! filesystem operation finishes; accepted queued operations receive cancellation
//! responses instead of executing. The host must await its flush/save response
//! before an intentional close. An OS file operation cannot be forcibly cancelled.
use crate::editor;
use anyhow::{ensure, Context};
use flowmux_core::EditorSessionState;
use flowmux_editor::{
    DocumentDiskStatus, DocumentPayload, EditorMessage, EditorSession, EditorViewState,
    HostMessage, RecoveryOperation, RecoveryStore, SearchDocument, TextDocumentEncoding,
    TextDocumentLineEnding, WorkspaceSearchResult, EDITOR_ZOOM_DEFAULT,
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
};

pub const MAX_PENDING: usize = 8;
pub const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_SEARCH_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum Work {
    Initialize,
    Open(PathBuf),
    Message(EditorMessage),
    SaveAll,
    DiscardAll,
    PollDisk,
    Snapshot,
    SearchSnapshot,
    SearchOpen(SearchOpen),
}

/// All open paths override disk search, even if the scanner skips a large buffer.
#[derive(Debug)]
pub struct SearchBuffer {
    pub document_id: String,
    pub version: u64,
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug)]
pub struct SearchSnapshot {
    pub documents: Vec<SearchBuffer>,
    pub total_bytes: usize,
}

/// The UI owner resolves a retained result token before submitting this work.
#[derive(Debug)]
pub struct SearchOpen {
    pub path: PathBuf,
    pub range: Option<SearchRange>,
    pub expected_document: Option<SearchDocumentVersion>,
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct SearchRange {
    pub line: u32,
    pub column: u32,
    pub length: u32,
}

#[derive(Debug)]
pub struct SearchDocumentVersion {
    pub document_id: String,
    pub version: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocumentMeta {
    pub id: String,
    pub path: PathBuf,
    pub dirty: bool,
    pub encoding: TextDocumentEncoding,
    pub eol: TextDocumentLineEnding,
    pub version: u64,
    pub active: bool,
    pub read_only: bool,
    pub external_change: bool,
    /// False after an incomplete disk scan until an authoritative status,
    /// clean replacement or save result resolves this document's uncertainty.
    pub disk_status_known: bool,
}

#[derive(Debug)]
pub struct Response {
    pub id: u64,
    pub messages: Vec<HostMessage>,
    pub state: EditorSessionState,
    pub dirty: Vec<PathBuf>,
    pub documents: Vec<DocumentMeta>,
    pub ready: bool,
    /// No document session was initialized; its original saved state is retained.
    /// Ordinary operation/recovery-write failures never set this flag.
    pub initialization_failed: bool,
    pub restored_errors: Vec<String>,
    pub error: Option<String>,
    /// Present only for a successful SearchSnapshot capture. A later recovery
    /// persistence error can still accompany the captured, acknowledged text.
    pub search_snapshot: Option<SearchSnapshot>,
}

#[derive(Default)]
struct Shared {
    stopped: AtomicBool,
    queued_bytes: AtomicUsize,
}

struct Job {
    id: u64,
    work: Option<Work>,
    bytes: usize,
    shared: Arc<Shared>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.shared
            .queued_bytes
            .fetch_sub(self.bytes, Ordering::SeqCst);
    }
}

pub struct Worker {
    sender: Option<mpsc::SyncSender<Job>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

enum RecoverySetup {
    Existing(Option<RecoveryStore>),
    Scoped { state_root: PathBuf, scope: String },
}

impl Worker {
    pub fn start(
        root: PathBuf,
        restored: EditorSessionState,
        recovery: Option<RecoveryStore>,
        emit: impl Fn(Response) + Send + 'static,
    ) -> anyhow::Result<Self> {
        Self::start_with_recovery(root, restored, RecoverySetup::Existing(recovery), emit)
    }

    /// All recovery directory inspection/creation occurs on the worker. Missing
    /// workspace roots and inaccessible recovery stores become response errors,
    /// so restoring one unavailable editor does not abort a mixed native window.
    pub fn start_scoped(
        root: PathBuf,
        restored: EditorSessionState,
        state_root: PathBuf,
        scope: String,
        emit: impl Fn(Response) + Send + 'static,
    ) -> anyhow::Result<Self> {
        Self::start_with_recovery(
            root,
            restored,
            RecoverySetup::Scoped { state_root, scope },
            emit,
        )
    }

    fn start_with_recovery(
        root: PathBuf,
        restored: EditorSessionState,
        recovery: RecoverySetup,
        emit: impl Fn(Response) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(MAX_PENDING);
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        let thread = std::thread::Builder::new()
            .name("flowmux-editor-io".into())
            .spawn(move || {
                let mut engine = Engine::new(root, restored, recovery, &thread_shared);
                for mut job in receiver {
                    let id = job.id;
                    let work = job.work.take().expect("queued editor operation");
                    drop(job); // Release queue bytes before executing its owned work.
                    let response = if thread_shared.stopped.load(Ordering::SeqCst) {
                        engine.response(
                            id,
                            Vec::new(),
                            Some("editor worker closed; queued operation was cancelled".into()),
                        )
                    } else {
                        engine.run(id, work)
                    };
                    emit(response);
                }
            })
            .context("cannot start editor I/O worker")?;
        Ok(Self {
            sender: Some(sender),
            shared,
            thread: Some(thread),
        })
    }

    pub fn submit(&self, id: u64, work: Work) -> anyhow::Result<()> {
        ensure!(
            !self.shared.stopped.load(Ordering::SeqCst),
            "editor worker is closed"
        );
        let bytes = work_size(&work)?;
        self.shared
            .queued_bytes
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                used.checked_add(bytes)
                    .filter(|next| *next <= MAX_QUEUED_BYTES)
            })
            .map_err(|_| anyhow::anyhow!("editor worker is busy (queued content limit)"))?;
        let job = Job {
            id,
            work: Some(work),
            bytes,
            shared: self.shared.clone(),
        };
        self.sender
            .as_ref()
            .context("editor worker is closed")?
            .try_send(job)
            .map_err(|_| anyhow::anyhow!("editor worker is busy or stopped (queue limit eight)"))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::SeqCst);
        self.sender.take();
        // Dropping JoinHandle detaches; never wait for a slow disk on the UI thread.
        self.thread.take();
    }
}

fn work_size(work: &Work) -> anyhow::Result<usize> {
    let size = match work {
        Work::Open(path) => {
            editor::validate_path(path)?;
            path.as_os_str().len()
        }
        Work::SearchOpen(request) => {
            editor::validate_relative_path(&request.path)?;
            if let Some(expected) = &request.expected_document {
                ensure!(
                    expected.document_id.len() <= 128,
                    "invalid search document identity"
                );
            }
            if let Some(hash) = &request.expected_sha256 {
                ensure!(
                    hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "invalid search source fingerprint"
                );
            }
            request.path.as_os_str().len().saturating_add(256)
        }
        Work::Message(message) => match message {
            EditorMessage::DocumentChanged {
                document_id,
                content,
                ..
            }
            | EditorMessage::SaveRequested {
                document_id,
                content,
                ..
            } => {
                editor::validate_text(content)?;
                document_id.len().saturating_add(content.len())
            }
            EditorMessage::SaveAsRequested {
                document_id,
                content,
                path,
                ..
            } => {
                editor::validate_text(content)?;
                editor::validate_relative_path(std::path::Path::new(path))?;
                document_id
                    .len()
                    .saturating_add(content.len())
                    .saturating_add(path.len())
            }
            _ => serde_json::to_vec(message)?.len(),
        },
        _ => 0,
    };
    size.checked_add(256)
        .context("editor operation size overflow")
}

struct Engine {
    root: PathBuf,
    session: Option<EditorSession>,
    recovery: Option<RecoveryStore>,
    recovery_pending: BTreeMap<PathBuf, RecoveryOperation>,
    documents: BTreeMap<String, DocumentMeta>,
    document_order: Vec<String>,
    content_bytes: BTreeMap<String, usize>,
    search_snapshot: Option<SearchSnapshot>,
    disk_status_unknown: BTreeSet<String>,
    zoom: u16,
    restored_errors: Vec<String>,
    startup_error: Option<String>,
    operation_error: Option<String>,
    restored_state: EditorSessionState,
}

struct ContentUpdate {
    document_id: String,
    change_sequence: u64,
    bytes: usize,
    was_current: bool,
}

impl Engine {
    fn new(
        root: PathBuf,
        restored: EditorSessionState,
        recovery: RecoverySetup,
        shared: &Shared,
    ) -> Self {
        let mut engine = Self {
            root,
            session: None,
            recovery: None,
            recovery_pending: BTreeMap::new(),
            documents: BTreeMap::new(),
            document_order: Vec::new(),
            content_bytes: BTreeMap::new(),
            search_snapshot: None,
            disk_status_unknown: BTreeSet::new(),
            zoom: restored.zoom_percent.unwrap_or(EDITOR_ZOOM_DEFAULT),
            restored_errors: Vec::new(),
            startup_error: None,
            operation_error: None,
            restored_state: restored.clone(),
        };
        if let Err(error) = engine.restore(restored, recovery, shared) {
            let error = format!(
                "Cannot initialize editor at {}: {error:#}",
                engine.root.display()
            );
            engine.restored_errors.push(error.clone());
            engine.startup_error = Some(error);
            engine.session = None;
        }
        engine
    }

    fn restore(
        &mut self,
        restored: EditorSessionState,
        recovery: RecoverySetup,
        shared: &Shared,
    ) -> anyhow::Result<()> {
        editor::validate_session(&restored)?;
        self.root = editor::canonical_root(&self.root)?;
        self.recovery = match recovery {
            RecoverySetup::Existing(store) => store,
            RecoverySetup::Scoped { state_root, scope } => {
                editor::validate_path(&state_root)?;
                ensure!(
                    !scope.is_empty() && scope.len() <= 128,
                    "invalid editor recovery scope"
                );
                Some(
                    RecoveryStore::new_scoped(state_root, &self.root, &scope)
                        .context("cannot initialize scoped editor recovery")?,
                )
            }
        };
        let session = match &self.recovery {
            Some(store) => EditorSession::with_recovery_store(&self.root, store.clone())?,
            None => EditorSession::new(&self.root)?,
        };
        self.session = Some(session);
        let active = restored.active_file;
        for file in restored.open_files {
            ensure!(
                !shared.stopped.load(Ordering::SeqCst),
                "editor worker closed during restoration"
            );
            let restored = (|| -> anyhow::Result<Vec<HostMessage>> {
                let path = editor::resolve_existing(&self.root, &file.path)?;
                self.check_recovery(&path)?;
                Ok(self.session.as_mut().unwrap().restore_document(
                    path,
                    EditorViewState {
                        cursor_line: file.cursor_line,
                        cursor_column: file.cursor_column,
                        scroll_top: file.scroll_top,
                    },
                )?)
            })();
            match restored {
                Ok(messages) => self.update_metadata(&messages),
                Err(error) => self
                    .restored_errors
                    .push(format!("{}: {error:#}", file.path.display())),
            }
        }
        if let Some(active) = active {
            if let Ok(path) = editor::resolve_existing(&self.root, &active) {
                self.session.as_mut().unwrap().activate_path(path);
            }
        }
        Ok(())
    }

    // The shared session intentionally tolerates unreadable recovery records.
    // Windows reports them, rather than silently claiming recovery succeeded.
    fn check_recovery(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(store) = &self.recovery {
            let identity = std::fs::canonicalize(path)?;
            store
                .read(identity)
                .context("cannot read editor recovery snapshot")?;
        }
        Ok(())
    }

    fn initialize(&self) -> Vec<HostMessage> {
        let session = self.session.as_ref().unwrap();
        let mut messages = session.initialize_messages(self.zoom);
        messages.extend(session.pending_recovery_messages());
        messages
    }

    fn open(&mut self, path: PathBuf) -> anyhow::Result<Vec<HostMessage>> {
        let path = editor::resolve_existing(&self.root, &path)?;
        let state =
            editor::session_state(self.session.as_ref().unwrap().session_snapshot(), self.zoom);
        ensure!(
            state.open_files.len() < editor::MAX_DOCUMENTS
                || state.open_files.iter().any(|f| f.path == path),
            "editor session exceeds 128 documents"
        );
        self.check_recovery(&path)?;
        Ok(self.session.as_mut().unwrap().open_document(path)?)
    }

    fn run(&mut self, id: u64, work: Work) -> Response {
        if self.session.is_none() {
            return self.response(id, Vec::new(), self.startup_error.clone());
        }
        self.operation_error = None;
        self.search_snapshot = None;
        let content_update = self.content_update(&work);
        let disk_poll = matches!(&work, Work::PollDisk);
        let result = self.execute(work);
        let (messages, mut error) = match result {
            Ok(messages) => (messages, None),
            Err(error) => (Vec::new(), Some(format!("{error:#}"))),
        };
        if let Some(operation) = self.operation_error.take() {
            append_error(&mut error, &operation);
        }
        self.update_metadata(&messages);
        self.record_content_update(content_update, &messages);
        self.update_disk_certainty(disk_poll, error.is_some(), &messages);
        for message in &messages {
            let reason = match message {
                HostMessage::SaveFailed { reason, .. }
                | HostMessage::SaveAsFailed { reason, .. }
                | HostMessage::ConflictActionFailed { reason, .. } => Some(reason.as_str()),
                HostMessage::WorkspaceSearchCompleted { error, .. } => error.as_deref(),
                _ => None,
            };
            if let Some(reason) = reason {
                append_error(&mut error, reason);
            }
        }
        if let Err(recovery) = self.persist_recovery() {
            append_error(&mut error, &format!("{recovery:#}"));
        }
        self.response(id, messages, error)
    }

    fn execute(&mut self, work: Work) -> anyhow::Result<Vec<HostMessage>> {
        match work {
            Work::Initialize => Ok(self.initialize()),
            Work::Open(path) => self.open(path),
            Work::Message(message) => self.message(message),
            Work::SaveAll => {
                let (messages, result) = self.session.as_mut().unwrap().save_all_dirty();
                // Earlier files may have saved before a later conflict. Preserve
                // their replacement messages even when Save All is incomplete.
                self.operation_error = result.err().map(|error| error.to_string());
                Ok(messages)
            }
            Work::DiscardAll => {
                self.session.as_mut().unwrap().discard_all_dirty();
                Ok(self.initialize())
            }
            Work::PollDisk => Ok(self.poll_disk()),
            Work::Snapshot => Ok(Vec::new()),
            Work::SearchOpen(request) => self.search_open(request),
            Work::SearchSnapshot => {
                self.search_snapshot = Some(self.capture_search_snapshot(|| {
                    self.session.as_ref().unwrap().search_documents()
                })?);
                Ok(Vec::new())
            }
        }
    }

    fn content_update(&self, work: &Work) -> Option<ContentUpdate> {
        let Work::Message(message) = work else {
            return None;
        };
        match message {
            EditorMessage::DocumentChanged {
                document_id,
                document_version,
                change_sequence,
                content,
            }
            | EditorMessage::SaveRequested {
                document_id,
                document_version,
                change_sequence,
                content,
            }
            | EditorMessage::SaveAsRequested {
                document_id,
                document_version,
                change_sequence,
                content,
                ..
            } => Some(ContentUpdate {
                document_id: document_id.clone(),
                change_sequence: *change_sequence,
                bytes: content.len(),
                was_current: self
                    .documents
                    .get(document_id)
                    .is_some_and(|document| document.version == *document_version),
            }),
            _ => None,
        }
    }

    fn record_content_update(&mut self, update: Option<ContentUpdate>, messages: &[HostMessage]) {
        let Some(update) = update else { return };
        let acknowledged = messages.iter().any(|message| match message {
            HostMessage::DocumentChangeApplied {
                document_id,
                change_sequence,
                ..
            }
            | HostMessage::SaveCompleted {
                document_id,
                change_sequence,
                ..
            } => document_id == &update.document_id && *change_sequence == update.change_sequence,
            HostMessage::SaveAsCompleted {
                document,
                change_sequence,
            } => document.id == update.document_id && *change_sequence == update.change_sequence,
            _ => false,
        });
        if acknowledged {
            self.content_bytes.insert(update.document_id, update.bytes);
        } else if update.was_current {
            // Shared save/change code can mutate text before a later I/O or
            // recovery error prevents its ACK. Do not guess that length, or use
            // the request's rejected content for admission. A later full payload
            // or successful content ACK restores certainty. Stale requests never
            // reach a mutation and must leave the previous accounting intact.
            self.content_bytes.remove(&update.document_id);
        }
    }

    fn capture_search_snapshot(
        &self,
        collect: impl FnOnce() -> Vec<SearchDocument>,
    ) -> anyhow::Result<SearchSnapshot> {
        // This check MUST precede search_documents(), whose public shared API
        // clones every open buffer before returning it. Never admit by a sum
        // computed after that potentially 128 x 16 MiB allocation.
        let total_bytes = self.documents.keys().try_fold(0usize, |total, id| {
            let bytes = self.content_bytes.get(id).context(
                "editor buffer length is not synchronized; reconcile the document before searching",
            )?;
            let next = total
                .checked_add(*bytes)
                .context("editor search snapshot size overflow")?;
            ensure!(
                next <= MAX_SEARCH_SNAPSHOT_BYTES,
                "editor search snapshot exceeds 16 MiB of open buffers"
            );
            Ok::<_, anyhow::Error>(next)
        })?;
        let state = self.session.as_ref().unwrap().session_snapshot();
        ensure!(
            state.open_files.len() == self.documents.len(),
            "editor search document catalog is not synchronized"
        );
        // Full payload/close messages maintain the same order as the session.
        // Do not infer IDs from display paths: Save As through an alias can have
        // a different display path and canonical identity while keeping its ID.
        ensure!(
            self.document_order.len() == self.documents.len(),
            "editor search document order is not synchronized"
        );
        let ordered: Vec<_> = self
            .document_order
            .iter()
            .map(|id| {
                self.documents
                    .get(id)
                    .context("editor search document identity is not synchronized")
            })
            .collect::<anyhow::Result<_>>()?;
        let captured = collect();
        ensure!(
            captured.len() == ordered.len(),
            "editor search snapshot changed while being captured"
        );
        let documents = captured
            .into_iter()
            .zip(ordered)
            .map(|(document, metadata)| {
                ensure!(
                    Some(document.content.len()) == self.content_bytes.get(&metadata.id).copied(),
                    "editor search buffer length changed without an acknowledgment"
                );
                Ok(SearchBuffer {
                    document_id: metadata.id.clone(),
                    version: metadata.version,
                    path: editor::display_path(document.path),
                    content: document.content,
                })
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(SearchSnapshot {
            documents,
            total_bytes,
        })
    }

    fn search_open(&mut self, request: SearchOpen) -> anyhow::Result<Vec<HostMessage>> {
        editor::validate_relative_path(&request.path)?;
        let Some(range) = request.range else {
            // Quick Open still goes through the ordinary canonical root, file,
            // document count and recovery validation on this I/O worker.
            return self.open(request.path);
        };
        let hash = request
            .expected_sha256
            .as_deref()
            .context("search range requires a retained source fingerprint")?;
        let snapshot =
            self.capture_search_snapshot(|| self.session.as_ref().unwrap().search_documents())?;
        let path = self.root.join(&request.path);
        if let Some(document) = snapshot
            .documents
            .iter()
            .find(|document| crate::editor_search::same_path(&document.path, &path))
        {
            let expected = request
                .expected_document
                .as_ref()
                .context("search source is now an open buffer; search again")?;
            ensure!(
                document.document_id == expected.document_id
                    && document.version == expected.version,
                "search result document identity or version is stale; search again"
            );
            ensure!(
                crate::editor_search::content_sha256(&document.content) == hash,
                "search result buffer content is stale; search again"
            );
            validate_search_range(&document.content, range)?;
            // Do not reopen/reread dirty or deleted buffers from disk. This public
            // session API activates the already validated display path directly.
            let index = self
                .document_order
                .iter()
                .position(|id| id == &document.document_id)
                .context("search document order changed before activation")?;
            let active_path = self
                .session
                .as_ref()
                .unwrap()
                .session_snapshot()
                .open_files
                .get(index)
                .context("search document disappeared before activation")?
                .path
                .clone();
            self.session.as_mut().unwrap().activate_path(active_path);
            return Ok(vec![
                HostMessage::SetActiveDocument {
                    document_id: document.document_id.clone(),
                    document_version: document.version,
                },
                reveal_search_range(&document.document_id, document.version, range),
            ]);
        }
        ensure!(
            request.expected_document.is_none(),
            "search result buffer was closed or replaced; search again"
        );
        crate::editor_search::validate_result_source(&self.root, &request.path, hash)?;
        let messages = self.open(request.path.clone())?;
        Ok(self.finish_search_open(&request, messages))
    }

    fn finish_search_open(
        &mut self,
        request: &SearchOpen,
        mut messages: Vec<HostMessage>,
    ) -> Vec<HostMessage> {
        // Opening uses the shared API, which reads again after validation. Check
        // the actual payload before revealing. Keep its model/activation messages
        // even on mismatch: that open has already changed the backend session.
        let checked = (|| -> anyhow::Result<HostMessage> {
            let document = messages
                .iter()
                .find_map(|message| match message {
                    HostMessage::OpenDocument { document } => Some(document),
                    _ => None,
                })
                .context("search source became an open buffer while opening; search again")?;
            let hash = request
                .expected_sha256
                .as_deref()
                .context("missing search source fingerprint")?;
            ensure!(crate::editor_search::content_sha256(&document.content) == hash,
                "search source changed while opening; document opened without revealing a stale range");
            let range = request.range.context("missing search result range")?;
            validate_search_range(&document.content, range)?;
            Ok(reveal_search_range(&document.id, document.version, range))
        })();
        match checked {
            Ok(reveal) => messages.push(reveal),
            Err(error) => self.operation_error = Some(format!("{error:#}")),
        }
        messages
    }

    fn poll_disk(&mut self) -> Vec<HostMessage> {
        // Automatic delivery uses the Windows refresh adapter's non-activating
        // apply path. Do not add SetActiveDocument: the shared session already
        // preserves its active document, and the adapter preserves its model.
        let result = self
            .session
            .as_mut()
            .unwrap()
            .poll_external_changes_after_fs_event();
        match result {
            Ok(messages) => messages,
            Err(error) => {
                // The shared poll can reload earlier documents, then lose its
                // accumulated messages when a later filesystem read fails.
                // Preserve its error AND reconcile those completed changes.
                self.operation_error = Some(format!("{error:#}"));
                self.reconcile_partial_disk_poll()
            }
        }
    }

    fn reconcile_partial_disk_poll(&self) -> Vec<HostMessage> {
        // This is the only public shared API exposing current document payloads.
        // Use it only on the exceptional path, and never forward InitializeEditor:
        // doing so would dispose every Monaco model and erase undo history.
        let snapshot = self
            .session
            .as_ref()
            .unwrap()
            .initialize_messages(self.zoom);
        let documents = snapshot.into_iter().flat_map(|message| match message {
            HostMessage::InitializeEditor { documents, .. } => documents,
            HostMessage::OpenDocument { document } => vec![document],
            _ => Vec::new(),
        });
        let mut messages = Vec::new();
        for document in documents {
            let Some(previous) = self.documents.get(&document.id) else {
                // Poll never adds/removes documents. An unexpected unknown ID
                // must not create a second frontend model from an error path.
                continue;
            };
            let status = self.reconciled_disk_status(&document);
            let document_id = document.id.clone();
            let document_version = document.version;
            if document.version > previous.version && !document.dirty {
                messages.push(HostMessage::ReplaceDocument { document });
            }
            // Same-version dirty buffers retain their model/undo/diff state.
            // Recover status-only transitions that the shared poll also lost.
            if let Some(status) = status {
                messages.push(HostMessage::DocumentDiskStatus {
                    document_id,
                    document_version,
                    status,
                });
            }
        }
        messages
    }

    fn reconciled_disk_status(&self, document: &DocumentPayload) -> Option<DocumentDiskStatus> {
        if !document.external_change {
            // The public payload accessor trusts size/mtime. A forced-byte poll
            // may already have consumed a same-stamp Modified transition before
            // the later error. Never clear a conflict from this weak snapshot.
            return None;
        }
        // Payload.external_change also becomes true when its disk-status read
        // fails. Do not misreport unreadability as a content conflict. A tiny
        // readability probe distinguishes that failure and the deleted case;
        // it does not re-read the entire file or mutate shared state.
        use std::io::Read;
        let path = self.root.join(&document.relative_path);
        let readable = std::fs::File::open(&path).and_then(|mut file| {
            let mut first_byte = [0];
            file.read(&mut first_byte).map(|_| ())
        });
        match readable {
            Ok(()) => Some(DocumentDiskStatus::Modified),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Some(DocumentDiskStatus::Deleted)
            }
            Err(_) => None, // Original poll error remains visible; keep prior UI status.
        }
    }

    fn update_disk_certainty(&mut self, poll: bool, failed: bool, messages: &[HostMessage]) {
        if poll && failed {
            // The public shared API cannot identify the unreturned status of
            // every document. Mark it explicitly instead of claiming an exact
            // scan. Clean advancing replacements below still repair versions.
            self.disk_status_unknown
                .extend(self.documents.keys().cloned());
        }
        for message in messages {
            let confirmed = match message {
                HostMessage::ReplaceDocument { document } if !document.dirty => {
                    Some(document.id.as_str())
                }
                HostMessage::SaveAsCompleted { document, .. } => Some(document.id.as_str()),
                HostMessage::SaveCompleted { document_id, .. } => Some(document_id.as_str()),
                HostMessage::DocumentDiskStatus { document_id, .. } if poll && !failed => {
                    Some(document_id.as_str())
                }
                _ => None,
            };
            if let Some(id) = confirmed {
                self.disk_status_unknown.remove(id);
            }
        }
        // Empty successful polls cannot repair a previously consumed transition:
        // the shared reported-status map may suppress the missing message again.
        self.disk_status_unknown
            .retain(|id| self.documents.contains_key(id));
        for document in self.documents.values_mut() {
            document.disk_status_known = !self.disk_status_unknown.contains(&document.id);
        }
    }

    fn message(&mut self, message: EditorMessage) -> anyhow::Result<Vec<HostMessage>> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Envelope<'a> {
            protocol_version: u16,
            surface_id: &'static str,
            #[serde(flatten)]
            message: &'a EditorMessage,
        }
        let encoded = serde_json::to_string(&Envelope {
            protocol_version: flowmux_editor::PROTOCOL_VERSION,
            surface_id: "worker",
            message: &message,
        })?;
        flowmux_editor::parse_editor_message(&encoded)?;
        match message {
            EditorMessage::EditorReady => Ok(self.initialize()),
            EditorMessage::ZoomChanged { zoom_percent } => {
                self.zoom = zoom_percent;
                Ok(Vec::new())
            }
            EditorMessage::NativeEditRequested { .. }
            | EditorMessage::FocusDirectionRequested { .. }
            | EditorMessage::FlushCompleted { .. } => {
                anyhow::bail!("editor native/flush request must be handled by its UI owner")
            }
            EditorMessage::QuickOpenRequested { request_id } => {
                self.operation_error = Some("Windows Quick Open is not implemented".into());
                Ok(vec![HostMessage::QuickOpenCompleted {
                    request_id,
                    paths: Vec::new(),
                    truncated: false,
                }])
            }
            EditorMessage::WorkspaceSearchRequested { request_id, .. } => {
                Ok(vec![HostMessage::WorkspaceSearchCompleted {
                    request_id,
                    result: WorkspaceSearchResult::default(),
                    error: Some("Windows workspace search is not implemented".into()),
                }])
            }
            EditorMessage::SearchCancelled { request_id } => {
                Ok(vec![HostMessage::WorkspaceSearchCompleted {
                    request_id,
                    result: WorkspaceSearchResult {
                        cancelled: true,
                        ..Default::default()
                    },
                    error: None,
                }])
            }
            EditorMessage::SearchResultOpenRequested { .. } => {
                anyhow::bail!(
                    "search result open requires a retained result token from its UI owner"
                )
            }
            message => Ok(self
                .session
                .as_mut()
                .unwrap()
                .handle_editor_message(message)?),
        }
    }

    fn persist_recovery(&mut self) -> anyhow::Result<()> {
        for operation in self.session.as_mut().unwrap().take_recovery_operations() {
            self.recovery_pending
                .insert(operation.identity_path().to_path_buf(), operation);
        }
        let Some(store) = &self.recovery else {
            return Ok(());
        };
        let mut errors = Vec::new();
        for (path, operation) in std::mem::take(&mut self.recovery_pending) {
            if let Err(error) = crate::editor_recovery::apply(store, &operation) {
                errors.push(error.to_string());
                self.recovery_pending.insert(path, operation);
            }
        }
        ensure!(
            errors.is_empty(),
            "editor recovery was not persisted: {}",
            errors.join("; ")
        );
        Ok(())
    }

    fn metadata(&self, payload: &DocumentPayload) -> DocumentMeta {
        DocumentMeta {
            id: payload.id.clone(),
            path: self.root.join(&payload.relative_path),
            dirty: payload.dirty,
            encoding: payload.encoding,
            eol: payload.eol,
            version: payload.version,
            active: false,
            read_only: payload.read_only,
            external_change: payload.external_change,
            disk_status_known: !self.disk_status_unknown.contains(&payload.id),
        }
    }
    fn update_metadata(&mut self, messages: &[HostMessage]) {
        for message in messages {
            match message {
                HostMessage::InitializeEditor { documents, .. } => {
                    self.documents.clear();
                    self.document_order.clear();
                    self.content_bytes.clear();
                    for document in documents {
                        self.document_order.push(document.id.clone());
                        self.content_bytes
                            .insert(document.id.clone(), document.content.len());
                        self.documents
                            .insert(document.id.clone(), self.metadata(document));
                    }
                }
                HostMessage::OpenDocument { document }
                | HostMessage::ReplaceDocument { document }
                | HostMessage::SaveAsCompleted { document, .. } => {
                    if !self.documents.contains_key(&document.id) {
                        self.document_order.push(document.id.clone());
                    }
                    self.content_bytes
                        .insert(document.id.clone(), document.content.len());
                    self.documents
                        .insert(document.id.clone(), self.metadata(document));
                }
                HostMessage::CloseDocument { document_id, .. } => {
                    self.documents.remove(document_id);
                    self.document_order.retain(|id| id != document_id);
                    self.content_bytes.remove(document_id);
                }
                HostMessage::DocumentChangeApplied {
                    document_id,
                    document_version,
                    ..
                }
                | HostMessage::SaveCompleted {
                    document_id,
                    document_version,
                    ..
                } => {
                    if let Some(document) = self.documents.get_mut(document_id) {
                        document.version = *document_version;
                        if matches!(message, HostMessage::SaveCompleted { .. }) {
                            document.external_change = false;
                        }
                    }
                }
                HostMessage::DocumentDiskStatus {
                    document_id,
                    status,
                    ..
                } => {
                    if let Some(document) = self.documents.get_mut(document_id) {
                        document.external_change = *status != DocumentDiskStatus::Unchanged;
                    }
                }
                _ => {}
            }
        }
    }
    fn response(&mut self, id: u64, messages: Vec<HostMessage>, error: Option<String>) -> Response {
        let (state, dirty) = self
            .session
            .as_ref()
            .map(|session| {
                (
                    editor::session_state(session.session_snapshot(), self.zoom),
                    session
                        .dirty_document_paths()
                        .into_iter()
                        .map(editor::display_path)
                        .collect::<Vec<_>>(),
                )
            })
            .unwrap_or_else(|| (self.restored_state.clone(), Vec::new()));
        for document in self.documents.values_mut() {
            document.dirty = dirty.contains(&document.path);
            document.active = state.active_file.as_ref() == Some(&document.path);
        }
        Response {
            id,
            messages,
            state,
            dirty,
            documents: self.documents.values().cloned().collect(),
            ready: self.session.is_some(),
            initialization_failed: self.session.is_none() && self.startup_error.is_some(),
            restored_errors: self.restored_errors.clone(),
            error,
            search_snapshot: self.search_snapshot.take(),
        }
    }
}

fn validate_search_range(content: &str, range: SearchRange) -> anyhow::Result<()> {
    let line = content
        .split('\n')
        .nth(range.line as usize)
        .context("search result line is invalid")?;
    let end = range
        .column
        .checked_add(range.length)
        .context("search range overflow")?;
    let mut units = 0u32;
    let mut start_valid = range.column == 0;
    let mut end_valid = end == 0;
    for character in line.chars() {
        units += character.len_utf16() as u32;
        start_valid |= units == range.column;
        end_valid |= units == end;
    }
    ensure!(
        start_valid && end_valid,
        "search range is not on valid UTF-16 boundaries"
    );
    Ok(())
}

fn reveal_search_range(
    document_id: &str,
    document_version: u64,
    range: SearchRange,
) -> HostMessage {
    HostMessage::RevealRange {
        document_id: document_id.into(),
        document_version,
        line: range.line,
        column: range.column,
        length: range.length,
    }
}

fn append_error(error: &mut Option<String>, reason: &str) {
    if let Some(error) = error {
        error.push_str("; ");
        error.push_str(reason);
    } else {
        *error = Some(reason.into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_editor::{ConflictAction, RecoveryChoice};
    use std::{fs, time::Duration};

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("flowmux-editor-worker-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn start(
        root: &Directory,
        restored: EditorSessionState,
        store: Option<RecoveryStore>,
    ) -> (Worker, mpsc::Receiver<Response>) {
        let (send, receive) = mpsc::channel();
        let worker = Worker::start(root.0.clone(), restored, store, move |response| {
            let _ = send.send(response);
        })
        .unwrap();
        (worker, receive)
    }
    fn send(worker: &Worker, receive: &mpsc::Receiver<Response>, id: u64, work: Work) -> Response {
        worker.submit(id, work).unwrap();
        let response = receive.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(response.id, id);
        response
    }
    fn payload(response: &Response) -> DocumentPayload {
        response
            .messages
            .iter()
            .find_map(|message| match message {
                HostMessage::OpenDocument { document }
                | HostMessage::ReplaceDocument { document }
                | HostMessage::SaveAsCompleted { document, .. } => Some(document.clone()),
                HostMessage::InitializeEditor { documents, .. } => documents.first().cloned(),
                _ => None,
            })
            .expect("document payload")
    }
    fn change(document: &DocumentPayload, content: &str) -> Work {
        Work::Message(EditorMessage::DocumentChanged {
            document_id: document.id.clone(),
            document_version: document.version,
            change_sequence: 1,
            content: content.into(),
        })
    }

    fn engine(root: &Directory) -> Engine {
        Engine::new(
            root.0.clone(),
            EditorSessionState::default(),
            RecoverySetup::Existing(None),
            &Shared::default(),
        )
    }

    fn search_request(path: &str, text: &str, expected: Option<(&str, u64)>) -> SearchOpen {
        SearchOpen {
            path: path.into(),
            range: Some(SearchRange {
                line: 0,
                column: 0,
                length: 1,
            }),
            expected_document: expected.map(|(id, version)| SearchDocumentVersion {
                document_id: id.into(),
                version,
            }),
            expected_sha256: Some(crate::editor_search::content_sha256(text)),
        }
    }

    #[test]
    fn search_snapshot_uses_acknowledged_unicode_and_ignores_stale_edit_lengths() {
        let root = Directory::new();
        let first = root.0.join("한글.txt");
        let second = root.0.join("second.txt");
        fs::write(&first, "disk\n").unwrap();
        fs::write(&second, "other").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let original = payload(&send(&worker, &receive, 1, Work::Open(first.clone())));
        let other = payload(&send(&worker, &receive, 2, Work::Open(second)));
        let text = "한글 한 é 😀\n";
        let changed = send(&worker, &receive, 3, change(&original, text));
        assert!(changed.error.is_none());
        let version = changed
            .documents
            .iter()
            .find(|d| d.id == original.id)
            .unwrap()
            .version;
        let rejected = send(&worker, &receive, 4, change(&original, "unacknowledged"));
        assert!(rejected.error.as_deref().unwrap().contains("stale"));
        let result = send(&worker, &receive, 5, Work::SearchSnapshot);
        assert!(result.error.is_none() && result.messages.is_empty());
        let snapshot = result.search_snapshot.unwrap();
        assert_eq!(snapshot.total_bytes, text.len() + 5);
        assert_eq!(snapshot.documents.len(), 2);
        assert_eq!(snapshot.documents[0].document_id, original.id);
        assert_eq!(snapshot.documents[0].version, version);
        assert_eq!(snapshot.documents[0].content, text);
        assert_eq!(
            snapshot.documents[0].path,
            editor::display_path(fs::canonicalize(&first).unwrap())
        );
        assert_eq!(snapshot.documents[1].document_id, other.id);
        assert_eq!(snapshot.documents[1].content, "other");
        assert_eq!(fs::read_to_string(first).unwrap(), "disk\n");
        assert!(send(&worker, &receive, 6, Work::Snapshot)
            .search_snapshot
            .is_none());
    }

    #[test]
    fn search_snapshot_admits_before_clone_and_keeps_oversized_buffer_overrides() {
        let root = Directory::new();
        let first = root.0.join("large-a.txt");
        let second = root.0.join("large-b.txt");
        let half = MAX_SEARCH_SNAPSHOT_BYTES / 2;
        fs::write(&first, vec![b'a'; half]).unwrap();
        fs::write(&second, vec![b'b'; half]).unwrap();
        let mut engine = engine(&root);
        assert!(engine.run(1, Work::Open(first)).error.is_none());
        let opened = engine.run(2, Work::Open(second));
        assert!(opened.error.is_none());
        let second_document = payload(&opened);
        drop(opened);
        let exact = engine.run(3, Work::SearchSnapshot).search_snapshot.unwrap();
        assert_eq!(exact.total_bytes, MAX_SEARCH_SNAPSHOT_BYTES);
        assert_eq!(
            exact.documents.len(),
            2,
            "each >2MiB path must still override disk"
        );
        assert!(exact
            .documents
            .iter()
            .all(|document| document.content.len() == half));
        drop(exact);
        assert!(engine
            .run(4, change(&second_document, &"b".repeat(half + 1)))
            .error
            .is_none());
        let rejected =
            engine.capture_search_snapshot(|| panic!("oversized buffers must not be cloned"));
        assert!(rejected.unwrap_err().to_string().contains("exceeds 16 MiB"));
        let version = engine.documents[&second_document.id].version;
        let closed = engine.run(
            5,
            Work::Message(EditorMessage::DiscardCloseRequested {
                document_id: second_document.id,
                document_version: version,
            }),
        );
        assert!(closed.error.is_none());
        let remaining = engine.run(6, Work::SearchSnapshot).search_snapshot.unwrap();
        assert_eq!(remaining.documents.len(), 1);
        assert_eq!(remaining.total_bytes, half);
    }

    #[test]
    fn search_snapshot_replaces_save_as_paths_and_tracks_clean_disk_replacements() {
        let root = Directory::new();
        let original = root.0.join("original.txt");
        let renamed = root.0.join("저장.txt");
        fs::write(&original, "base").unwrap();
        let mut engine = engine(&root);
        let document = payload(&engine.run(1, Work::Open(original.clone())));
        let saved = engine.run(
            2,
            Work::Message(EditorMessage::SaveAsRequested {
                document_id: document.id.clone(),
                document_version: document.version,
                change_sequence: 1,
                content: "saved 한글".into(),
                path: "저장.txt".into(),
                overwrite: false,
            }),
        );
        assert!(saved.error.is_none());
        let snapshot = engine.run(3, Work::SearchSnapshot).search_snapshot.unwrap();
        assert_eq!(snapshot.documents.len(), 1);
        assert_eq!(
            snapshot.documents[0].path,
            editor::display_path(fs::canonicalize(&renamed).unwrap())
        );
        assert_eq!(snapshot.documents[0].content, "saved 한글");
        assert_eq!(snapshot.documents[0].document_id, document.id);
        assert_eq!(fs::read_to_string(original).unwrap(), "base");
        fs::write(&renamed, "external 😀\n").unwrap();
        let polled = engine.run(4, Work::PollDisk);
        assert!(polled.error.is_none());
        let current = engine.run(5, Work::SearchSnapshot).search_snapshot.unwrap();
        assert_eq!(current.total_bytes, "external 😀\n".len());
        assert_eq!(current.documents[0].content, "external 😀\n");
        assert!(current.documents[0].version > snapshot.documents[0].version);
    }

    #[test]
    fn unacknowledged_current_save_failure_blocks_snapshot_before_clone() {
        let root = Directory::new();
        let path = root.0.join("base.txt");
        fs::write(&path, "base").unwrap();
        let mut engine = engine(&root);
        let document = payload(&engine.run(1, Work::Open(path)));
        let rejected = engine.run(
            2,
            Work::Message(EditorMessage::SaveAsRequested {
                document_id: document.id.clone(),
                document_version: document.version,
                change_sequence: 1,
                content: "larger unsaved content".into(),
                path: "missing/target.txt".into(),
                overwrite: false,
            }),
        );
        assert!(rejected.error.is_some());
        assert!(!engine.content_bytes.contains_key(&document.id));
        assert!(engine
            .capture_search_snapshot(|| panic!("unknown lengths must not be cloned"))
            .unwrap_err()
            .to_string()
            .contains("not synchronized"));
        // A subsequent full acknowledged payload establishes the actual length.
        assert!(engine.run(3, Work::Initialize).error.is_none());
        let snapshot = engine.run(4, Work::SearchSnapshot).search_snapshot.unwrap();
        assert_eq!(snapshot.total_bytes, snapshot.documents[0].content.len());
    }

    #[test]
    fn guarded_search_open_checks_buffer_identity_version_hash_before_activation() {
        let root = Directory::new();
        let first = root.0.join("first.txt");
        let second = root.0.join("second.txt");
        fs::write(&first, "disk").unwrap();
        fs::write(&second, "other").unwrap();
        let mut engine = engine(&root);
        let document = payload(&engine.run(1, Work::Open(first.clone())));
        let text = "한글 😀 unsaved";
        let changed = engine.run(2, change(&document, text));
        let version = changed.documents[0].version;
        engine.run(3, Work::Open(second.clone()));
        for request in [
            search_request("first.txt", text, Some((&document.id, document.version))),
            search_request("first.txt", "wrong", Some((&document.id, version))),
            search_request("first.txt", text, Some(("wrong-id", version))),
            search_request("first.txt", text, None),
        ] {
            let response = engine.run(4, Work::SearchOpen(request));
            assert!(response.error.is_some() && response.messages.is_empty());
            assert_eq!(
                response.state.active_file,
                Some(editor::display_path(fs::canonicalize(&second).unwrap()))
            );
        }
        // An open dirty buffer remains authoritative even after its disk file is deleted.
        fs::remove_file(first).unwrap();
        let response = engine.run(
            5,
            Work::SearchOpen(search_request(
                "first.txt",
                text,
                Some((&document.id, version)),
            )),
        );
        assert!(response.error.is_none());
        assert!(response.messages.iter().any(|message| matches!(message,
            HostMessage::RevealRange { document_id, document_version, .. }
                if document_id == &document.id && *document_version == version)));
        assert!(
            response
                .documents
                .iter()
                .find(|meta| meta.id == document.id)
                .unwrap()
                .active
        );
    }

    #[test]
    fn guarded_disk_open_uses_normalized_payload_and_rejects_changed_source() {
        let root = Directory::new();
        let path = root.0.join("한글.txt");
        let text = "한글 😀\nsecond\n";
        fs::write(&path, "\u{feff}한글 😀\r\nsecond\r\n").unwrap();
        let mut engine = engine(&root);
        let stale = engine.run(1, Work::SearchOpen(search_request("한글.txt", "old", None)));
        assert!(stale.error.is_some() && stale.documents.is_empty() && stale.messages.is_empty());
        let mut request = search_request("한글.txt", text, None);
        request.range = Some(SearchRange {
            line: 0,
            column: 3,
            length: 2,
        });
        let response = engine.run(2, Work::SearchOpen(request));
        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(payload(&response).content, text);
        assert!(matches!(
            response.messages.last(),
            Some(HostMessage::RevealRange {
                column: 3,
                length: 2,
                ..
            })
        ));
    }

    #[test]
    fn changed_payload_after_disk_validation_keeps_open_messages_without_reveal() {
        let root = Directory::new();
        let path = root.0.join("race.txt");
        fs::write(&path, "original").unwrap();
        let mut engine = engine(&root);
        let request = search_request("race.txt", "original", None);
        crate::editor_search::validate_result_source(
            &engine.root,
            &request.path,
            request.expected_sha256.as_deref().unwrap(),
        )
        .unwrap();
        // Deterministic TOCTOU boundary: mutate between the two real reads.
        fs::write(&path, "replacement").unwrap();
        let opened = engine.open(request.path.clone()).unwrap();
        let messages = engine.finish_search_open(&request, opened);
        assert!(engine
            .operation_error
            .as_deref()
            .unwrap()
            .contains("changed while opening"));
        assert!(messages.iter().any(|message| matches!(message,
            HostMessage::OpenDocument { document } if document.content == "replacement")));
        assert!(!messages
            .iter()
            .any(|message| matches!(message, HostMessage::RevealRange { .. })));
        engine.update_metadata(&messages);
        let error = engine.operation_error.take();
        let response = engine.response(1, messages, error);
        assert!(response.error.is_some());
        assert_eq!(response.documents.len(), 1);
        assert_eq!(
            engine
                .run(2, Work::SearchSnapshot)
                .search_snapshot
                .unwrap()
                .documents[0]
                .content,
            "replacement"
        );
    }

    #[test]
    fn quick_open_validates_root_and_raw_search_open_cannot_bypass_token_owner() {
        let root = Directory::new();
        fs::write(root.0.join("inside.txt"), "inside").unwrap();
        let mut engine = engine(&root);
        for path in ["../outside.txt", "/outside.txt"] {
            let result = engine.run(
                1,
                Work::SearchOpen(SearchOpen {
                    path: path.into(),
                    range: None,
                    expected_document: None,
                    expected_sha256: None,
                }),
            );
            assert!(result.error.is_some() && result.documents.is_empty());
        }
        let raw = engine.run(
            2,
            Work::Message(EditorMessage::SearchResultOpenRequested {
                path: "inside.txt".into(),
                line: 0,
                column: 0,
                length: 1,
            }),
        );
        assert!(raw
            .error
            .as_deref()
            .unwrap()
            .contains("retained result token"));
        assert!(raw.documents.is_empty());
        let opened = engine.run(
            3,
            Work::SearchOpen(SearchOpen {
                path: "inside.txt".into(),
                range: None,
                expected_document: None,
                expected_sha256: None,
            }),
        );
        assert!(opened.error.is_none() && opened.documents.len() == 1);
        assert!(!opened
            .messages
            .iter()
            .any(|message| matches!(message, HostMessage::RevealRange { .. })));
    }

    #[test]
    fn search_ranges_require_real_utf16_boundaries_and_allow_empty_matches() {
        let text = "한😀글\n";
        for (column, length) in [(0, 1), (1, 2), (3, 1), (4, 0)] {
            assert!(validate_search_range(
                text,
                SearchRange {
                    line: 0,
                    column,
                    length
                }
            )
            .is_ok());
        }
        for (line, column, length) in [(0, 2, 1), (0, 1, 1), (0, 4, 1), (2, 0, 0), (0, u32::MAX, 1)]
        {
            assert!(validate_search_range(
                text,
                SearchRange {
                    line,
                    column,
                    length
                }
            )
            .is_err());
        }
    }

    #[test]
    fn worker_preserves_bom_crlf_unicode_and_rejects_stale_edits() {
        let root = Directory::new();
        let path = root.0.join("한글.txt");
        fs::write(&path, "\u{feff}한글\r\n").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let opened = send(&worker, &receive, 1, Work::Open(path.clone()));
        assert!(opened.ready && opened.error.is_none());
        let document = payload(&opened);
        assert_eq!(document.content, "한글\n");
        assert_eq!(document.encoding, TextDocumentEncoding::Utf8Bom);
        assert_eq!(document.eol, TextDocumentLineEnding::CrLf);
        let text = "한글 한 é 😀\nsecond\n";
        let changed = send(&worker, &receive, 2, change(&document, text));
        assert!(changed.error.is_none());
        let version = changed.documents[0].version;
        let saved = send(
            &worker,
            &receive,
            3,
            Work::Message(EditorMessage::SaveRequested {
                document_id: document.id.clone(),
                document_version: version,
                change_sequence: 1,
                content: text.into(),
            }),
        );
        assert!(saved.error.is_none());
        assert!(saved.dirty.is_empty());
        assert_eq!(
            fs::read(&path).unwrap(),
            format!("\u{feff}{}", text.replace('\n', "\r\n")).as_bytes()
        );
        let stale = send(&worker, &receive, 4, change(&document, "stale"));
        assert!(stale.error.as_deref().unwrap().contains("stale"));
        assert_eq!(stale.documents[0].version, version);
        assert!(stale.dirty.is_empty());
    }

    #[test]
    fn external_conflict_keeps_dirty_text_until_explicit_keep_mine() {
        let root = Directory::new();
        let path = root.0.join("conflict.txt");
        fs::write(&path, "base\n").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let document = payload(&send(&worker, &receive, 1, Work::Open(path.clone())));
        let edited = send(&worker, &receive, 2, change(&document, "my 한 é\n"));
        let version = edited.documents[0].version;
        fs::write(&path, "external\n").unwrap();
        let failed = send(
            &worker,
            &receive,
            3,
            Work::Message(EditorMessage::SaveRequested {
                document_id: document.id.clone(),
                document_version: version,
                change_sequence: 1,
                content: "my 한 é\n".into(),
            }),
        );
        assert!(failed.error.is_some() && failed.documents[0].dirty);
        assert_eq!(failed.documents[0].version, version);
        assert_eq!(fs::read_to_string(&path).unwrap(), "external\n");
        let compared = send(
            &worker,
            &receive,
            4,
            Work::Message(EditorMessage::ConflictActionRequested {
                document_id: document.id.clone(),
                document_version: version,
                action: ConflictAction::Compare,
            }),
        );
        assert!(
            matches!(&compared.messages[0], HostMessage::ShowDiff { disk_content, .. } if disk_content == "external\n")
        );
        let kept = send(
            &worker,
            &receive,
            5,
            Work::Message(EditorMessage::ConflictActionRequested {
                document_id: document.id,
                document_version: version,
                action: ConflictAction::KeepMine,
            }),
        );
        assert!(kept.error.is_none() && kept.documents[0].dirty);
        let saved = send(&worker, &receive, 6, Work::SaveAll);
        assert!(saved.error.is_none() && saved.dirty.is_empty());
        assert_eq!(fs::read_to_string(&path).unwrap(), "my 한 é\n");
    }

    #[test]
    fn partial_save_all_retains_successful_replacements_and_later_conflict() {
        let root = Directory::new();
        let first = root.0.join("first.txt");
        let second = root.0.join("second.txt");
        fs::write(&first, "first").unwrap();
        fs::write(&second, "second").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let first_document = payload(&send(&worker, &receive, 1, Work::Open(first.clone())));
        let second_document = payload(&send(&worker, &receive, 2, Work::Open(second.clone())));
        send(&worker, &receive, 3, change(&first_document, "saved first"));
        send(
            &worker,
            &receive,
            4,
            change(&second_document, "unsaved second"),
        );
        fs::write(&second, "other process").unwrap();
        let result = send(&worker, &receive, 5, Work::SaveAll);
        assert!(result.error.is_some());
        assert!(result.messages.iter().any(|m| matches!(m, HostMessage::ReplaceDocument { document } if document.id == first_document.id && !document.dirty)));
        assert_eq!(
            result.dirty,
            vec![editor::display_path(fs::canonicalize(&second).unwrap())]
        );
        assert_eq!(fs::read_to_string(&first).unwrap(), "saved first");
        assert_eq!(fs::read_to_string(&second).unwrap(), "other process");
    }

    #[test]
    fn save_as_denial_retains_version_and_explicit_overwrite_updates_path() {
        let root = Directory::new();
        let path = root.0.join("original.txt");
        let target = root.0.join("저장 한.txt");
        fs::write(&path, "original").unwrap();
        fs::write(&target, "existing").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let document = payload(&send(&worker, &receive, 1, Work::Open(path)));
        let changed = send(&worker, &receive, 2, change(&document, "한글 é 😀"));
        let version = changed.documents[0].version;
        for (id, overwrite) in [(3, false), (4, true)] {
            let result = send(
                &worker,
                &receive,
                id,
                Work::Message(EditorMessage::SaveAsRequested {
                    document_id: document.id.clone(),
                    document_version: version,
                    change_sequence: 1,
                    content: "한글 é 😀".into(),
                    path: "저장 한.txt".into(),
                    overwrite,
                }),
            );
            if !overwrite {
                assert!(result.error.is_some());
                assert_eq!(result.documents[0].version, version);
                assert_eq!(fs::read_to_string(&target).unwrap(), "existing");
            } else {
                assert!(result.error.is_none() && result.dirty.is_empty());
                editor::validate_session(&result.state).unwrap();
                assert_eq!(
                    result.state.active_file,
                    Some(result.documents[0].path.clone())
                );
                assert_eq!(fs::read_to_string(&target).unwrap(), "한글 é 😀");
            }
        }
    }

    #[test]
    fn recovery_is_persisted_before_reply_and_offered_after_worker_restart() {
        let root = Directory::new();
        let state = Directory::new();
        let path = root.0.join("recovery.txt");
        fs::write(&path, "base").unwrap();
        let store = RecoveryStore::new_scoped(&state.0, &root.0, "surface-one").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), Some(store.clone()));
        let document = payload(&send(&worker, &receive, 1, Work::Open(path.clone())));
        let changed = send(&worker, &receive, 2, change(&document, "crash 한글 é 😀"));
        assert!(changed.error.is_none());
        let snapshot = store
            .read(fs::canonicalize(&path).unwrap())
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(snapshot.content, "crash 한글 é 😀");
        drop(worker);
        let (worker, receive) = start(&root, changed.state, Some(store));
        let initialized = send(&worker, &receive, 3, Work::Initialize);
        assert!(initialized
            .messages
            .iter()
            .any(|message| matches!(message, HostMessage::RecoveryAvailable { .. })));
        let document = payload(&initialized);
        let recovered = send(
            &worker,
            &receive,
            4,
            Work::Message(EditorMessage::RecoveryDecision {
                document_id: document.id,
                document_version: document.version,
                choice: RecoveryChoice::Restore,
            }),
        );
        assert_eq!(payload(&recovered).content, "crash 한글 é 😀");
        assert!(recovered.documents[0].dirty);
        assert_eq!(fs::read_to_string(path).unwrap(), "base");
    }

    #[test]
    fn recovery_write_failure_is_reported_and_retained_for_next_operation() {
        let root = Directory::new();
        let state = Directory::new();
        let path = root.0.join("recovery.txt");
        fs::write(&path, "base").unwrap();
        let store = RecoveryStore::new_scoped(&state.0, &root.0, "surface-one").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), Some(store.clone()));
        let document = payload(&send(&worker, &receive, 1, Work::Open(path.clone())));
        fs::remove_dir_all(state.0.join("editor-recovery")).unwrap();
        let changed = send(&worker, &receive, 2, change(&document, "retained"));
        assert!(changed.error.as_deref().unwrap().contains("recovery"));
        assert!(changed.documents[0].dirty);
        RecoveryStore::new_scoped(&state.0, &root.0, "surface-one").unwrap();
        let retried = send(&worker, &receive, 3, Work::Snapshot);
        assert!(retried.error.is_none());
        assert_eq!(
            store
                .read(fs::canonicalize(path).unwrap())
                .unwrap()
                .unwrap()
                .0
                .content,
            "retained"
        );
    }

    #[test]
    fn queue_capacity_rejects_busy_and_drop_cancels_accepted_work() {
        let root = Directory::new();
        let (send_response, receive) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let first = AtomicBool::new(true);
        let worker = Worker::start(
            root.0.clone(),
            EditorSessionState::default(),
            None,
            move |response| {
                send_response.send(response).unwrap();
                if first.swap(false, Ordering::SeqCst) {
                    blocked.recv_timeout(Duration::from_secs(10)).unwrap();
                }
            },
        )
        .unwrap();
        send(&worker, &receive, 0, Work::Snapshot);
        for id in 1..=MAX_PENDING as u64 {
            worker.submit(id, Work::Snapshot).unwrap();
        }
        assert!(worker.submit(100, Work::Snapshot).is_err());
        drop(worker);
        release.send(()).unwrap();
        for id in 1..=MAX_PENDING as u64 {
            let response = receive.recv_timeout(Duration::from_secs(10)).unwrap();
            assert_eq!(response.id, id);
            assert!(response.error.as_deref().unwrap().contains("cancelled"));
        }
    }

    #[test]
    fn missing_restored_root_retains_saved_state_without_failing_another_worker() {
        let root = Directory::new();
        let missing = root.0.join("missing workspace");
        let file = missing.join("한글.txt");
        let restored = EditorSessionState {
            open_files: vec![flowmux_core::EditorFileState {
                path: file.clone(),
                cursor_line: 12,
                cursor_column: 4,
                scroll_top: 123.5,
            }],
            active_file: Some(file),
            zoom_percent: Some(125),
        };
        let (sender, receiver) = mpsc::channel();
        let failed = Worker::start_scoped(
            missing,
            restored.clone(),
            root.0.join("state"),
            "missing-surface".into(),
            move |response| {
                let _ = sender.send(response);
            },
        )
        .unwrap();
        let response = send(&failed, &receiver, 1, Work::Initialize);
        assert!(!response.ready && response.initialization_failed);
        assert!(response.error.is_some() && !response.restored_errors.is_empty());
        assert_eq!(response.state, restored);
        assert!(response.documents.is_empty() && response.dirty.is_empty());
        assert_eq!(send(&failed, &receiver, 2, Work::Snapshot).state, restored);

        let (healthy, receiver) = start(&root, EditorSessionState::default(), None);
        let response = send(&healthy, &receiver, 3, Work::Initialize);
        assert!(response.ready && !response.initialization_failed && response.error.is_none());
    }

    #[test]
    fn scoped_recovery_creation_failure_is_reported_as_uninitialized_with_state_intact() {
        let root = Directory::new();
        let file = root.0.join("existing.txt");
        fs::write(&file, "disk content").unwrap();
        let obstructed = root.0.join("state-is-a-file");
        fs::write(&obstructed, "do not replace").unwrap();
        let restored = EditorSessionState {
            open_files: vec![flowmux_core::EditorFileState {
                path: file.clone(),
                cursor_line: 0,
                cursor_column: 3,
                scroll_top: 0.0,
            }],
            active_file: Some(file),
            zoom_percent: None,
        };
        let (sender, receiver) = mpsc::channel();
        let worker = Worker::start_scoped(
            root.0.clone(),
            restored.clone(),
            obstructed.clone(),
            "surface-one".into(),
            move |response| {
                let _ = sender.send(response);
            },
        )
        .unwrap();
        let response = send(&worker, &receiver, 1, Work::Initialize);
        assert!(!response.ready && response.initialization_failed);
        assert!(response.error.as_deref().unwrap().contains("recovery"));
        assert_eq!(response.state, restored);
        assert_eq!(fs::read_to_string(obstructed).unwrap(), "do not replace");
    }
    // A regular file replaced by a directory makes reads fail on Windows and Linux
    // without relying on administrator/root permission behavior or sleeps.

    #[test]
    fn partial_disk_poll_delivers_completed_clean_reload_and_preserves_active_document() {
        let root = Directory::new();
        let first = root.0.join("first 한글 한 😀.txt");
        let second = root.0.join("second unreadable.txt");
        fs::write(&first, "\u{feff}first\r\n").unwrap();
        fs::write(&second, "second\n").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let before_first = payload(&send(&worker, &receive, 1, Work::Open(first.clone())));
        let before_second = payload(&send(&worker, &receive, 2, Work::Open(second.clone())));
        let updated = "외부 변경 한 e\u{301} 😀\nsecond line\n";
        let disk_bytes = format!("\u{feff}{}", updated.replace('\n', "\r\n"));
        fs::write(&first, &disk_bytes).unwrap();
        fs::remove_file(&second).unwrap();
        fs::create_dir(&second).unwrap();

        let polled = send(&worker, &receive, 3, Work::PollDisk);
        assert!(polled
            .error
            .as_deref()
            .unwrap()
            .contains("inspect document"));
        let replacements: Vec<_> = polled
            .messages
            .iter()
            .filter_map(|message| match message {
                HostMessage::ReplaceDocument { document } => Some(document),
                _ => None,
            })
            .collect();
        assert_eq!(replacements.len(), 1);
        assert_eq!(replacements[0].id, before_first.id);
        assert_eq!(replacements[0].content, updated);
        assert_eq!(replacements[0].version, before_first.version + 1);
        assert_eq!(replacements[0].encoding, TextDocumentEncoding::Utf8Bom);
        assert_eq!(replacements[0].eol, TextDocumentLineEnding::CrLf);
        assert!(!replacements[0].dirty);
        assert!(!polled.messages.iter().any(|message| matches!(
            message,
            HostMessage::InitializeEditor { .. } | HostMessage::OpenDocument { .. }
        )));
        assert!(
            polled.messages.iter().all(|message| matches!(
                message,
                HostMessage::ReplaceDocument { .. } | HostMessage::DocumentDiskStatus { .. }
            )),
            "automatic refresh must not activate any document"
        );
        let synchronized = polled
            .documents
            .iter()
            .find(|document| document.id == before_first.id)
            .unwrap();
        assert_eq!(synchronized.version, replacements[0].version);
        assert!(!synchronized.dirty);
        assert!(
            synchronized.disk_status_known,
            "the completed clean reload is authoritative"
        );
        let blocked = polled
            .documents
            .iter()
            .find(|document| document.id == before_second.id)
            .unwrap();
        assert_eq!(blocked.version, before_second.version);
        assert!(
            !blocked.disk_status_known,
            "the incomplete scan must be observable"
        );
        assert!(
            !blocked.external_change,
            "unreadable is an I/O error, not a proven content conflict"
        );
        assert_eq!(polled.state.active_file, Some(second.clone()));
        assert_eq!(fs::read(&first).unwrap(), disk_bytes.as_bytes());

        // Retry does not repeat the already-consumed shared reload; its payload had
        // to be present in the partial-error response above or the model stays stale.
        fs::remove_dir(&second).unwrap();
        fs::write(&second, "second\n").unwrap();
        let retried = send(&worker, &receive, 4, Work::PollDisk);
        assert!(retried.error.is_none());
        assert!(!retried.messages.iter().any(|message| matches!(message,
        HostMessage::ReplaceDocument { document } if document.id == before_first.id)));
        let edited = send(
            &worker,
            &receive,
            5,
            change(replacements[0], "edited after reload 한글\n"),
        );
        assert!(
            edited.error.is_none(),
            "the delivered replacement version must accept the next edit"
        );
        let saved = send(&worker, &receive, 6, Work::SaveAll);
        assert!(saved.error.is_none());
        assert_eq!(
            fs::read(&first).unwrap(),
            "\u{feff}edited after reload 한글\r\n".as_bytes()
        );
        assert_eq!(fs::read_to_string(second).unwrap(), "second\n");
    }

    #[test]
    fn partial_disk_poll_delivers_dirty_conflict_without_replacing_unsaved_models() {
        let root = Directory::new();
        let first = root.0.join("dirty 한글.txt");
        let second = root.0.join("later unreadable.txt");
        fs::write(&first, "base\n").unwrap();
        fs::write(&second, "untouched\n").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let document = payload(&send(&worker, &receive, 1, Work::Open(first.clone())));
        let changed = send(
            &worker,
            &receive,
            2,
            change(&document, "unsaved 한 e\u{301} 😀\n"),
        );
        let version = changed.documents[0].version;
        let other = payload(&send(&worker, &receive, 3, Work::Open(second.clone())));
        fs::write(&first, "external\n").unwrap();
        fs::remove_file(&second).unwrap();
        fs::create_dir(&second).unwrap();

        let polled = send(&worker, &receive, 4, Work::PollDisk);
        assert!(polled.error.is_some());
        assert!(
            matches!(polled.messages.iter().find(|message| matches!(message,
        HostMessage::DocumentDiskStatus { document_id, .. } if document_id == &document.id)),
        Some(HostMessage::DocumentDiskStatus { document_version, status: DocumentDiskStatus::Modified, .. })
        if *document_version == version)
        );
        assert!(!polled.messages.iter().any(|message| matches!(
            message,
            HostMessage::InitializeEditor { .. }
                | HostMessage::OpenDocument { .. }
                | HostMessage::ReplaceDocument { .. }
        )));
        let dirty = polled
            .documents
            .iter()
            .find(|metadata| metadata.id == document.id)
            .unwrap();
        assert!(dirty.dirty && dirty.external_change);
        assert!(!dirty.disk_status_known);
        assert_eq!(dirty.version, version);
        assert_eq!(polled.state.active_file, Some(second.clone()));
        assert_eq!(
            polled
                .documents
                .iter()
                .find(|metadata| metadata.id == other.id)
                .unwrap()
                .version,
            other.version
        );

        let failed = send(&worker, &receive, 5, Work::SaveAll);
        assert!(failed.error.is_some());
        assert_eq!(fs::read_to_string(&first).unwrap(), "external\n");
        let kept = send(
            &worker,
            &receive,
            6,
            Work::Message(EditorMessage::ConflictActionRequested {
                document_id: document.id.clone(),
                document_version: version,
                action: ConflictAction::KeepMine,
            }),
        );
        assert!(kept.error.is_none());
        let retained = payload(&kept);
        assert_eq!(retained.id, document.id);
        assert_eq!(retained.content, "unsaved 한 e\u{301} 😀\n");
        assert!(retained.dirty);
        assert_eq!(
            fs::read_to_string(&first).unwrap(),
            "external\n",
            "Keep Mine must retain the buffer without saving it"
        );
        let saved = send(&worker, &receive, 7, Work::SaveAll);
        assert!(saved.error.is_none() && saved.dirty.is_empty());
        assert_eq!(
            fs::read_to_string(&first).unwrap(),
            "unsaved 한 e\u{301} 😀\n"
        );
        fs::remove_dir(&second).unwrap();
        fs::write(&second, "untouched\n").unwrap();
        assert!(send(&worker, &receive, 8, Work::PollDisk).error.is_none());
    }

    #[test]
    fn partial_disk_poll_preserves_deleted_status_before_a_later_read_failure() {
        let root = Directory::new();
        let first = root.0.join("deleted first.txt");
        let second = root.0.join("unreadable second.txt");
        fs::write(&first, "keep buffer\n").unwrap();
        fs::write(&second, "second\n").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let document = payload(&send(&worker, &receive, 1, Work::Open(first.clone())));
        send(&worker, &receive, 2, Work::Open(second.clone()));
        fs::remove_file(&first).unwrap();
        fs::remove_file(&second).unwrap();
        fs::create_dir(&second).unwrap();
        let polled = send(&worker, &receive, 3, Work::PollDisk);
        assert!(polled.error.is_some());
        assert!(polled.messages.iter().any(|message| matches!(message,
        HostMessage::DocumentDiskStatus { document_id, status: DocumentDiskStatus::Deleted, .. }
        if document_id == &document.id)));
        assert!(!polled
            .messages
            .iter()
            .any(|message| matches!(message, HostMessage::ReplaceDocument { .. })));
        assert!(
            !first.exists(),
            "automatic polling must not recreate a deleted file"
        );
        assert_eq!(
            polled
                .documents
                .iter()
                .find(|metadata| metadata.id == document.id)
                .unwrap()
                .version,
            document.version
        );
    }

    #[test]
    fn successful_disk_reload_preserves_active_document_without_activation_messages() {
        let root = Directory::new();
        let first = root.0.join("changed background.txt");
        let second = root.0.join("active.txt");
        fs::write(&first, "before\n").unwrap();
        fs::write(&second, "active\n").unwrap();
        let (worker, receive) = start(&root, EditorSessionState::default(), None);
        let first_document = payload(&send(&worker, &receive, 1, Work::Open(first.clone())));
        send(&worker, &receive, 2, Work::Open(second.clone()));
        fs::write(first, "after\n").unwrap();
        let polled = send(&worker, &receive, 3, Work::PollDisk);
        assert!(polled.error.is_none());
        assert!(polled.messages.iter().any(|message| matches!(message,
        HostMessage::ReplaceDocument { document } if document.id == first_document.id && document.content == "after\n")));
        assert!(
            polled.messages.iter().all(|message| matches!(
                message,
                HostMessage::ReplaceDocument { .. } | HostMessage::DocumentDiskStatus { .. }
            )),
            "automatic refresh must not activate any document"
        );
        assert_eq!(polled.state.active_file, Some(second));
    }

    #[test]
    fn partial_same_stamp_dirty_poll_marks_unknown_and_never_clears_known_conflict() {
        for known_conflict in [false, true] {
            let root = Directory::new();
            let first = root.0.join("same stamp 한글.txt");
            let second = root.0.join("later unreadable.txt");
            fs::write(&first, "base\n").unwrap();
            fs::write(&second, "second\n").unwrap();
            let original_mtime = fs::metadata(&first).unwrap().modified().unwrap();
            let (worker, receive) = start(&root, EditorSessionState::default(), None);
            let document = payload(&send(&worker, &receive, 1, Work::Open(first.clone())));
            let changed = send(&worker, &receive, 2, change(&document, "mine\n"));
            let version = changed.documents[0].version;
            send(&worker, &receive, 3, Work::Open(second.clone()));
            if known_conflict {
                fs::write(&first, "previously observed external content\n").unwrap();
                let conflict = send(&worker, &receive, 4, Work::PollDisk);
                assert!(conflict.error.is_none());
                let first_meta = conflict
                    .documents
                    .iter()
                    .find(|meta| meta.id == document.id)
                    .unwrap();
                assert!(first_meta.external_change && first_meta.disk_status_known);
            }
            fs::write(&first, "disk\n").unwrap();
            fs::File::options()
                .write(true)
                .open(&first)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(original_mtime))
                .unwrap();
            assert_eq!(
                fs::metadata(&first).unwrap().modified().unwrap(),
                original_mtime
            );
            assert_eq!(fs::metadata(&first).unwrap().len(), 5);
            fs::remove_file(&second).unwrap();
            fs::create_dir(&second).unwrap();

            let polled = send(&worker, &receive, 5, Work::PollDisk);
            assert!(polled.error.is_some());
            assert!(!polled.messages.iter().any(|message| matches!(
                message,
                HostMessage::DocumentDiskStatus {
                    status: DocumentDiskStatus::Unchanged,
                    ..
                } | HostMessage::ReplaceDocument { .. }
            )));
            let first_meta = polled
                .documents
                .iter()
                .find(|meta| meta.id == document.id)
                .unwrap();
            assert_eq!(first_meta.external_change, known_conflict);
            assert!(
                !first_meta.disk_status_known,
                "a metadata shortcut cannot establish disk equality"
            );
            assert!(first_meta.dirty);
            assert_eq!(first_meta.version, version);
            let reactivated = send(&worker, &receive, 6, Work::Open(first.clone()));
            assert!(reactivated.error.is_none());
            let reactivated_meta = reactivated
                .documents
                .iter()
                .find(|meta| meta.id == document.id)
                .unwrap();
            assert!(!reactivated_meta.disk_status_known);
            assert_eq!(reactivated_meta.external_change, known_conflict);
            assert_eq!(reactivated_meta.version, version);
            let failed_save = send(&worker, &receive, 7, Work::SaveAll);
            assert!(failed_save.error.is_some());
            assert_eq!(fs::read_to_string(&first).unwrap(), "disk\n");

            fs::remove_dir(&second).unwrap();
            fs::write(&second, "second\n").unwrap();
            let retried = send(&worker, &receive, 8, Work::PollDisk);
            assert!(retried.error.is_none());
            assert!(
                !retried
                    .documents
                    .iter()
                    .find(|meta| meta.id == document.id)
                    .unwrap()
                    .disk_status_known,
                "a successful empty poll does not replay the consumed transition"
            );
            let kept = send(
                &worker,
                &receive,
                9,
                Work::Message(EditorMessage::ConflictActionRequested {
                    document_id: document.id.clone(),
                    document_version: version,
                    action: ConflictAction::KeepMine,
                }),
            );
            assert!(kept.error.is_none());
            assert_eq!(payload(&kept).content, "mine\n");
            assert_eq!(fs::read_to_string(&first).unwrap(), "disk\n");
            let saved = send(&worker, &receive, 10, Work::SaveAll);
            assert!(saved.error.is_none());
            assert!(
                saved
                    .documents
                    .iter()
                    .find(|meta| meta.id == document.id)
                    .unwrap()
                    .disk_status_known
            );
            assert_eq!(fs::read_to_string(&first).unwrap(), "mine\n");
        }
    }
}
