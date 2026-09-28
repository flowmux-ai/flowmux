// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned page-find controls. Construction and background requests never show the window.
use super::*;
use std::cell::RefCell;
use windows_sys::Win32::{
    System::SystemServices::SS_NOPREFIX,
    UI::{
        Controls::EM_LIMITTEXT,
        Input::KeyboardAndMouse::{GetFocus, VK_TAB},
    },
};

#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Next,
    Previous,
    Close,
    Layout,
}

fn emit(action: UiAction) {
    post(Event::BrowserFindUi(action));
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
            info.ptMinTrackSize.x = (480.0 * scale).round() as i32;
            info.ptMinTrackSize.y = (180.0 * scale).round() as i32;
            0
        }
        WM_SIZE => {
            emit(UiAction::Layout);
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
            emit(UiAction::Layout);
            0
        }
        WM_COMMAND => {
            let action = match (wparam & 0xffff, (wparam >> 16) as u32) {
                (30, BN_CLICKED) => Some(UiAction::Previous),
                (31, BN_CLICKED) => Some(UiAction::Next),
                (2, BN_CLICKED) => Some(UiAction::Close),
                // Query and checkbox edits are applied only by explicit find actions.
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
    label: HWND,
    query: HWND,
    case: HWND,
    previous: HWND,
    next: HWND,
    close: HWND,
    status: HWND,
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
            let class = wide("flowmux.windows.browser-find");
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
                "cannot register page find window"
            );
            let scale = GetDpiForWindow(parent).max(96) as f64 / 96.0;
            let window = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Find in page").as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                (580.0 * scale).round() as i32,
                (190.0 * scale).round() as i32,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create page find window");
            // Own the HWND before creating children so an error destroys all controls.
            let mut panel = Self {
                window,
                label: std::ptr::null_mut(),
                query: std::ptr::null_mut(),
                case: std::ptr::null_mut(),
                previous: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                message: RefCell::new(String::new()),
            };
            panel.label = panel.child("STATIC", "Find:", 9, SS_NOPREFIX)?;
            panel.query = panel.child(
                "EDIT",
                "",
                10,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            SendMessageW(
                panel.query,
                EM_LIMITTEXT,
                crate::browser_find::MAX_QUERY_BYTES,
                0,
            );
            panel.case = panel.child(
                "BUTTON",
                "Match case",
                11,
                WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            )?;
            panel.previous = panel.child("BUTTON", "Previous", 30, WS_TABSTOP)?;
            panel.next = panel.child("BUTTON", "Next", 31, WS_TABSTOP)?;
            panel.close = panel.child("BUTTON", "Close", 2, WS_TABSTOP)?;
            panel.status = panel.child("STATIC", "", 12, SS_NOPREFIX)?;
            panel.layout();
            panel.status("Enter text, then choose Next or Previous.");
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
            let px = |value: i32| (value as f64 * scale).round() as i32;
            for (handle, x, y, width, height) in [
                (self.label, px(12), px(17), px(44), px(22)),
                (
                    self.query,
                    px(60),
                    px(12),
                    (rect.right - px(72)).max(1),
                    px(28),
                ),
                (self.case, px(12), px(50), px(112), px(28)),
                (self.previous, rect.right - px(304), px(50), px(92), px(28)),
                (self.next, rect.right - px(204), px(50), px(92), px(28)),
                (self.close, rect.right - px(104), px(50), px(92), px(28)),
                (
                    self.status,
                    px(12),
                    px(90),
                    (rect.right - px(24)).max(1),
                    (rect.bottom - px(102)).max(1),
                ),
            ] {
                if !handle.is_null() {
                    SetWindowPos(
                        handle,
                        std::ptr::null_mut(),
                        x,
                        y,
                        width,
                        height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
        }
    }

    pub(super) fn show(&self, background: bool) {
        // A background request must not affect visibility, activation, or focus.
        if !background {
            unsafe {
                ShowWindow(self.window, SW_SHOW);
                SetFocus(self.query);
            }
        }
    }

    pub(super) fn hide(&self) {
        unsafe {
            ShowWindow(self.window, SW_HIDE);
        }
    }

    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if IsWindowVisible(self.window) == 0
                || (message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0)
            {
                return false;
            }
            // IsDialogMessageW interprets Enter/Escape as dialog commands. Keep all
            // query input except Tab on the native EDIT/IME path, including key-up,
            // character and composition messages. Enter does not find and Escape
            // does not close while editing; the explicit buttons remain available.
            if GetFocus() == self.query
                && !(message.message == WM_KEYDOWN && message.wParam == VK_TAB as usize)
            {
                return false;
            }
            IsDialogMessageW(self.window, message) != 0
        }
    }

    pub(super) fn query(&self) -> String {
        self.control_text(self.query)
    }

    pub(super) fn case_sensitive(&self) -> bool {
        unsafe { SendMessageW(self.case, BM_GETCHECK, 0, 0) == 1 }
    }

    pub(super) fn set_query(&self, text: &str, case_sensitive: bool) {
        unsafe {
            SetWindowTextW(self.query, wide(text).as_ptr());
            SendMessageW(self.case, BM_SETCHECK, usize::from(case_sensitive), 0);
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
        self.control_text(self.status)
    }

    pub(super) fn query_handle(&self) -> HWND {
        self.query
    }

    fn control_text(&self, handle: HWND) -> String {
        unsafe {
            let mut text = vec![0u16; GetWindowTextLengthW(handle).max(0) as usize + 1];
            let length = GetWindowTextW(handle, text.as_mut_ptr(), text.len() as i32);
            String::from_utf16_lossy(&text[..length.max(0) as usize])
        }
    }
}
