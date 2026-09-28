// SPDX-License-Identifier: GPL-3.0-or-later
//! Native editor surfaces. WebViews stay on the UI thread; document I/O is ordered off-thread.
use super::*;
use crate::{
    editor as domain,
    editor_open::{OpenPaths, OpenPreparer, Prepared, Ticket},
    editor_worker::{Response, Work, Worker},
};
use flowmux_core::{EditorSessionState, PaneSurface};
use flowmux_editor::{EditorMessage, HostMessage as EditorMessageOut};
use std::collections::HashSet;
#[path = "editor_close_panel.rs"]
mod close_panel;
pub(super) use close_panel::Panel as ClosePanel;
#[path = "editor_picker.rs"]
mod editor_picker;
#[path = "editor_view.rs"]
mod editor_view;
#[path = "editor_refresh.rs"]
mod refresh;
#[path = "editor_search.rs"]
pub(super) mod search;

pub(super) enum Signal {
    CloseChoice(u64, close_panel::Choice),
    Prepared(Prepared),
    Pick(PickerTarget),
    Bridge(SurfaceId, Uuid, String, String),
    Worker(SurfaceId, Uuid, Response),
    Watch(SurfaceId, Uuid, crate::editor_watch::Notice),
    Search(crate::editor_search::Response),
    SearchTick,
    SearchOpenFinished(SurfaceId, Uuid, Uuid, Value),
}
pub(super) struct PickerTarget {
    source: SurfaceId,
    workspace: WorkspaceId,
    pane: PaneId,
    root: PathBuf,
}
#[derive(Clone)]
pub(super) enum Completion {
    Ipc(ipc::Reply),
    User(HWND),
    FilesUi {
        sender: EventSender,
        owner: crate::files_model::Owner,
    },
    SearchUi {
        sender: EventSender,
        surface: SurfaceId,
        instance: Uuid,
        token: Uuid,
    },
}
impl Completion {
    fn try_send(&self, value: Value) -> Result<(), mpsc::TrySendError<Value>> {
        match self {
            Self::Ipc(reply) => reply.try_send(value),
            Self::FilesUi { sender, owner } => {
                sender.send(Event::Files(super::files::Signal::OpenFinished(
                    owner.clone(),
                    value,
                )));
                Ok(())
            }
            Self::SearchUi {
                sender,
                surface,
                instance,
                token,
            } => {
                sender.send(Event::Editor(Signal::SearchOpenFinished(
                    *surface, *instance, *token, value,
                )));
                Ok(())
            }
            Self::User(window) => {
                if let Some(error) = value.get("error").and_then(Value::as_str) {
                    report(error);
                    unsafe {
                        MessageBoxW(
                            *window,
                            wide(error).as_ptr(),
                            wide("Open file").as_ptr(),
                            MB_OK | MB_ICONWARNING,
                        );
                    }
                }
                Ok(())
            }
        }
    }
    fn cancel(&self, reason: &str) {
        // A user intentionally closing its source does not need another dialog.
        if let Self::Ipc(reply) = self {
            let _ = reply.try_send(json!({"error":reason}));
        }
    }
}
pub(super) struct PendingOpen {
    ticket: Ticket,
    source: SurfaceId,
    workspace: WorkspaceId,
    pane: PaneId,
    reply: Completion,
    files_owner: Option<crate::files_model::Owner>,
}
pub(super) struct Editor {
    pub(super) view: editor_view::View,
    worker: Worker,
    instance: Uuid,
    root: PathBuf,
    data_root: PathBuf,
    frontend_ready: bool,
    backend_ready: bool,
    initialization_failed: bool,
    sync_failed: bool,
    pub(super) ready: bool,
    state: EditorSessionState,
    documents: Value,
    dirty: Vec<PathBuf>,
    error: Option<String>,
    restore_errors: Vec<String>,
    deferred: Vec<EditorMessageOut>,
    inflight: HashSet<u64>,
    replacements: HashSet<u64>,
    pending: Option<Pending>,
    refresh: refresh::State,
    search: search::State,
}
struct Pending {
    id: u64,
    reply: Completion,
    started: Instant,
    kind: PendingKind,
    result: Value,
    error: Option<String>,
    open_loaded: bool,
    initial_open: bool,
    timed_out: bool,
    open_path: Option<PathBuf>,
    files_owner: Option<crate::files_model::Owner>,
}
enum PendingKind {
    Command,
    Flush,
    Disk,
    Open,
    SearchOpen,
}
pub(super) enum Operation {
    Tab(SurfaceId),
    Pane {
        pane: PaneId,
        surfaces: Vec<SurfaceId>,
    },
    Workspace(WorkspaceId),
    Workspaces {
        ids: Vec<WorkspaceId>,
        surfaces: Vec<SurfaceId>,
    },
    MainWindow {
        surfaces: Vec<SurfaceId>,
    },
    Window(CloseRequest),
    QuitDiscard(ipc::Reply),
    Checkpoint(Option<ipc::Reply>),
}
pub(super) struct Barrier {
    id: u64,
    waiting: HashSet<SurfaceId>,
    targets: Vec<SurfaceId>,
    started: Instant,
    operation: Operation,
    reply: Option<ipc::Reply>,
    prompt: Option<close_panel::Panel>,
    resolving: bool,
    failure: Option<String>,
}
impl Editor {
    pub(super) fn can_move(&self) -> bool {
        self.ready && self.pending.is_none() && self.inflight.is_empty()
    }
    pub(super) fn checkpoint_ready(&self) -> bool {
        self.unavailable_restore() || (self.ready && self.pending.is_none())
    }
    fn refresh_ready(&mut self) {
        self.ready = self.frontend_ready
            && self.backend_ready
            && !self.sync_failed
            && self.refresh.pending.is_none()
            && !self.search.synchronizing()
            && self.replacements.is_empty()
            && !self.pending.as_ref().is_some_and(|p| p.timed_out);
    }
    fn unavailable_restore(&self) -> bool {
        // Initialization never exposed an editable model. Keep its saved paths
        // in checkpoints, but do not let a missing root trap unrelated tabs.
        self.initialization_failed
            && !self.sync_failed
            && self.inflight.is_empty()
            && self.pending.is_none()
            && self.dirty.is_empty()
            && self.documents.as_array().is_some_and(Vec::is_empty)
            && self.error.is_some()
    }
    fn send(&self, message: &EditorMessageOut) -> anyhow::Result<()> {
        self.view
            .view
            .evaluate_script(&flowmux_editor::javascript_for_host_message(
                &self.surface_string(),
                message,
            )?)?;
        Ok(())
    }
    fn surface_string(&self) -> String {
        url::Url::parse(&self.view.url)
            .ok()
            .and_then(|u| {
                u.query_pairs()
                    .find(|(k, _)| k == "surface")
                    .map(|(_, v)| v.into_owned())
            })
            .unwrap_or_default()
    }
    fn apply_theme(&self, colors: &crate::theme::ResolvedTheme) -> anyhow::Result<()> {
        // The owned editor view permits only its authenticated initial document.
        // Color-only updates never replace documents or the view's font settings.
        let value = json!({
            "dark":colors.dark,"background":colors.background,
            "foreground":colors.foreground,"cursor":colors.cursor,
            "selectionBackground":colors.selection_background.clone().unwrap_or_else(|| format!("{}47",colors.foreground)),
            "selectionForeground":colors.selection_foreground.as_ref().unwrap_or(&colors.foreground),
        });
        self.view.view.evaluate_script(&format!(
            "window.flowmuxWindowsEditor.setTheme({})",
            serde_json::to_string(&value)?
        ))?;
        Ok(())
    }
    fn barrier(&self, id: u64, seal: bool) -> anyhow::Result<()> {
        self.view
            .view
            .evaluate_script(&format!("window.flowmuxWindowsEditor.barrier({id},{seal})"))?;
        Ok(())
    }
    fn release(&self, id: u64) {
        if self.sync_failed {
            return;
        }
        let _ = self
            .view
            .view
            .evaluate_script(&format!("window.flowmuxWindowsEditor.releaseBarrier({id})"));
    }
    pub(super) fn status(&self, id: SurfaceId) -> Value {
        let active = self
            .documents
            .as_array()
            .and_then(|docs| docs.iter().find(|d| d["active"] == true))
            .map(|d| d["id"].clone());
        json!({"id":id,"surface":id,"kind":"editor","workspace_root":self.root,
            "storage_root":self.data_root,
            "profile_path":self.data_root.join("editor-profile"),
            "recovery_root":self.data_root.join("editor-recovery"),
            "ready":self.ready,"visible":self.view.visible,"dirty":!self.dirty.is_empty(),
            "initialization_failed":self.initialization_failed,
            "synchronization_failed":self.sync_failed,
            "dirty_paths":self.dirty,"documents":self.documents,"active_document_id":active,
            "last_error":self.error,"restore_errors":self.restore_errors,"pending":self.pending.is_some(),
            "automatic_refresh":self.refresh.status(),
            "search":self.search.status(),
            "view_handle":self.view.view.hwnd().0 as usize,
            "bounds":self.view.holder.view_bounds(&self.view.view),"holder":self.view.holder.diagnostics(),
            "session":self.state})
    }
}
impl App {
    pub(super) fn editor_apply_theme(&mut self) -> anyhow::Result<()> {
        let colors = crate::theme::resolve(&self.settings.terminal);
        for editor in self.editors.values().filter(|editor| editor.frontend_ready) {
            if let Err(error) = editor.apply_theme(&colors) {
                report(&format!("editor theme delivery: {error:#}"));
            }
        }
        // A delivery failure is reported without turning a successful settings
        // disk write into a failure or rolling back its already committed state.
        Ok(())
    }
    fn editor_next(&mut self) -> u64 {
        self.editor_request += 1;
        self.editor_request
    }
    pub(super) fn add_editor_view(
        &mut self,
        id: SurfaceId,
        root: PathBuf,
        state: EditorSessionState,
    ) -> anyhow::Result<()> {
        let data_root = self
            .editor_data_root
            .clone()
            .context("background editor requires an isolated state directory")?;
        if self.editor_assets.is_none() {
            self.editor_assets = Some(crate::editor_assets::EditorAssets::start()?);
        }
        if self.editor_context.is_none() {
            let profile = data_root.join("editor-profile");
            self.editor_context = Some(WebContext::new(Some(profile)));
        }
        let instance = Uuid::new_v4();
        let sender = self.sender.clone();
        let view = editor_view::View::new(
            self.window,
            id,
            self.editor_context.as_mut().unwrap(),
            self.editor_assets.as_ref().unwrap(),
            self.background_test,
            move |origin, body| {
                sender.send(Event::Editor(Signal::Bridge(id, instance, origin, body)))
            },
        )?;
        let sender = self.sender.clone();
        let worker = Worker::start_scoped(
            root.clone(),
            state.clone(),
            data_root.clone(),
            id.to_string(),
            move |response| sender.send(Event::Editor(Signal::Worker(id, instance, response))),
        )?;
        let request = self.editor_next();
        worker.submit(request, Work::Initialize)?;
        let refresh = refresh::State::start(root.clone(), id, instance, self.sender.clone());
        self.editors.insert(
            id,
            Editor {
                view,
                worker,
                instance,
                root,
                data_root,
                frontend_ready: false,
                backend_ready: false,
                initialization_failed: false,
                sync_failed: false,
                ready: false,
                state,
                documents: json!([]),
                dirty: Vec::new(),
                error: None,
                restore_errors: Vec::new(),
                deferred: Vec::new(),
                inflight: HashSet::from([request]),
                replacements: HashSet::new(),
                pending: None,
                refresh,
                search: search::State::default(),
            },
        );
        Ok(())
    }
    pub(super) fn editor_files_operation_guard(
        &self,
        root: &std::path::Path,
        source: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.closing
                && !self.close_accepted
                && self.close_request.is_none()
                && self.editor_barrier.is_none()
                && self.pending_save.is_none()
                && !self.editor_picker_pending
                && self.editor_open_pending.is_empty(),
            "Files actions require idle editor and window operations"
        );
        let path = root.join(source);
        for editor in self.editors.values() {
            anyhow::ensure!(
                editor.ready
                    && editor.pending.is_none()
                    && editor.inflight.is_empty()
                    && editor.replacements.is_empty()
                    && editor.refresh.pending.is_none()
                    && !editor.search.pending(),
                "Files actions require all editors to be idle"
            );
            if let Some(documents) = editor.documents.as_array() {
                for document in documents {
                    if let Some(relative) = document["path"].as_str() {
                        anyhow::ensure!(!crate::editor_search::same_path(&editor.root.join(relative), &path), "Close the source editor document before copying, renaming or moving its file");
                    }
                }
            }
        }
        Ok(())
    }
    fn editor_work(&mut self, surface: SurfaceId, id: u64, work: Work) -> anyhow::Result<()> {
        if !matches!(
            &work,
            Work::Message(
                EditorMessage::DocumentDirty { .. }
                    | EditorMessage::DocumentChanged { .. }
                    | EditorMessage::ViewStateChanged { .. }
                    | EditorMessage::ActiveDocumentChanged { .. }
                    | EditorMessage::ZoomChanged { .. }
            )
        ) {
            self.files_operation_guard()?;
        }
        let editor = self
            .editors
            .get_mut(&surface)
            .context("editor tab was closed")?;
        editor.worker.submit(id, work)?;
        editor.inflight.insert(id);
        Ok(())
    }
    pub(super) fn editor_event(&mut self, event: Signal) -> anyhow::Result<()> {
        match event {
            Signal::CloseChoice(id, choice) => self.editor_close_choice(id, choice)?,
            Signal::Search(response) => self.editor_search_completed(response),
            Signal::SearchTick => self.editor_search_tick(),
            Signal::SearchOpenFinished(surface, instance, token, value) => {
                if let Some(editor) = self
                    .editors
                    .get(&surface)
                    .filter(|e| e.instance == instance)
                {
                    let token = serde_json::to_string(&token.to_string())?;
                    let error = value.get("error").cloned().unwrap_or(Value::Null);
                    editor.view.view.evaluate_script(&format!(
                        "window.flowmuxWindowsEditor.searchOpenFinished({token},{error})"
                    ))?;
                }
            }
            Signal::Watch(surface, instance, notice) => {
                self.editor_watch_notice(surface, instance, notice);
            }
            Signal::Pick(target) => {
                self.editor_run_picker(target);
            }
            Signal::Prepared(prepared) => {
                let Some(pending) = self.editor_open_pending.remove(&prepared.id) else {
                    return Ok(()); // Cancelled/expired; dropping the result releases its slot.
                };
                let completion = pending.reply.clone();
                let result = prepared
                    .result
                    .map_err(anyhow::Error::from)
                    .and_then(|paths| self.editor_finish_open(pending, paths));
                if let Err(error) = result {
                    let _ = completion.try_send(json!({"error":error.to_string()}));
                }
            }
            Signal::Bridge(surface, instance, origin, body) => {
                let Some(editor) = self
                    .editors
                    .get(&surface)
                    .filter(|e| e.instance == instance)
                else {
                    return Ok(());
                };
                anyhow::ensure!(
                    origin == editor.view.url,
                    "editor message came from an unexpected document"
                );
                anyhow::ensure!(
                    body.len() <= flowmux_editor::MAX_BRIDGE_MESSAGE_BYTES + 1024,
                    "editor bridge message too large"
                );
                let value: Value = serde_json::from_str(&body)?;
                anyhow::ensure!(
                    value["credential"] == editor.view.credential
                        && value["signal_id"] == surface.to_string(),
                    "invalid editor instance credentials"
                );
                match value["kind"].as_str() {
                    Some("theme_error") => {
                        report(&format!(
                            "editor theme application: {}",
                            value["error"].as_str().unwrap_or("unknown error")
                        ));
                    }
                    Some("search_open") => {
                        let token = value["token"]
                            .as_str()
                            .context("missing search token")?
                            .parse::<Uuid>()?;
                        let index = value["index"]
                            .as_u64()
                            .and_then(|n| usize::try_from(n).ok())
                            .context("invalid search result index")?;
                        let completion = Completion::SearchUi {
                            sender: self.sender.clone(),
                            surface,
                            instance,
                            token,
                        };
                        if let Err(error) =
                            self.editor_search_open(surface, token, index, completion.clone())
                        {
                            let _ = completion.try_send(json!({"error":error.to_string()}));
                        }
                    }
                    Some("refresh_deferred") => {
                        let id = value["id"].as_u64().context("invalid refresh id")?;
                        self.editor_refresh_deferred(surface, id);
                    }
                    Some("refresh_error") => {
                        let id = value["id"].as_u64().context("invalid refresh id")?;
                        self.editor_refresh_failed(
                            surface,
                            id,
                            value["error"]
                                .as_str()
                                .unwrap_or("automatic refresh could not be applied"),
                        );
                    }
                    Some("refresh_applied") => {
                        let id = value["id"].as_u64().context("invalid refresh id")?;
                        if let Err(error) = self.editor_refresh_applied(surface, id) {
                            self.editor_refresh_failed(surface, id, &error.to_string());
                        }
                    }
                    Some("command_result") => {
                        let id = value["id"].as_u64().context("invalid editor command id")?;
                        let editor = self.editors.get_mut(&surface).unwrap();
                        let Some(pending) = editor.pending.as_mut().filter(|p| p.id == id) else {
                            return Ok(());
                        };
                        pending.result = value["result"].clone();
                        if let Some(error) = value["error"].as_str() {
                            pending.error = Some(error.to_owned())
                        }
                        editor.barrier(id, false)?;
                    }
                    Some("barrier_error") => {
                        let id = value["id"].as_u64().context("invalid editor barrier id")?;
                        let search_error = value["error"]
                            .as_str()
                            .unwrap_or("search could not synchronize")
                            .to_owned();
                        if self.editor_search_flush(surface, id, Some(search_error)) {
                            return Ok(());
                        }
                        if self.editors[&surface]
                            .refresh
                            .pending
                            .as_ref()
                            .is_some_and(|p| p.id == id)
                        {
                            self.editor_refresh_failed(
                                surface,
                                id,
                                value["error"]
                                    .as_str()
                                    .unwrap_or("automatic refresh could not synchronize"),
                            );
                            return Ok(());
                        }
                        self.editor_barrier_error(
                            surface,
                            id,
                            value["error"]
                                .as_str()
                                .unwrap_or("editor could not synchronize"),
                        );
                    }
                    Some("editor_message") => {
                        let raw = serde_json::to_string(&value["message"])?;
                        let (message_surface, message) =
                            flowmux_editor::parse_editor_message(&raw)?;
                        anyhow::ensure!(
                            message_surface == surface.to_string(),
                            "editor surface identity does not match native view"
                        );
                        if let Err(error) = self.files_operation_guard() {
                            let reason = error.to_string();
                            let failure = match &message {
                                EditorMessage::SaveRequested {
                                    document_id,
                                    document_version,
                                    change_sequence,
                                    ..
                                } => Some(EditorMessageOut::SaveFailed {
                                    document_id: document_id.clone(),
                                    document_version: *document_version,
                                    change_sequence: *change_sequence,
                                    reason: reason.clone(),
                                    conflict: false,
                                }),
                                EditorMessage::SaveAsRequested {
                                    document_id,
                                    document_version,
                                    change_sequence,
                                    ..
                                } => Some(EditorMessageOut::SaveAsFailed {
                                    document_id: document_id.clone(),
                                    document_version: *document_version,
                                    change_sequence: *change_sequence,
                                    reason: reason.clone(),
                                    target_exists: false,
                                }),
                                EditorMessage::ConflictActionRequested {
                                    document_id,
                                    document_version,
                                    ..
                                } => Some(EditorMessageOut::ConflictActionFailed {
                                    document_id: document_id.clone(),
                                    document_version: *document_version,
                                    reason: reason.clone(),
                                }),
                                _ => None,
                            };
                            if let Some(failure) = failure {
                                let editor = self.editors.get_mut(&surface).unwrap();
                                editor.error = Some(reason);
                                editor.send(&failure)?;
                                return Ok(());
                            }
                            if matches!(
                                &message,
                                EditorMessage::RecoveryDecision { .. }
                                    | EditorMessage::CloseRequested { .. }
                                    | EditorMessage::DiscardCloseRequested { .. }
                            ) {
                                self.editors.get_mut(&surface).unwrap().error = Some(reason);
                                return Ok(());
                            }
                        }
                        match message {
                            EditorMessage::QuickOpenRequested { request_id } => {
                                self.editor_search_ui(surface, request_id, search::Query::Quick)?;
                            }
                            EditorMessage::WorkspaceSearchRequested {
                                request_id,
                                query,
                                options,
                            } => {
                                self.editor_search_ui(
                                    surface,
                                    request_id,
                                    search::Query::Workspace { query, options },
                                )?;
                            }
                            EditorMessage::SearchCancelled { request_id } => {
                                self.editor_search_cancel(
                                    surface,
                                    Some(&request_id),
                                    "search cancelled",
                                );
                            }
                            EditorMessage::SearchResultOpenRequested { .. } => {
                                anyhow::bail!(
                                    "search results require the current retained token and index"
                                );
                            }
                            EditorMessage::EditorReady => {
                                let colors = crate::theme::resolve(&self.settings.terminal);
                                if self.editors[&surface].frontend_ready {
                                    self.editors.get_mut(&surface).unwrap().frontend_ready = false;
                                    self.editor_sync_failure(surface, "editor document unexpectedly reloaded; retained backend documents must be recovered before closing");
                                    return Ok(());
                                }
                                let editor = self.editors.get_mut(&surface).unwrap();
                                anyhow::ensure!(
                                    !editor.sync_failed,
                                    "editor synchronization failed; reopening is required"
                                );
                                editor.frontend_ready = true;
                                if let Err(error) = editor.apply_theme(&colors) {
                                    report(&format!("initial editor theme delivery: {error:#}"));
                                }
                                for message in std::mem::take(&mut editor.deferred) {
                                    editor.send(&message)?
                                }
                                editor.refresh_ready();
                                if editor.pending.as_ref().is_some_and(|p| {
                                    matches!(p.kind, PendingKind::Open | PendingKind::SearchOpen)
                                        && p.open_loaded
                                }) {
                                    editor.barrier(editor.pending.as_ref().unwrap().id, false)?;
                                }
                            }
                            EditorMessage::FlushCompleted { request_id, error } => {
                                if self.editor_search_flush(surface, request_id, error.clone()) {
                                    return Ok(());
                                }
                                if self.editor_refresh_flush(surface, request_id, error.clone()) {
                                    return Ok(());
                                }
                                let invalid_files_open = self.editors[&surface]
                                    .pending
                                    .as_ref()
                                    .filter(|p| p.id == request_id && p.open_path.is_some())
                                    .and_then(|p| p.files_owner.as_ref().map(|owner| (p, owner)))
                                    .is_some_and(|(p, owner)| {
                                        !self.files_owner_current(owner)
                                            || p.started.elapsed()
                                                >= crate::editor_open::OPEN_BUDGET
                                    });
                                if invalid_files_open {
                                    self.editor_barrier_error(surface, request_id,
                                        "Files Open was cancelled or expired before document dispatch");
                                    return Ok(());
                                }
                                if let Some(error) = error {
                                    self.editor_barrier_error(surface, request_id, &error)
                                } else {
                                    let work = if let Some(pending) = self
                                        .editors
                                        .get_mut(&surface)
                                        .unwrap()
                                        .pending
                                        .as_mut()
                                        .filter(|p| p.id == request_id)
                                    {
                                        if let Some(path) = pending.open_path.take() {
                                            Work::Open(path)
                                        } else if matches!(pending.kind, PendingKind::Disk) {
                                            Work::PollDisk
                                        } else {
                                            Work::Snapshot
                                        }
                                    } else {
                                        Work::Snapshot
                                    };
                                    if self.editor_barrier.as_ref().is_some_and(|b| {
                                        b.id == request_id && b.waiting.contains(&surface)
                                    }) || self.editors[&surface]
                                        .pending
                                        .as_ref()
                                        .is_some_and(|p| p.id == request_id)
                                    {
                                        if let Err(error) =
                                            self.editor_work(surface, request_id, work)
                                        {
                                            self.editor_barrier_error(
                                                surface,
                                                request_id,
                                                &error.to_string(),
                                            )
                                        }
                                    }
                                }
                            }
                            EditorMessage::NativeEditRequested { .. } => {
                                self.editors.get_mut(&surface).unwrap().error = Some(
                                    "Native editor clipboard integration is not available yet"
                                        .into(),
                                );
                            }
                            EditorMessage::FocusDirectionRequested { direction } => {
                                if !self.background_test {
                                    let direction = match direction {
                                        flowmux_editor::EditorFocusDirection::Left => {
                                            FocusDirection::Left
                                        }
                                        flowmux_editor::EditorFocusDirection::Right => {
                                            FocusDirection::Right
                                        }
                                        flowmux_editor::EditorFocusDirection::Up => {
                                            FocusDirection::Up
                                        }
                                        flowmux_editor::EditorFocusDirection::Down => {
                                            FocusDirection::Down
                                        }
                                    };
                                    self.focus_direction(surface, direction)?;
                                }
                            }
                            message => {
                                if !matches!(
                                    &message,
                                    EditorMessage::ViewStateChanged { .. }
                                        | EditorMessage::ActiveDocumentChanged { .. }
                                        | EditorMessage::ZoomChanged { .. }
                                ) {
                                    self.editor_search_invalidate(surface);
                                }
                                self.editors.get_mut(&surface).unwrap().refresh.activity =
                                    Instant::now();
                                let id = self.editor_next();
                                let replacing = matches!(
                                    &message,
                                    EditorMessage::SaveRequested { .. }
                                        | EditorMessage::SaveAsRequested { .. }
                                        | EditorMessage::CloseRequested { .. }
                                        | EditorMessage::DiscardCloseRequested { .. }
                                        | EditorMessage::RecoveryDecision {
                                            choice: flowmux_editor::RecoveryChoice::Restore,
                                            ..
                                        }
                                        | EditorMessage::ConflictActionRequested {
                                            action: flowmux_editor::ConflictAction::KeepMine
                                                | flowmux_editor::ConflictAction::ReloadFromDisk,
                                            ..
                                        }
                                );
                                if replacing {
                                    let editor = self.editors.get_mut(&surface).unwrap();
                                    editor.replacements.insert(id);
                                    editor.refresh_ready();
                                }
                                if let Err(error) =
                                    self.editor_work(surface, id, Work::Message(message))
                                {
                                    self.editor_sync_failure(surface, &error.to_string())
                                }
                            }
                        }
                    }
                    _ => anyhow::bail!("unknown Windows editor bridge message"),
                }
            }
            Signal::Worker(surface, instance, response) => {
                let resolving_close = self.editor_barrier.as_ref().is_some_and(|barrier| {
                    barrier.id == response.id
                        && barrier.resolving
                        && barrier.waiting.contains(&surface)
                });
                let invalid_files_open = self
                    .editors
                    .get(&surface)
                    .filter(|e| e.instance == instance)
                    .and_then(|e| e.pending.as_ref())
                    .filter(|p| p.id == response.id)
                    .and_then(|p| p.files_owner.as_ref())
                    .is_some_and(|owner| !self.files_owner_current(owner));
                let Some(editor) = self
                    .editors
                    .get_mut(&surface)
                    .filter(|e| e.instance == instance)
                else {
                    return Ok(());
                };
                editor.inflight.remove(&response.id);
                if invalid_files_open {
                    // The document worker may already have applied Open. Keep
                    // every actual model response and its synchronization chain,
                    // but never acknowledge the obsolete Files request as success.
                    if let Some(pending) = &mut editor.pending {
                        pending.error = Some("Files changed while Open was in flight; review the editor state before retrying".into());
                    }
                }
                editor.backend_ready = response.ready;
                editor.initialization_failed = response.initialization_failed;
                editor.refresh_ready();
                editor.state = response.state;
                editor.dirty = response.dirty;
                editor.documents = serde_json::to_value(response.documents)?;
                editor.restore_errors = response.restored_errors;
                let automatic = editor
                    .refresh
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.id == response.id && p.phase == refresh::Phase::Working);
                let mut failure = response.error;
                let search_snapshot = response.search_snapshot;
                let search_capture = editor.search.captured(response.id);
                let late_search_open = editor.pending.as_ref().is_some_and(|p| {
                    p.id == response.id
                        && matches!(p.kind, PendingKind::SearchOpen)
                        && (p.timed_out || p.started.elapsed() >= crate::editor_open::OPEN_BUDGET)
                });
                for message in response.messages {
                    // Already admitted I/O must reconcile actual model state, but
                    // an expired result may never reveal its old search selection.
                    if late_search_open && matches!(message, EditorMessageOut::RevealRange { .. }) {
                        continue;
                    }
                    let failed = match &message {
                        EditorMessageOut::SaveFailed { reason, .. }
                        | EditorMessageOut::SaveAsFailed { reason, .. }
                        | EditorMessageOut::ConflictActionFailed { reason, .. } => {
                            Some(reason.clone())
                        }
                        _ => None,
                    };
                    if failed.is_some() {
                        failure = failed;
                    }
                    if editor.frontend_ready {
                        if automatic {
                            if let Err(error) = editor.refresh_send(response.id, &message) {
                                self.editor_refresh_failed(
                                    surface,
                                    response.id,
                                    &error.to_string(),
                                );
                                return Ok(());
                            }
                        } else if let Err(error) = editor.send(&message) {
                            if !resolving_close {
                                return Err(error);
                            }
                            // Consume the admitted worker response even if its
                            // view failed. Other saves must drain before close
                            // cancellation can release any surviving editor.
                            failure.get_or_insert_with(|| error.to_string());
                            editor.sync_failed = true;
                            editor.refresh_ready();
                            let _ = editor
                                .view
                                .view
                                .evaluate_script("window.flowmuxWindowsEditor.quarantine()");
                        }
                    } else {
                        editor.deferred.push(message)
                    }
                }
                if editor.replacements.remove(&response.id) {
                    editor
                        .view
                        .view
                        .evaluate_script("window.flowmuxWindowsEditor.replacementCompleted()")?;
                    editor.refresh_ready();
                }
                if let Some(error) = &failure {
                    editor.error = Some(error.clone());
                    if !search_capture {
                        if let Some(pending) = &mut editor.pending {
                            pending.error = Some(error.clone())
                        }
                    }
                }
                let state = editor.state.clone();
                if let Some((index, pane, _)) = self.locate(surface) {
                    self.workspaces[index]
                        .root
                        .set_surface_editor_session(pane, surface, state);
                    self.refresh_surface_metadata(surface);
                }
                if search_capture {
                    self.editor_search_snapshot(
                        surface,
                        response.id,
                        search_snapshot,
                        failure.clone(),
                    );
                    return Ok(());
                }
                if automatic {
                    if let Err(error) =
                        self.editor_refresh_worker_done(surface, response.id, failure)
                    {
                        self.editor_refresh_failed(surface, response.id, &error.to_string());
                    }
                } else if self
                    .editor_barrier
                    .as_ref()
                    .is_some_and(|b| b.id == response.id && b.waiting.contains(&surface))
                {
                    if self.editor_barrier.as_ref().unwrap().resolving {
                        let barrier = self.editor_barrier.as_mut().unwrap();
                        barrier.waiting.remove(&surface);
                        // A completion can arrive before the next timer tick.
                        if barrier.started.elapsed() > Duration::from_secs(12) {
                            barrier.failure.get_or_insert_with(|| {
                                "Editor file operation timed out; the close was cancelled".into()
                            });
                        }
                        if let Some(error) = failure {
                            barrier.failure.get_or_insert(error);
                        }
                        if barrier.waiting.is_empty() {
                            self.editor_finish_barrier()?;
                        }
                        return Ok(());
                    }
                    if let Some(error) = failure {
                        self.editor_barrier_error(surface, response.id, &error)
                    } else {
                        self.editor_barrier
                            .as_mut()
                            .unwrap()
                            .waiting
                            .remove(&surface);
                        if self.editor_barrier.as_ref().unwrap().waiting.is_empty() {
                            self.editor_finish_barrier()?;
                        }
                    }
                } else if self
                    .editors
                    .get(&surface)
                    .is_some_and(|e| e.pending.as_ref().is_some_and(|p| p.id == response.id))
                {
                    let editor = self.editors.get_mut(&surface).unwrap();
                    if editor.pending.as_ref().is_some_and(|p| {
                        matches!(p.kind, PendingKind::Open | PendingKind::SearchOpen)
                            && !p.open_loaded
                    }) {
                        let pending = editor.pending.as_mut().unwrap();
                        pending.open_loaded = true;
                        if pending.initial_open && !editor.restore_errors.is_empty() {
                            pending.error = Some(editor.restore_errors.join("; "));
                        }
                        if editor.frontend_ready {
                            editor.barrier(response.id, false)?;
                        }
                        return Ok(());
                    }
                    let mut pending = editor.pending.take().unwrap();
                    // A worker/renderer completion can be queued ahead of Tick.
                    // Enforce the original reply deadline here as well; otherwise
                    // a delayed UI loop could acknowledge expired work as timely.
                    if !pending.timed_out
                        && pending.started.elapsed() >= crate::editor_open::OPEN_BUDGET
                    {
                        pending.error = Some(
                            "editor command finished after its reply deadline; review the document state before retrying"
                                .into(),
                        );
                        editor.error = pending.error.clone();
                    }
                    editor.release(pending.id);
                    editor.refresh.activity = Instant::now();
                    editor.refresh_ready();
                    if pending.timed_out {
                        editor.error = pending.error.clone().or_else(|| Some(
                            "The previous command exceeded its reply deadline and has now finished; review the document state before retrying".into(),
                        ));
                    }
                    let result = if let Some(error) = pending.error {
                        json!({"error":error})
                    } else if matches!(pending.kind, PendingKind::Open | PendingKind::SearchOpen) {
                        pending.result
                    } else if matches!(pending.kind, PendingKind::Command) {
                        json!({"result":pending.result})
                    } else {
                        json!({"ok":true,"status":editor.status(surface)})
                    };
                    if !pending.timed_out {
                        let _ = pending.reply.try_send(result);
                    }
                }
            }
        }
        Ok(())
    }
    fn editor_sync_failure(&mut self, surface: SurfaceId, error: &str) {
        self.editor_search_cancel(surface, None, error);
        if let Some(editor) = self.editors.get_mut(&surface) {
            editor.error = Some(error.to_owned());
            editor.ready = false;
            editor.sync_failed = true;
            let _ = editor
                .view
                .view
                .evaluate_script("window.flowmuxWindowsEditor.quarantine()");
            if let Some(pending) = editor.pending.take() {
                let _ = pending.reply.try_send(json!({"error":error}));
            }
        }
        if let Some(id) = self
            .editor_barrier
            .as_ref()
            .filter(|b| b.targets.contains(&surface))
            .map(|b| b.id)
        {
            self.editor_barrier_error(surface, id, error);
        }
    }
    fn editor_barrier_error(&mut self, surface: SurfaceId, id: u64, error: &str) {
        if self.editor_barrier.as_ref().is_some_and(|b| b.id == id) {
            let barrier = self.editor_barrier.as_mut().unwrap();
            if barrier.resolving && !barrier.waiting.is_empty() {
                // An accepted filesystem operation cannot be cancelled. Keep
                // its renderer sealed until every original response arrives.
                barrier.failure.get_or_insert_with(|| error.to_owned());
                if let Some(prompt) = &barrier.prompt {
                    prompt.status("Waiting for the current file operation to finish…");
                }
                return;
            }
            let mut barrier = self.editor_barrier.take().unwrap();
            drop(barrier.prompt.take());
            for target in &barrier.targets {
                if let Some(e) = self.editors.get(target) {
                    e.release(id)
                }
            }
            self.editor_operation_error(barrier.operation, barrier.reply, error);
        } else if let Some(editor) = self.editors.get_mut(&surface) {
            if editor.pending.as_ref().is_some_and(|p| p.id == id) {
                let pending = editor.pending.take().unwrap();
                editor.search.discard_open(id);
                editor.release(id);
                editor.refresh_ready();
                let _ = pending.reply.try_send(json!({"error":error}));
            }
        }
    }
    fn editor_operation_error(
        &mut self,
        operation: Operation,
        reply: Option<ipc::Reply>,
        error: &str,
    ) {
        let quiet = matches!(&operation, Operation::Checkpoint(_));
        let owner = self.editor_close_owner(&operation);
        let reply = reply.or(match operation {
            Operation::Window(CloseRequest::Ipc(reply))
            | Operation::QuitDiscard(reply)
            | Operation::Checkpoint(Some(reply)) => Some(reply),
            _ => None,
        });
        self.state_error = Some(error.into());
        if let Some(reply) = reply {
            let _ = reply.try_send(json!({"error":error}));
        } else if !self.background_test && !quiet {
            unsafe {
                MessageBoxW(
                    owner,
                    wide(error).as_ptr(),
                    wide("flowmux editor").as_ptr(),
                    MB_OK | MB_ICONWARNING,
                );
            }
        }
    }
    pub(super) fn editor_guard(
        &mut self,
        operation: Operation,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<bool> {
        self.files_operation_guard()?;
        if self.editor_bypass {
            return Ok(false);
        }
        let mut targets: Vec<_> = match &operation {
            Operation::Tab(id) => self
                .editors
                .contains_key(id)
                .then_some(*id)
                .into_iter()
                .collect(),
            Operation::Pane { surfaces, .. }
            | Operation::MainWindow { surfaces }
            | Operation::Workspaces { surfaces, .. } => surfaces
                .iter()
                .filter(|id| self.editors.contains_key(id))
                .copied()
                .collect(),
            Operation::Workspace(id) => self
                .editors
                .keys()
                .filter(|id2| {
                    self.locate(**id2)
                        .is_some_and(|(index, _, _)| self.workspaces[index].id == *id)
                })
                .copied()
                .collect(),
            _ => self.editors.keys().copied().collect(),
        };
        targets.retain(|id| !self.editors[id].unavailable_restore());
        if targets.is_empty() {
            return Ok(false);
        }
        anyhow::ensure!(
            self.editor_barrier.is_none(),
            "editor synchronization is already in progress"
        );
        anyhow::ensure!(
            targets
                .iter()
                .all(|id| self.editors[id].ready && self.editors[id].pending.is_none()),
            "wait for editor loading or the current editor command to finish"
        );
        let id = self.editor_next();
        let seal = !matches!(operation, Operation::Checkpoint(_));
        self.editor_barrier = Some(Barrier {
            id,
            waiting: targets.iter().copied().collect(),
            targets: targets.clone(),
            started: Instant::now(),
            operation,
            reply,
            prompt: None,
            resolving: false,
            failure: None,
        });
        for surface in targets {
            if let Err(error) = self.editors[&surface].barrier(id, seal) {
                self.editor_barrier_error(surface, id, &error.to_string());
                break;
            }
        }
        Ok(true)
    }
    fn editor_finish_barrier(&mut self) -> anyhow::Result<()> {
        let mut barrier = self.editor_barrier.take().unwrap();
        if let Some(error) = barrier.failure.take() {
            drop(barrier.prompt.take());
            for id in &barrier.targets {
                if let Some(editor) = self.editors.get(id) {
                    editor.release(barrier.id);
                }
            }
            self.editor_operation_error(barrier.operation, barrier.reply, &error);
            return Ok(());
        }
        let dirty = barrier
            .targets
            .iter()
            .any(|id| self.editors.get(id).is_some_and(|e| !e.dirty.is_empty()));
        if dirty && !matches!(barrier.operation, Operation::Checkpoint(_)) {
            let native = barrier.reply.is_none()
                && !matches!(
                    barrier.operation,
                    Operation::Window(CloseRequest::Ipc(_)) | Operation::QuitDiscard(_)
                );
            if native && !barrier.resolving {
                let mut labels: Vec<_> = barrier
                    .targets
                    .iter()
                    .filter_map(|id| self.editors.get(id))
                    .flat_map(|editor| {
                        editor.dirty.iter().map(|path| {
                            path.strip_prefix(&editor.root)
                                .unwrap_or(path)
                                .to_string_lossy()
                                .into_owned()
                        })
                    })
                    .collect();
                labels.sort();
                labels.dedup();
                match close_panel::Panel::new(
                    self.editor_close_owner(&barrier.operation),
                    barrier.id,
                    &labels,
                    self.background_test,
                ) {
                    Ok(prompt) => {
                        barrier.prompt = Some(prompt);
                        self.editor_barrier = Some(barrier);
                        return Ok(());
                    }
                    Err(error) => {
                        for id in &barrier.targets {
                            if let Some(editor) = self.editors.get(id) {
                                editor.release(barrier.id);
                            }
                        }
                        self.editor_operation_error(
                            barrier.operation,
                            barrier.reply,
                            &error.to_string(),
                        );
                        return Ok(());
                    }
                }
            }
            drop(barrier.prompt.take());
            for id in &barrier.targets {
                if let Some(e) = self.editors.get(id) {
                    e.release(barrier.id)
                }
            }
            self.editor_operation_error(barrier.operation,barrier.reply,"Editor documents have unsaved changes. Save them or explicitly discard the documents in the editor before closing.");
            return Ok(());
        }
        // Destroy the owned dialog before its close action can destroy its owner.
        drop(barrier.prompt.take());
        let closing = matches!(
            &barrier.operation,
            Operation::Window(_) | Operation::QuitDiscard(_)
        ) || matches!(&barrier.operation, Operation::Tab(surface)
            if self.workspaces.len() == 1 && self.main_closed && self.detached.contains_key(surface));
        self.editor_bypass = true;
        let result = (|| -> anyhow::Result<()> {
            match barrier.operation {
                Operation::Tab(id) => {
                    self.select(id)?;
                    self.action(Action::CloseTab)?;
                }
                Operation::Pane { pane, surfaces } => {
                    anyhow::ensure!(
                        self.pane_surface_ids(pane)? == surfaces,
                        "Pane tabs changed while editor close was pending"
                    );
                    self.close_pane(pane, None)?;
                }
                Operation::Workspaces { ids, surfaces } => self.close_workspaces(ids, surfaces)?,
                Operation::Workspace(id) => {
                    self.workspace_command(WorkspaceOp::Close { workspace: id.0 }, None)?;
                }
                Operation::MainWindow { surfaces } => {
                    anyhow::ensure!(
                        self.main_surface_ids() == surfaces,
                        "Main window surfaces changed while editor close was pending"
                    );
                    self.close_main_window()?;
                }
                Operation::Window(request) => self.request_close(request)?,
                Operation::QuitDiscard(reply) => {
                    self.accept_close();
                    // The request may have expired while queued before its
                    // editor barrier started. Keep an accepted close from
                    // leaving the clean editor sealed without a live receiver.
                    self.closing |= reply.try_send(json!({"ok":true})).is_err();
                }
                Operation::Checkpoint(reply) => self.begin_save(reply)?,
            }
            Ok(())
        })();
        self.editor_bypass = false;
        // Closing windows remain sealed through the terminal checkpoint/IPC reply.
        if !closing || result.is_err() {
            for id in barrier.targets {
                if let Some(e) = self.editors.get(&id) {
                    e.release(barrier.id)
                }
            }
        }
        if let Some(reply) = barrier.reply {
            let _ = reply.try_send(match &result {
                Ok(_) => json!({"ok":true}),
                Err(error) => json!({"error":error.to_string()}),
            });
        }
        result
    }
    fn editor_close_owner(&self, operation: &Operation) -> HWND {
        match operation {
            Operation::Tab(surface) => self.surface_window(*surface),
            Operation::Window(CloseRequest::Native) => self
                .current_surface()
                .map_or(self.window, |surface| self.surface_window(surface)),
            _ => self.window,
        }
    }
    pub(super) fn editor_close_diagnostics(&self) -> Value {
        self.editor_barrier
            .as_ref()
            .and_then(|barrier| barrier.prompt.as_ref())
            .map_or(Value::Null, close_panel::Panel::diagnostics)
    }
    pub(super) fn editor_close_handle_message(&self, message: &MSG) -> bool {
        self.editor_barrier
            .as_ref()
            .and_then(|barrier| barrier.prompt.as_ref())
            .is_some_and(|prompt| prompt.handle_message(message))
    }
    fn editor_close_choice(&mut self, id: u64, choice: close_panel::Choice) -> anyhow::Result<()> {
        let Some(barrier) = self
            .editor_barrier
            .as_mut()
            .filter(|barrier| barrier.id == id && barrier.prompt.is_some() && !barrier.resolving)
        else {
            return Ok(());
        };
        if choice == close_panel::Choice::Cancel {
            let mut barrier = self.editor_barrier.take().unwrap();
            drop(barrier.prompt.take());
            for surface in &barrier.targets {
                if let Some(editor) = self.editors.get(surface) {
                    editor.release(id);
                }
            }
            return self.focus_active();
        }
        barrier.resolving = true;
        barrier.started = Instant::now();
        barrier.waiting = barrier.targets.iter().copied().collect();
        let prompt = barrier.prompt.as_ref().unwrap();
        prompt.busy();
        if choice == close_panel::Choice::Discard {
            prompt.status("Discarding…");
        }
        let targets = barrier.targets.clone();
        for surface in targets {
            let work = if choice == close_panel::Choice::Save {
                Work::SaveAll
            } else {
                Work::DiscardAll
            };
            if let Err(error) = self.editor_work(surface, id, work) {
                let barrier = self.editor_barrier.as_mut().unwrap();
                barrier.waiting.remove(&surface);
                barrier.failure.get_or_insert_with(|| error.to_string());
            }
        }
        if self.editor_barrier.as_ref().unwrap().waiting.is_empty() {
            self.editor_finish_barrier()?;
        }
        Ok(())
    }
    pub(super) fn editor_release_all(&self) {
        // releaseBarrier(0) is the host's explicit cancel-all path.
        for editor in self.editors.values() {
            editor.release(0)
        }
    }
    pub(super) fn editor_tick(&mut self) {
        let now = Instant::now();
        let expired: Vec<_> = self
            .editor_open_pending
            .iter()
            .filter(|(_, pending)| pending.ticket.is_expired(now))
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            let pending = self.editor_open_pending.remove(&id).unwrap();
            pending.ticket.cancel();
            let _ = pending.reply.try_send(json!({"error":"editor open preparation timed out; no editor was opened, and any late preparation result will be ignored"}));
        }
        if let Some((surface, id)) = self
            .editor_barrier
            .as_ref()
            .filter(|b| {
                (b.prompt.is_none() || b.resolving)
                    && b.failure.is_none()
                    && b.started.elapsed() > Duration::from_secs(12)
            })
            .and_then(|b| b.targets.first().map(|s| (*s, b.id)))
        {
            self.editor_barrier_error(
                surface,
                id,
                "editor did not acknowledge synchronization; the close/save was cancelled",
            );
        }
        let expired: Vec<_> = self
            .editors
            .iter()
            .filter_map(|(id, e)| {
                e.pending
                    .as_ref()
                    .filter(|p| {
                        !p.timed_out && p.started.elapsed() >= crate::editor_open::OPEN_BUDGET
                    })
                    .map(|p| (*id, p.id))
            })
            .collect();
        for (surface, id) in expired {
            let editor = self.editors.get_mut(&surface).unwrap();
            let pending = editor.pending.as_mut().filter(|p| p.id == id).unwrap();
            let error = "editor command timed out; its outcome is unknown, and editing remains suspended until the original operation finishes (no retry)";
            pending.timed_out = true;
            editor.ready = false;
            editor.error = Some(error.into());
            let _ = pending.reply.try_send(json!({"error":error}));
            // Keep the original callback/flush/snapshot chain alive. In particular,
            // late PollDisk/Reload responses must be applied before the model can
            // accept another edit. A timeout is not filesystem cancellation.
        }
    }
    pub(super) fn editor_remove(&mut self, id: SurfaceId) {
        self.editor_search_cancel(id, None, "editor closed during search");
        self.editor_cancel_opens(
            Some(id),
            "editor Open source closed before preparation completed",
        );
        if let Some(mut editor) = self.editors.remove(&id) {
            if let Some(pending) = editor.pending.take() {
                let _ = pending
                    .reply
                    .try_send(json!({"error":"editor tab closed during command"}));
            }
        }
    }
    pub(super) fn editor_cancel_opens(&mut self, source: Option<SurfaceId>, reason: &str) {
        let ids: Vec<_> = self
            .editor_open_pending
            .iter()
            .filter(|(_, pending)| source.is_none_or(|source| pending.source == source))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let pending = self.editor_open_pending.remove(&id).unwrap();
            pending.ticket.cancel();
            pending.reply.cancel(reason);
        }
    }
    pub(super) fn editor_cancel_files_opens(&mut self, instance: Uuid, reason: &str) {
        let ids: Vec<_> = self
            .editor_open_pending
            .iter()
            .filter(|(_, pending)| {
                pending
                    .files_owner
                    .as_ref()
                    .is_some_and(|owner| owner.instance == instance)
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let pending = self.editor_open_pending.remove(&id).unwrap();
            pending.ticket.cancel();
            pending.reply.cancel(reason);
        }
        for editor in self.editors.values_mut() {
            let Some(pending) = editor.pending.as_mut().filter(|p| {
                p.files_owner
                    .as_ref()
                    .is_some_and(|owner| owner.instance == instance)
            }) else {
                continue;
            };
            if pending.open_path.is_some() {
                // The reuse barrier has not dispatched document I/O yet.
                let pending = editor.pending.take().unwrap();
                editor.release(pending.id);
                editor.error = Some(reason.into());
                editor.refresh_ready();
                let _ = pending.reply.try_send(json!({"error":reason}));
            } else if !pending.timed_out {
                // Already submitted I/O owns its guard until model reconciliation.
                pending.error = Some(reason.into());
                pending.timed_out = true;
                editor.ready = false;
                editor.error = Some(reason.into());
                let _ = pending.reply.try_send(json!({"error":reason}));
            }
        }
    }
    pub(super) fn editor_open_from_files(
        &mut self,
        source: SurfaceId,
        owner: crate::files_model::Owner,
        root: PathBuf,
        path: PathBuf,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<()> {
        let (workspace, pane, _) = self
            .locate(source)
            .context("Files source surface disappeared before Open")?;
        anyhow::ensure!(
            self.workspaces[workspace].id.0 == owner.workspace && pane.0 == owner.pane,
            "Files source moved before Open; refresh the panel"
        );
        let completion = match reply {
            Some(reply) => Completion::Ipc(reply),
            None => Completion::FilesUi {
                sender: self.sender.clone(),
                owner: owner.clone(),
            },
        };
        self.editor_submit_open(
            domain::OpenArgs {
                path,
                root: Some(root),
                pane: None,
            },
            Some(source),
            completion,
            Some(owner),
        )
    }
    fn editor_submit_open(
        &mut self,
        args: domain::OpenArgs,
        caller: Option<SurfaceId>,
        reply: Completion,
        files_owner: Option<crate::files_model::Owner>,
    ) -> anyhow::Result<()> {
        self.files_operation_guard()?;
        let submitted = match &reply {
            Completion::Ipc(reply) => reply.received_at(),
            Completion::User(_) | Completion::FilesUi { .. } => Instant::now(),
            Completion::SearchUi { .. } => {
                anyhow::bail!("search results require retained-result validation")
            }
        };
        anyhow::ensure!(
            submitted.elapsed() < crate::editor_open::OPEN_BUDGET,
            "editor Open expired in the UI queue; no editor was opened"
        );
        anyhow::ensure!(
            self.editor_barrier.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && !self.closing,
            "editor Open cannot begin during synchronization or window close"
        );
        let source = self.target(args.pane, caller)?;
        if !self.editors.contains_key(&source) {
            self.ensure_attached(source)?;
        }
        let (index, pane, _) = self
            .locate(source)
            .context("editor source pane disappeared")?;
        anyhow::ensure!(
            self.workspaces[index].ssh.is_none(),
            "Remote file editing is not available in SSH workspaces"
        );
        let workspace = self.workspaces[index].id;
        let root = args.root.unwrap_or_else(|| {
            self.editors
                .get(&source)
                .filter(|_| self.detached.contains_key(&source))
                .map_or_else(
                    || self.workspaces[index].cwd.clone(),
                    |editor| editor.root.clone(),
                )
        });
        anyhow::ensure!(
            !self.editor_open_pending.values().any(|p| p.pane == pane),
            "this pane already has an editor Open preparation pending"
        );
        // Paths are captured now; filesystem inspection never runs in this UI handler.
        domain::validate_path(&root)?;
        domain::validate_path(&args.path)?;
        if self.editor_preparer.is_none() {
            let sender = self.sender.clone();
            self.editor_preparer = Some(OpenPreparer::start(move |prepared| {
                sender.send(Event::Editor(Signal::Prepared(prepared)))
            })?);
        }
        let id = self.editor_next();
        let ticket = self
            .editor_preparer
            .as_ref()
            .unwrap()
            .submit(id, root, args.path, submitted)?;
        self.editor_open_pending.insert(
            id,
            PendingOpen {
                ticket,
                source,
                workspace,
                pane,
                reply,
                files_owner,
            },
        );
        Ok(())
    }
    fn editor_open_target(&self, pending: &PendingOpen) -> anyhow::Result<usize> {
        anyhow::ensure!(
            pending
                .files_owner
                .as_ref()
                .is_none_or(|owner| self.files_owner_current(owner)),
            "Files panel changed before Open completed; request was cancelled"
        );
        anyhow::ensure!(
            !pending.ticket.is_cancelled() && !pending.ticket.is_expired(Instant::now()),
            "editor Open exceeded its original deadline or was cancelled"
        );
        anyhow::ensure!(
            self.editor_barrier.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && !self.closing,
            "editor Open was cancelled because synchronization or window close began"
        );
        if !self.editors.contains_key(&pending.source) {
            self.ensure_attached(pending.source)?;
        }
        let (index, pane, _) = self
            .locate(pending.source)
            .context("editor Open source closed")?;
        anyhow::ensure!(
            self.workspaces[index].id == pending.workspace && pane == pending.pane,
            "editor Open source moved during preparation; request was cancelled"
        );
        Ok(index)
    }
    fn editor_finish_open(&mut self, pending: PendingOpen, paths: OpenPaths) -> anyhow::Result<()> {
        let index = self.editor_open_target(&pending)?;
        let OpenPaths { root, path } = paths;
        let pane = pending.pane;
        let reuse = self.workspaces[index].leaves().into_iter()
            .find(|(p, _, _)| *p == pane)
            .and_then(|(_, _, tabs)| tabs.into_iter().find(|tab|
                matches!(&tab.kind, SurfaceKind::Editor { workspace_root, .. } if *workspace_root == root)))
            .map(|tab| tab.id);
        anyhow::ensure!(
            !self.detached.contains_key(&pending.source) || reuse == Some(pending.source),
            "a separate editor can only open documents in its existing workspace root; move it back before creating another editor tab"
        );
        let (id, placement, request, open_path) = if let Some(id) = reuse {
            anyhow::ensure!(
                self.editors
                    .get(&id)
                    .is_some_and(|e| e.ready && e.pending.is_none()),
                "editor is loading or busy"
            );
            let request = self.editor_next();
            self.editors[&id].barrier(request, true)?;
            (id, "reuse_tab", request, Some(path))
        } else {
            let mut tab = PaneSurface::editor("Editor", root.clone());
            let id = tab.id;
            let state = EditorSessionState {
                open_files: vec![flowmux_core::EditorFileState {
                    path: path.clone(),
                    cursor_line: 0,
                    cursor_column: 0,
                    scroll_top: 0.0,
                }],
                active_file: Some(path),
                ..EditorSessionState::default()
            };
            tab.kind = SurfaceKind::Editor {
                workspace_root: root.clone(),
                session: state.clone(),
            };
            self.add_editor_view(id, root, state)?;
            // WebView construction can be slow. Do not publish a late view, even
            // though the native COM call itself cannot be interrupted safely.
            let insertion = self.editor_open_target(&pending).and_then(|index| {
                self.workspaces[index]
                    .root
                    .add_surface_to_leaf(pane, tab)
                    .context("editor Open target pane disappeared")
            });
            if let Err(error) = insertion {
                self.editor_remove(id);
                return Err(error);
            }
            let request = *self.editors[&id]
                .inflight
                .iter()
                .next()
                .context("editor initialization missing")?;
            (id, "new_tab", request, None)
        };
        self.editors.get_mut(&id).unwrap().pending = Some(Pending {
            id: request,
            reply: pending.reply,
            started: pending.ticket.submitted,
            kind: PendingKind::Open,
            result: json!({"editor_opened":{"pane":pane,"surface":id,"placement_strategy":placement}}),
            error: None,
            open_loaded: false,
            initial_open: reuse.is_none(),
            timed_out: false,
            open_path,
            files_owner: pending.files_owner,
        });
        let layout = self.select(id).and_then(|()| {
            self.zoomed = None;
            self.rebuild()
        });
        if let Err(error) = layout {
            // The Open is already in flight. Its normal ordered completion owns
            // the single response, including a failure after model publication.
            self.editors
                .get_mut(&id)
                .unwrap()
                .pending
                .as_mut()
                .unwrap()
                .error = Some(error.to_string());
        }
        Ok(())
    }
    fn editor_capture_picker(
        &self,
        pane: Option<Uuid>,
        caller: Option<SurfaceId>,
    ) -> anyhow::Result<PickerTarget> {
        anyhow::ensure!(
            !self.background_test,
            "Open File is unavailable in background mode"
        );
        anyhow::ensure!(
            !self.editor_picker_pending,
            "an editor picker is already pending"
        );
        anyhow::ensure!(
            self.editor_barrier.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && !self.closing,
            "editor picker cannot open during synchronization or window close"
        );
        let source = self.target(pane, caller)?;
        let (index, pane, _) = self
            .locate(source)
            .context("editor source pane disappeared")?;
        anyhow::ensure!(
            self.workspaces[index].ssh.is_none(),
            "Remote file editing is not available in SSH workspaces"
        );
        Ok(PickerTarget {
            source,
            pane,
            workspace: self.workspaces[index].id,
            root: self
                .editors
                .get(&source)
                .filter(|_| self.detached.contains_key(&source))
                .map_or_else(
                    || self.workspaces[index].cwd.clone(),
                    |editor| editor.root.clone(),
                ),
        })
    }
    fn editor_show_picker(
        &mut self,
        target: PickerTarget,
        completion: Completion,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.background_test,
            "Open File is unavailable in background mode"
        );
        anyhow::ensure!(
            self.editor_barrier.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && !self.closing,
            "editor picker was cancelled because synchronization or window close began"
        );
        let (index, pane, _) = self
            .locate(target.source)
            .context("editor picker source closed")?;
        anyhow::ensure!(
            pane == target.pane && self.workspaces[index].id == target.workspace,
            "editor picker source moved before the dialog opened"
        );
        let Some(path) = editor_picker::pick(
            self.surface_window(target.source),
            &target.root,
            self.background_test,
        )?
        else {
            return Ok(());
        };
        // The dialog and its native modal loop finish before application events
        // are drained. Pin the original source and root into the async request.
        self.editor_submit_open(
            domain::OpenArgs {
                path,
                pane: None,
                root: Some(target.root),
            },
            Some(target.source),
            completion,
            None,
        )
    }
    fn editor_run_picker(&mut self, target: PickerTarget) {
        let completion = Completion::User(self.surface_window(target.source));
        let result = self.editor_show_picker(target, completion.clone());
        self.editor_picker_pending = false;
        if let Err(error) = result {
            let _ = completion.try_send(json!({"error":error.to_string()}));
        }
    }
    pub(super) fn editor_pick_action(&mut self) -> anyhow::Result<()> {
        self.files_operation_guard()?;
        let target = self.editor_capture_picker(None, None)?;
        self.editor_picker_pending = true;
        self.editor_run_picker(target);
        Ok(())
    }
    pub(super) fn editor_command(
        &mut self,
        op: domain::Op,
        caller: Option<SurfaceId>,
        reply: ipc::Reply,
    ) -> anyhow::Result<Option<Value>> {
        if !matches!(&op, domain::Op::Status(_)) {
            self.files_operation_guard()?;
        }
        match op {
            domain::Op::QuickOpen(args) => {
                self.editor_search_start(
                    SurfaceId(args.surface),
                    Uuid::new_v4().to_string(),
                    search::Query::Quick,
                    Some(reply),
                )?;
                Ok(None)
            }
            domain::Op::Search(args) => {
                let options = args.options()?;
                self.editor_search_start(
                    SurfaceId(args.surface),
                    Uuid::new_v4().to_string(),
                    search::Query::Workspace {
                        query: args.query,
                        options,
                    },
                    Some(reply),
                )?;
                Ok(None)
            }
            domain::Op::SearchCancel(args) => {
                anyhow::ensure!(
                    self.editors.contains_key(&SurfaceId(args.surface)),
                    "editor tab not found"
                );
                self.editor_search_cancel(
                    SurfaceId(args.surface),
                    None,
                    "search cancelled by request",
                );
                Ok(Some(json!({"ok":true})))
            }
            domain::Op::SearchOpen(args) => {
                self.editor_search_open(
                    SurfaceId(args.surface),
                    args.token,
                    args.index,
                    Completion::Ipc(reply),
                )?;
                Ok(None)
            }
            domain::Op::Open(args) => {
                self.editor_submit_open(args, caller, Completion::Ipc(reply), None)?;
                Ok(None)
            }
            domain::Op::Pick(args) => {
                let target = self.editor_capture_picker(args.pane, caller)?;
                // Human dialog interaction has no IPC-sized duration budget.
                // Acknowledge only the request before entering its modal loop,
                // and never show a dialog for a request whose receiver expired.
                reply
                    .try_send(
                        json!({"picker_requested":true,"pane":target.pane,"source":target.source}),
                    )
                    .map_err(|_| {
                        anyhow::anyhow!("editor picker requester expired; dialog was not opened")
                    })?;
                self.editor_picker_pending = true;
                self.sender.send(Event::Editor(Signal::Pick(target)));
                Ok(None)
            }
            domain::Op::Status(args) => {
                let mut status = self
                    .editors
                    .get(&SurfaceId(args.surface))
                    .context("editor tab not found")?
                    .status(SurfaceId(args.surface));
                status["search_service"] =
                    serde_json::to_value(self.editor_search_service.as_ref().map(|s| s.status()))?;
                Ok(Some(status))
            }
            op => {
                anyhow::ensure!(
                    self.editor_barrier.is_none(),
                    "editor synchronization in progress"
                );
                let surface = match &op {
                    domain::Op::Command(args) => args.surface,
                    domain::Op::Find(args) => args.surface,
                    domain::Op::ReplaceMatch(args) | domain::Op::ReplaceAll(args) => args.surface,
                    domain::Op::Flush(args) | domain::Op::CheckDisk(args) => args.surface,
                    _ => unreachable!(),
                };
                let surface = SurfaceId(surface);
                let editor = self.editors.get(&surface).context("editor tab not found")?;
                anyhow::ensure!(
                    editor.ready && editor.pending.is_none(),
                    "editor is loading or another command is pending"
                );
                let id = self.editor_next();
                let (kind, script) = match op {
                    domain::Op::Find(args) => (
                        PendingKind::Command,
                        format!("window.flowmuxWindowsEditor.command({})", args.command(id)?),
                    ),
                    domain::Op::ReplaceMatch(args) => (
                        PendingKind::Command,
                        format!(
                            "window.flowmuxWindowsEditor.command({})",
                            args.command(id, false)?
                        ),
                    ),
                    domain::Op::ReplaceAll(args) => (
                        PendingKind::Command,
                        format!(
                            "window.flowmuxWindowsEditor.command({})",
                            args.command(id, true)?
                        ),
                    ),
                    domain::Op::Command(args) => {
                        domain::validate_command(&args)?;
                        let mut command =
                            json!({"id":id,"action":args.action,"overwrite":args.overwrite});
                        if let Some(text) = args.text {
                            command["text"] = json!(text);
                        }
                        if let Some(path) = args.path {
                            command["path"] = json!(path);
                        }
                        (
                            PendingKind::Command,
                            format!("window.flowmuxWindowsEditor.command({command})"),
                        )
                    }
                    domain::Op::CheckDisk(_) => (
                        PendingKind::Disk,
                        format!("window.flowmuxWindowsEditor.barrier({id},true)"),
                    ),
                    domain::Op::Flush(_) => (
                        PendingKind::Flush,
                        format!("window.flowmuxWindowsEditor.barrier({id},false)"),
                    ),
                    _ => unreachable!(),
                };
                let editor = self.editors.get_mut(&surface).unwrap();
                editor.error = None;
                editor.refresh.activity = Instant::now();
                editor.view.view.evaluate_script(&script)?;
                editor.pending = Some(Pending {
                    id,
                    reply: Completion::Ipc(reply),
                    started: Instant::now(),
                    kind,
                    result: Value::Null,
                    error: None,
                    open_loaded: false,
                    initial_open: false,
                    timed_out: false,
                    open_path: None,
                    files_owner: None,
                });
                Ok(None)
            }
        }
    }
}
