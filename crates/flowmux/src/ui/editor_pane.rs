// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform editor WebView and its versioned bridge state.

use flowmux_core::{EditorFileState, EditorSessionState, PaneId, SurfaceId};
use flowmux_editor::{
    index_workspace_files, javascript_for_host_message, parse_editor_message, search_workspace,
    EditorFileSessionState, EditorFocusDirection, EditorMessage, EditorNativeEditAction,
    EditorSession, EditorSessionSnapshot, EditorViewState, HostMessage, ProtocolError,
    RecoveryOperation, RecoveryStore, SearchCancellation, SearchOptions, WorkspaceSearchResult,
    EDITOR_ZOOM_DEFAULT, EDITOR_ZOOM_MAX, EDITOR_ZOOM_MIN,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

const RECOVERY_DEBOUNCE: Duration = Duration::from_millis(350);
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);
const FLUSH_POLL_INTERVAL: Duration = Duration::from_millis(10);
const QUICK_OPEN_LIMIT: usize = 2_000;
const LAST_EDITOR_ZOOM_FILE: &str = "editor-zoom-percent";

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EditorNavigationKey {
    Left,
    Right,
    Up,
    Down,
    PageUp,
    PageDown,
    Home,
    End,
}

#[cfg(target_os = "macos")]
impl EditorNavigationKey {
    pub(crate) fn monaco_action(self, extend_selection: bool) -> &'static str {
        match (self, extend_selection) {
            (Self::Left, false) => "cursorLeft",
            (Self::Right, false) => "cursorRight",
            (Self::Up, false) => "cursorUp",
            (Self::Down, false) => "cursorDown",
            (Self::PageUp, false) => "cursorPageUp",
            (Self::PageDown, false) => "cursorPageDown",
            (Self::Home, false) => "cursorHome",
            (Self::End, false) => "cursorEnd",
            (Self::Left, true) => "cursorLeftSelect",
            (Self::Right, true) => "cursorRightSelect",
            (Self::Up, true) => "cursorUpSelect",
            (Self::Down, true) => "cursorDownSelect",
            (Self::PageUp, true) => "cursorPageUpSelect",
            (Self::PageDown, true) => "cursorPageDownSelect",
            (Self::Home, true) => "cursorHomeSelect",
            (Self::End, true) => "cursorEndSelect",
        }
    }
}

pub(super) type EditorFocusDirectionCallback =
    Rc<RefCell<Option<Box<dyn FnMut(PaneId, EditorFocusDirection)>>>>;

enum SearchWorkerMessage {
    QuickOpen {
        request_id: String,
        paths: Vec<String>,
        truncated: bool,
    },
    WorkspaceSearch {
        request_id: String,
        result: WorkspaceSearchResult,
        error: Option<String>,
    },
}

#[derive(Clone, Copy)]
enum SearchWorkerKind {
    QuickOpen,
    WorkspaceSearch,
}

struct SearchWorker {
    request_id: String,
    kind: SearchWorkerKind,
    cancellation: SearchCancellation,
    receiver: Receiver<SearchWorkerMessage>,
}

type FlushCompletion = Rc<RefCell<Option<Result<(), String>>>>;

#[derive(Default)]
pub(super) struct EditorBridgeReceive {
    scripts: Vec<String>,
    message: Option<EditorMessage>,
}

pub(super) struct EditorBridgeDispatch {
    pub scripts: Vec<String>,
    pub focus_direction: Option<EditorFocusDirection>,
    pub native_edit_action: Option<EditorNativeEditAction>,
    pub native_edit_text: Option<String>,
    pub zoom_percent: Option<u16>,
}

pub(super) struct EditorBridgeState {
    surface_id: String,
    ready: Cell<bool>,
    pending: RefCell<Vec<HostMessage>>,
    // Bumped by `reset`. A bridge message that was still waiting on the file
    // worker when the page died must not queue its replies for the new page.
    generation: Cell<u64>,
}

impl EditorBridgeState {
    fn new(surface_id: SurfaceId) -> Self {
        Self {
            surface_id: surface_id.0.to_string(),
            ready: Cell::new(false),
            pending: RefCell::new(Vec::new()),
            generation: Cell::new(0),
        }
    }

    /// Forget readiness and queued messages after the WebView's web process
    /// dies: the reloaded page starts from scratch and reports `editor_ready`
    /// again, which drains whatever is queued after this reset.
    pub(super) fn reset(&self) {
        self.ready.set(false);
        self.pending.borrow_mut().clear();
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    fn generation(&self) -> u64 {
        self.generation.get()
    }

    pub(super) fn queue(&self, message: HostMessage) -> Result<Option<String>, ProtocolError> {
        let script = javascript_for_host_message(&self.surface_id, &message)?;
        if self.ready.get() {
            Ok(Some(script))
        } else {
            self.pending.borrow_mut().push(message);
            Ok(None)
        }
    }

    pub(super) fn receive(&self, raw: &str) -> EditorBridgeReceive {
        let (surface_id, message) = match parse_editor_message(raw) {
            Ok(message) => message,
            Err(error) => {
                tracing::warn!(%error, "editor WebView sent an invalid bridge message");
                return EditorBridgeReceive::default();
            }
        };
        if surface_id != self.surface_id {
            tracing::warn!(
                expected = %self.surface_id,
                actual = %surface_id,
                "editor WebView bridge surface mismatch"
            );
            return EditorBridgeReceive::default();
        }

        let became_ready = matches!(&message, EditorMessage::EditorReady);
        if became_ready {
            self.ready.set(true);
            tracing::debug!(surface_id = %self.surface_id, "editor WebView bridge ready");
        }
        if !became_ready {
            return EditorBridgeReceive {
                scripts: Vec::new(),
                message: Some(message),
            };
        }

        let scripts = self
            .pending
            .borrow_mut()
            .drain(..)
            .filter_map(|message| {
                javascript_for_host_message(&self.surface_id, &message)
                    .map_err(|error| {
                        tracing::error!(%error, "failed to encode queued editor message");
                    })
                    .ok()
            })
            .collect();
        EditorBridgeReceive {
            scripts,
            message: None,
        }
    }
}

struct EditorSessionWorker {
    session: Result<EditorSession, String>,
    startup_messages: Vec<HostMessage>,
    recovery_store: Option<RecoveryStore>,
}

type SessionMutex = tokio::sync::Mutex<Option<EditorSessionWorker>>;
type SessionGuard = tokio::sync::OwnedMutexGuard<Option<EditorSessionWorker>>;

impl EditorSessionWorker {
    fn create(
        workspace_root: &Path,
        restored: EditorSessionState,
        recovery_scope: Option<String>,
    ) -> Self {
        let recovery_store = flowmux_config::paths::state_dir().and_then(|state_root| {
            let store = match recovery_scope.as_deref() {
                Some(scope) => RecoveryStore::new_scoped(state_root, workspace_root, scope),
                None => RecoveryStore::new(state_root, workspace_root),
            };
            match store {
                Ok(store) => Some(store),
                Err(error) => {
                    tracing::warn!(%error, "editor recovery store is unavailable");
                    None
                }
            }
        });
        let mut session = match &recovery_store {
            Some(store) => EditorSession::with_recovery_store(workspace_root, store.clone()),
            None => EditorSession::new(workspace_root),
        }
        .map_err(|error| {
            let error = error.to_string();
            tracing::error!(%error, "failed to initialize editor document session");
            error
        });
        let mut startup_messages = Vec::new();
        if let Ok(session) = &mut session {
            for file in restored.open_files {
                match session.restore_document(
                    &file.path,
                    EditorViewState {
                        cursor_line: file.cursor_line,
                        cursor_column: file.cursor_column,
                        scroll_top: file.scroll_top,
                    },
                ) {
                    Ok(messages) => {
                        startup_messages.extend(messages.into_iter().filter(|message| {
                            matches!(message, HostMessage::RecoveryAvailable { .. })
                        }))
                    }
                    Err(error) => {
                        tracing::warn!(path = %file.path.display(), %error, "skipping unavailable restored editor document");
                    }
                }
            }
            if let Some(active_file) = restored.active_file {
                session.activate_path(active_file);
            }
        }
        Self {
            session,
            startup_messages,
            recovery_store,
        }
    }
}

pub(super) struct EditorHostState {
    workspace_root: PathBuf,
    // One FIFO lock covers both document mutations and I/O. Only a worker
    // owns the guard while touching the filesystem; GTK reads the last snapshot.
    // ponytail: a slow file delays this editor's other documents; use per-document
    // queues if they need independent progress within the same editor surface.
    session: Arc<SessionMutex>,
    // Inputs for the lazily created worker; consumed by the first lock holder.
    creation: RefCell<Option<(EditorSessionState, Option<String>)>>,
    // Held from construction until `initialize_messages` runs, so a caller
    // that opens a file right after the pane is created cannot overtake the
    // page's InitializeEditor message.
    initialization_guard: RefCell<Option<SessionGuard>>,
    snapshot: RefCell<EditorSessionState>,
    dirty_paths: RefCell<Vec<PathBuf>>,
    recovery_operations: RefCell<Vec<RecoveryOperation>>,
    polling: Cell<bool>,
    poll_after_fs_event: Cell<bool>,
    zoom_percent: Cell<u16>,
    startup_messages: RefCell<Vec<HostMessage>>,
    recovery_sender: RefCell<Option<RecoverySender>>,
    recovery_worker: RefCell<Option<JoinHandle<()>>>,
    pending_recovery: RefCell<HashMap<PathBuf, RecoveryOperation>>,
    recovery_flush_pending: Cell<bool>,
    search_worker: RefCell<Option<SearchWorker>>,
    next_flush_request: Cell<u64>,
    pending_flushes: RefCell<HashMap<u64, FlushCompletion>>,
}

impl EditorHostState {
    #[cfg(test)]
    pub(super) fn new(workspace_root: &Path, restored: EditorSessionState) -> Self {
        Self::create(workspace_root, restored, None)
    }

    pub(super) fn new_scoped(
        workspace_root: &Path,
        restored: EditorSessionState,
        surface_id: SurfaceId,
    ) -> Self {
        Self::create(workspace_root, restored, Some(surface_id.0.to_string()))
    }

    fn create(
        workspace_root: &Path,
        restored: EditorSessionState,
        recovery_scope: Option<String>,
    ) -> Self {
        let zoom_percent = restored
            .zoom_percent
            .map(clamp_editor_zoom)
            .unwrap_or_else(load_last_editor_zoom);
        let mut snapshot = restored.clone();
        snapshot.zoom_percent = Some(zoom_percent);
        let session: Arc<SessionMutex> = Arc::new(tokio::sync::Mutex::new(None));
        let initialization_guard = session
            .clone()
            .try_lock_owned()
            .expect("fresh editor session mutex is unlocked");
        Self {
            workspace_root: workspace_root.to_path_buf(),
            session,
            creation: RefCell::new(Some((restored, recovery_scope))),
            initialization_guard: RefCell::new(Some(initialization_guard)),
            snapshot: RefCell::new(snapshot),
            dirty_paths: RefCell::new(Vec::new()),
            recovery_operations: RefCell::new(Vec::new()),
            polling: Cell::new(false),
            poll_after_fs_event: Cell::new(false),
            zoom_percent: Cell::new(zoom_percent),
            startup_messages: RefCell::new(Vec::new()),
            recovery_sender: RefCell::new(None),
            recovery_worker: RefCell::new(None),
            pending_recovery: RefCell::new(HashMap::new()),
            recovery_flush_pending: Cell::new(false),
            search_worker: RefCell::new(None),
            next_flush_request: Cell::new(0),
            pending_flushes: RefCell::new(HashMap::new()),
        }
    }

    async fn with_session<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Result<EditorSession, String>) -> T + Send + 'static,
    ) -> Result<T, String> {
        let guard = self.session.clone().lock_owned().await;
        self.with_session_guard(guard, operation).await
    }

    async fn with_session_guard<T: Send + 'static>(
        &self,
        mut guard: SessionGuard,
        operation: impl FnOnce(&mut Result<EditorSession, String>) -> T + Send + 'static,
    ) -> Result<T, String> {
        let root = self.workspace_root.clone();
        let creation = if guard.is_none() {
            self.creation.borrow_mut().take()
        } else {
            None
        };
        let (guard, result, snapshot, dirty, operations, startup, recovery_store) =
            gtk::gio::spawn_blocking(move || {
                let worker = guard.get_or_insert_with(|| {
                    let (restored, scope) = creation.unwrap_or_default();
                    EditorSessionWorker::create(&root, restored, scope)
                });
                let result = operation(&mut worker.session);
                let (snapshot, dirty, operations) = match &mut worker.session {
                    Ok(session) => (
                        Some(session.session_snapshot()),
                        session.dirty_document_paths(),
                        session.take_recovery_operations(),
                    ),
                    Err(_) => (None, Vec::new(), Vec::new()),
                };
                let startup = std::mem::take(&mut worker.startup_messages);
                let recovery_store = worker.recovery_store.take();
                (
                    guard,
                    result,
                    snapshot,
                    dirty,
                    operations,
                    startup,
                    recovery_store,
                )
            })
            .await
            .map_err(|_| "Editor file worker stopped unexpectedly.".to_string())?;
        if let Some(snapshot) = snapshot {
            *self.snapshot.borrow_mut() = core_session_state(snapshot, self.zoom_percent.get());
        }
        *self.dirty_paths.borrow_mut() = dirty;
        self.recovery_operations.borrow_mut().extend(operations);
        self.startup_messages.borrow_mut().extend(startup);
        if let Some((sender, worker)) = recovery_store.and_then(start_recovery_worker) {
            *self.recovery_sender.borrow_mut() = Some(sender);
            *self.recovery_worker.borrow_mut() = Some(worker);
        }
        // Publish the snapshot before the next ordered operation can complete.
        drop(guard);
        Ok(result)
    }

    fn empty_initialize_message(zoom: u16) -> HostMessage {
        HostMessage::InitializeEditor {
            documents: Vec::new(),
            active_document_id: None,
            zoom_percent: zoom,
            max_document_bytes: flowmux_editor::DEFAULT_MAX_DOCUMENT_BYTES,
        }
    }

    pub(super) async fn initialize_messages(&self) -> Vec<HostMessage> {
        let zoom = self.zoom_percent.get();
        // Reuse the guard taken at construction so this runs before any
        // open/edit queued by callers between `new` and the first poll here.
        let taken = self.initialization_guard.borrow_mut().take();
        let guard = match taken {
            Some(guard) => guard,
            None => self.session.clone().lock_owned().await,
        };
        let messages = self
            .with_session_guard(guard, move |session| match session {
                Ok(session) => session.initialize_messages(zoom),
                Err(_) => vec![Self::empty_initialize_message(zoom)],
            })
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error);
                vec![Self::empty_initialize_message(zoom)]
            });
        self.stage_recovery_operations();
        messages
    }

    pub(super) fn take_startup_messages(&self) -> Vec<HostMessage> {
        std::mem::take(&mut *self.startup_messages.borrow_mut())
    }

    /// Everything a freshly reloaded page needs after a web-process crash:
    /// the full document set plus any still-undecided recovery proposals.
    /// A session that never came up still gets an empty InitializeEditor so
    /// the page leaves its pre-init state.
    pub(super) async fn reinitialize_messages(&self) -> Vec<HostMessage> {
        let zoom = self.zoom_percent.get();
        self.with_session(move |session| match session {
            Ok(session) => {
                let mut messages = session.initialize_messages(zoom);
                messages.extend(session.pending_recovery_messages());
                messages
            }
            Err(_) => vec![Self::empty_initialize_message(zoom)],
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error);
            vec![Self::empty_initialize_message(zoom)]
        })
    }

    pub(super) fn session_state(&self) -> EditorSessionState {
        let mut snapshot = self.snapshot.borrow().clone();
        snapshot.zoom_percent = Some(self.zoom_percent.get());
        snapshot
    }

    pub(super) fn zoom_factor(&self) -> f64 {
        self.zoom_percent.get() as f64 / 100.0
    }

    fn set_zoom_percent(&self, zoom_percent: u16) {
        self.zoom_percent.set(clamp_editor_zoom(zoom_percent));
        if let Err(error) = save_last_editor_zoom(zoom_percent) {
            tracing::warn!(%error, "failed to persist the editor zoom default");
        }
    }

    pub(super) async fn open_document(&self, path: &Path) -> Result<Vec<HostMessage>, String> {
        let path = path.to_path_buf();
        let result = self
            .with_session(move |session| match session {
                Ok(session) => session
                    .open_document(path)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.clone()),
            })
            .await?;
        self.stage_recovery_operations();
        result
    }

    pub(super) fn dirty_document_paths(&self) -> Vec<PathBuf> {
        self.dirty_paths.borrow().clone()
    }

    pub(super) fn start_flush(&self) -> (u64, FlushCompletion, HostMessage) {
        let request_id = self.next_flush_request.get().wrapping_add(1);
        self.next_flush_request.set(request_id);
        let completion = Rc::new(RefCell::new(None));
        self.pending_flushes
            .borrow_mut()
            .insert(request_id, completion.clone());
        (
            request_id,
            completion,
            HostMessage::FlushChanges { request_id },
        )
    }

    fn finish_flush(&self, request_id: u64, error: Option<String>) {
        if let Some(completion) = self.pending_flushes.borrow_mut().remove(&request_id) {
            *completion.borrow_mut() = Some(error.map_or(Ok(()), Err));
        }
    }

    pub(super) fn cancel_flush(&self, request_id: u64) {
        self.pending_flushes.borrow_mut().remove(&request_id);
    }

    pub(super) async fn wait_for_flush(
        &self,
        request_id: u64,
        completion: FlushCompletion,
    ) -> Result<(), String> {
        let attempts = FLUSH_TIMEOUT.as_millis() / FLUSH_POLL_INTERVAL.as_millis();
        for _ in 0..attempts {
            if let Some(result) = completion.borrow_mut().take() {
                return result;
            }
            gtk::glib::timeout_future(FLUSH_POLL_INTERVAL).await;
        }
        self.pending_flushes.borrow_mut().remove(&request_id);
        Err("Timed out while synchronizing editor changes.".into())
    }

    pub(super) async fn save_all_dirty(&self) -> (Vec<HostMessage>, Result<(), String>) {
        let result = self
            .with_session(|session| match session {
                Ok(session) => {
                    let (messages, result) = session.save_all_dirty();
                    (messages, result.map_err(|error| error.to_string()))
                }
                Err(error) => (Vec::new(), Err(error.clone())),
            })
            .await
            .unwrap_or_else(|error| (Vec::new(), Err(error)));
        self.stage_recovery_operations();
        result
    }

    pub(super) async fn discard_all_dirty(&self) -> Result<(), String> {
        self.with_session(|session| {
            if let Ok(session) = session {
                session.discard_all_dirty();
            }
        })
        .await?;
        self.stage_recovery_operations();
        Ok(())
    }

    async fn handle(&self, message: EditorMessage) -> Vec<HostMessage> {
        match message {
            EditorMessage::QuickOpenRequested { request_id } => self.start_quick_open(request_id),
            EditorMessage::WorkspaceSearchRequested {
                request_id,
                query,
                options,
            } => {
                self.start_workspace_search(request_id, query, options)
                    .await
            }
            EditorMessage::SearchCancelled { request_id } => {
                self.cancel_search(&request_id);
                Vec::new()
            }
            EditorMessage::SearchResultOpenRequested {
                path,
                line,
                column,
                length,
            } => self.open_search_result(path, line, column, length).await,
            message => self.handle_session_message(message).await,
        }
    }

    async fn handle_session_message(&self, message: EditorMessage) -> Vec<HostMessage> {
        self.with_session(move |session| match session {
            Ok(session) => session
                .handle_editor_message(message)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.clone()),
        })
        .await
        .and_then(|result| result)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "editor document message was rejected");
            Vec::new()
        })
    }

    fn start_quick_open(&self, request_id: String) -> Vec<HostMessage> {
        self.cancel_current_search();
        let root = self.workspace_root.clone();
        let cancellation = SearchCancellation::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let worker_request_id = request_id.clone();
        let spawned = std::thread::Builder::new()
            .name("flowmux-editor-quick-open".into())
            .spawn(move || {
                let mut paths = index_workspace_files(
                    &root,
                    QUICK_OPEN_LIMIT.saturating_add(1),
                    &worker_cancellation,
                );
                let truncated = paths.len() > QUICK_OPEN_LIMIT;
                paths.truncate(QUICK_OPEN_LIMIT);
                let paths = paths
                    .into_iter()
                    .filter_map(|path| path.strip_prefix(&root).ok()?.to_str().map(str::to_string))
                    .collect();
                let _ = sender.send(SearchWorkerMessage::QuickOpen {
                    request_id: worker_request_id,
                    paths,
                    truncated,
                });
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "failed to start editor quick open worker");
            return vec![HostMessage::QuickOpenCompleted {
                request_id,
                paths: Vec::new(),
                truncated: false,
            }];
        }
        *self.search_worker.borrow_mut() = Some(SearchWorker {
            request_id,
            kind: SearchWorkerKind::QuickOpen,
            cancellation,
            receiver,
        });
        Vec::new()
    }

    async fn start_workspace_search(
        &self,
        request_id: String,
        query: String,
        options: SearchOptions,
    ) -> Vec<HostMessage> {
        self.cancel_current_search();
        let cancellation = SearchCancellation::default();
        let worker_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        *self.search_worker.borrow_mut() = Some(SearchWorker {
            request_id: request_id.clone(),
            kind: SearchWorkerKind::WorkspaceSearch,
            cancellation: cancellation.clone(),
            receiver,
        });
        let documents = self
            .with_session(|session| match session {
                Ok(session) => Ok(session.search_documents()),
                Err(error) => Err(error.clone()),
            })
            .await
            .and_then(|result| result);
        if cancellation.is_cancelled() {
            return Vec::new();
        }
        let documents = match documents {
            Ok(documents) => documents,
            Err(error) => {
                self.search_worker.borrow_mut().take();
                return vec![HostMessage::WorkspaceSearchCompleted {
                    request_id,
                    result: WorkspaceSearchResult::default(),
                    error: Some(error.clone()),
                }];
            }
        };
        let root = self.workspace_root.clone();
        let worker_request_id = request_id.clone();
        let spawned = std::thread::Builder::new()
            .name("flowmux-editor-search".into())
            .spawn(move || {
                let (result, error) = match search_workspace(
                    &root,
                    &query,
                    &options,
                    &documents,
                    &worker_cancellation,
                ) {
                    Ok(result) => (result, None),
                    Err(error) => (WorkspaceSearchResult::default(), Some(error.to_string())),
                };
                let _ = sender.send(SearchWorkerMessage::WorkspaceSearch {
                    request_id: worker_request_id,
                    result,
                    error,
                });
            });
        if let Err(error) = spawned {
            self.search_worker.borrow_mut().take();
            tracing::warn!(%error, "failed to start editor workspace search worker");
            return vec![HostMessage::WorkspaceSearchCompleted {
                request_id,
                result: WorkspaceSearchResult::default(),
                error: Some("Workspace search could not be started.".into()),
            }];
        }
        Vec::new()
    }

    fn cancel_search(&self, request_id: &str) {
        let matches = self
            .search_worker
            .borrow()
            .as_ref()
            .is_some_and(|worker| worker.request_id == request_id);
        if matches {
            self.cancel_current_search();
        }
    }

    fn cancel_current_search(&self) {
        if let Some(worker) = self.search_worker.borrow_mut().take() {
            worker.cancellation.cancel();
        }
    }

    async fn open_search_result(
        &self,
        relative_path: String,
        line: u32,
        column: u32,
        length: u32,
    ) -> Vec<HostMessage> {
        let path = self.workspace_root.join(relative_path);
        self.with_session(move |session| match session {
            Ok(session) => session
                .open_search_result(path, line, column, length)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.clone()),
        })
        .await
        .and_then(|result| result)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "failed to open editor workspace search result");
            Vec::new()
        })
    }

    pub(super) fn poll_search_messages(&self) -> Vec<HostMessage> {
        self.poll_search_worker_messages()
    }

    fn poll_search_worker_messages(&self) -> Vec<HostMessage> {
        let received = {
            let workers = self.search_worker.borrow();
            let Some(worker) = workers.as_ref() else {
                return Vec::new();
            };
            match worker.receiver.try_recv() {
                Ok(message) => Ok(message),
                Err(TryRecvError::Empty) => return Vec::new(),
                Err(TryRecvError::Disconnected) => Err((worker.request_id.clone(), worker.kind)),
            }
        };
        self.search_worker.borrow_mut().take();
        match received {
            Ok(SearchWorkerMessage::QuickOpen {
                request_id,
                paths,
                truncated,
            }) => vec![HostMessage::QuickOpenCompleted {
                request_id,
                paths,
                truncated,
            }],
            Ok(SearchWorkerMessage::WorkspaceSearch {
                request_id,
                result,
                error,
            }) => vec![HostMessage::WorkspaceSearchCompleted {
                request_id,
                result,
                error,
            }],
            Err((request_id, SearchWorkerKind::QuickOpen)) => {
                tracing::warn!("editor quick open worker stopped unexpectedly");
                vec![HostMessage::QuickOpenCompleted {
                    request_id,
                    paths: Vec::new(),
                    truncated: false,
                }]
            }
            Err((request_id, SearchWorkerKind::WorkspaceSearch)) => {
                tracing::warn!("editor workspace search worker stopped unexpectedly");
                vec![HostMessage::WorkspaceSearchCompleted {
                    request_id,
                    result: WorkspaceSearchResult::default(),
                    error: Some("Workspace search stopped unexpectedly.".into()),
                }]
            }
        }
    }

    fn stage_recovery_operations(&self) -> bool {
        let operations = std::mem::take(&mut *self.recovery_operations.borrow_mut());
        if operations.is_empty() {
            return false;
        }

        let sender = self.recovery_sender.borrow();
        let Some(sender) = sender.as_ref() else {
            return false;
        };
        let mut pending = self.pending_recovery.borrow_mut();
        let mut has_write = false;
        for operation in operations {
            let path = operation.identity_path().to_path_buf();
            match operation {
                RecoveryOperation::Write(_) => {
                    pending.insert(path, operation);
                    has_write = true;
                }
                RecoveryOperation::Remove(_) => {
                    pending.remove(&path);
                    if sender.send(operation).is_err() {
                        tracing::warn!("editor recovery worker stopped unexpectedly");
                    }
                }
            }
        }
        has_write
    }

    fn flush_recovery(&self) {
        let sender = self.recovery_sender.borrow();
        let Some(sender) = sender.as_ref() else {
            return;
        };
        for operation in self
            .pending_recovery
            .borrow_mut()
            .drain()
            .map(|(_, value)| value)
        {
            if sender.send(operation).is_err() {
                tracing::warn!("editor recovery worker stopped unexpectedly");
                break;
            }
        }
    }

    pub(super) async fn poll_external_changes(&self) -> Vec<HostMessage> {
        self.poll_external_changes_inner(false).await
    }

    pub(super) async fn poll_external_changes_after_fs_event(&self) -> Vec<HostMessage> {
        self.poll_external_changes_inner(true).await
    }

    async fn poll_external_changes_inner(&self, after_fs_event: bool) -> Vec<HostMessage> {
        if after_fs_event {
            self.poll_after_fs_event.set(true);
        }
        if self.polling.replace(true) {
            return Vec::new();
        }
        let mut messages = Vec::new();
        // Coalesce file-monitor events while I/O is pending, without building
        // an unbounded queue behind a stalled filesystem.
        for _ in 0..2 {
            let after_fs_event = self.poll_after_fs_event.replace(false);
            let result = self
                .with_session(move |session| match session {
                    Ok(session) => {
                        let result = if after_fs_event {
                            session.poll_external_changes_after_fs_event()
                        } else {
                            session.poll_external_changes()
                        };
                        result.map_err(|error| error.to_string())
                    }
                    Err(error) => Err(error.clone()),
                })
                .await
                .and_then(|result| result);
            match result {
                Ok(received) => messages.extend(received),
                Err(error) => tracing::warn!(%error, "failed to inspect open editor documents"),
            }
            if !self.poll_after_fs_event.get() {
                break;
            }
        }
        self.polling.set(false);
        messages
    }
}

impl Drop for EditorHostState {
    fn drop(&mut self) {
        // The throttled flush timer holds only a weak reference; without this
        // final flush any recovery snapshot staged in the last few hundred
        // milliseconds would be lost when the surface is torn down (pane
        // close, workspace rerender, app quit).
        self.stage_recovery_operations();
        self.flush_recovery();
        self.recovery_sender.get_mut().take();
        if let Some(worker) = self.recovery_worker.get_mut().take() {
            if worker.join().is_err() {
                tracing::warn!("editor recovery worker panicked during shutdown");
            }
        }
        if let Some(worker) = self.search_worker.get_mut().take() {
            worker.cancellation.cancel();
        }
    }
}

pub(super) fn queue_host_messages(
    bridge: &EditorBridgeState,
    messages: Vec<HostMessage>,
) -> Vec<String> {
    messages
        .into_iter()
        .filter_map(|message| {
            bridge
                .queue(message)
                .map_err(|error| {
                    tracing::error!(%error, "failed to encode editor response");
                })
                .ok()
                .flatten()
        })
        .collect()
}

pub(super) async fn handle_bridge_message(
    bridge: &EditorBridgeState,
    host: &Rc<EditorHostState>,
    raw: &str,
) -> EditorBridgeDispatch {
    let received = bridge.receive(raw);
    let mut scripts = received.scripts;
    let mut focus_direction = None;
    let mut native_edit_action = None;
    let mut native_edit_text = None;
    let mut zoom_percent = None;
    if let Some(message) = received.message {
        match message {
            EditorMessage::ZoomChanged {
                zoom_percent: changed,
            } => {
                host.set_zoom_percent(changed);
                zoom_percent = Some(changed);
            }
            EditorMessage::FocusDirectionRequested { direction } => {
                focus_direction = Some(direction);
            }
            EditorMessage::NativeEditRequested { action, text } => {
                native_edit_action = Some(action);
                native_edit_text = text;
            }
            EditorMessage::FlushCompleted { request_id, error } => {
                // The WebView ack follows its edit messages. Wait for their
                // queued file-worker operations before deciding whether close is safe.
                let synchronized = host.with_session(|_| ()).await;
                host.finish_flush(request_id, error.or_else(|| synchronized.err()));
            }
            message => {
                let generation = bridge.generation();
                let replies = host.handle(message).await;
                // The page that sent this message is gone when the bridge was
                // reset while the file worker ran. Its replies name document ids
                // the reloaded page never received; reinitialization resends
                // the whole set, so drop them instead of queueing them ahead.
                if bridge.generation() == generation {
                    scripts.extend(queue_host_messages(bridge, replies));
                } else {
                    tracing::debug!("dropping editor replies for a reloaded page");
                }
                if host.stage_recovery_operations() {
                    schedule_recovery_flush(host);
                }
            }
        }
    }
    EditorBridgeDispatch {
        scripts,
        focus_direction,
        native_edit_action,
        native_edit_text,
        zoom_percent,
    }
}

// Throttle, not debounce: the flush fires a fixed delay after the first
// pending write. A debounce that re-arms per keystroke would defer the crash
// snapshot indefinitely while the user types continuously.
fn schedule_recovery_flush(host: &Rc<EditorHostState>) {
    if host.recovery_flush_pending.replace(true) {
        return;
    }
    let host = Rc::downgrade(host);
    gtk::glib::timeout_add_local_once(RECOVERY_DEBOUNCE, move || {
        let Some(host) = host.upgrade() else {
            return;
        };
        host.recovery_flush_pending.set(false);
        host.flush_recovery();
    });
}

struct RecoverySender {
    pending: Arc<Mutex<HashMap<PathBuf, RecoveryOperation>>>,
    wake: SyncSender<()>,
}

impl RecoverySender {
    fn send(&self, operation: RecoveryOperation) -> Result<(), TrySendError<()>> {
        self.pending
            .lock()
            .unwrap()
            .insert(operation.identity_path().to_path_buf(), operation);
        match self.wake.try_send(()) {
            Err(TrySendError::Full(())) => Ok(()),
            result => result,
        }
    }
}

fn start_recovery_worker(store: RecoveryStore) -> Option<(RecoverySender, JoinHandle<()>)> {
    // Only the latest operation for each document waits behind slow disk I/O.
    // A bounded wake-up channel never blocks GTK or retains document copies.
    let pending = Arc::new(Mutex::new(HashMap::new()));
    let (wake, receiver) = mpsc::sync_channel(1);
    let sender = RecoverySender {
        pending: pending.clone(),
        wake,
    };
    let result = std::thread::Builder::new()
        .name("flowmux-editor-recovery".into())
        .spawn(move || {
            for () in receiver {
                let operations = std::mem::take(&mut *pending.lock().unwrap());
                for operation in operations.into_values() {
                    if let Err(error) = store.apply(&operation) {
                        tracing::warn!(%error, "failed to update editor recovery snapshot");
                    }
                }
            }
        });
    match result {
        Ok(worker) => Some((sender, worker)),
        Err(error) => {
            tracing::warn!(%error, "failed to start editor recovery worker");
            None
        }
    }
}

fn core_session_state(snapshot: EditorSessionSnapshot, zoom_percent: u16) -> EditorSessionState {
    EditorSessionState {
        open_files: snapshot
            .open_files
            .into_iter()
            .map(
                |EditorFileSessionState {
                     path,
                     view:
                         EditorViewState {
                             cursor_line,
                             cursor_column,
                             scroll_top,
                         },
                 }| EditorFileState {
                    path,
                    cursor_line,
                    cursor_column,
                    scroll_top,
                },
            )
            .collect(),
        active_file: snapshot.active_file,
        zoom_percent: Some(zoom_percent),
    }
}

fn clamp_editor_zoom(zoom_percent: u16) -> u16 {
    zoom_percent.clamp(EDITOR_ZOOM_MIN, EDITOR_ZOOM_MAX)
}

fn load_last_editor_zoom() -> u16 {
    flowmux_config::paths::state_dir()
        .map(|dir| load_editor_zoom(&dir.join(LAST_EDITOR_ZOOM_FILE)))
        .unwrap_or(EDITOR_ZOOM_DEFAULT)
}

fn load_editor_zoom(path: &Path) -> u16 {
    fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .map(clamp_editor_zoom)
        .unwrap_or(EDITOR_ZOOM_DEFAULT)
}

fn save_last_editor_zoom(zoom_percent: u16) -> std::io::Result<()> {
    let Some(dir) = flowmux_config::paths::state_dir() else {
        return Ok(());
    };
    save_editor_zoom(&dir.join(LAST_EDITOR_ZOOM_FILE), zoom_percent)
}

fn save_editor_zoom(path: &Path, zoom_percent: u16) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, clamp_editor_zoom(zoom_percent).to_string())
}

pub(super) fn is_allowed_editor_navigation(url: &str, allowed_prefix: &str) -> bool {
    url.starts_with(allowed_prefix)
}

pub(super) fn should_poll_editor_documents(event: gtk::gio::FileMonitorEvent) -> bool {
    matches!(
        event,
        gtk::gio::FileMonitorEvent::Changed
            | gtk::gio::FileMonitorEvent::ChangesDoneHint
            | gtk::gio::FileMonitorEvent::AttributeChanged
            | gtk::gio::FileMonitorEvent::Created
            | gtk::gio::FileMonitorEvent::Deleted
            | gtk::gio::FileMonitorEvent::Moved
            | gtk::gio::FileMonitorEvent::Renamed
            | gtk::gio::FileMonitorEvent::MovedIn
            | gtk::gio::FileMonitorEvent::MovedOut
    )
}

#[cfg(target_os = "linux")]
#[path = "editor_pane_webkit.rs"]
mod imp;

#[cfg(target_os = "macos")]
#[path = "editor_pane_macos.rs"]
mod imp;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[path = "editor_pane_stub.rs"]
mod imp;

pub use imp::*;

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::SurfaceId;
    use std::fs;

    #[cfg(target_os = "macos")]
    #[test]
    fn editor_navigation_keys_map_to_monaco_commands() {
        let cases = [
            (EditorNavigationKey::Left, "cursorLeft", "cursorLeftSelect"),
            (
                EditorNavigationKey::Right,
                "cursorRight",
                "cursorRightSelect",
            ),
            (EditorNavigationKey::Up, "cursorUp", "cursorUpSelect"),
            (EditorNavigationKey::Down, "cursorDown", "cursorDownSelect"),
            (
                EditorNavigationKey::PageUp,
                "cursorPageUp",
                "cursorPageUpSelect",
            ),
            (
                EditorNavigationKey::PageDown,
                "cursorPageDown",
                "cursorPageDownSelect",
            ),
            (EditorNavigationKey::Home, "cursorHome", "cursorHomeSelect"),
            (EditorNavigationKey::End, "cursorEnd", "cursorEndSelect"),
        ];

        for (key, plain, selecting) in cases {
            assert_eq!(key.monaco_action(false), plain);
            assert_eq!(key.monaco_action(true), selecting);
        }
    }

    fn wait_for_search(host: &EditorHostState) -> Vec<HostMessage> {
        for _ in 0..100 {
            let messages = host.poll_search_messages();
            if !messages.is_empty() {
                return messages;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("editor search worker did not complete");
    }

    #[test]
    fn navigation_is_limited_to_exact_token_prefix() {
        let allowed = "http://127.0.0.1:43125/token/";
        assert!(is_allowed_editor_navigation(
            "http://127.0.0.1:43125/token/index.html?surface=abc",
            allowed
        ));
        assert!(!is_allowed_editor_navigation(
            "http://127.0.0.1:43125/other/index.html",
            allowed
        ));
        assert!(!is_allowed_editor_navigation(
            "http://127.0.0.1:43125.evil/token/index.html",
            allowed
        ));
        assert!(!is_allowed_editor_navigation(
            "https://example.com",
            allowed
        ));
    }

    #[test]
    fn bridge_queues_until_matching_editor_ready() {
        let surface_id = SurfaceId::new();
        let bridge = EditorBridgeState::new(surface_id);
        assert!(bridge
            .queue(HostMessage::InitializeEditor {
                documents: Vec::new(),
                active_document_id: None,
                zoom_percent: 100,
                max_document_bytes: flowmux_editor::DEFAULT_MAX_DOCUMENT_BYTES,
            })
            .unwrap()
            .is_none());

        assert!(bridge
            .receive(r#"{"protocolVersion":1,"surfaceId":"wrong","type":"editor_ready"}"#)
            .scripts
            .is_empty());
        let ready = format!(
            r#"{{"protocolVersion":1,"surfaceId":"{}","type":"editor_ready"}}"#,
            surface_id.0
        );
        let received = bridge.receive(&ready);
        assert_eq!(received.scripts.len(), 1);
        assert!(received.scripts[0].contains("initialize_editor"));
    }

    #[test]
    fn file_monitor_filters_events_that_can_change_document_state() {
        assert!(should_poll_editor_documents(
            gtk::gio::FileMonitorEvent::ChangesDoneHint
        ));
        assert!(should_poll_editor_documents(
            gtk::gio::FileMonitorEvent::Deleted
        ));
        assert!(!should_poll_editor_documents(
            gtk::gio::FileMonitorEvent::PreUnmount
        ));
    }

    #[tokio::test]
    async fn focus_navigation_message_bypasses_document_session() {
        let workspace = tempfile::tempdir().unwrap();
        let host = Rc::new(EditorHostState::new(
            workspace.path(),
            EditorSessionState::default(),
        ));
        let bridge = EditorBridgeState::new(SurfaceId::new());
        let raw = serde_json::json!({
            "protocolVersion": flowmux_editor::PROTOCOL_VERSION,
            "surfaceId": bridge.surface_id,
            "type": "focus_direction_requested",
            "direction": "down"
        })
        .to_string();

        let dispatch = handle_bridge_message(&bridge, &host, &raw).await;

        assert_eq!(dispatch.focus_direction, Some(EditorFocusDirection::Down));
        assert!(dispatch.scripts.is_empty());
    }

    #[tokio::test]
    async fn initialization_precedes_an_open_already_waiting_on_the_session() {
        use std::future::Future;
        use std::task::Poll;

        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("ordered.txt");
        fs::write(&path, "original").unwrap();
        let host = EditorHostState::new(workspace.path(), EditorSessionState::default());
        let mut open = std::pin::pin!(host.open_document(&path));
        std::future::poll_fn(|cx| {
            assert!(open.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let initialized = host.initialize_messages().await;
        assert!(matches!(initialized.as_slice(),
            [HostMessage::InitializeEditor { documents, .. }] if documents.is_empty()));
        let opened = tokio::time::timeout(Duration::from_secs(2), open)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            opened.first(),
            Some(HostMessage::OpenDocument { .. })
        ));
        assert_eq!(host.session_state().active_file.as_ref(), Some(&path));
    }

    #[tokio::test]
    async fn unavailable_session_still_initializes_after_a_crash() {
        let workspace = tempfile::tempdir().unwrap();
        let host = EditorHostState::new(
            &workspace.path().join("missing"),
            EditorSessionState {
                zoom_percent: Some(140),
                ..Default::default()
            },
        );
        let initial = host.initialize_messages().await;
        assert_eq!(
            initial,
            vec![EditorHostState::empty_initialize_message(140)]
        );
        assert_eq!(host.reinitialize_messages().await, initial);
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    async fn crash_drops_waiting_replies_but_reinitializes_with_the_applied_edit() {
        use std::future::Future;
        use std::task::Poll;

        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("crash.txt");
        fs::write(&path, "original").unwrap();
        let host = Rc::new(EditorHostState::new(
            workspace.path(),
            EditorSessionState::default(),
        ));
        host.initialize_messages().await;
        let opened = host.open_document(&path).await.unwrap();
        let HostMessage::OpenDocument { document } = &opened[0] else {
            panic!("missing document")
        };
        let bridge = EditorBridgeState::new(SurfaceId::new());
        let raw = serde_json::json!({
            "protocolVersion": flowmux_editor::PROTOCOL_VERSION,
            "surfaceId": bridge.surface_id,
            "type": "document_changed",
            "documentId": document.id,
            "documentVersion": document.version,
            "changeSequence": 1,
            "content": "unsaved before crash"
        })
        .to_string();
        let guard = host.session.clone().lock_owned().await;
        let mut dispatch = std::pin::pin!(handle_bridge_message(&bridge, &host, &raw));
        std::future::poll_fn(|cx| {
            assert!(dispatch.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        bridge.reset();
        drop(guard);
        assert!(dispatch.await.scripts.is_empty());
        assert!(bridge.pending.borrow().is_empty());
        let restored = host.reinitialize_messages().await;
        assert!(
            matches!(&restored[0], HostMessage::InitializeEditor { documents, .. }
            if documents[0].content == "unsaved before crash" && documents[0].dirty)
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "original");
    }

    #[tokio::test]
    async fn flush_completion_releases_the_native_close_waiter() {
        let workspace = tempfile::tempdir().unwrap();
        let host = Rc::new(EditorHostState::new(
            workspace.path(),
            EditorSessionState::default(),
        ));
        host.initialize_messages().await;
        let bridge = EditorBridgeState::new(SurfaceId::new());
        let (request_id, completion, message) = host.start_flush();
        assert_eq!(message, HostMessage::FlushChanges { request_id });
        let raw = serde_json::json!({
            "protocolVersion": flowmux_editor::PROTOCOL_VERSION,
            "surfaceId": bridge.surface_id,
            "type": "flush_completed",
            "requestId": request_id,
        })
        .to_string();

        handle_bridge_message(&bridge, &host, &raw).await;

        assert_eq!(*completion.borrow(), Some(Ok(())));

        let (request_id, completion, _) = host.start_flush();
        let raw = serde_json::json!({
            "protocolVersion": flowmux_editor::PROTOCOL_VERSION,
            "surfaceId": bridge.surface_id,
            "type": "flush_completed",
            "requestId": request_id,
            "error": "Document exceeds the editing limit.",
        })
        .to_string();
        handle_bridge_message(&bridge, &host, &raw).await;
        assert_eq!(
            *completion.borrow(),
            Some(Err("Document exceeds the editing limit.".into()))
        );
    }

    #[tokio::test]
    async fn native_copy_message_preserves_the_monaco_selection() {
        let workspace = tempfile::tempdir().unwrap();
        let host = Rc::new(EditorHostState::new(
            workspace.path(),
            EditorSessionState::default(),
        ));
        let bridge = EditorBridgeState::new(SurfaceId::new());
        let raw = serde_json::json!({
            "protocolVersion": flowmux_editor::PROTOCOL_VERSION,
            "surfaceId": bridge.surface_id,
            "type": "native_edit_requested",
            "action": "copy",
            "text": "선택한 text",
        })
        .to_string();

        let dispatch = handle_bridge_message(&bridge, &host, &raw).await;

        assert_eq!(
            dispatch.native_edit_action,
            Some(EditorNativeEditAction::Copy)
        );
        assert_eq!(dispatch.native_edit_text.as_deref(), Some("선택한 text"));
        assert!(dispatch.scripts.is_empty());
    }

    #[test]
    fn editor_zoom_persists_as_the_next_editor_default() {
        let state = tempfile::tempdir().unwrap();
        let path = state.path().join(LAST_EDITOR_ZOOM_FILE);

        save_editor_zoom(&path, 140).unwrap();
        assert_eq!(load_editor_zoom(&path), 140);
        fs::write(&path, "invalid").unwrap();
        assert_eq!(load_editor_zoom(&path), EDITOR_ZOOM_DEFAULT);
    }

    #[test]
    fn restored_editors_keep_independent_zoom_values() {
        let workspace = tempfile::tempdir().unwrap();
        let first = EditorHostState::new(
            workspace.path(),
            EditorSessionState {
                zoom_percent: Some(80),
                ..Default::default()
            },
        );
        let second = EditorHostState::new(
            workspace.path(),
            EditorSessionState {
                zoom_percent: Some(150),
                ..Default::default()
            },
        );

        assert_eq!(first.session_state().zoom_percent, Some(80));
        assert_eq!(second.session_state().zoom_percent, Some(150));
    }

    #[test]
    fn recovery_sender_coalesces_stalled_writes_and_preserves_removals() {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (wake, receiver) = mpsc::sync_channel(1);
        let sender = RecoverySender {
            pending: pending.clone(),
            wake,
        };
        let first = PathBuf::from("/workspace/first.txt");
        let second = PathBuf::from("/workspace/second.txt");
        let mut snapshot = flowmux_editor::RecoverySnapshot::new(
            "workspace".into(),
            first.clone(),
            b"base",
            1,
            "unsaved".into(),
            flowmux_editor::TextEncoding::Utf8,
            flowmux_editor::LineEnding::Lf,
        );

        // The disk worker cannot receive anything until after this burst.
        for version in 1..=100 {
            snapshot.document_version = version;
            sender
                .send(RecoveryOperation::Write(snapshot.clone()))
                .unwrap();
        }
        snapshot.identity_path = second.clone();
        sender
            .send(RecoveryOperation::Write(snapshot.clone()))
            .unwrap();
        sender
            .send(RecoveryOperation::Remove(first.clone()))
            .unwrap();
        {
            let pending = pending.lock().unwrap();
            assert_eq!(
                pending.len(),
                2,
                "old document versions must not accumulate"
            );
            assert_eq!(pending[&first], RecoveryOperation::Remove(first.clone()));
            assert_eq!(pending[&second], RecoveryOperation::Write(snapshot.clone()));
        }
        // Reopening and editing a saved document supersedes its queued removal.
        sender
            .send(RecoveryOperation::Remove(second.clone()))
            .unwrap();
        snapshot.document_version = 101;
        sender
            .send(RecoveryOperation::Write(snapshot.clone()))
            .unwrap();
        assert_eq!(
            pending.lock().unwrap()[&second],
            RecoveryOperation::Write(snapshot)
        );
        assert_eq!(receiver.try_recv(), Ok(()));
        assert_eq!(receiver.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn recovery_worker_drains_queued_snapshot_before_shutdown() {
        let workspace = tempfile::tempdir().unwrap();
        let state = tempfile::tempdir().unwrap();
        let path = workspace.path().join("recovery.txt");
        let base = b"base\n";
        fs::write(&path, base).unwrap();
        let store = RecoveryStore::new(state.path(), workspace.path()).unwrap();
        let snapshot = flowmux_editor::RecoverySnapshot::new(
            store.workspace_id().to_string(),
            fs::canonicalize(&path).unwrap(),
            base,
            2,
            "unsaved\n".into(),
            flowmux_editor::TextEncoding::Utf8,
            flowmux_editor::LineEnding::Lf,
        );
        let (sender, worker) = start_recovery_worker(store.clone()).unwrap();

        sender.send(RecoveryOperation::Write(snapshot)).unwrap();
        drop(sender);
        worker.join().unwrap();

        let (recovered, _) = store
            .read(fs::canonicalize(path).unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(recovered.content, "unsaved\n");
    }

    #[tokio::test]
    async fn search_waiting_for_file_io_respects_cancellation_and_newer_requests() {
        use std::future::Future;
        use std::task::Poll;

        let workspace = tempfile::tempdir().unwrap();
        let host = EditorHostState::new(workspace.path(), EditorSessionState::default());
        host.initialize_messages().await;
        for replacement in [false, true] {
            let guard = host.session.clone().lock_owned().await;
            let mut search = std::pin::pin!(host.start_workspace_search(
                "old".into(),
                "query".into(),
                SearchOptions::default(),
            ));
            std::future::poll_fn(|cx| {
                assert!(search.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            if replacement {
                host.start_quick_open("new".into());
            } else {
                host.cancel_search("old");
            }
            drop(guard);
            assert!(search.await.is_empty());
            assert_eq!(
                host.search_worker
                    .borrow()
                    .as_ref()
                    .map(|worker| worker.request_id.as_str()),
                replacement.then_some("new")
            );
        }
    }

    #[tokio::test]
    async fn latest_workspace_search_opens_multilingual_result_at_range() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("문서-日本語🙂.txt");
        fs::write(&path, "첫 줄\n찾을 값🙂\n").unwrap();
        let host = EditorHostState::new(workspace.path(), EditorSessionState::default());
        host.initialize_messages().await;

        host.handle(EditorMessage::WorkspaceSearchRequested {
            request_id: "search-old".into(),
            query: "missing".into(),
            options: SearchOptions::default(),
        })
        .await;
        host.handle(EditorMessage::WorkspaceSearchRequested {
            request_id: "search-latest".into(),
            query: "값🙂".into(),
            options: SearchOptions::default(),
        })
        .await;

        let completion = wait_for_search(&host);
        let [HostMessage::WorkspaceSearchCompleted {
            request_id,
            result,
            error,
        }] = completion.as_slice()
        else {
            panic!("expected workspace search completion");
        };
        assert_eq!(request_id, "search-latest");
        assert!(error.is_none());
        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].path, "문서-日本語🙂.txt");

        let messages = host
            .handle(EditorMessage::SearchResultOpenRequested {
                path: result.matches[0].path.clone(),
                line: result.matches[0].line,
                column: result.matches[0].column,
                length: result.matches[0].length,
            })
            .await;
        assert!(matches!(
            messages.as_slice(),
            [
                HostMessage::OpenDocument { .. },
                HostMessage::RevealRange {
                    line: 1,
                    column: 3,
                    length: 3,
                    ..
                }
            ]
        ));
    }
}
