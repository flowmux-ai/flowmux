// SPDX-License-Identifier: GPL-3.0-or-later
//! Inline page-find controls hosted beneath the browser address bar.
use super::super::super::chrome;
use super::*;
use std::cell::{Cell, RefCell};
use windows_sys::Win32::{
    System::SystemServices::{SS_ENDELLIPSIS, SS_NOPREFIX},
    UI::{
        Controls::{EM_LIMITTEXT, EM_SETCUEBANNER},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
    },
};

#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Next(usize),
    Previous(usize),
    Close(usize),
    Layout(usize),
    Changed(usize),
    Refresh(usize),
}

static NEXT_PANEL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);
thread_local! {
    static COMPOSING: Cell<bool> = const { Cell::new(false) };
    static SETTLING: Cell<bool> = const { Cell::new(false) };
    static SETTING_QUERY: Cell<bool> = const { Cell::new(false) };
}

fn emit(action: UiAction) {
    if matches!(action, UiAction::Next(_) | UiAction::Previous(_)) && COMPOSING.with(Cell::get) {
        return;
    }
    post(Event::BrowserFindUi(action));
}

unsafe extern "system" fn query_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    subclass: usize,
    _: usize,
) -> LRESULT {
    match message {
        WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION => {
            COMPOSING.with(|v| v.set(message == WM_IME_STARTCOMPOSITION));
            SETTLING.with(|v| v.set(true));
        }
        WM_KEYDOWN if wparam == 229 => SETTLING.with(|v| v.set(true)),
        WM_KEYUP if !matches!(wparam, 0x10..=0x12 | 0xa0..=0xa5) => {
            SETTLING.with(|v| v.set(false));
        }
        WM_KILLFOCUS | WM_NCDESTROY => {
            COMPOSING.with(|v| v.set(false));
            SETTLING.with(|v| v.set(false));
            if message == WM_NCDESTROY {
                RemoveWindowSubclass(window, Some(query_proc), subclass);
            }
        }
        _ => {}
    }
    let result = DefSubclassProc(window, message, wparam, lparam);
    if matches!(message, WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION) {
        let generation = GetWindowLongPtrW(GetParent(window), GWLP_USERDATA) as usize;
        emit(UiAction::Changed(generation));
    }
    result
}

unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let generation = GetWindowLongPtrW(window, GWLP_USERDATA) as usize;
    match message {
        WM_CLOSE => {
            emit(UiAction::Close(generation));
            0
        }
        WM_SIZE | WM_DPICHANGED_AFTERPARENT => {
            emit(UiAction::Layout(generation));
            0
        }
        WM_TIMER if wparam == 1 => {
            KillTimer(window, 1);
            emit(UiAction::Refresh(generation));
            0
        }
        WM_COMMAND => {
            let control = lparam as HWND;
            if control.is_null()
                || GetParent(control) != window
                || IsWindowEnabled(control) == 0
                || GetDlgCtrlID(control) as usize != (wparam & 0xffff)
            {
                return 0;
            }
            let action = match (wparam & 0xffff, (wparam >> 16) as u32) {
                (30, BN_CLICKED) => Some(UiAction::Previous(generation)),
                (31, BN_CLICKED) => Some(UiAction::Next(generation)),
                (2, BN_CLICKED) => Some(UiAction::Close(generation)),
                (10, EN_CHANGE) | (11, BN_CLICKED) if !SETTING_QUERY.with(Cell::get) => {
                    Some(UiAction::Changed(generation))
                }
                _ => None,
            };
            if let Some(action) = action {
                emit(action);
            }
            0
        }
        _ => chrome::message(window, message, wparam, lparam)
            .unwrap_or_else(|| DefWindowProcW(window, message, wparam, lparam)),
    }
}

// Physical-pixel geometry shared by height() and layout(), including narrow panes.
fn geometry(width: i32, scale: f64) -> (i32, [(i32, i32, i32, i32); 6]) {
    let px = |value: i32| ((value as f64 * scale).round() as i32).max(1);
    let margin = px(8).min(width.max(1) / 4);
    let available = (width - 2 * margin).max(1);
    let row = px(28);
    let gap = px(4);
    let tools = [px(58), row, row, row];
    let tools_width = tools.iter().sum::<i32>() + 3 * gap;
    let inline = available >= px(120) + gap + tools_width;
    let inline_status = available >= px(120) + tools_width + px(120) + 2 * gap;
    let query_width = if inline_status {
        available - tools_width - px(120) - 2 * gap
    } else if inline {
        available - tools_width - gap
    } else {
        available
    };
    let mut controls = [(margin, px(8), query_width, row); 6];
    let mut x = if inline {
        margin + query_width + gap + if inline_status { px(120) + gap } else { 0 }
    } else {
        margin
    };
    let mut y = if inline { px(8) } else { px(8) + row + gap };
    for (index, width) in tools.into_iter().enumerate() {
        let width = width.min(available);
        if x > margin && x + width > margin + available {
            x = margin;
            y += row + gap;
        }
        controls[index + 1] = (x, y, width, row);
        x += width + gap;
    }
    if inline_status {
        controls[5] = (
            margin + query_width + gap,
            y + (row - px(20)) / 2,
            px(120),
            px(20),
        );
        return (y + row + px(8), controls);
    }
    let status_y = y + row + gap;
    let status_height = px(if available < px(120) { 40 } else { 20 });
    controls[5] = (margin, status_y, available, status_height);
    (status_y + status_height + px(8), controls)
}

pub(super) struct Panel {
    pub(super) window: HWND,
    pub(super) generation: usize,
    query: HWND,
    case: HWND,
    previous: HWND,
    next: HWND,
    close: HWND,
    status: HWND,
    message: RefCell<String>,
    opened: Cell<bool>,
    focus_pending: Cell<bool>,
}

impl Drop for Panel {
    fn drop(&mut self) {
        for control in [
            self.query,
            self.case,
            self.previous,
            self.next,
            self.close,
            self.status,
        ] {
            chrome::unregister(control);
        }
        chrome::unregister(self.window);
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
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register page find window"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Find in page").as_ptr(),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create page find window");
            let generation = NEXT_PANEL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            SetWindowLongPtrW(window, GWLP_USERDATA, generation as isize);
            chrome::register_control(window, chrome::ControlRole::Static);
            // Own the HWND before creating children so an error destroys all controls.
            let mut panel = Self {
                window,
                generation,
                query: std::ptr::null_mut(),
                case: std::ptr::null_mut(),
                previous: std::ptr::null_mut(),
                next: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                message: RefCell::new(String::new()),
                opened: Cell::new(false),
                focus_pending: Cell::new(false),
            };
            panel.query = panel.child(
                "EDIT",
                "",
                10,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            anyhow::ensure!(
                SetWindowSubclass(panel.query, Some(query_proc), 1, 0) != 0,
                "cannot preserve page-find input composition"
            );
            SendMessageW(
                panel.query,
                EM_LIMITTEXT,
                crate::browser_find::MAX_QUERY_BYTES,
                0,
            );
            SendMessageW(
                panel.query,
                EM_SETCUEBANNER,
                1,
                wide("Find in page…").as_ptr() as LPARAM,
            );
            panel.case = panel.child("BUTTON", "Aa", 11, WS_TABSTOP | BS_AUTOCHECKBOX as u32)?;
            panel.previous =
                panel.child("BUTTON", "Previous", 30, WS_TABSTOP | BS_OWNERDRAW as u32)?;
            panel.next = panel.child("BUTTON", "Next", 31, WS_TABSTOP | BS_OWNERDRAW as u32)?;
            panel.close = panel.child("BUTTON", "Close", 2, WS_TABSTOP | BS_OWNERDRAW as u32)?;
            panel.status = panel.child("STATIC", "", 12, SS_NOPREFIX | SS_ENDELLIPSIS)?;
            chrome::register_button(
                panel.previous,
                chrome::Role::Icon {
                    kind: chrome::ChromeIcon::Back,
                    marked: false,
                },
            );
            chrome::register_button(
                panel.next,
                chrome::Role::Icon {
                    kind: chrome::ChromeIcon::Forward,
                    marked: false,
                },
            );
            chrome::register_button(panel.close, chrome::Role::Tool);
            chrome::register_control(panel.status, chrome::ControlRole::Caption);
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
            chrome::register_control(
                handle,
                if class == "EDIT" {
                    chrome::ControlRole::Edit
                } else {
                    chrome::ControlRole::Static
                },
            );
            Ok(handle)
        }
    }

    pub(super) fn layout(&self) {
        unsafe {
            let mut rect = RECT::default();
            GetClientRect(self.window, &mut rect);
            let scale = GetDpiForWindow(self.window).max(96) as f64 / 96.0;
            let (_, controls) = geometry(rect.right, scale);
            for (handle, (x, y, width, height)) in [
                self.query,
                self.case,
                self.previous,
                self.next,
                self.close,
                self.status,
            ]
            .into_iter()
            .zip(controls)
            {
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

    pub(super) fn owner(&self) -> HWND {
        unsafe { GetAncestor(self.window, GA_ROOT) }
    }

    pub(super) fn parent(&self) -> HWND {
        unsafe { GetParent(self.window) }
    }

    pub(super) fn height(width: i32, scale: f64) -> i32 {
        geometry(width, scale).0
    }

    pub(super) fn is_open(&self) -> bool {
        self.opened.get()
    }

    pub(super) fn place(&self, width: i32, top: i32, height: i32, background: bool) {
        unsafe {
            SetWindowPos(
                self.window,
                std::ptr::null_mut(),
                0,
                top,
                width.max(1),
                height.max(1),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            self.layout();
            let show = self.opened.get() && width > 0 && height > 0 && !background;
            if (GetWindowLongPtrW(self.window, GWL_STYLE) as u32 & WS_VISIBLE != 0) != show {
                ShowWindow(self.window, if show { SW_SHOWNA } else { SW_HIDE });
            }
            if show
                && IsWindowVisible(self.window) != 0
                && IsWindowEnabled(self.owner()) != 0
                && self.focus_pending.replace(false)
            {
                SetFocus(self.query);
            }
        }
    }

    pub(super) fn show(&self, background: bool) {
        self.opened.set(true);
        self.focus_pending.set(!background);
    }

    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if !self.is_open()
                || (message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0)
                || IsWindowEnabled(self.owner()) == 0
            {
                return false;
            }
            // Do not reuse the key that commits/cancels an IME composition.
            if self.composing() || SETTLING.with(Cell::get) || message.wParam == 229 {
                return false;
            }
            if message.message == WM_KEYDOWN && matches!(message.wParam, 13 | 27) {
                if message.lParam as usize & (1 << 30) == 0 {
                    emit(if message.wParam == 27 || message.hwnd == self.close {
                        UiAction::Close(self.generation)
                    } else if message.hwnd == self.previous {
                        UiAction::Previous(self.generation)
                    } else {
                        UiAction::Next(self.generation)
                    });
                }
                return true;
            }
            if message.message == WM_CHAR && matches!(message.wParam, 13 | 27) {
                return true;
            }
            IsWindowVisible(self.window) != 0 && IsDialogMessageW(self.window, message) != 0
        }
    }

    pub(super) fn composing(&self) -> bool {
        COMPOSING.with(Cell::get)
    }

    pub(super) fn schedule(&self) {
        unsafe {
            SetTimer(self.window, 1, 200, None);
        }
    }

    pub(super) fn cancel_scheduled(&self) {
        unsafe {
            KillTimer(self.window, 1);
        }
    }

    pub(super) fn query(&self) -> String {
        self.control_text(self.query)
    }

    pub(super) fn case_sensitive(&self) -> bool {
        unsafe { SendMessageW(self.case, BM_GETCHECK, 0, 0) == 1 }
    }

    pub(super) fn set_query(&self, text: &str, case_sensitive: bool) {
        if self.composing() {
            return;
        }
        SETTING_QUERY.with(|v| v.set(true));
        unsafe {
            SetWindowTextW(self.query, wide(text).as_ptr());
            SendMessageW(self.case, BM_SETCHECK, usize::from(case_sensitive), 0);
        }
        SETTING_QUERY.with(|v| v.set(false));
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

    pub(super) fn bounds(&self) -> Value {
        unsafe {
            let mut rect = RECT::default();
            GetWindowRect(self.window, &mut rect);
            let mut point = POINT {
                x: rect.left,
                y: rect.top,
            };
            ScreenToClient(self.parent(), &mut point);
            json!({"x":point.x,"y":point.y,"width":rect.right-rect.left,"height":rect.bottom-rect.top})
        }
    }

    pub(super) fn diagnostics(&self) -> Value {
        json!({"query":self.query as usize,"case":self.case as usize,"previous":self.previous as usize,"next":self.next as usize,"close":self.close as usize,"status":self.status as usize,"composing":self.composing(),"settling":SETTLING.with(Cell::get)})
    }

    fn control_text(&self, handle: HWND) -> String {
        unsafe {
            let mut text = vec![0u16; GetWindowTextLengthW(handle).max(0) as usize + 1];
            let length = GetWindowTextW(handle, text.as_mut_ptr(), text.len() as i32);
            String::from_utf16_lossy(&text[..length.max(0) as usize])
        }
    }
}
