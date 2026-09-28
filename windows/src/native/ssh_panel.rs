// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned asynchronous SSH workspace form; OpenSSH handles authentication in its terminal.
use super::*;
use flowmux_core::{SshTarget, SshWorkspaceConfig};
use windows_sys::Win32::System::SystemServices::SS_NOPREFIX;
use windows_sys::Win32::UI::{
    Controls::{BST_CHECKED, EM_GETCUEBANNER, EM_LIMITTEXT, EM_SETCUEBANNER},
    Input::KeyboardAndMouse::{EnableWindow, GetFocus},
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};

const CONNECT: usize = 1;
const TMUX: usize = 2;
const LABELS: [&str; 6] = [
    "Host",
    "Remote directory",
    "Workspace name",
    "Port",
    "Identity file",
    "SSH config file",
];
const KEYS: [&str; 6] = ["host", "cwd", "name", "port", "identity", "config"];
const CUES: [&str; 6] = [
    "Host alias or user@hostname",
    "/srv/project (optional)",
    "Optional",
    "From SSH config (default 22)",
    "Optional local key path",
    "Optional; defaults to ~/.ssh/config",
];

fn error_color() -> COLORREF {
    let palette = chrome::palette();
    if palette.high_contrast {
        palette.foreground
    } else {
        palette.destructive
    }
}

#[derive(Clone, Copy)]
pub(super) enum UiAction {
    Connect,
    Close,
    Layout,
}
#[derive(Clone, Copy)]
struct Route {
    id: Uuid,
    composing: bool,
    settling: bool,
    submitted: bool,
}
thread_local! { static ROUTES: RefCell<HashMap<isize, Route>> = RefCell::new(HashMap::new()); }
fn route(window: HWND) -> Option<Route> {
    ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied())
}
fn emit(window: HWND, action: UiAction) {
    let id = ROUTES.with(|routes| {
        let mut routes = routes.borrow_mut();
        let route = routes.get_mut(&(window as isize))?;
        if matches!(action, UiAction::Connect) {
            if route.composing || route.submitted {
                return None;
            }
            route.submitted = true;
        }
        Some(route.id)
    });
    if let Some(id) = id {
        post(Event::SshDialog(id, action));
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
    let parent = GetParent(window);
    ROUTES.with(|routes| {
        if let Some(route) = routes.borrow_mut().get_mut(&(parent as isize)) {
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
    match message {
        WM_CLOSE => {
            emit(window, UiAction::Close);
            return 0;
        }
        WM_SIZE => {
            emit(window, UiAction::Layout);
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
            emit(window, UiAction::Layout);
            return 0;
        }
        WM_GETMINMAXINFO if l != 0 => {
            let info = &mut *(l as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(window).max(96) as i32;
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 460 * dpi / 96,
                bottom: 540 * dpi / 96,
            };
            AdjustWindowRectExForDpi(
                &mut rect,
                GetWindowLongW(window, GWL_STYLE) as u32,
                0,
                GetWindowLongW(window, GWL_EXSTYLE) as u32,
                dpi as u32,
            );
            info.ptMinTrackSize.x = rect.right - rect.left;
            info.ptMinTrackSize.y = rect.bottom - rect.top;
            return 0;
        }
        WM_CTLCOLORSTATIC
            if l != 0 && GetParent(l as HWND) == window && GetDlgCtrlID(l as HWND) == 31 =>
        {
            let brush = chrome::message(window, message, w, l).unwrap_or(0);
            SetTextColor(w as HDC, error_color());
            return brush;
        }
        WM_COMMAND if (w >> 16) as u32 == BN_CLICKED && l != 0 => {
            let child = l as HWND;
            if w & 0xffff == CONNECT
                && GetParent(child) == window
                && GetDlgCtrlID(child) == CONNECT as i32
                && IsWindowEnabled(window) != 0
                && IsWindowEnabled(child) != 0
            {
                emit(window, UiAction::Connect);
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

pub(super) struct Panel {
    pub(super) id: Uuid,
    pub(super) window: HWND,
    pub(super) owner: HWND,
    labels: [HWND; 6],
    inputs: [HWND; 6],
    tmux: HWND,
    hint: HWND,
    error_label: HWND,
    connect: HWND,
    owner_disabled: bool,
    previous_focus: HWND,
    background: bool,
}
impl Panel {
    pub(super) fn new(owner: HWND, background: bool) -> anyhow::Result<Self> {
        unsafe {
            anyhow::ensure!(
                IsWindow(owner) != 0 && IsWindowEnabled(owner) != 0,
                "SSH dialog owner is unavailable"
            );
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux.windows.ssh");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register SSH dialog"
            );
            let dpi = GetDpiForWindow(owner).max(96) as i32;
            let mut bounds = RECT::default();
            checked(GetWindowRect(owner, &mut bounds))?;
            let style = WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN;
            let ex_style = WS_EX_CONTROLPARENT | WS_EX_DLGMODALFRAME;
            let mut frame = RECT {
                left: 0,
                top: 0,
                right: 460 * dpi / 96,
                bottom: 540 * dpi / 96,
            };
            checked(AdjustWindowRectExForDpi(
                &mut frame, style, 0, ex_style, dpi as u32,
            ))?;
            let (width, height) = (frame.right - frame.left, frame.bottom - frame.top);
            let window = CreateWindowExW(
                ex_style,
                class.as_ptr(),
                wide("New SSH Workspace").as_ptr(),
                style,
                bounds.left + (bounds.right - bounds.left - width) / 2,
                bounds.top + (bounds.bottom - bounds.top - height) / 2,
                width,
                height,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create SSH dialog");
            let mut panel = Self {
                id: Uuid::new_v4(),
                window,
                owner,
                labels: [std::ptr::null_mut(); 6],
                inputs: [std::ptr::null_mut(); 6],
                tmux: std::ptr::null_mut(),
                hint: std::ptr::null_mut(),
                error_label: std::ptr::null_mut(),
                connect: std::ptr::null_mut(),
                owner_disabled: false,
                previous_focus: std::ptr::null_mut(),
                background,
            };
            ROUTES.with(|routes| {
                routes.borrow_mut().insert(
                    window as isize,
                    Route {
                        id: panel.id,
                        composing: false,
                        settling: false,
                        submitted: false,
                    },
                )
            });
            for (index, label) in LABELS.iter().enumerate() {
                panel.labels[index] = panel.child("STATIC", label, 20 + index, SS_NOPREFIX)?;
                panel.inputs[index] = panel.child(
                    "EDIT",
                    "",
                    100 + index,
                    WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
                )?;
                SendMessageW(
                    panel.inputs[index],
                    EM_LIMITTEXT,
                    if index == 3 { 5 } else { 4096 },
                    0,
                );
                checked(SendMessageW(
                    panel.inputs[index],
                    EM_SETCUEBANNER,
                    1,
                    wide(CUES[index]).as_ptr() as LPARAM,
                ) as i32)
                .context("cannot set SSH field hint")?;
                checked(SetWindowSubclass(
                    panel.inputs[index],
                    Some(edit_proc),
                    1,
                    0,
                ))?;
            }
            panel.tmux = panel.child(
                "BUTTON",
                "Keep remote sessions with tmux",
                TMUX,
                WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            )?;
            panel.hint = panel.child("STATIC", "Authentication and host key confirmation use OpenSSH.\r\nRemote host requires a POSIX-compatible login shell.", 30, SS_NOPREFIX)?;
            panel.error_label = panel.child("STATIC", "", 31, SS_NOPREFIX)?;
            panel.connect = panel.child(
                "BUTTON",
                "Connect",
                CONNECT,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.relayout();
            if !background {
                panel.previous_focus = GetFocus();
            }
            panel.owner_disabled = true;
            EnableWindow(owner, 0);
            if !background {
                ShowWindow(window, SW_SHOW);
                SetFocus(panel.inputs[0]);
            }
            Ok(panel)
        }
    }
    fn child(&self, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
        let child = unsafe {
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
        anyhow::ensure!(!child.is_null(), "cannot create SSH form control");
        if class == "BUTTON" && id == CONNECT {
            chrome::register_button(child, chrome::Role::Suggested);
        } else {
            chrome::register_control(
                child,
                if class == "EDIT" {
                    chrome::ControlRole::Edit
                } else {
                    chrome::ControlRole::Static
                },
            );
        }
        Ok(child)
    }
    pub(super) fn values(&self) -> anyhow::Result<(Option<String>, SshWorkspaceConfig)> {
        anyhow::ensure!(
            !route(self.window).is_some_and(|r| r.composing),
            "Finish composing text before connecting"
        );
        let values: Vec<_> = self
            .inputs
            .iter()
            .map(|input| text(*input).map(|value| value.trim().to_owned()))
            .collect::<anyhow::Result<_>>()?;
        let optional = |index: usize| (!values[index].is_empty()).then(|| values[index].clone());
        let mut target = SshTarget::parse(&values[0]).map_err(anyhow::Error::msg)?;
        target.port = optional(3)
            .map(|value| {
                value
                    .parse::<u16>()
                    .context("Port must be between 1 and 65535")
            })
            .transpose()?;
        target.identity_file = optional(4).map(PathBuf::from);
        target.config_file = optional(5).map(PathBuf::from);
        let name = optional(2);
        if let Some(name) = &name {
            model::validate_name(name)?;
        }
        let config = SshWorkspaceConfig {
            target,
            cwd: optional(1),
            tmux: unsafe { SendMessageW(self.tmux, BM_GETCHECK, 0, 0) == BST_CHECKED as isize },
            forwards: Vec::new(),
        };
        config.validate().map_err(anyhow::Error::msg)?;
        Ok((name, config))
    }
    pub(super) fn error(&self, value: &str) {
        ROUTES.with(|routes| {
            if let Some(route) = routes.borrow_mut().get_mut(&(self.window as isize)) {
                route.submitted = false;
            }
        });
        unsafe {
            SetWindowTextW(self.error_label, wide(value).as_ptr());
        }
    }
    pub(super) fn is_open(&self) -> bool {
        unsafe { IsWindow(self.window) != 0 }
    }
    pub(super) fn relayout(&self) {
        unsafe {
            let mut client = RECT::default();
            if GetClientRect(self.window, &mut client) == 0 {
                return;
            }
            let dpi = GetDpiForWindow(self.window).max(96) as i32;
            let px = |dip| dip * dpi / 96;
            let width = (client.right - px(32)).max(1);
            let place = |window: HWND, x: i32, y: i32, width: i32, height: i32| {
                if !window.is_null() {
                    SetWindowPos(
                        window,
                        std::ptr::null_mut(),
                        x,
                        y,
                        width,
                        height,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            };
            for index in 0..6 {
                let y = px(12 + index as i32 * 56);
                place(self.labels[index], px(16), y, width, px(20));
                place(self.inputs[index], px(16), y + px(22), width, px(28));
            }
            place(self.tmux, px(16), px(352), width, px(28));
            place(self.hint, px(16), px(388), width, px(48));
            place(
                self.error_label,
                px(16),
                px(442),
                width,
                (client.bottom - px(504)).max(1),
            );
            place(
                self.connect,
                px(16),
                (client.bottom - px(52)).max(0),
                width,
                px(36),
            );
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if (message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0)
                || IsWindowEnabled(self.window) == 0
            {
                return false;
            }
            let guarded = route(self.window).is_some_and(|route| route.composing || route.settling)
                || message.wParam == 229;
            if guarded {
                return false;
            }
            if message.message == WM_KEYDOWN && matches!(message.wParam, 13 | 27) {
                if message.lParam as usize & (1 << 30) == 0 {
                    emit(
                        self.window,
                        if message.wParam == 27 {
                            UiAction::Close
                        } else {
                            UiAction::Connect
                        },
                    );
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
    pub(super) fn diagnostics(&self) -> Value {
        let state = route(self.window);
        let fields: Vec<_> = self.inputs.iter().enumerate().map(|(index, input)| json!({"key":KEYS[index],"label":LABELS[index],"input":*input as usize,"caption":self.labels[index] as usize,"value":text(*input).unwrap_or_default(),"cue":cue(*input)})).collect();
        let (connect_background, connect_foreground) = chrome::suggested_colors();
        json!({"id":self.id,"window":self.window as usize,"owner":unsafe {GetWindow(self.window,GW_OWNER)} as usize,"open":self.is_open(),
            "fields":fields,"tmux":self.tmux as usize,"tmux_checked":unsafe {SendMessageW(self.tmux,BM_GETCHECK,0,0)==BST_CHECKED as isize},
            "hint":self.hint as usize,"connect":self.connect as usize,"error_handle":self.error_label as usize,"error":text(self.error_label).unwrap_or_default(),
            "colors":{"connect_background":connect_background,"connect_foreground":connect_foreground,"error":error_color(),"background":chrome::palette().background},"high_contrast":chrome::palette().high_contrast,
            "composing":state.is_some_and(|r|r.composing),"settling":state.is_some_and(|r|r.settling),"pending":state.is_some_and(|r|r.submitted),"native_visible":unsafe {IsWindowVisible(self.window)!=0}})
    }
}
fn cue(window: HWND) -> Option<String> {
    let mut value = [0u16; 256];
    // EM_GETCUEBANNER contains a pointer and must run in the owning process.
    let ok = unsafe {
        SendMessageW(
            window,
            EM_GETCUEBANNER,
            value.as_mut_ptr() as WPARAM,
            value.len() as LPARAM,
        )
    };
    (ok != 0).then(|| {
        String::from_utf16_lossy(
            &value[..value
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(value.len())],
        )
    })
}
fn text(window: HWND) -> anyhow::Result<String> {
    unsafe {
        let length = GetWindowTextLengthW(window).max(0) as usize;
        anyhow::ensure!(length <= 16384, "SSH field is too long");
        let mut value = vec![0u16; length + 1];
        let read = GetWindowTextW(window, value.as_mut_ptr(), value.len() as i32).max(0) as usize;
        String::from_utf16(&value[..read]).context("SSH field contains invalid UTF-16")
    }
}
impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            let restore = !self.background && GetForegroundWindow() == self.window;
            ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
            if self.owner_disabled && IsWindow(self.owner) != 0 {
                EnableWindow(self.owner, 1);
            }
            for child in self.inputs.into_iter().chain(self.labels).chain([
                self.tmux,
                self.hint,
                self.error_label,
                self.connect,
            ]) {
                if !child.is_null() {
                    chrome::unregister(child);
                }
            }
            DestroyWindow(self.window);
            if restore
                && !self.previous_focus.is_null()
                && IsWindow(self.previous_focus) != 0
                && IsWindowEnabled(self.previous_focus) != 0
            {
                SetFocus(self.previous_focus);
            }
        }
    }
}
