// SPDX-License-Identifier: GPL-3.0-or-later
//! Native EDIT/LISTBOX controls; creation never displays the owned window.
use super::*;
use std::cell::RefCell;
use windows_sys::Win32::UI::Controls::EM_LIMITTEXT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;

#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Show,
    Changed,
    Refresh,
    Next,
    Previous,
    Open,
    Cancel,
    Close,
    Layout,
    Tick,
}
fn emit(action: UiAction) {
    post(Event::SearchUi(action));
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
        WM_SIZE => {
            emit(UiAction::Layout);
            0
        }
        WM_TIMER => {
            KillTimer(window, 2);
            emit(UiAction::Tick);
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
            let id = wparam & 0xffff;
            let code = (wparam >> 16) as u32;
            let action = match (id, code) {
                (10, EN_CHANGE) | (11, BN_CLICKED) => Some(UiAction::Changed),
                (1, BN_CLICKED) => Some(UiAction::Refresh),
                (20, LBN_DBLCLK) => Some(UiAction::Open),
                (30, BN_CLICKED) => Some(UiAction::Previous),
                (31, BN_CLICKED) => Some(UiAction::Next),
                (32, BN_CLICKED) => Some(UiAction::Open),
                (33, BN_CLICKED) => Some(UiAction::Cancel),
                (2, BN_CLICKED) => Some(UiAction::Close),
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
    query: HWND,
    case: HWND,
    refresh: HWND,
    status: HWND,
    list: HWND,
    previous: HWND,
    next: HWND,
    open: HWND,
    cancel: HWND,
    close: HWND,
    labels: RefCell<Vec<String>>,
    status_text: RefCell<String>,
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
            let class = wide("flowmux.windows.output-search");
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
                "cannot register search window"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Search all terminals").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                850,
                560,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create search window");
            // Own the HWND immediately so any subsequent control-creation failure cleans up.
            let mut panel = Self {
                window,
                query: std::ptr::null_mut(),
                case: std::ptr::null_mut(),
                refresh: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                list: std::ptr::null_mut(),
                previous: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
                open: std::ptr::null_mut(),
                cancel: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                labels: RefCell::new(Vec::new()),
                status_text: RefCell::new(String::new()),
            };
            panel.query = panel.child(
                "EDIT",
                "",
                10,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            SendMessageW(panel.query, EM_LIMITTEXT, 1024, 0);
            panel.case = panel.child(
                "BUTTON",
                "Match case",
                11,
                WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            )?;
            panel.refresh =
                panel.child("BUTTON", "Refresh", 1, WS_TABSTOP | BS_DEFPUSHBUTTON as u32)?;
            panel.status = panel.child("STATIC", "", 12, 0)?;
            panel.list = panel.child(
                "LISTBOX",
                "",
                20,
                WS_BORDER
                    | WS_TABSTOP
                    | WS_VSCROLL
                    | WS_HSCROLL
                    | LBS_NOTIFY as u32
                    | LBS_NOINTEGRALHEIGHT as u32,
            )?;
            panel.previous = panel.child("BUTTON", "Previous 500", 30, WS_TABSTOP)?;
            panel.next = panel.child("BUTTON", "Next 500", 31, WS_TABSTOP)?;
            panel.open = panel.child("BUTTON", "Open result", 32, WS_TABSTOP)?;
            panel.cancel = panel.child("BUTTON", "Cancel search", 33, WS_TABSTOP)?;
            panel.close = panel.child("BUTTON", "Close", 2, WS_TABSTOP)?;
            panel.layout();
            panel.status("Search retained output in all workspaces in this window");
            panel.buttons(false, false, false);
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
                GetStockObject(DEFAULT_GUI_FONT) as WPARAM,
                1,
            );
            Ok(handle)
        }
    }
    pub(super) fn layout(&self) {
        unsafe {
            let mut rect = RECT::default();
            GetClientRect(self.window, &mut rect);
            let scale = GetDpiForWindow(self.window).max(96) as f64 / 96.0;
            let px = |n: i32| (n as f64 * scale).round() as i32;
            let width = rect.right;
            let height = rect.bottom;
            for (handle, x, y, w, h) in [
                (self.query, px(12), px(12), (width - px(252)).max(1), px(28)),
                (self.case, width - px(230), px(12), px(112), px(28)),
                (self.refresh, width - px(110), px(12), px(98), px(28)),
                (self.status, px(12), px(48), (width - px(24)).max(1), px(42)),
                (
                    self.list,
                    px(12),
                    px(94),
                    (width - px(24)).max(1),
                    (height - px(146)).max(1),
                ),
                (self.previous, px(12), height - px(40), px(118), px(28)),
                (self.next, px(138), height - px(40), px(118), px(28)),
                (self.open, px(264), height - px(40), px(118), px(28)),
                (self.cancel, px(390), height - px(40), px(118), px(28)),
                (self.close, width - px(110), height - px(40), px(98), px(28)),
            ] {
                if !handle.is_null() {
                    SetWindowPos(
                        handle,
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
    pub(super) fn show(&self, background: bool) {
        if !background {
            unsafe {
                ShowWindow(self.window, SW_SHOW);
                SetFocus(self.query);
            }
        }
    }
    pub(super) fn schedule(&self) {
        unsafe {
            SetTimer(self.window, 2, 200, None);
        }
    }
    pub(super) fn hide(&self) {
        unsafe {
            ShowWindow(self.window, SW_HIDE);
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            IsWindowVisible(self.window) != 0
                && (message.hwnd == self.window || IsChild(self.window, message.hwnd) != 0)
                && IsDialogMessageW(self.window, message) != 0
        }
    }
    pub(super) fn query(&self, text: &str, case: bool) {
        unsafe {
            SetWindowTextW(self.query, wide(text).as_ptr());
            SendMessageW(self.case, BM_SETCHECK, usize::from(case), 0);
        }
    }
    pub(super) fn read_query(&self) -> (String, bool) {
        unsafe {
            let mut text = vec![0u16; GetWindowTextLengthW(self.query) as usize + 1];
            let size = GetWindowTextW(self.query, text.as_mut_ptr(), text.len() as i32);
            (
                String::from_utf16_lossy(&text[..size.max(0) as usize]),
                SendMessageW(self.case, BM_GETCHECK, 0, 0) == 1,
            )
        }
    }
    pub(super) fn status(&self, text: &str) {
        if *self.status_text.borrow() != text {
            unsafe {
                SetWindowTextW(self.status, wide(text).as_ptr());
            }
            *self.status_text.borrow_mut() = text.to_owned();
        }
    }
    pub(super) fn results(&self, hits: &[Hit]) {
        let labels: Vec<_> = hits
            .iter()
            .map(|hit| {
                format!(
                    "{} / {}  —  {}",
                    hit.workspace,
                    hit.title,
                    hit.found.preview.replace(['\r', '\n', '\t'], " ")
                )
            })
            .collect();
        if *self.labels.borrow() == labels {
            return;
        }
        unsafe {
            SendMessageW(self.list, WM_SETREDRAW, 0, 0);
            SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
            for label in &labels {
                SendMessageW(self.list, LB_ADDSTRING, 0, wide(label).as_ptr() as isize);
            }
            if !labels.is_empty() {
                SendMessageW(self.list, LB_SETCURSEL, 0, 0);
            }
            SendMessageW(self.list, WM_SETREDRAW, 1, 0);
            InvalidateRect(self.list, std::ptr::null(), 1);
        }
        *self.labels.borrow_mut() = labels;
    }
    pub(super) fn buttons(&self, previous: bool, next: bool, open: bool) {
        unsafe {
            EnableWindow(self.previous, previous as i32);
            EnableWindow(self.next, next as i32);
            EnableWindow(self.open, open as i32);
        }
    }
    pub(super) fn rows(&self) -> usize {
        unsafe { SendMessageW(self.list, LB_GETCOUNT, 0, 0).max(0) as usize }
    }
    pub(super) fn selected(&self) -> Option<usize> {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, 0, 0) };
        (index >= 0).then_some(index as usize)
    }
}
