// SPDX-License-Identifier: GPL-3.0-or-later
//! Native tab menus retain stable entries and generation-tagged semantic actions.
use super::*;
use std::{cell::Cell, rc::Rc};
use windows_sys::Win32::UI::{
    Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODT_BUTTON},
    Input::KeyboardAndMouse::GetFocus,
};

#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Choose(usize),
    Close,
    Back,
    Dismiss(bool),
}
thread_local! {
    static ROUTES: RefCell<HashMap<isize, Uuid>> = RefCell::new(HashMap::new());
    static VIEWPORTS: RefCell<HashMap<isize, Viewport>> = RefCell::new(HashMap::new());
}
#[derive(Clone)]
struct Viewport {
    buttons: Vec<HWND>,
    first: usize,
    visible: usize,
    row_height: i32,
    margin: i32,
    width: i32,
    selected: Rc<Cell<Option<usize>>>,
    background: bool,
    move_index: Option<usize>,
}
fn layout_visible(window: HWND, index: usize) -> bool {
    VIEWPORTS.with(|views| {
        views
            .borrow()
            .get(&(window as isize))
            .is_some_and(|view| index >= view.first && index < view.first + view.visible)
    })
}
fn scroll(window: HWND, delta: i32, reveal: Option<usize>) {
    let view = VIEWPORTS.with(|views| {
        let mut views = views.borrow_mut();
        let view = views.get_mut(&(window as isize))?;
        let maximum = view.buttons.len().saturating_sub(view.visible);
        view.first = (view.first as i64 + i64::from(delta)).clamp(0, maximum as i64) as usize;
        if let Some(index) = reveal {
            if index < view.first {
                view.first = index;
            } else if index >= view.first + view.visible {
                view.first = index + 1 - view.visible;
            }
            view.first = view.first.min(maximum);
        }
        Some(view.clone())
    });
    if let Some(view) = view {
        // Release thread-local borrows before synchronous window callbacks.
        for (index, button) in view.buttons.iter().enumerate() {
            let visible = index >= view.first && index < view.first + view.visible;
            let y = if visible {
                view.margin + (index - view.first) as i32 * view.row_height
            } else {
                -view.row_height - view.margin
            };
            unsafe {
                SetWindowPos(
                    *button,
                    std::ptr::null_mut(),
                    view.margin,
                    y,
                    view.width,
                    view.row_height,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
        if delta != 0 {
            let end = (view.first + view.visible).min(view.buttons.len());
            if view
                .selected
                .get()
                .is_none_or(|index| index < view.first || index >= end)
            {
                let selected = (view.first..end)
                    .find(|index| unsafe { IsWindowEnabled(view.buttons[*index]) } != 0);
                view.selected.set(selected);
                if !view.background {
                    if let Some(index) = selected {
                        unsafe {
                            SetFocus(view.buttons[index]);
                        }
                    }
                }
            }
        }
        unsafe {
            InvalidateRect(window, std::ptr::null(), 1);
        }
    }
}
fn emit(window: HWND, action: UiAction) {
    let id = ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied());
    if let Some(id) = id {
        post(Event::TabMenu(id, action));
    }
}
unsafe fn owned_descendant(window: HWND, mut active: HWND) -> bool {
    if active.is_null() {
        return false;
    }
    active = GetAncestor(active, GA_ROOT);
    // Win32 owns the acyclic owner chain. Bound traversal defensively.
    for _ in 0..64 {
        if active == window {
            return true;
        }
        active = GetWindow(active, GW_OWNER);
        if active.is_null() {
            break;
        }
    }
    false
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_CLOSE => {
            emit(window, UiAction::Close);
            return 0;
        }
        WM_ACTIVATE
            if w & 0xffff == WA_INACTIVE as usize && !owned_descendant(window, l as HWND) =>
        {
            emit(
                window,
                UiAction::Dismiss(GetAncestor(l as HWND, GA_ROOT) == GetWindow(window, GW_OWNER)),
            );
        }
        WM_DRAWITEM if l != 0 => {
            if let Some(result) = chrome::message(window, message, w, l) {
                draw_move_arrow(window, &*(l as *const DRAWITEMSTRUCT));
                return result;
            }
        }
        WM_MOUSEWHEEL => {
            let delta = (w >> 16) as u16 as i16 as i32;
            if delta != 0 {
                let rows = if delta.abs() < 120 {
                    -delta.signum()
                } else {
                    -delta / 120 * 3
                };
                scroll(window, rows, None);
            }
            return 0;
        }
        WM_COMMAND => {
            let button = l as HWND;
            let id = (w & 0xffff) as i32;
            if (w >> 16) as u32 == BN_CLICKED
                && !button.is_null()
                && id >= 100
                && GetParent(button) == window
                && GetDlgItem(window, id) == button
                && IsWindowEnabled(window) != 0
                && IsWindowEnabled(button) != 0
                && layout_visible(window, (id - 100) as usize)
            {
                emit(window, UiAction::Choose((id - 100) as usize));
            }
            return 0;
        }
        WM_NCDESTROY => {
            ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
            VIEWPORTS.with(|views| views.borrow_mut().remove(&(window as isize)));
        }
        _ => {}
    }
    if let Some(result) = chrome::message(window, message, w, l) {
        return result;
    }
    DefWindowProcW(window, message, w, l)
}

unsafe fn draw_move_arrow(window: HWND, item: &DRAWITEMSTRUCT) {
    let is_move = VIEWPORTS.with(|views| {
        views.borrow().get(&(window as isize)).is_some_and(|view| {
            view.move_index
                .and_then(|index| view.buttons.get(index))
                .copied()
                == Some(item.hwndItem)
        })
    });
    if item.CtlType != ODT_BUTTON || !is_move {
        return;
    }
    let saved = SaveDC(item.hDC);
    if saved == 0 {
        return;
    }
    let dpi = GetDpiForWindow(item.hwndItem).max(96) as i32;
    let px = |value: i32| (value * dpi + 48) / 96;
    let x = item.rcItem.right - px(12);
    let y = (item.rcItem.top + item.rcItem.bottom) / 2;
    let palette = chrome::palette();
    let color = if item.itemState & ODS_DISABLED != 0 {
        palette.muted
    } else if palette.high_contrast && GetPixel(item.hDC, x, y) == GetSysColor(COLOR_HIGHLIGHT) {
        GetSysColor(COLOR_HIGHLIGHTTEXT)
    } else {
        palette.foreground
    };
    SelectObject(item.hDC, GetStockObject(DC_PEN));
    SetDCPenColor(item.hDC, color);
    MoveToEx(item.hDC, x - px(2), y - px(4), std::ptr::null_mut());
    LineTo(item.hDC, x + px(2), y);
    LineTo(item.hDC, x - px(2), y + px(4));
    RestoreDC(item.hDC, saved);
}

pub(super) struct Panel {
    pub(super) id: Uuid,
    pub(super) window: HWND,
    pub(super) owner: HWND,
    pub(super) entries: Vec<Entry>,
    buttons: Vec<HWND>,
    selected: Rc<Cell<Option<usize>>>,
    background: bool,
}
impl Panel {
    pub(super) fn new(
        owner: HWND,
        entries: Vec<Entry>,
        point: (i32, i32),
        background: bool,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !entries.is_empty() && entries.len() < 65_435,
            "invalid menu size"
        );
        unsafe {
            anyhow::ensure!(IsWindow(owner) != 0, "menu owner no longer exists");
            let class = wide("flowmux.windows.tab-menu");
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
                "cannot register tab menu"
            );
            let window = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                wide("Tab actions").as_ptr(),
                WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
                point.0,
                point.1,
                1,
                1,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create tab menu");
            let panel = Self {
                id: Uuid::new_v4(),
                window,
                owner,
                selected: Rc::new(Cell::new(entries.iter().position(|entry| entry.enabled))),
                entries,
                buttons: Vec::new(),
                background,
            };
            panel.build(point)
        }
    }
    unsafe fn build(mut self, point: (i32, i32)) -> anyhow::Result<Self> {
        ROUTES.with(|routes| routes.borrow_mut().insert(self.window as isize, self.id));
        let dpi = GetDpiForWindow(self.window).max(96) as i32;
        let px = |value: i32| (value * dpi + 48) / 96;
        let mut width = px(220);
        for (index, entry) in self.entries.iter().enumerate() {
            let button = CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide(entry.label.replace('&', "&&")).as_ptr(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | BS_OWNERDRAW as u32
                    | if entry.enabled { 0 } else { WS_DISABLED },
                0,
                0,
                1,
                1,
                self.window,
                (100 + index) as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            anyhow::ensure!(!button.is_null(), "cannot create tab menu item");
            self.buttons.push(button);
            chrome::register_button(
                button,
                chrome::Role::Workspace {
                    selected: false,
                    color: None,
                },
            );
            let dc = GetDC(button);
            if !dc.is_null() {
                let font = SendMessageW(button, WM_GETFONT, 0, 0) as HGDIOBJ;
                let old = SelectObject(dc, font);
                let text: Vec<u16> = entry.label.encode_utf16().collect();
                let drawing = chrome::caption_for_paint(&text);
                let mut size = SIZE::default();
                if GetTextExtentPoint32W(dc, drawing.as_ptr(), drawing.len() as i32, &mut size) != 0
                {
                    width = width.max(size.cx.saturating_add(px(28))).min(px(420));
                }
                SelectObject(dc, old);
                ReleaseDC(button, dc);
            }
        }
        let row_height = px(28);
        let margin = px(4);
        let mut outer = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: margin * 2 + row_height * self.buttons.len() as i32,
        };
        checked(AdjustWindowRectExForDpi(
            &mut outer,
            WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
            0,
            WS_EX_TOOLWINDOW,
            dpi as u32,
        ))?;
        let mut width = outer.right - outer.left;
        let mut height = outer.bottom - outer.top;
        let mut monitor = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let valid = GetMonitorInfoW(
            MonitorFromPoint(
                POINT {
                    x: point.0,
                    y: point.1,
                },
                MONITOR_DEFAULTTONEAREST,
            ),
            &mut monitor,
        ) != 0;
        if valid {
            width = width.min((monitor.rcWork.right - monitor.rcWork.left).max(1));
            height = height.min((monitor.rcWork.bottom - monitor.rcWork.top).max(1));
        }
        let (x, y) = if valid {
            (
                point.0.clamp(
                    monitor.rcWork.left,
                    (monitor.rcWork.right - width).max(monitor.rcWork.left),
                ),
                point.1.clamp(
                    monitor.rcWork.top,
                    (monitor.rcWork.bottom - height).max(monitor.rcWork.top),
                ),
            )
        } else {
            point
        };
        checked(SetWindowPos(
            self.window,
            std::ptr::null_mut(),
            x,
            y,
            width,
            height,
            SWP_NOZORDER | SWP_NOACTIVATE,
        ))?;
        let mut client = RECT::default();
        checked(GetClientRect(self.window, &mut client))?;
        VIEWPORTS.with(|views| {
            views.borrow_mut().insert(
                self.window as isize,
                Viewport {
                    buttons: self.buttons.clone(),
                    first: 0,
                    visible: ((client.bottom - margin * 2) / row_height).max(1) as usize,
                    row_height,
                    margin,
                    width: (client.right - margin * 2).max(1),
                    selected: self.selected.clone(),
                    background: self.background,
                    move_index: self
                        .entries
                        .iter()
                        .position(|entry| matches!(entry.action, MenuAction::Move)),
                },
            )
        });
        scroll(self.window, 0, self.selected.get());
        if !self.background {
            ShowWindow(self.window, SW_SHOWNORMAL);
            self.focus_selected();
        }
        Ok(self)
    }
    pub(super) fn select(&self, index: usize) {
        if self.entries.get(index).is_some_and(|entry| entry.enabled) {
            self.selected.set(Some(index));
            self.focus_selected();
        }
    }
    pub(super) fn focus_selected(&self) {
        scroll(self.window, 0, self.selected.get());
        if !self.background {
            if let Some(index) = self.selected.get() {
                unsafe {
                    SetFocus(self.buttons[index]);
                }
            }
        }
    }
    pub(super) fn row_rect(&self, index: usize) -> Option<RECT> {
        let button = *self.buttons.get(index)?;
        let mut rect = RECT::default();
        (unsafe { GetWindowRect(button, &mut rect) } != 0).then_some(rect)
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        if message.hwnd != self.window && unsafe { IsChild(self.window, message.hwnd) } == 0 {
            return false;
        }
        if message.message != WM_KEYDOWN || message.wParam == 229 {
            return false;
        }
        if unsafe { IsWindowEnabled(self.window) } == 0 {
            return false;
        }
        let focused = unsafe { GetFocus() };
        if let Some(index) = self.buttons.iter().position(|button| *button == focused) {
            self.selected.set(Some(index));
        }
        let enabled: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| entry.enabled.then_some(index))
            .collect();
        match message.wParam {
            0x26 | 0x28 | 0x24 | 0x23 => {
                if !enabled.is_empty() {
                    let current = self
                        .selected
                        .get()
                        .and_then(|index| enabled.iter().position(|value| *value == index));
                    let index = match message.wParam {
                        0x24 => 0,
                        0x23 => enabled.len() - 1,
                        0x26 => current.map_or(enabled.len() - 1, |at| {
                            (at + enabled.len() - 1) % enabled.len()
                        }),
                        _ => current.map_or(0, |at| (at + 1) % enabled.len()),
                    };
                    self.selected.set(Some(enabled[index]));
                    self.focus_selected();
                }
            }
            0x0d | 0x27 => {
                if let Some(index) = self
                    .selected
                    .get()
                    .filter(|index| self.entries[*index].enabled)
                {
                    if message.wParam == 0x0d
                        || matches!(self.entries[index].action, MenuAction::Move)
                    {
                        emit(self.window, UiAction::Choose(index));
                    }
                }
            }
            0x1b => emit(self.window, UiAction::Close),
            0x25 => emit(self.window, UiAction::Back),
            _ => return false,
        }
        true
    }
    pub(super) fn diagnostics(&self) -> Value {
        let rows = self.entries.iter().enumerate().map(|(index, entry)| {
            let bounds = self.row_rect(index).map(|rect| {
                let mut point = POINT { x: rect.left, y: rect.top };
                unsafe { ScreenToClient(self.window, &mut point); }
                json!({"x":point.x,"y":point.y,"width":rect.right-rect.left,"height":rect.bottom-rect.top})
            });
            json!({"window":self.buttons[index] as usize,"label":entry.label,"enabled":entry.enabled,"bounds":bounds,"layout_visible":layout_visible(self.window,index)})
        }).collect::<Vec<_>>();
        let viewport = VIEWPORTS.with(|views| {
            views
                .borrow()
                .get(&(self.window as isize))
                .map(|view| json!({"first":view.first,"visible":view.visible}))
        });
        json!({"id":self.id,"window":self.window as usize,"owner":self.owner as usize,
            "native_visible":unsafe { IsWindowVisible(self.window) != 0 },"selected":self.selected.get(),"rows":rows,"viewport":viewport})
    }
}
impl Drop for Panel {
    fn drop(&mut self) {
        ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
        VIEWPORTS.with(|views| views.borrow_mut().remove(&(self.window as isize)));
        for button in &self.buttons {
            chrome::unregister(*button);
        }
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
