// SPDX-License-Identifier: GPL-3.0-or-later
//! An owned native notification list. Construction never displays a window.
use super::*;
use std::cell::RefCell;

#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Show,
    Open,
    Read,
    Delete,
    Clear,
    Close,
    Selected,
    Layout,
}
fn emit(action: UiAction) {
    post(Event::NotificationUi(action));
}
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            emit(UiAction::Close);
            0
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(lparam as *mut MINMAXINFO);
            let scale = GetDpiForWindow(window).max(96) as f64 / 96.0;
            info.ptMinTrackSize.x = (730.0 * scale).round() as i32;
            info.ptMinTrackSize.y = (330.0 * scale).round() as i32;
            0
        }
        WM_SIZE => {
            emit(UiAction::Layout);
            0
        }
        WM_DPICHANGED => {
            let r = &*(lparam as *const RECT);
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            emit(UiAction::Layout);
            0
        }
        WM_COMMAND => {
            let code = (wparam >> 16) as u32;
            let action = match (wparam & 0xffff, code) {
                (1, BN_CLICKED) | (10, LBN_DBLCLK) => Some(UiAction::Open),
                (2, BN_CLICKED) => Some(UiAction::Close),
                (3, BN_CLICKED) => Some(UiAction::Read),
                (4, BN_CLICKED) => Some(UiAction::Delete),
                (5, BN_CLICKED) => Some(UiAction::Clear),
                (10, LBN_SELCHANGE) => Some(UiAction::Selected),
                _ => None,
            };
            if let Some(action) = action {
                emit(action);
            }
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}
pub(super) struct Panel {
    pub(super) window: HWND,
    list: HWND,
    detail: HWND,
    status: HWND,
    buttons: Vec<HWND>,
    values: RefCell<Vec<Value>>,
    message: RefCell<String>,
}
impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    pub(super) fn new(parent: HWND) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.notifications");
            let instance = GetModuleHandleW(std::ptr::null());
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_BTNFACE + 1) as HBRUSH,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register notification window"
            );
            let scale = GetDpiForWindow(parent).max(96) as f64 / 96.0;
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Notifications").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                (850.0 * scale).round() as i32,
                (560.0 * scale).round() as i32,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create notification window");
            let mut panel = Self {
                window,
                list: std::ptr::null_mut(),
                detail: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                buttons: vec![],
                values: RefCell::new(vec![]),
                message: RefCell::new(String::new()),
            };
            panel.status = panel.child("STATIC", "", 11, 0)?;
            panel.list = panel.child(
                "LISTBOX",
                "",
                10,
                WS_BORDER
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | WS_HSCROLL
                    | LBS_NOTIFY as u32
                    | LBS_NOINTEGRALHEIGHT as u32,
            )?;
            panel.detail = panel.child(
                "EDIT",
                "",
                12,
                WS_BORDER
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | ES_MULTILINE as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_READONLY as u32,
            )?;
            for (id, label) in [
                (1, "Open source"),
                (3, "Mark read"),
                (4, "Delete"),
                (5, "Clear all"),
                (2, "Close"),
            ] {
                panel.buttons.push(panel.child(
                    "BUTTON",
                    label,
                    id,
                    WS_TABSTOP | if id == 1 { BS_DEFPUSHBUTTON as u32 } else { 0 },
                )?);
            }
            panel.layout();
            Ok(panel)
        }
    }
    fn child(&self, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
        unsafe {
            let handle = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(text).as_ptr(),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                1,
                1,
                self.window,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            checked((!handle.is_null()) as i32)?;
            SendMessageW(
                handle,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as usize,
                1,
            );
            Ok(handle)
        }
    }
    pub(super) fn layout(&self) {
        unsafe {
            let mut r = RECT::default();
            GetClientRect(self.window, &mut r);
            let scale = GetDpiForWindow(self.window).max(96) as f64 / 96.0;
            let px = |v: i32| (v as f64 * scale).round() as i32;
            let list_height = ((r.bottom - px(126)) * 2 / 3).max(1);
            let mut rects = vec![
                (
                    self.status,
                    px(12),
                    px(10),
                    (r.right - px(24)).max(1),
                    px(26),
                ),
                (
                    self.list,
                    px(12),
                    px(42),
                    (r.right - px(24)).max(1),
                    list_height,
                ),
                (
                    self.detail,
                    px(12),
                    px(50) + list_height,
                    (r.right - px(24)).max(1),
                    (r.bottom - list_height - px(100)).max(1),
                ),
            ];
            for (index, button) in self.buttons.iter().enumerate() {
                rects.push((
                    *button,
                    px(12 + index as i32 * 136),
                    r.bottom - px(40),
                    px(128),
                    px(28),
                ));
            }
            for (hwnd, x, y, w, h) in rects {
                if !hwnd.is_null() {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        x,
                        y,
                        w,
                        h,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            SendMessageW(self.list, LB_SETHORIZONTALEXTENT, px(2400) as usize, 0);
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            IsWindowVisible(self.window) != 0
                && (message.hwnd == self.window || IsChild(self.window, message.hwnd) != 0)
                && IsDialogMessageW(self.window, message) != 0
        }
    }
    pub(super) fn show(&self, background: bool) {
        if !background {
            unsafe {
                ShowWindow(self.window, SW_SHOW);
                SetFocus(self.list);
            }
        }
    }
    pub(super) fn hide(&self) {
        unsafe {
            ShowWindow(self.window, SW_HIDE);
        }
    }
    pub(super) fn status(&self, text: &str) {
        if *self.message.borrow() != text {
            unsafe {
                SetWindowTextW(self.status, wide(text).as_ptr());
            }
            *self.message.borrow_mut() = text.to_owned();
        }
    }
    pub(super) fn status_text(&self) -> String {
        unsafe {
            let mut buf = vec![0u16; GetWindowTextLengthW(self.status) as usize + 1];
            let n = GetWindowTextW(self.status, buf.as_mut_ptr(), buf.len() as i32);
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        }
    }
    pub(super) fn selected(&self) -> Option<Uuid> {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, 0, 0) };
        (index >= 0)
            .then(|| {
                self.values
                    .borrow()
                    .get(index as usize)
                    .and_then(|v| v["id"].as_str())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            .flatten()
    }
    pub(super) fn rows(&self) -> usize {
        unsafe { SendMessageW(self.list, LB_GETCOUNT, 0, 0).max(0) as usize }
    }
    pub(super) fn results(&self, values: &[Value]) {
        if *self.values.borrow() == values && !self.message.borrow().is_empty() {
            return;
        }
        let selected = self.selected();
        unsafe {
            SendMessageW(self.list, WM_SETREDRAW, 0, 0);
            SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
            for value in values {
                let state = if value["read"].as_bool() == Some(true) {
                    "Read"
                } else {
                    "Unread"
                };
                let label = format!(
                    "[{state}] {} / {} — {}{}",
                    value["level"].as_str().unwrap_or(""),
                    value["workspace_name"].as_str().unwrap_or("Global"),
                    value["title"]
                        .as_str()
                        .unwrap_or("")
                        .replace(['\n', '\r', '\t'], " "),
                    if value["closed"] == true {
                        " (source closed)"
                    } else {
                        ""
                    }
                );
                SendMessageW(self.list, LB_ADDSTRING, 0, wide(label).as_ptr() as isize);
            }
            let index = selected
                .and_then(|id| {
                    values
                        .iter()
                        .position(|v| v["id"].as_str().is_some_and(|s| s == id.to_string()))
                })
                .unwrap_or(0);
            if !values.is_empty() {
                SendMessageW(self.list, LB_SETCURSEL, index, 0);
            }
            SendMessageW(self.list, WM_SETREDRAW, 1, 0);
            InvalidateRect(self.list, std::ptr::null(), 1);
        }
        *self.values.borrow_mut() = values.to_vec();
        self.detail();
        self.status(&format!(
            "{} retained · {} unread · Desktop delivery not yet available",
            values.len(),
            values.iter().filter(|v| v["read"] == false).count()
        ));
    }
    pub(super) fn detail(&self) {
        let selected = self.selected();
        let text = selected
            .and_then(|id| {
                self.values
                    .borrow()
                    .iter()
                    .find(|v| v["id"].as_str().is_some_and(|s| s == id.to_string()))
                    .map(|v| {
                        format!(
                            "{}\r\n\r\n{}",
                            v["title"].as_str().unwrap_or(""),
                            v["body"]
                                .as_str()
                                .unwrap_or("")
                                .replace("\r\n", "\n")
                                .replace('\n', "\r\n")
                        )
                    })
            })
            .unwrap_or_default();
        unsafe {
            SetWindowTextW(self.detail, wide(text).as_ptr());
        }
    }
}
