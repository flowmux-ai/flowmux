// SPDX-License-Identifier: GPL-3.0-or-later
//! Native right-hand worktree dock; repository operations belong to the host.
use super::*;
use windows_sys::Win32::System::SystemServices::{
    SS_ENDELLIPSIS, SS_NOPREFIX, SS_NOTIFY, SS_PATHELLIPSIS,
};
use windows_sys::Win32::UI::{
    Controls::SetScrollInfo,
    Input::KeyboardAndMouse::{EnableWindow, GetKeyState},
};

const MAX_ROWS: usize = 256;
const ROW_HEIGHT: i32 = 146;
#[derive(Clone, Debug)]
pub(super) enum UiAction {
    Refresh,
    Close,
    Select(PathBuf),
    Navigate(usize),
    Info(PathBuf),
    Remove(PathBuf),
    Layout,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub(super) info: crate::worktrees::Info,
    pub(super) remove_block_reason: Option<String>,
}
#[derive(Clone, Copy)]
struct Route {
    id: Uuid,
    viewport: HWND,
    scroll: i32,
    limit: i32,
    step: i32,
    open: bool,
    busy: bool,
    selection: Option<model::Rect>,
}
thread_local! {
    static ROUTES: RefCell<HashMap<isize, Route>> = RefCell::new(HashMap::new());
    static ACTIONS: RefCell<HashMap<isize, (HWND, UiAction)>> = RefCell::new(HashMap::new());
}
fn owner(window: HWND) -> HWND {
    if ROUTES.with(|routes| routes.borrow().contains_key(&(window as isize))) {
        window
    } else {
        unsafe { GetParent(window) }
    }
}
fn emit(window: HWND, action: UiAction) {
    let route = ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied());
    if let Some(route) = route {
        if !route.open || (route.busy && matches!(action, UiAction::Refresh | UiAction::Remove(_)))
        {
            return;
        }
        post(Event::Worktrees(worktrees::Signal::Ui(route.id, action)));
    }
}
fn scroll(window: HWND, next: impl FnOnce(Route) -> i32) {
    let changed = ROUTES.with(|routes| {
        let mut routes = routes.borrow_mut();
        let Some(route) = routes.get_mut(&(window as isize)) else {
            return false;
        };
        let position = next(*route).clamp(0, route.limit);
        let changed = route.scroll != position;
        route.scroll = position;
        changed
    });
    if changed {
        emit(window, UiAction::Layout);
    }
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let root = owner(window);
    match message {
        WM_ERASEBKGND | WM_PRINTCLIENT if window != root => {
            chrome::message(window, WM_ERASEBKGND, w, l);
            if let Some(rect) = ROUTES.with(|routes| {
                routes
                    .borrow()
                    .get(&(root as isize))
                    .and_then(|route| route.selection)
            }) {
                let color = SetDCBrushColor(w as HDC, chrome::palette().accent);
                FrameRect(
                    w as HDC,
                    &RECT {
                        left: rect.x,
                        top: rect.y,
                        right: rect.x + rect.width,
                        bottom: rect.y + rect.height,
                    },
                    GetStockObject(DC_BRUSH),
                );
                SetDCBrushColor(w as HDC, color);
            }
            return 1;
        }
        WM_COMMAND if l != 0 => {
            let child = l as HWND;
            if GetParent(child) != window
                || IsWindowEnabled(root) == 0
                || IsWindowEnabled(GetParent(root)) == 0
                || IsWindowEnabled(child) == 0
            {
                return 0;
            }
            if (w >> 16) as u32 == BN_CLICKED {
                let action =
                    ACTIONS.with(|actions| actions.borrow().get(&(child as isize)).cloned());
                if let Some((expected, action)) = action.filter(|(expected, _)| *expected == root) {
                    emit(expected, action);
                }
            } else if (w >> 16) as u32 == BN_SETFOCUS && window != root {
                let mut item = RECT::default();
                let mut viewport = RECT::default();
                if GetWindowRect(child, &mut item) != 0 && GetWindowRect(window, &mut viewport) != 0
                {
                    scroll(root, |route| {
                        if item.top < viewport.top {
                            route.scroll + item.top - viewport.top
                        } else if item.bottom > viewport.bottom {
                            route.scroll + item.bottom - viewport.bottom
                        } else {
                            route.scroll
                        }
                    });
                }
            }
            return 0;
        }
        WM_VSCROLL | WM_MOUSEWHEEL => {
            let viewport =
                ROUTES.with(|routes| routes.borrow().get(&(root as isize)).map(|r| r.viewport));
            if let Some(viewport) = viewport {
                let mut info = SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_TRACKPOS | SIF_PAGE,
                    ..Default::default()
                };
                GetScrollInfo(viewport, SB_VERT, &mut info);
                scroll(root, |route| {
                    if message == WM_MOUSEWHEEL {
                        route.scroll - ((w >> 16) as u16 as i16 as i32 / 120) * route.step * 2
                    } else {
                        match (w & 0xffff) as i32 {
                            SB_LINEUP => route.scroll - route.step,
                            SB_LINEDOWN => route.scroll + route.step,
                            SB_PAGEUP => route.scroll - info.nPage as i32,
                            SB_PAGEDOWN => route.scroll + info.nPage as i32,
                            SB_TOP => 0,
                            SB_BOTTOM => route.limit,
                            SB_THUMBTRACK | SB_THUMBPOSITION => info.nTrackPos,
                            _ => route.scroll,
                        }
                    }
                });
            }
            return 0;
        }
        WM_SIZE if window == root => {
            emit(root, UiAction::Layout);
            return 0;
        }
        WM_NCDESTROY => {
            ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
        }
        _ => {}
    }
    chrome::message(window, message, w, l).unwrap_or_else(|| DefWindowProcW(window, message, w, l))
}
struct Controls {
    labels: [HWND; 5],
    info: HWND,
    remove: HWND,
}
impl Controls {
    fn new() -> Self {
        Self {
            labels: [std::ptr::null_mut(); 5],
            info: std::ptr::null_mut(),
            remove: std::ptr::null_mut(),
        }
    }
}
impl Drop for Controls {
    fn drop(&mut self) {
        for window in self.labels.into_iter().chain([self.info, self.remove]) {
            if !window.is_null() {
                ACTIONS.with(|actions| actions.borrow_mut().remove(&(window as isize)));
                chrome::unregister(window);
                unsafe { DestroyWindow(window) };
            }
        }
    }
}
pub(super) struct Panel {
    pub(super) window: HWND,
    owner: HWND,
    id: Uuid,
    heading: HWND,
    repository: HWND,
    message: HWND,
    refresh: HWND,
    close: HWND,
    viewport: HWND,
    rows: Vec<Row>,
    controls: Vec<Controls>,
    title: String,
    status_text: String,
    busy: bool,
    area: Option<model::Rect>,
    scale: f64,
    background: bool,
    shown: bool,
    selected: Option<PathBuf>,
    modifiers: std::cell::Cell<u16>,
}
impl Panel {
    pub(super) fn new(owner: HWND, background: bool, id: Uuid) -> anyhow::Result<Self> {
        unsafe {
            anyhow::ensure!(IsWindow(owner) != 0, "Worktree panel owner is unavailable");
            let class = wide("flowmux.windows.worktrees");
            let instance = GetModuleHandleW(std::ptr::null());
            let definition = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&definition) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "Cannot register worktree panel"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Worktrees").as_ptr(),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            checked((!window.is_null()) as i32)?;
            let mut panel = Self {
                window,
                owner,
                id,
                heading: std::ptr::null_mut(),
                repository: std::ptr::null_mut(),
                message: std::ptr::null_mut(),
                refresh: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                viewport: std::ptr::null_mut(),
                rows: Vec::new(),
                controls: Vec::new(),
                title: String::new(),
                status_text: String::new(),
                busy: false,
                area: None,
                scale: 1.0,
                background,
                shown: false,
                selected: None,
                modifiers: std::cell::Cell::new(0),
            };
            ROUTES.with(|routes| {
                routes.borrow_mut().insert(
                    window as isize,
                    Route {
                        id,
                        viewport: std::ptr::null_mut(),
                        scroll: 0,
                        limit: 0,
                        step: 26,
                        open: false,
                        busy: false,
                        selection: None,
                    },
                )
            });
            chrome::register_control(window, chrome::ControlRole::Static);
            panel.heading = panel.child(
                window,
                "STATIC",
                "Worktrees",
                1,
                SS_NOPREFIX | SS_ENDELLIPSIS,
            )?;
            chrome::register_control(panel.heading, chrome::ControlRole::Caption);
            panel.repository =
                panel.child(window, "STATIC", "", 2, SS_NOPREFIX | SS_PATHELLIPSIS)?;
            panel.message = panel.child(window, "STATIC", "", 3, SS_NOPREFIX)?;
            panel.refresh = panel.child(window, "BUTTON", "Refresh", 4, 0)?;
            panel.close = panel.child(window, "BUTTON", "Close", 5, 0)?;
            panel.viewport = panel.child(
                window,
                "flowmux.windows.worktrees",
                "",
                6,
                WS_VSCROLL | WS_CLIPCHILDREN | WS_TABSTOP,
            )?;
            SetWindowLongW(panel.viewport, GWL_EXSTYLE, WS_EX_CONTROLPARENT as i32);
            ROUTES.with(|routes| {
                routes
                    .borrow_mut()
                    .get_mut(&(window as isize))
                    .unwrap()
                    .viewport = panel.viewport
            });
            ACTIONS.with(|actions| {
                let mut actions = actions.borrow_mut();
                actions.insert(panel.refresh as isize, (window, UiAction::Refresh));
                actions.insert(panel.close as isize, (window, UiAction::Close));
            });
            Ok(panel)
        }
    }
    fn child(
        &self,
        parent: HWND,
        class: &str,
        text: &str,
        id: usize,
        style: u32,
    ) -> anyhow::Result<HWND> {
        let style = style
            | if class == "BUTTON" {
                WS_TABSTOP | BS_OWNERDRAW as u32 | BS_NOTIFY as u32
            } else {
                0
            };
        let window = unsafe {
            CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                parent,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        checked((!window.is_null()) as i32)?;
        if class == "BUTTON" {
            chrome::register_button(window, chrome::Role::Button);
        } else {
            chrome::register_control(window, chrome::ControlRole::Static);
        }
        Ok(window)
    }
    pub(super) fn set_state(
        &mut self,
        title: &str,
        status: &str,
        rows: Vec<Row>,
        busy: bool,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            rows.len() <= MAX_ROWS,
            "Worktree panel supports up to 256 rows"
        );
        if self.title == title
            && self.status_text == status
            && self.rows == rows
            && self.busy == busy
        {
            return Ok(());
        }
        while self.controls.len() < rows.len() {
            let mut controls = Controls::new();
            let base = 100 + self.controls.len() * 8;
            for (index, label) in controls.labels.iter_mut().enumerate() {
                *label = self.child(
                    self.viewport,
                    "STATIC",
                    "",
                    base + index,
                    SS_NOPREFIX
                        | SS_NOTIFY
                        | if index == 2 {
                            SS_PATHELLIPSIS
                        } else {
                            SS_ENDELLIPSIS
                        },
                )?;
            }
            chrome::register_control(controls.labels[0], chrome::ControlRole::Caption);
            controls.info = self.child(self.viewport, "BUTTON", "Info", base + 5, 0)?;
            controls.remove = self.child(self.viewport, "BUTTON", "Remove", base + 6, 0)?;
            self.controls.push(controls);
        }
        self.title = title.into();
        self.status_text = status.into();
        if self
            .selected
            .as_ref()
            .is_some_and(|path| !rows.iter().any(|row| &row.info.path == path))
        {
            self.selected = None;
        }
        self.rows = rows;
        self.busy = busy;
        ROUTES.with(|routes| {
            routes
                .borrow_mut()
                .get_mut(&(self.window as isize))
                .unwrap()
                .busy = busy
        });
        unsafe {
            checked(SetWindowTextW(self.repository, wide(title).as_ptr()))?;
            checked(SetWindowTextW(self.message, wide(status).as_ptr()))?;
            EnableWindow(self.refresh, i32::from(!busy));
        }
        for (index, controls) in self.controls.iter().enumerate() {
            ACTIONS.with(|actions| {
                let mut actions = actions.borrow_mut();
                for label in controls.labels {
                    actions.remove(&(label as isize));
                }
                actions.remove(&(controls.info as isize));
                actions.remove(&(controls.remove as isize));
                if let Some(row) = self.rows.get(index) {
                    for label in controls.labels {
                        actions.insert(
                            label as isize,
                            (self.window, UiAction::Select(row.info.path.clone())),
                        );
                    }
                    actions.insert(
                        controls.info as isize,
                        (self.window, UiAction::Info(row.info.path.clone())),
                    );
                    actions.insert(
                        controls.remove as isize,
                        (self.window, UiAction::Remove(row.info.path.clone())),
                    );
                }
            });
            if let Some(row) = self.rows.get(index) {
                let labels = labels(row);
                for (window, value) in controls.labels.iter().zip(labels) {
                    unsafe { checked(SetWindowTextW(*window, wide(value).as_ptr()))? };
                }
                unsafe {
                    EnableWindow(
                        controls.remove,
                        i32::from(!busy && row.remove_block_reason.is_none()),
                    )
                };
            }
        }
        self.layout(self.area, self.scale, self.background);
        Ok(())
    }
    pub(super) fn layout(&mut self, area: Option<model::Rect>, scale: f64, background: bool) {
        self.area = area;
        if area.is_none() {
            self.modifiers.set(0);
        }
        self.scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self.background = background;
        ROUTES.with(|routes| {
            routes
                .borrow_mut()
                .get_mut(&(self.window as isize))
                .unwrap()
                .open = area.is_some()
        });
        unsafe {
            let show = area.is_some() && !background;
            if self.shown != show {
                ShowWindow(self.window, if show { SW_SHOWNA } else { SW_HIDE });
                self.shown = show;
            }
            let Some(area) = area else { return };
            let px = |n: i32| (n as f64 * self.scale).round() as i32;
            place(self.window, area.x, area.y, area.width, area.height);
            let width = area.width.max(1);
            let margin = px(6);
            let inner = (width - margin * 2).max(1);
            let close_width = px(48).min(inner);
            let refresh_width = px(64).min((inner - close_width - px(4)).max(1));
            place(
                self.close,
                width - margin - close_width,
                px(4),
                close_width,
                px(28),
            );
            place(
                self.refresh,
                width - margin - close_width - px(4) - refresh_width,
                px(4),
                refresh_width,
                px(28),
            );
            place(
                self.heading,
                margin,
                px(5),
                (inner - close_width - refresh_width - px(12)).max(1),
                px(24),
            );
            place(self.repository, margin, px(36), inner, px(20));
            let top = if self.status_text.is_empty() {
                px(60)
            } else {
                px(90)
            };
            ShowWindow(
                self.message,
                if self.status_text.is_empty() {
                    SW_HIDE
                } else {
                    SW_SHOWNA
                },
            );
            place(self.message, margin, px(59), inner, px(26));
            place(
                self.viewport,
                margin,
                top,
                inner,
                (area.height - top - margin).max(1),
            );
            let mut view = RECT::default();
            GetClientRect(self.viewport, &mut view);
            let page = view.bottom.max(1);
            let content = px(self.rows.len() as i32 * ROW_HEIGHT);
            let offset = ROUTES.with(|routes| {
                let mut routes = routes.borrow_mut();
                let route = routes.get_mut(&(self.window as isize)).unwrap();
                route.limit = (content - page).max(0);
                route.scroll = route.scroll.min(route.limit);
                route.step = px(26).max(1);
                route.scroll
            });
            let info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: content.saturating_sub(1),
                nPage: page as u32,
                nPos: offset,
                nTrackPos: 0,
            };
            SetScrollInfo(self.viewport, SB_VERT, &info, 1);
            GetClientRect(self.viewport, &mut view);
            let row_width = view.right.max(1);
            let selection = self
                .rows
                .iter()
                .position(|row| self.selected.as_ref() == Some(&row.info.path))
                .map(|index| model::Rect {
                    x: 0,
                    y: px(index as i32 * ROW_HEIGHT) - offset,
                    width: row_width,
                    height: px(ROW_HEIGHT),
                });
            ROUTES.with(|routes| {
                routes
                    .borrow_mut()
                    .get_mut(&(self.window as isize))
                    .unwrap()
                    .selection = selection
            });
            InvalidateRect(self.viewport, std::ptr::null(), 1);
            for (index, controls) in self.controls.iter().enumerate() {
                let visible = index < self.rows.len();
                for window in controls
                    .labels
                    .into_iter()
                    .chain([controls.info, controls.remove])
                {
                    ShowWindow(window, if visible { SW_SHOWNA } else { SW_HIDE });
                }
                if !visible {
                    continue;
                }
                let y = px(index as i32 * ROW_HEIGHT) - offset;
                for (line, label) in controls.labels.iter().enumerate() {
                    place(
                        *label,
                        px(4),
                        y + px(6 + line as i32 * 20),
                        (row_width - px(8)).max(1),
                        px(20),
                    );
                }
                place(
                    controls.info,
                    (row_width - px(144)).max(0),
                    y + px(110),
                    px(62).min(row_width),
                    px(28),
                );
                place(
                    controls.remove,
                    (row_width - px(76)).max(0),
                    y + px(110),
                    px(72).min(row_width),
                    px(28),
                );
            }
        }
    }
    pub(super) fn select(&mut self, path: &PathBuf) {
        let Some(index) = self.rows.iter().position(|row| &row.info.path == path) else {
            return;
        };
        self.selected = Some(path.clone());
        let top = (index as f64 * f64::from(ROW_HEIGHT) * self.scale).round() as i32;
        let height = (f64::from(ROW_HEIGHT) * self.scale).round() as i32;
        unsafe {
            let mut view = RECT::default();
            GetClientRect(self.viewport, &mut view);
            scroll(self.window, |route| {
                if top < route.scroll {
                    top
                } else if top + height > route.scroll + view.bottom {
                    top + height - view.bottom
                } else {
                    route.scroll
                }
            });
        }
        self.layout(self.area, self.scale, self.background);
        if !self.background {
            unsafe {
                SetFocus(self.controls[index].info);
            }
        }
    }
    pub(super) fn navigate(&mut self, key: usize) {
        let count = self.rows.len();
        if count == 0 {
            return;
        }
        let current = self
            .rows
            .iter()
            .position(|row| self.selected.as_ref() == Some(&row.info.path));
        let index = match key {
            0x24 => 0,
            0x23 => count - 1,
            0x26 => current.map_or(count - 1, |at| (at + count - 1) % count),
            0x28 => current.map_or(0, |at| (at + 1) % count),
            _ => return,
        };
        self.select(&self.rows[index].info.path.clone());
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if self.area.is_none()
                || (message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0)
                || IsWindowEnabled(self.owner) == 0
                || IsWindowEnabled(self.window) == 0
            {
                return false;
            }
            if matches!(
                message.message,
                WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
            ) {
                if let Some(bit) =
                    crate::keybindings::native_modifier(message.wParam, message.lParam)
                {
                    self.modifiers
                        .set(if matches!(message.message, WM_KEYUP | WM_SYSKEYUP) {
                            self.modifiers.get() & !bit
                        } else {
                            self.modifiers.get() | bit
                        });
                }
            }
            let modified = if self.background {
                self.modifiers.get() & (3 | 12 | 192) != 0
            } else {
                [0x11, 0x12, 0x5b, 0x5c]
                    .iter()
                    .any(|key| GetKeyState(*key) < 0)
            };
            if message.message == WM_KEYDOWN
                && matches!(message.wParam, 0x23 | 0x24 | 0x26 | 0x28)
                && !modified
            {
                emit(self.window, UiAction::Navigate(message.wParam));
                return true;
            }
            if message.message == WM_KEYDOWN && matches!(message.wParam, 13 | 27) {
                if message.lParam as usize & (1 << 30) == 0 {
                    if message.wParam == 27 {
                        emit(self.window, UiAction::Close);
                    } else if IsWindowEnabled(message.hwnd) != 0 {
                        let action = ACTIONS.with(|actions| {
                            actions.borrow().get(&(message.hwnd as isize)).cloned()
                        });
                        if let Some((owner, action)) =
                            action.filter(|(owner, _)| *owner == self.window)
                        {
                            emit(owner, action);
                        }
                    }
                }
                return true;
            }
            if self.background {
                return matches!(message.message, WM_KEYDOWN | WM_KEYUP | WM_CHAR)
                    && message.wParam == 9;
            }
            IsDialogMessageW(self.window, message) != 0
        }
    }
    pub(super) fn refresh_theme(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            unsafe { IsWindow(self.window) } != 0,
            "Worktree panel is unavailable"
        );
        for window in [
            self.window,
            self.heading,
            self.repository,
            self.message,
            self.viewport,
        ] {
            chrome::register_control(window, chrome::ControlRole::Static);
        }
        chrome::register_control(self.heading, chrome::ControlRole::Caption);
        for window in [self.refresh, self.close] {
            chrome::register_button(window, chrome::Role::Button);
        }
        for controls in &self.controls {
            for window in controls.labels {
                chrome::register_control(window, chrome::ControlRole::Static);
            }
            chrome::register_control(controls.labels[0], chrome::ControlRole::Caption);
            for window in [controls.info, controls.remove] {
                chrome::register_button(window, chrome::Role::Button);
            }
        }
        unsafe { InvalidateRect(self.window, std::ptr::null(), 1) };
        Ok(())
    }
    #[cfg(debug_assertions)]
    pub(super) fn capture_window(&self) -> Option<HWND> {
        self.area.map(|_| self.window)
    }
    pub(super) fn status(&self) -> Value {
        let route = ROUTES.with(|routes| routes.borrow().get(&(self.window as isize)).copied());
        let rows: Vec<_> = self.rows.iter().zip(&self.controls).map(|(row, controls)| {
            let labels = labels(row);
            json!({"path":row.info.path,"info":controls.info as usize,"remove":controls.remove as usize,
                "branch":labels[0],"subject":labels[1],"badges":labels[3],"remove_block_reason":row.remove_block_reason,
                "parent":self.viewport as usize,"label_handles":controls.labels.map(|window| window as usize),
                "branch_bounds":geometry(controls.labels[0],self.viewport),"path_bounds":geometry(controls.labels[2],self.viewport),
                "info_bounds":geometry(controls.info,self.viewport),"remove_bounds":geometry(controls.remove,self.viewport),
                "remove_enabled":unsafe{IsWindowEnabled(controls.remove)!=0}})
        }).collect();
        json!({"id":self.id,"window":self.window as usize,"owner":self.owner as usize,"refresh":self.refresh as usize,
            "close":self.close as usize,"viewport":self.viewport as usize,"open":self.area.is_some(),"busy":self.busy,
            "title":self.title,"status":self.status_text,"selected_path":self.selected,"rows":rows,"scroll":route.map_or(0,|r|r.scroll),
            "scroll_limit":route.map_or(0,|r|r.limit),"bounds":self.area,"viewport_bounds":geometry(self.viewport,self.window),
            "native_visible":unsafe{IsWindowVisible(self.window)!=0}})
    }
}
fn labels(row: &Row) -> [String; 5] {
    let info = &row.info;
    let branch = info.branch.clone().unwrap_or_else(|| {
        format!(
            "Detached at {}",
            info.head.chars().take(8).collect::<String>()
        )
    });
    let mut badges = Vec::new();
    if info.is_current {
        badges.push("Activated".into());
    }
    if info.lock_reason.is_some() {
        badges.push("Locked".into());
    }
    if let Some(changes) = &info.changes {
        let modified = changes.staged.saturating_add(changes.unstaged);
        if modified == 0 && changes.untracked == 0 {
            badges.push("Clean".into());
        }
        if modified != 0 {
            badges.push(format!("Modified {modified}"));
        }
        if changes.untracked != 0 {
            badges.push(format!("Untracked {}", changes.untracked));
        }
    } else {
        badges.push("Status unavailable".into());
    }
    [
        branch,
        info.commit_subject
            .clone()
            .unwrap_or_else(|| "Description unavailable".into()),
        info.path.to_string_lossy().into_owned(),
        badges.join(" · "),
        row.remove_block_reason.clone().unwrap_or_default(),
    ]
}
unsafe fn place(window: HWND, x: i32, y: i32, width: i32, height: i32) {
    SetWindowPos(
        window,
        std::ptr::null_mut(),
        x,
        y,
        width.max(1),
        height.max(1),
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
}
fn geometry(window: HWND, parent: HWND) -> Option<model::Rect> {
    unsafe {
        let mut rect = RECT::default();
        if GetWindowRect(window, &mut rect) == 0 {
            return None;
        }
        let mut point = POINT {
            x: rect.left,
            y: rect.top,
        };
        if ScreenToClient(parent, &mut point) == 0 {
            return None;
        }
        Some(model::Rect {
            x: point.x,
            y: point.y,
            width: rect.right - rect.left,
            height: rect.bottom - rect.top,
        })
    }
}
impl Drop for Panel {
    fn drop(&mut self) {
        ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
        ACTIONS.with(|actions| {
            actions
                .borrow_mut()
                .retain(|_, (owner, _)| *owner != self.window)
        });
        self.controls.clear();
        for window in [
            self.heading,
            self.repository,
            self.message,
            self.refresh,
            self.close,
            self.viewport,
            self.window,
        ] {
            if !window.is_null() {
                chrome::unregister(window);
            }
        }
        unsafe { DestroyWindow(self.window) };
    }
}
