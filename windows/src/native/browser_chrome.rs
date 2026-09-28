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
    more: HWND,
    loading: bool,
    navigation_visible: bool,
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
                more: std::ptr::null_mut(),
                loading: false,
                navigation_visible: false,
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
            chrome.more = chrome.child(
                "BUTTON",
                "Browser tools",
                WS_TABSTOP | BS_PUSHBUTTON as u32,
                11,
            )?;
            for (button, kind) in [
                (chrome.buttons[0], shell_chrome::ChromeIcon::Back),
                (chrome.buttons[1], shell_chrome::ChromeIcon::Forward),
                (chrome.buttons[2], shell_chrome::ChromeIcon::Reload),
                (chrome.buttons[3], shell_chrome::ChromeIcon::Stop),
                (chrome.buttons[4], shell_chrome::ChromeIcon::Forward),
                (chrome.more, shell_chrome::ChromeIcon::More),
            ] {
                shell_chrome::set_role(
                    button,
                    shell_chrome::Role::Icon {
                        kind,
                        marked: false,
                    },
                );
            }
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
        (40.0 * scale).round() as i32
    }
    pub(super) fn layout(&mut self, area: model::Rect, scale: f64) {
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
            let mut left = px(4);
            let full = area.width >= px(250);
            self.navigation_visible = area.width >= px(104);
            let go = area.width >= px(168);
            for (i, button) in self.buttons.iter().enumerate() {
                let shown = match i {
                    0 | 1 => full,
                    2 => self.navigation_visible && !self.loading,
                    3 => self.navigation_visible && self.loading,
                    4 => go,
                    _ => false,
                };
                let x = if i == 4 { area.width - px(68) } else { left };
                // Both Reload and Stop keep current bounds while hidden. Metadata
                // only switches visibility, so a stale slot cannot shrink a button.
                if shown || matches!(i, 2 | 3) {
                    SetWindowPos(
                        *button,
                        std::ptr::null_mut(),
                        x.max(0),
                        px(4),
                        px(30),
                        px(30),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                ShowWindow(*button, if shown { SW_SHOWNA } else { SW_HIDE });
                if (i < 2 && full) || (i == 3 && self.navigation_visible) {
                    left += px(32);
                }
            }
            ShowWindow(
                self.address,
                if area.width >= px(50) {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
            SetWindowPos(
                self.address,
                std::ptr::null_mut(),
                left,
                px(5),
                (area.width - left - px(if go { 72 } else { 38 })).max(1),
                px(28),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let tool_width = px(30).min((area.width - px(8)).max(1));
            ShowWindow(
                self.more,
                if area.width >= px(10) {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
            SetWindowPos(
                self.more,
                std::ptr::null_mut(),
                (area.width - px(4) - tool_width).max(0),
                px(4),
                tool_width,
                px(30),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            // Status is available through the tools caption/tooltip and native diagnostics.
            ShowWindow(self.status, SW_HIDE);
        }
    }
    pub(super) fn tools_anchor(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            GetWindowRect(self.more, &mut r);
        }
        (r.left, r.bottom)
    }
    pub(super) fn diagnostics(&self) -> Value {
        let mut r = RECT::default();
        unsafe {
            GetClientRect(self.window, &mut r);
        }
        json!({"height":r.bottom,"rows":1,"tools_handle":self.more as usize,
            "status_handle":self.status as usize,"controls":self.buttons.iter().map(|h|*h as usize).collect::<Vec<_>>()})
    }
    pub(super) fn address(&self) -> String {
        unsafe {
            let mut value = vec![0u16; crate::browser::MAX_URL_BYTES + 1];
            let n = GetWindowTextW(self.address, value.as_mut_ptr(), value.len() as i32);
            String::from_utf16_lossy(&value[..n.max(0) as usize])
        }
    }
    pub(super) fn update(
        &mut self,
        url: &str,
        back: bool,
        forward: bool,
        status: &str,
        loading: bool,
    ) {
        self.loading = loading;
        unsafe {
            ShowWindow(
                self.buttons[2],
                if self.navigation_visible && !loading {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
            ShowWindow(
                self.buttons[3],
                if self.navigation_visible && loading {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
            SetWindowTextW(
                self.more,
                wide(format!("Browser tools — {status}")).as_ptr(),
            );
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
