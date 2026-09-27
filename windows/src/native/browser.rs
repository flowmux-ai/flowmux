// SPDX-License-Identifier: GPL-3.0-or-later
//! Untrusted pages have a separate WebView2 profile and no terminal IPC bridge.
use super::*;
use crate::browser::{self as domain, Op};
use crate::browser_dom as dom;
use std::sync::atomic::{AtomicU64, Ordering};
#[path = "browser_chrome.rs"]
mod chrome;
#[path = "browser_wait.rs"]
pub(super) mod wait;
const MAX_SCRIPT: usize = 128 * 1024;
pub(super) enum Signal {
    Navigation(SurfaceId, u64),
    Loaded(SurfaceId, u64, Option<i32>),
    Denied(SurfaceId, String),
    Metadata(SurfaceId),
    Eval(Uuid, u64, String),
    Ui(SurfaceId, u16),
    WaitTick,
    WaitResult(Uuid, Uuid, u64, String),
}
enum Response {
    Eval,
    Action,
    Snapshot(Uuid),
    Query {
        snapshot: Option<Uuid>,
        kind: &'static str,
    },
}
pub(super) struct Pending {
    response: Response,
    limit: usize,
    surface: SurfaceId,
    epoch: u64,
    reply: ipc::Reply,
    started: Instant,
}
fn action_outcome(mut value: Value, action: bool) -> Value {
    if action {
        if let Some(error) = value["error"].as_str() {
            value["error"] = json!(format!("{error}; action may have executed (not retried)"));
        }
    }
    value
}
impl Pending {
    fn error(&self, reason: &str) -> Value {
        action_outcome(
            json!({"error":reason}),
            matches!(self.response, Response::Action),
        )
    }
}
pub(super) struct Browser {
    // Drop the view before its chrome/context/parent HWND.
    pub(super) view: WebView,
    chrome: chrome::Chrome,
    epoch: Arc<AtomicU64>,
    pub(super) visible: bool,
    url: String,
    title: String,
    loading: bool,
    back: bool,
    forward: bool,
    zoom: f64,
    error: Option<String>,
    refs: dom::Refs,
    dom_key: String,
}
impl Browser {
    pub(super) fn new(app: &mut App, id: SurfaceId) -> anyhow::Result<Self> {
        let chrome = chrome::Chrome::new(app.window, id)?;
        let epoch = Arc::new(AtomicU64::new(0));
        let nav_epoch = epoch.clone();
        let nav_sender = app.sender.clone();
        let load_sender = app.sender.clone();
        let title_sender = app.sender.clone();
        let background = app.background_test;
        if app.browser_context.is_none() {
            app.browser_context = Some(WebContext::new(Some(profile(background)?)));
        }
        let view = WebViewBuilder::new_with_web_context(app.browser_context.as_mut().unwrap())
            .with_url("about:blank")
            .with_visible(false)
            .with_focused(false)
            .with_clipboard(false)
            .with_devtools(false)
            .with_hotkeys_zoom(false)
            .with_document_title_changed_handler(move |_| {
                title_sender.send(Event::Browser(Signal::Metadata(id)))
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_download_started_handler(|_, _| false)
            .with_permission_handler(move |_| {
                if background {
                    wry::PermissionResponse::Deny
                } else {
                    wry::PermissionResponse::Default
                }
            })
            .build_as_child(&Parent(app.window))?;
        unsafe {
            let core = view.controller().CoreWebView2()?;
            let settings = core.Settings()?;
            settings.SetIsWebMessageEnabled(false)?;
            settings.SetAreHostObjectsAllowed(false)?;
            if background {
                settings.SetAreDefaultScriptDialogsEnabled(false)?;
            }
            // Use native navigation IDs. Wry's load callback reports the current
            // URL even for an older completion, so URL comparison cannot guard it.
            let mut token = 0;
            core.add_NavigationStarting(
                &webview2_com::NavigationStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let mut uri = Default::default();
                    args.Uri(&mut uri)?;
                    let uri = webview2_com::take_pwstr(uri);
                    if let Err(error) = domain::url(&uri) {
                        args.SetCancel(true)?;
                        nav_sender.send(Event::Browser(Signal::Denied(id, error.to_string())));
                        return Ok(());
                    }
                    let mut navigation = 0;
                    args.NavigationId(&mut navigation)?;
                    nav_epoch.store(navigation, Ordering::SeqCst);
                    nav_sender.send(Event::Browser(Signal::Navigation(id, navigation)));
                    Ok(())
                })),
                &mut token,
            )?;
            core.add_NavigationCompleted(
                &webview2_com::NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else {
                        return Ok(());
                    };
                    let mut navigation = 0;
                    args.NavigationId(&mut navigation)?;
                    let mut success = Default::default();
                    args.IsSuccess(&mut success)?;
                    let error = if success.as_bool() {
                        None
                    } else {
                        let mut status = Default::default();
                        args.WebErrorStatus(&mut status)?;
                        Some(status.0)
                    };
                    load_sender.send(Event::Browser(Signal::Loaded(id, navigation, error)));
                    Ok(())
                })),
                &mut token,
            )?;
        }
        Ok(Self {
            view,
            chrome,
            epoch,
            visible: false,
            url: "about:blank".into(),
            title: "Browser".into(),
            loading: false,
            back: false,
            forward: false,
            zoom: 1.0,
            error: None,
            refs: dom::Refs::new(id.0),
            dom_key: format!("__flowmuxDom_{}", Uuid::new_v4().simple()),
        })
    }
    fn refresh(&mut self) -> anyhow::Result<()> {
        self.url = self.view.url()?;
        unsafe {
            let core = self.view.controller().CoreWebView2()?;
            let mut title = Default::default();
            core.DocumentTitle(&mut title)?;
            self.title = webview2_com::take_pwstr(title)
                .chars()
                .filter(|c| !c.is_control())
                .take(1000)
                .collect();
            let mut back = Default::default();
            let mut forward = Default::default();
            core.CanGoBack(&mut back)?;
            core.CanGoForward(&mut forward)?;
            self.back = back.as_bool();
            self.forward = forward.as_bool();
        }
        self.chrome.update(
            &self.url,
            self.back,
            self.forward,
            self.error
                .as_deref()
                .unwrap_or(if self.loading { "Loading…" } else { "Ready" }),
        );
        Ok(())
    }
    pub(super) fn status(&self, id: SurfaceId) -> Value {
        json!({"id":id,"kind":"browser","url":self.url,"title":self.title,"loading":self.loading,"can_go_back":self.back,"can_go_forward":self.forward,"zoom":self.zoom,"generation":self.epoch.load(Ordering::SeqCst),"visible":self.visible,"navigation_error":self.error,"view_handle":self.view.hwnd().0 as usize,"chrome_handle":self.chrome.window as usize,"address_handle":self.chrome.address as usize})
    }
    pub(super) fn layout(&mut self, area: Option<model::Rect>, scale: f64) -> anyhow::Result<()> {
        let show = area.is_some();
        if self.visible != show {
            self.refs.clear();
            self.view.set_visible(show)?;
            unsafe {
                ShowWindow(self.chrome.window, if show { SW_SHOWNA } else { SW_HIDE });
            }
            self.visible = show;
        }
        if let Some(area) = area {
            self.chrome.layout(area, scale);
            let height = chrome::Chrome::height(scale);
            self.view.set_bounds(bounds(model::Rect {
                y: area.y + height,
                height: (area.height - height).max(1),
                ..area
            }))?;
        }
        Ok(())
    }
    fn navigate(&mut self, url: &str) -> anyhow::Result<()> {
        let url = domain::url(url)?;
        self.refs.clear();
        self.view.load_url(&url)?;
        self.loading = true;
        self.error = None;
        Ok(())
    }
    fn operation(&mut self, action: u16) -> anyhow::Result<()> {
        self.error = None;
        if matches!(action, 1..=3) {
            self.refs.clear();
        }
        match action {
            1 => unsafe {
                self.view.controller().CoreWebView2()?.GoBack()?;
            },
            2 => unsafe {
                self.view.controller().CoreWebView2()?.GoForward()?;
            },
            3 => self.view.reload()?,
            4 => {
                unsafe {
                    self.view.controller().CoreWebView2()?.Stop()?;
                }
                self.loading = false;
            }
            5 => self.navigate(&self.chrome.address())?,
            6 => self.zoom((self.zoom - 0.1).max(0.5))?,
            7 => self.zoom((self.zoom + 0.1).min(3.0))?,
            8 => self.zoom(1.0)?,
            _ => anyhow::bail!("unknown browser control"),
        }
        Ok(())
    }
    fn zoom(&mut self, scale: f64) -> anyhow::Result<()> {
        anyhow::ensure!(
            scale.is_finite() && (0.5..=3.0).contains(&scale),
            "browser zoom must be between 0.5 and 3.0"
        );
        self.view.zoom(scale)?;
        self.zoom = scale;
        Ok(())
    }
}
pub(super) fn profile(background: bool) -> anyhow::Result<PathBuf> {
    let root = if background {
        PathBuf::from(
            std::env::var_os("FLOWMUX_TEST_STATE_DIR")
                .context("background browser requires an isolated state directory")?,
        )
    } else {
        data_dir()?
    };
    let path = root.join("browser-profile");
    std::fs::create_dir_all(&path)?;
    Ok(path)
}
impl App {
    pub(super) fn open_browser(
        &mut self,
        source: SurfaceId,
        url: String,
        down: bool,
    ) -> anyhow::Result<Value> {
        let url = domain::url(&url)?;
        let (index, pane, _) = self.locate(source).context("source pane not found")?;
        let mut candidate = self.workspaces[index].clone();
        let opened = domain::open(&mut candidate, pane, url.clone(), down)?;
        let mut browser = Browser::new(self, opened.surface)?;
        browser.navigate(&url)?;
        self.workspaces[index] = candidate;
        self.browsers.insert(opened.surface, browser);
        self.active_workspace = index;
        self.zoomed = None;
        self.rebuild()?;
        Ok(
            json!({"browser_pane_opened":{"pane":opened.pane,"surface":opened.surface,"placement_strategy":opened.placement}}),
        )
    }
    pub(super) fn add_browser_view(&mut self, id: SurfaceId, url: String) -> anyhow::Result<()> {
        let mut browser = Browser::new(self, id)?;
        browser.navigate(&url)?;
        self.browsers.insert(id, browser);
        Ok(())
    }
    fn browser_refresh(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        let Some(browser) = self.browsers.get_mut(&id) else {
            return Ok(());
        };
        browser.refresh()?;
        let url = browser.url.clone();
        let title = browser.title.clone();
        if let Some((ws, pane, _)) = self.locate(id) {
            if let Ok(url) = domain::url(&url) {
                self.workspaces[ws]
                    .root
                    .set_surface_browser_url(pane, id, url);
            }
            if !title.is_empty() {
                self.workspaces[ws]
                    .root
                    .set_surface_title_auto(pane, id, title);
                self.refresh_tab_title(id);
            }
        }
        Ok(())
    }
    pub(super) fn browser_cancel(&mut self, id: SurfaceId, reason: &str) {
        self.browser_wait_cancel(id, reason);
        self.pending_browser.retain(|_, pending| {
            if pending.surface == id {
                let _ = pending.reply.try_send(pending.error(reason));
                false
            } else {
                true
            }
        });
    }
    pub(super) fn browser_tick(&mut self) {
        self.pending_browser.retain(|_, p| {
            if p.started.elapsed() > Duration::from_secs(12) {
                let _ = p.reply.try_send(
                    p.error("browser script callback timed out; script may have executed"),
                );
                false
            } else {
                true
            }
        });
        let ids: Vec<_> = self.browsers.keys().copied().collect();
        for id in ids {
            // A failed browser controller must not stop terminal expiry, search
            // maintenance or periodic checkpoint handling on the host timer.
            if let Err(error) = self.browser_refresh(id) {
                if let Some(browser) = self.browsers.get_mut(&id) {
                    let error = error.to_string();
                    if browser.error.as_deref() != Some(error.as_str()) {
                        report(&format!("browser {id}: {error}"));
                    }
                    browser.error = Some(error);
                }
            }
        }
    }
    pub(super) fn browser_event(&mut self, event: Signal) -> anyhow::Result<()> {
        match event {
            Signal::WaitTick => self.browser_wait_tick(),
            Signal::WaitResult(id, poll, epoch, result) => {
                self.browser_wait_result(id, poll, epoch, result)
            }
            Signal::Navigation(id, navigation) => {
                let epoch = self
                    .browsers
                    .get(&id)
                    .map(|b| b.epoch.load(Ordering::SeqCst));
                self.pending_browser.retain(|_, p| {
                    if p.surface == id && Some(p.epoch) != epoch {
                        let _ = p
                            .reply
                            .try_send(p.error("browser navigated during script request"));
                        false
                    } else {
                        true
                    }
                });
                if epoch == Some(navigation) {
                    if let Some(browser) = self.browsers.get_mut(&id) {
                        browser.refs.clear();
                        browser.loading = true;
                        browser.error = None;
                    }
                }
            }
            Signal::Loaded(id, navigation, error) => {
                if let Some(browser) = self.browsers.get_mut(&id) {
                    if browser.epoch.load(Ordering::SeqCst) == navigation {
                        browser.loading = false;
                        browser.error = error.map(|code| {
                            format!("Navigation failed or stopped (WebView2 error {code})")
                        });
                    }
                }
                self.browser_refresh(id)?;
            }
            Signal::Denied(id, error) => {
                if let Some(browser) = self.browsers.get_mut(&id) {
                    browser.error = Some(error);
                }
                self.browser_refresh(id)?;
            }
            Signal::Metadata(id) => self.browser_refresh(id)?,
            Signal::Ui(id, action) => {
                if self.close_request.is_some() {
                    return Ok(());
                }
                if let Some(browser) = self.browsers.get_mut(&id) {
                    if let Err(e) = browser.operation(action) {
                        browser.error = Some(e.to_string());
                    }
                }
                self.browser_refresh(id)?;
            }
            Signal::Eval(request, epoch, result) => {
                if let Some(p) = self.pending_browser.remove(&request) {
                    let action = matches!(p.response, Response::Action);
                    let reply = if p.started.elapsed() > Duration::from_secs(12) {
                        json!({"error":"browser script callback timed out; script may have executed"})
                    } else if epoch != p.epoch
                        || self
                            .browsers
                            .get(&p.surface)
                            .is_none_or(|b| b.epoch.load(Ordering::SeqCst) != epoch)
                    {
                        json!({"error":"browser document changed during script request"})
                    } else if result.len() > p.limit {
                        json!({"error":"browser response exceeds size limit"})
                    } else {
                        match serde_json::from_str::<Value>(&result) {
                            Ok(value) if value.is_object() => self
                                .browser_result(p.surface, p.response, value)
                                .unwrap_or_else(|error| json!({"error":error.to_string()})),
                            _ => json!({"error":"invalid browser script result"}),
                        }
                    };
                    let _ = p.reply.try_send(action_outcome(reply, action));
                }
            }
        }
        Ok(())
    }
    fn browser_result(
        &mut self,
        id: SurfaceId,
        response: Response,
        value: Value,
    ) -> anyhow::Result<Value> {
        if value.get("error").is_some() {
            return Ok(value);
        }
        let result = value
            .get("result")
            .context("browser response missing result")?
            .clone();
        match response {
            Response::Eval => Ok(value),
            Response::Action => {
                anyhow::ensure!(result == "ok", "invalid browser action result");
                Ok(json!({"ok":true,"surface":id}))
            }
            Response::Snapshot(request) => {
                let browser = self.browsers.get_mut(&id).context("browser was closed")?;
                let mut result = browser
                    .refs
                    .publish(request, result, &mut self.browser_tokens)?;
                result["surface"] = json!(id);
                Ok(result)
            }
            Response::Query { snapshot, kind } => {
                if let Some(snapshot) = snapshot {
                    anyhow::ensure!(
                        self.browsers
                            .get(&id)
                            .is_some_and(|b| b.refs.current == Some(snapshot)),
                        "snapshot was superseded or hidden"
                    );
                }
                let valid = match kind {
                    "count" => result.is_u64(),
                    "is_visible" | "is_enabled" | "is_checked" => result.is_boolean(),
                    _ => result.is_string(),
                };
                anyhow::ensure!(valid, "invalid DOM query result type");
                Ok(json!({"result":result,"surface":id}))
            }
        }
    }
    fn browser_script(
        &mut self,
        id: SurfaceId,
        source: String,
        response: Response,
        limit: usize,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        let browser = self.browsers.get(&id).context("browser was closed")?;
        anyhow::ensure!(
            !browser.loading,
            "wait for browser navigation to finish before evaluating a script"
        );
        anyhow::ensure!(source.len() <= MAX_SCRIPT, "browser script exceeds 128 KiB");
        anyhow::ensure!(
            self.pending_browser.len() < 16,
            "too many pending browser scripts"
        );
        let epoch = browser.epoch.load(Ordering::SeqCst);
        let request = Uuid::new_v4();
        let sender = self.sender.clone();
        let source = serde_json::to_string(&source)?;
        let script=format!("(()=>{{try{{const result=(0,eval)({source});if(result&&typeof result.then==='function')throw new Error('asynchronous scripts are not supported');const out={{result:result===undefined?null:result}};if(JSON.stringify(out).length>{limit})throw new Error('browser response exceeds size limit');return out;}}catch(e){{return {{error:String(e).slice(0,4096)}};}}}})()");
        browser
            .view
            .evaluate_script_with_callback(&script, move |result| {
                sender.send(Event::Browser(Signal::Eval(
                    request,
                    epoch,
                    if result.len() > limit {
                        "{\"error\":\"browser response exceeds size limit\"}".into()
                    } else {
                        result
                    },
                )))
            })?;
        self.pending_browser.insert(
            request,
            Pending {
                surface: id,
                epoch,
                reply,
                started: Instant::now(),
                response,
                limit,
            },
        );
        Ok(())
    }
    pub(super) fn browser_command(
        &mut self,
        op: Op,
        caller: Option<SurfaceId>,
        reply: ipc::Reply,
    ) -> anyhow::Result<Option<Value>> {
        if let Op::Open {
            url,
            pane,
            right,
            down,
        } = op
        {
            anyhow::ensure!(!(right && down), "choose right or down");
            let source = self.target(pane, caller)?;
            return self.open_browser(source, url, down).map(Some);
        }
        let pane = match &op {
            Op::Navigate { pane, .. }
            | Op::Back { pane }
            | Op::Forward { pane }
            | Op::Reload { pane }
            | Op::Stop { pane }
            | Op::Url { pane }
            | Op::Title { pane }
            | Op::Status { pane }
            | Op::Zoom { pane, .. }
            | Op::Eval { pane, .. }
            | Op::Snapshot { pane }
            | Op::Text { pane, .. }
            | Op::Value { pane, .. }
            | Op::Attr { pane, .. }
            | Op::IsVisible { pane, .. }
            | Op::IsEnabled { pane, .. }
            | Op::IsChecked { pane, .. }
            | Op::Count { pane, .. }
            | Op::Wait { pane, .. } => *pane,
            Op::Click(args)
            | Op::Dblclick(args)
            | Op::Hover(args)
            | Op::Focus(args)
            | Op::Blur(args)
            | Op::Check(args)
            | Op::Uncheck(args) => args.pane,
            Op::Fill(args) | Op::Select(args) => args.pane,
            Op::Scroll(args) => args.pane,
            Op::Open { .. } => unreachable!(),
        };
        let id = self.target(Some(pane), None)?;
        anyhow::ensure!(
            self.browsers.contains_key(&id),
            "pane has no active browser tab"
        );
        let action_pending = self
            .pending_browser
            .values()
            .any(|p| p.surface == id && matches!(p.response, Response::Action));
        if op.is_action() || matches!(op, Op::Snapshot { .. }) {
            anyhow::ensure!(
                !action_pending,
                "wait for the pending browser action before taking a snapshot or another action"
            );
        }
        self.browser_refresh(id)?;
        if let Some((target, action)) = crate::browser_action::from_op(&op) {
            anyhow::ensure!(
                !self.background_test || action.allowed_in_background(),
                "DOM focus/blur is disabled in background test hosts"
            );
            let browser = &self.browsers[&id];
            let source = browser.refs.action(&browser.dom_key, target, &action)?;
            self.browser_script(id, source, Response::Action, MAX_SCRIPT, reply)?;
            // DOM revision checks preserve the existing ref contract: explicit
            // repeated actions remain valid while the same snapshot/DOM is current.
            // The transport never retries an action after an uncertain response.
            return Ok(None);
        }
        let browser = self.browsers.get_mut(&id).unwrap();
        let query_kind = match &op {
            Op::Text { .. } => Some(("text", None)),
            Op::Value { .. } => Some(("value", None)),
            Op::Attr { name, .. } => Some(("attr", Some(name.clone()))),
            Op::IsVisible { .. } => Some(("is_visible", None)),
            Op::IsEnabled { .. } => Some(("is_enabled", None)),
            Op::IsChecked { .. } => Some(("is_checked", None)),
            _ => None,
        };
        match op {
            Op::Wait { options, .. } => {
                self.browser_wait_start(id, options, reply)?;
                return Ok(None);
            }
            Op::Navigate { url, .. } => browser.navigate(&url)?,
            Op::Back { .. } => browser.operation(1)?,
            Op::Forward { .. } => browser.operation(2)?,
            Op::Reload { .. } => browser.operation(3)?,
            Op::Stop { .. } => browser.operation(4)?,
            Op::Url { .. } => return Ok(Some(json!({"url":browser.url}))),
            Op::Title { .. } => return Ok(Some(json!({"title":browser.title}))),
            Op::Status { .. } => {
                let mut status = browser.status(id);
                status["action_pending"] = json!(action_pending);
                return Ok(Some(status));
            }
            Op::Zoom { scale, .. } => browser.zoom(scale)?,
            Op::Eval { source, .. } => {
                self.browser_script(id, source, Response::Eval, MAX_SCRIPT, reply)?;
                return Ok(None);
            }
            Op::Snapshot { .. } => {
                let request = browser.refs.begin();
                let source = dom::snapshot(&browser.dom_key);
                self.browser_script(
                    id,
                    source,
                    Response::Snapshot(request),
                    dom::MAX_SNAPSHOT,
                    reply,
                )?;
                return Ok(None);
            }
            Op::Count { selector, .. } => {
                self.browser_script(
                    id,
                    dom::count(&selector)?,
                    Response::Query {
                        snapshot: None,
                        kind: "count",
                    },
                    MAX_SCRIPT,
                    reply,
                )?;
                return Ok(None);
            }
            Op::Text { target, .. }
            | Op::Value { target, .. }
            | Op::Attr { target, .. }
            | Op::IsVisible { target, .. }
            | Op::IsEnabled { target, .. }
            | Op::IsChecked { target, .. } => {
                let (kind, name) = query_kind.context("missing DOM query kind")?;
                let (snapshot, source) =
                    browser
                        .refs
                        .query(&browser.dom_key, &target, kind, name.as_deref())?;
                self.browser_script(
                    id,
                    source,
                    Response::Query {
                        snapshot: Some(snapshot),
                        kind,
                    },
                    MAX_SCRIPT,
                    reply,
                )?;
                return Ok(None);
            }
            Op::Open { .. } => unreachable!(),
            Op::Click(..)
            | Op::Dblclick(..)
            | Op::Hover(..)
            | Op::Focus(..)
            | Op::Blur(..)
            | Op::Scroll(..)
            | Op::Fill(..)
            | Op::Select(..)
            | Op::Check(..)
            | Op::Uncheck(..) => unreachable!(),
        }
        self.browser_refresh(id)?;
        Ok(Some(json!({"ok":true,"surface":id})))
    }
}
