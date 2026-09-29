// SPDX-License-Identifier: GPL-3.0-or-later
use super::{
    checked, data_dir, ipc,
    session::{Session, SessionEvent},
    settings_store,
    state_store::{self, Store},
    wide,
};
use crate::{
    command::{Command, Direction, FocusDirection, Launch, Request, WorkspaceOp},
    model::{self, Workspace},
    protocol::{ClientMessage, HostMessage, Identity, TERMINAL_ORIGIN},
    state::{SavedScreen, WindowState},
};
use anyhow::Context;
use base64::Engine;
use flowmux_core::{PaneId, SplitDirection, SurfaceId, SurfaceKind, WorkspaceId};
use raw_window_handle::{HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle};
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet},
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
        Input::KeyboardAndMouse::{
            GetCapture, IsWindowEnabled, ReleaseCapture, SetCapture, SetFocus,
        },
        WindowsAndMessaging::*,
    },
};
use wry::{WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows, WebViewExtWindows};
#[path = "agent_bar.rs"]
mod agent_bar;
#[path = "agent_list.rs"]
mod agent_list;
#[path = "appearance.rs"]
mod appearance;
#[path = "browser.rs"]
mod browser;
#[path = "chrome.rs"]
mod chrome;
#[path = "command_palette.rs"]
mod command_palette;
#[path = "detached.rs"]
mod detached;
#[path = "detached_host.rs"]
mod detached_host;
#[path = "downloads.rs"]
mod downloads;
#[path = "editor.rs"]
mod editor;
#[path = "files.rs"]
mod files;
#[path = "keybindings.rs"]
mod keybindings;
#[path = "keys.rs"]
mod keys;
#[path = "notifications.rs"]
mod notifications;
#[path = "overview.rs"]
mod overview;
#[path = "panes.rs"]
mod panes;
#[path = "paste.rs"]
mod paste;
#[path = "search.rs"]
mod search;
#[path = "session_panel.rs"]
mod session_panel;
#[path = "sessions.rs"]
mod sessions;
#[path = "shells.rs"]
mod shells;
#[path = "ssh.rs"]
mod ssh;
#[path = "ssh_auth.rs"]
mod ssh_auth;
#[path = "ssh_forward.rs"]
mod ssh_forward;
#[path = "ssh_listener.rs"]
mod ssh_listener;
#[path = "ssh_panel.rs"]
mod ssh_panel;
#[path = "ssh_ports.rs"]
mod ssh_ports;
#[path = "ssh_ports_panel.rs"]
mod ssh_ports_panel;
#[path = "surface_host.rs"]
pub(super) mod surface_host;
#[path = "tab_menu.rs"]
mod tab_menu;
#[path = "usage.rs"]
mod usage;
#[path = "usage_panel.rs"]
mod usage_panel;
#[path = "workspaces.rs"]
mod workspaces;
#[path = "worktree_panel.rs"]
mod worktree_panel;
#[path = "worktrees.rs"]
mod worktrees;

const WAKE: u32 = WM_APP + 1;
thread_local! {
    static EVENTS: RefCell<Option<EventSender>> = const { RefCell::new(None) };
    static CONTROL_ACTIONS: RefCell<HashMap<isize, Action>> = RefCell::new(HashMap::new());
}
enum Event {
    EmptyWindowShortcut(crate::keybindings::ActionId),
    SshDialog(Uuid, ssh_panel::UiAction),
    SshPorts(Uuid, ssh_ports_panel::UiAction),
    SshForwardUi(SurfaceId, ssh_forward::UiAction),
    SshAuthenticationUi(SurfaceId, Uuid, ssh_auth::UiAction),
    TabMenu(Uuid, tab_menu::UiAction),
    WorkspaceClose(Uuid, bool),
    Editor(editor::Signal),
    Files(files::Signal),
    Worktrees(worktrees::Signal),
    Browser(browser::Signal),
    Layout,
    WindowMoved,
    Detached(SurfaceId, detached::Signal),
    SidebarScroll(workspaces::Scroll),
    Tick,
    Close,
    ExitAfterReply,
    Saved(Result<(), String>),
    Button(Action),
    Bridge(SurfaceId, String, String),
    Session(SurfaceId, Uuid, SessionEvent),
    Command(Request, ipc::Reply),
    SearchUi(search::UiAction),
    CommandPalette(command_palette::UiAction),
    BrowserFindUi(browser::find::UiAction),
    Pointer(panes::Pointer),
    ContextMenu(Action, i32, i32),
    Metadata(Uuid, workspaces::EditAction),
    Settings(settings_store::Update),
    NotificationUi(notifications::UiAction),
    Sessions(sessions::Signal),
    AgentsReady,
    AgentChanged,
    AgentBar(agent_bar::UiAction),
    UsageUi(usage_panel::UiAction),
    UsageResult(Uuid, [crate::usage::ProviderRefresh; 2]),
    OptionsUi(appearance::UiAction),
    Overview(overview::Signal),
    Download(downloads::Signal),
    Activated,
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

// Intercept reorderable rows before BUTTON's default handler takes capture/focus.
// Stable IDs are queued now; a later rebuild must not retarget this gesture.
unsafe fn row_pointer(window: HWND, message: u32, lparam: LPARAM) -> bool {
    if !matches!(
        message,
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_MOUSEMOVE | WM_LBUTTONUP
    ) {
        return false;
    }
    let action = CONTROL_ACTIONS.with(|actions| actions.borrow().get(&(window as isize)).cloned());
    let agent = agent_bar::target(window);
    if !matches!(action, Some(Action::Tab(..) | Action::Workspace(_))) && agent.is_none() {
        return false;
    }
    let parent = if agent.is_some() {
        GetAncestor(window, GA_ROOT)
    } else {
        GetParent(window)
    };
    let mut point = POINT {
        x: lparam as u16 as i16 as i32,
        y: (lparam >> 16) as u16 as i16 as i32,
    };
    MapWindowPoints(window, parent, &mut point, 1);
    if IsWindowEnabled(parent) != 0 {
        post(Event::Pointer(match message {
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK if agent.is_some() => panes::Pointer::AgentDown {
                surface: agent.unwrap(),
                x: point.x,
                y: point.y,
            },
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => match action {
                Some(Action::Tab(pane, surface)) => panes::Pointer::TabDown {
                    pane,
                    surface,
                    x: point.x,
                    y: point.y,
                    double_click: message == WM_LBUTTONDBLCLK,
                },
                Some(Action::Workspace(workspace)) => panes::Pointer::WorkspaceDown {
                    workspace,
                    x: point.x,
                    y: point.y,
                },
                _ => unreachable!("reorderable control checked above"),
            },
            WM_LBUTTONUP => panes::Pointer::Up(point.x, point.y),
            _ => panes::Pointer::Move(point.x, point.y),
        }));
    }
    message != WM_MOUSEMOVE
}

// WebView2 delivers accelerator events on the controller's UI thread even
// when the renderer owns keyboard focus. Cancel capture without blurring IME.
pub(super) fn install_drag_escape(view: &WebView, window: HWND) -> anyhow::Result<()> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN, COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
    };
    let holder = window as isize;
    let mut token = 0;
    unsafe {
        view.controller().add_AcceleratorKeyPressed(
            &webview2_com::AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else {
                    return Ok(());
                };
                let owner = GetAncestor(holder as HWND, GA_ROOT);
                if owner.is_null() || GetCapture() != owner {
                    return Ok(());
                }
                let mut key = 0;
                let mut kind = Default::default();
                args.VirtualKey(&mut key)?;
                args.KeyEventKind(&mut kind)?;
                if key == 0x1b
                    && matches!(
                        kind,
                        COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                            | COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                    )
                {
                    args.SetHandled(true)?;
                    // The retained holder may now belong to a detached frame.
                    PostMessageW(owner, WM_CANCELMODE, 0, 0);
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    keybindings::native_key_guard(window, message);
    if message != WM_CTLCOLORSTATIC {
        if let Some(result) = chrome::message(window, message, wparam, lparam) {
            return result;
        }
    }
    match message {
        WM_MOUSEWHEEL => {
            let mut point = POINT {
                x: lparam as u16 as i16 as i32,
                y: (lparam >> 16) as u16 as i16 as i32,
            };
            ScreenToClient(window, &mut point);
            let sidebar = panes::cached_sidebar_width();
            if point.x >= 0 && point.x < sidebar {
                let delta = (wparam >> 16) as u16 as i16 as i32;
                if delta != 0 {
                    post(Event::SidebarScroll(workspaces::Scroll::Wheel(-delta)));
                }
                return 0;
            }
            DefWindowProcW(window, message, wparam, lparam)
        }
        WM_CONTEXTMENU => {
            let mut action =
                CONTROL_ACTIONS.with(|actions| actions.borrow().get(&(wparam as isize)).cloned());
            let mut x = lparam as u16 as i16 as i32;
            let mut y = (lparam >> 16) as u16 as i16 as i32;
            if action.is_none() && wparam as HWND == window && (x, y) != (-1, -1) {
                let mut point = POINT { x, y };
                let mut bounds = RECT::default();
                ScreenToClient(window, &mut point);
                GetClientRect(window, &mut bounds);
                let dpi = GetDpiForWindow(window).max(96) as i32;
                if point.x >= 0
                    && point.x < panes::cached_sidebar_width()
                    && point.y >= 40 * dpi / 96
                    && point.y < bounds.bottom - 36 * dpi / 96
                {
                    action = Some(Action::WorkspaceMenu);
                }
            }
            if let Some(action) = action {
                if x == -1 && y == -1 {
                    let Some(rect) = chrome::visible_control_rect(wparam as HWND) else {
                        return 0;
                    };
                    x = rect.left;
                    y = rect.bottom;
                }
                post(Event::ContextMenu(action, x, y));
            }
            0
        }
        WM_CTLCOLORSTATIC => chrome::message(window, message, wparam, lparam)
            .unwrap_or_else(|| DefWindowProcW(window, message, wparam, lparam)),
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
        WM_MOVE => {
            post(Event::WindowMoved);
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
        WM_ACTIVATE => {
            if (wparam as u16) != WA_INACTIVE as u16 {
                post(Event::Activated);
            } else {
                post(Event::Pointer(panes::Pointer::Cancel));
            }
            DefWindowProcW(window, message, wparam, lparam)
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
                    if matches!(action, Action::WorkspaceClose(_))
                        && (GetParent(lparam as HWND) != window
                            || GetDlgCtrlID(lparam as HWND) != (wparam as u16) as i32
                            || GetWindowLongPtrW(lparam as HWND, GWL_STYLE) as u32 & WS_VISIBLE
                                == 0
                            || IsWindowEnabled(lparam as HWND) == 0
                            || IsWindowEnabled(window) == 0)
                    {
                        return 0;
                    }
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
            if wparam == browser::wait::TIMER {
                post(Event::Browser(browser::Signal::WaitTick));
            } else if wparam == editor::search::TIMER {
                post(Event::Editor(editor::Signal::SearchTick));
            } else if wparam == files::TIMER {
                post(Event::Files(files::Signal::Tick));
            } else if wparam == 1 {
                post(Event::Tick);
            }
            0
        }
        WAKE => 0,
        notifications::DESKTOP_MESSAGE => {
            post(Event::NotificationUi(notifications::UiAction::Desktop(wparam as u32, lparam as u32)));
            0
        }
        message if message != 0 && message == notifications::restart_message() => {
            post(Event::NotificationUi(notifications::UiAction::DesktopRestarted));
            0
        }
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
    startup_error: Option<String>,
    applied_settings: Option<Value>,
    menu_capabilities: Option<(PaneId, bool, bool)>,
    view: WebView,
    // WebView2 must close before its stable native parent is destroyed.
    holder: surface_host::Host,
    identity: Identity,
    session: Option<Session>,
    session_generation: Uuid,
    session_after: u64,
    ssh_connected: bool,
    pending_tig: bool,
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
    notification_sniffer: crate::notifications::Sniffer,
    observed_output_bytes: u64,
    last_output_ms: Option<i64>,
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
#[derive(Clone, PartialEq, Eq)]
enum Action {
    OpenEditor,
    ShowFiles,
    Worktrees,
    NewBrowser,
    Notifications,
    Usage,
    AgentBar,
    Sessions,
    Settings,
    CommandPalette,
    Overview,
    NewWorkspace,
    NewWindow,
    NewSshWorkspace,
    SshStatus(WorkspaceId),
    SshConnect(WorkspaceId),
    SshDisconnect(WorkspaceId),
    SshAuthentication(WorkspaceId),
    SshPorts(WorkspaceId),
    Workspace(WorkspaceId),
    WorkspaceClose(WorkspaceId),
    WorkspaceMenu,
    NewTab,
    Vertical,
    Horizontal,
    CloseTab,
    MoveTabMenu,
    DetachTab,
    Find,
    SearchAll,
    TogglePaneZoom,
    Tab(PaneId, SurfaceId),
    TabClose(PaneId, SurfaceId),
    PaneAdd(PaneId, SurfaceId),
    PaneMenu(PaneId, SurfaceId),
    PaneZoom(PaneId, SurfaceId),
    PaneSplitRight(PaneId, SurfaceId),
    PaneSplitDown(PaneId, SurfaceId),
    PaneBrowser(PaneId, SurfaceId),
    SidebarScroll(i32),
    EmptyState,
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
struct PendingScreen {
    read: PendingRead,
    mode: crate::screen::Mode,
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
    shells: HashMap<SurfaceId, crate::shell::Shell>,
    startup_shells: HashMap<SurfaceId, crate::shell::Shell>,
    settings_worker: settings_store::Worker,
    settings: crate::settings::Document,
    settings_error: Option<String>,
    settings_pending: HashMap<Uuid, (Option<ipc::Reply>, Option<Uuid>)>,
    window: HWND,
    sender: EventSender,
    _ipc: ipc::Server,
    context: WebContext,
    // Native deferrals/download objects must drop before browser WebViews even on early errors.
    browser_popups: browser::popup::Controller,
    downloads: downloads::Controller,
    browsers: HashMap<SurfaceId, browser::Browser>,
    editors: HashMap<SurfaceId, editor::Editor>,
    editor_assets: Option<crate::editor_assets::EditorAssets>,
    editor_context: Option<WebContext>,
    editor_data_root: Option<PathBuf>,
    editor_preparer: Option<crate::editor_open::OpenPreparer>,
    editor_picker_pending: bool,
    editor_open_pending: HashMap<u64, editor::PendingOpen>,
    editor_request: u64,
    editor_search_service: Option<crate::editor_search::Service>,
    files: files::Controller,
    editor_barrier: Option<editor::Barrier>,
    editor_bypass: bool,
    browser_context: Option<WebContext>,
    pending_browser: HashMap<Uuid, browser::Pending>,
    pending_captures: HashMap<Uuid, browser::capture::Pending>,
    browser_waits: HashMap<Uuid, browser::wait::Wait>,
    browser_tokens: crate::browser_dom::Tokens,
    workspaces: Vec<Workspace>,
    active_workspace: usize,
    surfaces: HashMap<SurfaceId, Surface>,
    // The retained terminal's WebView/holder must drop before this parent HWND.
    ssh_auth_window: Option<(SurfaceId, ssh_auth::Window)>,
    detached: HashMap<SurfaceId, detached::Window>,
    detached_focus: Option<SurfaceId>,
    main_closed: bool,
    controls: Vec<Control>,
    sidebar_offset: i32,
    sidebar_wheel_remainder: i32,
    sidebar_width_dip: u32,
    sidebar_active: Option<WorkspaceId>,
    sidebar_mru: RefCell<HashMap<WorkspaceId, Vec<PaneId>>>,
    sidebar_metrics: (i32, u32, i32),
    pane_layout: model::Layout,
    zoomed: Option<PaneId>,
    drag: Option<panes::Drag>,
    drop_preview: Option<chrome::DropPreview>,
    metadata: Option<workspaces::Panel>,
    tab_menu: Option<tab_menu::Menu>,
    copy_feedback: Option<tab_menu::CopyFeedback>,
    workspace_close: Option<workspaces::Close>,
    ssh_dialog: Option<ssh_panel::Panel>,
    ssh_disconnected: HashSet<WorkspaceId>,
    ssh_attempted: HashSet<SurfaceId>,
    ssh_ports: Option<ssh_ports_panel::Panel>,
    ssh_forwards: HashMap<Uuid, ssh_forward::Forward>,
    initial_cwd: PathBuf,
    options: Option<appearance::Panel>,
    command_palette: command_palette::Controller,
    overview: overview::Controller,
    worktrees: worktrees::Controller,
    pending_reads: HashMap<Uuid, PendingScreen>,
    pending_finds: HashMap<Uuid, PendingRead>,
    pending_pastes: HashMap<Uuid, PendingRead>,
    pending_keys: HashMap<Uuid, keys::PendingKey>,
    empty_window_keys: keybindings::WindowKeys,
    #[cfg(debug_assertions)]
    pending_terminal_ui_tests: HashMap<Uuid, PendingRead>,
    pending_selections: HashMap<Uuid, PendingRead>,
    pending_minimaps: HashMap<Uuid, PendingRead>,
    search: search::Controller,
    browser_find: browser::find::Controller,
    browser_bookmarks: browser::bookmarks::Controller,
    notifications: notifications::Controller,
    usage: usage::Controller,
    sessions: sessions::Controller,
    pending_agents: Option<agent_list::Pending>,
    agent_states: RefCell<HashMap<SurfaceId, agent_list::State>>,
    last_agent_scan: Instant,
    agent_scan_after: Option<SurfaceId>,
    agent_bar: Option<agent_bar::Bar>,
    agent_bar_order: Vec<SurfaceId>,
    activity_height_dip: Option<i32>,
    closing: bool,
    close_accepted: bool,
    background_test: bool,
    #[cfg(debug_assertions)]
    last_new_window_pid: Option<u32>,
    store: Option<Arc<Store>>,
    restore_screens: HashMap<SurfaceId, SavedScreen>,
    pending_save: Option<PendingSave>,
    close_request: Option<CloseRequest>,
    last_save_attempt: Instant,
    state_error: Option<String>,
}

pub fn run(launch: Launch) -> anyhow::Result<()> {
    let initial_shell = launch.shell.requested()?;
    let background_test =
        cfg!(debug_assertions) && std::env::var("FLOWMUX_TEST_BACKGROUND").as_deref() == Ok("1");
    // Resolve storage once at startup, before the UI loop. Hidden hosts never
    // fall back to a real user editor profile when no isolated root was supplied.
    let editor_data_root = if background_test {
        std::env::var_os("FLOWMUX_TEST_STATE_DIR").map(PathBuf::from)
    } else {
        Some(data_dir()?)
    };
    let terminal_data_root = if background_test {
        std::env::var_os("FLOWMUX_TEST_STATE_DIR")
            .or_else(|| std::env::var_os("FLOWMUX_TEST_ARTIFACT_ROOT"))
            .map(PathBuf::from)
            .context("background terminal requires an isolated profile directory")?
    } else {
        data_dir()?
    };
    let (store, restored) = if launch.temporary {
        (None, None)
    } else {
        state_store::open(
            launch.new_window || launch.cwd.is_some() || initial_shell.is_some(),
            launch.restore_window,
            background_test,
        )?
    };
    let cwd = launch
        .cwd
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .context("No initial working directory")?;
    let cwd = std::path::absolute(cwd)?;
    anyhow::ensure!(
        cwd.is_dir(),
        "Working directory does not exist: {}",
        cwd.display()
    );
    let restoring_window = restored.is_some();
    let restore_detached = restored
        .as_ref()
        .map(|state| state.detached_windows.clone())
        .unwrap_or_default();
    let restore_main_closed = restored.as_ref().is_some_and(|state| state.main_closed);
    let restore_detached_focus = restored.as_ref().and_then(|state| state.detached_focus);
    let sidebar_width_dip = restored
        .as_ref()
        .map_or(crate::state::DEFAULT_SIDEBAR_WIDTH, |state| {
            state.sidebar_width_dip
        });
    let (mut workspaces, active_workspace, restore_screens, mut shells) = match restored {
        Some(state) => {
            let active = state
                .workspaces
                .iter()
                .position(|w| Some(w.id) == state.active_workspace)
                .unwrap_or(0);
            (state.workspaces, active, state.screens, state.shells)
        }
        None => (
            vec![Workspace::new(cwd.clone())],
            0,
            HashMap::new(),
            HashMap::new(),
        ),
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
        let settings_events = sender.clone();
        let (settings_worker, initial_settings) =
            settings_store::Worker::start(background_test, move |update| {
                settings_events.send(Event::Settings(update))
            })?;
        let settings_error = initial_settings.as_ref().err().cloned();
        let settings = initial_settings.unwrap_or_default();
        let mut ssh_attempted = HashSet::new();
        if restoring_window {
            // Old checkpoints always launched Windows PowerShell. Changing the
            // default affects future terminals, never silently changes old ones.
            for ws in &workspaces {
                for (_, _, tabs) in ws.leaves() {
                    for tab in tabs {
                        match &tab.kind {
                            SurfaceKind::Terminal { .. } => {
                                shells.entry(tab.id).or_default();
                            }
                            SurfaceKind::SshTerminal { cwd, tmux_session } => {
                                ssh_attempted.insert(tab.id);
                                let config = ws
                                    .ssh
                                    .as_ref()
                                    .context("SSH workspace configuration missing")?;
                                shells.insert(
                                    tab.id,
                                    crate::ssh::terminal_shell(
                                        config,
                                        cwd.as_deref(),
                                        tmux_session.as_deref(),
                                        true,
                                    )?,
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
        } else {
            shells.insert(
                workspaces[0].active(),
                initial_shell.unwrap_or_else(|| settings.default_shell.clone()),
            );
        }
        let ssh_disconnected = workspaces
            .iter()
            .filter(|w| restoring_window && w.ssh.is_some())
            .map(|w| w.id)
            .collect();
        let mut app = App {
            ssh_disconnected,
            ssh_attempted,
            ssh_ports: None,
            ssh_forwards: HashMap::new(),
            #[cfg(debug_assertions)]
            last_new_window_pid: None,
            ssh_auth_window: None,
            shells,
            startup_shells: HashMap::new(),
            settings_worker,
            settings,
            settings_error,
            settings_pending: HashMap::new(),
            window,
            sender,
            _ipc: ipc,
            context: WebContext::new(Some(terminal_data_root.join("terminal-profile"))),
            browser_context: None,
            browser_popups: browser::popup::Controller::default(),
            browsers: HashMap::new(),
            editors: HashMap::new(),
            editor_assets: None,
            editor_context: None,
            editor_data_root,
            editor_preparer: None,
            editor_picker_pending: false,
            editor_open_pending: HashMap::new(),
            editor_request: 0,
            editor_search_service: None,
            files: files::Controller::default(),
            editor_barrier: None,
            editor_bypass: false,
            pending_browser: HashMap::new(),
            pending_captures: HashMap::new(),
            browser_waits: HashMap::new(),
            browser_tokens: crate::browser_dom::Tokens::default(),
            workspaces,
            active_workspace,
            surfaces: HashMap::new(),
            detached: HashMap::new(),
            detached_focus: None,
            main_closed: false,
            controls: vec![],
            sidebar_offset: 0,
            sidebar_wheel_remainder: 0,
            sidebar_width_dip,
            sidebar_active: None,
            sidebar_mru: RefCell::new(HashMap::new()),
            sidebar_metrics: (0, 0, 0),
            pane_layout: model::Layout::default(),
            zoomed: None,
            drag: None,
            drop_preview: None,
            metadata: None,
            tab_menu: None,
            copy_feedback: None,
            workspace_close: None,
            ssh_dialog: None,
            initial_cwd: cwd.clone(),
            options: None,
            command_palette: command_palette::Controller::default(),
            overview: overview::Controller::default(),
            worktrees: worktrees::Controller::default(),
            pending_reads: HashMap::new(),
            pending_finds: HashMap::new(),
            pending_pastes: HashMap::new(),
            pending_keys: HashMap::new(),
            empty_window_keys: keybindings::WindowKeys::default(),
            #[cfg(debug_assertions)]
            pending_terminal_ui_tests: HashMap::new(),
            pending_selections: HashMap::new(),
            pending_minimaps: HashMap::new(),
            search: search::Controller::default(),
            browser_find: browser::find::Controller::default(),
            browser_bookmarks: browser::bookmarks::Controller::default(),
            notifications: notifications::Controller::default(),
            usage: usage::Controller::default(),
            sessions: sessions::Controller::default(),
            pending_agents: None,
            agent_states: RefCell::new(HashMap::new()),
            last_agent_scan: Instant::now(),
            agent_scan_after: None,
            agent_bar: None,
            agent_bar_order: vec![],
            activity_height_dip: None,
            downloads: downloads::Controller::default(),
            closing: false,
            close_accepted: false,
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
        chrome::configure_settings(&app.settings.terminal, GetDpiForWindow(window).max(96));
        chrome::window_theme(window, app.settings.terminal.theme);
        app.restore_detached(
            restore_detached,
            restore_main_closed,
            restore_detached_focus,
        )?;
        app.usage_initialize()?;
        app.rebuild_without_focus()?;
        if !app.background_test && !app.main_closed {
            ShowWindow(window, SW_SHOW);
        }
        for window in app.detached.values() {
            window.show(app.background_test);
        }
        if let Some(surface) = app.detached_focus {
            app.select(surface)?;
        }
        app.focus_active()?;
        SetTimer(window, 1, 1000, None);
        let result = message_loop(&mut app, events);
        app.pending_agents.take();
        app.agent_bar.take();
        app.sessions.shutdown();
        app.usage.shutdown();
        app.worktrees.shutdown();
        app.files_shutdown();
        app.editor_cancel_opens(None, "window closed before editor Open completed");
        app.editor_preparer.take();
        app.editor_search_service.take();
        app._ipc.shutdown(); // Stop accepting commands before terminal teardown.

        // Cancel and release native download operations before their WebView
        // controllers close (older runtimes invalidate these COM objects).
        app.browser_popups.shutdown();
        drop(std::mem::take(&mut app.overview));
        drop(std::mem::take(&mut app.browser_bookmarks));
        drop(std::mem::take(&mut app.browser_find));
        drop(std::mem::take(&mut app.downloads));
        app.ssh_dialog.take();
        app.ssh_ports.take();
        app.workspace_close.take();
        app.metadata.take();
        app.tab_menu.take();
        app.copy_feedback.take();
        app.browsers.clear();
        app.ssh_forwards.clear();
        app.editors.clear();
        app.surfaces.clear(); // Parent HWND must outlive every WebView controller.
        app.ssh_auth_window.take();
        app.detached.clear();
        drop(app);
        EVENTS.with(|slot| *slot.borrow_mut() = None);
        DestroyWindow(window);
        chrome::shutdown();
        CoUninitialize();
        result
    }
}
fn message_loop(app: &mut App, events: Receiver<Event>) -> anyhow::Result<()> {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        app.sync_browser_keys();
        let result = unsafe { GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) };
        if result == 0 {
            return Ok(());
        }
        anyhow::ensure!(result != -1, "Windows message loop failed");
        // GetMessage dispatches sent IME/focus messages too. Apply only their
        // guards before keys; ordinary events stay after DispatchMessage.
        app.apply_native_key_guards();
        if message.message == WM_KEYDOWN && message.wParam == 0x1b {
            let owner = unsafe { GetAncestor(message.hwnd, GA_ROOT) };
            if app
                .detached
                .values()
                .find(|window| window.window == owner)
                .is_some_and(detached::Window::cancel_drag)
            {
                continue;
            }
        }
        if message.message == WM_KEYDOWN
            && message.wParam == 0x1b
            && matches!(
                app.drag,
                Some(
                    panes::Drag::Tab { .. }
                        | panes::Drag::Workspace { .. }
                        | panes::Drag::Agent { .. }
                        | panes::Drag::Activity { .. }
                )
            )
        {
            app.cancel_drag();
            continue;
        }
        unsafe {
            if !app.empty_window_shortcut(&message)
                && !app.sidebar_scroll_key(&message)
                && !app
                    .agent_bar
                    .as_ref()
                    .is_some_and(|bar| bar.handle_message(&message, app.background_test))
                && !chrome::workspace_close_key(&message)
                && !app
                    .tab_menu
                    .as_ref()
                    .is_some_and(|menu| menu.handle_message(&message))
                && !app
                    .workspace_close
                    .as_ref()
                    .is_some_and(|close| close.panel.handle_message(&message))
                && !app
                    .ssh_dialog
                    .as_ref()
                    .is_some_and(|panel| panel.handle_message(&message))
                && !app
                    .ssh_ports
                    .as_ref()
                    .is_some_and(|panel| panel.handle_message(&message))
                && !app.editor_close_handle_message(&message)
                && !app.command_palette.handle_message(&message)
                && !app.overview_handle_message(&message)
                && !app.options_handle_message(&message)
                && !app.search.handle_message(&message)
                && !app.worktrees.handle_message(&message)
                && !app.files_handle_message(&message)
                && !app.notifications.handle_message(&message)
                && !app.usage.handle_message(&message)
                && !app.sessions.handle_message(&message)
                && !app.downloads.handle_message(&message)
                && !app.browser_bookmarks.handle_message(&message)
                && !app.browser_find.handle_message(&message)
                && !app.browsers.iter().any(|(id, browser)| {
                    browser.handle_chrome_message(*id, &message, &app.settings)
                })
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
    fn current_workspace(&self) -> Option<&Workspace> {
        self.workspaces
            .get(self.active_workspace)
            .filter(|workspace| !self.is_detached_workspace(workspace.id))
    }
    fn current_surface(&self) -> Option<SurfaceId> {
        self.detached_focus
            .filter(|id| self.detached.contains_key(id))
            .or_else(|| self.current_workspace().map(Workspace::active))
    }
    fn active(&self) -> SurfaceId {
        self.current_surface()
            .expect("active surface required by this operation")
    }
    fn empty_action(action: &Action) -> bool {
        matches!(
            action,
            Action::NewWorkspace
                | Action::NewWindow
                | Action::NewSshWorkspace
                | Action::WorkspaceMenu
                | Action::Settings
                | Action::CommandPalette
                | Action::Notifications
                | Action::Usage
                | Action::Sessions
                | Action::AgentBar
                | Action::SidebarScroll(_)
                | Action::EmptyState
        )
    }
    fn rebuild(&mut self) -> anyhow::Result<()> {
        self.rebuild_without_focus()?;
        self.focus_active()
    }
    fn rebuild_without_focus(&mut self) -> anyhow::Result<()> {
        self.empty_window_keys = keybindings::WindowKeys::default();
        self.cancel_drag();
        CONTROL_ACTIONS.with(|actions| actions.borrow_mut().clear());
        let mut missing = Vec::new();
        for workspace in &self.workspaces {
            for (_, _, tabs) in workspace.leaves() {
                for tab in tabs {
                    if !self.surfaces.contains_key(&tab.id)
                        && !self.browsers.contains_key(&tab.id)
                        && !self.editors.contains_key(&tab.id)
                    {
                        missing.push((tab.id, tab.kind));
                    }
                }
            }
        }
        for (id, kind) in missing {
            match kind {
                SurfaceKind::Terminal { .. } | SurfaceKind::SshTerminal { .. } => {
                    self.add_view(id)?
                }
                SurfaceKind::Editor {
                    workspace_root,
                    session,
                } => self.add_editor_view(id, workspace_root, session)?,
                SurfaceKind::Browser { initial_url } => {
                    self.add_browser_view(id, initial_url.unwrap_or_else(|| "about:blank".into()))?
                }
            }
            if let Some(window) = self.detached.get(&id) {
                self.surface_holder(id)?.reparent(window.window)?;
                self.refresh_tab_title(id);
            }
        }
        for workspace in &mut self.workspaces {
            workspace.refresh_name();
        }
        if self.main_closed {
            self.refresh_chrome_metadata();
            return self.layout();
        }
        let mut desired = Vec::new();
        for (name, action) in [
            ("New workspace", Action::NewWorkspace),
            ("Workspaces", Action::WorkspaceMenu),
            ("Settings", Action::Settings),
            ("Command Palette", Action::CommandPalette),
            ("Workspace overview", Action::Overview),
            ("Files", Action::ShowFiles),
            ("Worktrees", Action::Worktrees),
            ("Search", Action::SearchAll),
            ("Open file", Action::OpenEditor),
            ("Notices", Action::Notifications),
            ("AI usage (Ctrl+Alt+U)", Action::Usage),
            ("Agents bar", Action::AgentBar),
            ("Agent sessions (Ctrl+Alt+J)", Action::Sessions),
            ("Previous", Action::SidebarScroll(-1)),
            ("Next", Action::SidebarScroll(1)),
        ] {
            desired.push((name.to_owned(), action));
        }
        for index in self.main_workspace_indices() {
            let id = self.workspaces[index].id;
            desired.push((
                self.workspace_caption(id).unwrap_or_default(),
                Action::Workspace(id),
            ));
            desired.push(("Close workspace".into(), Action::WorkspaceClose(id)));
        }
        if self.current_workspace().is_none() {
            desired.push(("flowmux\nNo workspaces yet".into(), Action::EmptyState));
        }
        if let Some(workspace) = self.current_workspace().filter(|w| w.ssh.is_some()) {
            let id = workspace.id;
            desired.extend([
                (self.ssh_toolbar_caption(id), Action::SshStatus(id)),
                ("Connect".into(), Action::SshConnect(id)),
                ("Disconnect".into(), Action::SshDisconnect(id)),
                ("Authentication".into(), Action::SshAuthentication(id)),
                ("Ports".into(), Action::SshPorts(id)),
            ]);
        }
        for (pane, active, tabs) in self
            .current_workspace()
            .into_iter()
            .flat_map(Workspace::leaves)
        {
            for tab in tabs {
                desired.push((tab.title.clone(), Action::Tab(pane, tab.id)));
                desired.push(("Close tab".into(), Action::TabClose(pane, tab.id)));
            }
            desired.push(("Maximize pane".into(), Action::PaneZoom(pane, active)));
            desired.push(("Split right".into(), Action::PaneSplitRight(pane, active)));
            desired.push(("Split down".into(), Action::PaneSplitDown(pane, active)));
            desired.push(("+".into(), Action::PaneAdd(pane, active)));
            desired.push(("New browser".into(), Action::PaneBrowser(pane, active)));
            desired.push(("Pane actions".into(), Action::PaneMenu(pane, active)));
        }
        let same_controls = self.controls.len() == desired.len()
            && self
                .controls
                .iter()
                .zip(&desired)
                .all(|(control, (_, action))| {
                    control.action == *action
                        || matches!((&control.action,action),
                (Action::PaneAdd(a,_),Action::PaneAdd(b,_))|
                (Action::PaneMenu(a,_),Action::PaneMenu(b,_))|
                (Action::PaneZoom(a,_),Action::PaneZoom(b,_))|
                (Action::PaneSplitRight(a,_),Action::PaneSplitRight(b,_))|
                (Action::PaneSplitDown(a,_),Action::PaneSplitDown(b,_))|
                (Action::PaneBrowser(a,_),Action::PaneBrowser(b,_)) if a==b)
                });
        if same_controls {
            for (control, (label, action)) in self.controls.iter_mut().zip(&desired) {
                control.action = action.clone();
                CONTROL_ACTIONS.with(|actions| {
                    actions
                        .borrow_mut()
                        .insert(control.hwnd as isize, action.clone())
                });
                workspaces::set_caption(control.hwnd, label);
            }
            self.refresh_chrome_metadata();
        } else {
            self.usage.hide_panel();
            for control in self.controls.drain(..) {
                chrome::unregister(control.hwnd);
                unsafe {
                    DestroyWindow(control.hwnd);
                }
            }
            for (label, action) in desired {
                self.button(&label, action)?;
            }
        }
        for control in &self.controls {
            if let Action::Workspace(id) = control.action {
                if let Some(close) = self.controls.iter().find(|candidate|
                    matches!(candidate.action, Action::WorkspaceClose(other) if other == id)) {
                    chrome::register_workspace_close(control.hwnd, close.hwnd);
                }
            }
        }
        self.refresh_notifications();
        self.overview_refresh()?;
        self.layout()
    }
    fn button(&mut self, name: &str, action: Action) -> anyhow::Result<()> {
        let empty = matches!(action, Action::EmptyState);
        let label = matches!(action, Action::SshStatus(_));
        let id = self.controls.len() + 100;
        anyhow::ensure!(id < 65535, "too many controls");
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                wide(if empty || label { "STATIC" } else { "BUTTON" }).as_ptr(),
                wide(name.replace('&', "&&")).as_ptr(),
                WS_CHILD
                    | if matches!(action, Action::Workspace(_) | Action::WorkspaceClose(_)) {
                        WS_CLIPSIBLINGS
                    } else {
                        0
                    }
                    | if matches!(action, Action::WorkspaceClose(_)) {
                        0
                    } else {
                        WS_VISIBLE
                    }
                    | if empty {
                        windows_sys::Win32::System::SystemServices::SS_CENTER
                            | windows_sys::Win32::System::SystemServices::SS_NOPREFIX
                    } else if label {
                        windows_sys::Win32::System::SystemServices::SS_LEFTNOWORDWRAP
                            | windows_sys::Win32::System::SystemServices::SS_ENDELLIPSIS
                            | windows_sys::Win32::System::SystemServices::SS_NOPREFIX
                            | windows_sys::Win32::System::SystemServices::SS_CENTERIMAGE
                    } else {
                        WS_TABSTOP | BS_OWNERDRAW as u32
                    }
                    | if matches!(action, Action::Tab(..)) {
                        BS_NOTIFY as u32
                    } else {
                        0
                    },
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
        let role = self.chrome_role(&action);
        if empty {
            chrome::register_control(hwnd, chrome::ControlRole::EmptyState);
        } else if label {
            chrome::register_control(hwnd, chrome::ControlRole::Static);
        } else {
            chrome::register_button(hwnd, role);
        }

        CONTROL_ACTIONS.with(|actions| actions.borrow_mut().insert(hwnd as isize, action.clone()));
        self.controls.push(Control { hwnd, action });
        Ok(())
    }
    fn add_view(&mut self, surface: SurfaceId) -> anyhow::Result<()> {
        let spec = self
            .shells
            .entry(surface)
            .or_insert_with(|| self.settings.default_shell.clone())
            .clone();
        if spec.program != "powershell" {
            if let Some((ws, pane, _)) = self.locate(surface) {
                if self.workspaces[ws].root.surface_title(pane, surface) == Some("PowerShell") {
                    self.workspaces[ws].root.set_surface_title_auto(
                        pane,
                        surface,
                        spec.program.clone(),
                    );
                }
            }
        }
        let bindings = crate::keybindings::resolved(&self.settings.keybindings)?;
        let view = self.terminal_view(surface, self.window, bindings)?;
        self.surfaces.insert(surface, view);
        Ok(())
    }
    fn terminal_view(
        &mut self,
        surface: SurfaceId,
        parent: HWND,
        bindings: Vec<crate::keybindings::Binding>,
    ) -> anyhow::Result<Surface> {
        let identity = Identity::new(surface.0);
        let dispatch = self.sender.clone();
        let init = format!(
            "window.__flowmuxIdentity={};window.__flowmuxSettings={};window.__flowmuxBindings={};window.__flowmuxBackgroundTesting={};window.__flowmuxTheme={};",
            serde_json::to_string(&identity)?,
            serde_json::to_string(&self.settings)?,
            serde_json::to_string(&bindings)?, self.background_test,
            serde_json::to_string(&crate::theme::resolve(&self.settings.terminal))?
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
        let holder = surface_host::Host::new(parent)?;
        let view = WebViewBuilder::new_with_web_context(&mut self.context)
            .with_background_color((40, 44, 52, 255))
            .with_devtools(false)
            .with_hotkeys_zoom(false)
            .with_browser_accelerator_keys(false)
            .with_visible(false)
            .with_focused(false)
            .with_clipboard(!self.background_test)
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
                    .header("Cache-Control", "no-store")
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
                x: 0,
                y: 0,
                width: 800,
                height: 600,
            }))
            .with_url("flowmux-terminal://localhost/")
            .build_as_child(&Parent(holder.window))
            .context("Cannot create the terminal WebView2 view")?;
        install_drag_escape(&view, holder.window)?;
        Ok(Surface {
            startup_error: None,
            applied_settings: None,
            menu_capabilities: None,
            view,
            holder,
            identity,
            session: None,
            session_generation: Uuid::nil(),
            session_after: 0,
            ssh_connected: false,
            pending_tig: false,
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
            notification_sniffer: Default::default(),
            observed_output_bytes: 0,
            last_output_ms: None,
        })
    }
    fn layout(&mut self) -> anyhow::Result<()> {
        chrome::configure_settings(
            &self.settings.terminal,
            unsafe { GetDpiForWindow(self.window) }.max(96),
        );
        for surface in self.detached.keys().copied().collect::<Vec<_>>() {
            self.detached_layout(surface)?;
        }
        self.ssh_auth_layout()?;
        let terminals: Vec<_> = self
            .surfaces
            .iter()
            .filter_map(|(id, terminal)| terminal.ready.then_some(*id))
            .collect();
        for surface in terminals {
            self.refresh_terminal_menu(surface)?;
        }
        if self.main_closed {
            self.agent_bar.take();
            chrome::set_zoom_frame(self.window, None);
            return Ok(());
        }
        let mut client: RECT = unsafe { std::mem::zeroed() };
        unsafe {
            checked(GetClientRect(self.window, &mut client))?;
        }
        let scale = unsafe { GetDpiForWindow(self.window) }.max(96) as f64 / 96.0;
        let px = |value: i32| (value as f64 * scale).round() as i32;
        let sidebar = self.sidebar_width(client.right, unsafe { GetDpiForWindow(self.window) });
        panes::cache_sidebar(sidebar, client.bottom, px(4), !self.background_test);
        let bar = px(28);
        self.agent_bar_sync()?;
        self.files_reconcile();
        self.worktrees_reconcile()?;
        self.sessions_reconcile()?;
        let (mut geometry, content) = self.geometry(self.active_workspace)?;
        let mut zoom_frame = None;
        if let Some(pane) = self.zoomed {
            let inset_x = px(2).min(content.width / 2);
            let inset_y = px(2).min(content.height / 2);
            let inner = model::Rect {
                x: content.x + inset_x,
                y: content.y + inset_y,
                width: (content.width - 2 * px(2)).max(0),
                height: (content.height - 2 * px(2)).max(0),
            };
            zoom_frame = Some((content, inner));
            geometry.panes = vec![(pane, inner)];
            geometry.dividers.clear();
        }
        panes::cursor_dividers(if self.background_test {
            &[]
        } else {
            &geometry.dividers
        });
        if self.main_workspace_indices().is_empty() {
            geometry.panes.clear();
            geometry.dividers.clear();
            zoom_frame = None;
        }
        chrome::set_zoom_frame(self.window, zoom_frame);
        let view_areas = geometry.panes.clone();
        let sessions_width = self.sessions_width((client.right - content.x - px(4)).max(1), scale);
        self.sessions_layout((sessions_width > 0).then_some(model::Rect {
            x: client.right - px(4) - sessions_width,
            y: content.y,
            width: sessions_width,
            height: content.height,
        }));
        let dock_width = self.files_dock_width(
            self.active_workspace,
            (client.right - content.x - px(4) - sessions_width).max(1),
            scale,
        );
        self.files_layout(
            (dock_width > 0).then_some(model::Rect {
                x: client.right - px(4) - sessions_width - dock_width,
                y: content.y,
                width: dock_width,
                height: content.height,
            }),
            scale,
        )?;
        let worktrees_width = self.worktrees_width(
            (client.right - content.x - px(4) - sessions_width - dock_width).max(1),
            scale,
        );
        self.usage_layout(client, sidebar + px(4));
        self.agent_bar_layout(client, sidebar + px(4));
        self.worktrees_layout(
            (worktrees_width > 0).then_some(model::Rect {
                x: client.right - px(4) - sessions_width - dock_width - worktrees_width,
                y: content.y,
                width: worktrees_width,
                height: content.height,
            }),
            scale,
        );
        let areas = &geometry.panes;
        self.refresh_pane_headers(areas);
        let visible: HashMap<_, _> = view_areas
            .iter()
            .filter(|(_, area)| area.width > 0 && area.height > bar)
            .map(|(pane, area)| {
                (
                    self.workspace().root.active_surface_id(*pane).unwrap(),
                    model::Rect {
                        y: area.y + bar,
                        height: area.height - bar,
                        ..*area
                    },
                )
            })
            .collect();
        for (id, surface) in &mut self.surfaces {
            if self.detached.contains_key(id)
                || self
                    .ssh_auth_window
                    .as_ref()
                    .is_some_and(|(hosted, _)| hosted == id)
            {
                continue;
            }
            let show = visible.contains_key(id) && client.right > 0 && client.bottom > 0;
            let area = visible.get(id).filter(|_| show).copied();
            surface.holder.layout(area, self.background_test)?;
            if let Some(area) = area {
                surface
                    .view
                    .set_bounds(bounds(model::Rect { x: 0, y: 0, ..area }))?;
                unsafe {
                    surface
                        .view
                        .controller()
                        .NotifyParentWindowPositionChanged()?;
                }
            }
            // Hiding and re-showing an already visible view can cancel native IME composition.
            if surface.visible != show {
                surface.view.set_visible(show)?;
                surface.visible = show;
                if surface.ready || surface.restoring {
                    surface.send(&HostMessage::Visibility { visible: show })?;
                }
            }
        }
        for (id, editor) in &mut self.editors {
            if self.detached.contains_key(id) {
                continue;
            }
            let area = visible
                .get(id)
                .filter(|_| client.right > 0 && client.bottom > 0)
                .copied();
            editor.view.layout(area)?;
        }
        for (id, browser) in &mut self.browsers {
            if self.detached.contains_key(id) {
                continue;
            }
            let area = visible
                .get(id)
                .filter(|_| client.right > 0 && client.bottom > 0)
                .copied();
            let find_height = self
                .browser_find
                .layout(*id, area, scale, self.background_test);
            browser.layout(area, scale, find_height)?;
        }
        self.browser_bookmarks_layout();
        let sidebar_layout =
            self.sidebar_layout(client.bottom, unsafe { GetDpiForWindow(self.window) });
        let footer_top = sidebar_layout.footer_top;
        let max_offset = sidebar_layout.max_offset;
        self.sidebar_offset = sidebar_layout.offset;
        self.sidebar_metrics = sidebar_layout.metrics;
        self.sidebar_active = self.current_workspace().map(|workspace| workspace.id);
        for control in &self.controls {
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    control.hwnd,
                    i32::from(
                        self.current_workspace().is_some() || Self::empty_action(&control.action),
                    ),
                );
            }
            let rect = match control.action {
                Action::SshStatus(_)
                | Action::SshConnect(_)
                | Action::SshDisconnect(_)
                | Action::SshAuthentication(_)
                | Action::SshPorts(_) => self.ssh_toolbar_rect(&control.action, client.right),
                Action::EmptyState => Some((content.x, content.y, content.width, content.height)),
                Action::NewWorkspace => {
                    (sidebar >= px(36)).then_some((px(4), px(5), px(28), px(28)))
                }
                Action::WorkspaceMenu => (sidebar >= px(100)).then_some((
                    px(36),
                    px(5),
                    sidebar - px(if sidebar >= px(132) { 104 } else { 72 }),
                    px(28),
                )),
                Action::Sessions => {
                    (sidebar >= px(132)).then_some((sidebar - px(64), px(5), px(28), px(28)))
                }
                Action::Notifications => {
                    (sidebar >= px(64)).then_some((sidebar - px(32), px(5), px(28), px(28)))
                }
                Action::Workspace(id) | Action::WorkspaceClose(id) => {
                    let row = sidebar_layout
                        .rows
                        .iter()
                        .find(|(workspace, _, _)| *workspace == id);
                    if let Some(&(_, row_y, row_height)) = row.filter(|_| sidebar > px(12)) {
                        let row_width = sidebar - px(12);
                        let row_height = (row_height - px(2)).max(1);
                        if matches!(control.action, Action::WorkspaceClose(_)) {
                            (row_width >= px(48) && row_height >= px(24)).then_some((
                                sidebar - px(36),
                                row_y + (row_height - px(24)) / 2,
                                px(24),
                                px(24),
                            ))
                        } else {
                            Some((px(6), row_y, row_width.max(1), row_height))
                        }
                    } else {
                        None
                    }
                }
                Action::SidebarScroll(direction) => {
                    if !sidebar_layout.pager
                        || sidebar < px(64)
                        || sidebar_layout.list_bottom + px(28) > footer_top
                    {
                        None
                    } else {
                        unsafe {
                            windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                                control.hwnd,
                                i32::from(if direction < 0 {
                                    self.sidebar_offset > 0
                                } else {
                                    self.sidebar_offset < max_offset
                                }),
                            );
                        }
                        Some((
                            if direction < 0 { px(4) } else { sidebar / 2 },
                            sidebar_layout.list_bottom,
                            (sidebar / 2 - px(6)).max(1),
                            px(24),
                        ))
                    }
                }
                Action::Settings
                | Action::Overview
                | Action::CommandPalette
                | Action::ShowFiles
                | Action::Worktrees
                | Action::Usage
                | Action::AgentBar
                | Action::SearchAll
                | Action::OpenEditor => {
                    let x = match control.action {
                        Action::Settings => px(4),
                        Action::Overview => sidebar - px(132),
                        Action::CommandPalette => px(68),
                        Action::Usage => sidebar - px(164),
                        Action::AgentBar => sidebar - px(196),
                        Action::OpenEditor => px(36),
                        Action::Worktrees => sidebar - px(100),
                        Action::ShowFiles => sidebar - px(68),
                        _ => sidebar - px(36),
                    };
                    (sidebar
                        >= px(match control.action {
                            Action::Usage => 260,
                            Action::AgentBar => 232,
                            Action::CommandPalette => 296,
                            Action::OpenEditor => 260,
                            Action::Overview => 168,
                            Action::Worktrees => 168,
                            _ => 136,
                        })
                        && footer_top >= px(40))
                    .then_some((x, footer_top + px(4), px(28), px(28)))
                }
                Action::Tab(pane, surface)
                | Action::TabClose(pane, surface)
                | Action::PaneZoom(pane, surface)
                | Action::PaneSplitRight(pane, surface)
                | Action::PaneSplitDown(pane, surface)
                | Action::PaneBrowser(pane, surface)
                | Action::PaneAdd(pane, surface)
                | Action::PaneMenu(pane, surface) => areas
                    .iter()
                    .find(|(id, _)| *id == pane)
                    .and_then(|(_, area)| {
                        let header = workspaces::pane_header_layout(*area, unsafe {
                            GetDpiForWindow(self.window)
                        });
                        let slot = match control.action {
                            Action::PaneZoom(..) => Some(0),
                            Action::PaneSplitRight(..) => Some(1),
                            Action::PaneSplitDown(..) => Some(2),
                            Action::PaneAdd(..) => Some(3),
                            Action::PaneBrowser(..) => Some(4),
                            Action::PaneMenu(..) => Some(5),
                            _ => None,
                        };
                        if let Some(slot) = slot {
                            return header.tools[slot].map(|r| (r.x, r.y, r.width, r.height));
                        }
                        let tabs = self
                            .workspace()
                            .leaves()
                            .into_iter()
                            .find(|(id, _, _)| *id == pane)?
                            .2;
                        let at = tabs.iter().position(|tab| tab.id == surface)?;
                        let active = self.workspace().root.active_surface_id(pane)?;
                        let active_index =
                            tabs.iter().position(|tab| tab.id == active).unwrap_or(0);
                        let available = header.tabs_width;
                        let slots = ((available / px(92).max(1)).max(1) as usize).min(tabs.len());
                        let start = active_index.saturating_sub(slots.saturating_sub(1));
                        if at < start || at >= start + slots || available < px(30) {
                            return None;
                        }
                        let width = (available / slots.max(1) as i32).min(px(190));
                        let close = matches!(control.action, Action::TabClose(..));
                        let close_width = if width >= px(72) { px(22) } else { 0 };
                        if close && close_width == 0 {
                            return None;
                        }
                        Some((
                            area.x
                                + px(2)
                                + (at - start) as i32 * width
                                + if close { width - close_width } else { 0 },
                            area.y + px(4),
                            if close {
                                close_width
                            } else {
                                width - close_width
                            },
                            (bar - px(5)).min((area.height - px(5)).max(0)),
                        ))
                    }),
                _ => None,
            };
            let workspace_row = matches!(
                control.action,
                Action::Workspace(_) | Action::WorkspaceClose(_)
            );
            unsafe {
                if let Some((x, y, width, height)) = rect.filter(|(x, y, width, height)| {
                    *x >= 0
                        && (workspace_row || *y >= 0)
                        && *width > 0
                        && *height > 0
                        && x.saturating_add(*width) <= client.right
                        && if workspace_row {
                            y.saturating_add(*height) > sidebar_layout.list_top
                                && *y < sidebar_layout.list_bottom
                        } else {
                            y.saturating_add(*height) <= client.bottom
                        }
                }) {
                    if workspace_row {
                        chrome::clip_workspace_row(
                            control.hwnd,
                            width,
                            height,
                            (sidebar_layout.list_top - y).max(0),
                            (sidebar_layout.list_bottom - y).min(height),
                        )?;
                    }
                    let workspace_close = matches!(control.action, Action::WorkspaceClose(_));
                    SetWindowPos(
                        control.hwnd,
                        if workspace_close {
                            HWND_TOP
                        } else {
                            std::ptr::null_mut()
                        },
                        x,
                        y,
                        width.max(1),
                        height.max(1),
                        SWP_NOACTIVATE
                            | if workspace_close {
                                0
                            } else {
                                SWP_NOZORDER | SWP_SHOWWINDOW
                            },
                    );
                    if workspace_close {
                        chrome::layout_workspace_close(control.hwnd, true);
                    }
                } else {
                    if matches!(control.action, Action::WorkspaceClose(_)) {
                        chrome::layout_workspace_close(control.hwnd, false);
                    } else {
                        ShowWindow(control.hwnd, SW_HIDE);
                    }
                }
            }
        }
        self.refresh_ssh_toolbar();
        self.pane_layout = geometry;
        self.overview_layout()?;
        self.usage.reposition();
        Ok(())
    }
    fn focus_active(&self) -> anyhow::Result<()> {
        if self.background_test
            || self.overview.is_open()
            || self.command_palette.is_open()
            || self.usage.is_open()
            || self.ssh_dialog.is_some()
            || unsafe {
                IsWindowEnabled(
                    self.current_surface()
                        .map_or(self.window, |id| self.surface_window(id)),
                )
            } == 0
        {
            return Ok(());
        }
        let Some(active) = self.current_surface() else {
            if let Some(control) = self
                .controls
                .iter()
                .find(|control| matches!(control.action, Action::NewWorkspace))
            {
                unsafe {
                    SetFocus(control.hwnd);
                }
            }
            return Ok(());
        };
        if self
            .ssh_auth_window
            .as_ref()
            .is_some_and(|(id, _)| *id == active)
        {
            // Main-window relayout must not move focus into a modeless popup.
            return Ok(());
        }
        self.ack_focused_notifications(active);
        if let Some(editor) = self.editors.get(&active) {
            return editor.view.focus();
        }
        if let Some(browser) = self
            .browsers
            .get(&active)
            .filter(|b| !b.native_closed.get())
        {
            let mut info = GUITHREADINFO {
                cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                ..GUITHREADINFO::default()
            };
            let child = browser.view.hwnd().0;
            let focused = unsafe {
                let thread = GetWindowThreadProcessId(self.window, std::ptr::null_mut());
                GetGUIThreadInfo(thread, &mut info) != 0
                    && (info.hwndFocus == child || IsChild(child, info.hwndFocus) != 0)
            };
            if !focused {
                browser.view.focus()?;
            }
        }
        if let Some(surface) = self.surfaces.get(&active) {
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
            Event::Worktrees(signal) => self.worktrees_event(signal)?,
            Event::EmptyWindowShortcut(action) => self.empty_window_shortcut_action(action)?,
            Event::SshDialog(id, action) => self.ssh_dialog_action(id, action)?,
            Event::SshPorts(id, action) => self.ssh_ports_action(id, action)?,
            Event::SshAuthenticationUi(surface, window, action) => {
                if let Some(forward) = self
                    .ssh_forwards
                    .values_mut()
                    .find(|f| f.surface == surface)
                {
                    forward.authentication_ui(window, action)?;
                } else if self
                    .ssh_auth_window
                    .as_ref()
                    .is_some_and(|(id, popup)| *id == surface && popup.id == window)
                {
                    match action {
                        ssh_auth::UiAction::Close => {
                            if self.ssh_auth_window.as_ref().is_some_and(
                                |(_, popup)| unsafe { IsWindowEnabled(popup.window) } != 0,
                            ) {
                                self.close_ssh_auth()?;
                            }
                        }
                        ssh_auth::UiAction::Layout => self.ssh_auth_layout()?,
                    }
                }
            }
            Event::SshForwardUi(id, action) => {
                if let Some(forward) = self.ssh_forwards.values_mut().find(|f| f.surface == id) {
                    if matches!(action, ssh_forward::UiAction::Reconnect) {
                        let workspace = forward.workspace;
                        self.ssh_connect(workspace)?;
                    } else {
                        forward.ui(action)?;
                    }
                }
            }
            Event::Editor(event) => self.editor_event(event)?,
            Event::Files(event) => self.files_event(event)?,
            Event::Browser(event) => self.browser_event(event)?,
            Event::Download(event) => self.download_event(event),
            Event::NotificationUi(action) => self.notification_ui(action)?,
            Event::Sessions(signal) => self.sessions_event(signal)?,
            Event::AgentsReady => self.agents_poll(),
            Event::AgentChanged => {
                self.refresh_chrome_metadata();
                self.sessions_reconcile()?;
                self.layout()?;
            }
            Event::AgentBar(action) => self.agent_bar_ui(action)?,
            Event::UsageUi(action) => self.usage_ui(action)?,
            Event::UsageResult(id, results) => self.usage_complete(id, results)?,
            Event::Activated => {
                if !self.main_closed {
                    self.detached_focus = None;
                    if let Some(surface) = self.current_surface() {
                        self.ack_focused_notifications(surface);
                    }
                }
            }
            Event::Detached(surface, signal) => self.detached_event(surface, signal)?,
            Event::SidebarScroll(scroll) if !self.overview.is_open() => {
                self.scroll_sidebar(scroll)?
            }
            Event::Layout => {
                self.cancel_drag();
                self.layout()?;
            }
            Event::WindowMoved => {
                self.usage.reposition();
                self.browser_bookmarks_layout();
                self.cancel_drag();
                // Child WebViews do not receive Wry's top-level WM_MOVE hook.
                for view in self
                    .surfaces
                    .values()
                    .map(|surface| &surface.view)
                    .chain(
                        self.browsers
                            .values()
                            .filter(|browser| !browser.native_closed.get())
                            .map(|browser| &browser.view),
                    )
                    .chain(self.editors.values().map(|editor| &editor.view.view))
                {
                    unsafe {
                        view.controller().NotifyParentWindowPositionChanged()?;
                    }
                }
            }
            Event::Pointer(pointer) if !self.overview.is_open() => self.pointer(pointer)?,
            Event::Pointer(_) | Event::SidebarScroll(_) => {}
            Event::Close => self.close_main_window()?,
            Event::ExitAfterReply => self.closing = true,
            Event::Saved(result) => self.finish_save(result),
            Event::SearchUi(action) => self.search_ui(action)?,
            Event::CommandPalette(action) => self.command_palette_ui(action)?,
            Event::OptionsUi(action) => self.options_ui(action)?,
            Event::Overview(signal) => self.overview_event(signal)?,
            Event::BrowserFindUi(action) => self.browser_find_ui(action),
            Event::Metadata(id, action) => self.metadata_action(id, action)?,
            Event::TabMenu(id, action) => self.tab_menu_action(id, action)?,
            Event::WorkspaceClose(id, accepted) => self.workspace_close_choice(id, accepted)?,
            Event::ContextMenu(action, x, y) if !self.overview.is_open() => {
                self.cancel_drag();
                self.context_menu(action, x, y)?
            }
            Event::ContextMenu(..) => {}
            Event::Tick => {
                self.agents_tick();
                self.copy_feedback_tick();
                self.usage_tick()?;
                self.sessions_reconcile()?;
                self.worktrees_reconcile()?;
                self.ssh_ports_tick()?;
                #[cfg(debug_assertions)]
                self.pending_terminal_ui_tests.retain(|_, request| {
                    if request.started.elapsed() > Duration::from_secs(2) {
                        let _ = request
                            .reply
                            .try_send(json!({"error":"renderer UI test exceeded two seconds"}));
                        false
                    } else {
                        true
                    }
                });
                self.files_tick();
                self.editor_tick();
                self.browser_tick();
                self.search_tick()?;
                self.overview_tick()?;
                self.pending_selections.retain(|_, request| {
                    if request.started.elapsed() > Duration::from_secs(12) {
                        let _ = request.reply.try_send(
                            json!({"error":"terminal did not answer the selection request"}),
                        );
                        false
                    } else {
                        true
                    }
                });
                self.pending_keys.retain(|_, pending| {
                    if pending.read.started.elapsed() > Duration::from_secs(12) {
                        let _ = pending.read.reply.try_send(
                            json!({"error":"named-key request expired before input was queued"}),
                        );
                        false
                    } else {
                        true
                    }
                });
                self.pending_pastes.retain(|_, request| {
                    if request.started.elapsed() > Duration::from_secs(12) {
                        let _ = request.reply.try_send(
                            json!({"error":"paste request expired before input was queued"}),
                        );
                        false
                    } else {
                        true
                    }
                });
                self.pending_reads.retain(|_, request| {
                    let request = &request.read;
                    if request.started.elapsed() > Duration::from_secs(12) {
                        let _ = request.reply.try_send(
                            json!({"error":"terminal parser did not answer the screen barrier"}),
                        );
                        false
                    } else {
                        true
                    }
                });
                self.pending_minimaps.retain(|_, request| {
                    if request.started.elapsed() > Duration::from_secs(12) {
                        let _ = request.reply.try_send(
                            json!({"error":"terminal did not answer the minimap request"}),
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
                    && !self.close_accepted
                    && self.editors.values().all(editor::Editor::checkpoint_ready)
                    && self.last_save_attempt.elapsed() > Duration::from_secs(30)
                {
                    self.last_save_attempt = Instant::now();
                    if let Err(error) = self.begin_save(None) {
                        self.state_error = Some(error.to_string());
                    }
                }
                // Give due checkpoints first admission. A refresh already in
                // progress defers the checkpoint until the next idle tick.
                if self.files_operation_guard().is_ok() {
                    self.editor_refresh_tick();
                }
            }
            Event::Button(action) => {
                // Recheck queued SSH actions before changing focus: a rebuild or
                // session exit can disable their captured workspace in the meantime.
                if !self.ssh_action_enabled(&action) {
                    return Ok(());
                }
                self.detached_focus = None;
                if !self.command_palette.is_open() {
                    self.action(action)?;
                }
            }
            Event::Bridge(id, origin, body) => self.bridge(id, &origin, &body)?,
            Event::Session(id, generation, message) => {
                if let Some(forward) = self.ssh_forwards.values_mut().find(|f| f.surface == id) {
                    if forward.session_generation() == generation {
                        forward.session_event(message)?;
                        self.ssh_forward_refresh()?;
                    }
                    return Ok(());
                }
                if self
                    .surfaces
                    .get(&id)
                    .is_none_or(|s| s.session_generation != generation)
                {
                    return Ok(());
                }
                let status_changed = !matches!(message, SessionEvent::Output { .. });
                let mut notices = Vec::new();
                if let Some(surface) = self.surfaces.get_mut(&id) {
                    match message {
                        SessionEvent::Output { sequence, bytes } => {
                            anyhow::ensure!(
                                sequence == surface.output_sequence + 1,
                                "out-of-order PTY output"
                            );
                            notices = surface.notification_sniffer.feed(&bytes);
                            surface.observed_output_bytes = surface
                                .observed_output_bytes
                                .saturating_add(bytes.len() as u64);
                            surface.last_output_ms = Some(chrono::Utc::now().timestamp_millis());
                            surface.send(&HostMessage::Output {
                                sequence,
                                data: base64::engine::general_purpose::STANDARD.encode(bytes),
                            })?;
                            surface.output_sequence = sequence;
                        }
                        SessionEvent::OutputEnd => surface.output_ended = true,
                        SessionEvent::Exit(code) => {
                            surface.pending_tig = false;
                            surface.exit_code = Some(code);
                            surface.send(&HostMessage::Exit { code })?;
                        }
                        SessionEvent::Error(message) => {
                            surface.pending_tig = false;
                            report(&format!("terminal {id}: {message}"))
                        }
                    }
                    if surface.output_ended && surface.exit_code.is_some() {
                        // All final bytes have entered the WebView's ordered stream.
                        // Retain its grid and accept parser ACKs after releasing native resources.
                        surface.session.take();
                    }
                }
                if status_changed {
                    self.refresh_ssh_toolbar();
                }
                for notice in notices {
                    self.add_notification(Some(id), notice.title, notice.body, notice.level)?;
                }
            }
            Event::Settings(update) => self.settings_result(update)?,
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
        if let Some(forward) = self.ssh_forwards.values_mut().find(|f| f.surface == id) {
            let before = forward.state().to_owned();
            let result = forward.bridge(origin, body, &self.settings);
            if before != forward.state() {
                self.ssh_forward_refresh()?;
            }
            return result;
        }
        let Some(surface) = self.surfaces.get(&id) else {
            return Ok(());
        };
        let envelope = surface.identity.decode(origin, body)?;
        if !envelope.accepts_session(surface.session_generation) {
            return Ok(());
        }
        match envelope.message {
            ClientMessage::Ready => {
                surface.send(&HostMessage::Visibility {
                    visible: surface.visible,
                })?;
                if surface.ready || surface.restoring {
                    return Ok(());
                }
                surface.send(&HostMessage::Settings {
                    document: Box::new(self.settings.clone()),
                    colors: Box::new(crate::theme::resolve(&self.settings.terminal)),
                    bindings: crate::keybindings::resolved(&self.settings.keybindings)?,
                })?;
                if let Some(screen) = self
                    .restore_screens
                    .remove(&id)
                    .filter(|_| self.settings.terminal.restore_terminal_scrollback)
                {
                    surface.send(&HostMessage::Restore { screen })?;
                    self.surfaces.get_mut(&id).unwrap().restoring = true;
                } else {
                    self.start_session(id)?;
                }
                self.refresh_terminal_menu(id)?;
            }
            ClientMessage::Restored => {
                anyhow::ensure!(
                    surface.restoring && !surface.ready,
                    "unexpected history restore acknowledgement"
                );
                self.start_session(id)?;
            }
            ClientMessage::RetryCommandPrompt => {
                self.retry_shell(id, Some(crate::shell::Shell::profile("cmd")))?
            }
            ClientMessage::SettingsApplied {
                revision,
                rendered_font_size,
                cursor_animation_duration_ms,
                terminal,
                background,
                foreground,
                bindings,
                colors,
            } => {
                if revision == self.settings.revision
                    && rendered_font_size == self.settings.terminal.rendered_font_size()
                    && *terminal == self.settings.terminal
                    && *colors == crate::theme::resolve(&self.settings.terminal)
                    && background == colors.background
                    && foreground == colors.foreground
                    && bindings == crate::keybindings::resolved(&self.settings.keybindings)?
                {
                    self.surfaces.get_mut(&id).unwrap().applied_settings = Some(
                        json!({"revision":revision,"rendered_font_size":rendered_font_size,"cursor_animation_duration_ms":cursor_animation_duration_ms,"terminal":terminal,"background":background,"foreground":foreground,"colors":colors,"bindings":bindings}),
                    );
                }
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
            ClientMessage::KeyMode {
                request,
                sequence,
                outcome,
            } => self.key_mode(id, request, sequence, outcome)?,
            ClientMessage::Pasted {
                request,
                sequence,
                outcome,
            } => self.pasted(id, request, sequence, outcome)?,
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
                    if sequence > surface.session_after {
                        session.acknowledge(sequence)?;
                    }
                }
                surface.acknowledged_sequence = sequence;
            }
            ClientMessage::SshConnect => {
                let (index, _, _) = self.locate(id).context("SSH terminal no longer exists")?;
                self.ssh_connect(self.workspaces[index].id)?;
            }
            ClientMessage::SshReady { session } => {
                if self.remote_directory(id).is_some() {
                    let surface = self.surfaces.get_mut(&id).unwrap();
                    if surface.session_generation == session
                        && surface.session.is_some()
                        && surface.exit_code.is_none()
                    {
                        let newly_connected = !surface.ssh_connected;
                        surface.ssh_connected = true;
                        surface.send(&HostMessage::SshStatus {
                            state: "connected".into(),
                            error: None,
                        })?;
                        if newly_connected
                            && self
                                .ssh_auth_window
                                .as_ref()
                                .is_some_and(|(hosted, popup)| {
                                    *hosted == id && unsafe { IsWindowEnabled(popup.window) } != 0
                                })
                        {
                            self.close_ssh_auth()?;
                        }
                        self.flush_pending_tig(id)?;
                        self.refresh_ssh_toolbar();
                    }
                }
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
                        self.workspace_mut().refresh_name();
                        self.refresh_chrome_metadata();
                        self.ack_focused_notifications(id);
                    }
                }
            }
            ClientMessage::Shortcut {
                action,
                chord,
                revision,
            } => self.shortcut(id, &action, &chord, revision)?,
            ClientMessage::TerminalMenuAction { pane, action } => {
                self.terminal_menu_action(id, PaneId(pane), action)?;
            }
            #[cfg(debug_assertions)]
            ClientMessage::TerminalMenuTested { request, state } => {
                if let Some(pending) = self.pending_terminal_ui_tests.get(&request) {
                    anyhow::ensure!(
                        self.background_test && pending.surface == id,
                        "invalid terminal menu test acknowledgement"
                    );
                    let pending = self.pending_terminal_ui_tests.remove(&request).unwrap();
                    let _ = pending
                        .reply
                        .try_send(json!({"surface":id,"request":request,"state":state}));
                }
            }
            #[cfg(debug_assertions)]
            ClientMessage::ShortcutTested { request, forwarded } => {
                self.shortcut_tested(id, request, forwarded)?;
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
                    self.refresh_surface_metadata(id);
                }
            }
            ClientMessage::Cwd { path } => {
                if surface.ready && !surface.restoring {
                    if self.remote_directory(id).is_some() {
                        self.update_remote_directory(id, path);
                    } else if let Ok(path) = crate::cwd::local_path(&path) {
                        self.update_cwd(id, path);
                    }
                }
            }
            ClientMessage::Selected {
                request,
                sequence,
                result,
            } => {
                if let Some(pending) = self.pending_selections.get(&request) {
                    anyhow::ensure!(
                        pending.surface == id
                            && sequence >= pending.after
                            && sequence <= surface.output_sequence,
                        "invalid selection response"
                    );
                    result.validate()?;
                    let pending = self.pending_selections.remove(&request).unwrap();
                    let _ = pending
                        .reply
                        .try_send(json!({"surface":id,"sequence":sequence,"result":result}));
                }
            }
            ClientMessage::Minimap {
                request,
                sequence,
                outcome,
            } => {
                if let Some(pending) = self.pending_minimaps.get(&request) {
                    anyhow::ensure!(
                        pending.surface == id
                            && sequence >= pending.after
                            && sequence <= surface.output_sequence,
                        "invalid minimap response"
                    );
                    outcome.validate()?;
                    let pending = self.pending_minimaps.remove(&request).unwrap();
                    let value = match outcome {
                        crate::minimap::Outcome::Ok { snapshot } => {
                            json!({"surface":id,"sequence":sequence,"result":snapshot})
                        }
                        crate::minimap::Outcome::Error { message } => json!({"error":message}),
                    };
                    let _ = pending.reply.try_send(value);
                }
            }
            ClientMessage::Screen {
                request,
                sequence,
                outcome,
            } => {
                if let Some(pending) = self.pending_reads.get(&request) {
                    anyhow::ensure!(
                        pending.read.surface == id
                            && sequence >= pending.read.after
                            && sequence <= surface.output_sequence,
                        "invalid screen response"
                    );
                    outcome.validate(pending.mode)?;
                    let pending = self.pending_reads.remove(&request).unwrap();
                    let value = match outcome {
                        crate::screen::Outcome::Ok { snapshot } => {
                            json!({"surface":id,"sequence":sequence,
                            "text":snapshot.text,"screen":snapshot.metadata})
                        }
                        crate::screen::Outcome::Error { message } => json!({"error":message}),
                    };
                    let _ = pending.read.reply.try_send(value);
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
                self.open_browser(id, url, false)?;
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
                                            if let SurfaceKind::SshTerminal { cwd, .. } = &tab.kind
                                            {
                                                crate::ssh::set_remote_cwd(
                                                    &mut workspace.root,
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
                                            workspace.refresh_name();
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
        let remote = self.remote_directory(id).is_some();
        let workspace_id = self.workspaces[workspace].id;
        if remote && self.ssh_disconnected.contains(&workspace_id) {
            let surface = self.surfaces.get_mut(&id).unwrap();
            surface.ready = true;
            surface.restoring = false;
            surface.send(&HostMessage::SshStatus {
                state: "disconnected".into(),
                error: None,
            })?;
            self.refresh_ssh_toolbar();
            return Ok(());
        }
        if let Some(shell) = self.ssh_shell(id)? {
            self.shells.insert(id, shell);
        }
        let generation = Uuid::new_v4();
        let mut shell = self
            .startup_shells
            .remove(&id)
            .unwrap_or_else(|| self.shells[&id].clone());
        if remote {
            let command = shell.args.last_mut().context("SSH bootstrap is missing")?;
            *command = format!("printf '\\033]777;flowmux-ssh-ready;{generation}\\007'; {command}");
        }
        let sender = self.sender.clone();
        anyhow::ensure!(
            self.surfaces[&id].session.is_none(),
            "terminal session is already running"
        );
        self.cancel_surface_search(id);
        self.cancel_terminal_requests(id);
        let surface = self.surfaces.get_mut(&id).unwrap();
        surface.session_generation = generation;
        surface.session_after = surface.output_sequence;
        surface.ssh_connected = false;
        surface.exit_code = None;
        surface.output_ended = false;
        surface.notification_sniffer = Default::default();
        surface.send(&HostMessage::SessionStart {
            session: generation,
        })?;
        // Once launch is attempted, reconnect must not silently recreate a
        // vanished tmux session, including after a local spawn failure.
        if remote {
            self.ssh_attempted.insert(id);
        }
        let session = Session::spawn_after(
            &cwd,
            &shell,
            pane,
            id,
            workspace_id,
            &self._ipc.name,
            surface.cols,
            surface.rows,
            surface.output_sequence,
            move |event| sender.send(Event::Session(id, generation, event)),
        );
        let session = match session {
            Ok(session) => session,
            Err(error) => {
                let error = format!("{error:#}");
                report(&error);
                let surface = self.surfaces.get_mut(&id).unwrap();
                surface.ready = true;
                surface.restoring = false;
                surface.startup_error = Some(error.clone());
                surface.pending_tig = false;
                surface.send(&if remote {
                    HostMessage::SshStatus {
                        state: "failed".into(),
                        error: Some(error),
                    }
                } else {
                    HostMessage::ShellStatus { error: Some(error) }
                })?;
                self.refresh_ssh_toolbar();
                return Ok(());
            }
        };
        let surface = self.surfaces.get_mut(&id).unwrap();
        surface.startup_error = None;
        surface.send(&if remote {
            HostMessage::SshStatus {
                state: "connecting".into(),
                error: None,
            }
        } else {
            HostMessage::ShellStatus { error: None }
        })?;
        surface.process_pid = Some(session.pid);
        surface.session = Some(session);
        surface.ready = true;
        surface.restoring = false;
        self.flush_pending_tig(id)?;
        self.refresh_ssh_toolbar();
        if self.current_surface() == Some(id) {
            self.focus_active()?;
        }
        Ok(())
    }
    fn begin_save(&mut self, reply: Option<ipc::Reply>) -> anyhow::Result<()> {
        if self.editor_guard(editor::Operation::Checkpoint(reply.clone()), None)? {
            return Ok(());
        }
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
        let detached_windows = self
            .detached
            .iter()
            .map(|(id, window)| Ok((*id, window.placement()?)))
            .collect::<anyhow::Result<_>>()?;
        let request = Uuid::new_v4();
        let mut waiting = HashMap::new();
        if self.settings.terminal.restore_terminal_scrollback {
            for (id, surface) in &self.surfaces {
                let after = surface
                    .session
                    .as_ref()
                    .map_or(surface.output_sequence, Session::barrier);
                surface.send(&HostMessage::Snapshot { request, after })?;
                waiting.insert(*id, after);
            }
        }
        self.pending_save = Some(PendingSave {
            id: request,
            state: WindowState {
                version: 1,
                window: store.id,
                workspaces: self.workspaces.clone(),
                active_workspace: self
                    .workspaces
                    .get(self.active_workspace)
                    .map(|workspace| workspace.id),
                screens: HashMap::new(),
                shells: self.shells.clone(),
                sidebar_width_dip: self.sidebar_width_dip,
                detached_windows,
                main_closed: self.main_closed,
                detached_focus: self.detached_focus,
            },
            waiting,
            started: Instant::now(),
            writing: false,
            closing: self.close_request.is_some(),
            reply,
        });
        self.last_save_attempt = Instant::now();
        self.write_save()
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
    fn accept_close(&mut self) {
        // Keep the IPC reply alive until its transport drains, while preventing
        // late preparations from publishing a new tab into an accepted close.
        self.close_accepted = true;
        self.files_shutdown();
        self.editor_cancel_opens(
            None,
            "window close was accepted before editor Open completed",
        );
    }
    fn request_close(&mut self, request: CloseRequest) -> anyhow::Result<()> {
        self.files_operation_guard()?;
        let copy = match &request {
            CloseRequest::Native => CloseRequest::Native,
            CloseRequest::Ipc(reply) => CloseRequest::Ipc(reply.clone()),
        };
        if self.editor_guard(editor::Operation::Window(copy), None)? {
            return Ok(());
        }
        anyhow::ensure!(self.close_request.is_none(), "window is already closing");
        if self.store.is_none() {
            self.accept_close();
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
                Ok(()) => {
                    self.accept_close();
                    match &self.close_request {
                        Some(CloseRequest::Native) => self.closing = true,
                        Some(CloseRequest::Ipc(reply)) => {
                            // A slow save can outlive the IPC caller's deadline. Its
                            // successful close request must not leave the UI frozen.
                            self.closing |= reply.try_send(response).is_err();
                        }
                        None => {}
                    }
                }
                Err(error) => self.close_failed(&error),
            }
        } else if self.close_request.is_some() {
            // A checkpoint captured an older layout. Closing always captures again
            // after structural changes have stopped, even if that checkpoint failed.
            // The close barrier already synchronized and sealed editor models.
            // A second editor barrier would reject that still-active seal.
            self.editor_bypass = true;
            let result = self.begin_save(None);
            self.editor_bypass = false;
            if let Err(error) = result {
                self.close_failed(&error.to_string());
            }
        }
    }
    fn close_failed(&mut self, error: &str) {
        self.editor_release_all();
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
        if self.remote_directory(id).is_some() {
            return;
        }
        if let Some((workspace, pane, _)) = self.locate(id) {
            let first_report = self.surfaces.get(&id).is_some_and(|s| !s.cwd_reported);
            let root = &mut self.workspaces[workspace].root;
            root.set_surface_cwd(pane, id, cwd.clone());
            if first_report
                && matches!(
                    root.surface_title(pane, id),
                    Some("PowerShell" | "Windows PowerShell" | "cmd" | "pwsh" | "powershell")
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
            self.refresh_surface_metadata(id);
            self.refresh_chrome_metadata();
        }
    }
    fn refresh_surface_metadata(&mut self, id: SurfaceId) {
        if let Some((workspace, _, _)) = self.locate(id) {
            if self.workspaces[workspace].refresh_name() {
                self.refresh_chrome_metadata();
            }
        }
        self.refresh_tab_title(id);
        if self.agent_presence(id).is_some() {
            post(Event::Layout);
        }
    }
    fn refresh_tab_title(&self, id: SurfaceId) {
        if let Some((workspace, pane, _)) = self.locate(id) {
            let root = &self.workspaces[workspace].root;
            if let Some(title) = root.surface_title(pane, id) {
                if let Some(window) = self.detached.get(&id) {
                    window.caption(title, self.surface_icon(id));
                }
                let mut label = title.to_owned();
                let unread = self
                    .notifications
                    .store
                    .entries()
                    .iter()
                    .filter(|entry| !entry.read && entry.surface == Some(id))
                    .count();
                if unread > 0 {
                    label = format!("[{unread}] {label}");
                }
                for control in &self.controls {
                    if matches!(control.action, Action::Tab(_, surface) if surface == id) {
                        workspaces::set_caption(control.hwnd, &label);
                        chrome::set_role(control.hwnd, self.chrome_role(&control.action));
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
            let id = caller
                .or_else(|| self.current_surface())
                .context("No active workspace; create a workspace first")?;
            anyhow::ensure!(
                self.locate(id).is_some(),
                "calling surface no longer exists"
            );
            Ok(id)
        }
    }
    fn select(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        if self.detached.contains_key(&id) {
            self.detached_focus = Some(id);
            if !self.background_test {
                unsafe {
                    SetForegroundWindow(self.detached[&id].window);
                }
            }
            return Ok(());
        }
        self.detached_focus = None;
        let (workspace, pane, _) = self.locate(id).context("surface not found")?;
        if workspace != self.active_workspace || self.zoomed.is_some_and(|zoomed| zoomed != pane) {
            self.zoomed = None;
        }
        self.active_workspace = workspace;
        self.workspace_mut().focused = pane;
        self.workspace_mut().root.set_active_surface(pane, id);
        self.workspace_mut().refresh_name();
        self.refresh_chrome_metadata();
        Ok(())
    }
    fn move_tab(&mut self, surface: SurfaceId, target: PaneId, index: usize) -> anyhow::Result<()> {
        let destination = self
            .workspaces
            .iter()
            .find(|workspace| workspace.root.find_leaf_content(target).is_some())
            .context("destination pane not found")?;
        anyhow::ensure!(
            !self.is_detached_workspace(destination.id),
            "cannot add tabs to a separate window"
        );
        let mut candidate = self.workspaces.clone();
        let active = model::move_surface(&mut candidate, surface, target, index)?;
        self.tab_menu_surface_closing(surface);
        if self.detached.contains_key(&surface) {
            self.surface_holder(surface)?.reparent(self.window)?;
            if let Some(browser) = self.browsers.get_mut(&surface) {
                browser.parent_changed();
            }
            let window = self.detached.remove(&surface).unwrap();
            self.metadata_owner_closing(window.window);
            self.download_owner_closing(window.window);
            // The inline find controls move with the retained browser holder.
            drop(window);
        }
        self.workspaces = candidate;
        self.active_workspace = active;
        self.detached_focus = None;
        self.zoomed = None;
        // No surface is removed or recreated: its WebView, parser, IME target and
        // native process stay attached to the same identity throughout the move.
        self.rebuild()
    }
    fn move_menu(&mut self) -> anyhow::Result<()> {
        let surface = self.target(None, None)?;
        let (workspace, pane, _) = self.locate(surface).context("Tab no longer exists")?;
        let tabs = self.workspaces[workspace]
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
        self.agent_states.get_mut().remove(&surface);
        self.tab_menu_surface_closing(surface);
        let owner = self.surface_window(surface);
        if owner != self.window {
            self.metadata_owner_closing(owner);
            self.download_owner_closing(owner);
        }
        self.editor_remove(surface);
        self.browser_cancel(surface, "browser closed during script request");
        self.browsers.remove(&surface);
        self.shells.remove(&surface);
        self.startup_shells.remove(&surface);
        self.ssh_attempted.remove(&surface);
        self.surfaces.remove(&surface);
        if self
            .ssh_auth_window
            .as_ref()
            .is_some_and(|(id, _)| *id == surface)
        {
            self.ssh_auth_window.take();
        }
        self.detached.remove(&surface);
        if self
            .pending_save
            .as_ref()
            .is_some_and(|s| !s.writing && s.waiting.contains_key(&surface))
        {
            self.finish_save(Err(
                "terminal closed during checkpoint; previous save preserved".into(),
            ));
        }
        self.cancel_terminal_requests(surface);
    }
    pub(super) fn cancel_terminal_requests(&mut self, surface: SurfaceId) {
        self.pending_selections.retain(|_, request| {
            if request.surface == surface {
                let _ = request
                    .reply
                    .try_send(json!({"error":"terminal closed during selection request"}));
                false
            } else {
                true
            }
        });
        self.pending_reads.retain(|_, request| {
            let request = &request.read;
            if request.surface == surface {
                let _ = request
                    .reply
                    .try_send(json!({"error":"terminal closed during screen read"}));
                false
            } else {
                true
            }
        });
        self.pending_minimaps.retain(|_, request| {
            if request.surface == surface {
                let _ = request
                    .reply
                    .try_send(json!({"error":"terminal closed during minimap request"}));
                false
            } else {
                true
            }
        });
        self.pending_keys.retain(|_, pending| {
            if pending.read.surface == surface {
                let _ = pending
                    .read
                    .reply
                    .try_send(json!({"error":"terminal closed before named key was queued"}));
                false
            } else {
                true
            }
        });
        self.pending_pastes.retain(|_, request| {
            if request.surface == surface {
                let _ = request
                    .reply
                    .try_send(json!({"error":"terminal closed before paste was queued"}));
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
    fn new_window(&mut self, source: Option<SurfaceId>) -> anyhow::Result<()> {
        let owner = source.map_or(self.window, |id| self.surface_window(id));
        anyhow::ensure!(
            self.pending_save.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && !self.closing
                && self.editor_barrier.is_none()
                && !self.overview.is_open()
                && unsafe { IsWindowEnabled(owner) } != 0,
            "Window is busy"
        );
        // locate returns a local working directory even for an SSH surface.
        let directory = match source {
            Some(id) => {
                self.locate(id)
                    .context("New window source no longer exists")?
                    .2
            }
            None => self.initial_cwd.clone(),
        };
        let _pid = super::entry::new_window(&directory, self.store.is_none())?;
        #[cfg(debug_assertions)]
        {
            self.last_new_window_pid = Some(_pid);
        }
        Ok(())
    }

    fn action(&mut self, action: Action) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.overview.is_open() || matches!(action, Action::Overview),
            "workspace overview is open"
        );
        anyhow::ensure!(
            self.editor_bypass || self.editor_barrier.is_none(),
            "editor synchronization is in progress"
        );
        anyhow::ensure!(
            !self.close_accepted && self.close_request.is_none(),
            "window is saving before close"
        );
        anyhow::ensure!(
            self.current_surface().is_some() || Self::empty_action(&action),
            "No active workspace; create a workspace first"
        );
        match action {
            Action::EmptyState => return Ok(()),
            Action::OpenEditor => return self.editor_pick_action(),
            Action::ShowFiles => return self.files_show_current(),
            Action::Worktrees => return self.toggle_worktrees(),
            Action::NewBrowser => return self.new_browser_tab(self.active()),
            Action::Notifications => return self.notification_ui(notifications::UiAction::Show),
            Action::Usage => return self.toggle_usage(),
            Action::AgentBar => {
                return self.settings_submit(
                    crate::command::SettingsOp::Set {
                        key: crate::settings::SettingKey::AgentBarMode,
                        value: (!self.settings.terminal.agent_bar_mode).to_string(),
                        expected: Some(self.settings.terminal.agent_bar_mode.to_string()),
                    },
                    None,
                    None,
                )
            }
            Action::Sessions => return self.toggle_sessions(),
            Action::Settings => return self.settings_menu(),
            Action::CommandPalette => {
                return self.command_palette_ui(command_palette::UiAction::Show)
            }
            Action::Overview => return self.overview_toggle(),
            Action::NewWorkspace => {
                return self.new_workspace(None, None, None).map(|_| ());
            }
            Action::NewSshWorkspace => return self.show_ssh_dialog(),
            Action::NewWindow => return self.new_window(self.current_surface()),
            Action::SshStatus(_) => return Ok(()),
            Action::SshPorts(id) => return self.show_ssh_ports(id),
            Action::SshConnect(id) => return self.ssh_connect(id),
            Action::SshDisconnect(id) => return self.ssh_disconnect(id),
            Action::SshAuthentication(id) => return self.ssh_authentication(id),
            Action::Workspace(id) => {
                let index = self.workspace_index(id)?;
                if self.is_detached_workspace(id) {
                    self.select(self.workspaces[index].active())?;
                    return self.focus_active();
                }
                if self.active_workspace != index {
                    self.zoomed = None;
                }
                self.active_workspace = index;
                self.detached_focus = None;
            }
            Action::WorkspaceClose(id) => {
                if self.main_closed
                    || self.is_detached_workspace(id)
                    || !self.workspaces.iter().any(|workspace| workspace.id == id)
                {
                    return Ok(());
                }
                return self.confirm_close_workspaces(vec![id]);
            }
            Action::WorkspaceMenu => return self.workspace_creation_menu(None),
            Action::NewTab => {
                return self
                    .new_terminal(self.active(), None, None, shells::NewTerminal::Tab)
                    .map(|_| ());
            }
            Action::Vertical => {
                return self
                    .new_terminal(
                        self.active(),
                        None,
                        None,
                        shells::NewTerminal::Split(SplitDirection::Vertical),
                    )
                    .map(|_| ());
            }
            Action::Horizontal => {
                return self
                    .new_terminal(
                        self.active(),
                        None,
                        None,
                        shells::NewTerminal::Split(SplitDirection::Horizontal),
                    )
                    .map(|_| ());
            }
            Action::CloseTab => {
                if self.detached.contains_key(&self.active()) {
                    return self.close_detached(self.active());
                }
                if self.editor_guard(editor::Operation::Tab(self.active()), None)? {
                    return Ok(());
                }
                let surface = self
                    .workspace_mut()
                    .close_active()
                    .context("cannot close the final tab")?;
                self.zoomed = None;
                self.remove_surface(surface);
            }
            Action::MoveTabMenu => return self.move_menu(),
            Action::DetachTab => return self.detach_tab(self.active()),
            Action::SearchAll => return self.search_ui(search::UiAction::Show),
            Action::TogglePaneZoom => return self.toggle_zoom(self.active()),
            Action::Find => {
                // The native button explicitly opens the current terminal's find bar.
                // Background test hosts never request desktop or DOM focus.
                if self.surfaces.get(&self.active()).is_some_and(|s| s.ready) {
                    if !self.background_test {
                        self.focus_active()?;
                    }
                    self.surfaces[&self.active()].send(&HostMessage::OpenFind {
                        focus: !self.background_test,
                    })?;
                }
                return Ok(());
            }
            Action::SidebarScroll(delta) => {
                return self.scroll_sidebar(workspaces::Scroll::Page(delta))
            }
            Action::PaneAdd(pane, surface) => {
                anyhow::ensure!(
                    self.locate(surface)
                        .is_some_and(|(_, current, _)| current == pane),
                    "Pane source changed"
                );
                return self
                    .new_terminal(surface, None, None, shells::NewTerminal::Tab)
                    .map(|_| ());
            }
            Action::PaneMenu(pane, surface) => return self.pane_actions_menu(pane, surface),
            Action::PaneZoom(pane, surface)
            | Action::PaneSplitRight(pane, surface)
            | Action::PaneSplitDown(pane, surface)
            | Action::PaneBrowser(pane, surface) => {
                anyhow::ensure!(
                    self.locate(surface)
                        .is_some_and(|(_, current, _)| current == pane),
                    "Pane source changed"
                );
                self.select(surface)?;
                return self.action(match action {
                    Action::PaneZoom(..) => Action::TogglePaneZoom,
                    Action::PaneSplitRight(..) => Action::Vertical,
                    Action::PaneSplitDown(..) => Action::Horizontal,
                    _ => Action::NewBrowser,
                });
            }
            Action::TabClose(pane, surface) => {
                anyhow::ensure!(
                    self.locate(surface)
                        .is_some_and(|(_, current, _)| current == pane),
                    "Tab moved before close"
                );
                self.select(surface)?;
                return self.action(Action::CloseTab);
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
            (!self.close_accepted && self.close_request.is_none())
                || matches!(
                    command,
                    Command::Tree
                        | Command::Identify
                        | Command::Capabilities
                        | Command::ReadScreen { .. }
                        | Command::Files {
                            op: crate::files_model::Op::Status(_)
                        }
                        | Command::Quit {
                            discard_state: true
                        }
                ),
            "window is saving before close"
        );
        anyhow::ensure!(
            self.editor_barrier.is_none()
                || matches!(
                    &command,
                    Command::Tree
                        | Command::Identify
                        | Command::Capabilities
                        | Command::Editor {
                            op: crate::editor::Op::Status(_)
                        }
                        | Command::Files {
                            op: crate::files_model::Op::Status(_)
                        }
                ),
            "editor synchronization is in progress"
        );
        match command {
            Command::Agents => {
                self.agents_request(None, reply)?;
                return Ok(None);
            }
            Command::ReportAgent(args) => return self.agent_report_request(args, caller, reply),
            Command::Hooks { mut op } => {
                let args = op.runtime_mut().context("Hook configuration is a local CLI operation")?.clone();
                return self.agent_hook_request(args, caller, reply);
            }
            Command::Editor { op } => return self.editor_command(op, caller, reply),
            Command::Files { op } => return self.files_command(op, reply),
            Command::Browser { op } => return self.browser_command(op, caller, reply),
            Command::LaunchWindow { context } => {
                let caller = caller.context("Window launch requires a calling terminal")?;
                let source = self
                    .surfaces
                    .get(&caller)
                    .context("Calling terminal no longer exists")?;
                anyhow::ensure!(source.session.is_some(), "Calling terminal is not running");
                // The GUI is outside its terminal jobs. Keep those jobs strict;
                // the new window must not be owned by the invoking shell's job.
                let pid = super::entry::launch_gui(&context)
                    .map_err(|error| anyhow::anyhow!("{error:#}"))?;
                return Ok(Some(json!({"spawned_pid":pid})));
            }
            Command::Shells => {
                return Ok(Some(
                    json!({"profiles":super::shell::profiles(),"default_shell":self.settings.default_shell}),
                ))
            }
            Command::RetryShell { surface, shell } => {
                let id = surface.map(SurfaceId).unwrap_or(self.target(None, caller)?);
                self.retry_shell(id, shell.requested()?)?;
                return Ok(Some(
                    json!({"ok":self.surfaces[&id].startup_error.is_none(),"surface":id,"startup_error":self.surfaces[&id].startup_error}),
                ));
            }
            Command::Settings { op } => {
                if matches!(
                    op,
                    crate::command::SettingsOp::Keybindings {
                        op: crate::keybindings::Op::Show
                    }
                ) {
                    return Ok(Some(
                        json!({"revision":self.settings.revision,"scope":"terminal_editor_and_browser","catalog":crate::keybindings::catalog(&self.settings.keybindings)?,"bindings":crate::keybindings::resolved(&self.settings.keybindings)?}),
                    ));
                }
                if matches!(op, crate::command::SettingsOp::Show) {
                    return Ok(Some(self.settings_status()));
                }
                self.settings_submit(op, Some(reply), None)?;
                return Ok(None);
            }
            Command::Doctor | Command::ShellIntegration => {
                anyhow::bail!("this is a local CLI operation")
            }
            Command::Identify => {
                let surface = self.target(None, caller)?;
                let (workspace, pane, cwd) = self.locate(surface).unwrap();
                return Ok(Some(json!({"pid":std::process::id(),"pipe":self._ipc.name,
                "workspace":self.workspaces[workspace].id,"pane":pane,"surface":surface,"cwd":self.remote_directory(surface).map_or_else(||json!(cwd), |remote|json!(remote)),"shell":self.shells.get(&surface),"platform":"windows"})));
            }
            Command::Capabilities => {
                return Ok(Some(json!({"platform":"windows","status":"development",
                "ssh_workspace":true,"ssh_backend":"OpenSSH","ssh_multiplexing":false,"ssh_port_forwarding":false,
                "terminal_backend":"ConPTY/xterm.js","webview_runtime":"WebView2","browser_automation":false,"browser_automation_status":"partial","browser_commands":["open","navigate","back","forward","reload","stop","url","title","status","zoom","eval","snapshot","text","value","attr","is-visible","is-enabled","is-checked","count","wait","click","dblclick","hover","focus","blur","scroll","fill","select","check","uncheck","screenshot","find","find-show","find-close"],"browser_wait_limits":{"timeout_ms":120000,"poll_ms_max":10000,"pending":8},
                "named_key_protocol":"send_key_mode","detached_surfaces":["terminal","browser","editor"],"detached_window_restore":true,
                "editor_status":"partial","editor_commands":["open","pick","status","command","check-disk","flush"],
                "editor_open_limits":{"pending":crate::editor_open::MAX_PENDING,"budget_ms":crate::editor_open::OPEN_BUDGET.as_millis()},
                "files_status":"partial","files_commands":["show","status","expand","collapse","select","more","refresh","open","hide"],
                "files_limits":{"pending":crate::files_service::MAX_ADMITTED,"budget_ms":crate::files_service::BUDGET.as_millis(),"page_rows":crate::files_model::PAGE_SIZE,"entries":crate::files_model::MAX_ENTRIES,"expanded":crate::files_model::MAX_EXPANDED},
                "agent_activity":{"ordered_reports":true,"native_hooks":false,"session_hooks":["session-start","session-end"],"native_activity_hooks":{"codex":["turn-start","running","notification","subagent-start","subagent-stop","stop","interrupt"],"claude":["turn-start","tool-start","tool-end","permission-request","tool-batch","notification","subagent-start","subagent-stop","stop","stop-failure"]},"process_discovery":true},
                "commands":["agents","report-agent","hooks","files","editor","browser","downloads","identify","capabilities","tree","read-screen","capture-pane","minimap","notify","notify-complete","notifications","send-keys","send-key","split","new-tab",
                    "new-workspace","focus-pane","focus-tab","close-tab","move-tab","detach-tab","save-state","quit","shell-integration","find",
                    "search-all","search-results","search-cancel","search-open","resize-pane","focus-direction","toggle-pane-zoom","workspace","rename-tab","settings","shells","retry-shell","paste","selection"],
                "acceptance":"Release validation is incomplete; physical Korean IME behavior remains unverified"})))
            }
            #[cfg(debug_assertions)]
            Command::TestShortcut { surface, event } => {
                self.test_shortcut(SurfaceId(surface), &event, reply)?;
                return Ok(None);
            }
            #[cfg(debug_assertions)]
            Command::TestTerminalMenu { surface, event } => {
                self.test_terminal_menu(SurfaceId(surface), &event, reply)?;
                return Ok(None);
            }
            #[cfg(debug_assertions)]
            Command::TestUsage { input } => return self.test_usage(&input).map(Some),
            #[cfg(debug_assertions)]
            Command::ChromeCapture {
                path,
                usage_bar,
                agent_bar,
            } => {
                anyhow::ensure!(
                    self.background_test,
                    "chrome capture requires an owned hidden debug host"
                );
                if usage_bar {
                    let window = self
                        .usage
                        .capture_bar_window()
                        .context("usage bar is not available")?;
                    return chrome::capture_subtree(window, &path).map(Some);
                }
                if agent_bar {
                    let window = self
                        .agent_bar
                        .as_ref()
                        .context("agents bar is not available")?
                        .window;
                    return chrome::capture_subtree(window, &path).map(Some);
                }
                return if let Some(window) = self
                    .tab_menu
                    .as_ref()
                    .map(tab_menu::Menu::capture_window)
                    .or_else(|| self.ssh_dialog.as_ref().map(|panel| panel.window))
                    .or_else(|| self.ssh_auth_window.as_ref().map(|(_, popup)| popup.window))
                    .or_else(|| self.ssh_ports.as_ref().map(|panel| panel.window))
                    .or_else(|| {
                        self.options
                            .as_ref()
                            .and_then(appearance::Panel::capture_window)
                    })
                    .or_else(|| self.browser_bookmarks.capture_window())
                    .or_else(|| self.overview_capture_window())
                    .or_else(|| self.usage.capture_window())
                    .or_else(|| self.sessions.capture_window())
                    .or_else(|| self.command_palette.capture_window())
                    .or_else(|| self.notifications.capture_window())
                {
                    chrome::capture_subtree(window, &path)
                } else {
                    chrome::capture(self.window, &path)
                }
                .map(Some);
            }
            Command::Tree => {
                #[cfg(debug_assertions)]
                let last_new_window_pid = self.last_new_window_pid;
                #[cfg(not(debug_assertions))]
                let last_new_window_pid = None::<u32>;
                let surfaces: Vec<_> = self.surfaces.iter().map(|(id, surface)| json!({"id":id,"ready":surface.ready,
                    "pid":surface.process_pid,"session":surface.session_generation,"running":surface.session.is_some() && surface.exit_code.is_none(),
                    "exit_code":surface.exit_code,"resources_released":surface.ready && surface.session.is_none(),
                    "agent":self.agent_presence(*id),
                    "output_sequence":surface.output_sequence,"parsed_sequence":surface.acknowledged_sequence,
                    "observed_output_bytes":surface.observed_output_bytes,"last_output_ms":surface.last_output_ms,
                    "cols":surface.cols,"rows":surface.rows,"cwd_reported":surface.cwd_reported,
                    "visible":surface.visible,"pending_tig":surface.pending_tig,
                    "settings":surface.applied_settings,
                    "shell":self.shells[id],"startup_error":surface.startup_error,
                    "bounds":surface.holder.view_bounds(&surface.view),"holder":surface.holder.diagnostics(),
                    "view_handle":surface.view.hwnd().0 as usize,
                    "cwd":self.locate(*id).map(|(_,_,cwd)|cwd),"remote_cwd":self.remote_directory(*id)})).collect();
                let mut result = json!({"workspaces":self.workspaces,"active_workspace":self.workspaces.get(self.active_workspace).map(|workspace|workspace.id),"main_empty":self.current_workspace().is_none(),"surfaces":surfaces,
                        "last_new_window_pid":last_new_window_pid,"worktrees":self.worktrees.status(),"usage":self.usage.status(),"agent_sessions":self.sessions.status(),
                        "browsers":self.browsers.iter().map(|(id,b)|b.status(*id)).collect::<Vec<_>>(),
                        "editors":self.editors.iter().map(|(id,e)|e.status(*id)).collect::<Vec<_>>(),
                        "editor_open_pending":self.editor_open_pending.len(),
                        "editor_synchronizing":self.editor_barrier.is_some(),
                        "editor_close_dialog":self.editor_close_diagnostics(),
                        "editor_open_admitted":self.editor_preparer.as_ref().map_or(0, |worker| worker.pending()),
                        "editor_picker_pending":self.editor_picker_pending,
                        "close_accepted":self.close_accepted,
                        "popup":self.browser_popup_status(),
                        "search_dialog":self.search.diagnostics(),
                        "command_palette":self.command_palette.diagnostics(),
                        "metadata":self.metadata.as_ref().map(workspaces::Panel::diagnostics),
                        "tab_menu":self.tab_menu.as_ref().map(tab_menu::Menu::diagnostics),
                        "copy_feedback":self.copy_feedback.as_ref().map(tab_menu::CopyFeedback::diagnostics),
                        "workspace_close_dialog":self.workspace_close.as_ref().map(|close|close.panel.diagnostics()),
                        "ssh_dialog":self.ssh_dialog.as_ref().map(ssh_panel::Panel::diagnostics),
                        "ssh_ports":self.ssh_ports.as_ref().map(ssh_ports_panel::Panel::status),
                        "ssh_authentication":self.ssh_auth_status(),
                        "ssh_forwards":self.ssh_forwards.values().map(ssh_forward::Forward::status).collect::<Vec<_>>(),
                        "ssh_toolbar":self.ssh_toolbar_status(),
                        "overview":self.overview_status(),
                        "zoomed_pane":self.zoomed,"layout":self.pane_layout,"chrome":self.chrome_status(),
                        "background_testing":self.background_test,"window_handle":self.window as usize,
                        "main_closed":self.main_closed,
                        "detached_windows":self.detached.iter().map(|(id,window)| {let mut value=window.diagnostics(); value["surface"]=json!(id); value}).collect::<Vec<_>>(),
                        "state":{"window":self.store.as_ref().map(|s| s.id),"path":self.store.as_ref().map(|s| &s.path),
                            "saving":self.pending_save.is_some(),"error":self.state_error}});
                result["agent_bar"] = self
                    .agent_bar
                    .as_ref()
                    .filter(|bar| !bar.activity)
                    .map_or(Value::Null, agent_bar::Bar::diagnostics);
                result["activity_panel"] = self
                    .agent_bar
                    .as_ref()
                    .filter(|bar| bar.activity)
                    .map_or(Value::Null, agent_bar::Bar::diagnostics);
                result["bookmarks"] = self.browser_bookmarks.diagnostics();
                return Ok(Some(result));
            }
            Command::ReadScreen {
                pane,
                surface,
                recent,
            } => {
                anyhow::ensure!(
                    self.pending_reads.len() < 16,
                    "too many pending screen reads"
                );
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
                self.surfaces[&id].send(&HostMessage::ReadScreen {
                    request,
                    after,
                    recent,
                })?;
                self.pending_reads.insert(
                    request,
                    PendingScreen {
                        read: PendingRead {
                            surface: id,
                            after,
                            reply,
                            started: Instant::now(),
                        },
                        mode: crate::screen::Mode::from_recent(recent),
                    },
                );
                return Ok(None);
            }
            Command::Minimap { surface, action } => {
                anyhow::ensure!(
                    self.pending_minimaps.len() < 16,
                    "too many pending minimap requests"
                );
                let id = self.target(None, surface.map(SurfaceId).or(caller))?;
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
                surface.send(&HostMessage::Minimap {
                    request,
                    after,
                    action,
                })?;
                self.pending_minimaps.insert(
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
            Command::Notify(crate::command::NotifyArgs {
                title,
                level,
                body,
                pane,
                surface,
                global,
            }) => {
                anyhow::ensure!(
                    !(global && (pane.is_some() || surface.is_some()))
                        && !(pane.is_some() && surface.is_some()),
                    "choose one notification target"
                );
                let source = if global {
                    None
                } else {
                    Some(self.target(pane, surface.map(SurfaceId).or(caller))?)
                };
                return Ok(Some(self.add_notification(
                    source,
                    title,
                    body,
                    crate::notifications::level(&level)?,
                )?));
            }
            Command::NotifyComplete(crate::command::NotifyCompleteArgs {
                agent,
                message,
                pane,
                surface,
            }) => {
                anyhow::ensure!(
                    !agent.trim().is_empty()
                        && agent.len() <= 256
                        && !agent.chars().any(char::is_control),
                    "agent name must be one nonempty line of at most 256 UTF-8 bytes"
                );
                anyhow::ensure!(
                    pane.is_none() || surface.is_none(),
                    "choose either pane or surface"
                );
                let source = self.target(pane, surface.map(SurfaceId).or(caller))?;
                return Ok(Some(self.add_notification(
                    Some(source),
                    format!("{agent} is ready"),
                    message,
                    flowmux_core::NotificationLevel::TurnCompleted,
                )?));
            }
            Command::Downloads { op } => return Ok(Some(self.download_command(op)?)),
            Command::Notifications { op } => return self.notification_command(op).map(Some),
            Command::Find(crate::command::FindArgs {
                query,
                surface,
                previous,
                match_case,
                regex,
                close,
            }) => {
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
            Command::Selection { surface, action } => {
                let id = self.target(None, surface.map(SurfaceId).or(caller))?;
                let surface = self
                    .surfaces
                    .get(&id)
                    .context("terminal surface not found")?;
                anyhow::ensure!(surface.ready && !surface.restoring, "terminal is not ready");
                anyhow::ensure!(
                    self.pending_selections.len() < 128,
                    "too many selection requests"
                );
                let after = surface
                    .session
                    .as_ref()
                    .map_or(surface.output_sequence, Session::barrier);
                let request = Uuid::new_v4();
                surface.send(&HostMessage::Selection {
                    request,
                    after,
                    action,
                })?;
                self.pending_selections.insert(
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
            Command::Paste {
                text,
                pane,
                surface,
            } => {
                anyhow::ensure!(
                    pane.is_none() || surface.is_none(),
                    "choose either pane or surface"
                );
                let id = self.target(pane, surface.map(SurfaceId).or(caller))?;
                self.begin_paste(id, text, reply)?;
                return Ok(None);
            }
            Command::SendKeys { pane, text } => self
                .session(self.target(Some(pane), caller)?)?
                .input(text.into_bytes())?,
            Command::SendKey { key, pane, surface } => {
                anyhow::ensure!(
                    pane.is_none() || surface.is_none(),
                    "choose either pane or surface"
                );
                let id = self.target(pane, surface.map(SurfaceId).or(caller))?;
                self.begin_key(id, key, reply)?;
                return Ok(None);
            }
            Command::Split { direction, shell } => {
                self.new_terminal(
                    self.target(None, caller)?,
                    None,
                    shell.requested()?,
                    shells::NewTerminal::Split(match direction {
                        Direction::Vertical => SplitDirection::Vertical,
                        Direction::Horizontal => SplitDirection::Horizontal,
                    }),
                )?;
            }
            Command::NewTab { cwd, shell } => {
                self.new_terminal(
                    self.target(None, caller)?,
                    cwd,
                    shell.requested()?,
                    shells::NewTerminal::Tab,
                )?;
            }
            Command::NewWorkspace { cwd, shell } => {
                self.new_workspace(caller, cwd, shell.requested()?)?;
            }
            Command::Workspace { op } => {
                if let WorkspaceOp::Close { workspace } = &op {
                    if self.editor_guard(
                        editor::Operation::Workspace(WorkspaceId(*workspace)),
                        Some(reply.clone()),
                    )? {
                        return Ok(None);
                    }
                }
                return self.workspace_command(op, caller).map(Some);
            }
            Command::RenameTab { surface, name } => {
                self.rename_tab(SurfaceId(surface), name)?;
            }
            Command::FocusPane { pane } => {
                let id = self.target(Some(pane), caller)?;
                self.select(id)?;
                self.rebuild()?;
            }
            Command::ClosePane { pane } => {
                if self.close_pane(PaneId(pane), Some(reply.clone()))? {
                    return Ok(None);
                }
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
                if matches!(command, Command::CloseTab { .. })
                    && self.editor_guard(editor::Operation::Tab(id), Some(reply.clone()))?
                {
                    return Ok(None);
                }
                self.select(id)?;
                if matches!(command, Command::CloseTab { .. }) {
                    self.action(Action::CloseTab)?;
                } else {
                    self.rebuild()?;
                }
            }
            Command::DetachTab { surface } => {
                let id = SurfaceId(surface);
                self.detach_tab(id)?;
                return Ok(Some(
                    json!({"ok":true,"surface":id,"window_handle":self.detached[&id].window as usize}),
                ));
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
            // After a successful save, the pipe worker attempts the reply and
            // waits for peer closure within a deadline before requesting exit.
            Command::Quit {
                discard_state: true,
            } => {
                if self.editor_guard(editor::Operation::QuitDiscard(reply.clone()), None)? {
                    return Ok(None);
                }
                // Explicit state recovery escape hatch; dirty editor buffers remain protected.
                self.accept_close();
                self.closing |= reply.try_send(json!({"ok":true})).is_err();
                return Ok(None);
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
