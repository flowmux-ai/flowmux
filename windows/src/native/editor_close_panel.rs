// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned asynchronous dirty-editor decision; the editor barrier owns its lifetime.
use super::super::chrome;
use super::*;
use std::cell::{Cell, RefCell};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetFocus, IsWindowEnabled};

const SAVE: usize = 1;
const CANCEL: usize = 2;
const DISCARD: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Choice {
    Save,
    Discard,
    Cancel,
}

#[derive(Clone)]
struct Route {
    id: u64,
    controls: [isize; 6],
    busy: bool,
    submitted: bool,
}
thread_local! {
    static ROUTES: RefCell<HashMap<isize, Route>> = RefCell::new(HashMap::new());
}

fn choose(window: HWND, choice: Choice) {
    let id = ROUTES.with(|routes| {
        let mut routes = routes.borrow_mut();
        let route = routes.get_mut(&(window as isize))?;
        if route.busy || route.submitted {
            return None;
        }
        route.submitted = true;
        Some(route.id)
    });
    if let Some(id) = id {
        post(Event::Editor(Signal::CloseChoice(id, choice)));
    }
}

fn body_text(labels: &[String]) -> String {
    if labels.len() == 1 {
        return format!("“{}” has unsaved changes.", labels[0]);
    }
    let mut body = format!("{} files have unsaved changes.\r\n\r\n", labels.len());
    for label in labels.iter().take(8) {
        body.push_str(&format!("• {label}\r\n"));
    }
    if labels.len() > 8 {
        body.push_str(&format!("• … and {} more\r\n", labels.len() - 8));
    }
    body.truncate(body.len().saturating_sub(2));
    body
}

fn layout(window: HWND) {
    let route = ROUTES.with(|routes| routes.borrow().get(&(window as isize)).cloned());
    let Some(route) = route else { return };
    unsafe {
        let mut area = RECT::default();
        if GetClientRect(window, &mut area) == 0 {
            return;
        }
        let dpi = GetDpiForWindow(window).max(96) as i32;
        let px = |value: i32| value * dpi / 96;
        let margin = px(20);
        let width = (area.right - margin * 2).max(1);
        let button_y = (area.bottom - px(50)).max(0);
        let status_y = (button_y - px(40)).max(px(60));
        let button_width = ((width - px(20)) / 3).clamp(1, px(96));
        let group_x = (area.right - margin - (button_width * 3 + px(20))).max(0);
        let boxes = [
            (margin, px(16), width, px(32)),
            (margin, px(60), width, (status_y - px(70)).max(1)),
            (margin, status_y, width, px(30)),
            (group_x, button_y, button_width, px(30)),
            (
                group_x + button_width + px(10),
                button_y,
                button_width,
                px(30),
            ),
            (
                group_x + button_width * 2 + px(20),
                button_y,
                button_width,
                px(30),
            ),
        ];
        for (control, (x, y, width, height)) in route.controls.into_iter().zip(boxes) {
            SetWindowPos(
                control as HWND,
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

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_CLOSE => {
            choose(window, Choice::Cancel);
            return 0;
        }
        WM_COMMAND if (w >> 16) as u32 == BN_CLICKED && l != 0 => {
            let button = l as HWND;
            if GetParent(button) == window
                && GetDlgCtrlID(button) == (w & 0xffff) as i32
                && IsWindowEnabled(button) != 0
            {
                match w & 0xffff {
                    SAVE => choose(window, Choice::Save),
                    DISCARD => choose(window, Choice::Discard),
                    CANCEL => choose(window, Choice::Cancel),
                    _ => {}
                }
            }
            return 0;
        }
        WM_SIZE => {
            layout(window);
            return 0;
        }
        WM_GETMINMAXINFO if l != 0 => {
            let info = &mut *(l as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(window).max(96) as i32;
            info.ptMinTrackSize.x = 440 * dpi / 96;
            info.ptMinTrackSize.y = 260 * dpi / 96;
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
            layout(window);
            return 0;
        }
        WM_NCDESTROY => {
            ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
        }
        _ => {}
    }
    chrome::message(window, message, w, l).unwrap_or_else(|| DefWindowProcW(window, message, w, l))
}

pub(super) struct Panel {
    window: HWND,
    owner: HWND,
    id: u64,
    controls: [HWND; 6],
    owner_disabled: bool,
    previous_focus: HWND,
    background: bool,
    busy: Cell<bool>,
}

impl Panel {
    pub(super) fn new(
        owner: HWND,
        id: u64,
        labels: &[String],
        background: bool,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !labels.is_empty(),
            "dirty editor decision requires a document"
        );
        let body = body_text(labels);
        unsafe {
            anyhow::ensure!(IsWindow(owner) != 0, "editor close owner no longer exists");
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux.windows.editor-close");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register editor close window"
            );
            let dpi = GetDpiForWindow(owner).max(96) as i32;
            let mut bounds = RECT::default();
            checked(GetWindowRect(owner, &mut bounds))?;
            let width = 540 * dpi / 96;
            let height = (260
                + labels.len().min(8).saturating_sub(1) as i32 * 22
                + if labels.len() > 8 { 22 } else { 0 })
                * dpi
                / 96;
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT | WS_EX_DLGMODALFRAME,
                class.as_ptr(),
                wide("Save changes before closing?").as_ptr(),
                WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
                bounds.left + (bounds.right - bounds.left - width) / 2,
                bounds.top + (bounds.bottom - bounds.top - height) / 2,
                width,
                height,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create editor close window");
            let mut panel = Self {
                window,
                owner,
                id,
                controls: [std::ptr::null_mut(); 6],
                owner_disabled: false,
                previous_focus: std::ptr::null_mut(),
                background,
                busy: Cell::new(false),
            };
            panel.controls[0] = panel.child(
                "STATIC",
                "Save changes before closing?",
                10,
                windows_sys::Win32::System::SystemServices::SS_NOPREFIX,
            )?;
            panel.controls[1] = panel.child(
                "EDIT",
                &body,
                11,
                WS_VSCROLL
                    | WS_TABSTOP
                    | ES_MULTILINE as u32
                    | ES_AUTOVSCROLL as u32
                    | ES_READONLY as u32,
            )?;
            panel.controls[2] = panel.child(
                "STATIC",
                "",
                12,
                windows_sys::Win32::System::SystemServices::SS_NOPREFIX,
            )?;
            panel.controls[3] =
                panel.child("BUTTON", "Cancel", CANCEL, WS_TABSTOP | BS_OWNERDRAW as u32)?;
            panel.controls[4] = panel.child(
                "BUTTON",
                "Discard",
                DISCARD,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.controls[5] =
                panel.child("BUTTON", "Save", SAVE, WS_TABSTOP | BS_OWNERDRAW as u32)?;
            ROUTES.with(|routes| {
                routes.borrow_mut().insert(
                    window as isize,
                    Route {
                        id,
                        controls: panel.controls.map(|control| control as isize),
                        busy: false,
                        submitted: false,
                    },
                )
            });
            layout(window);
            if !background {
                panel.previous_focus = GetFocus();
            }
            panel.owner_disabled = IsWindowEnabled(owner) != 0;
            if panel.owner_disabled {
                EnableWindow(owner, 0);
            }
            if !background {
                ShowWindow(window, SW_SHOW);
                SetFocus(panel.controls[5]);
            }
            Ok(panel)
        }
    }

    fn child(&self, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
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
                self.window,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        anyhow::ensure!(!window.is_null(), "cannot create editor close control");
        if class == "BUTTON" {
            chrome::register_button(
                window,
                if id == DISCARD {
                    chrome::Role::Destructive
                } else {
                    chrome::Role::Button
                },
            );
        } else {
            chrome::register_control(
                window,
                if class == "EDIT" {
                    chrome::ControlRole::Edit
                } else {
                    chrome::ControlRole::Static
                },
            );
        }
        Ok(window)
    }

    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0 {
                return false;
            }
            if message.message == WM_KEYDOWN {
                match message.wParam {
                    13 | 27 => {
                        if message.lParam as usize & (1 << 30) == 0 {
                            let choice = if message.wParam == 27 || message.hwnd == self.controls[3]
                            {
                                Choice::Cancel
                            } else if message.hwnd == self.controls[4] {
                                Choice::Discard
                            } else {
                                Choice::Save
                            };
                            choose(self.window, choice);
                        }
                        return true;
                    }
                    _ => {}
                }
            }
            // Hidden owned probes can exercise Enter/Escape, but dialog Tab
            // navigation must never change desktop focus in background mode.
            if self.background {
                return matches!(message.message, WM_KEYDOWN | WM_KEYUP | WM_CHAR)
                    && message.wParam == 9;
            }
            IsDialogMessageW(self.window, message) != 0
        }
    }

    pub(super) fn busy(&self) {
        self.busy.set(true);
        ROUTES.with(|routes| {
            if let Some(route) = routes.borrow_mut().get_mut(&(self.window as isize)) {
                route.busy = true;
            }
        });
        unsafe {
            for button in &self.controls[3..] {
                EnableWindow(*button, 0);
            }
        }
        self.status("Saving…");
    }

    pub(super) fn status(&self, text: &str) {
        unsafe {
            SetWindowTextW(self.controls[2], wide(text).as_ptr());
        }
    }

    pub(super) fn diagnostics(&self) -> Value {
        json!({"window":self.window as usize,"owner":unsafe { GetWindow(self.window,GW_OWNER) } as usize,
            "id":self.id,"body":self.text(self.controls[1]),"body_handle":self.controls[1] as usize,
            "save":self.controls[5] as usize,"discard":self.controls[4] as usize,"cancel":self.controls[3] as usize,
            "busy":self.busy.get(),"status":self.text(self.controls[2]),
            "native_visible":unsafe { IsWindowVisible(self.window) != 0 }})
    }

    fn text(&self, window: HWND) -> String {
        unsafe {
            let mut text = vec![0u16; GetWindowTextLengthW(window).max(0) as usize + 1];
            let n = GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32);
            String::from_utf16_lossy(&text[..n.max(0) as usize])
        }
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            let restore_focus = !self.background && GetForegroundWindow() == self.window;
            if self.owner_disabled && IsWindow(self.owner) != 0 {
                EnableWindow(self.owner, 1);
            }
            ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
            for control in self.controls {
                if !control.is_null() {
                    chrome::unregister(control);
                }
            }
            DestroyWindow(self.window);
            if restore_focus
                && !self.previous_focus.is_null()
                && IsWindow(self.previous_focus) != 0
                && IsWindowEnabled(self.previous_focus) != 0
            {
                SetFocus(self.previous_focus);
            }
        }
    }
}
