// SPDX-License-Identifier: GPL-3.0-or-later
//! Untrusted pages have a separate WebView2 profile and no terminal IPC bridge.
use super::*;
use crate::browser::{self as domain, Op};
use crate::browser_dom as dom;
use std::sync::atomic::{AtomicU64, Ordering};
use std::{cell::Cell, rc::Rc};
use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Environment;
use wry::WebViewBuilderExtWindows;
#[path = "browser_capture.rs"]
pub(super) mod capture;
#[path = "browser_chrome.rs"]
mod chrome;
#[path = "browser_find.rs"]
pub(super) mod find;
#[path = "browser_popup.rs"]
pub(super) mod popup;
#[path = "browser_popup_host.rs"]
mod popup_host;
#[path = "browser_wait.rs"]
pub(super) mod wait;
const MAX_SCRIPT: usize = 128 * 1024;
pub(super) enum Signal {
    Popup,
    PopupClose(SurfaceId, Uuid),
    Navigation(SurfaceId, u64),
    Loaded(SurfaceId, u64, Option<i32>),
    Denied(SurfaceId, String),
    Metadata(SurfaceId),
    Eval(Uuid, u64, String),
    Ui(SurfaceId, u16),
    Capture(Uuid, Result<Vec<u8>, String>),
    CaptureSaved(Uuid, Result<(), String>),
    WaitTick,
    WaitResult(Uuid, Uuid, u64, String),
}
enum Response {
    Eval,
    Action,
    Find,
    FindClose,
    Snapshot(Uuid),
    Query {
        snapshot: Option<Uuid>,
        kind: &'static str,
    },
}
impl Response {
    fn is_action(&self) -> bool {
        matches!(self, Self::Action | Self::Find | Self::FindClose)
    }
    fn is_find(&self) -> bool {
        matches!(self, Self::Find | Self::FindClose)
    }
}
pub(super) struct Pending {
    response: Response,
    limit: usize,
    surface: SurfaceId,
    epoch: u64,
    visibility_revision: u64,
    reply: Option<ipc::Reply>,
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
    fn send(&self, value: Value) {
        if let Some(reply) = &self.reply {
            let _ = reply.try_send(value);
        }
    }
    fn error(&self, reason: &str) -> Value {
        action_outcome(json!({"error":reason}), self.response.is_action())
    }
}
pub(super) struct Browser {
    // Drop the view before its chrome/context/parent HWND.
    pub(super) view: WebView,
    chrome: chrome::Chrome,
    epoch: Arc<AtomicU64>,
    pub(super) visible: bool,
    visibility_revision: u64,
    popup_visibility: Rc<Cell<Option<u64>>>,
    popup_opener: Option<SurfaceId>,
    popup_user_initiated: Option<bool>,
    instance: Uuid,
    pub(super) native_closed: Rc<Cell<bool>>,
    url: String,
    title: String,
    loading: bool,
    back: bool,
    forward: bool,
    zoom: f64,
    error: Option<String>,
    refs: dom::Refs,
    dom_key: String,
    find_key: String,
    find: find::State,
    viewport_revision: u64,
    viewport: Option<(i32, i32, i32, i32)>,
}
impl Browser {
    pub(super) fn new(app: &mut App, id: SurfaceId) -> anyhow::Result<Self> {
        Self::new_in_environment(app, id, None)
    }
    fn new_in_environment(
        app: &mut App,
        id: SurfaceId,
        environment: Option<ICoreWebView2Environment>,
    ) -> anyhow::Result<Self> {
        let chrome = chrome::Chrome::new(app.window, id)?;
        let instance = Uuid::new_v4();
        let popup_visibility = Rc::new(Cell::new(None));
        let native_closed = Rc::new(Cell::new(false));
        let nav_closed = native_closed.clone();
        let epoch = Arc::new(AtomicU64::new(0));
        let nav_epoch = epoch.clone();
        let nav_sender = app.sender.clone();
        let load_sender = app.sender.clone();
        let title_sender = app.sender.clone();
        let background = app.background_test;
        if app.browser_context.is_none() {
            app.browser_context = Some(WebContext::new(Some(profile(background)?)));
        }
        let mut builder =
            WebViewBuilder::new_with_web_context(app.browser_context.as_mut().unwrap())
                .with_visible(false)
                .with_focused(false)
                .with_clipboard(false)
                .with_devtools(false)
                .with_hotkeys_zoom(false)
                .with_document_title_changed_handler(move |_| {
                    title_sender.send(Event::Browser(Signal::Metadata(id)))
                })
                .with_permission_handler(move |_| {
                    if background {
                        wry::PermissionResponse::Deny
                    } else {
                        wry::PermissionResponse::Default
                    }
                });
        if let Some(environment) = environment {
            builder = builder.with_environment(environment);
        }
        let view = builder.build_as_child(&Parent(app.window))?;
        unsafe {
            let core = view.controller().CoreWebView2()?;
            app.downloads
                .install(&core, id, epoch.clone(), app.sender.clone())?;
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
                    if nav_closed.get() {
                        args.SetCancel(true)?;
                        return Ok(());
                    }
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
        unsafe {
            let core = view.controller().CoreWebView2()?;
            let close_sender = app.sender.clone();
            let visibility = popup_visibility.clone();
            let closed = native_closed.clone();
            let close_epoch = epoch.clone();
            core.add_WindowCloseRequested(
                &webview2_com::WindowCloseRequestedEventHandler::create(Box::new(move |_, _| {
                    closed.set(true);
                    // Existing script/wait/capture generation guards must reject
                    // results even before the queued model cleanup runs.
                    close_epoch.fetch_add(1, Ordering::SeqCst);
                    visibility.set(None);
                    close_sender.send(Event::Browser(Signal::PopupClose(id, instance)));
                    Ok(())
                })),
                &mut 0,
            )?;
            app.browser_popups.install(
                &core,
                id,
                epoch.clone(),
                popup_visibility.clone(),
                app.sender.clone(),
            )?;
        }
        Ok(Self {
            view,
            popup_visibility,
            popup_opener: None,
            popup_user_initiated: None,
            instance,
            native_closed,
            chrome,
            epoch,
            visible: false,
            visibility_revision: 0,
            url: "about:blank".into(),
            title: "Browser".into(),
            loading: false,
            back: false,
            forward: false,
            zoom: 1.0,
            error: None,
            refs: dom::Refs::new(id.0),
            viewport_revision: 0,
            viewport: None,
            find_key: format!("__flowmuxFind_{}", Uuid::new_v4().simple()),
            find: find::State::default(),
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
            self.loading,
        );
        Ok(())
    }
    pub(super) fn status(&self, id: SurfaceId) -> Value {
        json!({"id":id,"kind":"browser","url":self.url,"title":self.title,"loading":self.loading,"can_go_back":self.back,"can_go_forward":self.forward,"zoom":self.zoom,"generation":self.epoch.load(Ordering::SeqCst),"visible":self.visible,"popup_opener":self.popup_opener,"popup_user_initiated":self.popup_user_initiated,"native_closed":self.native_closed.get(),"navigation_error":self.error,"view_handle":self.view.hwnd().0 as usize,"chrome_handle":self.chrome.window as usize,"chrome":self.chrome.diagnostics(),"address_handle":self.chrome.address as usize})
    }
    pub(super) fn layout(&mut self, area: Option<model::Rect>, scale: f64) -> anyhow::Result<()> {
        if self.native_closed.get() {
            return Ok(());
        }
        let viewport = area.map(|r| (r.x, r.y, r.width, r.height));
        if self.viewport != viewport {
            self.viewport_revision = self.viewport_revision.wrapping_add(1);
            self.viewport = viewport;
        }
        let show = area.is_some();
        if self.visible != show {
            self.visibility_revision = self.visibility_revision.wrapping_add(1);
            self.refs.clear();
            self.view.set_visible(show)?;
            unsafe {
                ShowWindow(self.chrome.window, if show { SW_SHOWNA } else { SW_HIDE });
            }
            self.visible = show;
            self.popup_visibility
                .set(show.then_some(self.visibility_revision));
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
        if self.zoom != scale {
            self.viewport_revision = self.viewport_revision.wrapping_add(1);
        }
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
        if let Err(error) = browser.navigate(&url) {
            self.browser_cancel(
                opened.surface,
                "browser navigation failed during construction",
            );
            return Err(error);
        }
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
        if let Err(error) = browser.navigate(&url) {
            self.browser_cancel(id, "browser navigation failed during construction");
            return Err(error);
        }
        self.browsers.insert(id, browser);
        Ok(())
    }
    fn browser_refresh(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        let Some(browser) = self.browsers.get_mut(&id) else {
            return Ok(());
        };
        if browser.native_closed.get() {
            return Ok(());
        }
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
        self.browser_popups.cancel_surface(id);
        self.browser_find_reset(id, reason);
        self.browser_wait_cancel(id, reason);
        self.browser_capture_cancel(id, reason);
        self.download_cancel_surface(id);
        self.pending_browser.retain(|_, pending| {
            if pending.surface == id {
                pending.send(pending.error(reason));
                false
            } else {
                true
            }
        });
    }
    pub(super) fn browser_tick(&mut self) {
        self.browser_capture_tick();
        self.download_tick();
        let now = Instant::now();
        let expired_actions: Vec<_> = self
            .pending_browser
            .values()
            .filter(|p| {
                p.response.is_action() && now.duration_since(p.started) > Duration::from_secs(12)
            })
            .map(|p| (p.surface, p.response.is_find()))
            .collect();
        for (id, find) in &expired_actions {
            if *find {
                self.browser_find_feedback(
                    *id,
                    &json!({"error":"Page find timed out; action may have executed (not retried)"}),
                );
            }
        }
        self.pending_browser.retain(|_, p| {
            if now.duration_since(p.started) > Duration::from_secs(12) {
                p.send(p.error("browser script callback timed out; script may have executed"));
                false
            } else {
                true
            }
        });
        for (id, _) in expired_actions {
            self.browser_find_deferred_close(id);
        }
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
            Signal::Popup => self.browser_popup_dispatch(),
            Signal::PopupClose(id, instance) => self.browser_popup_close(id, instance)?,
            Signal::Capture(id, result) => self.browser_capture_result(id, result),
            Signal::CaptureSaved(id, result) => self.browser_capture_saved(id, result),
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
                        p.send(p.error("browser navigated during script request"));
                        false
                    } else {
                        true
                    }
                });
                if epoch == Some(navigation) {
                    self.browser_find_reset(id, "Page changed");
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
            Signal::Ui(id, mut action) => {
                if action == 11 {
                    let Some(browser) = self.browsers.get(&id) else {
                        return Ok(());
                    };
                    let labels = [
                        "Back",
                        "Forward",
                        "Reload",
                        "Stop",
                        "Go",
                        "Zoom out",
                        "Zoom in",
                        "Reset zoom",
                        "Downloads…",
                        "Find in page…",
                    ];
                    let disabled = [
                        (!browser.back).then_some(1),
                        (!browser.forward).then_some(2),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                    action = self.popup(&labels, &disabled, browser.chrome.tools_anchor())? as u16;
                    if action == 0 {
                        return Ok(());
                    }
                }
                if action == 10 {
                    self.browser_find_show(id)?;
                    return Ok(());
                }
                if action == 9 {
                    self.download_ui(downloads::UiAction::Show)?;
                    return Ok(());
                }
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
                    let action = p.response.is_action();
                    let find = p.response.is_find();
                    let find_close = matches!(&p.response, Response::FindClose);
                    let reply = if p.started.elapsed() > Duration::from_secs(12) {
                        json!({"error":"browser script callback timed out; script may have executed"})
                    } else if epoch != p.epoch
                        || self.browsers.get(&p.surface).is_none_or(|b| {
                            b.native_closed.get() || b.epoch.load(Ordering::SeqCst) != epoch
                        })
                    {
                        json!({"error":"browser document changed during script request"})
                    } else if find
                        && self.browsers.get(&p.surface).is_none_or(|b| {
                            !b.visible || b.visibility_revision != p.visibility_revision
                        })
                    {
                        json!({"error":"browser tab was hidden during page find"})
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
                    if find_close {
                        self.browser_find_close_feedback(p.surface, &reply);
                    } else if find {
                        self.browser_find_feedback(p.surface, &reply);
                    }
                    self.browser_find_deferred_close(p.surface);
                    if let Some(sender) = p.reply {
                        let _ = sender.try_send(action_outcome(reply, action));
                    }
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
            Response::Find => self.browser_find_result(id, result),
            Response::FindClose => {
                let cleared = result["cleared"]
                    .as_bool()
                    .context("invalid page find close result")?;
                Ok(json!({"ok":true,"surface":id,"cleared":cleared}))
            }
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
        self.browser_script_optional(id, source, response, limit, Some(reply))
    }
    fn browser_script_optional(
        &mut self,
        id: SurfaceId,
        source: String,
        response: Response,
        limit: usize,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<()> {
        let browser = self.browsers.get(&id).context("browser was closed")?;
        anyhow::ensure!(!browser.native_closed.get(), "browser window has closed");
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
                visibility_revision: browser.visibility_revision,
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
            Op::Find(args) => args.pane,
            Op::FindShow(args) | Op::FindClose(args) => args.pane,
            Op::Screenshot(args) => args.pane,
            Op::Open { .. } => unreachable!(),
        };
        let id = self.target(Some(pane), None)?;
        anyhow::ensure!(
            self.browsers.contains_key(&id),
            "pane has no active browser tab"
        );
        anyhow::ensure!(
            !self.browsers[&id].native_closed.get(),
            "browser window has closed"
        );
        let action_pending = self
            .pending_browser
            .values()
            .any(|p| p.surface == id && p.response.is_action());
        if op.is_action()
            || matches!(
                op,
                Op::Snapshot { .. } | Op::Screenshot(..) | Op::Find(..) | Op::FindClose(..)
            )
        {
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
        match &op {
            Op::Find(args) => {
                self.browser_find_start(id, args.clone(), Some(reply))?;
                return Ok(None);
            }
            Op::FindShow(..) => {
                self.browser_find_show(id)?;
                return Ok(Some(json!({"ok":true,"surface":id})));
            }
            Op::FindClose(..) => {
                self.browser_find_close(id, Some(reply))?;
                return Ok(None);
            }
            _ => {}
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
            Op::Screenshot(args) => {
                self.browser_capture_start(id, args.path, reply)?;
                return Ok(None);
            }
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
                status["find"] = self.browser_find_status(id);
                status["action_pending"] = json!(action_pending);
                status["captures_pending"] = json!(self.pending_captures.len());
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
            Op::Find(..) | Op::FindShow(..) | Op::FindClose(..) | Op::Open { .. } => {
                unreachable!()
            }
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
