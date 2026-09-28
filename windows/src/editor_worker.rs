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
    HostMessage, RecoveryOperation, RecoveryStore, TextDocumentEncoding, TextDocumentLineEnding,
    WorkspaceSearchResult, EDITOR_ZOOM_DEFAULT,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread::JoinHandle,
};

pub const MAX_PENDING: usize = 8;
pub const MAX_QUEUED_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug)]
pub enum Work {
    Initialize,
    Open(PathBuf),
    Message(EditorMessage),
    SaveAll,
    DiscardAll,
    PollDisk,
    Snapshot,
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
    zoom: u16,
    restored_errors: Vec<String>,
    startup_error: Option<String>,
    operation_error: Option<String>,
    restored_state: EditorSessionState,
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
        let result = self.execute(work);
        let (messages, mut error) = match result {
            Ok(messages) => (messages, None),
            Err(error) => (Vec::new(), Some(format!("{error:#}"))),
        };
        if let Some(operation) = self.operation_error.take() {
            append_error(&mut error, &operation);
        }
        self.update_metadata(&messages);
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
            Work::PollDisk => Ok(self
                .session
                .as_mut()
                .unwrap()
                .poll_external_changes_after_fs_event()?),
            Work::Snapshot => Ok(Vec::new()),
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
            EditorMessage::SearchResultOpenRequested {
                path,
                line,
                column,
                length,
            } => {
                editor::validate_relative_path(std::path::Path::new(&path))?;
                let messages = self.open(path.into())?;
                let active = self
                    .session
                    .as_ref()
                    .unwrap()
                    .session_snapshot()
                    .active_file
                    .unwrap();
                let mut messages = messages;
                messages.extend(
                    self.session
                        .as_mut()
                        .unwrap()
                        .open_search_result(active, line, column, length)?,
                );
                Ok(messages)
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
        }
    }
    fn update_metadata(&mut self, messages: &[HostMessage]) {
        for message in messages {
            match message {
                HostMessage::InitializeEditor { documents, .. } => {
                    self.documents.clear();
                    for document in documents {
                        self.documents
                            .insert(document.id.clone(), self.metadata(document));
                    }
                }
                HostMessage::OpenDocument { document }
                | HostMessage::ReplaceDocument { document }
                | HostMessage::SaveAsCompleted { document, .. } => {
                    self.documents
                        .insert(document.id.clone(), self.metadata(document));
                }
                HostMessage::CloseDocument { document_id, .. } => {
                    self.documents.remove(document_id);
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
        }
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
}
