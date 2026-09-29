// SPDX-License-Identifier: GPL-3.0-or-later
//! Whole-window output search, owned by the native host's UI thread.
use super::*;
use crate::output_search::{self, Match, PAGE_SIZE};
use std::collections::VecDeque;
#[path = "search_panel.rs"]
mod panel;
pub(super) use panel::UiAction;
// Renderer previews contain at most 220 Unicode scalars. Even JSON's six-byte
// control-character escaping plus per-hit metadata stays below the existing
// 1 MiB bridge limit at 700 hits. CLI pages remain exactly PAGE_SIZE (500).
const UI_RESULT_LIMIT: usize = 700;

#[derive(Clone, serde::Serialize)]
pub(super) struct Hit {
    surface: SurfaceId,
    workspace: String,
    title: String,
    #[serde(flatten)]
    found: Match,
}
struct Run {
    id: Uuid,
    query: String,
    match_case: bool,
    offset: usize,
    limit: usize,
    queue: VecDeque<SurfaceId>,
    waiting: Option<(SurfaceId, u64, Instant)>,
    hits: Vec<Hit>,
    total: usize,
    searched: usize,
    unavailable: Vec<Value>,
    cancelled: bool,
}
struct Opening {
    search: Uuid,
    surface: SurfaceId,
    after: u64,
    hit: u32,
    selected: bool,
    started: Instant,
    reply: Option<ipc::Reply>,
}
#[derive(Default)]
pub(super) struct Controller {
    run: Option<Run>,
    panel: Option<panel::Panel>,
    opening: HashMap<Uuid, Opening>,
    debounce: Option<Instant>,
}
impl Controller {
    pub(super) fn diagnostics(&self) -> Value {
        let mut value = self
            .panel
            .as_ref()
            .map_or(Value::Null, |panel| panel.diagnostics());
        if let Value::Object(object) = &mut value {
            object.insert("search".into(), json!(self.run.as_ref().map(|run| run.id)));
            object.insert(
                "result_limit".into(),
                json!(self.run.as_ref().map_or(PAGE_SIZE, |run| run.limit)),
            );
            object.insert("ui_result_limit".into(), json!(UI_RESULT_LIMIT));
            object.insert(
                "pending".into(),
                json!(self
                    .run
                    .as_ref()
                    .is_some_and(|run| run.waiting.is_some() || !run.queue.is_empty())),
            );
        }
        value
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|panel| panel.handle_message(message))
    }
}

impl App {
    pub(super) fn search_status(&self, id: Uuid) -> anyhow::Result<Value> {
        let run = self
            .search
            .run
            .as_ref()
            .filter(|r| r.id == id)
            .context("search expired; start a new search")?;
        Ok(
            json!({"search":id,"query":run.query,"match_case":run.match_case,"offset":run.offset,
            "pending":run.waiting.is_some() || !run.queue.is_empty(),"cancelled":run.cancelled,
            "total":run.total,"searched":run.searched,"unavailable":run.unavailable,"hits":run.hits,
            "page_size":PAGE_SIZE,"result_limit":run.limit,"panel_rows":self.search.panel.as_ref().map(|p|p.rows()),
            "panel_handle":self.search.panel.as_ref().map(|p|p.window as usize),"dialog":self.search.diagnostics()}),
        )
    }
    pub(super) fn cancel_surface_search(&mut self, surface: SurfaceId) {
        let affected = self
            .search
            .run
            .as_ref()
            .filter(|run| {
                run.waiting.is_some_and(|(id, _, _)| id == surface)
                    || self
                        .search
                        .opening
                        .values()
                        .any(|pending| pending.surface == surface)
            })
            .map(|run| run.id);
        if let Some(id) = affected {
            let _ = self.cancel_search(id);
        }
    }
    pub(super) fn cancel_search(&mut self, id: Uuid) -> anyhow::Result<()> {
        self.search.debounce = None;
        let run = self
            .search
            .run
            .as_mut()
            .filter(|r| r.id == id)
            .context("search expired")?;
        run.cancelled = true;
        run.waiting = None;
        run.queue.clear();
        run.hits.clear();
        for surface in self.surfaces.values() {
            let _ = surface.send(&HostMessage::CancelSearch { search: id });
        }
        self.search.opening.retain(|_, request| {
            if request.search == id {
                if let Some(reply) = &request.reply {
                    let _ = reply.try_send(json!({"error":"search cancelled"}));
                }
                false
            } else {
                true
            }
        });
        self.update_search_panel();
        Ok(())
    }
    pub(super) fn begin_search(
        &mut self,
        query: String,
        match_case: bool,
        offset: usize,
    ) -> anyhow::Result<Uuid> {
        self.begin_search_limited(query, match_case, offset, PAGE_SIZE)
    }
    fn begin_search_limited(
        &mut self,
        query: String,
        match_case: bool,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<Uuid> {
        anyhow::ensure!(
            (1..=UI_RESULT_LIMIT).contains(&limit),
            "invalid search result limit"
        );
        output_search::validate_query(&query)?;
        if let Some(run) = &self.search.run {
            self.cancel_search(run.id)?;
        }
        if self.search.panel.is_none() {
            self.search.panel = Some(panel::Panel::new(self.window)?);
        }
        self.search.debounce = None;
        let id = Uuid::new_v4();
        let queue = self
            .workspaces
            .iter()
            .flat_map(|w| w.leaves())
            .flat_map(|(_, _, tabs)| tabs.into_iter().map(|t| t.id))
            .collect();
        self.search.run = Some(Run {
            id,
            query: query.clone(),
            match_case,
            offset,
            limit,
            queue,
            waiting: None,
            hits: Vec::new(),
            total: 0,
            searched: 0,
            unavailable: Vec::new(),
            cancelled: false,
        });
        self.search
            .panel
            .as_ref()
            .unwrap()
            .query(&query, match_case);
        self.update_search_panel();
        self.advance_search()?;
        Ok(id)
    }
    fn advance_search(&mut self) -> anyhow::Result<()> {
        let Some(run) = &mut self.search.run else {
            return Ok(());
        };
        if run.cancelled || run.waiting.is_some() || run.queue.is_empty() {
            return Ok(());
        }
        while let Some(id) = run.queue.pop_front() {
            let Some(surface) = self.surfaces.get(&id).filter(|s| s.ready) else {
                run.unavailable
                    .push(json!({"surface":id,"reason":"terminal is closed or not ready"}));
                continue;
            };
            let after = surface
                .session
                .as_ref()
                .map_or(surface.output_sequence, Session::barrier);
            let result = surface.send(&HostMessage::SearchBuffer {
                search: run.id,
                after,
                query: run.query.clone(),
                match_case: run.match_case,
                skip: run.offset.saturating_sub(run.total),
                limit: run.limit - run.hits.len(),
            });
            if let Err(error) = result {
                run.unavailable
                    .push(json!({"surface":id,"reason":error.to_string()}));
                continue;
            }
            run.waiting = Some((id, after, Instant::now()));
            break;
        }
        self.update_search_panel();
        Ok(())
    }
    pub(super) fn searched(
        &mut self,
        id: SurfaceId,
        search: Uuid,
        sequence: u64,
        total: usize,
        hits: Vec<Match>,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        let metadata = self.locate(id).and_then(|(ws, pane, _)| {
            self.workspaces[ws].root.surface_title(pane, id).map(|t| {
                (
                    self.workspaces[ws]
                        .name
                        .chars()
                        .take(256)
                        .collect::<String>(),
                    t.chars().take(256).collect::<String>(),
                )
            })
        });
        let Some(run) = self
            .search
            .run
            .as_mut()
            .filter(|r| r.id == search && !r.cancelled)
        else {
            return Ok(());
        };
        let Some((expected, after, _)) = run.waiting else {
            return Ok(());
        };
        anyhow::ensure!(
            expected == id
                && sequence >= after
                && hits.len() <= run.limit - run.hits.len()
                && hits.len() <= total,
            "invalid output search response"
        );
        anyhow::ensure!(
            hits.iter()
                .all(|h| h.preview.encode_utf16().count() <= 1024 && h.column <= 1000),
            "invalid search preview"
        );
        run.waiting = None;
        if let Some(error) = error {
            run.unavailable.push(json!({"surface":id,"reason":error}));
        } else if let Some((workspace, title)) = metadata {
            run.total = run.total.saturating_add(total);
            run.searched += 1;
            run.hits.extend(hits.into_iter().map(|found| Hit {
                surface: id,
                workspace: workspace.clone(),
                title: title.clone(),
                found,
            }));
        } else {
            run.unavailable
                .push(json!({"surface":id,"reason":"terminal closed during search"}));
        }
        self.update_search_panel();
        self.advance_search()
    }
    pub(super) fn open_search(
        &mut self,
        search: Uuid,
        index: usize,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<()> {
        let run = self
            .search
            .run
            .as_ref()
            .filter(|r| r.id == search && !r.cancelled)
            .context("search expired")?;
        anyhow::ensure!(
            run.waiting.is_none() && run.queue.is_empty(),
            "search is still running"
        );
        let hit = run
            .hits
            .get(index)
            .context("result index is outside this page")?
            .clone();
        anyhow::ensure!(
            self.search.opening.len() < 128,
            "too many pending result activations"
        );
        let surface = self
            .surfaces
            .get(&hit.surface)
            .context("terminal has closed; refresh the search")?;
        let after = surface
            .session
            .as_ref()
            .map_or(surface.output_sequence, Session::barrier);
        let request = Uuid::new_v4();
        surface.send(&HostMessage::OpenSearchHit {
            request,
            search,
            after,
            hit: hit.found.id,
            commit: false,
        })?;
        self.search.opening.insert(
            request,
            Opening {
                search,
                surface: hit.surface,
                after,
                hit: hit.found.id,
                selected: false,
                started: Instant::now(),
                reply,
            },
        );
        Ok(())
    }
    pub(super) fn search_opened(
        &mut self,
        id: SurfaceId,
        request: Uuid,
        search: Uuid,
        sequence: u64,
        mut result: Result<String, String>,
        position: (bool, u32, u16),
    ) -> anyhow::Result<()> {
        let Some(pending) = self.search.opening.get(&request) else {
            return Ok(());
        };
        anyhow::ensure!(
            pending.surface == id
                && pending.search == search
                && sequence >= pending.after
                && pending.selected == position.0,
            "invalid search activation response"
        );
        let mut pending = self.search.opening.remove(&request).unwrap();
        if result.is_ok() && !pending.selected {
            // Verify before changing workspace/focus. Revalidate after activation,
            // since making a hidden view visible may resize/reflow its grid.
            let activate = (|| -> anyhow::Result<u64> {
                self.select(id)?;
                self.rebuild_without_focus()?;
                let surface = self
                    .surfaces
                    .get(&id)
                    .context("terminal closed during activation")?;
                let after = surface
                    .session
                    .as_ref()
                    .map_or(surface.output_sequence, Session::barrier);
                surface.send(&HostMessage::OpenSearchHit {
                    request,
                    search,
                    after,
                    hit: pending.hit,
                    commit: true,
                })?;
                Ok(after)
            })();
            match activate {
                Ok(after) => {
                    pending.after = after;
                    pending.selected = true;
                    self.search.opening.insert(request, pending);
                    return Ok(());
                }
                Err(error) => result = Err(error.to_string()),
            }
        }
        if let Err(error) = result {
            if let Some(panel) = &self.search.panel {
                panel.status(&error);
            }
            if let Some(reply) = pending.reply {
                let _ = reply.try_send(json!({"error":error}));
            }
        } else {
            if let Some(panel) = &self.search.panel {
                panel.hide();
            }
            self.focus_active()?;
            if let Some(reply) = pending.reply {
                let _=reply.try_send(json!({"surface":id,"search":search,"selection":result.unwrap(),"line":position.1,"column":position.2,"sequence":sequence}));
            }
        }
        Ok(())
    }
    fn update_search_panel(&self) {
        if let (Some(run), Some(panel)) = (&self.search.run, &self.search.panel) {
            let pending = run.waiting.is_some() || !run.queue.is_empty();
            let status = if run.cancelled {
                "Search cancelled".to_owned()
            } else {
                format!(
                    "{}{} of {} matching lines · {} terminals · {} unavailable{}",
                    if pending { "Searching… " } else { "" },
                    run.hits.len(),
                    run.total,
                    run.searched,
                    run.unavailable.len(),
                    if !pending && run.limit == UI_RESULT_LIMIT && run.total > run.limit {
                        " · 700-result display limit; refine your query for more results"
                    } else {
                        ""
                    }
                )
            };
            panel.status(&status);
            panel.results(&run.hits);
            panel.buttons(
                !pending && !run.cancelled && run.offset > 0,
                !pending
                    && !run.cancelled
                    && run.limit < UI_RESULT_LIMIT
                    && run.offset.saturating_add(run.limit) < run.total,
                !pending && !run.cancelled && !run.hits.is_empty(),
            );
        }
    }
    pub(super) fn search_tick(&mut self) -> anyhow::Result<()> {
        if self.search.debounce.is_some_and(|at| at <= Instant::now()) {
            self.search.debounce = None;
            self.search_ui(UiAction::Refresh)?;
        }
        let mut changed = false;
        if let Some(run) = &mut self.search.run {
            if let Some((id, _, at)) = run.waiting {
                if at.elapsed() > Duration::from_secs(12) || !self.surfaces.contains_key(&id) {
                    run.waiting = None;
                    changed = true;
                    run.unavailable.push(
                        json!({"surface":id,"reason":"terminal closed or timed out during search"}),
                    );
                    if let Some(surface) = self.surfaces.get(&id) {
                        let _ = surface.send(&HostMessage::CancelSearch { search: run.id });
                    }
                }
            }
        }
        if changed {
            self.update_search_panel();
        }
        self.search.opening.retain(|_, request| {
            if request.started.elapsed() > Duration::from_secs(12)
                || !self.surfaces.contains_key(&request.surface)
            {
                if let Some(reply) = &request.reply {
                    let _ = reply.try_send(
                        json!({"error":"terminal closed or timed out during result activation"}),
                    );
                }
                false
            } else {
                true
            }
        });
        self.advance_search()
    }
    pub(super) fn search_ui(&mut self, action: UiAction) -> anyhow::Result<()> {
        if self.search.panel.is_none() {
            self.search.panel = Some(panel::Panel::new(self.window)?);
        }
        match action {
            UiAction::Show => {
                chrome::window_theme(
                    self.search.panel.as_ref().unwrap().window,
                    self.settings.terminal.theme,
                );
                self.search
                    .panel
                    .as_ref()
                    .unwrap()
                    .show(self.background_test);
            }
            UiAction::Layout => self.search.panel.as_ref().unwrap().layout(),
            UiAction::Tick => return self.search_tick(),
            UiAction::Changed => {
                if self.search.panel.as_ref().unwrap().composing() {
                    self.search.debounce = None;
                    if let Some(run) = self.search.run.as_ref().filter(|run| !run.cancelled) {
                        self.cancel_search(run.id)?;
                    }
                    return Ok(());
                }
                let (query, match_case) = self.search.panel.as_ref().unwrap().read_query();
                if !self
                    .search
                    .run
                    .as_ref()
                    .is_some_and(|r| r.query == query && r.match_case == match_case && !r.cancelled)
                {
                    if let Some(run) = &self.search.run {
                        self.cancel_search(run.id)?;
                    }
                    self.search.debounce = Some(Instant::now() + Duration::from_millis(180));
                    self.search.panel.as_ref().unwrap().schedule();
                }
            }
            UiAction::Refresh | UiAction::More => {
                if self.search.panel.as_ref().unwrap().composing() {
                    self.search
                        .panel
                        .as_ref()
                        .unwrap()
                        .status("Finish composing text before searching");
                    return Ok(());
                }
                let (query, match_case) = self.search.panel.as_ref().unwrap().read_query();
                if query.is_empty() {
                    if let Some(run) = &self.search.run {
                        self.cancel_search(run.id)?;
                    }
                    self.search
                        .panel
                        .as_ref()
                        .unwrap()
                        .status("Search retained output in all workspaces in this window");
                } else {
                    let limit = if matches!(action, UiAction::More) {
                        let Some(run) = self.search.run.as_ref().filter(|run| {
                            !run.cancelled
                                && run.waiting.is_none()
                                && run.queue.is_empty()
                                && run.query == query
                                && run.match_case == match_case
                                && run.limit < UI_RESULT_LIMIT
                        }) else {
                            return Ok(());
                        };
                        run.limit.saturating_add(PAGE_SIZE).min(UI_RESULT_LIMIT)
                    } else {
                        PAGE_SIZE
                    };
                    if let Err(error) = self.begin_search_limited(query, match_case, 0, limit) {
                        self.search
                            .panel
                            .as_ref()
                            .unwrap()
                            .status(&error.to_string());
                    }
                }
            }
            UiAction::Open => {
                let panel = self.search.panel.as_ref().unwrap();
                let (query, match_case) = panel.read_query();
                if panel.composing() {
                    return Ok(());
                }
                if let (Some(run), Some(index)) = (
                    self.search.run.as_ref().filter(|run| {
                        run.query == query && run.match_case == match_case && !run.cancelled
                    }),
                    panel.selected().or_else(|| (panel.rows() > 0).then_some(0)),
                ) {
                    let id = run.id;
                    if let Err(error) = self.open_search(id, index, None) {
                        self.search
                            .panel
                            .as_ref()
                            .unwrap()
                            .status(&error.to_string());
                    }
                }
            }
            UiAction::Close => {
                self.search.debounce = None;
                if let Some(run) = &self.search.run {
                    self.cancel_search(run.id)?;
                }
                self.search.panel.as_ref().unwrap().hide();
                self.focus_active()?;
            }
        }
        Ok(())
    }
}
