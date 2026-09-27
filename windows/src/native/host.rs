// SPDX-License-Identifier: GPL-3.0-or-later
use super::{
    checked, data_dir, ipc,
    session::{Session, SessionEvent},
    state_store::{self, Store},
    wide,
};
use crate::{
    command::{key_bytes, Command, Direction, FocusDirection, Launch, Request, WorkspaceOp},
    model::{self, Workspace},
    protocol::{ClientMessage, HostMessage, Identity, TERMINAL_ORIGIN},
    state::{SavedScreen, WindowState},
};
use anyhow::Context;
use base64::Engine;
use flowmux_core::{PaneId, SplitDirection, SurfaceId, WorkspaceId};
use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::HashMap,
    num::NonZeroIsize,
    path::PathBuf,
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{Com::*, LibraryLoader::*},
    UI::{
        HiDpi::*,
        Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture, SetFocus},
        WindowsAndMessaging::*,
    },
};
use wry::{WebContext, WebView, WebViewBuilder, WebViewExtWindows};
#[path = "panes.rs"]
mod panes;
#[path = "search.rs"]
mod search;
#[path = "workspaces.rs"]
mod workspaces;

const WAKE: u32 = WM_APP + 1;
thread_local! {
    static EVENTS: RefCell<Option<EventSender>> = const { RefCell::new(None) };
    static CONTROL_ACTIONS: RefCell<HashMap<isize, Action>> = RefCell::new(HashMap::new());
}
enum Event {
    Layout,
    Tick,
    Close,
    ExitAfterReply,
    Saved(Result<(), String>),
    Button(Action),
    Bridge(SurfaceId, String, String),
    Session(SurfaceId, SessionEvent),
    Command(Request, ipc::Reply),
    SearchUi(search::UiAction),
    Pointer(panes::Pointer),
    ContextMenu(Action, i32, i32),
    Metadata(workspaces::EditAction),
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
        WM_CONTEXTMENU => {
            let action =
                CONTROL_ACTIONS.with(|actions| actions.borrow().get(&(wparam as isize)).cloned());
            if let Some(action) = action {
                let mut x = lparam as u16 as i16 as i32;
                let mut y = (lparam >> 16) as u16 as i16 as i32;
                if x == -1 && y == -1 {
                    let mut rect = RECT::default();
                    GetWindowRect(wparam as HWND, &mut rect);
                    x = rect.left;
                    y = rect.bottom;
                }
                post(Event::ContextMenu(action, x, y));
            }
            0
        }
        WM_CTLCOLORSTATIC => {
            if let Some(color) = workspaces::swatch_color(lparam as HWND) {
                SetTextColor(wparam as HDC, color);
                SetBkMode(wparam as HDC, TRANSPARENT as i32);
                GetSysColorBrush(COLOR_WINDOW) as LRESULT
            } else {
                DefWindowProcW(window, message, wparam, lparam)
            }
        }
        WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP => {
            let x = (lparam as u16 as i16) as i32;
            let y = ((lparam >> 16) as u16 as i16) as i32;
            post(Event::Pointer(match message {
                WM_LBUTTONDOWN => panes::Pointer::Down(x, y),
                WM_LBUTTONUP => panes::Pointer::Up(x, y),
                _ => panes::Pointer::Move(x, y),
            }));
            0
        }
        WM_CANCELMODE | WM_CAPTURECHANGED => {
            post(Event::Pointer(panes::Pointer::Cancel));
            DefWindowProcW(window, message, wparam, lparam)
        }
        WM_SETCURSOR if lparam as u16 == HTCLIENT as u16 => {
            if panes::set_cursor(window) {
                1
            } else {
                DefWindowProcW(window, message, wparam, lparam)
            }
        }
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
                let action = CONTROL_ACTIONS.with(|actions| actions.borrow().get(&lparam).cloned());
                if let Some(action) = action {
                    post(Event::Button(action));
                }
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
    restoring: bool,
    cwd_reported: bool,
    visible: bool,
    process_pid: Option<u32>,
    exit_code: Option<u32>,
    output_ended: bool,
    output_sequence: u64,
    acknowledged_sequence: u64,
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
    Workspace(WorkspaceId),
    WorkspaceMenu,
    WorkspaceColor(WorkspaceId),
    NewTab,
    Vertical,
    Horizontal,
    CloseTab,
    MoveTabMenu,
    Find,
    SearchAll,
    TogglePaneZoom,
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
enum CloseRequest {
    Native,
    Ipc(ipc::Reply),
}
struct PendingSave {
    id: Uuid,
    state: WindowState,
    waiting: HashMap<SurfaceId, u64>,
    started: Instant,
    writing: bool,
    closing: bool,
    reply: Option<ipc::Reply>,
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
    pane_layout: model::Layout,
    zoomed: Option<PaneId>,
    drag: Option<panes::Drag>,
    metadata: Option<workspaces::Panel>,
    pending_reads: HashMap<Uuid, PendingRead>,
    pending_finds: HashMap<Uuid, PendingRead>,
    search: search::Controller,
    closing: bool,
    background_test: bool,
    store: Option<Arc<Store>>,
    restore_screens: HashMap<SurfaceId, SavedScreen>,
    pending_save: Option<PendingSave>,
    close_request: Option<CloseRequest>,
    last_save_attempt: Instant,
    state_error: Option<String>,
}

pub fn run(launch: Launch) -> anyhow::Result<()> {
    let background_test =
        cfg!(debug_assertions) && std::env::var("FLOWMUX_TEST_BACKGROUND").as_deref() == Ok("1");
    let (store, restored) = if launch.temporary {
        (None, None)
    } else {
        state_store::open(
            launch.new_window || launch.cwd.is_some(),
            launch.restore_window,
            background_test,
        )?
    };
    let cwd = launch
        .cwd
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .context("No initial working directory")?;
    anyhow::ensure!(
        cwd.is_dir(),
        "Working directory does not exist: {}",
        cwd.display()
    );
    let (mut workspaces, active_workspace, restore_screens) = match restored {
        Some(state) => {
            let active = state
                .workspaces
                .iter()
                .position(|w| w.id == state.active_workspace)
                .unwrap();
            (state.workspaces, active, state.screens)
        }
        None => (vec![Workspace::new(cwd.clone())], 0, HashMap::new()),
    };
    // Deleted/unmounted directories cannot prevent recovery of the other tabs.
    for workspace in &mut workspaces {
        if !workspace.cwd.is_dir() {
            workspace.cwd = cwd.clone();
        }
        for (pane, _, tabs) in workspace.leaves() {
            for tab in tabs {
                if let flowmux_core::SurfaceKind::Terminal {
                    cwd: Some(path), ..
                } = &tab.kind
                {
                    if !path.is_dir() {
                        report(&format!(
                            "Restored cwd {} is unavailable; using {}",
                            path.display(),
                            workspace.cwd.display()
                        ));
                        workspace
                            .root
                            .set_surface_cwd(pane, tab.id, workspace.cwd.clone());
                    }
                }
            }
        }
    }
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
            move || shutdown.send(Event::ExitAfterReply),
        )?;
        let mut app = App {
            window,
            sender,
            _ipc: ipc,
            context: WebContext::new(Some(data_dir()?.join("terminal-profile"))),
            workspaces,
            active_workspace,
            surfaces: HashMap::new(),
            controls: vec![],
            pane_layout: model::Layout::default(),
            zoomed: None,
            drag: None,
            metadata: None,
            pending_reads: HashMap::new(),
            pending_finds: HashMap::new(),
            search: search::Controller::default(),
            closing: false,
            // Automated IPC verification can run without exposing a window or
            // taking desktop focus. Production builds ignore this test switch.
            background_test,
            store,
            restore_screens,
            pending_save: None,
            close_request: None,
            last_save_attempt: Instant::now(),
            state_error: None,
        };
        app.rebuild()?;
        if !app.background_test {
            ShowWindow(window, SW_SHOW);
        }
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
            if !app.search.handle_message(&message)
                && !app
                    .metadata
                    .as_ref()
                    .is_some_and(|p| p.handle_message(&message))
            {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
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
        self.rebuild_without_focus()?;
        self.focus_active()
    }
    fn rebuild_without_focus(&mut self) -> anyhow::Result<()> {
        self.cancel_drag();
        workspaces::clear_swatches();
        CONTROL_ACTIONS.with(|actions| actions.borrow_mut().clear());
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
            ("Move tab…", Action::MoveTabMenu),
            ("Find", Action::Find),
            ("Search all", Action::SearchAll),
            ("Maximize pane", Action::TogglePaneZoom),
        ] {
            self.button(name, action)?;
        }
        self.button("Workspace…", Action::WorkspaceMenu)?;
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
            let id = self.workspaces[index].id;
            self.button(&name, Action::Workspace(id))?;
            if let Some(color) = self.workspaces[index].color.clone() {
                self.swatch(id, &color)?;
            }
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
        self.layout()
    }
    fn button(&mut self, name: &str, action: Action) -> anyhow::Result<()> {
        let id = self.controls.len() + 100;
        anyhow::ensure!(id < 65535, "too many controls");
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide(name.replace('&', "&&")).as_ptr(),
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
        CONTROL_ACTIONS.with(|actions| actions.borrow_mut().insert(hwnd as isize, action.clone()));
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
                for (const type of ['keydown','keyup','compositionstart','compositionupdate','compositionend','focus','blur','input']) {{
                    window.addEventListener(type, e => window.ipc.postMessage(JSON.stringify({{
                        ...identity, message: {{type:'diagnostic', event: {{type:e.type, key:e.key,
                            code:e.code, keyCode:e.keyCode, shift:e.shiftKey, composing:e.isComposing, data:e.data,
                            value:e.target?.value, active:document.activeElement === e.target}} }}
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
                restoring: false,
                cwd_reported: false,
                visible: false,
                process_pid: None,
                exit_code: None,
                output_ended: false,
                output_sequence: 0,
                acknowledged_sequence: 0,
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
        let (mut geometry, content) = self.geometry(self.active_workspace)?;
        if let Some(pane) = self.zoomed {
            geometry.panes = vec![(pane, content)];
            geometry.dividers.clear();
        }
        panes::cursor_dividers(if self.background_test {
            &[]
        } else {
            &geometry.dividers
        });
        let areas = &geometry.panes;
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
        for (pane, area) in areas {
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
                Action::WorkspaceMenu => (px(5), bar + px(8), (sidebar - px(10)).max(1), px(32)),
                Action::Workspace(id) | Action::WorkspaceColor(id) => {
                    let i = self.workspaces.iter().position(|w| w.id == id).unwrap();
                    let swatch = matches!(control.action, Action::WorkspaceColor(_));
                    (
                        if swatch { px(5) } else { px(23) },
                        bar + px(48) + i as i32 * px(36),
                        if swatch {
                            px(16)
                        } else {
                            (sidebar - px(28)).max(1)
                        },
                        px(32),
                    )
                }
                Action::Tab(pane, _) => {
                    let Some((_, area)) = areas.iter().find(|(id, _)| *id == pane) else {
                        unsafe {
                            ShowWindow(control.hwnd, SW_HIDE);
                        }
                        continue;
                    };
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
                if matches!(control.action, Action::TogglePaneZoom) {
                    SetWindowTextW(
                        control.hwnd,
                        wide(if self.zoomed.is_some() {
                            "Restore pane"
                        } else {
                            "Maximize pane"
                        })
                        .as_ptr(),
                    );
                }
                SetWindowPos(
                    control.hwnd,
                    std::ptr::null_mut(),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
        }
        self.pane_layout = geometry;
        Ok(())
    }
    fn focus_active(&self) -> anyhow::Result<()> {
        if self.background_test {
            return Ok(());
        }
        if let Some(surface) = self.surfaces.get(&self.active()) {
            if surface.ready {
                let mut info = GUITHREADINFO {
                    cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                    ..GUITHREADINFO::default()
                };
                let child = surface.view.hwnd().0;
                let already_focused = unsafe {
                    let thread = GetWindowThreadProcessId(self.window, std::ptr::null_mut());
                    GetGUIThreadInfo(thread, &mut info) != 0
                        && (info.hwndFocus == child || IsChild(child, info.hwndFocus) != 0)
                };
                // Repeating WebView2 MoveFocus can blur the already focused textarea.
                // xterm clears its value on blur, which discards live IME composition.
                if !already_focused {
                    surface.view.focus()?;
                }
                surface.send(&HostMessage::Focus)?;
            }
        }
        Ok(())
    }
    fn event(&mut self, event: Event) -> anyhow::Result<()> {
        match event {
            Event::Layout => {
                self.cancel_drag();
                self.layout()?;
            }
            Event::Pointer(pointer) => self.pointer(pointer)?,
            Event::Close => self.request_close(CloseRequest::Native)?,
            Event::ExitAfterReply => self.closing = true,
            Event::Saved(result) => self.finish_save(result),
            Event::SearchUi(action) => self.search_ui(action)?,
            Event::Metadata(action) => self.metadata_action(action)?,
            Event::ContextMenu(action, x, y) => self.context_menu(action, x, y)?,
            Event::Tick => {
                self.search_tick()?;
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
                self.pending_finds.retain(|_, request| {
                    if request.started.elapsed() > Duration::from_secs(12) {
                        let _ = request
                            .reply
                            .try_send(json!({"error":"terminal did not answer the find request"}));
                        false
                    } else {
                        true
                    }
                });
                if self
                    .pending_save
                    .as_ref()
                    .is_some_and(|s| !s.writing && s.started.elapsed() > Duration::from_secs(12))
                {
                    self.finish_save(Err("terminal parser did not answer the history barrier; previous save preserved".into()));
                }
                if self.store.is_some()
                    && self.pending_save.is_none()
                    && self.close_request.is_none()
                    && self.last_save_attempt.elapsed() > Duration::from_secs(30)
                {
                    self.last_save_attempt = Instant::now();
                    if let Err(error) = self.begin_save(None) {
                        self.state_error = Some(error.to_string());
                    }
                }
            }
            Event::Button(action) => self.action(action)?,
            Event::Bridge(id, origin, body) => self.bridge(id, &origin, &body)?,
            Event::Session(id, message) => {
                if let Some(surface) = self.surfaces.get_mut(&id) {
                    match message {
                        SessionEvent::Output { sequence, bytes } => {
                            anyhow::ensure!(
                                sequence == surface.output_sequence + 1,
                                "out-of-order PTY output"
                            );
                            surface.send(&HostMessage::Output {
                                sequence,
                                data: base64::engine::general_purpose::STANDARD.encode(bytes),
                            })?;
                            surface.output_sequence = sequence;
                        }
                        SessionEvent::OutputEnd => surface.output_ended = true,
                        SessionEvent::Exit(code) => {
                            surface.exit_code = Some(code);
                            surface.send(&HostMessage::Exit { code })?;
                        }
                        SessionEvent::Error(message) => {
                            report(&format!("terminal {id}: {message}"))
                        }
                    }
                    if surface.output_ended && surface.exit_code.is_some() {
                        // All final bytes have entered the WebView's ordered stream.
                        // Retain its grid and accept parser ACKs after releasing native resources.
                        surface.session.take();
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
                if surface.ready || surface.restoring {
                    return Ok(());
                }
                if let Some(screen) = self.restore_screens.remove(&id) {
                    surface.send(&HostMessage::Restore { screen })?;
                    self.surfaces.get_mut(&id).unwrap().restoring = true;
                } else {
                    self.start_session(id)?;
                }
            }
            ClientMessage::Restored => {
                anyhow::ensure!(
                    surface.restoring && !surface.ready,
                    "unexpected history restore acknowledgement"
                );
                self.start_session(id)?;
            }
            ClientMessage::Resize { cols, rows } => {
                let surface = self.surfaces.get_mut(&id).unwrap();
                surface.cols = cols;
                surface.rows = rows;
                if surface.exit_code.is_none() {
                    if let Some(session) = &surface.session {
                        session.resize(cols, rows)?;
                    }
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
            ClientMessage::Ack { sequence } => {
                let surface = self.surfaces.get_mut(&id).unwrap();
                anyhow::ensure!(
                    sequence >= surface.acknowledged_sequence
                        && sequence <= surface.output_sequence,
                    "invalid parser acknowledgement"
                );
                if let Some(session) = &surface.session {
                    session.acknowledge(sequence)?;
                }
                surface.acknowledged_sequence = sequence;
            }
            ClientMessage::Focus => {
                if let Some((workspace, pane, _)) = self.locate(id) {
                    // A delayed hidden-view focus event must not switch the workspace back.
                    if self.close_request.is_none()
                        && workspace == self.active_workspace
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
            ClientMessage::FocusDirection { direction } => {
                if self.active() == id && self.surfaces[&id].visible && self.close_request.is_none()
                {
                    self.focus_direction(id, direction)?;
                }
            }
            ClientMessage::TogglePaneZoom => {
                if self.active() == id && self.surfaces[&id].visible && self.close_request.is_none()
                {
                    self.toggle_zoom(id)?;
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
                        .set_surface_title_auto(pane, id, title);
                    self.refresh_tab_title(id);
                }
            }
            ClientMessage::Cwd { path } => {
                if surface.ready && !surface.restoring {
                    if let Ok(path) = crate::cwd::local_path(&path) {
                        self.update_cwd(id, path);
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
            ClientMessage::Found {
                request,
                sequence,
                result,
            } => {
                if let Some(pending) = self.pending_finds.get(&request) {
                    anyhow::ensure!(
                        pending.surface == id && sequence >= pending.after,
                        "invalid find response"
                    );
                    let pending = self.pending_finds.remove(&request).unwrap();
                    let _ = pending
                        .reply
                        .try_send(json!({"surface":id,"sequence":sequence,"result":result}));
                }
            }
            ClientMessage::SearchResults {
                search,
                sequence,
                total,
                hits,
                error,
            } => self.searched(id, search, sequence, total, hits, error)?,
            ClientMessage::SearchOpened {
                request,
                search,
                sequence,
                error,
                selection,
                line,
                column,
                selected,
            } => {
                self.search_opened(
                    id,
                    request,
                    search,
                    sequence,
                    error.map_or(Ok(selection), Err),
                    (selected, line, column),
                )?;
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
            ClientMessage::Snapshot {
                request,
                sequence,
                screen,
            } => {
                let metadata = self
                    .locate(id)
                    .and_then(|(ws, pane, _)| self.workspaces[ws].root.find_surface(pane, id));
                if let Some(pending) = &mut self.pending_save {
                    if pending.id == request && !pending.writing {
                        if let Some(after) = pending.waiting.get(&id) {
                            anyhow::ensure!(
                                sequence >= *after && sequence <= surface.output_sequence,
                                "invalid history response"
                            );
                            screen.validate()?;
                            // Cwd/title can arrive while this snapshot waits for output
                            // parsing. Capture them at the same per-surface barrier.
                            if let Some(tab) = metadata {
                                for workspace in &mut pending.state.workspaces {
                                    for (pane, _, tabs) in workspace.leaves() {
                                        if tabs.iter().any(|t| t.id == id) {
                                            if let flowmux_core::SurfaceKind::Terminal {
                                                cwd: Some(cwd),
                                                ..
                                            } = &tab.kind
                                            {
                                                workspace.root.set_surface_cwd(
                                                    pane,
                                                    id,
                                                    cwd.clone(),
                                                );
                                            }
                                            workspace.root.set_surface_title_auto(
                                                pane,
                                                id,
                                                tab.title.clone(),
                                            );
                                        }
                                    }
                                }
                            }
                            pending.state.screens.insert(id, screen);
                            pending.waiting.remove(&id);
                            self.write_save()?;
                        }
                    }
                }
            }
            ClientMessage::SnapshotError { request, message } => {
                if self
                    .pending_save
                    .as_ref()
                    .is_some_and(|s| s.id == request && s.waiting.contains_key(&id) && !s.writing)
                {
                    self.finish_save(Err(format!("terminal history: {message}")));
                }
            }
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
    fn start_session(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        let (workspace, pane, cwd) = self.locate(id).context("terminal has no workspace")?;
        let sender = self.sender.clone();
        let surface = &self.surfaces[&id];
        let session = Session::spawn(
            &cwd,
            pane,
            id,
            self.workspaces[workspace].id,
            &self._ipc.name,
            surface.cols,
            surface.rows,
            move |event| sender.send(Event::Session(id, event)),
        )?;
        let surface = self.surfaces.get_mut(&id).unwrap();
        surface.process_pid = Some(session.pid);
        surface.session = Some(session);
        surface.ready = true;
        surface.restoring = false;
        if self.active() == id {
            self.focus_active()?;
        }
        Ok(())
    }
    fn begin_save(&mut self, reply: Option<ipc::Reply>) -> anyhow::Result<()> {
        let store = self
            .store
            .as_ref()
            .context("state persistence is disabled for this test host")?;
        anyhow::ensure!(
            self.pending_save.is_none(),
            "a state save is already in progress"
        );
        anyhow::ensure!(
            self.surfaces.values().all(|s| s.ready && !s.restoring),
            "wait for all terminals to finish loading before saving"
        );
        let request = Uuid::new_v4();
        let mut waiting = HashMap::new();
        for (id, surface) in &self.surfaces {
            let after = surface
                .session
                .as_ref()
                .map_or(surface.output_sequence, Session::barrier);
            surface.send(&HostMessage::Snapshot { request, after })?;
            waiting.insert(*id, after);
        }
        self.pending_save = Some(PendingSave {
            id: request,
            state: WindowState {
                version: 1,
                window: store.id,
                workspaces: self.workspaces.clone(),
                active_workspace: self.workspace().id,
                screens: HashMap::new(),
            },
            waiting,
            started: Instant::now(),
            writing: false,
            closing: self.close_request.is_some(),
            reply,
        });
        self.last_save_attempt = Instant::now();
        Ok(())
    }
    fn write_save(&mut self) -> anyhow::Result<()> {
        let pending = self.pending_save.as_mut().unwrap();
        if !pending.waiting.is_empty() || pending.writing {
            return Ok(());
        }
        let store = self.store.as_ref().unwrap().clone();
        let state = pending.state.clone();
        let sender = self.sender.clone();
        std::thread::Builder::new()
            .name("flowmux-state-save".into())
            .spawn(move || {
                sender.send(Event::Saved(
                    store.write(&state).map_err(|e| format!("{e:#}")),
                ));
            })?;
        pending.writing = true;
        Ok(())
    }
    fn request_close(&mut self, request: CloseRequest) -> anyhow::Result<()> {
        anyhow::ensure!(self.close_request.is_none(), "window is already closing");
        if self.store.is_none() {
            match request {
                CloseRequest::Native => self.closing = true,
                CloseRequest::Ipc(reply) => {
                    if reply.try_send(json!({"ok":true})).is_err() {
                        self.closing = true;
                    }
                }
            }
            return Ok(());
        }
        self.close_request = Some(request);
        if self.pending_save.is_none() {
            if let Err(error) = self.begin_save(None) {
                self.state_error = Some(error.to_string());
                self.close_failed(&error.to_string());
                return Err(error);
            }
        }
        Ok(())
    }
    fn finish_save(&mut self, result: Result<(), String>) {
        let Some(pending) = self.pending_save.take() else {
            return;
        };
        self.state_error = result.as_ref().err().cloned();
        let response = match &result {
            Ok(()) => {
                json!({"ok":true,"window":pending.state.window,"path":self.store.as_ref().map(|s| &s.path),
                "surfaces":pending.state.screens.len(),"truncated":pending.state.screens.values().filter(|s| s.truncated).count()})
            }
            Err(error) => {
                report(error);
                json!({"error":error})
            }
        };
        if let Some(reply) = pending.reply {
            let _ = reply.try_send(response.clone());
        }
        if pending.closing {
            match result {
                Ok(()) => match &self.close_request {
                    Some(CloseRequest::Native) => self.closing = true,
                    Some(CloseRequest::Ipc(reply)) => {
                        // A slow save can outlive the IPC caller's deadline. Its
                        // successful close request must not leave the UI frozen.
                        self.closing |= reply.try_send(response).is_err();
                    }
                    None => {}
                },
                Err(error) => self.close_failed(&error),
            }
        } else if self.close_request.is_some() {
            // A checkpoint captured an older layout. Closing always captures again
            // after structural changes have stopped, even if that checkpoint failed.
            if let Err(error) = self.begin_save(None) {
                self.close_failed(&error.to_string());
            }
        }
    }
    fn close_failed(&mut self, error: &str) {
        match self.close_request.take() {
            Some(CloseRequest::Ipc(reply)) => {
                let _ = reply.try_send(json!({"error":error}));
            }
            Some(CloseRequest::Native) if !self.background_test => unsafe {
                if MessageBoxW(
                    self.window,
                    wide(format!(
                        "The window state could not be saved. Close without saving changes? The last completed checkpoint will be kept.\n\n{error}"
                    ))
                    .as_ptr(),
                    wide("flowmux").as_ptr(),
                    MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING,
                ) == IDYES { self.closing = true; }
            },
            _ => {}
        }
    }
    fn update_cwd(&mut self, id: SurfaceId, cwd: PathBuf) {
        if let Some((workspace, pane, _)) = self.locate(id) {
            let first_report = self.surfaces.get(&id).is_some_and(|s| !s.cwd_reported);
            let root = &mut self.workspaces[workspace].root;
            root.set_surface_cwd(pane, id, cwd.clone());
            if first_report
                && matches!(
                    root.surface_title(pane, id),
                    Some("PowerShell" | "Windows PowerShell")
                )
            {
                root.set_surface_title_auto(
                    pane,
                    id,
                    flowmux_core::terminal_tab_title_for_cwd(Some(&cwd)),
                );
            }
            if let Some(surface) = self.surfaces.get_mut(&id) {
                surface.cwd_reported = true;
            }
            self.refresh_tab_title(id);
        }
    }
    fn refresh_tab_title(&self, id: SurfaceId) {
        if let Some((workspace, pane, _)) = self.locate(id) {
            let root = &self.workspaces[workspace].root;
            if let Some(title) = root.surface_title(pane, id) {
                let label = if root.active_surface_id(pane) == Some(id) {
                    format!("● {title}")
                } else {
                    title.to_owned()
                };
                for control in &self.controls {
                    if matches!(control.action, Action::Tab(_, surface) if surface == id) {
                        unsafe {
                            SetWindowTextW(control.hwnd, wide(label.replace('&', "&&")).as_ptr());
                        }
                    }
                }
            }
        }
    }
    fn locate(&self, surface: SurfaceId) -> Option<(usize, PaneId, PathBuf)> {
        for (index, workspace) in self.workspaces.iter().enumerate() {
            for (pane, _, tabs) in workspace.leaves() {
                if let Some(tab) = tabs.iter().find(|tab| tab.id == surface) {
                    return Some((
                        index,
                        pane,
                        match &tab.kind {
                            flowmux_core::SurfaceKind::Terminal { cwd, .. } => cwd.clone(),
                            _ => None,
                        }
                        .unwrap_or_else(|| workspace.cwd.clone()),
                    ));
                }
            }
        }
        None
    }
    fn session(&self, id: SurfaceId) -> anyhow::Result<&Session> {
        let surface = self
            .surfaces
            .get(&id)
            .context("terminal surface not found")?;
        anyhow::ensure!(surface.exit_code.is_none(), "terminal process has exited");
        surface.session.as_ref().context("terminal is not ready")
    }
    fn target(&self, pane: Option<Uuid>, caller: Option<SurfaceId>) -> anyhow::Result<SurfaceId> {
        if let Some(pane) = pane {
            self.workspaces
                .iter()
                .find_map(|ws| ws.root.active_surface_id(PaneId(pane)))
                .context("pane not found")
        } else {
            let id = caller.unwrap_or_else(|| self.active());
            anyhow::ensure!(
                self.locate(id).is_some(),
                "calling surface no longer exists"
            );
            Ok(id)
        }
    }
    fn select(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        let (workspace, pane, _) = self.locate(id).context("surface not found")?;
        if workspace != self.active_workspace || self.zoomed.is_some_and(|zoomed| zoomed != pane) {
            self.zoomed = None;
        }
        self.active_workspace = workspace;
        self.workspace_mut().focused = pane;
        self.workspace_mut().root.set_active_surface(pane, id);
        Ok(())
    }
    fn move_tab(&mut self, surface: SurfaceId, target: PaneId, index: usize) -> anyhow::Result<()> {
        self.active_workspace = model::move_surface(&mut self.workspaces, surface, target, index)?;
        self.zoomed = None;
        // No surface is removed or recreated: its WebView, parser, IME target and
        // native process stay attached to the same identity throughout the move.
        self.rebuild()
    }
    fn move_menu(&mut self) -> anyhow::Result<()> {
        let surface = self.active();
        let pane = self.workspace().focused;
        let tabs = self
            .workspace()
            .leaves()
            .into_iter()
            .find(|(id, _, _)| *id == pane)
            .unwrap()
            .2;
        let at = tabs.iter().position(|tab| tab.id == surface).unwrap();
        let mut destinations = Vec::new();
        if at > 0 {
            destinations.push(("Move left".to_owned(), pane, at - 1));
        }
        if at + 1 < tabs.len() {
            destinations.push(("Move right".to_owned(), pane, at + 1));
        }
        for workspace in &self.workspaces {
            for (i, (target, _, _)) in workspace.leaves().iter().enumerate() {
                if *target != pane {
                    destinations.push((
                        format!("{} — pane {}", workspace.name.replace('&', "&&"), i + 1),
                        *target,
                        usize::MAX,
                    ));
                }
            }
        }
        unsafe {
            let menu = CreatePopupMenu();
            anyhow::ensure!(!menu.is_null(), "cannot create tab move menu");
            // WebView2 has its own input thread. Move native focus to the host
            // while its popup owns keyboard navigation, then restore the terminal.
            SetFocus(self.window);
            for (index, (label, _, _)) in destinations.iter().enumerate() {
                AppendMenuW(
                    menu,
                    MF_STRING,
                    index + 1,
                    wide(label.replace('&', "&&")).as_ptr(),
                );
            }
            if destinations.is_empty() {
                AppendMenuW(
                    menu,
                    MF_STRING | MF_GRAYED,
                    0,
                    wide("Create another pane or workspace first").as_ptr(),
                );
            }
            let mut point = POINT::default();
            GetCursorPos(&mut point);
            let choice = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY,
                point.x,
                point.y,
                0,
                self.window,
                std::ptr::null(),
            ) as usize;
            DestroyMenu(menu);
            if choice > 0 {
                if let Some((_, target, index)) = destinations.get(choice - 1) {
                    self.move_tab(surface, *target, *index)?;
                }
            }
        }
        self.focus_active()
    }
    fn remove_surface(&mut self, surface: SurfaceId) {
        self.surfaces.remove(&surface);
        if self
            .pending_save
            .as_ref()
            .is_some_and(|s| !s.writing && s.waiting.contains_key(&surface))
        {
            self.finish_save(Err(
                "terminal closed during checkpoint; previous save preserved".into(),
            ));
        }
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
        self.pending_finds.retain(|_, request| {
            if request.surface == surface {
                let _ = request
                    .reply
                    .try_send(json!({"error":"terminal closed during find"}));
                false
            } else {
                true
            }
        });
    }
    fn action(&mut self, action: Action) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.close_request.is_none(),
            "window is saving before close"
        );
        match action {
            Action::NewWorkspace => {
                self.zoomed = None;
                let cwd = self.locate(self.active()).unwrap().2;
                self.workspaces.push(Workspace::new(cwd));
                self.active_workspace = self.workspaces.len() - 1;
            }
            Action::Workspace(id) => {
                let index = self.workspace_index(id)?;
                if self.active_workspace != index {
                    self.zoomed = None;
                }
                self.active_workspace = index;
            }
            Action::WorkspaceMenu => return self.workspace_menu(self.workspace().id, None),
            Action::WorkspaceColor(_) => return Ok(()),
            Action::NewTab => {
                self.workspace_mut().new_tab();
            }
            Action::Vertical => {
                self.zoomed = None;
                self.workspace_mut().split(SplitDirection::Vertical);
            }
            Action::Horizontal => {
                self.zoomed = None;
                self.workspace_mut().split(SplitDirection::Horizontal);
            }
            Action::CloseTab => {
                let surface = self
                    .workspace_mut()
                    .close_active()
                    .context("cannot close the final tab")?;
                self.zoomed = None;
                self.remove_surface(surface);
            }
            Action::MoveTabMenu => return self.move_menu(),
            Action::SearchAll => return self.search_ui(search::UiAction::Show),
            Action::TogglePaneZoom => return self.toggle_zoom(self.active()),
            Action::Find => {
                // The native button explicitly opens the current terminal's find bar.
                // Background test hosts never request desktop or DOM focus.
                if !self.background_test && self.surfaces[&self.active()].ready {
                    self.focus_active()?;
                    self.surfaces[&self.active()].send(&HostMessage::OpenFind)?;
                }
                return Ok(());
            }
            Action::Tab(_, surface) => self.select(surface)?,
        }
        self.rebuild()
    }
    fn command(&mut self, request: Request, reply: ipc::Reply) -> anyhow::Result<Option<Value>> {
        let caller = request.caller_surface.map(SurfaceId);
        // A CLI invoked after `cd` on the same command line may precede the next
        // prompt's OSC report. Its native working directory is fresher context.
        if let (Some(caller), Some(cwd)) = (caller, request.caller_cwd) {
            if let Some(path) = cwd.to_str().and_then(|p| crate::cwd::local_path(p).ok()) {
                self.update_cwd(caller, path);
            }
        }
        let command = request.command;
        anyhow::ensure!(
            self.close_request.is_none()
                || matches!(
                    command,
                    Command::Tree
                        | Command::Identify
                        | Command::Capabilities
                        | Command::ReadScreen { .. }
                        | Command::Quit {
                            discard_state: true
                        }
                ),
            "window is saving before close"
        );
        match command {
            Command::Doctor | Command::ShellIntegration => {
                anyhow::bail!("this is a local CLI operation")
            }
            Command::Identify => {
                let surface = self.target(None, caller)?;
                let (workspace, pane, cwd) = self.locate(surface).unwrap();
                return Ok(Some(json!({"pid":std::process::id(),"pipe":self._ipc.name,
                "workspace":self.workspaces[workspace].id,"pane":pane,"surface":surface,"cwd":cwd,"platform":"windows"})));
            }
            Command::Capabilities => {
                return Ok(Some(json!({"platform":"windows","status":"development",
                "terminal_backend":"ConPTY/xterm.js","webview_runtime":"WebView2","browser_automation":false,
                "commands":["identify","capabilities","tree","read-screen","send-keys","send-key","split","new-tab",
                    "new-workspace","focus-pane","focus-tab","close-tab","move-tab","save-state","quit","shell-integration","find",
                    "search-all","search-results","search-cancel","search-open","resize-pane","focus-direction","toggle-pane-zoom","workspace","rename-tab"],
                "acceptance":"All release gates remain pending; see windows/acceptance.json"})))
            }
            Command::Tree => {
                let surfaces: Vec<_> = self.surfaces.iter().map(|(id, surface)| json!({"id":id,"ready":surface.ready,
                    "pid":surface.process_pid,"running":surface.session.is_some() && surface.exit_code.is_none(),
                    "exit_code":surface.exit_code,"resources_released":surface.ready && surface.session.is_none(),
                    "output_sequence":surface.output_sequence,"parsed_sequence":surface.acknowledged_sequence,
                    "cols":surface.cols,"rows":surface.rows,"cwd_reported":surface.cwd_reported,
                    "visible":surface.visible,
                    "bounds":surface.view.bounds().ok().map(|rect| { let p = rect.position.to_physical::<i32>(1.0); let s = rect.size.to_physical::<u32>(1.0); json!({"x":p.x,"y":p.y,"width":s.width,"height":s.height}) }),
                    "cwd":self.locate(*id).map(|(_,_,cwd)|cwd)})).collect();
                return Ok(Some(
                    json!({"workspaces":self.workspaces,"active_workspace":self.workspace().id,"surfaces":surfaces,
                        "zoomed_pane":self.zoomed,"layout":self.pane_layout,
                        "background_testing":self.background_test,"window_handle":self.window as usize,
                        "state":{"window":self.store.as_ref().map(|s| s.id),"path":self.store.as_ref().map(|s| &s.path),
                            "saving":self.pending_save.is_some(),"error":self.state_error}}),
                ));
            }
            Command::ReadScreen { pane, surface } => {
                anyhow::ensure!(
                    pane.is_none() || surface.is_none(),
                    "choose either pane or surface"
                );
                let id = self.target(pane, surface.map(SurfaceId).or(caller))?;
                let surface = self
                    .surfaces
                    .get(&id)
                    .context("terminal surface not found")?;
                anyhow::ensure!(surface.ready, "terminal is not ready");
                let after = surface
                    .session
                    .as_ref()
                    .map_or(surface.output_sequence, Session::barrier);
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
            Command::Find {
                query,
                surface,
                previous,
                match_case,
                regex,
                close,
            } => {
                anyhow::ensure!(close != query.is_some(), "provide a query or --close");
                let query = query.unwrap_or_default();
                anyhow::ensure!(
                    query.encode_utf16().count() <= 1024 && !query.contains(['\r', '\n', '\0']),
                    "use a single-line query of at most 1024 characters"
                );
                let id = self.target(None, surface.map(SurfaceId).or(caller))?;
                let surface = self
                    .surfaces
                    .get(&id)
                    .context("terminal surface not found")?;
                anyhow::ensure!(surface.ready, "terminal is not ready");
                anyhow::ensure!(
                    self.pending_finds.len() < 128,
                    "too many pending find requests"
                );
                let after = surface
                    .session
                    .as_ref()
                    .map_or(surface.output_sequence, Session::barrier);
                let request = Uuid::new_v4();
                surface.send(&HostMessage::Find {
                    request,
                    after,
                    query,
                    previous,
                    match_case,
                    regex,
                    close,
                    // CLI searches update the result without taking keyboard focus.
                    focus: false,
                })?;
                self.pending_finds.insert(
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
            Command::SearchAll {
                query,
                match_case,
                offset,
            } => {
                let id = self.begin_search(query, match_case, offset)?;
                return Ok(Some(self.search_status(id)?));
            }
            Command::SearchResults { search } => return Ok(Some(self.search_status(search)?)),
            Command::SearchCancel { search } => {
                self.cancel_search(search)?;
                return Ok(Some(self.search_status(search)?));
            }
            Command::SearchOpen { search, index } => {
                self.open_search(search, index, Some(reply))?;
                return Ok(None);
            }
            Command::SendKeys { pane, text } => self
                .session(self.target(Some(pane), caller)?)?
                .input(text.into_bytes())?,
            Command::SendKey { key, pane } => self
                .session(self.target(pane, caller)?)?
                .input(key_bytes(&key)?)?,
            Command::Split { direction } => {
                self.select(self.target(None, caller)?)?;
                self.action(match direction {
                    Direction::Vertical => Action::Vertical,
                    Direction::Horizontal => Action::Horizontal,
                })?;
            }
            Command::NewTab => {
                self.select(self.target(None, caller)?)?;
                self.action(Action::NewTab)?;
            }
            Command::NewWorkspace { cwd } => {
                let id = self.target(None, caller)?;
                let cwd = cwd.unwrap_or_else(|| self.locate(id).unwrap().2);
                anyhow::ensure!(cwd.is_dir(), "working directory does not exist");
                self.zoomed = None;
                self.workspaces.push(Workspace::new(cwd));
                self.active_workspace = self.workspaces.len() - 1;
                self.rebuild()?;
            }
            Command::Workspace { op } => return self.workspace_command(op, caller).map(Some),
            Command::RenameTab { surface, name } => {
                self.rename_tab(SurfaceId(surface), name)?;
            }
            Command::FocusPane { pane } => {
                let id = self.target(Some(pane), caller)?;
                self.select(id)?;
                self.rebuild()?;
            }
            Command::ResizePane { pane, ratio } => {
                let (split, ratio) = self.resize_pane(PaneId(pane), ratio)?;
                return Ok(Some(json!({"ok":true,"split":split,"ratio":ratio})));
            }
            Command::FocusDirection { direction, pane } => {
                let id = self.target(pane, caller)?;
                let target = self.focus_direction(id, direction)?;
                return Ok(Some(
                    json!({"ok":true,"focused":target.is_some(),"pane":target}),
                ));
            }
            Command::TogglePaneZoom { pane } => {
                self.toggle_zoom(self.target(pane, caller)?)?;
                return Ok(Some(json!({"ok":true,"zoomed_pane":self.zoomed})));
            }
            Command::FocusTab { surface } | Command::CloseTab { surface } => {
                let id = SurfaceId(surface);
                self.select(id)?;
                if matches!(command, Command::CloseTab { .. }) {
                    self.action(Action::CloseTab)?;
                } else {
                    self.rebuild()?;
                }
            }
            Command::MoveTab {
                surface,
                to_pane,
                index,
            } => {
                self.move_tab(
                    SurfaceId(surface),
                    PaneId(to_pane),
                    index.unwrap_or(usize::MAX),
                )?;
            }
            Command::SaveState => {
                self.begin_save(Some(reply))?;
                return Ok(None);
            }
            // The pipe worker closes only after the save succeeds and the client receives its reply.
            Command::Quit {
                discard_state: true,
            } => {
                // Explicit recovery escape hatch. A completed checkpoint is never deleted.
                return Ok(Some(json!({"ok":true})));
            }
            Command::Quit {
                discard_state: false,
            } => {
                self.request_close(CloseRequest::Ipc(reply))?;
                return Ok(None);
            }
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
