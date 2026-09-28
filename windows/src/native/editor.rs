// SPDX-License-Identifier: GPL-3.0-or-later
//! Native editor surfaces. WebViews stay on the UI thread; document I/O is ordered off-thread.
use super::*;
use crate::{
    editor as domain,
    editor_worker::{Response, Work, Worker},
};
use flowmux_core::{EditorSessionState, PaneSurface};
use flowmux_editor::{EditorMessage, HostMessage as EditorMessageOut};
use std::collections::HashSet;
#[path = "editor_view.rs"]
mod editor_view;

pub(super) enum Signal {
    Bridge(SurfaceId, Uuid, String, String),
    Worker(SurfaceId, Uuid, Response),
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
}
struct Pending {
    id: u64,
    reply: ipc::Reply,
    started: Instant,
    kind: PendingKind,
    result: Value,
    error: Option<String>,
    open_loaded: bool,
    initial_open: bool,
    timed_out: bool,
    open_path: Option<PathBuf>,
}
enum PendingKind {
    Command,
    Flush,
    Disk,
    Open,
}
pub(super) enum Operation {
    Tab(SurfaceId),
    Workspace(WorkspaceId),
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
}
impl Editor {
    fn refresh_ready(&mut self) {
        self.ready = self.frontend_ready
            && self.backend_ready
            && !self.sync_failed
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
            "view_handle":self.view.view.hwnd().0 as usize,"session":self.state})
    }
}
impl App {
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
        let data_root = if self.background_test {
            PathBuf::from(
                std::env::var_os("FLOWMUX_TEST_STATE_DIR")
                    .context("background editor requires an isolated state directory")?,
            )
        } else {
            data_dir()?
        };
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
            },
        );
        Ok(())
    }
    fn editor_work(&mut self, surface: SurfaceId, id: u64, work: Work) -> anyhow::Result<()> {
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
                        match message {
                            EditorMessage::EditorReady => {
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
                                for message in std::mem::take(&mut editor.deferred) {
                                    editor.send(&message)?
                                }
                                editor.refresh_ready();
                                if editor.pending.as_ref().is_some_and(|p| {
                                    matches!(p.kind, PendingKind::Open) && p.open_loaded
                                }) {
                                    editor.barrier(editor.pending.as_ref().unwrap().id, false)?;
                                }
                            }
                            EditorMessage::FlushCompleted { request_id, error } => {
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
                let Some(editor) = self
                    .editors
                    .get_mut(&surface)
                    .filter(|e| e.instance == instance)
                else {
                    return Ok(());
                };
                editor.inflight.remove(&response.id);
                editor.backend_ready = response.ready;
                editor.initialization_failed = response.initialization_failed;
                editor.refresh_ready();
                editor.state = response.state;
                editor.dirty = response.dirty;
                editor.documents = serde_json::to_value(response.documents)?;
                editor.restore_errors = response.restored_errors;
                let mut failure = response.error;
                for message in response.messages {
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
                        editor.send(&message)?
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
                    if let Some(pending) = &mut editor.pending {
                        pending.error = Some(error.clone())
                    }
                }
                let state = editor.state.clone();
                if let Some((index, pane, _)) = self.locate(surface) {
                    self.workspaces[index]
                        .root
                        .set_surface_editor_session(pane, surface, state);
                    self.refresh_tab_title(surface);
                }
                if self
                    .editor_barrier
                    .as_ref()
                    .is_some_and(|b| b.id == response.id && b.waiting.contains(&surface))
                {
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
                    if editor
                        .pending
                        .as_ref()
                        .is_some_and(|p| matches!(p.kind, PendingKind::Open) && !p.open_loaded)
                    {
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
                    let pending = editor.pending.take().unwrap();
                    editor.release(pending.id);
                    editor.refresh_ready();
                    if pending.timed_out {
                        editor.error = pending.error.clone().or_else(|| Some(
                            "The previous command exceeded its reply deadline and has now finished; review the document state before retrying".into(),
                        ));
                    }
                    let result = if let Some(error) = pending.error {
                        json!({"error":error})
                    } else if matches!(pending.kind, PendingKind::Open) {
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
            let barrier = self.editor_barrier.take().unwrap();
            for target in &barrier.targets {
                if let Some(e) = self.editors.get(target) {
                    e.release(id)
                }
            }
            self.editor_operation_error(barrier.operation, barrier.reply, error);
        } else if let Some(editor) = self.editors.get_mut(&surface) {
            if editor.pending.as_ref().is_some_and(|p| p.id == id) {
                let pending = editor.pending.take().unwrap();
                editor.release(id);
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
                    self.window,
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
        let barrier = self.editor_barrier.take().unwrap();
        let dirty = barrier
            .targets
            .iter()
            .any(|id| self.editors.get(id).is_some_and(|e| !e.dirty.is_empty()));
        if dirty && !matches!(barrier.operation, Operation::Checkpoint(_)) {
            for id in &barrier.targets {
                if let Some(e) = self.editors.get(id) {
                    e.release(barrier.id)
                }
            }
            self.editor_operation_error(barrier.operation,barrier.reply,"Editor documents have unsaved changes. Save them or explicitly discard the documents in the editor before closing.");
            return Ok(());
        }
        let closing = matches!(
            &barrier.operation,
            Operation::Window(_) | Operation::QuitDiscard(_)
        );
        self.editor_bypass = true;
        let result = (|| -> anyhow::Result<()> {
            match barrier.operation {
                Operation::Tab(id) => {
                    self.select(id)?;
                    self.action(Action::CloseTab)?;
                }
                Operation::Workspace(id) => {
                    self.workspace_command(WorkspaceOp::Close { workspace: id.0 }, None)?;
                }
                Operation::Window(request) => self.request_close(request)?,
                Operation::QuitDiscard(reply) => {
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
    pub(super) fn editor_release_all(&self) {
        // releaseBarrier(0) is the host's explicit cancel-all path.
        for editor in self.editors.values() {
            editor.release(0)
        }
    }
    pub(super) fn editor_tick(&mut self) {
        if let Some((surface, id)) = self
            .editor_barrier
            .as_ref()
            .filter(|b| b.started.elapsed() > Duration::from_secs(12))
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
                    .filter(|p| !p.timed_out && p.started.elapsed() > Duration::from_secs(12))
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
        if let Some(mut editor) = self.editors.remove(&id) {
            if let Some(pending) = editor.pending.take() {
                let _ = pending
                    .reply
                    .try_send(json!({"error":"editor tab closed during command"}));
            }
        }
    }
    pub(super) fn editor_command(
        &mut self,
        op: domain::Op,
        caller: Option<SurfaceId>,
        reply: ipc::Reply,
    ) -> anyhow::Result<Option<Value>> {
        match op {
            domain::Op::Open(args) => {
                anyhow::ensure!(
                    self.editor_barrier.is_none(),
                    "editor synchronization in progress"
                );
                let source = self.target(args.pane, caller)?;
                let (index, pane, _) = self
                    .locate(source)
                    .context("editor source pane disappeared")?;
                let root = domain::canonical_root(
                    args.root.as_deref().unwrap_or(&self.workspaces[index].cwd),
                )?;
                let path = domain::resolve_existing(&root, &args.path)?;
                let reuse=self.workspaces[index].leaves().into_iter().find(|(p,_,_)|*p==pane).and_then(|(_,_,tabs)|tabs.into_iter().find(|t|matches!(&t.kind,SurfaceKind::Editor{workspace_root,..} if *workspace_root==root))).map(|t|t.id);
                let (id, placement, request, open_path) = if let Some(id) = reuse {
                    anyhow::ensure!(
                        self.editors
                            .get(&id)
                            .is_some_and(|e| e.ready && e.pending.is_none()),
                        "editor is loading or busy"
                    );
                    let request = self.editor_next();
                    (id, "reuse_tab", request, Some(path))
                } else {
                    let tab = PaneSurface::editor("Editor", root.clone());
                    let id = tab.id;
                    let mut state = EditorSessionState::default();
                    state.open_files.push(flowmux_core::EditorFileState {
                        path: path.clone(),
                        cursor_line: 0,
                        cursor_column: 0,
                        scroll_top: 0.0,
                    });
                    state.active_file = Some(path);
                    let mut tab = tab;
                    tab.kind = SurfaceKind::Editor {
                        workspace_root: root.clone(),
                        session: state.clone(),
                    };
                    self.add_editor_view(id, root, state)?;
                    self.workspaces[index]
                        .root
                        .add_surface_to_leaf(pane, tab)
                        .context("editor source pane disappeared")?;
                    let request = *self.editors[&id]
                        .inflight
                        .iter()
                        .next()
                        .context("editor initialization missing")?;
                    (id, "new_tab", request, None)
                };
                self.editors.get_mut(&id).unwrap().pending = Some(Pending {
                    id: request,
                    reply,
                    started: Instant::now(),
                    kind: PendingKind::Open,
                    result: json!({"editor_opened":{"pane":pane,"surface":id,"placement_strategy":placement}}),
                    error: None,
                    open_loaded: false,
                    initial_open: reuse.is_none(),
                    timed_out: false,
                    open_path,
                });
                if reuse.is_some() {
                    self.editors[&id].barrier(request, true)?;
                }
                self.select(id)?;
                self.zoomed = None;
                self.rebuild()?;
                Ok(None)
            }
            domain::Op::Status(args) => Ok(Some(
                self.editors
                    .get(&SurfaceId(args.surface))
                    .context("editor tab not found")?
                    .status(SurfaceId(args.surface)),
            )),
            op => {
                anyhow::ensure!(
                    self.editor_barrier.is_none(),
                    "editor synchronization in progress"
                );
                let surface = match &op {
                    domain::Op::Command(args) => args.surface,
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
                editor.view.view.evaluate_script(&script)?;
                editor.pending = Some(Pending {
                    id,
                    reply,
                    started: Instant::now(),
                    kind,
                    result: Value::Null,
                    error: None,
                    open_loaded: false,
                    initial_open: false,
                    timed_out: false,
                    open_path: None,
                });
                Ok(None)
            }
        }
    }
}
