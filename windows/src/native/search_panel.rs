// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned modal search, native EDIT/IME and accessible two-line LISTBOX results.
//! Modality disables only the owner we enabled previously; no nested message loop.
use super::*;
use std::cell::{Cell, RefCell};
use windows_sys::Win32::System::SystemServices::SS_NOPREFIX;
use windows_sys::Win32::UI::{
    Controls::{
        DRAWITEMSTRUCT, EM_LIMITTEXT, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_LISTBOX,
    },
    Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled},
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};
const QUERY_SUBCLASS: usize = 0x464d_5351;
#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Show,
    Changed,
    Refresh,
    More,
    Open,
    Close,
    Layout,
    Tick,
}
fn emit(action: UiAction) {
    post(Event::SearchUi(action));
}
#[derive(Clone)]
struct ResultPaint {
    caption: HWND,
    rows: Vec<(String, String)>,
}
thread_local! {
    static COMPOSING: Cell<bool> = const { Cell::new(false) };
    static SETTING_QUERY: Cell<bool> = const { Cell::new(false) };
    static PAINT: RefCell<HashMap<isize, ResultPaint>> = RefCell::new(HashMap::new());
}
unsafe extern "system" fn query_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match message {
        WM_IME_STARTCOMPOSITION => {
            COMPOSING.with(|v| v.set(true));
            emit(UiAction::Changed);
        }
        WM_IME_ENDCOMPOSITION => {
            COMPOSING.with(|v| v.set(false));
            emit(UiAction::Changed);
        }
        WM_NCDESTROY => {
            COMPOSING.with(|v| v.set(false));
            RemoveWindowSubclass(window, Some(query_proc), QUERY_SUBCLASS);
        }
        _ => {}
    }
    DefSubclassProc(window, message, wparam, lparam)
}
unsafe extern "system" fn procedure(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            emit(UiAction::Close);
            return 0;
        }
        WM_SIZE => {
            emit(UiAction::Layout);
            return 0;
        }
        WM_TIMER => {
            KillTimer(window, 2);
            emit(UiAction::Tick);
            return 0;
        }
        WM_GETMINMAXINFO if lparam != 0 => {
            let info = &mut *(lparam as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(window).max(96) as i32;
            info.ptMinTrackSize.x = 400 * dpi / 96;
            info.ptMinTrackSize.y = 300 * dpi / 96;
            return 0;
        }
        WM_DPICHANGED => {
            let rect = &*(lparam as *const RECT);
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            emit(UiAction::Layout);
            return 0;
        }
        WM_COMMAND => {
            let action = match (wparam & 0xffff, (wparam >> 16) as u32) {
                (10, EN_CHANGE) if !SETTING_QUERY.with(Cell::get) => Some(UiAction::Changed),
                (11, BN_CLICKED) => Some(UiAction::Changed),
                (1, BN_CLICKED) => Some(UiAction::Refresh),
                (20, LBN_DBLCLK) => Some(UiAction::Open),
                (31, BN_CLICKED) => Some(UiAction::More),
                (2, BN_CLICKED) => Some(UiAction::Close),
                _ => None,
            };
            if let Some(action) = action {
                emit(action);
            }
            return 0;
        }
        WM_DRAWITEM if lparam != 0 && draw_result(&*(lparam as *const DRAWITEMSTRUCT)) => return 1,
        _ => {}
    }
    if let Some(value) = chrome::message(window, message, wparam, lparam) {
        return value;
    }
    DefWindowProcW(window, message, wparam, lparam)
}
unsafe fn draw_result(item: &DRAWITEMSTRUCT) -> bool {
    if item.CtlType != ODT_LISTBOX {
        return false;
    }
    let row = PAINT.with(|paint| {
        paint
            .borrow()
            .get(&(item.hwndItem as isize))
            .and_then(|paint| {
                paint
                    .rows
                    .get(item.itemID as usize)
                    .map(|row| (paint.caption, row.clone()))
            })
    });
    let Some((caption, (location, preview))) = row else {
        return true;
    };
    let saved = SaveDC(item.hDC);
    if saved == 0 {
        return false;
    }
    let palette = chrome::palette();
    let selected = item.itemState & ODS_SELECTED != 0;
    let bg = if selected {
        palette.selected
    } else {
        palette.surface
    };
    let foreground = if selected && palette.high_contrast {
        GetSysColor(COLOR_HIGHLIGHTTEXT)
    } else {
        palette.foreground
    };
    SetDCBrushColor(item.hDC, bg);
    FillRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
    SetBkMode(item.hDC, TRANSPARENT as i32);
    let dpi = GetDpiForWindow(item.hwndItem).max(96) as i32;
    let px = |n| n * dpi / 96;
    let mut rect = RECT {
        left: item.rcItem.left + px(10),
        right: item.rcItem.right - px(10),
        top: item.rcItem.top + px(6),
        bottom: item.rcItem.top + px(26),
    };
    SelectObject(item.hDC, SendMessageW(caption, WM_GETFONT, 0, 0) as HGDIOBJ);
    SetTextColor(
        item.hDC,
        if palette.high_contrast {
            foreground
        } else {
            palette.muted
        },
    );
    let original: Vec<u16> = location.encode_utf16().collect();
    let text = chrome::caption_for_paint(&original);
    DrawTextW(
        item.hDC,
        text.as_ptr(),
        text.len() as i32,
        &mut rect,
        DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    SelectObject(
        item.hDC,
        SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HGDIOBJ,
    );
    SetTextColor(item.hDC, foreground);
    rect.top = item.rcItem.top + px(28);
    rect.bottom = item.rcItem.bottom - px(6);
    let original: Vec<u16> = preview.encode_utf16().collect();
    let text = chrome::caption_for_paint(&original);
    DrawTextW(
        item.hDC,
        text.as_ptr(),
        text.len() as i32,
        &mut rect,
        DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
        DrawFocusRect(item.hDC, &item.rcItem);
    }
    RestoreDC(item.hDC, saved);
    true
}
pub(super) struct Panel {
    pub(super) window: HWND,
    owner: HWND,
    opened: Cell<bool>,
    disabled_owner: Cell<bool>,
    query: HWND,
    case: HWND,
    refresh: HWND,
    status: HWND,
    list: HWND,
    more: HWND,
    caption: HWND,
    labels: RefCell<Vec<String>>,
    status_text: RefCell<String>,
    has_more: Cell<bool>,
}
impl Drop for Panel {
    fn drop(&mut self) {
        self.release_owner();
        PAINT.with(|paint| {
            paint.borrow_mut().remove(&(self.list as isize));
        });
        unsafe {
            RemoveWindowSubclass(self.query, Some(query_proc), QUERY_SUBCLASS);
        }
        for window in [
            self.query,
            self.case,
            self.refresh,
            self.status,
            self.list,
            self.more,
            self.caption,
        ] {
            chrome::unregister(window);
        }
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    pub(super) fn new(parent: HWND) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.output-search");
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
                "cannot register search window"
            );
            let dpi = GetDpiForWindow(parent).max(96) as i32;
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Search all terminals").as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                760 * dpi / 96,
                520 * dpi / 96,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create search window");
            let mut panel = Self {
                window,
                owner: parent,
                opened: Cell::new(false),
                disabled_owner: Cell::new(false),
                query: std::ptr::null_mut(),
                case: std::ptr::null_mut(),
                refresh: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                list: std::ptr::null_mut(),
                more: std::ptr::null_mut(),
                caption: std::ptr::null_mut(),
                labels: RefCell::new(vec![]),
                status_text: RefCell::new(String::new()),
                has_more: Cell::new(false),
            };
            panel.query = panel.child(
                "EDIT",
                "",
                10,
                WS_BORDER | WS_TABSTOP | ES_AUTOHSCROLL as u32,
            )?;
            SendMessageW(panel.query, EM_LIMITTEXT, 1024, 0);
            SendMessageW(
                panel.query,
                0x1501,
                1,
                wide("Search terminal output…").as_ptr() as LPARAM,
            );
            anyhow::ensure!(
                SetWindowSubclass(panel.query, Some(query_proc), QUERY_SUBCLASS, 0) != 0,
                "cannot preserve search input composition"
            );
            panel.case = panel.child(
                "BUTTON",
                "Match case",
                11,
                WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            )?;
            panel.refresh =
                panel.child("BUTTON", "Refresh", 1, WS_TABSTOP | BS_OWNERDRAW as u32)?;
            panel.status = panel.child("STATIC", "", 12, SS_NOPREFIX)?;
            panel.list = panel.child(
                "LISTBOX",
                "",
                20,
                WS_TABSTOP
                    | WS_VSCROLL
                    | LBS_NOTIFY as u32
                    | LBS_NOINTEGRALHEIGHT as u32
                    | LBS_OWNERDRAWFIXED as u32
                    | LBS_HASSTRINGS as u32,
            )?;
            panel.more = panel.child(
                "BUTTON",
                "Show more results",
                31,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.caption = panel.child("STATIC", "", 13, 0)?;
            chrome::register_control(panel.caption, chrome::ControlRole::Caption);
            ShowWindow(panel.caption, SW_HIDE);
            PAINT.with(|paint| {
                paint.borrow_mut().insert(
                    panel.list as isize,
                    ResultPaint {
                        caption: panel.caption,
                        rows: vec![],
                    },
                );
            });
            panel.layout();
            panel.status("Search retained output in all workspaces in this window");
            panel.buttons(false, false, false);
            Ok(panel)
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
            if class == "BUTTON" && style & 0xf == BS_OWNERDRAW as u32 {
                chrome::register_button(window, chrome::Role::Button);
            } else {
                chrome::register_control(
                    window,
                    match class {
                        "LISTBOX" => chrome::ControlRole::Listbox,
                        "EDIT" => chrome::ControlRole::Edit,
                        _ => chrome::ControlRole::Static,
                    },
                );
            }
            Ok(window)
        }
    }
    pub(super) fn layout(&self) {
        unsafe {
            let mut rect = RECT::default();
            GetClientRect(self.window, &mut rect);
            let dpi = GetDpiForWindow(self.window).max(96) as i32;
            let px = |v| v * dpi / 96;
            let footer = if self.has_more.get() { px(48) } else { px(12) };
            for (window, x, y, width, height) in [
                (
                    self.query,
                    px(12),
                    px(12),
                    (rect.right - px(224)).max(1),
                    px(30),
                ),
                (self.case, rect.right - px(200), px(12), px(112), px(30)),
                (self.refresh, rect.right - px(80), px(12), px(68), px(30)),
                (
                    self.status,
                    px(12),
                    px(50),
                    (rect.right - px(24)).max(1),
                    px(36),
                ),
                (
                    self.list,
                    px(12),
                    px(90),
                    (rect.right - px(24)).max(1),
                    (rect.bottom - px(90) - footer).max(1),
                ),
                (
                    self.more,
                    px(12),
                    rect.bottom - px(40),
                    (rect.right - px(24)).max(1),
                    px(28),
                ),
            ] {
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
            }
            SendMessageW(self.list, LB_SETITEMHEIGHT, 0, px(60) as LPARAM);
            ShowWindow(
                self.more,
                if self.has_more.get() {
                    SW_SHOWNA
                } else {
                    SW_HIDE
                },
            );
        }
    }
    pub(super) fn show(&self, background: bool) {
        if !self.opened.replace(true) {
            unsafe {
                if IsWindowEnabled(self.owner) != 0 {
                    self.disabled_owner.set(true);
                    EnableWindow(self.owner, 0);
                }
                let mut owner = RECT::default();
                let mut rect = RECT::default();
                GetWindowRect(self.owner, &mut owner);
                GetWindowRect(self.window, &mut rect);
                SetWindowPos(
                    self.window,
                    std::ptr::null_mut(),
                    owner.left + ((owner.right - owner.left) - (rect.right - rect.left)) / 2,
                    owner.top + ((owner.bottom - owner.top) - (rect.bottom - rect.top)) / 2,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        if !background {
            unsafe {
                ShowWindow(self.window, SW_SHOW);
                SetFocus(self.query);
            }
        }
    }
    fn release_owner(&self) {
        if self.disabled_owner.replace(false) && unsafe { IsWindow(self.owner) } != 0 {
            unsafe {
                EnableWindow(self.owner, 1);
            }
        }
    }
    pub(super) fn schedule(&self) {
        unsafe {
            SetTimer(self.window, 2, 200, None);
        }
    }
    pub(super) fn hide(&self) {
        self.opened.set(false);
        self.release_owner();
        unsafe {
            KillTimer(self.window, 2);
            ShowWindow(self.window, SW_HIDE);
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        // Preserve native composition commit/cancel; never translate EDIT Enter
        // through IsDialogMessage into the Refresh/default button action.
        if message.hwnd == self.query
            && matches!(message.message, WM_KEYDOWN | WM_KEYUP | WM_CHAR)
            && matches!(message.wParam, 13 | 27)
        {
            return false;
        }
        unsafe {
            IsWindowVisible(self.window) != 0
                && (message.hwnd == self.window || IsChild(self.window, message.hwnd) != 0)
                && IsDialogMessageW(self.window, message) != 0
        }
    }
    pub(super) fn composing(&self) -> bool {
        COMPOSING.with(Cell::get)
    }
    pub(super) fn query(&self, text: &str, case: bool) {
        if self.composing() {
            return;
        }
        let current = self.read_query();
        SETTING_QUERY.with(|value| value.set(true));
        unsafe {
            if current.0 != text {
                SetWindowTextW(self.query, wide(text).as_ptr());
            }
            if current.1 != case {
                SendMessageW(self.case, BM_SETCHECK, usize::from(case), 0);
            }
        }
        SETTING_QUERY.with(|value| value.set(false));
    }
    pub(super) fn read_query(&self) -> (String, bool) {
        unsafe {
            let mut text = vec![0u16; GetWindowTextLengthW(self.query).max(0) as usize + 1];
            let count = GetWindowTextW(self.query, text.as_mut_ptr(), text.len() as i32);
            (
                String::from_utf16_lossy(&text[..count.max(0) as usize]),
                SendMessageW(self.case, BM_GETCHECK, 0, 0) == 1,
            )
        }
    }
    pub(super) fn status(&self, text: &str) {
        if *self.status_text.borrow() != text {
            unsafe {
                SetWindowTextW(self.status, wide(text).as_ptr());
            }
            *self.status_text.borrow_mut() = text.into();
        }
    }
    pub(super) fn results(&self, hits: &[Hit]) {
        let rows: Vec<_> = hits
            .iter()
            .map(|hit| {
                (
                    format!("{} / {}", hit.workspace, hit.title),
                    hit.found.preview.clone(),
                )
            })
            .collect();
        let labels: Vec<_> = rows
            .iter()
            .map(|(location, preview)| format!("{location}\n{preview}"))
            .collect();
        if *self.labels.borrow() == labels {
            return;
        }
        PAINT.with(|paint| {
            paint.borrow_mut().insert(
                self.list as isize,
                ResultPaint {
                    caption: self.caption,
                    rows,
                },
            );
        });
        unsafe {
            let selected = self
                .selected()
                .unwrap_or(0)
                .min(labels.len().saturating_sub(1));
            let top = SendMessageW(self.list, LB_GETTOPINDEX, 0, 0);
            SendMessageW(self.list, WM_SETREDRAW, 0, 0);
            SendMessageW(self.list, LB_RESETCONTENT, 0, 0);
            for label in &labels {
                SendMessageW(self.list, LB_ADDSTRING, 0, wide(label).as_ptr() as LPARAM);
            }
            if !labels.is_empty() {
                SendMessageW(self.list, LB_SETCURSEL, selected, 0);
                SendMessageW(self.list, LB_SETTOPINDEX, top.max(0) as usize, 0);
            }
            SendMessageW(self.list, WM_SETREDRAW, 1, 0);
            InvalidateRect(self.list, std::ptr::null(), 1);
        }
        *self.labels.borrow_mut() = labels;
    }
    pub(super) fn buttons(&self, _previous: bool, more: bool, open: bool) {
        let changed = self.has_more.replace(more) != more;
        unsafe {
            EnableWindow(self.more, i32::from(more));
            EnableWindow(self.list, i32::from(open));
        }
        if changed {
            self.layout();
        }
    }
    pub(super) fn rows(&self) -> usize {
        unsafe { SendMessageW(self.list, LB_GETCOUNT, 0, 0).max(0) as usize }
    }
    pub(super) fn selected(&self) -> Option<usize> {
        let index = unsafe { SendMessageW(self.list, LB_GETCURSEL, 0, 0) };
        (index >= 0).then_some(index as usize)
    }
    pub(super) fn diagnostics(&self) -> Value {
        let (query, match_case) = self.read_query();
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(self.window, &mut rect);
        }
        json!({"window":self.window as usize,"owner":self.owner as usize,"modal":true,"open":self.opened.get(),"owner_enabled":unsafe{IsWindowEnabled(self.owner)}!=0,"native_visible":unsafe{IsWindowVisible(self.window)}!=0,
            "query":query,"match_case":match_case,"composing":self.composing(),"query_handle":self.query as usize,"case_handle":self.case as usize,"refresh_handle":self.refresh as usize,"list_handle":self.list as usize,"more_handle":self.more as usize,"more_visible":self.has_more.get(),"rows":self.rows(),"status":self.status_text.borrow().clone(),"rect":{"x":rect.left,"y":rect.top,"width":rect.right-rect.left,"height":rect.bottom-rect.top},"labels":self.labels.borrow().clone()})
    }
}
