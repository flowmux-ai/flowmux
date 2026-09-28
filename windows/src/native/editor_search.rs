// SPDX-License-Identifier: GPL-3.0-or-later
//! Search snapshots and callbacks stay pinned to their editor instance.
use super::*;
use crate::editor_search::{self as scan, Outcome, SourceStamp};
use crate::editor_worker::{SearchDocumentVersion, SearchOpen, SearchRange, SearchSnapshot};

pub(in crate::native::host) const TIMER: usize = 3;

#[derive(Clone)]
pub(super) enum Query {
    Quick,
    Workspace {
        query: String,
        options: flowmux_editor::SearchOptions,
    },
}
impl Query {
    fn name(&self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Workspace { .. } => "workspace",
        }
    }
    fn empty(&self, cancelled: bool) -> Outcome {
        match self {
            Self::Quick => Outcome::QuickOpen {
                paths: Vec::new(),
                truncated: false,
            },
            Self::Workspace { .. } => Outcome::Workspace {
                result: flowmux_editor::WorkspaceSearchResult {
                    cancelled,
                    ..Default::default()
                },
            },
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Flushing,
    Capturing,
    Working,
}
struct Version {
    id: String,
    path: PathBuf,
    version: u64,
}
struct PendingSearch {
    id: u64,
    token: scan::Token,
    query: Query,
    deadline: Instant,
    phase: Phase,
    reply: Option<ipc::Reply>,
    ticket: Option<scan::Ticket>,
    versions: Vec<Version>,
}
impl Drop for PendingSearch {
    fn drop(&mut self) {
        if let Some(ticket) = &self.ticket {
            ticket.cancel();
        }
    }
}
struct Retained {
    token: Uuid,
    outcome: Outcome,
    sources: Vec<SourceStamp>,
    versions: Vec<Version>,
}
#[derive(Default)]
pub(super) struct State {
    generation: u64,
    pending: Option<PendingSearch>,
    retained: Option<Retained>,
    open: Option<(u64, SearchOpen)>,
    error: Option<String>,
    completed: u64,
    captures: HashSet<u64>,
}
impl State {
    pub(super) fn captured(&mut self, id: u64) -> bool {
        self.captures.remove(&id)
    }
    pub(super) fn discard_open(&mut self, id: u64) {
        if self.open.as_ref().is_some_and(|(owner, _)| *owner == id) {
            self.open = None;
        }
    }
    pub(super) fn synchronizing(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| p.phase != Phase::Working)
    }
    pub(super) fn pending(&self) -> bool {
        self.pending.is_some()
    }
    pub(super) fn status(&self) -> Value {
        json!({"pending":self.pending.is_some(),"generation":self.generation,
            "request_id":self.pending.as_ref().map(|p|&p.token.request_id),
            "phase":self.pending.as_ref().map(|p|match p.phase {
                Phase::Flushing=>"flushing",Phase::Capturing=>"snapshot",Phase::Working=>"searching"}),
            "kind":self.pending.as_ref().map(|p|p.query.name()),
            "token":self.retained.as_ref().map(|r|r.token),
            "completed_count":self.completed,"error":self.error})
    }
}
fn short_error(error: &str) -> String {
    error[..error.floor_char_boundary(4096)].to_owned()
}
impl Editor {
    fn search_deliver(
        &self,
        pending: &PendingSearch,
        outcome: &Outcome,
        token: Option<Uuid>,
        diagnostics: &scan::Diagnostics,
        error: Option<&str>,
    ) -> anyhow::Result<()> {
        let error = error.map(short_error);
        if let Some(reply) = &pending.reply {
            let value = if let Some(error) = error {
                json!({"error":error,"search":{"request_id":pending.token.request_id,
                    "outcome":outcome,"diagnostics":diagnostics}})
            } else {
                json!({"result":{"token":token,"request_id":pending.token.request_id,
                    "outcome":outcome,"diagnostics":diagnostics}})
            };
            let _ = reply.try_send(value);
        } else {
            let message = match outcome {
                Outcome::QuickOpen { paths, truncated } => EditorMessageOut::QuickOpenCompleted {
                    request_id: pending.token.request_id.clone(),
                    paths: paths.clone(),
                    truncated: *truncated,
                },
                Outcome::Workspace { result } => EditorMessageOut::WorkspaceSearchCompleted {
                    request_id: pending.token.request_id.clone(),
                    result: result.clone(),
                    error: error.clone(),
                },
            };
            let encoded = flowmux_editor::serialize_host_message(&self.surface_string(), &message)?;
            let encoded = serde_json::to_string(&encoded)?;
            let context = json!({"request_id":pending.token.request_id,"token":token,
                "kind":pending.query.name(),"error":error,"diagnostics":diagnostics});
            self.view.view.evaluate_script(&format!(
                "window.flowmuxWindowsEditor.completeSearch(JSON.parse({encoded}),{context})"
            ))?;
        }
        Ok(())
    }
}
impl App {
    pub(super) fn editor_search_ui(
        &mut self,
        surface: SurfaceId,
        request_id: String,
        query: Query,
    ) -> anyhow::Result<()> {
        if let Err(error) =
            self.editor_search_start(surface, request_id.clone(), query.clone(), None)
        {
            let editor = self
                .editors
                .get_mut(&surface)
                .context("editor tab not found")?;
            let error = short_error(&error.to_string());
            editor.search.error = Some(error.clone());
            // Even admission failures must finish the exact UI request spinner.
            let failed = PendingSearch {
                id: 0,
                token: scan::Token {
                    surface: surface.0,
                    instance: editor.instance,
                    request_id,
                    generation: editor.search.generation,
                },
                query,
                deadline: Instant::now(),
                phase: Phase::Working,
                reply: None,
                ticket: None,
                versions: Vec::new(),
            };
            editor.search_deliver(
                &failed,
                &failed.query.empty(false),
                None,
                &scan::Diagnostics::default(),
                Some(&error),
            )?;
        }
        Ok(())
    }
    pub(super) fn editor_search_start(
        &mut self,
        surface: SurfaceId,
        request_id: String,
        query: Query,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            !request_id.is_empty()
                && request_id.len() <= 128
                && !request_id.chars().any(char::is_control),
            "invalid search request ID"
        );
        if let Query::Workspace { query, .. } = &query {
            domain::validate_query(query)?;
        }
        let started = reply
            .as_ref()
            .map_or_else(Instant::now, ipc::Reply::received_at);
        self.editor_search_cancel(surface, None, "search superseded by a newer request");
        let editor = self.editors.get(&surface).context("editor tab not found")?;
        anyhow::ensure!(
            editor.ready
                && editor.pending.is_none()
                && self.editor_barrier.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && !self.closing,
            "editor is loading or another command is pending"
        );
        anyhow::ensure!(
            self.editors.values().filter(|e| e.search.pending()).count() < scan::MAX_ADMITTED,
            "at most eight editor searches may be pending"
        );
        let deadline = started + scan::SEARCH_BUDGET;
        anyhow::ensure!(
            Instant::now() < deadline,
            "search request expired before admission"
        );
        let id = self.editor_next();
        let quick = matches!(query, Query::Quick);
        let editor = self.editors.get_mut(&surface).unwrap();
        editor.search.generation = editor
            .search
            .generation
            .checked_add(1)
            .context("search generation exhausted")?;
        editor.search.retained = None;
        editor.search.error = None;
        editor.search.pending = Some(PendingSearch {
            id,
            token: scan::Token {
                surface: surface.0,
                instance: editor.instance,
                request_id,
                generation: editor.search.generation,
            },
            query,
            deadline,
            phase: if quick {
                Phase::Capturing
            } else {
                Phase::Flushing
            },
            reply,
            ticket: None,
            versions: Vec::new(),
        });
        editor.refresh_ready();
        if quick {
            // The file index never reads editor buffers and needs no flush.
            self.editor_search_snapshot(
                surface,
                id,
                Some(SearchSnapshot {
                    documents: Vec::new(),
                    total_bytes: 0,
                }),
                None,
            );
        } else if let Err(error) = editor.barrier(id, false) {
            self.editor_search_cancel(surface, None, &error.to_string());
        }
        self.editor_search_schedule();
        Ok(())
    }
    pub(super) fn editor_search_cancel(
        &mut self,
        surface: SurfaceId,
        request: Option<&str>,
        reason: &str,
    ) {
        let Some(editor) = self.editors.get_mut(&surface) else {
            return;
        };
        if editor
            .search
            .pending
            .as_ref()
            .is_none_or(|p| request.is_some_and(|r| r != p.token.request_id))
        {
            return;
        }
        let pending = editor.search.pending.take().unwrap();
        if let Some(ticket) = &pending.ticket {
            ticket.cancel();
        }
        editor.search.error = Some(short_error(reason));
        editor.refresh_ready();
        if let Err(error) = editor.search_deliver(
            &pending,
            &pending.query.empty(true),
            None,
            &scan::Diagnostics {
                cancelled: true,
                ..Default::default()
            },
            Some(reason),
        ) {
            editor.error = Some(error.to_string());
        }
        self.editor_search_schedule();
    }
    pub(super) fn editor_search_invalidate(&mut self, surface: SurfaceId) {
        let Some(editor) = self.editors.get_mut(&surface) else {
            return;
        };
        if editor
            .search
            .retained
            .as_ref()
            .is_some_and(|r| matches!(r.outcome, Outcome::Workspace { .. }))
        {
            editor.search.retained = None;
        }
        if editor.search.pending.as_ref().is_some_and(|p| {
            p.phase != Phase::Flushing && matches!(p.query, Query::Workspace { .. })
        }) {
            self.editor_search_cancel(
                surface,
                None,
                "document changed during search; run the search again",
            );
        }
    }
    pub(super) fn editor_search_flush(
        &mut self,
        surface: SurfaceId,
        id: u64,
        error: Option<String>,
    ) -> bool {
        if self.editors[&surface]
            .search
            .open
            .as_ref()
            .is_some_and(|(owner, _)| *owner == id)
        {
            let (_, work) = self
                .editors
                .get_mut(&surface)
                .unwrap()
                .search
                .open
                .take()
                .unwrap();
            let expired = self.editors[&surface].pending.as_ref().is_none_or(|p| {
                p.id != id || p.timed_out || p.started.elapsed() >= crate::editor_open::OPEN_BUDGET
            });
            if expired {
                self.editor_barrier_error(
                    surface,
                    id,
                    "search result Open expired before filesystem work; no result was opened",
                );
            } else if let Some(error) = error {
                self.editor_barrier_error(surface, id, &error);
            } else if let Err(error) = self.editor_work(surface, id, Work::SearchOpen(work)) {
                self.editor_barrier_error(surface, id, &error.to_string());
            }
            return true;
        }
        if self.editors[&surface]
            .search
            .pending
            .as_ref()
            .is_none_or(|p| p.id != id)
        {
            return false;
        }
        if self.editors[&surface]
            .search
            .pending
            .as_ref()
            .unwrap()
            .phase
            != Phase::Flushing
        {
            return true;
        }
        if let Some(error) = error {
            self.editor_search_cancel(surface, None, &error);
            return true;
        }
        if Instant::now()
            >= self.editors[&surface]
                .search
                .pending
                .as_ref()
                .unwrap()
                .deadline
        {
            self.editor_search_cancel(
                surface,
                None,
                "search timed out before snapshot; not retried",
            );
            return true;
        }
        self.editors
            .get_mut(&surface)
            .unwrap()
            .search
            .pending
            .as_mut()
            .unwrap()
            .phase = Phase::Capturing;
        match self.editor_work(surface, id, Work::SearchSnapshot) {
            Ok(()) => {
                self.editors
                    .get_mut(&surface)
                    .unwrap()
                    .search
                    .captures
                    .insert(id);
            }
            Err(error) => self.editor_search_cancel(surface, None, &error.to_string()),
        }
        true
    }
    pub(super) fn editor_search_snapshot(
        &mut self,
        surface: SurfaceId,
        id: u64,
        snapshot: Option<SearchSnapshot>,
        error: Option<String>,
    ) -> bool {
        if self.editors[&surface]
            .search
            .pending
            .as_ref()
            .is_none_or(|p| p.id != id || p.phase != Phase::Capturing)
        {
            return false;
        }
        if let Some(error) = error {
            self.editor_search_cancel(surface, None, &error);
            return true;
        }
        let Some(snapshot) = snapshot else {
            self.editor_search_cancel(surface, None, "search snapshot was missing");
            return true;
        };
        if Instant::now()
            >= self.editors[&surface]
                .search
                .pending
                .as_ref()
                .unwrap()
                .deadline
        {
            self.editor_search_cancel(
                surface,
                None,
                "search timed out before filesystem work; not retried",
            );
            return true;
        }
        if self.editor_search_service.is_none() {
            let sender = self.sender.clone();
            match scan::Service::start(move |response| {
                sender.send(Event::Editor(Signal::Search(response)))
            }) {
                Ok(service) => self.editor_search_service = Some(service),
                Err(error) => {
                    self.editor_search_cancel(surface, None, &error.to_string());
                    return true;
                }
            }
        }
        let editor = self.editors.get_mut(&surface).unwrap();
        let pending = editor.search.pending.as_mut().unwrap();
        pending.versions = snapshot
            .documents
            .iter()
            .map(|d| Version {
                id: d.document_id.clone(),
                path: d.path.clone(),
                version: d.version,
            })
            .collect();
        let kind = match &pending.query {
            Query::Quick => scan::Kind::QuickOpen,
            Query::Workspace { query, options } => scan::Kind::Workspace {
                query: query.clone(),
                options: options.clone(),
                open_documents: snapshot
                    .documents
                    .into_iter()
                    .map(|d| flowmux_editor::SearchDocument {
                        path: d.path,
                        content: d.content,
                    })
                    .collect(),
            },
        };
        let request = scan::Request {
            token: pending.token.clone(),
            root: editor.root.clone(),
            kind,
            deadline: pending.deadline,
        };
        match self.editor_search_service.as_ref().unwrap().submit(request) {
            Ok(ticket) => {
                pending.ticket = Some(ticket);
                pending.phase = Phase::Working;
                editor.refresh_ready();
            }
            Err(error) => self.editor_search_cancel(surface, None, &error.to_string()),
        }
        true
    }
    pub(super) fn editor_search_completed(&mut self, response: scan::Response) {
        let surface = SurfaceId(response.token.surface);
        let Some(editor) = self
            .editors
            .get_mut(&surface)
            .filter(|e| e.instance == response.token.instance)
        else {
            return;
        };
        if editor
            .search
            .pending
            .as_ref()
            .is_none_or(|p| p.token != response.token || p.phase != Phase::Working)
        {
            return;
        }
        let mut pending = editor.search.pending.take().unwrap();
        let mut error = response.error;
        if Instant::now() >= pending.deadline {
            error = Some("search exceeded its deadline; late result discarded".into());
        }
        let same_versions = editor.documents.as_array().is_some_and(|docs| {
            docs.len() == pending.versions.len()
                && pending.versions.iter().all(|v| {
                    docs.iter()
                        .any(|d| d["id"] == v.id && d["version"] == v.version)
                })
        });
        if matches!(pending.query, Query::Workspace { .. }) && !same_versions {
            error = Some("open documents changed during search; late result discarded".into());
        }
        let token = error.is_none().then(Uuid::new_v4);
        editor.search.completed = editor.search.completed.saturating_add(1);
        editor.search.error = error.clone();
        editor.refresh_ready();
        let outcome = if error.is_some() {
            pending.query.empty(response.diagnostics.cancelled)
        } else {
            response.outcome
        };
        if let Err(error) = editor.search_deliver(
            &pending,
            &outcome,
            token,
            &response.diagnostics,
            error.as_deref(),
        ) {
            editor.search.error = Some(error.to_string());
        } else if let Some(token) = token {
            editor.search.retained = Some(Retained {
                token,
                outcome,
                sources: response.sources,
                versions: std::mem::take(&mut pending.versions),
            });
        }
        self.editor_search_schedule();
    }
    fn editor_search_schedule(&mut self) {
        unsafe {
            KillTimer(self.window, TIMER);
        }
        if let Some(next) = self
            .editors
            .values()
            .filter_map(|e| e.search.pending.as_ref().map(|p| p.deadline))
            .min()
        {
            let millis = next
                .saturating_duration_since(Instant::now())
                .as_millis()
                .clamp(10, u32::MAX as u128) as u32;
            if unsafe { SetTimer(self.window, TIMER, millis, None) } == 0 {
                // Drain first so cancellation cannot recursively retry a broken timer.
                let pending: Vec<_> = self
                    .editors
                    .iter_mut()
                    .filter_map(|(surface, e)| e.search.pending.take().map(|p| (*surface, p)))
                    .collect();
                for (surface, pending) in pending {
                    let editor = self.editors.get_mut(&surface).unwrap();
                    let reason = "could not schedule search deadline";
                    editor.search.error = Some(reason.into());
                    editor.refresh_ready();
                    if let Err(error) = editor.search_deliver(
                        &pending,
                        &pending.query.empty(true),
                        None,
                        &scan::Diagnostics {
                            cancelled: true,
                            ..Default::default()
                        },
                        Some(reason),
                    ) {
                        editor.error = Some(error.to_string());
                    }
                }
            }
        }
    }
    pub(in crate::native::host) fn editor_search_tick(&mut self) {
        let expired: Vec<_> = self
            .editors
            .iter()
            .filter(|(_, e)| {
                e.search
                    .pending
                    .as_ref()
                    .is_some_and(|p| Instant::now() >= p.deadline)
            })
            .map(|(s, _)| *s)
            .collect();
        for surface in expired {
            self.editor_search_cancel(surface, None, "search timed out; not retried");
        }
        self.editor_search_schedule();
    }
    pub(super) fn editor_search_open(
        &mut self,
        surface: SurfaceId,
        token: Uuid,
        index: usize,
        completion: Completion,
    ) -> anyhow::Result<()> {
        let started = match &completion {
            Completion::Ipc(reply) => reply.received_at(),
            _ => Instant::now(),
        };
        anyhow::ensure!(
            started.elapsed() < crate::editor_open::OPEN_BUDGET,
            "search result Open expired in the UI queue; no result was opened"
        );
        anyhow::ensure!(
            self.editor_barrier.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && !self.closing,
            "editor synchronization or close is pending"
        );
        let editor = self.editors.get(&surface).context("editor tab not found")?;
        anyhow::ensure!(
            editor.ready && editor.pending.is_none() && !editor.search.pending(),
            "editor is busy"
        );
        let retained = editor
            .search
            .retained
            .as_ref()
            .filter(|r| r.token == token)
            .context("search results expired; run the search again")?;
        let (path, range) = match &retained.outcome {
            Outcome::QuickOpen { paths, .. } => (
                paths
                    .get(index)
                    .context("search result index is out of range")?
                    .clone(),
                None,
            ),
            Outcome::Workspace { result } => {
                let found = result
                    .matches
                    .get(index)
                    .context("search result index is out of range")?;
                (
                    found.path.clone(),
                    Some(SearchRange {
                        line: found.line,
                        column: found.column,
                        length: found.length,
                    }),
                )
            }
        };
        let stamp = retained.sources.iter().find(|s| s.path == path);
        let expected_document = retained
            .versions
            .iter()
            .find(|v| scan::same_path(&v.path, &editor.root.join(&path)))
            .map(|v| SearchDocumentVersion {
                document_id: v.id.clone(),
                version: v.version,
            });
        let work = SearchOpen {
            path: path.into(),
            range,
            expected_document,
            expected_sha256: stamp.map(|s| s.sha256.clone()),
        };
        let id = self.editor_next();
        let editor = self.editors.get_mut(&surface).unwrap();
        editor.search.open = Some((id, work));
        editor.pending = Some(Pending {
            id,
            reply: completion,
            started,
            kind: PendingKind::SearchOpen,
            result: json!({"ok":true,"surface":surface,"search_token":token,"index":index}),
            error: None,
            open_loaded: false,
            initial_open: false,
            timed_out: false,
            open_path: None,
        });
        editor.refresh.activity = Instant::now();
        if let Err(error) = editor.barrier(id, true) {
            self.editor_barrier_error(surface, id, &error.to_string());
        }
        Ok(())
    }
}
