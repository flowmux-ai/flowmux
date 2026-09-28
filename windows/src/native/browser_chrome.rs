// SPDX-License-Identifier: GPL-3.0-or-later
//! Native child controls; never create a desktop window or intercept IME Enter.
use super::super::chrome as shell_chrome;
use super::*;
use windows_sys::Win32::UI::{
    Controls::EM_SETLIMITTEXT,
    Input::KeyboardAndMouse::{EnableWindow, GetFocus},
};
thread_local! { static OWNERS: RefCell<HashMap<isize, SurfaceId>> = RefCell::new(HashMap::new()); }
unsafe extern "system" fn procedure(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(result) = shell_chrome::message(hwnd, msg, w, l) {
        return result;
    }
    if msg == WM_COMMAND && (w >> 16) == 0 {
        if let Some(id) = OWNERS.with(|map| map.borrow().get(&(hwnd as isize)).copied()) {
            post(Event::Browser(browser::Signal::Ui(id, w as u16)));
        }
        return 0;
    }
    DefWindowProcW(hwnd, msg, w, l)
}
pub(super) struct Chrome {
    pub(super) window: HWND,
    pub(super) address: HWND,
    status: HWND,
    find: HWND,
    buttons: Vec<HWND>,
}
impl Drop for Chrome {
    fn drop(&mut self) {
        OWNERS.with(|map| map.borrow_mut().remove(&(self.window as isize)));
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Chrome {
    pub(super) fn new(parent: HWND, id: SurfaceId) -> anyhow::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux.windows.browser.chrome");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
                lpszClassName: class.as_ptr(),
                ..WNDCLASSW::default()
            };
            if RegisterClassW(&spec) == 0 {
                anyhow::ensure!(
                    GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                    "cannot register browser chrome"
                );
            }
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("Browser navigation").as_ptr(),
                WS_CHILD | WS_CLIPCHILDREN,
                0,
                0,
                1,
                1,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            checked((!window.is_null()) as i32)?;
            shell_chrome::register_control(window, shell_chrome::ControlRole::Static);
            let mut chrome = Self {
                window,
                address: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                find: std::ptr::null_mut(),
                buttons: vec![],
            };
            OWNERS.with(|map| map.borrow_mut().insert(window as isize, id));
            for (index, label) in [
                "Back",
                "Forward",
                "Reload",
                "Stop",
                "Go",
                "−",
                "+",
                "100%",
                "Downloads",
            ]
            .iter()
            .enumerate()
            {
                chrome.buttons.push(chrome.child(
                    "BUTTON",
                    label,
                    WS_TABSTOP | BS_PUSHBUTTON as u32,
                    index + 1,
                )?);
            }
            chrome.address = chrome.child(
                "EDIT",
                "about:blank",
                WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
                20,
            )?;
            SendMessageW(
                chrome.address,
                EM_SETLIMITTEXT,
                crate::browser::MAX_URL_BYTES,
                0,
            );
            chrome.status = chrome.child("STATIC", "", 0, 21)?;
            chrome.find = chrome.child("BUTTON", "Find", WS_TABSTOP | BS_PUSHBUTTON as u32, 10)?;
            Ok(chrome)
        }
    }
    unsafe fn child(&self, class: &str, text: &str, style: u32, id: usize) -> anyhow::Result<HWND> {
        let hwnd = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD
                | WS_VISIBLE
                | style
                | if class == "BUTTON" {
                    BS_OWNERDRAW as u32
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
        );
        checked((!hwnd.is_null()) as i32)?;
        match class {
            "BUTTON" => shell_chrome::register_button(hwnd, shell_chrome::Role::Button),
            "EDIT" => shell_chrome::register_control(hwnd, shell_chrome::ControlRole::Edit),
            _ => shell_chrome::register_control(hwnd, shell_chrome::ControlRole::Caption),
        }
        Ok(hwnd)
    }
    pub(super) fn height(scale: f64) -> i32 {
        (98.0 * scale).round() as i32
    }
    pub(super) fn layout(&self, area: model::Rect, scale: f64) {
        let px = |v: i32| (v as f64 * scale).round() as i32;
        unsafe {
            SetWindowPos(
                self.window,
                std::ptr::null_mut(),
                area.x,
                area.y,
                area.width,
                Self::height(scale),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let widths = [50, 62, 60, 48, 42, 30, 30, 48, 86];
            let mut x = px(2);
            for (i, button) in self.buttons.iter().enumerate() {
                // Go belongs to the address row; remaining controls stay stable.
                let (left, top, width) = if i == 4 {
                    ((area.width - px(46)).max(0), px(35), px(44))
                } else {
                    (x, px(2), px(widths[i]))
                };
                SetWindowPos(
                    *button,
                    std::ptr::null_mut(),
                    left,
                    top,
                    width,
                    px(29),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                if i != 4 {
                    x += width + px(2);
                }
            }
            SetWindowPos(
                self.address,
                std::ptr::null_mut(),
                px(2),
                px(35),
                (area.width - px(52)).max(1),
                px(28),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            SetWindowPos(
                self.status,
                std::ptr::null_mut(),
                px(2),
                px(68),
                (area.width - px(68)).max(1),
                px(26),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            SetWindowPos(
                self.find,
                std::ptr::null_mut(),
                (area.width - px(64)).max(0),
                px(68),
                px(62),
                px(26),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
    pub(super) fn address(&self) -> String {
        unsafe {
            let mut value = vec![0u16; crate::browser::MAX_URL_BYTES + 1];
            let n = GetWindowTextW(self.address, value.as_mut_ptr(), value.len() as i32);
            String::from_utf16_lossy(&value[..n.max(0) as usize])
        }
    }
    pub(super) fn update(&self, url: &str, back: bool, forward: bool, status: &str) {
        unsafe {
            // Never overwrite live native EDIT composition/typing on a timer.
            if GetFocus() != self.address && self.address() != url {
                SetWindowTextW(self.address, wide(url).as_ptr());
            }
            EnableWindow(self.buttons[0], back as i32);
            EnableWindow(self.buttons[1], forward as i32);
            SetWindowTextW(self.status, wide(status.replace('&', "&&")).as_ptr());
        }
    }
}
