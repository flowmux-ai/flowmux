// SPDX-License-Identifier: GPL-3.0-or-later
//! Modeless native UTF-16 edit control. A pending edit is attached to a stable ID.
use super::*;
use std::cell::Cell;
use windows_sys::Win32::System::SystemServices::SS_NOPREFIX;
use windows_sys::Win32::UI::{
    Controls::{EM_LIMITTEXT, EM_SETSEL},
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};
thread_local! { static COMPOSING: Cell<bool> = const { Cell::new(false) }; }

#[derive(Clone, Copy)]
pub(crate) enum EditAction {
    Apply,
    Close,
    Layout,
}
fn emit(action: EditAction) {
    post(Event::Metadata(action));
}
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            emit(EditAction::Close);
            0
        }
        WM_SIZE => {
            emit(EditAction::Layout);
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
            emit(EditAction::Layout);
            0
        }
        WM_COMMAND if wparam >> 16 == BN_CLICKED as usize => {
            match wparam & 0xffff {
                1 => emit(EditAction::Apply),
                2 => emit(EditAction::Close),
                _ => {}
            }
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}
unsafe extern "system" fn edit_procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    _: usize,
) -> LRESULT {
    match message {
        WM_IME_STARTCOMPOSITION => COMPOSING.with(|c| c.set(true)),
        WM_IME_ENDCOMPOSITION | WM_KILLFOCUS => COMPOSING.with(|c| c.set(false)),
        WM_NCDESTROY => {
            COMPOSING.with(|c| c.set(false));
            RemoveWindowSubclass(window, Some(edit_procedure), subclass);
        }
        _ => {}
    }
    DefSubclassProc(window, message, wparam, lparam)
}

pub(crate) struct Panel {
    pub(crate) edit_id: Uuid,
    window: HWND,
    input: HWND,
    hint: HWND,
    error: HWND,
    apply: HWND,
    close: HWND,
    pub(super) target: Option<EditTarget>,
    pub(super) original: String,
    pub(super) original_locked: bool,
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
            let class = wide("flowmux.windows.metadata");
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
                "cannot register name editor"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Edit name").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                560,
                240,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create name editor");
            let mut panel = Self {
                edit_id: Uuid::nil(),
                window,
                input: std::ptr::null_mut(),
                hint: std::ptr::null_mut(),
                error: std::ptr::null_mut(),
                apply: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                target: None,
                original: String::new(),
                original_locked: false,
            };
            panel.hint = panel.child("STATIC", "", 10, 0)?;
            panel.input = panel.child(
                "EDIT",
                "",
                11,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            SendMessageW(panel.input, EM_LIMITTEXT, 256, 0);
            checked(SetWindowSubclass(panel.input, Some(edit_procedure), 1, 0))?;
            panel.error = panel.child("STATIC", "", 12, SS_NOPREFIX)?;
            panel.apply =
                panel.child("BUTTON", "Apply", 1, WS_TABSTOP | BS_DEFPUSHBUTTON as u32)?;
            panel.close = panel.child("BUTTON", "Cancel", 2, WS_TABSTOP)?;
            panel.layout();
            Ok(panel)
        }
    }
    fn child(&self, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
        unsafe {
            let hwnd = CreateWindowExW(
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
            checked((!hwnd.is_null()) as i32)?;
            SendMessageW(
                hwnd,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as WPARAM,
                1,
            );
            Ok(hwnd)
        }
    }
    pub(super) fn edit(&mut self, target: EditTarget, value: &str, locked: bool, background: bool) {
        self.edit_id = Uuid::new_v4();
        self.target = Some(target);
        self.original = value.to_owned();
        self.original_locked = locked;
        let (title, hint) = match target {
            EditTarget::Setting(key) => match key {
                crate::settings::SettingKey::FontFamily => (
                    "Terminal font",
                    "Font family / fallback list (installed fonts)",
                ),
                crate::settings::SettingKey::FontSize => {
                    ("Terminal font size", "Font size in pixels, 6–72")
                }
                crate::settings::SettingKey::Scrollback => (
                    "Terminal scrollback",
                    "0–100000 lines; lowering discards the oldest history",
                ),
                _ => ("Terminal setting", "Value"),
            },
            EditTarget::WorkspaceName(_) => ("Rename workspace", "Workspace name"),
            EditTarget::WorkspaceColor(_) => {
                ("Workspace color", "Color as #RRGGBB; leave empty to clear")
            }
            EditTarget::TabName(_) => (
                "Rename tab",
                "Tab name (retained when the shell updates its title)",
            ),
        };
        unsafe {
            SetWindowTextW(self.window, wide(title).as_ptr());
            SetWindowTextW(self.hint, wide(hint).as_ptr());
            SetWindowTextW(self.input, wide(value).as_ptr());
            SendMessageW(self.input, EM_SETSEL, 0, -1);
            self.status("");
            if !background {
                ShowWindow(self.window, SW_SHOW);
                SetFocus(self.input);
            }
        }
    }
    pub(super) fn layout(&self) {
        unsafe {
            let mut r = RECT::default();
            GetClientRect(self.window, &mut r);
            let scale = GetDpiForWindow(self.window).max(96) as f64 / 96.0;
            let px = |v: i32| (v as f64 * scale).round() as i32;
            for (hwnd, x, y, w, h) in [
                (self.hint, px(12), px(12), (r.right - px(24)).max(1), px(24)),
                (
                    self.input,
                    px(12),
                    px(40),
                    (r.right - px(24)).max(1),
                    px(28),
                ),
                (
                    self.error,
                    px(12),
                    px(78),
                    (r.right - px(24)).max(1),
                    (r.bottom - px(124)).max(1),
                ),
                (
                    self.apply,
                    r.right - px(220),
                    r.bottom - px(36),
                    px(98),
                    px(28),
                ),
                (
                    self.close,
                    r.right - px(110),
                    r.bottom - px(36),
                    px(98),
                    px(28),
                ),
            ] {
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
        }
    }
    pub(crate) fn status(&self, text: &str) {
        unsafe {
            SetWindowTextW(self.error, wide(text).as_ptr());
        }
    }
    pub(super) fn value(&self) -> String {
        unsafe {
            let mut data = vec![0u16; GetWindowTextLengthW(self.input) as usize + 1];
            let n = GetWindowTextW(self.input, data.as_mut_ptr(), data.len() as i32);
            String::from_utf16_lossy(&data[..n.max(0) as usize])
        }
    }
    pub(crate) fn hide(&self) {
        unsafe {
            ShowWindow(self.window, SW_HIDE);
        }
    }
    pub(crate) fn handle_message(&self, message: &MSG) -> bool {
        // Text production belongs to the native EDIT/IME. Enter/Escape inside the
        // edit never applies/closes the panel; Tab to Apply/Cancel also works.
        if COMPOSING.with(Cell::get)
            || message.wParam == 229
            || (message.hwnd == self.input
                && matches!(message.message, WM_KEYDOWN | WM_KEYUP | WM_CHAR)
                && matches!(message.wParam, 13 | 27))
        {
            return false;
        }
        unsafe {
            IsWindowVisible(self.window) != 0
                && (message.hwnd == self.window || IsChild(self.window, message.hwnd) != 0)
                && IsDialogMessageW(self.window, message) != 0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hidden_native_edit_keeps_unicode_and_never_shows_or_focuses_a_window() {
        let mut panel = Panel::new(std::ptr::null_mut()).unwrap();
        let value = "한글 한 e\u{301} 😀 & && 이름";
        let id = WorkspaceId::new();
        panel.edit(EditTarget::WorkspaceName(id), value, false, true);
        assert_eq!(panel.value(), value);
        assert_eq!(panel.original, value);
        assert!(matches!(panel.target,Some(EditTarget::WorkspaceName(actual)) if actual == id));
        unsafe {
            assert_eq!(IsWindowVisible(panel.window), 0);
            assert_ne!(GetForegroundWindow(), panel.window);
            // A literal Enter in the EDIT is left with its IME/text handler.
            let message = MSG {
                hwnd: panel.input,
                message: WM_KEYDOWN,
                wParam: 13,
                ..MSG::default()
            };
            assert!(!panel.handle_message(&message));
        }
        panel.edit(EditTarget::WorkspaceColor(id), "#12abef", false, true);
        assert_eq!(panel.value(), "#12abef");
        panel.status("한글 오류 & 확인");
        unsafe {
            let mut value = [0u16; 128];
            let len = GetWindowTextW(panel.error, value.as_mut_ptr(), value.len() as i32);
            assert_eq!(
                String::from_utf16_lossy(&value[..len as usize]),
                "한글 오류 & 확인"
            );
        }
        panel.hide();
    }
}
