// SPDX-License-Identifier: GPL-3.0-or-later
//! Modeless owned SSH forwarding controls; all forwarding work belongs to the host.
use super::*;
use flowmux_core::SshForwardSpec;
use windows_sys::Win32::System::SystemServices::SS_NOPREFIX;
use windows_sys::Win32::UI::{
    Controls::{SetScrollInfo, BST_CHECKED, EM_LIMITTEXT, EM_SETCUEBANNER},
    Input::KeyboardAndMouse::EnableWindow,
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};

// ponytail: 16 retained native rows bound UI/process cost; virtualize the list
// and raise the matching admission/state limits if larger workspaces need it.
const MAX_ROWS: usize = 16;
const ADD: usize = 1;
const ERROR: usize = 5;
const REMOTE_CUE: &str = "Remote TCP port, e.g. 3000";
const LOCAL_CUE: &str = "Local port (empty = automatic)";
#[derive(Clone)]
pub(super) struct Row {
    pub(super) id: Uuid,
    pub(super) remote_port: u16,
    pub(super) local_port: Option<u16>,
    pub(super) state: String,
    pub(super) error: Option<String>,
    pub(super) authentication_available: bool,
}
#[derive(Clone, Copy)]
pub(super) enum UiAction {
    Add,
    Remove(Uuid),
    Preview(Uuid),
    Authentication(Uuid),
    Close,
    Layout,
}
#[derive(Clone, Copy)]
struct Route {
    id: Uuid,
    composing: bool,
    settling: bool,
    pending: bool,
    scroll: i32,
    limit: i32,
}
thread_local! {
    static ROUTES: RefCell<HashMap<isize, Route>> = RefCell::new(HashMap::new());
    static ACTIONS: RefCell<HashMap<isize, (HWND, UiAction)>> = RefCell::new(HashMap::new());
}
fn root(mut window: HWND) -> HWND {
    for _ in 0..4 {
        if ROUTES.with(|routes| routes.borrow().contains_key(&(window as isize))) {
            return window;
        }
        window = unsafe { GetParent(window) };
        if window.is_null() {
            break;
        }
    }
    std::ptr::null_mut()
}
fn route(window: HWND) -> Option<Route> {
    ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied())
}
fn emit(window: HWND, action: UiAction) {
    let id = ROUTES.with(|routes| {
        let mut routes = routes.borrow_mut();
        let route = routes.get_mut(&(window as isize))?;
        if matches!(
            action,
            UiAction::Add
                | UiAction::Remove(_)
                | UiAction::Preview(_)
                | UiAction::Authentication(_)
        ) {
            if route.composing || route.settling || route.pending {
                return None;
            }
            route.pending = true;
        }
        Some(route.id)
    });
    if let Some(id) = id {
        post(Event::SshPorts(id, action));
    }
}
unsafe extern "system" fn edit_proc(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    subclass: usize,
    _: usize,
) -> LRESULT {
    let owner = root(window);
    ROUTES.with(|routes| {
        if let Some(route) = routes.borrow_mut().get_mut(&(owner as isize)) {
            match message {
                WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION => {
                    route.composing = message == WM_IME_STARTCOMPOSITION;
                    route.settling = true;
                }
                WM_KEYDOWN if w == 229 => route.settling = true,
                WM_KEYUP if crate::keybindings::native_modifier(w, l).is_none() => {
                    route.settling = false
                }
                WM_KILLFOCUS => {
                    route.composing = false;
                    route.settling = false;
                }
                _ => {}
            }
        }
    });
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(window, Some(edit_proc), subclass);
    }
    DefSubclassProc(window, message, w, l)
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let owner = root(window);
    match message {
        WM_CLOSE => {
            emit(owner, UiAction::Close);
            return 0;
        }
        WM_SIZE if GetWindowLongW(window, GWL_STYLE) as u32 & WS_CHILD == 0 => {
            emit(owner, UiAction::Layout);
            return 0;
        }
        WM_DPICHANGED if l != 0 => {
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
            emit(owner, UiAction::Layout);
            return 0;
        }
        WM_GETMINMAXINFO if l != 0 => {
            let dpi = GetDpiForWindow(window).max(96);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 480 * dpi as i32 / 96,
                bottom: 350 * dpi as i32 / 96,
            };
            AdjustWindowRectExForDpi(
                &mut rect,
                GetWindowLongW(window, GWL_STYLE) as u32,
                0,
                GetWindowLongW(window, GWL_EXSTYLE) as u32,
                dpi,
            );
            let info = &mut *(l as *mut MINMAXINFO);
            info.ptMinTrackSize.x = rect.right - rect.left;
            info.ptMinTrackSize.y = rect.bottom - rect.top;
            return 0;
        }
        WM_VSCROLL | WM_MOUSEWHEEL => {
            let mut info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_TRACKPOS,
                ..Default::default()
            };
            GetScrollInfo(window, SB_VERT, &mut info);
            ROUTES.with(|routes| {
                if let Some(route) = routes.borrow_mut().get_mut(&(owner as isize)) {
                    let next = if message == WM_MOUSEWHEEL {
                        route.scroll - ((w >> 16) as u16 as i16 as i32 / 120) * 52
                    } else {
                        match (w & 0xffff) as i32 {
                            SB_LINEUP => route.scroll - 26,
                            SB_LINEDOWN => route.scroll + 26,
                            SB_PAGEUP => route.scroll - 156,
                            SB_PAGEDOWN => route.scroll + 156,
                            SB_TOP => 0,
                            SB_BOTTOM => route.limit,
                            SB_THUMBTRACK | SB_THUMBPOSITION => info.nTrackPos,
                            _ => route.scroll,
                        }
                    };
                    route.scroll = next.clamp(0, route.limit);
                }
            });
            emit(owner, UiAction::Layout);
            return 0;
        }
        WM_CTLCOLORSTATIC if l != 0 && GetDlgCtrlID(l as HWND) == ERROR as i32 => {
            let brush = chrome::message(window, message, w, l).unwrap_or(0);
            let palette = chrome::palette();
            SetTextColor(
                w as HDC,
                if palette.high_contrast {
                    palette.foreground
                } else {
                    palette.destructive
                },
            );
            return brush;
        }
        WM_COMMAND if (w >> 16) as u32 == BN_CLICKED && l != 0 => {
            let child = l as HWND;
            if GetParent(child) == window
                && IsWindowEnabled(owner) != 0
                && IsWindowEnabled(child) != 0
            {
                let action =
                    ACTIONS.with(|actions| actions.borrow().get(&(child as isize)).copied());
                if let Some((target, action)) = action {
                    if target == owner {
                        emit(owner, action);
                    }
                }
            }
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
    label: HWND,
    preview: HWND,
    authentication: HWND,
    remove: HWND,
}
pub(super) struct Panel {
    pub(super) id: Uuid,
    pub(super) workspace: WorkspaceId,
    pub(super) window: HWND,
    viewport: HWND,
    empty: HWND,
    remote: HWND,
    local: HWND,
    https: HWND,
    add: HWND,
    error: HWND,
    rows: Vec<Row>,
    controls: Vec<Controls>,
    background: bool,
}
impl Panel {
    pub(super) fn new(
        parent: HWND,
        workspace: WorkspaceId,
        background: bool,
        rows: Vec<Row>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            rows.len() <= MAX_ROWS,
            "At most 16 forwarded ports are supported"
        );
        unsafe {
            anyhow::ensure!(
                IsWindow(parent) != 0 && IsWindowEnabled(parent) != 0,
                "SSH Ports owner is unavailable"
            );
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux.windows.ssh.ports");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register SSH Ports window"
            );
            let dpi = GetDpiForWindow(parent).max(96);
            let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN;
            let ex = WS_EX_CONTROLPARENT;
            let mut bounds = RECT::default();
            checked(GetWindowRect(parent, &mut bounds))?;
            let mut frame = RECT {
                left: 0,
                top: 0,
                right: 480 * dpi as i32 / 96,
                bottom: 430 * dpi as i32 / 96,
            };
            checked(AdjustWindowRectExForDpi(&mut frame, style, 0, ex, dpi))?;
            let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
            let window = CreateWindowExW(
                ex,
                class.as_ptr(),
                wide("SSH Ports — local browser preview").as_ptr(),
                style,
                bounds.left + (bounds.right - bounds.left - width) / 2,
                bounds.top + (bounds.bottom - bounds.top - height) / 2,
                width,
                height,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create SSH Ports window");
            let mut panel = Self {
                id: Uuid::new_v4(),
                workspace,
                window,
                viewport: std::ptr::null_mut(),
                empty: std::ptr::null_mut(),
                remote: std::ptr::null_mut(),
                local: std::ptr::null_mut(),
                https: std::ptr::null_mut(),
                add: std::ptr::null_mut(),
                error: std::ptr::null_mut(),
                rows: Vec::new(),
                controls: Vec::new(),
                background,
            };
            ROUTES.with(|routes| {
                routes.borrow_mut().insert(
                    window as isize,
                    Route {
                        id: panel.id,
                        composing: false,
                        settling: false,
                        pending: false,
                        scroll: 0,
                        limit: 0,
                    },
                )
            });
            panel.viewport = panel.child(
                window,
                "flowmux.windows.ssh.ports",
                "",
                10,
                WS_VSCROLL | WS_CLIPCHILDREN,
            )?;
            SetWindowLongW(panel.viewport, GWL_EXSTYLE, WS_EX_CONTROLPARENT as i32);
            panel.empty = panel.child(
                panel.viewport,
                "STATIC",
                "No forwarded ports",
                11,
                SS_NOPREFIX,
            )?;
            panel.remote = panel.child(
                window,
                "EDIT",
                "",
                2,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            panel.local = panel.child(
                window,
                "EDIT",
                "",
                3,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            for (input, cue) in [(panel.remote, REMOTE_CUE), (panel.local, LOCAL_CUE)] {
                SendMessageW(input, EM_LIMITTEXT, 5, 0);
                checked(
                    SendMessageW(input, EM_SETCUEBANNER, 1, wide(cue).as_ptr() as LPARAM) as i32,
                )
                .context("cannot set SSH Ports hint")?;
                checked(SetWindowSubclass(input, Some(edit_proc), 1, 0))?;
            }
            panel.https = panel.child(
                window,
                "BUTTON",
                "HTTPS preview",
                4,
                WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            )?;
            panel.error = panel.child(window, "STATIC", "", ERROR, SS_NOPREFIX)?;
            panel.add = panel.child(
                window,
                "BUTTON",
                "Add forwarding",
                ADD,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            ACTIONS.with(|actions| {
                actions
                    .borrow_mut()
                    .insert(panel.add as isize, (window, UiAction::Add))
            });
            for index in 0..MAX_ROWS {
                let label = panel.child(panel.viewport, "STATIC", "", 100 + index, SS_NOPREFIX)?;
                let preview = panel.child(
                    panel.viewport,
                    "BUTTON",
                    "Open preview",
                    1000 + index * 3,
                    WS_TABSTOP | BS_OWNERDRAW as u32,
                )?;
                let authentication = panel.child(
                    panel.viewport,
                    "BUTTON",
                    "Authentication",
                    1001 + index * 3,
                    WS_TABSTOP | BS_OWNERDRAW as u32,
                )?;
                let remove = panel.child(
                    panel.viewport,
                    "BUTTON",
                    "Remove",
                    1002 + index * 3,
                    WS_TABSTOP | BS_OWNERDRAW as u32,
                )?;
                panel.controls.push(Controls {
                    label,
                    preview,
                    authentication,
                    remove,
                });
            }
            panel.set_rows(rows);
            if !background {
                ShowWindow(window, SW_SHOW);
                SetFocus(panel.remote);
            }
            Ok(panel)
        }
    }
    fn child(
        &self,
        parent: HWND,
        class: &str,
        caption: &str,
        id: usize,
        style: u32,
    ) -> anyhow::Result<HWND> {
        let window = unsafe {
            CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(caption).as_ptr(),
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
        anyhow::ensure!(!window.is_null(), "cannot create SSH Ports control");
        if class == "BUTTON" && style & BS_TYPEMASK as u32 == BS_OWNERDRAW as u32 {
            chrome::register_button(
                window,
                if id == ADD {
                    chrome::Role::Suggested
                } else {
                    chrome::Role::Button
                },
            );
        } else if class == "EDIT" {
            chrome::register_control(window, chrome::ControlRole::Edit);
        } else if class != "flowmux.windows.ssh.ports" {
            chrome::register_control(window, chrome::ControlRole::Static);
        }
        Ok(window)
    }
    pub(super) fn draft(&self) -> anyhow::Result<SshForwardSpec> {
        anyhow::ensure!(
            !route(self.window).is_some_and(|route| route.composing || route.settling),
            "Finish composing text before adding forwarding"
        );
        let remote = text(self.remote)?;
        let local = text(self.local)?;
        let remote_port = remote
            .trim()
            .parse::<u16>()
            .context("Enter a remote port between 1 and 65535")?;
        let local_port = if local.trim().is_empty() {
            None
        } else {
            Some(
                local
                    .trim()
                    .parse::<u16>()
                    .context("Enter a local port between 1 and 65535")?,
            )
        };
        let spec = SshForwardSpec {
            id: Uuid::new_v4(),
            remote_port,
            local_port,
            https: unsafe { SendMessageW(self.https, BM_GETCHECK, 0, 0) == BST_CHECKED as isize },
        };
        spec.validate().map_err(anyhow::Error::msg)?;
        Ok(spec)
    }
    pub(super) fn set_rows(&mut self, rows: Vec<Row>) {
        if rows.len() > MAX_ROWS {
            self.set_error("At most 16 forwarded ports are supported");
            return;
        }
        self.rows = rows;
        self.set_error("");
        for (index, control) in self.controls.iter().enumerate() {
            ACTIONS.with(|actions| {
                let mut actions = actions.borrow_mut();
                actions.remove(&(control.preview as isize));
                actions.remove(&(control.authentication as isize));
                actions.remove(&(control.remove as isize));
            });
            if let Some(row) = self.rows.get(index) {
                let mut label = format!(
                    "Remote {} → local {} · {}",
                    row.remote_port,
                    row.local_port
                        .map(|port| port.to_string())
                        .unwrap_or_else(|| "inactive".into()),
                    row.state
                );
                if let Some(error) = &row.error {
                    label.push_str(&format!("\r\n{error}"));
                }
                unsafe {
                    SetWindowTextW(control.label, wide(&label).as_ptr());
                    EnableWindow(control.preview, i32::from(row.local_port.is_some()));
                    EnableWindow(
                        control.authentication,
                        i32::from(row.authentication_available),
                    );
                }
                ACTIONS.with(|actions| {
                    let mut actions = actions.borrow_mut();
                    actions.insert(
                        control.preview as isize,
                        (self.window, UiAction::Preview(row.id)),
                    );
                    actions.insert(
                        control.authentication as isize,
                        (self.window, UiAction::Authentication(row.id)),
                    );
                    actions.insert(
                        control.remove as isize,
                        (self.window, UiAction::Remove(row.id)),
                    );
                });
            }
        }
        self.relayout();
    }
    pub(super) fn set_error(&self, value: &str) {
        ROUTES.with(|routes| {
            if let Some(route) = routes.borrow_mut().get_mut(&(self.window as isize)) {
                route.pending = false;
            }
        });
        unsafe {
            SetWindowTextW(self.error, wide(value).as_ptr());
        }
    }
    pub(super) fn relayout(&self) {
        unsafe {
            let mut client = RECT::default();
            if GetClientRect(self.window, &mut client) == 0 {
                return;
            }
            let dpi = GetDpiForWindow(self.window).max(96) as i32;
            let px = |dip: i32| dip * dpi / 96;
            let width = (client.right - px(24)).max(1);
            let form = (client.bottom - px(194)).max(px(100));
            let place = |window, x, y, width: i32, height: i32| {
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    x,
                    y,
                    width.max(1),
                    height.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            };
            place(self.viewport, px(12), px(12), width, form - px(20));
            let mut viewport = RECT::default();
            GetClientRect(self.viewport, &mut viewport);
            let content = px(self.rows.len() as i32 * 76);
            let page = viewport.bottom.max(1);
            let scroll = ROUTES.with(|routes| {
                let mut routes = routes.borrow_mut();
                let route = routes.get_mut(&(self.window as isize)).unwrap();
                route.limit = (content - page).max(0);
                route.scroll = route.scroll.min(route.limit);
                route.scroll
            });
            let info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: content.saturating_sub(1),
                nPage: page as u32,
                nPos: scroll,
                nTrackPos: 0,
            };
            SetScrollInfo(self.viewport, SB_VERT, &info, 1);
            GetClientRect(self.viewport, &mut viewport);
            let row_width = viewport.right.max(1);
            ShowWindow(
                self.empty,
                if self.rows.is_empty() {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
            place(self.empty, 0, 0, row_width, px(28));
            for (index, control) in self.controls.iter().enumerate() {
                let visible = index < self.rows.len();
                for child in [
                    control.label,
                    control.preview,
                    control.authentication,
                    control.remove,
                ] {
                    ShowWindow(child, if visible { SW_SHOWNA } else { SW_HIDE });
                }
                if visible {
                    let y = px(index as i32 * 76) - scroll;
                    place(control.label, 0, y, row_width, px(40));
                    place(control.preview, 0, y + px(42), px(110), px(28));
                    place(control.authentication, px(118), y + px(42), px(136), px(28));
                    place(control.remove, px(262), y + px(42), px(80), px(28));
                }
            }
            place(self.remote, px(12), form, width, px(28));
            place(self.local, px(12), form + px(36), width, px(28));
            place(self.https, px(12), form + px(72), width, px(26));
            place(self.add, px(12), form + px(106), width, px(32));
            place(self.error, px(12), form + px(144), width, px(42));
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if (message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0)
                || IsWindowEnabled(self.window) == 0
            {
                return false;
            }
            if route(self.window).is_some_and(|route| route.composing || route.settling)
                || message.wParam == 229
            {
                return false;
            }
            if message.message == WM_KEYDOWN && matches!(message.wParam, 13 | 27) {
                if message.lParam as usize & (1 << 30) == 0 {
                    if message.wParam == 27 {
                        emit(self.window, UiAction::Close);
                    } else if IsWindowEnabled(message.hwnd) != 0 {
                        let action = ACTIONS.with(|actions| {
                            actions
                                .borrow()
                                .get(&(message.hwnd as isize))
                                .map(|(_, action)| *action)
                        });
                        emit(self.window, action.unwrap_or(UiAction::Add));
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
    pub(super) fn status(&self) -> Value {
        let state = route(self.window);
        let rows:Vec<_>=self.rows.iter().zip(&self.controls).map(|(row,control)|json!({"id":row.id,"remote_port":row.remote_port,"local_port":row.local_port,"state":row.state,"error":row.error,"label":control.label as usize,"preview":control.preview as usize,"authentication":control.authentication as usize,"authentication_available":row.authentication_available,"remove":control.remove as usize})).collect();
        json!({"id":self.id,"workspace":self.workspace,"window":self.window as usize,"owner":unsafe{GetWindow(self.window,GW_OWNER)} as usize,"viewport":self.viewport as usize,"remote":self.remote as usize,"local":self.local as usize,"https":self.https as usize,"add":self.add as usize,"error_handle":self.error as usize,"error":text(self.error).unwrap_or_default(),"remote_value":text(self.remote).unwrap_or_default(),"local_value":text(self.local).unwrap_or_default(),"https_checked":unsafe{SendMessageW(self.https,BM_GETCHECK,0,0)==BST_CHECKED as isize},"rows":rows,"scroll":state.map_or(0,|route|route.scroll),"composing":state.is_some_and(|route|route.composing),"pending":state.is_some_and(|route|route.pending),"native_visible":unsafe{IsWindowVisible(self.window)!=0}})
    }
}
fn text(window: HWND) -> anyhow::Result<String> {
    unsafe {
        let length = GetWindowTextLengthW(window).max(0) as usize;
        anyhow::ensure!(length <= 16384, "SSH Ports field is too long");
        let mut value = vec![0u16; length + 1];
        let read = GetWindowTextW(window, value.as_mut_ptr(), value.len() as i32).max(0) as usize;
        String::from_utf16(&value[..read]).context("SSH Ports field contains invalid UTF-16")
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
        for child in [
            self.empty,
            self.remote,
            self.local,
            self.https,
            self.add,
            self.error,
        ]
        .into_iter()
        .chain(self.controls.iter().flat_map(|controls| {
            [
                controls.label,
                controls.preview,
                controls.authentication,
                controls.remove,
            ]
        })) {
            if !child.is_null() {
                chrome::unregister(child);
            }
        }
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
