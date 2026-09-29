// SPDX-License-Identifier: GPL-3.0-or-later
//! Bell-anchored native notification popover. Preparing it never displays it.
use super::*;
use std::cell::{Cell, RefCell};
use windows_sys::Win32::System::SystemServices::{SS_CENTER, SS_LEFT};
use windows_sys::Win32::UI::{
    Controls::{
        SetScrollInfo, DRAWITEMSTRUCT, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_BUTTON,
    },
    Input::KeyboardAndMouse::GetFocus,
};

#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Show,
    Open(Uuid),
    Delete(Uuid),
    Clear,
    Close,
    Layout,
    Scroll(i32),
    ScrollTo(i32),
    Navigate(i32),
}
fn emit(action: UiAction) {
    post(Event::NotificationUi(action));
}
#[derive(Clone)]
struct RowPaint {
    id: Uuid,
    title: String,
    body: String,
    time: String,
    read: bool,
    attention: bool,
    caption: HWND,
    delete: bool,
}
thread_local! {
    static ROWS: RefCell<HashMap<isize, RowPaint>> = RefCell::new(HashMap::new());
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
        WM_ACTIVATE if wparam & 0xffff == WA_INACTIVE as usize => {
            emit(UiAction::Close);
        }
        WM_SIZE | WM_DPICHANGED => {
            // The viewport is laid out by its popup. Its own size notifications
            // must not enqueue another identical parent layout indefinitely.
            if GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_CHILD == 0 {
                emit(UiAction::Layout);
            }
            return 0;
        }
        WM_MOUSEWHEEL => {
            emit(UiAction::Scroll(-((wparam >> 16) as i16 as i32) * 48 / 120));
            return 0;
        }
        WM_VSCROLL => {
            let mut info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_TRACKPOS,
                ..Default::default()
            };
            GetScrollInfo(window, SB_VERT, &mut info);
            match (wparam & 0xffff) as i32 {
                SB_LINEUP => emit(UiAction::Scroll(-40)),
                SB_LINEDOWN => emit(UiAction::Scroll(40)),
                SB_PAGEUP => emit(UiAction::Scroll(-240)),
                SB_PAGEDOWN => emit(UiAction::Scroll(240)),
                SB_THUMBTRACK | SB_THUMBPOSITION => emit(UiAction::ScrollTo(info.nTrackPos)),
                SB_TOP => emit(UiAction::ScrollTo(0)),
                SB_BOTTOM => emit(UiAction::ScrollTo(i32::MAX)),
                _ => {}
            }
            return 0;
        }
        WM_COMMAND => {
            if (wparam >> 16) as u32 == BN_CLICKED {
                let row = ROWS.with(|rows| rows.borrow().get(&lparam).cloned());
                if let Some(row) = row {
                    emit(if row.delete {
                        UiAction::Delete(row.id)
                    } else {
                        UiAction::Open(row.id)
                    });
                } else if wparam & 0xffff == 5 {
                    emit(UiAction::Clear);
                } else if wparam & 0xffff == 2 {
                    emit(UiAction::Close);
                }
            }
            return 0;
        }
        WM_DRAWITEM if lparam != 0 && draw_row(&*(lparam as *const DRAWITEMSTRUCT)) => {
            return 1;
        }
        _ => {}
    }
    if let Some(result) = chrome::message(window, message, wparam, lparam) {
        return result;
    }
    DefWindowProcW(window, message, wparam, lparam)
}
fn drawing(text: &str) -> Vec<u16> {
    let original: Vec<u16> = text.encode_utf16().collect();
    chrome::caption_for_paint(&original).into_owned()
}
unsafe fn draw_row(item: &DRAWITEMSTRUCT) -> bool {
    if item.CtlType != ODT_BUTTON {
        return false;
    }
    let Some(row) = ROWS.with(|rows| rows.borrow().get(&(item.hwndItem as isize)).cloned()) else {
        return false;
    };
    if row.delete {
        return false; // Use the shared trash icon painter.
    }
    let saved = SaveDC(item.hDC);
    if saved == 0 {
        return false;
    }
    let palette = chrome::palette();
    let dpi = GetDpiForWindow(item.hwndItem).max(96);
    let px = |n: i32| (n * dpi as i32 + 48) / 96;
    let focused = item.itemState & (ODS_FOCUS | ODS_SELECTED) != 0;
    let bg = if focused {
        palette.hover
    } else {
        palette.surface
    };
    let fg = if palette.high_contrast && focused {
        GetSysColor(COLOR_HIGHLIGHTTEXT)
    } else if row.read && !palette.high_contrast {
        palette.muted
    } else {
        palette.foreground
    };
    SetDCBrushColor(item.hDC, bg);
    FillRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
    SetBkMode(item.hDC, TRANSPARENT as i32);
    SetTextColor(item.hDC, fg);
    let body_font = SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HGDIOBJ;
    SelectObject(item.hDC, body_font);
    let mut title = RECT {
        left: item.rcItem.left + px(10),
        top: item.rcItem.top + px(8),
        right: item.rcItem.right - px(8),
        bottom: item.rcItem.top + px(32),
    };
    if row.attention && !row.read && !palette.high_contrast {
        SetTextColor(item.hDC, palette.accent);
    }
    let text = drawing(&row.title);
    DrawTextW(
        item.hDC,
        text.as_ptr(),
        text.len() as i32,
        &mut title,
        DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    SetTextColor(item.hDC, fg);
    let mut body = RECT {
        top: item.rcItem.top + px(34),
        bottom: item.rcItem.bottom - px(29),
        ..title
    };
    let text = drawing(&row.body);
    DrawTextW(
        item.hDC,
        text.as_ptr(),
        text.len() as i32,
        &mut body,
        DT_WORDBREAK | DT_NOPREFIX,
    );
    SelectObject(
        item.hDC,
        SendMessageW(row.caption, WM_GETFONT, 0, 0) as HGDIOBJ,
    );
    if !palette.high_contrast {
        SetTextColor(item.hDC, palette.muted);
    }
    let mut time = RECT {
        top: item.rcItem.bottom - px(25),
        bottom: item.rcItem.bottom - px(5),
        ..title
    };
    let text = drawing(&row.time);
    DrawTextW(
        item.hDC,
        text.as_ptr(),
        text.len() as i32,
        &mut time,
        DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
    );
    if focused && item.itemState & ODS_NOFOCUSRECT == 0 {
        let mut focus = item.rcItem;
        InflateRect(&mut focus, -px(2), -px(2));
        DrawFocusRect(item.hDC, &focus);
    }
    RestoreDC(item.hDC, saved);
    true
}
struct RowControls {
    id: Uuid,
    open: HWND,
    delete: HWND,
    height: i32,
}
impl Drop for RowControls {
    fn drop(&mut self) {
        for window in [self.open, self.delete] {
            ROWS.with(|rows| {
                rows.borrow_mut().remove(&(window as isize));
            });
            chrome::unregister(window);
            unsafe {
                DestroyWindow(window);
            }
        }
    }
}
pub(super) struct Panel {
    pub(super) window: HWND,
    viewport: HWND,
    status: HWND,
    clear: HWND,
    empty: HWND,
    caption: HWND,
    values: RefCell<Vec<Value>>,
    rows: RefCell<Vec<RowControls>>,
    message: RefCell<String>,
    opened: Cell<bool>,
    offset: Cell<i32>,
    anchor: Cell<HWND>,
    previous_focus: Cell<HWND>,
}
impl Drop for Panel {
    fn drop(&mut self) {
        self.rows.get_mut().clear();
        for child in [self.status, self.clear, self.empty, self.caption] {
            chrome::unregister(child);
        }
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    pub(super) fn new(parent: HWND) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.notifications");
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
                "cannot register notification popover"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT | WS_EX_TOOLWINDOW,
                class.as_ptr(),
                wide("Notifications").as_ptr(),
                WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
                0,
                0,
                320,
                208,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create notification popover");
            let mut panel = Self {
                window,
                viewport: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                clear: std::ptr::null_mut(),
                empty: std::ptr::null_mut(),
                caption: std::ptr::null_mut(),
                values: RefCell::new(vec![]),
                rows: RefCell::new(vec![]),
                message: RefCell::new(String::new()),
                opened: Cell::new(false),
                offset: Cell::new(0),
                anchor: Cell::new(parent),
                previous_focus: Cell::new(std::ptr::null_mut()),
            };
            panel.viewport = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                std::ptr::null(),
                WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_CLIPCHILDREN,
                0,
                0,
                1,
                1,
                window,
                10 as HMENU,
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(
                !panel.viewport.is_null(),
                "cannot create notification viewport"
            );
            panel.status = panel.child(window, "STATIC", "", 11, SS_LEFT)?;
            panel.clear = panel.child(
                window,
                "BUTTON",
                "All Clear",
                5,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.empty = panel.child(
                panel.viewport,
                "STATIC",
                "No notifications yet.",
                12,
                SS_CENTER,
            )?;
            panel.caption = panel.child(window, "STATIC", "", 13, 0)?;
            chrome::register_control(panel.status, chrome::ControlRole::Static);
            chrome::register_control(panel.empty, chrome::ControlRole::Static);
            chrome::register_control(panel.caption, chrome::ControlRole::Caption);
            chrome::register_button(panel.clear, chrome::Role::Button);
            ShowWindow(panel.caption, SW_HIDE);
            panel.layout();
            Ok(panel)
        }
    }
    fn child(
        &self,
        parent: HWND,
        class: &str,
        text: &str,
        id: usize,
        style: u32,
    ) -> anyhow::Result<HWND> {
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
            checked((!window.is_null()) as i32)?;
            Ok(window)
        }
    }
    fn px(&self, value: i32) -> i32 {
        (value * unsafe { GetDpiForWindow(self.window).max(96) } as i32 + 48) / 96
    }
    fn position(&self) {
        unsafe {
            let mut anchor = RECT::default();
            GetWindowRect(self.anchor.get(), &mut anchor);
            let height = self
                .rows
                .borrow()
                .iter()
                .map(|row| row.height)
                .sum::<i32>()
                .clamp(self.px(160), self.px(420))
                + self.px(48);
            let width = self.px(320);
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let monitor_handle = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
            let valid = GetMonitorInfoW(monitor_handle, &mut monitor) != 0;
            let mut x = anchor.right - width + self.px(6);
            let mut y = anchor.bottom + self.px(6);
            if valid {
                x = x.clamp(
                    monitor.rcWork.left,
                    (monitor.rcWork.right - width).max(monitor.rcWork.left),
                );
                if y + height > monitor.rcWork.bottom {
                    y = anchor.top - height - self.px(6);
                }
                y = y.clamp(
                    monitor.rcWork.top,
                    (monitor.rcWork.bottom - height).max(monitor.rcWork.top),
                );
            }
            SetWindowPos(
                self.window,
                std::ptr::null_mut(),
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.layout();
    }
    pub(super) fn layout(&self) {
        unsafe {
            let mut client = RECT::default();
            GetClientRect(self.window, &mut client);
            let width = (client.right - self.px(12)).max(1);
            let height = (client.bottom - self.px(48)).max(1);
            SetWindowPos(
                self.status,
                std::ptr::null_mut(),
                self.px(10),
                self.px(10),
                (width - self.px(90)).max(1),
                self.px(26),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            SetWindowPos(
                self.clear,
                std::ptr::null_mut(),
                client.right - self.px(88),
                self.px(6),
                self.px(80),
                self.px(30),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            SetWindowPos(
                self.viewport,
                std::ptr::null_mut(),
                self.px(6),
                self.px(40),
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            let rows = self.rows.borrow();
            let total = rows.iter().map(|row| row.height).sum::<i32>();
            let offset = self.offset.get().clamp(0, (total - height).max(0));
            self.offset.set(offset);
            let info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: (total - 1).max(0),
                nPage: height as u32,
                nPos: offset,
                ..Default::default()
            };
            SetScrollInfo(self.viewport, SB_VERT, &info, 1);
            let mut area = RECT::default();
            GetClientRect(self.viewport, &mut area);
            SetWindowPos(
                self.empty,
                std::ptr::null_mut(),
                self.px(8),
                self.px(30),
                (area.right - self.px(16)).max(1),
                self.px(40),
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            ShowWindow(
                self.empty,
                if rows.is_empty() { SW_SHOWNA } else { SW_HIDE },
            );
            ShowWindow(
                self.clear,
                if rows.is_empty() { SW_HIDE } else { SW_SHOWNA },
            );
            let mut y = -offset;
            for row in rows.iter() {
                let visible = y + row.height > 0 && y < height;
                if visible {
                    SetWindowPos(
                        row.open,
                        std::ptr::null_mut(),
                        0,
                        y,
                        (area.right - self.px(30)).max(1),
                        row.height - self.px(4),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    SetWindowPos(
                        row.delete,
                        std::ptr::null_mut(),
                        area.right - self.px(30),
                        y + (row.height - self.px(32)) / 2,
                        self.px(28),
                        self.px(28),
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                ShowWindow(row.open, if visible { SW_SHOWNA } else { SW_HIDE });
                ShowWindow(row.delete, if visible { SW_SHOWNA } else { SW_HIDE });
                y += row.height;
            }
            InvalidateRect(self.viewport, std::ptr::null(), 1);
        }
    }
    pub(super) fn scroll(&self, offset: i32) {
        self.offset.set(offset);
        self.layout();
    }
    pub(super) fn scroll_by(&self, delta: i32) {
        self.scroll(self.offset.get().saturating_add(self.px(delta)));
    }
    pub(super) fn navigate(&self, direction: i32) {
        let target = {
            let rows = self.rows.borrow();
            if rows.is_empty() {
                return;
            }
            let focus = unsafe { GetFocus() };
            let current = rows
                .iter()
                .position(|row| row.open == focus || row.delete == focus)
                .map_or(-1, |index| index as i32);
            let next = (current + direction).clamp(0, rows.len() as i32 - 1) as usize;
            let top = rows.iter().take(next).map(|row| row.height).sum::<i32>();
            (rows[next].open, top, rows[next].height)
        };
        let mut area = RECT::default();
        unsafe {
            GetClientRect(self.viewport, &mut area);
        }
        if target.1 < self.offset.get() || target.1 + target.2 > self.offset.get() + area.bottom {
            self.scroll(target.1);
        }
        unsafe {
            SetFocus(target.0);
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if IsWindowVisible(self.window) == 0 {
                return false;
            }
            if message.hwnd == self.window || IsChild(self.window, message.hwnd) != 0 {
                if message.message == WM_KEYDOWN && message.wParam == 0x1b {
                    emit(UiAction::Close);
                    return true;
                }
                if message.message == WM_KEYDOWN {
                    let action = match message.wParam {
                        0x26 => Some(UiAction::Navigate(-1)),
                        0x28 => Some(UiAction::Navigate(1)),
                        0x21 => Some(UiAction::Scroll(-240)),
                        0x22 => Some(UiAction::Scroll(240)),
                        _ => None,
                    };
                    if let Some(action) = action {
                        emit(action);
                        return true;
                    }
                }
                return IsDialogMessageW(self.window, message) != 0;
            }
            false
        }
    }
    pub(super) fn show(&self, background: bool, anchor: HWND) {
        self.anchor.set(anchor);
        self.offset.set(0);
        self.opened.set(true);
        self.position();
        if !background {
            unsafe {
                self.previous_focus.set(GetFocus());
                ShowWindow(self.window, SW_SHOW);
                let first = self
                    .rows
                    .borrow()
                    .first()
                    .map_or(self.window, |row| row.open);
                SetFocus(first);
            }
        }
    }
    pub(super) fn hide(&self) {
        self.opened.set(false);
        unsafe {
            let focus = GetFocus();
            let restore = IsWindowVisible(self.window) != 0
                && (focus == self.window || IsChild(self.window, focus) != 0);
            ShowWindow(self.window, SW_HIDE);
            if restore && IsWindow(self.previous_focus.get()) != 0 {
                SetFocus(self.previous_focus.get());
            }
        }
    }
    pub(super) fn status(&self, text: &str) {
        *self.message.borrow_mut() = text.to_owned();
        unsafe {
            SetWindowTextW(self.status, wide(text).as_ptr());
        }
    }
    pub(super) fn status_text(&self) -> String {
        self.message.borrow().clone()
    }
    pub(super) fn rows(&self) -> usize {
        self.rows.borrow().len()
    }
    pub(super) fn results(&self, values: &[Value]) {
        let values: Vec<_> = values.iter().rev().cloned().collect();
        if *self.values.borrow() == values {
            return;
        }
        self.rows.borrow_mut().clear();
        *self.values.borrow_mut() = values.clone();
        for (index, value) in values.iter().enumerate() {
            if let Err(error) = self.add_row(value, index) {
                self.status(&error.to_string());
                return;
            }
        }
        self.status("");
        if self.opened.get() {
            self.position();
        } else {
            self.layout();
        }
    }
    fn add_row(&self, value: &Value, index: usize) -> anyhow::Result<()> {
        let id = Uuid::parse_str(value["id"].as_str().context("notification has no id")?)?;
        let title = value["title"].as_str().unwrap_or("").to_owned();
        let body = value["body"].as_str().unwrap_or("").to_owned();
        let time = value["created_at"]
            .as_str()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
            .map(|time| {
                time.with_timezone(&chrono::Local)
                    .format("%H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        let open = self.child(
            self.viewport,
            "BUTTON",
            &format!("{title}\n{body}\n{time}"),
            1000 + index * 2,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        let delete = match self.child(
            self.viewport,
            "BUTTON",
            &format!("Delete notification: {title}"),
            1001 + index * 2,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        ) {
            Ok(window) => window,
            Err(error) => {
                unsafe {
                    DestroyWindow(open);
                }
                return Err(error);
            }
        };
        chrome::register_button(open, chrome::Role::Button);
        chrome::register_button(
            delete,
            chrome::Role::Icon {
                kind: chrome::ChromeIcon::Delete,
                marked: false,
            },
        );
        let paint = RowPaint {
            id,
            title,
            body,
            time,
            read: value["read"] == true,
            attention: matches!(value["level"].as_str(), Some("needs_input" | "error")),
            caption: self.caption,
            delete: false,
        };
        let height = unsafe {
            let dc = GetDC(open);
            let mut height = self.px(96);
            if !dc.is_null() {
                let old = SelectObject(dc, SendMessageW(open, WM_GETFONT, 0, 0) as HGDIOBJ);
                let text = drawing(&paint.body);
                let mut rect = RECT {
                    right: self.px(236),
                    ..Default::default()
                };
                DrawTextW(
                    dc,
                    text.as_ptr(),
                    text.len() as i32,
                    &mut rect,
                    DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
                );
                height = (rect.bottom + self.px(70)).max(self.px(96));
                SelectObject(dc, old);
                ReleaseDC(open, dc);
            }
            height
        };
        ROWS.with(|rows| {
            let mut rows = rows.borrow_mut();
            rows.insert(open as isize, paint.clone());
            rows.insert(
                delete as isize,
                RowPaint {
                    delete: true,
                    ..paint
                },
            );
        });
        self.rows.borrow_mut().push(RowControls {
            id,
            open,
            delete,
            height,
        });
        Ok(())
    }
    #[cfg(debug_assertions)]
    pub(super) fn capture_window(&self) -> Option<HWND> {
        self.opened.get().then_some(self.window)
    }
    pub(super) fn snapshot(&self) -> Value {
        unsafe {
            let mut rect = RECT::default();
            let mut anchor = RECT::default();
            GetWindowRect(self.window, &mut rect);
            GetWindowRect(self.anchor.get(), &mut anchor);
            let values = self.values.borrow();
            let rows: Vec<_> = self.rows.borrow().iter().map(|row| {
                let value = values.iter().find(|value| value["id"].as_str().is_some_and(|id| id == row.id.to_string()));
                let paint = ROWS.with(|rows| rows.borrow().get(&(row.open as isize)).cloned());
                json!({"id":row.id,"read":value.map(|v|v["read"].clone()),
                    "title":value.map(|v|v["title"].clone()),"body":value.map(|v|v["body"].clone()),
                    "time":paint.map(|p|p.time),"open_handle":row.open as usize,"delete_handle":row.delete as usize,
                    "height":row.height,"visible":GetWindowLongPtrW(row.open,GWL_STYLE) as u32 & WS_VISIBLE != 0})
            }).collect();
            json!({"kind":"bell-popover","open":self.opened.get(),"visible":IsWindowVisible(self.window)!=0,
                "rect":{"x":rect.left,"y":rect.top,"width":rect.right-rect.left,"height":rect.bottom-rect.top},
                "anchor_handle":self.anchor.get() as usize,
                "anchor_rect":{"x":anchor.left,"y":anchor.top,"width":anchor.right-anchor.left,"height":anchor.bottom-anchor.top},
                "scroll_offset":self.offset.get(),"rows":rows,"empty":rows.is_empty(),
                "clear_handle":self.clear as usize,"viewport_handle":self.viewport as usize})
        }
    }
}
