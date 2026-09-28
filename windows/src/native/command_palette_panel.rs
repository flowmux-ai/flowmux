// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned native command palette. Search is a drawing-independent copy of raw input.
use super::*;
use std::cell::{Cell, RefCell};
use windows_sys::Win32::System::SystemServices::SS_NOPREFIX;
use windows_sys::Win32::UI::{
    Controls::EM_LIMITTEXT,
    Input::KeyboardAndMouse::{EnableWindow, GetFocus, IsWindowEnabled},
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};
const QUERY_SUBCLASS: usize = 0x464d_4350;
const LIST_SUBCLASS: usize = 0x464d_434c;
#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Show,
    Changed,
    Activate,
    Close,
    Layout,
}
fn emit(action: UiAction) {
    post(Event::CommandPalette(action));
}
#[derive(Clone, Copy, Default)]
struct Guard {
    composing: bool,
    settling: bool,
}
thread_local! {
    static GUARDS: RefCell<HashMap<isize, Guard>> = RefCell::new(HashMap::new());
}
fn guard(window: HWND) -> Guard {
    GUARDS.with(|g| {
        g.borrow()
            .get(&(window as isize))
            .copied()
            .unwrap_or_default()
    })
}
fn modifier(key: usize) -> bool {
    matches!(key, 0x10..=0x12 | 0xa0..=0xa5)
}
unsafe extern "system" fn query_proc(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    GUARDS.with(|g| {
        if let Some(g) = g.borrow_mut().get_mut(&(window as isize)) {
            match message {
                WM_IME_STARTCOMPOSITION => {
                    g.composing = true;
                    g.settling = true;
                }
                WM_IME_ENDCOMPOSITION => {
                    g.composing = false;
                    g.settling = true;
                }
                WM_KEYDOWN if w == 229 => g.settling = true,
                WM_KEYUP if !modifier(w) => g.settling = false,
                _ => {}
            }
        }
    });
    let result = DefSubclassProc(window, message, w, l);
    if matches!(message, WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION) {
        emit(UiAction::Changed);
    }
    if message == WM_NCDESTROY {
        GUARDS.with(|g| g.borrow_mut().remove(&(window as isize)));
        RemoveWindowSubclass(window, Some(query_proc), id);
    }
    result
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_CLOSE => {
            emit(UiAction::Close);
            return 0;
        }
        WM_SIZE => {
            emit(UiAction::Layout);
            return 0;
        }
        WM_DPICHANGED if l != 0 => {
            let r = &*(l as *const RECT);
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                r.left,
                r.top,
                r.right - r.left,
                r.bottom - r.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            emit(UiAction::Layout);
            return 0;
        }
        WM_GETMINMAXINFO if l != 0 => {
            let info = &mut *(l as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(window).max(96) as i32;
            info.ptMinTrackSize.x = 320 * dpi / 96;
            info.ptMinTrackSize.y = 240 * dpi / 96;
            return 0;
        }
        WM_COMMAND => {
            if (w & 0xffff, (w >> 16) as u32) == (10, EN_CHANGE) {
                emit(UiAction::Changed);
            }
            return 0;
        }
        _ => {}
    }
    if let Some(result) = chrome::message(window, message, w, l) {
        return result;
    }
    DefWindowProcW(window, message, w, l)
}
unsafe extern "system" fn list_proc(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    let result = DefSubclassProc(window, message, w, l);
    if message == WM_LBUTTONUP {
        // Selection notifications also come from keyboard navigation and omit
        // clicks on the selected row. Only a completed in-list click executes.
        let hit = SendMessageW(window, LB_ITEMFROMPOINT, 0, l) as usize;
        let index = hit & 0xffff;
        let count = SendMessageW(window, LB_GETCOUNT, 0, 0);
        if hit >> 16 == 0 && count > 0 && index < count as usize {
            let mut rect = RECT::default();
            let x = (l as u16 as i16) as i32;
            let y = ((l as usize >> 16) as u16 as i16) as i32;
            if SendMessageW(
                window,
                LB_GETITEMRECT,
                index,
                &mut rect as *mut RECT as LPARAM,
            ) >= 0
                && x >= rect.left
                && x < rect.right
                && y >= rect.top
                && y < rect.bottom
            {
                SendMessageW(window, LB_SETCURSEL, index, 0);
                emit(UiAction::Activate);
            }
        }
    }
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(window, Some(list_proc), id);
    }
    result
}
fn read(window: HWND) -> String {
    unsafe {
        let mut value = vec![0u16; GetWindowTextLengthW(window).max(0) as usize + 1];
        let n = GetWindowTextW(window, value.as_mut_ptr(), value.len() as i32);
        String::from_utf16_lossy(&value[..n.max(0) as usize])
    }
}
fn matches(query: &str, candidate: &str) -> bool {
    let candidate = candidate.to_lowercase();
    query.to_lowercase().split_whitespace().all(|token| {
        let mut chars = candidate.chars();
        token.chars().all(|needle| chars.any(|ch| ch == needle))
    })
}
pub(super) struct Panel {
    pub(super) window: HWND,
    owner: HWND,
    query: HWND,
    list: HWND,
    status: HWND,
    opened: bool,
    disabled: Vec<HWND>,
    previous: HWND,
    entries: Vec<Entry>,
    filtered: Vec<usize>,
    applied_query: String,
    setting: Cell<bool>,
}
impl Panel {
    pub(super) fn new(owner: HWND) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.command-palette");
            let instance = GetModuleHandleW(std::ptr::null());
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register command palette"
            );
            let dpi = GetDpiForWindow(owner).max(96) as i32;
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Command Palette").as_ptr(),
                WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                480 * dpi / 96,
                420 * dpi / 96,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create command palette");
            let mut p = Self {
                window,
                owner,
                query: std::ptr::null_mut(),
                list: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                opened: false,
                disabled: vec![],
                previous: std::ptr::null_mut(),
                entries: vec![],
                filtered: vec![],
                applied_query: String::new(),
                setting: Cell::new(false),
            };
            p.query = p.child(
                "EDIT",
                "",
                10,
                WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            )?;
            p.list = p.child(
                "LISTBOX",
                "",
                20,
                WS_TABSTOP
                    | WS_VSCROLL
                    | LBS_NOTIFY as u32
                    | LBS_NOINTEGRALHEIGHT as u32
                    | LBS_USETABSTOPS as u32,
            )?;
            p.status = p.child("STATIC", "", 30, SS_NOPREFIX)?;
            SendMessageW(p.query, EM_LIMITTEXT, 1024, 0);
            SendMessageW(
                p.query,
                0x1501,
                1,
                wide("Search commands…").as_ptr() as LPARAM,
            );
            GUARDS.with(|g| g.borrow_mut().insert(p.query as isize, Guard::default()));
            anyhow::ensure!(
                SetWindowSubclass(p.query, Some(query_proc), QUERY_SUBCLASS, 0) != 0,
                "cannot preserve command palette composition"
            );
            anyhow::ensure!(
                SetWindowSubclass(p.list, Some(list_proc), LIST_SUBCLASS, 0) != 0,
                "cannot handle command palette clicks"
            );
            p.layout();
            Ok(p)
        }
    }
    fn child(&self, class: &str, text: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
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
                self.window,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            checked((!window.is_null()) as i32)?;
            chrome::register_control(
                window,
                match class {
                    "EDIT" => chrome::ControlRole::Edit,
                    "LISTBOX" => chrome::ControlRole::Listbox,
                    _ => chrome::ControlRole::Static,
                },
            );
            Ok(window)
        }
    }
    pub(super) fn show(&mut self, entries: Vec<Entry>, background: bool) {
        if self.opened {
            if !background {
                unsafe {
                    SetFocus(self.query);
                }
            }
            return;
        }
        self.entries = entries;
        self.opened = true;
        self.previous = if background {
            std::ptr::null_mut()
        } else {
            unsafe { GetFocus() }
        };
        GUARDS.with(|g| g.borrow_mut().insert(self.query as isize, Guard::default()));
        self.setting.set(true);
        unsafe {
            SetWindowTextW(self.query, wide("").as_ptr());
            let mut current = self.owner;
            for _ in 0..8 {
                if current.is_null() {
                    break;
                }
                if IsWindowEnabled(current) != 0 {
                    EnableWindow(current, 0);
                    self.disabled.push(current);
                }
                current = GetWindow(current, GW_OWNER);
            }
            let mut owner = RECT::default();
            let mut own = RECT::default();
            GetWindowRect(self.owner, &mut owner);
            GetWindowRect(self.window, &mut own);
            SetWindowPos(
                self.window,
                std::ptr::null_mut(),
                owner.left + ((owner.right - owner.left) - (own.right - own.left)) / 2,
                owner.top + ((owner.bottom - owner.top) - (own.bottom - own.top)) / 2,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.setting.set(false);
        self.changed();
        self.layout();
        if !background {
            unsafe {
                ShowWindow(self.window, SW_SHOW);
                SetFocus(self.query);
            }
        }
    }
    pub(super) fn changed(&mut self) {
        if !self.opened || self.setting.get() {
            return;
        }
        let composing = guard(self.query).composing;
        unsafe {
            EnableWindow(self.list, i32::from(!composing));
        }
        if composing {
            unsafe {
                SetWindowTextW(
                    self.status,
                    wide("Finish composing to search commands.").as_ptr(),
                );
            }
            return;
        }
        let query = read(self.query);
        self.filtered = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                let candidate = entry.id.strip_prefix("action:").map_or_else(
                    || entry.label.clone(),
                    |action| format!("{} {action}", entry.label),
                );
                matches(&query, &candidate).then_some(i)
            })
            .collect();
        self.applied_query = query;
        unsafe {
            SendMessageW(self.list, WM_SETREDRAW, 0, 0);
            SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
            for index in &self.filtered {
                let entry = &self.entries[*index];
                let label = if entry.shortcut.is_empty() {
                    entry.label.clone()
                } else {
                    format!("{}\t{}", entry.label, entry.shortcut)
                };
                SendMessageW(self.list, LB_ADDSTRING, 0, wide(label).as_ptr() as LPARAM);
            }
            if !self.filtered.is_empty() {
                SendMessageW(self.list, LB_SETCURSEL, 0, 0);
            }
            SendMessageW(self.list, WM_SETREDRAW, 1, 0);
            InvalidateRect(self.list, std::ptr::null(), 1);
            SetWindowTextW(
                self.status,
                wide(if self.filtered.is_empty() {
                    "No matching commands"
                } else {
                    "Enter to run · Esc to close"
                })
                .as_ptr(),
            );
        }
    }
    pub(super) fn status(&self, text: &str) {
        unsafe {
            SetWindowTextW(self.status, wide(text).as_ptr());
        }
    }
    pub(super) fn is_open(&self) -> bool {
        self.opened
    }
    pub(super) fn selected(&self) -> Option<Entry> {
        if !self.opened || guard(self.query).composing || self.applied_query != read(self.query) {
            return None;
        }
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, 0, 0) };
        if index < 0 {
            return None;
        }
        self.filtered
            .get(index as usize)
            .and_then(|i| self.entries.get(*i))
            .cloned()
    }
    fn release_owner(&mut self) {
        for owner in self.disabled.drain(..).rev() {
            if unsafe { IsWindow(owner) } != 0 {
                unsafe {
                    EnableWindow(owner, 1);
                }
            }
        }
    }
    pub(super) fn hide(&mut self, background: bool) {
        if !self.opened {
            return;
        }
        self.opened = false;
        unsafe {
            if IsWindowVisible(self.window) != 0 {
                ShowWindow(self.window, SW_HIDE);
            }
        }
        self.release_owner();
        if !background && !self.previous.is_null() {
            unsafe {
                if IsWindow(self.previous) != 0 && IsWindowEnabled(self.previous) != 0 {
                    SetFocus(self.previous);
                }
            }
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        if !self.opened
            || (message.hwnd != self.window && unsafe { IsChild(self.window, message.hwnd) } == 0)
        {
            return false;
        }
        let guard = guard(self.query);
        if guard.composing || guard.settling || message.wParam == 229 {
            return false;
        }
        if message.message == WM_KEYDOWN {
            match message.wParam {
                13 => {
                    emit(UiAction::Activate);
                    return true;
                }
                27 => {
                    emit(UiAction::Close);
                    return true;
                }
                38 | 40 => {
                    if !self.filtered.is_empty() {
                        unsafe {
                            let current =
                                SendMessageW(self.list, LB_GETCURSEL, 0, 0).max(0) as usize;
                            let next = if message.wParam == 38 {
                                current.saturating_sub(1)
                            } else {
                                (current + 1).min(self.filtered.len() - 1)
                            };
                            SendMessageW(self.list, LB_SETCURSEL, next, 0);
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }
        if message.message == WM_CHAR && matches!(message.wParam, 13 | 27) {
            return true;
        }
        unsafe { IsWindowVisible(self.window) != 0 && IsDialogMessageW(self.window, message) != 0 }
    }
    pub(super) fn layout(&self) {
        unsafe {
            let mut r = RECT::default();
            GetClientRect(self.window, &mut r);
            let dpi = GetDpiForWindow(self.window).max(96) as i32;
            let px = |v| v * dpi / 96;
            for (window, x, y, w, h) in [
                (self.query, px(12), px(12), r.right - px(24), px(32)),
                (
                    self.list,
                    px(12),
                    px(54),
                    r.right - px(24),
                    r.bottom - px(96),
                ),
                (
                    self.status,
                    px(12),
                    r.bottom - px(32),
                    r.right - px(24),
                    px(24),
                ),
            ] {
                if !window.is_null() {
                    SetWindowPos(
                        window,
                        std::ptr::null_mut(),
                        x,
                        y,
                        w.max(1),
                        h.max(1),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
            }
            SendMessageW(self.list, LB_SETITEMHEIGHT, 0, px(32) as LPARAM);
            let tab = (r.right - px(170)).max(px(80)) * 4 / px(7).max(1);
            SendMessageW(self.list, LB_SETTABSTOPS, 1, &tab as *const i32 as LPARAM);
        }
    }
    pub(super) fn diagnostics(&self) -> Value {
        let mut r = RECT::default();
        unsafe {
            GetWindowRect(self.window, &mut r);
        }
        let selected = unsafe { SendMessageW(self.list, LB_GETCURSEL, 0, 0) };
        let selected_index = (selected >= 0).then_some(selected as usize);
        json!({"window":self.window as usize,"owner":self.owner as usize,"open":self.opened,"modal":true,
            "native_visible":unsafe{IsWindowVisible(self.window)}!=0,"owner_enabled":unsafe{IsWindowEnabled(self.owner)}!=0,
            "query_handle":self.query as usize,"list_handle":self.list as usize,"query":read(self.query),"composing":guard(self.query).composing,"settling":guard(self.query).settling,
            "selected":self.selected().map(|e|e.id),"selected_index":selected_index,"empty":self.filtered.is_empty(),"status":read(self.status),"entries":self.entries.iter().map(|e|json!({"id":e.id,"label":e.label,"shortcut":e.shortcut})).collect::<Vec<_>>(),
            "filtered":self.filtered.iter().map(|i|self.entries[*i].id.clone()).collect::<Vec<_>>(),
            "rect":{"x":r.left,"y":r.top,"width":r.right-r.left,"height":r.bottom-r.top}})
    }
}
impl Drop for Panel {
    fn drop(&mut self) {
        self.release_owner();
        GUARDS.with(|g| g.borrow_mut().remove(&(self.query as isize)));
        unsafe {
            RemoveWindowSubclass(self.query, Some(query_proc), QUERY_SUBCLASS);
            RemoveWindowSubclass(self.list, Some(list_proc), LIST_SUBCLASS);
        }
        for child in [self.query, self.list, self.status] {
            chrome::unregister(child);
        }
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
