// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent native window for one retained surface; the App owns its lifecycle.
use super::*;
use std::cell::Cell;

const TAB: usize = 100;
const CLOSE: usize = 101;
const WORKSPACE: usize = 200;
const WORKSPACE_CLOSE: usize = 201;
#[derive(Clone, Copy)]
struct Route {
    surface: SurfaceId,
    background: bool,
    gutter: Option<model::Rect>,
}
thread_local! {
    static ROUTES: RefCell<HashMap<isize, Route>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Copy)]
pub(super) enum Signal {
    Layout,
    Moved,
    Activated,
    RenameTab,
    ContextMenu(i32, i32),
    Close,
    Pointer(u32, i32, i32),
}

fn emit(window: HWND, signal: Signal) {
    let surface = ROUTES.with(|routes| {
        routes
            .borrow()
            .get(&(window as isize))
            .map(|route| route.surface)
    });
    if let Some(surface) = surface {
        post(Event::Detached(surface, signal));
    }
}

pub(super) struct Window {
    pub window: HWND,
    pub workspace: WorkspaceId,
    surface: SurfaceId,
    tab: HWND,
    close: HWND,
    tools: [HWND; 6],
    sidebar: [HWND; 9],
    workspace_row: HWND,
    workspace_close: HWND,
    sidebar_width: Cell<u32>,
    drag: Cell<Option<(i32, i32, bool)>>,
    background: bool,
    // (maximize on first show, hidden-test restore). Hidden windows never apply
    // maximization, but subsequent checkpoints must retain the saved intent.
    restored_show: std::cell::Cell<Option<(bool, bool)>>,
}

impl Window {
    pub(super) fn new(
        surface: SurfaceId,
        workspace: WorkspaceId,
        title: &str,
        sidebar_width_dip: u32,
        background: bool,
    ) -> anyhow::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux.windows.detached");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register detached surface window"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide(title).as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1280,
                800,
                std::ptr::null_mut(), // Independent: main-window destruction must not own this HWND.
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create detached surface window");
            let mut result = Self {
                window,
                workspace,
                surface,
                tab: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                tools: [std::ptr::null_mut(); 6],
                sidebar: [std::ptr::null_mut(); 9],
                workspace_row: std::ptr::null_mut(),
                workspace_close: std::ptr::null_mut(),
                sidebar_width: Cell::new(sidebar_width_dip.clamp(
                    crate::state::MIN_SIDEBAR_WIDTH,
                    crate::state::MAX_SIDEBAR_WIDTH,
                )),
                drag: Cell::new(None),
                background,
                restored_show: std::cell::Cell::new(None),
            };
            ROUTES.with(|routes| {
                routes.borrow_mut().insert(
                    window as isize,
                    Route {
                        surface,
                        background,
                        gutter: None,
                    },
                )
            });
            result.workspace_row = result.button(
                title,
                WORKSPACE,
                chrome::Role::Workspace {
                    tree: false,
                    selected: true,
                    color: None,
                    unread: false,
                },
                true,
            )?;
            result.workspace_close =
                result.button("Close workspace", WORKSPACE_CLOSE, chrome::Role::Tool, true)?;
            for (index, (label, icon)) in [
                ("New workspace", None),
                ("Workspaces", None),
                ("Notifications", Some(chrome::ChromeIcon::Notifications)),
                ("Settings", Some(chrome::ChromeIcon::Settings)),
                ("Command Palette", Some(chrome::ChromeIcon::CommandPalette)),
                ("Workspace overview", Some(chrome::ChromeIcon::Overview)),
                ("Open file", Some(chrome::ChromeIcon::OpenFile)),
                ("Files", Some(chrome::ChromeIcon::Files)),
                ("Search", Some(chrome::ChromeIcon::Search)),
            ]
            .into_iter()
            .enumerate()
            {
                let role = icon.map_or(
                    if index == 0 {
                        chrome::Role::Tool
                    } else {
                        chrome::Role::Button
                    },
                    |kind| chrome::Role::Icon {
                        kind,
                        marked: false,
                    },
                );
                // Linux's torn-off sidebar has no workspace-action receiver.
                // Keep unavailable entry points visibly disabled here as well.
                result.sidebar[index] = result.button(label, 210 + index, role, false)?;
            }
            result.tab = result.button(
                title,
                TAB,
                chrome::Role::Tab {
                    selected: true,
                    multiple: false,
                    kind: chrome::SurfaceIcon::Terminal,
                },
                true,
            )?;
            result.close = result.button(
                "Close tab",
                CLOSE,
                chrome::Role::TabClose {
                    selected: true,
                    multiple: false,
                },
                true,
            )?;
            for (index, (label, role)) in [
                (
                    "Maximize pane",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::Maximize,
                        marked: false,
                    },
                ),
                (
                    "Split right",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::SplitRight,
                        marked: false,
                    },
                ),
                (
                    "Split down",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::SplitDown,
                        marked: false,
                    },
                ),
                ("+", chrome::Role::Tool),
                (
                    "New browser",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::Browser,
                        marked: false,
                    },
                ),
                (
                    "Pane actions",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::More,
                        marked: false,
                    },
                ),
            ]
            .into_iter()
            .enumerate()
            {
                result.tools[index] = result.button(label, CLOSE + 1 + index, role, false)?;
            }
            result.layout()?;
            Ok(result)
        }
    }

    fn button(
        &self,
        label: &str,
        id: usize,
        role: chrome::Role,
        enabled: bool,
    ) -> anyhow::Result<HWND> {
        let window = unsafe {
            CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide(label.replace('&', "&&")).as_ptr(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | BS_OWNERDRAW as u32
                    | if id == TAB { BS_NOTIFY as u32 } else { 0 }
                    | if enabled { 0 } else { WS_DISABLED },
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
        anyhow::ensure!(!window.is_null(), "cannot create detached surface header");
        chrome::register_button(window, role);
        Ok(window)
    }

    pub(super) fn area(&self) -> anyhow::Result<model::Rect> {
        let mut rect = RECT::default();
        unsafe {
            checked(GetClientRect(self.window, &mut rect))?;
        }
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let bar = (28.0 * dpi as f64 / 96.0).round() as i32;
        let gutter = self.gutter()?;
        let x = gutter.x + gutter.width;
        Ok(model::Rect {
            x,
            y: bar.min(rect.bottom),
            width: (rect.right - x).max(1),
            height: (rect.bottom - bar).max(1),
        })
    }

    pub(super) fn layout(&self) -> anyhow::Result<()> {
        let mut client = RECT::default();
        unsafe {
            checked(GetClientRect(self.window, &mut client))?;
        }
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let px = |value: i32| (value as f64 * dpi as f64 / 96.0).round() as i32;
        let gutter = self.gutter()?;
        ROUTES.with(|routes| {
            if let Some(route) = routes.borrow_mut().get_mut(&(self.window as isize)) {
                route.gutter = Some(gutter);
            }
        });
        let sidebar = gutter.x;
        let footer = (client.bottom - px(36)).max(0);
        let row_top = px(40);
        let row_height = (footer - row_top).clamp(0, px(58));
        let controls = [
            (
                self.sidebar[0],
                model::Rect {
                    x: px(4),
                    y: px(5),
                    width: px(28),
                    height: px(28),
                },
            ),
            (
                self.sidebar[1],
                model::Rect {
                    x: px(36),
                    y: px(5),
                    width: sidebar - px(72),
                    height: px(28),
                },
            ),
            (
                self.sidebar[2],
                model::Rect {
                    x: sidebar - px(32),
                    y: px(5),
                    width: px(28),
                    height: px(28),
                },
            ),
            (
                self.workspace_row,
                model::Rect {
                    x: px(6),
                    y: row_top,
                    width: sidebar - px(40),
                    height: row_height,
                },
            ),
            (
                self.workspace_close,
                model::Rect {
                    x: sidebar - px(32),
                    y: row_top + (row_height - px(22)).max(0) / 2,
                    width: px(22),
                    height: px(22).min(row_height),
                },
            ),
        ];
        for (window, rect) in controls {
            place(
                window,
                (rect.x >= 0 && rect.x + rect.width <= sidebar && rect.y + rect.height <= footer)
                    .then_some(rect),
            )?;
        }
        for (index, window) in self.sidebar[3..].iter().enumerate() {
            let x = if index < 3 {
                px(4 + index as i32 * 32)
            } else {
                sidebar - px((6 - index) as i32 * 32 + 4)
            };
            let rect = model::Rect {
                x,
                y: footer + px(4),
                width: px(28),
                height: px(28),
            };
            place(
                *window,
                (sidebar >= px(200)
                    && rect.x + rect.width <= sidebar
                    && rect.y + rect.height <= client.bottom)
                    .then_some(rect),
            )?;
        }
        let area = model::Rect {
            x: gutter.x + gutter.width,
            y: 0,
            width: (client.right - gutter.x - gutter.width).max(1),
            height: client.bottom.min(px(28)),
        };
        let header = workspaces::pane_header_layout(area, dpi);
        chrome::set_pane_headers(self.window, vec![(area, false)]);
        let width = header.tabs_width.min(px(190));
        let close_width = if width >= px(72) { px(22) } else { 0 };
        for (index, window) in self.tools.iter().enumerate() {
            place(*window, header.tools[index])?;
        }
        place(
            self.tab,
            (width > close_width).then_some(model::Rect {
                x: area.x + px(2),
                y: px(4),
                width: width - close_width,
                height: (area.height - px(5)).max(0),
            }),
        )?;
        place(
            self.close,
            (close_width > 0).then_some(model::Rect {
                x: area.x + px(2) + width - close_width,
                y: px(4),
                width: close_width,
                height: (area.height - px(5)).max(0),
            }),
        )?;
        Ok(())
    }

    fn gutter(&self) -> anyhow::Result<model::Rect> {
        let mut client = RECT::default();
        unsafe {
            checked(GetClientRect(self.window, &mut client))?;
        }
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96) as f64;
        let px = |dip: u32| (dip as f64 * dpi / 96.0).round() as i32;
        let width = px(self.sidebar_width.get()).min((client.right - px(320) - px(4)).max(0));
        Ok(model::Rect {
            x: width,
            y: 0,
            width: px(4).min(client.right.max(0)),
            height: client.bottom,
        })
    }

    pub(super) fn sidebar_width_dip(&self) -> u32 {
        self.sidebar_width.get()
    }

    pub(super) fn workspace_caption(&self, title: &str, role: chrome::Role) {
        workspaces::set_caption(self.workspace_row, title);
        chrome::set_role(self.workspace_row, role);
    }

    pub(super) fn cancel_drag(&self) -> bool {
        let active = self.drag.take().is_some();
        if !self.background {
            unsafe {
                if GetCapture() == self.window {
                    ReleaseCapture();
                }
            }
        }
        active
    }

    pub(super) fn pointer(&self, message: u32, x: i32, y: i32) -> anyhow::Result<()> {
        match message {
            WM_LBUTTONDOWN => {
                self.cancel_drag();
                let gutter = self.gutter()?;
                if gutter.contains(x, y) {
                    self.drag.set(Some((x - gutter.x, x, false)));
                    if !self.background {
                        unsafe {
                            SetCapture(self.window);
                        }
                    }
                }
            }
            WM_MOUSEMOVE | WM_LBUTTONUP => {
                if let Some((offset, start, moved)) = self.drag.get() {
                    if moved || x != start {
                        self.drag.set(Some((offset, start, true)));
                        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96) as f64;
                        let width = (((x - offset) as f64 * 96.0 / dpi).round().max(0.0) as u32)
                            .clamp(
                                crate::state::MIN_SIDEBAR_WIDTH,
                                crate::state::MAX_SIDEBAR_WIDTH,
                            );
                        if self.sidebar_width.replace(width) != width {
                            emit(self.window, Signal::Layout);
                        }
                    }
                }
                if message == WM_LBUTTONUP {
                    self.cancel_drag();
                }
            }
            _ => {
                self.cancel_drag();
            }
        }
        Ok(())
    }

    pub(super) fn caption(&self, title: &str, kind: chrome::SurfaceIcon) {
        unsafe {
            SetWindowTextW(self.window, wide(title).as_ptr());
        }
        workspaces::set_caption(self.tab, title);
        chrome::set_role(
            self.tab,
            chrome::Role::Tab {
                selected: true,
                multiple: false,
                kind,
            },
        );
    }

    pub(super) fn placement(&self) -> anyhow::Result<crate::state::SavedPlacement> {
        let mut native = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        unsafe {
            checked(GetWindowPlacement(self.window, &mut native))?;
        }
        let rect = native.rcNormalPosition;
        let width = u32::try_from(i64::from(rect.right) - i64::from(rect.left))?;
        let height = u32::try_from(i64::from(rect.bottom) - i64::from(rect.top))?;
        anyhow::ensure!(width > 0 && height > 0, "invalid detached window placement");
        let maximized = match self.restored_show.get() {
            Some((maximized, background))
                if background || unsafe { IsWindowVisible(self.window) == 0 } =>
            {
                maximized
            }
            _ => {
                native.showCmd == SW_SHOWMAXIMIZED as u32
                    || (unsafe { IsIconic(self.window) != 0 }
                        && native.flags & WPF_RESTORETOMAXIMIZED != 0)
            }
        };
        Ok(crate::state::SavedPlacement {
            left: rect.left,
            top: rect.top,
            width,
            height,
            maximized,
            sidebar_width_dip: Some(self.sidebar_width.get()),
        })
    }

    pub(super) fn restore_placement(
        &self,
        saved: &crate::state::SavedPlacement,
        background: bool,
    ) -> anyhow::Result<()> {
        saved.validate()?;
        if let Some(width) = saved.sidebar_width_dip {
            self.sidebar_width.set(width);
        }
        anyhow::ensure!(
            saved.width > 0 && saved.height > 0,
            "invalid saved window size"
        );
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96) as i32;
        let width = i32::try_from(saved.width)?.max(420 * dpi / 96);
        let height = i32::try_from(saved.height)?.max(240 * dpi / 96);
        let right = saved
            .left
            .checked_add(width)
            .context("saved window right edge overflow")?;
        let bottom = saved
            .top
            .checked_add(height)
            .context("saved window bottom edge overflow")?;
        let native = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            showCmd: SW_HIDE as u32,
            ptMinPosition: POINT { x: -1, y: -1 },
            ptMaxPosition: POINT { x: -1, y: -1 },
            rcNormalPosition: RECT {
                left: saved.left,
                top: saved.top,
                right,
                bottom,
            },
            ..Default::default()
        };
        // Normal placement uses workspace coordinates. SetWindowPlacement also
        // corrects a saved position made offscreen by changed monitor geometry.
        // SW_HIDE applies geometry without displaying or activating the frame.
        unsafe {
            checked(SetWindowPlacement(self.window, &native))?;
        }
        self.restored_show.set(Some((saved.maximized, background)));
        Ok(())
    }

    pub(super) fn show(&self, background: bool) {
        unsafe {
            let command = if background {
                SW_HIDE
            } else {
                let maximized = self
                    .restored_show
                    .take()
                    .map_or_else(|| IsZoomed(self.window) != 0, |(maximized, _)| maximized);
                if maximized {
                    SW_SHOWMAXIMIZED
                } else {
                    SW_SHOWNOACTIVATE
                }
            };
            ShowWindow(self.window, command);
        }
    }

    pub(super) fn diagnostics(&self) -> Value {
        json!({"window_handle":self.window as usize,"workspace":self.workspace,"surface":self.surface,
            "owner":unsafe{GetWindow(self.window,GW_OWNER)} as usize,
            "native_visible":unsafe{IsWindowVisible(self.window)}!=0,"area":self.area().ok(),
            "placement":self.placement().ok(),
            "tab":self.tab as usize,"close":self.close as usize,
            "sidebar":{"width_dip":self.sidebar_width.get(),"width":self.gutter().ok().map(|r|r.x),
                "gutter":self.gutter().ok(),"dragging":self.drag.get().is_some(),
                "header":self.sidebar[1] as usize,"workspace_row":self.workspace_row as usize,
                "workspace_close":self.workspace_close as usize},
            "tools":self.tools.map(|window| window as usize)})
    }
}

fn place(window: HWND, rect: Option<model::Rect>) -> anyhow::Result<()> {
    unsafe {
        if let Some(rect) = rect.filter(|rect| rect.width > 0 && rect.height > 0) {
            checked(SetWindowPos(
                window,
                std::ptr::null_mut(),
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            ))?;
        } else {
            ShowWindow(window, SW_HIDE);
        }
    }
    Ok(())
}

impl Drop for Window {
    fn drop(&mut self) {
        self.cancel_drag();
        ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
        for window in [
            self.tab,
            self.close,
            self.workspace_row,
            self.workspace_close,
        ]
        .into_iter()
        .chain(self.tools)
        .chain(self.sidebar)
        {
            if !window.is_null() {
                chrome::unregister(window);
            }
        }
        // Caller drops/reparents the surface's WebView and holder before this.
        unsafe {
            DestroyWindow(self.window);
        }
    }
}

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(result) = chrome::message(window, message, w, l) {
        return result;
    }
    match message {
        WM_CONTEXTMENU
            if w == GetDlgItem(window, TAB as i32) as usize
                || w == GetDlgItem(window, CLOSE as i32) as usize =>
        {
            if IsWindowEnabled(window) != 0 {
                let mut x = l as u16 as i16 as i32;
                let mut y = (l >> 16) as u16 as i16 as i32;
                if x == -1 && y == -1 {
                    let mut rect = RECT::default();
                    GetWindowRect(w as HWND, &mut rect);
                    x = rect.left;
                    y = rect.bottom;
                }
                emit(window, Signal::ContextMenu(x, y));
            }
            0
        }
        WM_CLOSE => {
            emit(window, Signal::Close);
            0
        }
        WM_SIZE => {
            emit(window, Signal::Pointer(WM_CANCELMODE, 0, 0));
            emit(window, Signal::Layout);
            0
        }
        WM_MOVE => {
            emit(window, Signal::Pointer(WM_CANCELMODE, 0, 0));
            emit(window, Signal::Moved);
            0
        }
        WM_ACTIVATE => {
            if w as u16 != WA_INACTIVE as u16 {
                emit(window, Signal::Activated);
            } else {
                emit(window, Signal::Pointer(WM_CANCELMODE, 0, 0));
            }
            0
        }
        WM_DPICHANGED => {
            emit(window, Signal::Pointer(WM_CANCELMODE, 0, 0));
            if l != 0 {
                let rect = &*(l as *const RECT);
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            emit(window, Signal::Layout);
            0
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(l as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(window).max(96) as i32;
            info.ptMinTrackSize.x = 420 * dpi / 96;
            info.ptMinTrackSize.y = 240 * dpi / 96;
            0
        }
        WM_COMMAND => {
            if l != 0
                && IsWindowEnabled(window) != 0
                && IsWindowEnabled(l as HWND) != 0
                && GetParent(l as HWND) == window
                && GetDlgCtrlID(l as HWND) == (w & 0xffff) as i32
            {
                match ((w >> 16) as u32, w & 0xffff) {
                    (BN_CLICKED, CLOSE | WORKSPACE_CLOSE) => emit(window, Signal::Close),
                    (BN_CLICKED, TAB | WORKSPACE) => emit(window, Signal::Activated),
                    (BN_DOUBLECLICKED, TAB) => emit(window, Signal::RenameTab),
                    _ => {}
                }
            }
            0
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut paint);
            let mut rect = RECT::default();
            GetClientRect(window, &mut rect);
            let brush = CreateSolidBrush(chrome::palette().background);
            FillRect(dc, &rect, brush);
            DeleteObject(brush);
            EndPaint(window, &paint);
            0
        }
        WM_LBUTTONDOWN | WM_MOUSEMOVE | WM_LBUTTONUP => {
            emit(
                window,
                Signal::Pointer(
                    message,
                    l as u16 as i16 as i32,
                    (l >> 16) as u16 as i16 as i32,
                ),
            );
            0
        }
        WM_CANCELMODE | WM_CAPTURECHANGED => {
            emit(window, Signal::Pointer(WM_CANCELMODE, 0, 0));
            DefWindowProcW(window, message, w, l)
        }
        WM_KEYDOWN if w == 27 => {
            emit(window, Signal::Pointer(WM_CANCELMODE, 0, 0));
            0
        }
        WM_SETCURSOR if l as u16 == HTCLIENT as u16 => {
            let route = ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied());
            if let Some(Route {
                background: false,
                gutter: Some(gutter),
                ..
            }) = route
            {
                let mut point = POINT::default();
                if GetCursorPos(&mut point) != 0
                    && ScreenToClient(window, &mut point) != 0
                    && gutter.contains(point.x, point.y)
                {
                    SetCursor(LoadCursorW(std::ptr::null_mut(), IDC_SIZEWE));
                    return 1;
                }
            }
            DefWindowProcW(window, message, w, l)
        }
        WM_NCDESTROY => {
            ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
            DefWindowProcW(window, message, w, l)
        }
        // WM_DESTROY deliberately does not post WM_QUIT: this window owns one surface.
        _ => DefWindowProcW(window, message, w, l),
    }
}
