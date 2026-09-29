// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned metadata dialog; native UTF-16 input and stable targets preserve edits.
use super::*;
use std::cell::Cell;
use windows_sys::Win32::System::SystemServices::SS_NOPREFIX;
use windows_sys::Win32::UI::{
    Controls::{EM_LIMITTEXT, EM_SETSEL},
    Input::KeyboardAndMouse::EnableWindow,
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};
thread_local! { static COMPOSING: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static SETTLING: Cell<bool> = const { Cell::new(false) }; }
thread_local! { static EDITS: RefCell<HashMap<isize, Uuid>> = RefCell::new(HashMap::new()); }

#[derive(Clone, Copy)]
pub(crate) enum EditAction {
    Apply,
    Close,
    Layout,
    Changed,
    Pick,
}
fn emit(window: HWND, action: EditAction) {
    if matches!(action, EditAction::Apply | EditAction::Pick) && COMPOSING.with(Cell::get) {
        return;
    }
    if let Some(id) = EDITS.with(|edits| edits.borrow().get(&(window as isize)).copied()) {
        post(Event::Metadata(id, action));
    }
}
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            emit(window, EditAction::Close);
            0
        }
        WM_SIZE => {
            emit(window, EditAction::Layout);
            0
        }
        WM_DPICHANGED if lparam != 0 => {
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
            emit(window, EditAction::Layout);
            0
        }
        WM_COMMAND if wparam >> 16 == BN_CLICKED as usize => {
            if lparam == 0
                || GetParent(lparam as HWND) != window
                || IsWindowEnabled(lparam as HWND) == 0
            {
                return 0;
            }
            match wparam & 0xffff {
                1 => emit(window, EditAction::Apply),
                2 => emit(window, EditAction::Close),
                3 | 4 => emit(window, EditAction::Pick),
                _ => {}
            }
            0
        }
        WM_COMMAND if wparam & 0xffff == 11 && wparam >> 16 == EN_CHANGE as usize => {
            emit(window, EditAction::Changed);
            0
        }
        _ => chrome::message(window, message, wparam, lparam)
            .unwrap_or_else(|| DefWindowProcW(window, message, wparam, lparam)),
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
        WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION => {
            COMPOSING.with(|c| c.set(message == WM_IME_STARTCOMPOSITION));
            SETTLING.with(|c| c.set(true));
        }
        WM_KEYDOWN if wparam == 229 => SETTLING.with(|c| c.set(true)),
        WM_KEYUP if !matches!(wparam, 0x10..=0x12 | 0xa0..=0xa5) => {
            SETTLING.with(|c| c.set(false));
        }
        WM_KILLFOCUS => {
            COMPOSING.with(|c| c.set(false));
            SETTLING.with(|c| c.set(false));
        }
        WM_NCDESTROY => {
            COMPOSING.with(|c| c.set(false));
            SETTLING.with(|c| c.set(false));
            RemoveWindowSubclass(window, Some(edit_procedure), subclass);
        }
        _ => {}
    }
    DefSubclassProc(window, message, wparam, lparam)
}

pub(crate) struct Panel {
    pub(crate) edit_id: Uuid,
    pub(super) window: HWND,
    pub(super) owner: HWND,
    input: HWND,
    hint: HWND,
    error: HWND,
    apply: HWND,
    close: HWND,
    swatch: HWND,
    picker: HWND,
    custom: [COLORREF; 16],
    open: Cell<bool>,
    owner_disabled: Cell<bool>,
    background: bool,
    pub(super) target: Option<EditTarget>,
    pub(super) original: String,
    pub(super) original_locked: bool,
}
impl Drop for Panel {
    fn drop(&mut self) {
        self.hide();
        EDITS.with(|edits| edits.borrow_mut().remove(&(self.window as isize)));
        for window in [
            self.input,
            self.hint,
            self.error,
            self.apply,
            self.close,
            self.swatch,
            self.picker,
        ] {
            if !window.is_null() {
                chrome::unregister(window);
            }
        }
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
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register name editor"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT | WS_EX_DLGMODALFRAME,
                class.as_ptr(),
                wide("Edit name").as_ptr(),
                WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                460,
                220,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create name editor");
            let mut panel = Self {
                edit_id: Uuid::nil(),
                window,
                owner: parent,
                input: std::ptr::null_mut(),
                hint: std::ptr::null_mut(),
                error: std::ptr::null_mut(),
                apply: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                swatch: std::ptr::null_mut(),
                picker: std::ptr::null_mut(),
                custom: [0; 16],
                open: Cell::new(false),
                owner_disabled: Cell::new(false),
                background: true,
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
            panel.apply = panel.child("BUTTON", "OK", 1, WS_TABSTOP | BS_OWNERDRAW as u32)?;
            panel.close = panel.child("BUTTON", "Cancel", 2, WS_TABSTOP | BS_OWNERDRAW as u32)?;
            panel.swatch = panel.child(
                "BUTTON",
                "Color preview",
                3,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            chrome::register_swatch(panel.swatch, chrome::palette().background);
            panel.picker = panel.child("BUTTON", "Choose…", 4, WS_TABSTOP | BS_OWNERDRAW as u32)?;
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
            if class == "BUTTON" {
                chrome::register_button(hwnd, chrome::Role::Button);
            } else {
                chrome::register_control(
                    hwnd,
                    if class == "EDIT" {
                        chrome::ControlRole::Edit
                    } else {
                        chrome::ControlRole::Static
                    },
                );
            }
            Ok(hwnd)
        }
    }
    pub(super) fn edit(&mut self, target: EditTarget, value: &str, locked: bool, background: bool) {
        self.edit_id = Uuid::new_v4();
        EDITS.with(|edits| {
            edits
                .borrow_mut()
                .insert(self.window as isize, self.edit_id)
        });
        COMPOSING.with(|composing| composing.set(false));
        SETTLING.with(|settling| settling.set(false));
        self.background = background;
        self.target = Some(target);
        self.original = value.to_owned();
        self.original_locked = locked;
        let (title, hint) = match target {
            EditTarget::WorkspaceName(_) => (
                "Rename workspace",
                "Workspace name (leave empty for automatic naming)",
            ),
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
            let dpi = GetDpiForWindow(self.owner).max(96) as i32;
            let mut owner = RECT::default();
            GetWindowRect(self.owner, &mut owner);
            let width = 460 * dpi / 96;
            let height = 220 * dpi / 96;
            SetWindowPos(
                self.window,
                std::ptr::null_mut(),
                owner.left + (owner.right - owner.left - width) / 2,
                owner.top + (owner.bottom - owner.top - height) / 2,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            self.open.set(true);
            if !self.owner.is_null() && IsWindowEnabled(self.owner) != 0 {
                EnableWindow(self.owner, 0);
                self.owner_disabled.set(true);
            }
            self.status("");
            self.layout();
            self.preview();
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
            let color = matches!(self.target, Some(EditTarget::WorkspaceColor(_)));
            ShowWindow(self.swatch, if color { SW_SHOWNOACTIVATE } else { SW_HIDE });
            ShowWindow(self.picker, if color { SW_SHOWNOACTIVATE } else { SW_HIDE });
            for (hwnd, x, y, w, h) in [
                (self.hint, px(12), px(12), (r.right - px(24)).max(1), px(24)),
                (
                    self.input,
                    px(12),
                    px(40),
                    (r.right - px(if color { 156 } else { 24 })).max(1),
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
                    r.right - px(110),
                    r.bottom - px(36),
                    px(98),
                    px(28),
                ),
                (
                    self.close,
                    r.right - px(220),
                    r.bottom - px(36),
                    px(98),
                    px(28),
                ),
                (self.swatch, r.right - px(140), px(40), px(28), px(28)),
                (self.picker, r.right - px(104), px(40), px(92), px(28)),
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
    pub(super) fn is_open(&self) -> bool {
        self.open.get()
    }
    pub(super) fn composing(&self) -> bool {
        COMPOSING.with(Cell::get)
    }
    pub(super) fn preview(&self) {
        if !matches!(self.target, Some(EditTarget::WorkspaceColor(_))) {
            return;
        }
        if let Ok(color) = model::parse_color(&self.value()) {
            let color = color
                .and_then(|value| u32::from_str_radix(&value[1..], 16).ok())
                .map(|rgb| ((rgb & 255) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 255))
                .unwrap_or(chrome::palette().background);
            chrome::set_swatch(self.swatch, color);
        }
    }
    pub(super) fn choose_color(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(self.target, Some(EditTarget::WorkspaceColor(_))),
            "no color target"
        );
        if self.background {
            self.status("The native color dialog is disabled during hidden verification; use the hex field.");
            return Ok(());
        }
        let color = model::parse_color(&self.value())?.unwrap_or_else(|| "#ffffff".into());
        let rgb = u32::from_str_radix(&color[1..], 16)?;
        let initial = ((rgb & 255) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 255);
        if let Some(color) =
            chrome::choose_color(self.window, initial, &mut self.custom, self.background)?
        {
            let value = format!(
                "#{:02x}{:02x}{:02x}",
                color & 255,
                (color >> 8) & 255,
                (color >> 16) & 255
            );
            unsafe {
                SetWindowTextW(self.input, wide(value).as_ptr());
            }
            self.preview();
            self.status("");
        }
        Ok(())
    }
    pub(crate) fn diagnostics(&self) -> Value {
        let mut text = [0u16; 2048];
        let len = unsafe { GetWindowTextW(self.error, text.as_mut_ptr(), text.len() as i32) }.max(0)
            as usize;
        json!({"open":self.open.get(),"window":self.window as usize,"owner":self.owner as usize,
            "input":self.input as usize,"apply":self.apply as usize,"cancel":self.close as usize,
            "swatch":self.swatch as usize,"picker":self.picker as usize,"error":String::from_utf16_lossy(&text[..len]),
            "edit_id":self.edit_id,"native_visible":unsafe{IsWindowVisible(self.window)!=0},"composing":self.composing(),"settling":SETTLING.with(Cell::get)})
    }
    pub(crate) fn hide(&self) {
        self.open.set(false);
        COMPOSING.with(|composing| composing.set(false));
        SETTLING.with(|settling| settling.set(false));
        unsafe {
            ShowWindow(self.window, SW_HIDE);
            if self.owner_disabled.replace(false) && IsWindow(self.owner) != 0 {
                EnableWindow(self.owner, 1);
            }
        }
    }
    pub(crate) fn handle_message(&self, message: &MSG) -> bool {
        if !self.open.get()
            || (message.hwnd != self.window && unsafe { IsChild(self.window, message.hwnd) } == 0)
        {
            return false;
        }
        // The commit/cancel key still belongs to the IME after composition ends.
        // Once released, ordinary Enter/Escape apply/cancel as on Linux.
        if self.composing() || SETTLING.with(Cell::get) || message.wParam == 229 {
            return false;
        }
        if message.message == WM_KEYDOWN && matches!(message.wParam, 13 | 27) {
            if message.lParam as usize & (1 << 30) == 0 {
                emit(
                    self.window,
                    if message.wParam == 27 || message.hwnd == self.close {
                        EditAction::Close
                    } else if message.hwnd == self.picker || message.hwnd == self.swatch {
                        EditAction::Pick
                    } else {
                        EditAction::Apply
                    },
                );
            }
            return true;
        }
        if message.message == WM_CHAR && matches!(message.wParam, 13 | 27) {
            return true;
        }
        if self.background {
            return matches!(message.message, WM_KEYDOWN | WM_KEYUP | WM_CHAR)
                && message.wParam == 9;
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
            // Only the composition-ending key is left with the native EDIT.
            let message = MSG {
                hwnd: panel.input,
                message: WM_KEYDOWN,
                wParam: 13,
                ..MSG::default()
            };
            assert!(panel.handle_message(&message));
            SendMessageW(panel.input, WM_IME_STARTCOMPOSITION, 0, 0);
            assert!(!panel.handle_message(&message));
            SendMessageW(panel.input, WM_IME_ENDCOMPOSITION, 0, 0);
            assert!(!panel.handle_message(&message));
            SendMessageW(panel.input, WM_KEYUP, 0x10, 0);
            assert!(!panel.handle_message(&message));
            SendMessageW(panel.input, WM_KEYUP, 13, 0);
            assert!(panel.handle_message(&message));
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
