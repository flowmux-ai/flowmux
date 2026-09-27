// SPDX-License-Identifier: GPL-3.0-or-later
use super::{
    checked, data_dir, ipc,
    session::{Session, SessionEvent},
    wide,
};
use crate::{
    command::{key_bytes, Command, Direction},
    model::{self, Workspace},
    protocol::{ClientMessage, HostMessage, Identity, TERMINAL_ORIGIN},
};
use anyhow::Context;
use base64::Engine;
use flowmux_core::{PaneId, SplitDirection, SurfaceId};
use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    num::NonZeroIsize,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{Com::*, LibraryLoader::*},
    UI::{HiDpi::*, WindowsAndMessaging::*},
};
use wry::{WebContext, WebView, WebViewBuilder};

const WAKE: u32 = WM_APP + 1;
thread_local! { static EVENTS: RefCell<Option<EventSender>> = const { RefCell::new(None) }; }
enum Event {
    Layout,
    Tick,
    Close,
    Button(u16),
    Bridge(SurfaceId, String, String),
    Session(SurfaceId, SessionEvent),
    Command(Command, ipc::Reply),
}
#[derive(Clone)]
struct EventSender {
    window: isize,
    sender: Sender<Event>,
}
impl EventSender {
    fn send(&self, event: Event) {
        if self.sender.send(event).is_ok() {
            unsafe {
                PostMessageW(self.window as HWND, WAKE, 0, 0);
            }
        }
    }
}
fn post(event: Event) {
    EVENTS.with(|slot| {
        if let Some(sender) = slot.borrow().as_ref() {
            sender.send(event);
        }
    });
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SIZE => {
            post(Event::Layout);
            0
        }
        WM_DPICHANGED => {
            let rect = &*(lparam as *const RECT);
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            post(Event::Layout);
            0
        }
        WM_COMMAND => {
            if (wparam >> 16) == 0 {
                post(Event::Button((wparam & 0xffff) as u16));
            }
            0
        }
        WM_CLOSE => {
            post(Event::Close);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        WM_TIMER => {
            post(Event::Tick);
            0
        }
        WAKE => 0,
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

struct Parent(HWND);
impl HasWindowHandle for Parent {
    fn window_handle(&self) -> Result<WindowHandle<'_>, raw_window_handle::HandleError> {
        let handle = Win32WindowHandle::new(
            NonZeroIsize::new(self.0 as isize)
                .ok_or(raw_window_handle::HandleError::Unavailable)?,
        );
        Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Win32(handle)) })
    }
}
struct Surface {
    view: WebView,
    identity: Identity,
    session: Option<Session>,
    cols: u16,
    rows: u16,
    ready: bool,
    visible: bool,
}
impl Surface {
    fn send(&self, message: &HostMessage) -> anyhow::Result<()> {
        self.view.evaluate_script(&format!(
            "window.flowmuxHost({})",
            serde_json::to_string(message)?
        ))?;
        Ok(())
    }
}
#[derive(Clone)]
enum Action {
    NewWorkspace,
    Workspace(usize),
    NewTab,
    Vertical,
    Horizontal,
    CloseTab,
    Tab(PaneId, SurfaceId),
}
struct Control {
    hwnd: HWND,
    action: Action,
}
struct PendingRead {
    surface: SurfaceId,
    after: u64,
    reply: ipc::Reply,
    started: Instant,
}
struct App {
    window: HWND,
    sender: EventSender,
    _ipc: ipc::Server,
    context: WebContext,
    workspaces: Vec<Workspace>,
    active_workspace: usize,
    surfaces: HashMap<SurfaceId, Surface>,
    controls: Vec<Control>,
    pending_reads: HashMap<Uuid, PendingRead>,
    closing: bool,
}

pub fn run(cwd: Option<PathBuf>) -> anyhow::Result<()> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let result = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        anyhow::ensure!(
            result >= 0,
            "Cannot initialize the Windows UI apartment: 0x{result:08x}"
        );
        let instance = GetModuleHandleW(std::ptr::null());
        let class = wide("flowmux.windows.native");
        let spec = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        anyhow::ensure!(
            RegisterClassW(&spec) != 0,
            "Cannot register window class: {}",
            std::io::Error::last_os_error()
        );
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            wide("flowmux — Windows").as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1200,
            800,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        anyhow::ensure!(
            !window.is_null(),
            "Cannot create window: {}",
            std::io::Error::last_os_error()
        );
        let (sender, events) = mpsc::channel();
        let sender = EventSender {
            window: window as isize,
            sender,
        };
        EVENTS.with(|slot| *slot.borrow_mut() = Some(sender.clone()));
        let dispatch = sender.clone();
        let shutdown = sender.clone();
        let ipc = ipc::Server::start(
            move |command, reply| dispatch.send(Event::Command(command, reply)),
            move || shutdown.send(Event::Close),
        )?;
        let cwd = cwd
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .context("No initial working directory")?;
        anyhow::ensure!(
            cwd.is_dir(),
            "Working directory does not exist: {}",
            cwd.display()
        );
        let mut app = App {
            window,
            sender,
            _ipc: ipc,
            context: WebContext::new(Some(data_dir()?.join("terminal-profile"))),
            workspaces: vec![Workspace::new(cwd)],
            active_workspace: 0,
            surfaces: HashMap::new(),
            controls: vec![],
            pending_reads: HashMap::new(),
            closing: false,
        };
        app.rebuild()?;
        ShowWindow(window, SW_SHOW);
        SetTimer(window, 1, 1000, None);
        let result = message_loop(&mut app, events);
        app.surfaces.clear(); // Parent HWND must outlive every WebView controller.
        drop(app);
        EVENTS.with(|slot| *slot.borrow_mut() = None);
        DestroyWindow(window);
        CoUninitialize();
        result
    }
}
fn message_loop(app: &mut App, events: Receiver<Event>) -> anyhow::Result<()> {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if result == 0 {
            return Ok(());
        }
        anyhow::ensure!(result != -1, "Windows message loop failed");
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        // WebView construction pumps Win32 messages. Mutable state is only used here,
        // never from window_proc or callbacks, so reentrant UI messages are safe.
        while let Ok(event) = events.try_recv() {
            if let Err(error) = app.event(event) {
                report(&format!("{error:#}"));
            }
            if app.closing {
                return Ok(());
            }
        }
    }
}
impl App {
    fn workspace(&self) -> &Workspace {
        &self.workspaces[self.active_workspace]
    }
    fn workspace_mut(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.active_workspace]
    }
    fn active(&self) -> SurfaceId {
        self.workspace().active()
    }
    fn rebuild(&mut self) -> anyhow::Result<()> {
        let mut missing = Vec::new();
        for workspace in &self.workspaces {
            for (_, _, tabs) in workspace.leaves() {
                for tab in tabs {
                    if !self.surfaces.contains_key(&tab.id) {
                        missing.push(tab.id);
                    }
                }
            }
        }
        for id in missing {
            self.add_view(id)?;
        }
        for control in self.controls.drain(..) {
            unsafe {
                DestroyWindow(control.hwnd);
            }
        }
        for (name, action) in [
            ("+ Workspace", Action::NewWorkspace),
            ("+ Tab", Action::NewTab),
            ("Split right", Action::Vertical),
            ("Split down", Action::Horizontal),
            ("Close tab", Action::CloseTab),
        ] {
            self.button(name, action)?;
        }
        for index in 0..self.workspaces.len() {
            let name = format!(
                "{} {}",
                if index == self.active_workspace {
                    "●"
                } else {
                    "○"
                },
                self.workspaces[index].name
            );
            self.button(&name, Action::Workspace(index))?;
        }
        for (pane, active, tabs) in self.workspace().leaves() {
            for tab in tabs {
                let title = if active == tab.id {
                    format!("● {}", tab.title)
                } else {
                    tab.title
                };
                self.button(&title, Action::Tab(pane, tab.id))?;
            }
        }
        self.layout()?;
        self.focus_active()
    }
    fn button(&mut self, name: &str, action: Action) -> anyhow::Result<()> {
        let id = self.controls.len() + 100;
        anyhow::ensure!(id < 65535, "too many controls");
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide(name).as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
                0,
                0,
                1,
                1,
                self.window,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        checked((!hwnd.is_null()) as i32)?;
        unsafe {
            SendMessageW(
                hwnd,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as WPARAM,
                1,
            );
        }
        self.controls.push(Control { hwnd, action });
        Ok(())
    }
    fn add_view(&mut self, surface: SurfaceId) -> anyhow::Result<()> {
        let identity = Identity::new(surface.0);
        let dispatch = self.sender.clone();
        let init = format!(
            "window.__flowmuxIdentity={};",
            serde_json::to_string(&identity)?
        );
        #[cfg(debug_assertions)]
        let init = if std::env::var_os("FLOWMUX_TEST_INPUT_TRACE").is_some() {
            format!("{init} (() => {{ const identity=window.__flowmuxIdentity;
                for (const type of ['keydown','keyup','compositionstart','compositionend']) {{
                    window.addEventListener(type, e => window.ipc.postMessage(JSON.stringify({{
                        ...identity, message: {{type:'diagnostic', event: {{type:e.type, key:e.key,
                            code:e.code, keyCode:e.keyCode, shift:e.shiftKey, composing:e.isComposing, data:e.data}} }}
                    }})), true);
                }} }})();")
        } else {
            init
        };
        let view = WebViewBuilder::new_with_web_context(&mut self.context)
            .with_background_color((23, 25, 31, 255))
            .with_devtools(false)
            .with_hotkeys_zoom(false)
            .with_visible(false)
            .with_focused(false)
            .with_clipboard(true)
            .with_initialization_script(init)
            .with_custom_protocol("flowmux-terminal".into(), |_id, request| {
                let (body, mime): (&'static [u8], &str) = match request.uri().path() {
                    "/" | "/index.html" => (
                        include_bytes!("../../assets/index.html"),
                        "text/html; charset=utf-8",
                    ),
                    "/terminal.js" => (
                        include_bytes!("../../assets/terminal.js"),
                        "application/javascript",
                    ),
                    "/terminal.css" => (include_bytes!("../../assets/terminal.css"), "text/css"),
                    "/xterm.css" => (include_bytes!("../../assets/xterm.css"), "text/css"),
                    _ => {
                        return wry::http::Response::builder()
                            .status(404)
                            .body(Cow::Borrowed(&[][..]))
                            .unwrap()
                    }
                };
                wry::http::Response::builder()
                    .header("Content-Type", mime)
                    .header("X-Content-Type-Options", "nosniff")
                    .body(Cow::Borrowed(body))
                    .unwrap()
            })
            .with_navigation_handler(|url| {
                url == "flowmux-terminal://localhost/" || url == format!("{TERMINAL_ORIGIN}/")
            })
            .with_new_window_req_handler(|_, _| wry::NewWindowResponse::Deny)
            .with_ipc_handler(move |request| {
                if request.body().len() <= crate::protocol::MAX_MESSAGE_BYTES {
                    dispatch.send(Event::Bridge(
                        surface,
                        request.uri().to_string(),
                        request.body().clone(),
                    ));
                }
            })
            .with_bounds(bounds(model::Rect {
                x: 220,
                y: 80,
                width: 800,
                height: 600,
            }))
            .with_url("flowmux-terminal://localhost/")
            .build_as_child(&Parent(self.window))
            .context("Cannot create the terminal WebView2 view")?;
        self.surfaces.insert(
            surface,
            Surface {
                view,
                identity,
                session: None,
                cols: 80,
                rows: 24,
                ready: false,
                visible: false,
            },
        );
        Ok(())
    }
    fn layout(&mut self) -> anyhow::Result<()> {
        let mut client: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            checked(GetClientRect(self.window, &mut client))?;
        }
        let scale = unsafe { GetDpiForWindow(self.window) }.max(96) as f64 / 96.0;
        let px = |value: i32| (value as f64 * scale).round() as i32;
        let sidebar = px(185).min((client.right / 3).max(0));
        let bar = px(34);
        let mut areas = Vec::new();
        model::layout(
            &self.workspace().root,
            model::Rect {
                x: sidebar + px(4),
                y: bar + px(4),
                width: (client.right - sidebar - px(8)).max(1),
                height: (client.bottom - bar - px(8)).max(1),
            },
            px(5),
            &mut areas,
        );
        let visible: HashMap<_, _> = areas
            .iter()
            .map(|(pane, area)| {
                (
                    self.workspace().root.active_surface_id(*pane).unwrap(),
                    *area,
                )
            })
            .collect();
        for (id, surface) in &mut self.surfaces {
            let show = visible.contains_key(id) && client.right > 0 && client.bottom > 0;
            // Hiding and re-showing an already visible view can cancel native IME composition.
            if surface.visible != show {
                surface.view.set_visible(show)?;
                surface.visible = show;
            }
        }
        for (pane, area) in &areas {
            let id = self.workspace().root.active_surface_id(*pane).unwrap();
            if let Some(surface) = self.surfaces.get(&id) {
                surface.view.set_bounds(bounds(model::Rect {
                    y: area.y + bar,
                    height: (area.height - bar).max(1),
                    ..*area
                }))?;
            }
        }
        let mut tab_positions: HashMap<PaneId, i32> = HashMap::new();
        for (index, control) in self.controls.iter().enumerate() {
            let (x, y, width, height) = match control.action {
                Action::Workspace(i) => (
                    px(5),
                    bar + px(8) + i as i32 * px(36),
                    (sidebar - px(10)).max(1),
                    px(32),
                ),
                Action::Tab(pane, _) => {
                    let area = areas.iter().find(|(id, _)| *id == pane).unwrap().1;
                    let offset = tab_positions.entry(pane).or_default();
                    let count = self.workspace().root.surface_count(pane).unwrap_or(1) as i32;
                    let width = (area.width / count).min(px(200)).max(1);
                    let result = (area.x + *offset, area.y, width, bar);
                    *offset += width;
                    result
                }
                _ => (px(5 + index as i32 * 130), px(2), px(124), bar - px(2)),
            };
            unsafe {
                SetWindowPos(
                    control.hwnd,
                    std::ptr::null_mut(),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        Ok(())
    }
    fn focus_active(&self) -> anyhow::Result<()> {
        if let Some(surface) = self.surfaces.get(&self.active()) {
            if surface.ready {
                surface.view.focus()?;
                surface.send(&HostMessage::Focus)?;
            }
        }
        Ok(())
    }
    fn event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Layout => self.layout()?,
            Event::Close => self.closing = true,
            Event::Tick => {
                self.pending_reads.retain(|_, request| {
                    if request.started.elapsed() > Duration::from_secs(12) {
                        let _ = request.reply.try_send(
                            json!({"error":"terminal parser did not answer the screen barrier"}),
                        );
                        false
                    } else {
                        true
                    }
                });
            }
            Event::Button(id) => {
                if let Some(control) = self.controls.get(id.saturating_sub(100) as usize) {
                    self.action(control.action.clone())?;
                }
            }
            Event::Bridge(id, origin, body) => self.bridge(id, &origin, &body)?,
            Event::Session(id, message) => {
                if let Some(surface) = self.surfaces.get(&id) {
                    match message {
                        SessionEvent::Output { sequence, bytes } => {
                            surface.send(&HostMessage::Output {
                                sequence,
                                data: base64::engine::general_purpose::STANDARD.encode(bytes),
                            })?
                        }
                        SessionEvent::Exit(code) => surface.send(&HostMessage::Exit { code })?,
                        SessionEvent::Error(message) => {
                            report(&format!("terminal {id}: {message}"))
                        }
                    }
                }
            }
            Event::Command(command, reply) => match self.command(command, reply.clone()) {
                Ok(Some(result)) => {
                    let _ = reply.try_send(result);
                }
                Ok(None) => {}
                Err(error) => {
                    let _ = reply.try_send(json!({"error":error.to_string()}));
                }
            },
        }
        Ok(())
    }
    fn bridge(&mut self, id: SurfaceId, origin: &str, body: &str) -> anyhow::Result<()> {
        let Some(surface) = self.surfaces.get(&id) else {
            return Ok(());
        };
        let message = surface.identity.decode(origin, body)?;
        match message {
            ClientMessage::Ready => {
                if surface.ready {
                    return Ok(());
                }
                let (workspace, pane, cwd) =
                    self.locate(id).context("terminal has no workspace")?;
                let workspace_id = self.workspaces[workspace].id;
                let sender = self.sender.clone();
                let session = Session::spawn(
                    &cwd,
                    pane,
                    id,
                    workspace_id,
                    &self._ipc.name,
                    surface.cols,
                    surface.rows,
                    move |event| sender.send(Event::Session(id, event)),
                )?;
                let surface = self.surfaces.get_mut(&id).unwrap();
                surface.session = Some(session);
                surface.ready = true;
                if self.active() == id {
                    self.focus_active()?;
                }
            }
            ClientMessage::Resize { cols, rows } => {
                let surface = self.surfaces.get_mut(&id).unwrap();
                surface.cols = cols;
                surface.rows = rows;
                if let Some(session) = &surface.session {
                    session.resize(cols, rows)?;
                }
            }
            ClientMessage::Input { data } => self.session(id)?.input(data.into_bytes())?,
            ClientMessage::BinaryInput { data } => {
                anyhow::ensure!(
                    data.chars().all(|c| c as u32 <= 255),
                    "invalid binary terminal input"
                );
                self.session(id)?
                    .input(data.chars().map(|c| c as u8).collect())?;
            }
            ClientMessage::Ack { sequence } => self.session(id)?.acknowledge(sequence)?,
            ClientMessage::Focus => {
                if let Some((workspace, pane, _)) = self.locate(id) {
                    // A delayed hidden-view focus event must not switch the workspace back.
                    if workspace == self.active_workspace
                        && self.workspaces[workspace].root.active_surface_id(pane) == Some(id)
                        && self
                            .surfaces
                            .get(&id)
                            .is_some_and(|surface| surface.visible)
                    {
                        self.workspace_mut().focused = pane;
                    }
                }
            }
            ClientMessage::Title { title } => {
                let title: String = title
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(160)
                    .collect();
                if let Some((workspace, pane, _)) = self.locate(id) {
                    self.workspaces[workspace]
                        .root
                        .set_surface_title_auto(pane, id, title.clone());
                    for control in &self.controls {
                        if matches!(control.action, Action::Tab(_, surface) if surface == id) {
                            unsafe {
                                SetWindowTextW(control.hwnd, wide(&title).as_ptr());
                            }
                        }
                    }
                }
            }
            ClientMessage::Screen {
                request,
                sequence,
                text,
            } => {
                if let Some(pending) = self.pending_reads.get(&request) {
                    anyhow::ensure!(
                        pending.surface == id && sequence >= pending.after,
                        "invalid screen response"
                    );
                    let pending = self.pending_reads.remove(&request).unwrap();
                    let _ = pending
                        .reply
                        .try_send(json!({"surface":id,"sequence":sequence,"text":text}));
                }
            }
            ClientMessage::Link { url } => {
                if url.starts_with("https://") || url.starts_with("http://") {
                    unsafe {
                        windows_sys::Win32::UI::Shell::ShellExecuteW(
                            self.window,
                            wide("open").as_ptr(),
                            wide(url).as_ptr(),
                            std::ptr::null(),
                            std::ptr::null(),
                            SW_SHOWNORMAL,
                        );
                    }
                }
            }
            ClientMessage::Fault { message } => anyhow::bail!("terminal frontend: {message}"),
            ClientMessage::Snapshot { .. } => {}
            ClientMessage::Diagnostic { event } => {
                #[cfg(debug_assertions)]
                if let Some(path) = std::env::var_os("FLOWMUX_TEST_INPUT_TRACE") {
                    use std::io::Write;
                    let mut path = path;
                    path.push(".keys.jsonl");
                    if let Ok(mut file) = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                    {
                        let _ =
                            writeln!(file, "{}", serde_json::json!({"surface":id,"event":event}));
                    }
                }
                #[cfg(not(debug_assertions))]
                let _ = event;
            }
        }
        Ok(())
    }
    fn locate(&self, surface: SurfaceId) -> Option<(usize, PaneId, PathBuf)> {
        for (index, workspace) in self.workspaces.iter().enumerate() {
            for (pane, _, tabs) in workspace.leaves() {
                if tabs.iter().any(|tab| tab.id == surface) {
                    return Some((
                        index,
                        pane,
                        workspace
                            .root
                            .terminal_surface_cwd(pane)
                            .unwrap_or_else(|| workspace.cwd.clone()),
                    ));
                }
            }
        }
        None
    }
    fn session(&self, id: SurfaceId) -> anyhow::Result<&Session> {
        self.surfaces
            .get(&id)
            .and_then(|surface| surface.session.as_ref())
            .context("terminal is not ready")
    }
    fn target(&self, pane: Option<Uuid>) -> anyhow::Result<SurfaceId> {
        if let Some(pane) = pane {
            self.workspaces
                .iter()
                .find_map(|ws| ws.root.active_surface_id(PaneId(pane)))
                .context("pane not found")
        } else {
            Ok(self.active())
        }
    }
    fn action(&mut self, action: Action) -> anyhow::Result<()> {
        match action {
            Action::NewWorkspace => {
                self.workspaces
                    .push(Workspace::new(self.workspace().cwd.clone()));
                self.active_workspace = self.workspaces.len() - 1;
            }
            Action::Workspace(index) => self.active_workspace = index,
            Action::NewTab => {
                self.workspace_mut().new_tab();
            }
            Action::Vertical => {
                self.workspace_mut().split(SplitDirection::Vertical);
            }
            Action::Horizontal => {
                self.workspace_mut().split(SplitDirection::Horizontal);
            }
            Action::CloseTab => {
                let surface = self
                    .workspace_mut()
                    .close_active()
                    .context("cannot close the final tab")?;
                self.surfaces.remove(&surface);
                self.pending_reads.retain(|_, request| {
                    if request.surface == surface {
                        let _ = request
                            .reply
                            .try_send(json!({"error":"terminal closed during screen read"}));
                        false
                    } else {
                        true
                    }
                });
            }
            Action::Tab(pane, surface) => {
                self.workspace_mut().focused = pane;
                self.workspace_mut().root.set_active_surface(pane, surface);
            }
        }
        self.rebuild()
    }
    fn command(&mut self, command: Command, reply: ipc::Reply) -> anyhow::Result<Option<Value>> {
        match command {
            Command::Doctor => anyhow::bail!("doctor is a local CLI operation"),
            Command::Identify => {
                return Ok(Some(json!({"pid":std::process::id(),"pipe":self._ipc.name,
                "workspace":self.workspace().id,"pane":self.workspace().focused,"surface":self.active(),"platform":"windows"})))
            }
            Command::Capabilities => {
                return Ok(Some(json!({"platform":"windows","status":"development",
                "terminal_backend":"ConPTY/xterm.js","browser_backend":"WebView2",
                "commands":["identify","capabilities","tree","read-screen","send-keys","send-key","split","new-tab",
                    "new-workspace","focus-pane","focus-tab","close-tab","quit"],
                "acceptance":"All release gates remain pending; see windows/acceptance.json"})))
            }
            Command::Tree => {
                let surfaces: Vec<_> = self.surfaces.iter().map(|(id, surface)| json!({"id":id,"ready":surface.ready,
                    "pid":surface.session.as_ref().map(|s| s.pid),"cols":surface.cols,"rows":surface.rows})).collect();
                return Ok(Some(
                    json!({"workspaces":self.workspaces,"active_workspace":self.workspace().id,"surfaces":surfaces}),
                ));
            }
            Command::ReadScreen { pane } => {
                let id = self.target(pane)?;
                let after = self.session(id)?.barrier();
                let request = Uuid::new_v4();
                self.surfaces[&id].send(&HostMessage::ReadScreen { request, after })?;
                self.pending_reads.insert(
                    request,
                    PendingRead {
                        surface: id,
                        after,
                        reply,
                        started: Instant::now(),
                    },
                );
                return Ok(None);
            }
            Command::SendKeys { pane, text } => self
                .session(self.target(Some(pane))?)?
                .input(text.into_bytes())?,
            Command::SendKey { key, pane } => {
                self.session(self.target(pane)?)?.input(key_bytes(&key)?)?
            }
            Command::Split { direction } => self.action(match direction {
                Direction::Vertical => Action::Vertical,
                Direction::Horizontal => Action::Horizontal,
            })?,
            Command::NewTab => self.action(Action::NewTab)?,
            Command::NewWorkspace { cwd } => {
                let cwd = cwd.unwrap_or_else(|| self.workspace().cwd.clone());
                anyhow::ensure!(cwd.is_dir(), "working directory does not exist");
                self.workspaces.push(Workspace::new(cwd));
                self.active_workspace = self.workspaces.len() - 1;
                self.rebuild()?;
            }
            Command::FocusPane { pane } => {
                let id = self.target(Some(pane))?;
                let (workspace, pane, _) = self.locate(id).unwrap();
                self.active_workspace = workspace;
                self.workspace_mut().focused = pane;
                self.rebuild()?;
            }
            Command::FocusTab { surface } | Command::CloseTab { surface } => {
                let id = SurfaceId(surface);
                let (workspace, pane, _) = self.locate(id).context("surface not found")?;
                self.active_workspace = workspace;
                self.workspace_mut().focused = pane;
                self.workspace_mut().root.set_active_surface(pane, id);
                if matches!(command, Command::CloseTab { .. }) {
                    self.action(Action::CloseTab)?;
                } else {
                    self.rebuild()?;
                }
            }
            // The pipe worker requests close only after the client receives this reply.
            Command::Quit => {}
        }
        Ok(Some(json!({"ok":true})))
    }
}
fn bounds(rect: model::Rect) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::PhysicalPosition::new(rect.x, rect.y).into(),
        size: wry::dpi::PhysicalSize::new(rect.width.max(1) as u32, rect.height.max(1) as u32)
            .into(),
    }
}
pub fn report(message: &str) {
    use std::io::Write;
    if let Ok(directory) = data_dir() {
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("host.log"))
        {
            let _ = writeln!(file, "{} {message}", std::process::id());
        }
    }
}
