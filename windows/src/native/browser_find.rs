// SPDX-License-Identifier: GPL-3.0-or-later
//! Page-find state remains attached to the original browser surface.
use super::*;
#[path = "browser_find_panel.rs"]
mod panel;
pub(crate) use panel::UiAction;

#[derive(Default)]
pub(super) struct State {
    query: String,
    backward: bool,
    case_sensitive: bool,
    wrap: bool,
    found: Option<bool>,
}
#[derive(Default)]
pub(crate) struct Controller {
    panel: Option<panel::Panel>,
    surface: Option<SurfaceId>,
    deferred_close: HashMap<SurfaceId, (u64, u64)>,
}
impl Controller {
    pub(crate) fn handle_message(&self, message: &MSG) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|p| p.handle_message(message))
    }
    pub(crate) fn layout(
        &self,
        id: SurfaceId,
        area: Option<model::Rect>,
        scale: f64,
        background: bool,
    ) -> i32 {
        let Some((panel, area)) = self
            .panel
            .as_ref()
            .filter(|p| self.surface == Some(id) && p.is_open())
            .zip(area)
        else {
            return 0;
        };
        let top = chrome::Chrome::height(scale);
        let height = panel::Panel::height(area.width, scale).min((area.height - top - 1).max(0));
        panel.place(area.width, top, height, background);
        height
    }
}
impl App {
    fn browser_find_available(&self, id: SurfaceId) -> anyhow::Result<()> {
        let browser = self.browsers.get(&id).context("browser was closed")?;
        anyhow::ensure!(browser.visible, "page find requires a visible browser tab");
        anyhow::ensure!(
            !browser.loading,
            "wait for browser navigation to finish before page find"
        );
        anyhow::ensure!(self.close_request.is_none(), "window is closing");
        Ok(())
    }
    fn browser_find_idle(&self, id: SurfaceId) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self
                .pending_browser
                .values()
                .any(|p| p.surface == id && p.response.is_action()),
            "wait for the pending browser action before page find"
        );
        Ok(())
    }
    pub(super) fn browser_find_status(&self, id: SurfaceId) -> Value {
        let state = &self.browsers[&id].find;
        let panel = self
            .browser_find
            .panel
            .as_ref()
            .filter(|_| self.browser_find.surface == Some(id));
        json!({"query":state.query,"found":state.found,
            "busy":self.pending_browser.values().any(|p|p.surface == id && p.response.is_find()),
            "panel_handle":panel.map(|p|p.window as usize),
            "panel_owner":panel.map(|p|p.owner() as usize),
            "panel_parent":panel.map(|p|p.parent() as usize),
            "panel_bounds":panel.map(|p|p.bounds()),
            "panel_controls":panel.map(|p|p.diagnostics()),
            "panel_query_handle":panel.map(|p|p.query_handle() as usize),
            "panel_status":panel.map(|p|p.status_text())})
    }
    pub(super) fn browser_find_show(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        self.browser_find_available(id)?;
        let parent = self.browsers[&id].holder.window;
        if self
            .browser_find
            .panel
            .as_ref()
            .is_none_or(|p| p.parent() != parent)
        {
            self.browser_find.panel = Some(panel::Panel::new(parent)?);
        }
        self.browser_find.deferred_close.remove(&id);
        self.browser_find.surface = Some(id);
        let panel = self.browser_find.panel.as_ref().unwrap();
        let state = &self.browsers[&id].find;
        panel.set_query(&state.query, state.case_sensitive);
        let pending = self
            .pending_browser
            .values()
            .find(|p| p.surface == id && p.response.is_find());
        panel.status(match pending.map(|p| &p.response) {
            Some(Response::FindClose) => "Closing search…",
            Some(_) => "Searching…",
            None => match state.found {
                Some(true) => "Match found",
                Some(false) => "No match",
                None => "Enter text and choose Next or Previous",
            },
        });
        panel.show(self.background_test);
        self.layout()
    }
    pub(super) fn browser_find_start(
        &mut self,
        id: SurfaceId,
        args: crate::browser_find::Args,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<()> {
        let crate::browser_find::Args {
            query,
            backward,
            case_sensitive,
            no_wrap,
            ..
        } = args;
        let wrap = !no_wrap;
        self.browser_find_available(id)?;
        self.browser_find_idle(id)?;
        anyhow::ensure!(
            self.browser_find.surface != Some(id)
                || self
                    .browser_find
                    .panel
                    .as_ref()
                    .is_none_or(|p| !p.composing()),
            "Finish composing text before searching"
        );
        let source = crate::browser_find::script(
            &self.browsers[&id].find_key,
            &query,
            backward,
            case_sensitive,
            wrap,
        )?;
        self.browser_script_optional(id, source, Response::Find, MAX_SCRIPT, reply)?;
        self.browsers.get_mut(&id).unwrap().find = State {
            query,
            backward,
            case_sensitive,
            wrap,
            found: None,
        };
        if self.browser_find.surface == Some(id) {
            if let Some(panel) = &self.browser_find.panel {
                let state = &self.browsers[&id].find;
                // Only an explicit request updates the edit, never periodic status.
                if panel.query() != state.query || panel.case_sensitive() != state.case_sensitive {
                    panel.set_query(&state.query, state.case_sensitive);
                }
                panel.status("Searching…");
            }
        }
        Ok(())
    }
    pub(super) fn browser_find_close(
        &mut self,
        id: SurfaceId,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<()> {
        self.browser_find_available(id)?;
        self.browser_find_idle(id)?;
        let source = crate::browser_find::close_script(&self.browsers[&id].find_key);
        self.browser_script_optional(id, source, Response::FindClose, MAX_SCRIPT, reply)?;
        self.browsers.get_mut(&id).unwrap().find.found = None;
        if self.browser_find.surface == Some(id) {
            self.browser_find.panel.take();
            self.browser_find.surface = None;
            self.layout()?;
        }
        Ok(())
    }
    pub(crate) fn browser_find_reset(&mut self, id: SurfaceId, reason: &str) {
        self.browser_find.deferred_close.remove(&id);
        if let Some(browser) = self.browsers.get_mut(&id) {
            browser.find.found = None;
        }
        if self.browser_find.surface == Some(id) {
            if let Some(panel) = self.browser_find.panel.take() {
                panel.status(reason);
            }
            self.browser_find.surface = None;
            if let Err(error) = self.layout() {
                report(&format!("page find layout: {error:#}"));
            }
        }
    }
    pub(super) fn browser_find_result(
        &mut self,
        id: SurfaceId,
        result: Value,
    ) -> anyhow::Result<Value> {
        let found = result["found"]
            .as_bool()
            .context("invalid page find result")?;
        let selection = result["selection"]
            .as_str()
            .context("invalid page find selection")?;
        let state = &mut self
            .browsers
            .get_mut(&id)
            .context("browser was closed")?
            .find;
        state.found = Some(found);
        Ok(
            json!({"surface":id,"query":state.query,"backward":state.backward,
            "case_sensitive":state.case_sensitive,"wrap":state.wrap,"found":found,"selection":selection}),
        )
    }
    pub(super) fn browser_find_feedback(&mut self, id: SurfaceId, result: &Value) {
        if self.browser_find.surface == Some(id) {
            if let Some(panel) = &self.browser_find.panel {
                panel.status(
                    result["error"]
                        .as_str()
                        .unwrap_or(if result["found"] == true {
                            "Match found"
                        } else {
                            "No match"
                        }),
                );
            }
        }
    }
    pub(super) fn browser_find_close_feedback(&mut self, id: SurfaceId, result: &Value) {
        if result.get("error").is_some() {
            self.browser_find_feedback(id, result);
        } else if self.browser_find.surface == Some(id) {
            if let Some(panel) = &self.browser_find.panel {
                panel.status("Enter text and choose Next or Previous");
            }
        }
    }
    pub(super) fn browser_find_deferred_close(&mut self, id: SurfaceId) {
        if self.browser_find_idle(id).is_err() {
            return;
        }
        if let Some((epoch, revision)) = self.browser_find.deferred_close.remove(&id) {
            if self.browsers.get(&id).is_some_and(|b| {
                b.visible
                    && b.epoch.load(Ordering::SeqCst) == epoch
                    && b.visibility_revision == revision
            }) {
                if let Err(error) = self.browser_find_close(id, None) {
                    report(&format!("page find close: {error}"));
                }
            }
        }
    }
    pub(crate) fn browser_find_ui(&mut self, action: UiAction) {
        let generation = match action {
            UiAction::Next(generation)
            | UiAction::Previous(generation)
            | UiAction::Close(generation)
            | UiAction::Layout(generation) => generation,
        };
        if self
            .browser_find
            .panel
            .as_ref()
            .is_none_or(|p| p.generation != generation)
        {
            return;
        }
        if matches!(action, UiAction::Layout(_)) {
            if let Some(panel) = &self.browser_find.panel {
                panel.layout();
            }
            return;
        }
        let result = if let Some(id) = self.browser_find.surface {
            if matches!(action, UiAction::Close(_)) {
                // Always let the user dismiss a stale/hidden panel. Only a current
                // visible document may have its owned selection cleared.
                let result = if self.browser_find_idle(id).is_err() {
                    if let Some(browser) = self.browsers.get(&id) {
                        self.browser_find.deferred_close.insert(
                            id,
                            (
                                browser.epoch.load(Ordering::SeqCst),
                                browser.visibility_revision,
                            ),
                        );
                    }
                    Ok(())
                } else {
                    self.browser_find_close(id, None)
                };
                self.browser_find.panel.take();
                self.browser_find.surface = None;
                result.and_then(|()| self.layout())
            } else {
                let panel = self.browser_find.panel.as_ref().unwrap();
                self.browser_find_start(
                    id,
                    crate::browser_find::Args {
                        pane: id.0,
                        query: panel.query(),
                        backward: matches!(action, UiAction::Previous(_)),
                        case_sensitive: panel.case_sensitive(),
                        no_wrap: false,
                    },
                    None,
                )
            }
        } else {
            Ok(())
        };
        if let Err(error) = result {
            if let Some(panel) = &self.browser_find.panel {
                panel.status(&error.to_string());
            }
        }
    }
}
