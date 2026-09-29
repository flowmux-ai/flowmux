// SPDX-License-Identifier: GPL-3.0-or-later
//! Native child controls; address navigation preserves native EDIT composition.
use super::super::chrome as shell_chrome;
use super::*;
use windows_sys::Win32::UI::{
    Controls::EM_SETLIMITTEXT,
    Input::KeyboardAndMouse::{EnableWindow, GetFocus, GetKeyState, IsWindowEnabled},
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};
const ADDRESS_SUBCLASS: usize = 0x464d_4241;
#[derive(Clone, Copy, Default)]
struct AddressKeys {
    composing: bool,
    settling: bool,
    modifiers: u16,
    enter_down: bool,
    blurred: bool,
}
impl AddressKeys {
    fn update(&mut self, message: u32, w: WPARAM, l: LPARAM) -> (bool, bool) {
        match message {
            WM_IME_STARTCOMPOSITION => {
                self.composing = true;
                self.settling = true;
            }
            WM_IME_ENDCOMPOSITION => {
                self.composing = false;
                self.settling = true;
            }
            WM_SETFOCUS => self.blurred = false,
            WM_KILLFOCUS => {
                self.composing = false;
                self.modifiers = 0;
                self.enter_down = false;
                self.blurred = true;
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP => {
                let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
                if let Some(bit) = crate::keybindings::native_modifier(w, l) {
                    if down {
                        self.modifiers |= bit;
                    } else {
                        self.modifiers &= !bit;
                    }
                } else if !down {
                    self.settling = false;
                }
                if down && w == 229 {
                    self.settling = true;
                }
                if w == 13 {
                    let repeated = self.enter_down || l as usize & (1 << 30) != 0;
                    self.enter_down = down;
                    let plain = down
                        && message == WM_KEYDOWN
                        && self.modifiers == 0
                        && l as usize & (1 << 29) == 0
                        && !self.composing
                        && !self.settling
                        && !self.blurred;
                    return (plain, repeated);
                }
            }
            _ => {}
        }
        (false, false)
    }
}
thread_local! {
    static OWNERS: RefCell<HashMap<isize, SurfaceId>> = RefCell::new(HashMap::new());
    static ADDRESS_KEYS: RefCell<HashMap<isize, AddressKeys>> = RefCell::new(HashMap::new());
}
unsafe extern "system" fn address_proc(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    ADDRESS_KEYS.with(|keys| {
        if let Some(keys) = keys.borrow_mut().get_mut(&(window as isize)) {
            keys.update(message, w, l);
        }
    });
    if message == WM_NCDESTROY {
        ADDRESS_KEYS.with(|keys| keys.borrow_mut().remove(&(window as isize)));
        RemoveWindowSubclass(window, Some(address_proc), id);
    }
    DefSubclassProc(window, message, w, l)
}

pub(super) fn handle_message(message: &MSG) -> bool {
    if !matches!(
        message.message,
        WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
    ) {
        return false;
    }
    let state = ADDRESS_KEYS.with(|keys| {
        keys.borrow_mut()
            .get_mut(&(message.hwnd as isize))
            .map(|keys| keys.update(message.message, message.wParam, message.lParam))
    });
    let Some((true, repeated)) = state else {
        return false;
    };
    unsafe {
        let root = GetAncestor(message.hwnd, GA_ROOT);
        if IsWindowEnabled(message.hwnd) == 0 || IsWindowEnabled(root) == 0 {
            return false;
        }
        // A modifier may already be held when the user clicks the address EDIT.
        // Inspect this UI thread's queued key state only for the focused visible
        // control; background probes rely entirely on their owned key messages.
        if GetFocus() == message.hwnd
            && IsWindowVisible(root) != 0
            && [0x10, 0x11, 0x12, 0x5b, 0x5c]
                .into_iter()
                .any(|key| GetKeyState(key) < 0)
        {
            return false;
        }
    }
    let owner = unsafe { GetParent(message.hwnd) };
    let surface = OWNERS.with(|map| map.borrow().get(&(owner as isize)).copied());
    let Some(surface) = surface else {
        return false;
    };
    if !repeated {
        post(Event::Browser(browser::Signal::Ui(surface, 5)));
    }
    // Consume before TranslateMessage: no generated WM_CHAR, and IME/modified
    // keys continue through the complete native EDIT message path above.
    true
}
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
    bookmarks: HWND,
    displayed_url: String,
    loading: bool,
    navigation_visible: bool,
    buttons: Vec<HWND>,
}
impl Drop for Chrome {
    fn drop(&mut self) {
        OWNERS.with(|map| map.borrow_mut().remove(&(self.window as isize)));
        ADDRESS_KEYS.with(|keys| keys.borrow_mut().remove(&(self.address as isize)));
        unsafe {
            RemoveWindowSubclass(self.address, Some(address_proc), ADDRESS_SUBCLASS);
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
                bookmarks: std::ptr::null_mut(),
                displayed_url: "about:blank".into(),
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
            ADDRESS_KEYS.with(|keys| {
                keys.borrow_mut()
                    .insert(chrome.address as isize, AddressKeys::default())
            });
            checked(SetWindowSubclass(
                chrome.address,
                Some(address_proc),
                ADDRESS_SUBCLASS,
                0,
            ))?;
            chrome.status = chrome.child("STATIC", "", 0, 21)?;
            chrome.more = chrome.child(
                "BUTTON",
                "Browser tools",
                WS_TABSTOP | BS_PUSHBUTTON as u32,
                11,
            )?;
            chrome.bookmarks =
                chrome.child("BUTTON", "Bookmarks", WS_TABSTOP | BS_OWNERDRAW as u32, 12)?;
            for (button, kind) in [
                (chrome.bookmarks, shell_chrome::ChromeIcon::Bookmarks),
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
    pub(super) fn visible(&self, visible: bool, background: bool) {
        unsafe {
            // Keep the entire native toolbar hidden in background verification;
            // child control geometry and text remain available to owned probes.
            ShowWindow(
                self.window,
                if visible && !background {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
        }
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
            let bookmarks = area.width >= px(232);
            for (i, button) in self.buttons.iter().enumerate() {
                let shown = match i {
                    0 | 1 => full,
                    2 => self.navigation_visible && !self.loading,
                    3 => self.navigation_visible && self.loading,
                    4 => go,
                    _ => false,
                };
                let x = if i == 4 {
                    area.width - px(if bookmarks { 100 } else { 68 })
                } else {
                    left
                };
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
                (area.width
                    - left
                    - px(if bookmarks {
                        104
                    } else if go {
                        72
                    } else {
                        38
                    }))
                .max(1),
                px(28),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            ShowWindow(self.bookmarks, if bookmarks { SW_SHOWNA } else { SW_HIDE });
            SetWindowPos(
                self.bookmarks,
                std::ptr::null_mut(),
                (area.width - px(68)).max(0),
                px(4),
                px(30),
                px(30),
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
    pub(super) fn bookmarks_anchor(&self) -> (i32, i32) {
        let mut r = RECT::default();
        unsafe {
            GetWindowRect(
                if GetWindowLongPtrW(self.bookmarks, GWL_STYLE) as u32 & WS_VISIBLE != 0 {
                    self.bookmarks
                } else {
                    self.more
                },
                &mut r,
            );
        }
        (r.left, r.bottom)
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
        json!({"parent":unsafe{GetParent(self.window)} as usize,"height":r.bottom,"rows":1,"tools_handle":self.more as usize,"bookmarks_handle":self.bookmarks as usize,
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
            let guarded = ADDRESS_KEYS.with(|keys| {
                keys.borrow()
                    .get(&(self.address as isize))
                    .is_some_and(|keys| keys.composing || keys.settling)
            });
            // Metadata ticks must not erase a draft when its tab loses focus.
            // Only a changed page URL replaces it, after composition has settled.
            if GetFocus() != self.address
                && !guarded
                && self.displayed_url != url
                && SetWindowTextW(self.address, wide(url).as_ptr()) != 0
            {
                self.displayed_url = url.to_string();
            }
            EnableWindow(self.buttons[0], back as i32);
            EnableWindow(self.buttons[1], forward as i32);
            SetWindowTextW(self.status, wide(status.replace('&', "&&")).as_ptr());
        }
    }
}
