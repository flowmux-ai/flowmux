// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned shortcut edit/capture windows; no nested loop or global keyboard state.
use super::*;
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{EnableWindow, GetFocus, IsWindowEnabled},
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};

#[derive(Clone, Copy)]
pub(crate) enum Signal {
    Changed,
    Confirm,
    Cancel,
    Reset,
    Unbind,
    Capture,
    CaptureCancel,
    Layout,
}
#[derive(Clone)]
struct Route {
    id: Uuid,
    input: isize,
    label: isize,
    capture: bool,
    composing: bool,
    settling: bool,
    keys: u16,
}
thread_local! { static ROUTES: RefCell<HashMap<isize,Route>> = RefCell::new(HashMap::new()); }
fn emit(id: Uuid, signal: Signal) {
    super::emit(super::Signal::Editor(id, signal));
}
fn read(window: HWND) -> String {
    unsafe {
        let mut value = vec![0u16; 2049];
        let n = GetWindowTextW(window, value.as_mut_ptr(), value.len() as i32);
        String::from_utf16_lossy(&value[..n.max(0) as usize])
    }
}
fn route(window: HWND) -> Option<Route> {
    ROUTES.with(|r| r.borrow().get(&(window as isize)).cloned())
}
unsafe extern "system" fn input_proc(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    let parent = GetParent(window);
    if matches!(message, WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION) {
        ROUTES.with(|r| {
            if let Some(r) = r.borrow_mut().get_mut(&(parent as isize)) {
                r.composing = message == WM_IME_STARTCOMPOSITION;
                r.settling = true;
            }
        });
    }
    if (message == WM_KEYDOWN && w == 229)
        || (message == WM_KEYUP && crate::keybindings::native_modifier(w, l).is_none())
    {
        ROUTES.with(|routes| {
            if let Some(r) = routes.borrow_mut().get_mut(&(parent as isize)) {
                r.settling = message == WM_KEYDOWN;
            }
        });
    }
    let result = DefSubclassProc(window, message, w, l);
    if matches!(message, WM_IME_ENDCOMPOSITION | WM_IME_STARTCOMPOSITION) {
        if let Some(r) = route(parent) {
            emit(r.id, Signal::Changed);
        }
    }
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(window, Some(input_proc), id);
    }
    result
}

unsafe fn capture_key(window: HWND, message: u32, w: WPARAM, l: LPARAM) {
    let Some(mut r) = route(window) else { return };
    let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    if let Some(bit) = crate::keybindings::native_modifier(w, l) {
        if down {
            r.keys |= bit;
        } else {
            r.keys &= !bit;
        }
    }
    if !down && crate::keybindings::native_modifier(w, l).is_none() {
        r.settling = false;
    }
    if down && w == 229 {
        r.settling = true;
    }
    ROUTES.with(|routes| routes.borrow_mut().insert(window as isize, r.clone()));
    if !down || r.composing || r.settling || l as usize & (1 << 30) != 0 {
        return;
    }
    if w == 27 {
        emit(r.id, Signal::CaptureCancel);
        return;
    }
    // Right Alt can be AltGraph; never interpret its synthetic Ctrl+Alt as a binding.
    if r.keys & (8 | 64 | 128) != 0 {
        return;
    }
    match crate::keybindings::captured_key(
        w as u32,
        r.keys & 3 != 0,
        r.keys & 12 != 0,
        r.keys & 48 != 0,
    ) {
        Ok(Some(value)) => {
            SetWindowTextW(r.input as HWND, wide(&value).as_ptr());
            emit(r.id, Signal::Changed);
            emit(r.id, Signal::CaptureCancel);
        }
        Ok(None) => {}
        Err(error) => {
            SetWindowTextW(r.label as HWND, wide(format!("{error:#}")).as_ptr());
        }
    }
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(r) = route(window) {
        match message {
            WM_CLOSE => {
                emit(
                    r.id,
                    if r.capture {
                        Signal::CaptureCancel
                    } else {
                        Signal::Cancel
                    },
                );
                return 0;
            }
            WM_COMMAND if !r.capture => {
                let signal = match (w & 0xffff, (w >> 16) as u32) {
                    (100, EN_CHANGE) => Some(Signal::Changed),
                    (1, BN_CLICKED) => Some(Signal::Confirm),
                    (2, BN_CLICKED) => Some(Signal::Cancel),
                    (3, BN_CLICKED) => Some(Signal::Reset),
                    (4, BN_CLICKED) => Some(Signal::Unbind),
                    (5, BN_CLICKED) => Some(Signal::Capture),
                    _ => None,
                };
                if let Some(signal) = signal {
                    emit(r.id, signal);
                }
                return 0;
            }
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP if r.capture => {
                capture_key(window, message, w, l);
                return 0;
            }
            WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION if r.capture => {
                ROUTES.with(|routes| {
                    if let Some(r) = routes.borrow_mut().get_mut(&(window as isize)) {
                        r.composing = message == WM_IME_STARTCOMPOSITION;
                        r.settling = true;
                        r.keys = 0;
                    }
                });
                return 0;
            }
            WM_KILLFOCUS if r.capture => {
                ROUTES.with(|routes| {
                    if let Some(r) = routes.borrow_mut().get_mut(&(window as isize)) {
                        r.keys = 0;
                    }
                });
            }
            WM_SIZE => {
                emit(r.id, Signal::Layout);
                return 0;
            }
            WM_GETMINMAXINFO if l != 0 => {
                let info = &mut *(l as *mut MINMAXINFO);
                let dpi = GetDpiForWindow(window).max(96) as i32;
                info.ptMinTrackSize.x = if r.capture { 320 } else { 420 } * dpi / 96;
                info.ptMinTrackSize.y = if r.capture { 140 } else { 220 } * dpi / 96;
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
                emit(r.id, Signal::Layout);
                return 0;
            }
            WM_NCDESTROY => {
                ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
            }
            _ => {}
        }
    }
    if let Some(value) = chrome::message(window, message, w, l) {
        return value;
    }
    DefWindowProcW(window, message, w, l)
}
struct Modal {
    window: HWND,
    disabled: Vec<HWND>,
    previous: HWND,
    background: bool,
}
impl Modal {
    fn new(
        owner: HWND,
        title: &str,
        width: i32,
        height: i32,
        background: bool,
        chain: bool,
    ) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.keybinding.editor");
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
                "cannot register shortcut editor"
            );
            let dpi = GetDpiForWindow(owner).max(96) as i32;
            let mut parent = RECT::default();
            GetWindowRect(owner, &mut parent);
            let width = width * dpi / 96;
            let height = height * dpi / 96;
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide(title).as_ptr(),
                WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
                parent.left + (parent.right - parent.left - width) / 2,
                parent.top + (parent.bottom - parent.top - height) / 2,
                width,
                height,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create shortcut editor");
            let previous = if background {
                std::ptr::null_mut()
            } else {
                GetFocus()
            };
            let mut disabled = Vec::new();
            let mut target = owner;
            for _ in 0..8 {
                if target.is_null() {
                    break;
                }
                if IsWindowEnabled(target) != 0 {
                    EnableWindow(target, 0);
                    disabled.push(target);
                }
                if !chain {
                    break;
                }
                target = GetWindow(target, GW_OWNER);
            }
            Ok(Self {
                window,
                disabled,
                previous,
                background,
            })
        }
    }
    fn show(&self, focus: HWND) {
        if !self.background {
            unsafe {
                ShowWindow(self.window, SW_SHOW);
                SetFocus(focus);
            }
        }
    }
}
impl Drop for Modal {
    fn drop(&mut self) {
        unsafe {
            let foreground = GetForegroundWindow();
            let restore =
                foreground == self.window || GetWindow(foreground, GW_OWNER) == self.window;
            for window in &self.disabled {
                if IsWindow(*window) != 0 {
                    EnableWindow(*window, 1);
                }
            }
            ROUTES.with(|r| r.borrow_mut().remove(&(self.window as isize)));
            DestroyWindow(self.window);
            if !self.background
                && restore
                && !self.previous.is_null()
                && IsWindow(self.previous) != 0
                && IsWindowEnabled(self.previous) != 0
                && IsWindowVisible(self.previous) != 0
            {
                SetFocus(self.previous);
            }
        }
    }
}
fn child(parent: HWND, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
    unsafe {
        let window = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            1,
            1,
            parent,
            id as HMENU,
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        anyhow::ensure!(!window.is_null(), "cannot create shortcut editor control");
        if class == "BUTTON" {
            chrome::register_button(window, chrome::Role::Button);
        } else {
            chrome::register_control(
                window,
                if class == "EDIT" {
                    chrome::ControlRole::Edit
                } else {
                    chrome::ControlRole::Caption
                },
            );
        }
        Ok(window)
    }
}
struct Capture {
    modal: Modal,
    label: HWND,
}
pub(super) struct Panel {
    pub(super) id: Uuid,
    modal: Modal,
    input: HWND,
    hint: HWND,
    error: HWND,
    buttons: [HWND; 5],
    capture: RefCell<Option<Capture>>,
    pending: Cell<bool>,
    theme: Cell<crate::settings::Theme>,
}
impl Panel {
    pub(super) fn new(
        owner: HWND,
        label: &str,
        raw: &str,
        background: bool,
    ) -> anyhow::Result<Self> {
        let modal = Modal::new(
            owner,
            &format!("Edit shortcut: {label}"),
            420,
            220,
            background,
            true,
        )?;
        let window = modal.window;
        let mut panel = Self {
            id: Uuid::new_v4(),
            modal,
            input: std::ptr::null_mut(),
            hint: std::ptr::null_mut(),
            error: std::ptr::null_mut(),
            buttons: [std::ptr::null_mut(); 5],
            capture: RefCell::new(None),
            pending: Cell::new(false),
            theme: Cell::new(crate::settings::Theme::Dark),
        };
        panel.hint = child(
            window,
            "STATIC",
            "Use Ctrl+Shift+Key or GTK syntax. Separate shortcuts with commas. Empty means unbind.",
            110,
            SS_NOPREFIX,
        )?;
        panel.input = child(
            window,
            "EDIT",
            raw,
            100,
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
        )?;
        panel.error = child(window, "STATIC", "", 111, SS_NOPREFIX)?;
        for (index, (text, id)) in [
            ("OK", 1),
            ("Cancel", 2),
            ("Reset", 3),
            ("Unbind", 4),
            ("Capture key…", 5),
        ]
        .into_iter()
        .enumerate()
        {
            panel.buttons[index] = child(window, "BUTTON", text, id, WS_TABSTOP)?;
        }
        ROUTES.with(|r| {
            r.borrow_mut().insert(
                window as isize,
                Route {
                    id: panel.id,
                    input: panel.input as isize,
                    label: panel.error as isize,
                    capture: false,
                    composing: false,
                    settling: false,
                    keys: 0,
                },
            )
        });
        unsafe {
            SendMessageW(panel.input, EM_LIMITTEXT, 2048, 0);
            checked(SetWindowSubclass(panel.input, Some(input_proc), 100, 0))?;
        }
        panel.layout();
        panel.modal.show(panel.input);
        Ok(panel)
    }
    pub(super) fn raw(&self) -> String {
        read(self.input)
    }
    pub(super) fn set_raw(&self, value: &str) {
        if self.raw() != value {
            unsafe {
                SetWindowTextW(self.input, wide(value).as_ptr());
            }
        }
    }
    pub(super) fn status(&self, value: &str) {
        unsafe {
            SetWindowTextW(self.error, wide(value).as_ptr());
        }
    }
    pub(super) fn pending(&self, pending: bool) {
        self.pending.set(pending);
        unsafe {
            for index in [0, 2, 3, 4] {
                EnableWindow(self.buttons[index], i32::from(!pending));
            }
        }
    }
    pub(super) fn composing(&self) -> bool {
        route(self.modal.window).is_some_and(|r| r.composing)
    }
    pub(super) fn focus(&self) {
        if !self.modal.background {
            unsafe {
                SetFocus(self.input);
            }
        }
    }
    pub(super) fn theme(&self, theme: crate::settings::Theme) {
        self.theme.set(theme);
        chrome::window_theme(self.modal.window, theme);
        if let Some(capture) = self.capture.borrow().as_ref() {
            chrome::window_theme(capture.modal.window, theme);
        }
    }
    pub(super) fn capture_start(&self) -> anyhow::Result<()> {
        if self.capture.borrow().is_some() || self.pending.get() || self.composing() {
            return Ok(());
        }
        let modal = Modal::new(
            self.modal.window,
            "Press a shortcut",
            320,
            140,
            self.modal.background,
            false,
        )?;
        chrome::window_theme(modal.window, self.theme.get());
        let label = child(
            modal.window,
            "STATIC",
            "Press the key combination you want to bind.\n(Esc cancels.)",
            112,
            SS_NOPREFIX | 1,
        )?;
        ROUTES.with(|r| {
            r.borrow_mut().insert(
                modal.window as isize,
                Route {
                    id: self.id,
                    input: self.input as isize,
                    label: label as isize,
                    capture: true,
                    composing: false,
                    settling: false,
                    keys: 0,
                },
            )
        });
        *self.capture.borrow_mut() = Some(Capture { modal, label });
        self.layout();
        if let Some(capture) = self.capture.borrow().as_ref() {
            capture.modal.show(capture.modal.window);
        }
        Ok(())
    }
    pub(super) fn capture_close(&self) {
        self.capture.borrow_mut().take();
    }
    pub(super) fn layout(&self) {
        unsafe {
            let window = self.modal.window;
            let dpi = GetDpiForWindow(window).max(96) as i32;
            let px = |n| n * dpi / 96;
            let mut r = RECT::default();
            GetClientRect(window, &mut r);
            let place = |w, x, y, width, height| {
                SetWindowPos(
                    w,
                    std::ptr::null_mut(),
                    x,
                    y,
                    width,
                    height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            };
            place(self.hint, px(12), px(10), r.right - px(24), px(38));
            place(self.input, px(12), px(52), r.right - px(24), px(28));
            place(
                self.error,
                px(12),
                px(84),
                r.right - px(24),
                (r.bottom - px(124)).max(px(20)),
            );
            let y = r.bottom - px(38);
            place(self.buttons[4], px(12), y, px(108), px(28));
            place(self.buttons[2], px(124), y, px(62), px(28));
            place(self.buttons[3], px(190), y, px(68), px(28));
            place(self.buttons[1], r.right - px(136), y, px(72), px(28));
            place(self.buttons[0], r.right - px(60), y, px(48), px(28));
            if let Some(capture) = self.capture.borrow().as_ref() {
                let dpi = GetDpiForWindow(capture.modal.window).max(96) as i32;
                let px = |n| n * dpi / 96;
                let mut r = RECT::default();
                GetClientRect(capture.modal.window, &mut r);
                place(
                    capture.label,
                    px(20),
                    px(20),
                    r.right - px(40),
                    r.bottom - px(40),
                );
            }
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if let Some(capture) = self.capture.borrow().as_ref() {
                if message.hwnd == capture.modal.window
                    || IsChild(capture.modal.window, message.hwnd) != 0
                {
                    return false;
                }
            }
            let window = self.modal.window;
            if message.hwnd != window && IsChild(window, message.hwnd) == 0 {
                return false;
            }
            if route(window).is_some_and(|r| r.composing || r.settling) || message.wParam == 229 {
                return false;
            }
            if message.message == WM_KEYDOWN {
                match message.wParam {
                    13 => {
                        let signal = if message.hwnd == self.buttons[1] {
                            Signal::Cancel
                        } else if message.hwnd == self.buttons[2] {
                            Signal::Reset
                        } else if message.hwnd == self.buttons[3] {
                            Signal::Unbind
                        } else if message.hwnd == self.buttons[4] {
                            Signal::Capture
                        } else {
                            Signal::Confirm
                        };
                        if !self.pending.get() || matches!(signal, Signal::Cancel) {
                            emit(self.id, signal);
                        }
                        return true;
                    }
                    27 => {
                        emit(self.id, Signal::Cancel);
                        return true;
                    }
                    _ => {}
                }
            }
            IsDialogMessageW(window, message) != 0
        }
    }
    #[cfg(debug_assertions)]
    pub(super) fn capture_window(&self) -> HWND {
        self.capture
            .borrow()
            .as_ref()
            .map(|capture| capture.modal.window)
            .unwrap_or(self.modal.window)
    }
    pub(super) fn diagnostics(&self) -> Value {
        json!({"id":self.id,"window":self.modal.window as usize,"owner":unsafe{GetWindow(self.modal.window,GW_OWNER)} as usize,"input":self.input as usize,"ok":self.buttons[0] as usize,"cancel":self.buttons[1] as usize,"reset":self.buttons[2] as usize,"unbind":self.buttons[3] as usize,"capture":self.buttons[4] as usize,"raw":self.raw(),"error":read(self.error),"composing":self.composing(),"pending":self.pending.get(),"modal":true,"capture_dialog":self.capture.borrow().as_ref().map(|c|json!({"window":c.modal.window as usize,"owner":self.modal.window as usize,"message":read(c.label),"composing":route(c.modal.window).is_some_and(|r|r.composing),"modal":true}))})
    }
}
impl Drop for Panel {
    fn drop(&mut self) {
        self.capture.borrow_mut().take();
    }
}
