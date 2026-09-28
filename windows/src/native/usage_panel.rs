// SPDX-License-Identifier: GPL-3.0-or-later
//! Sidebar-anchored usage popover and the persistent main-content usage bar.
use super::*;
use crate::usage::{format_token_count, Provider, ProviderState, UsagePanelState};
use std::cell::Cell;
use windows_sys::Win32::UI::{
    Controls::{SetScrollInfo, DRAWITEMSTRUCT, ODT_BUTTON},
    Input::KeyboardAndMouse::{EnableWindow, GetFocus},
};

#[derive(Clone, Copy)]
pub(super) enum UiAction {
    Refresh,
    ToggleBar(bool),
    Close,
    Layout,
    Scroll(i32),
    ScrollTo(i32),
}
#[derive(Clone)]
struct Line {
    text: String,
    muted: bool,
    percent: Option<f64>,
}
#[derive(Clone, Default)]
struct Card {
    title: String,
    lines: Vec<Line>,
}
#[derive(Clone)]
struct Meter {
    text: String,
    percent: f64,
    stale: bool,
}
#[derive(Clone)]
enum Paint {
    Card(Card),
    Bar(Vec<Meter>),
}
thread_local! {
    static PAINT: RefCell<HashMap<isize, Paint>> = RefCell::new(HashMap::new());
    static TOGGLES: RefCell<HashMap<isize, bool>> = RefCell::new(HashMap::new());
}
fn emit(action: UiAction) {
    post(Event::UsageUi(action));
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
        WM_SIZE | WM_DPICHANGED if GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_CHILD == 0 => {
            emit(UiAction::Layout);
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
        WM_COMMAND if (wparam >> 16) as u32 == BN_CLICKED => {
            let control = lparam as HWND;
            if !control.is_null() && GetParent(control) == window && IsWindowEnabled(control) != 0 {
                match wparam & 0xffff {
                    2 => emit(UiAction::Refresh),
                    3 => {
                        if let Some(enabled) =
                            TOGGLES.with(|values| values.borrow().get(&(window as isize)).copied())
                        {
                            emit(UiAction::ToggleBar(!enabled));
                        }
                    }
                    _ => {}
                }
            }
            return 0;
        }
        WM_DRAWITEM if lparam != 0 && draw(&*(lparam as *const DRAWITEMSTRUCT)) => return 1,
        _ => {}
    }
    if let Some(result) = chrome::message(window, message, wparam, lparam) {
        return result;
    }
    DefWindowProcW(window, message, wparam, lparam)
}
fn px(window: HWND, value: i32) -> i32 {
    (value * unsafe { GetDpiForWindow(window).max(96) } as i32 + 48) / 96
}
fn caption(window: HWND, text: &str) {
    unsafe {
        SetWindowTextW(window, wide(text).as_ptr());
    }
}
fn bounds(window: HWND) -> Value {
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(window, &mut rect);
    }
    json!({"x":rect.left,"y":rect.top,"width":rect.right-rect.left,"height":rect.bottom-rect.top})
}
unsafe fn text(dc: HDC, value: &str, rect: &mut RECT, flags: u32) {
    let original: Vec<u16> = value.encode_utf16().collect();
    let raw = chrome::caption_for_paint(&original);
    DrawTextW(
        dc,
        raw.as_ptr(),
        raw.len() as i32,
        rect,
        flags | DT_NOPREFIX,
    );
}
unsafe fn line_height(dc: HDC, value: &str, width: i32, minimum: i32) -> i32 {
    let mut rect = RECT {
        right: width.max(1),
        ..Default::default()
    };
    text(dc, value, &mut rect, DT_CALCRECT | DT_WORDBREAK);
    rect.bottom.max(minimum)
}
fn card_height(window: HWND, card: &Card, width: i32) -> i32 {
    unsafe {
        let dc = GetDC(window);
        if dc.is_null() {
            return px(window, 64 + card.lines.len() as i32 * 44);
        }
        let old = SelectObject(dc, SendMessageW(window, WM_GETFONT, 0, 0) as HGDIOBJ);
        let height = px(window, 44)
            + card
                .lines
                .iter()
                .map(|line| {
                    line_height(dc, &line.text, width - px(window, 24), px(window, 20))
                        + px(window, if line.percent.is_some() { 18 } else { 6 })
                })
                .sum::<i32>();
        SelectObject(dc, old);
        ReleaseDC(window, dc);
        height
    }
}
unsafe fn meter(dc: HDC, area: RECT, value: f64, fill: COLORREF, track: COLORREF) {
    SetDCBrushColor(dc, track);
    FillRect(dc, &area, GetStockObject(DC_BRUSH));
    let fraction = if value.is_finite() {
        (value / 100.0).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let end = area.left + ((area.right - area.left) as f64 * fraction).round() as i32;
    if end > area.left {
        SetDCBrushColor(dc, fill);
        FillRect(dc, &RECT { right: end, ..area }, GetStockObject(DC_BRUSH));
    }
}
unsafe fn draw(item: &DRAWITEMSTRUCT) -> bool {
    if item.CtlType != ODT_BUTTON {
        return false;
    }
    let Some(paint) = PAINT.with(|values| values.borrow().get(&(item.hwndItem as isize)).cloned())
    else {
        return false;
    };
    let saved = SaveDC(item.hDC);
    if saved == 0 {
        return false;
    }
    let palette = chrome::palette();
    let p = |n| px(item.hwndItem, n);
    SetBkMode(item.hDC, TRANSPARENT as i32);
    SelectObject(
        item.hDC,
        SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HGDIOBJ,
    );
    SetDCBrushColor(item.hDC, palette.surface);
    FillRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
    match paint {
        Paint::Card(card) => {
            SetDCBrushColor(item.hDC, palette.border);
            FrameRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
            SetTextColor(item.hDC, palette.foreground);
            let mut rect = RECT {
                left: p(12),
                top: p(10),
                right: item.rcItem.right - p(12),
                bottom: p(34),
            };
            text(
                item.hDC,
                &card.title,
                &mut rect,
                DT_SINGLELINE | DT_END_ELLIPSIS,
            );
            let mut y = p(38);
            for line in card.lines {
                SetTextColor(
                    item.hDC,
                    if line.muted {
                        palette.muted
                    } else {
                        palette.foreground
                    },
                );
                let height = line_height(item.hDC, &line.text, rect.right - rect.left, p(20));
                rect.top = y;
                rect.bottom = y + height;
                text(item.hDC, &line.text, &mut rect, DT_WORDBREAK);
                y += height;
                if let Some(percent) = line.percent {
                    meter(
                        item.hDC,
                        RECT {
                            left: rect.left,
                            top: y + p(4),
                            right: rect.right,
                            bottom: y + p(10),
                        },
                        percent,
                        palette.accent,
                        palette.border,
                    );
                    y += p(18);
                } else {
                    y += p(6);
                }
            }
        }
        Paint::Bar(meters) => {
            let count = meters.len().max(1) as i32;
            let width = (item.rcItem.right - p(16)).max(0);
            for (index, value) in meters.iter().enumerate() {
                let left = p(8) + width * index as i32 / count;
                let right = p(8) + width * (index as i32 + 1) / count - p(8);
                if right <= left {
                    continue;
                }
                SetTextColor(
                    item.hDC,
                    if value.stale {
                        palette.muted
                    } else {
                        palette.foreground
                    },
                );
                let mut rect = RECT {
                    left,
                    top: p(1),
                    right,
                    bottom: p(19),
                };
                text(
                    item.hDC,
                    &value.text,
                    &mut rect,
                    DT_SINGLELINE | DT_END_ELLIPSIS,
                );
                meter(
                    item.hDC,
                    RECT {
                        left,
                        top: p(21),
                        right,
                        bottom: p(24),
                    },
                    value.percent,
                    palette.accent,
                    palette.border,
                );
            }
        }
    }
    RestoreDC(item.hDC, saved);
    true
}
fn make_window(parent: HWND, child: bool) -> anyhow::Result<HWND> {
    unsafe {
        let class = wide("flowmux.windows.usage");
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
            "cannot register usage panel"
        );
        let window = CreateWindowExW(
            WS_EX_CONTROLPARENT | if child { 0 } else { WS_EX_TOOLWINDOW },
            class.as_ptr(),
            wide("AI Usage").as_ptr(),
            WS_CLIPCHILDREN
                | if child {
                    WS_CHILD
                } else {
                    WS_POPUP | WS_BORDER
                },
            0,
            0,
            360,
            240,
            parent,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        );
        anyhow::ensure!(!window.is_null(), "cannot create usage window");
        Ok(window)
    }
}
fn child(parent: HWND, class: &str, value: &str, id: usize, style: u32) -> anyhow::Result<HWND> {
    unsafe {
        let window = CreateWindowExW(
            0,
            wide(class).as_ptr(),
            wide(value).as_ptr(),
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
        anyhow::ensure!(!window.is_null(), "cannot create usage control");
        chrome::register_control(window, chrome::ControlRole::Static);
        Ok(window)
    }
}
fn place(window: HWND, x: i32, y: i32, width: i32, height: i32) {
    unsafe {
        SetWindowPos(
            window,
            std::ptr::null_mut(),
            x,
            y,
            width.max(1),
            height.max(1),
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}
fn percent(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.0}%")
    } else {
        "—".into()
    }
}
fn provider_name(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "Claude",
        Provider::Codex => "Codex",
    }
}
fn card(state: &ProviderState, refreshing: bool) -> Card {
    let mut result = Card {
        title: provider_name(state.provider).into(),
        lines: vec![],
    };
    let mut add = |text: String, muted: bool, percent: Option<f64>| {
        result.lines.push(Line {
            text,
            muted,
            percent,
        })
    };
    if let Some(tokens) = &state.tokens {
        if let Some(today) = tokens.value.today {
            add(
                format!("Tokens today {}", format_token_count(today)),
                false,
                None,
            );
        }
        if let Some(lifetime) = tokens.value.lifetime {
            add(
                format!("Lifetime tokens {}", format_token_count(lifetime)),
                false,
                None,
            );
        }
        if state.token_error.is_some() {
            add(
                format!(
                    "Token data last updated {}",
                    tokens
                        .updated_at
                        .with_timezone(&chrono::Local)
                        .format("%H:%M")
                ),
                true,
                None,
            );
        }
    }
    if let Some(limits) = &state.limits {
        if state.limits_error.is_some() {
            add(
                format!(
                    "Rate limits last updated {}",
                    limits
                        .updated_at
                        .with_timezone(&chrono::Local)
                        .format("%H:%M")
                ),
                true,
                None,
            );
        }
    }
    let mut errors = HashSet::new();
    for error in [state.token_error.as_ref(), state.limits_error.as_ref()]
        .into_iter()
        .flatten()
    {
        if errors.insert(&error.message) {
            add(error.message.clone(), true, None);
        }
    }
    if let Some(limits) = &state.limits {
        if limits.value.is_empty() {
            add("No rate limit data".into(), true, None);
        }
        for limit in &limits.value {
            let scope = limit
                .scope
                .as_ref()
                .map(|value| format!(" · {value}"))
                .unwrap_or_default();
            add(
                format!("{}{scope}    {}", limit.label, percent(limit.used_percent)),
                false,
                Some(limit.used_percent),
            );
            if let Some(reset) = limit.resets_at {
                add(
                    format!(
                        "Resets at {}",
                        reset.with_timezone(&chrono::Local).format("%m/%d %H:%M")
                    ),
                    true,
                    None,
                );
            }
        }
    }
    if result.lines.is_empty() {
        result.lines.push(Line {
            text: if refreshing {
                "Loading usage…"
            } else {
                "No usage data"
            }
            .into(),
            muted: true,
            percent: None,
        });
    }
    if let Some(time) = state
        .tokens
        .as_ref()
        .map(|value| value.updated_at)
        .into_iter()
        .chain(state.limits.as_ref().map(|value| value.updated_at))
        .max()
    {
        result.title.push_str(&format!(
            "     Updated {}",
            time.with_timezone(&chrono::Local).format("%H:%M")
        ));
    }
    result
}
fn card_caption(value: &Card) -> String {
    std::iter::once(value.title.as_str())
        .chain(value.lines.iter().map(|line| line.text.as_str()))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) struct Panel {
    pub(super) window: HWND,
    viewport: HWND,
    title: HWND,
    refresh: HWND,
    toggle: HWND,
    cards: [HWND; 2],
    values: RefCell<[Card; 2]>,
    opened: Cell<bool>,
    enabled: Cell<bool>,
    refreshing: Cell<bool>,
    anchor: Cell<HWND>,
    offset: Cell<i32>,
    previous_focus: Cell<HWND>,
}
impl Drop for Panel {
    fn drop(&mut self) {
        self.hide();
        TOGGLES.with(|values| values.borrow_mut().remove(&(self.window as isize)));
        for window in [
            self.title,
            self.refresh,
            self.toggle,
            self.cards[0],
            self.cards[1],
        ] {
            PAINT.with(|values| values.borrow_mut().remove(&(window as isize)));
            chrome::unregister(window);
        }
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    pub(super) fn new(owner: HWND) -> anyhow::Result<Self> {
        let mut panel = Self {
            window: make_window(owner, false)?,
            viewport: std::ptr::null_mut(),
            title: std::ptr::null_mut(),
            refresh: std::ptr::null_mut(),
            toggle: std::ptr::null_mut(),
            cards: [std::ptr::null_mut(); 2],
            values: RefCell::new([Card::default(), Card::default()]),
            opened: Cell::new(false),
            enabled: Cell::new(false),
            refreshing: Cell::new(false),
            anchor: Cell::new(owner),
            offset: Cell::new(0),
            previous_focus: Cell::new(std::ptr::null_mut()),
        };
        panel.viewport = make_window(panel.window, true)?;
        unsafe {
            SetWindowLongPtrW(
                panel.viewport,
                GWL_STYLE,
                (GetWindowLongPtrW(panel.viewport, GWL_STYLE) as u32 | WS_VSCROLL | WS_VISIBLE)
                    as isize,
            );
        }
        panel.title = child(panel.window, "STATIC", "AI Usage", 1, 0)?;
        panel.refresh = child(
            panel.window,
            "BUTTON",
            "Refresh",
            2,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        panel.toggle = child(
            panel.window,
            "BUTTON",
            "Show bar",
            3,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        chrome::register_button(panel.refresh, chrome::Role::Button);
        chrome::register_button(panel.toggle, chrome::Role::Button);
        for (index, window) in panel.cards.iter_mut().enumerate() {
            *window = child(
                panel.viewport,
                "BUTTON",
                "",
                10 + index,
                BS_OWNERDRAW as u32,
            )?;
        }
        panel.update(&UsagePanelState::default(), true);
        Ok(panel)
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
                SetFocus(self.toggle);
            }
        }
    }
    pub(super) fn hide(&self) {
        self.opened.set(false);
        self.anchor.set(std::ptr::null_mut());
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
    pub(super) fn is_open(&self) -> bool {
        self.opened.get()
    }
    pub(super) fn update(&self, state: &UsagePanelState, bar_enabled: bool) {
        self.enabled.set(bar_enabled);
        self.refreshing.set(state.refreshing);
        TOGGLES.with(|values| {
            values
                .borrow_mut()
                .insert(self.window as isize, bar_enabled)
        });
        caption(
            self.toggle,
            if bar_enabled { "Hide bar" } else { "Show bar" },
        );
        caption(
            self.refresh,
            if state.refreshing {
                "Refreshing…"
            } else {
                "Refresh"
            },
        );
        unsafe {
            EnableWindow(self.refresh, i32::from(!state.refreshing));
        }
        let values = [
            card(&state.claude, state.refreshing),
            card(&state.codex, state.refreshing),
        ];
        for (window, value) in self.cards.iter().zip(&values) {
            caption(*window, &card_caption(value));
            PAINT.with(|paint| {
                paint
                    .borrow_mut()
                    .insert(*window as isize, Paint::Card(value.clone()))
            });
            unsafe {
                InvalidateRect(*window, std::ptr::null(), 1);
            }
        }
        *self.values.borrow_mut() = values;
        if self.opened.get() {
            self.position();
        } else {
            self.layout();
        }
    }
    pub(super) fn position(&self) {
        if !self.opened.get() {
            return;
        }
        unsafe {
            let p = |n| px(self.window, n);
            let mut anchor = RECT::default();
            if GetWindowRect(self.anchor.get(), &mut anchor) == 0 {
                self.hide();
                return;
            }
            let width = p(360);
            let total = self
                .cards
                .iter()
                .zip(self.values.borrow().iter())
                .map(|(window, value)| card_height(*window, value, p(320)) + p(8))
                .sum::<i32>();
            let mut height = total.clamp(p(180), p(520)) + p(58);
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let valid = GetMonitorInfoW(
                MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            ) != 0;
            let mut x = anchor.left;
            let mut y = anchor.top - height - p(6);
            let mut width = width;
            if valid {
                height = height.min((monitor.rcWork.bottom - monitor.rcWork.top).max(1));
                width = width.min((monitor.rcWork.right - monitor.rcWork.left).max(1));
                x = x.clamp(
                    monitor.rcWork.left,
                    (monitor.rcWork.right - width).max(monitor.rcWork.left),
                );
                if y < monitor.rcWork.top {
                    y = anchor.bottom + p(6);
                }
                y = y.clamp(
                    monitor.rcWork.top,
                    (monitor.rcWork.bottom - height).max(monitor.rcWork.top),
                );
            }
            place(self.window, x, y, width, height);
        }
        self.layout();
    }
    pub(super) fn layout(&self) {
        unsafe {
            let p = |n| px(self.window, n);
            let mut client = RECT::default();
            GetClientRect(self.window, &mut client);
            place(
                self.title,
                p(10),
                p(10),
                (client.right - p(206)).max(1),
                p(28),
            );
            place(
                self.toggle,
                (client.right - p(196)).max(0),
                p(8),
                p(88),
                p(30),
            );
            place(
                self.refresh,
                (client.right - p(102)).max(0),
                p(8),
                p(92),
                p(30),
            );
            let height = (client.bottom - p(58)).max(1);
            place(
                self.viewport,
                p(10),
                p(48),
                (client.right - p(20)).max(1),
                height,
            );
            let mut viewport = RECT::default();
            GetClientRect(self.viewport, &mut viewport);
            let heights: Vec<_> = self
                .cards
                .iter()
                .zip(self.values.borrow().iter())
                .map(|(window, value)| card_height(*window, value, viewport.right))
                .collect();
            let total = heights.iter().sum::<i32>() + p(8);
            let offset = self.offset.get().clamp(0, (total - height).max(0));
            self.offset.set(offset);
            SetScrollInfo(
                self.viewport,
                SB_VERT,
                &SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                    nMin: 0,
                    nMax: (total - 1).max(0),
                    nPage: height as u32,
                    nPos: offset,
                    ..Default::default()
                },
                1,
            );
            GetClientRect(self.viewport, &mut viewport);
            let mut y = -offset;
            for (window, height) in self.cards.iter().zip(heights) {
                place(*window, 0, y, viewport.right, height);
                y += height + p(8);
            }
        }
    }
    pub(super) fn scroll_by(&self, delta: i32) {
        self.scroll_to(self.offset.get().saturating_add(px(self.window, delta)));
    }
    pub(super) fn scroll(&self, offset: i32) {
        self.scroll_to(offset);
    }
    pub(super) fn scroll_to(&self, offset: i32) {
        self.offset.set(offset);
        self.layout();
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        unsafe {
            if !self.opened.get()
                || (message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0)
            {
                return false;
            }
            if message.message == WM_KEYDOWN {
                match message.wParam {
                    0x1b => {
                        emit(UiAction::Close);
                        return true;
                    }
                    0x21 => {
                        emit(UiAction::Scroll(-240));
                        return true;
                    }
                    0x22 => {
                        emit(UiAction::Scroll(240));
                        return true;
                    }
                    _ => {}
                }
            }
            IsDialogMessageW(self.window, message) != 0
        }
    }
    pub(super) fn diagnostics(&self) -> Value {
        let palette = chrome::palette();
        json!({"window":self.window as usize,"owner":unsafe {GetWindow(self.window,GW_OWNER)} as usize,"open":self.opened.get(),"native_visible":unsafe {IsWindowVisible(self.window)!=0},"bounds":bounds(self.window),"anchor":self.anchor.get() as usize,"viewport":self.viewport as usize,"refresh":self.refresh as usize,"toggle":self.toggle as usize,"title":self.title as usize,"bar_enabled":self.enabled.get(),"refreshing":self.refreshing.get(),"scroll_offset":self.offset.get(),"colors":{"surface":palette.surface,"foreground":palette.foreground,"muted":palette.muted,"progress":palette.accent,"track":palette.border},"cards":self.cards.iter().zip(self.values.borrow().iter()).map(|(window,value)|json!({"window":*window as usize,"text":card_caption(value),"bounds":bounds(*window),"lines":value.lines.iter().map(|line|json!({"text":line.text,"muted":line.muted,"percent":line.percent})).collect::<Vec<_>>()})).collect::<Vec<_>>()})
    }
}

pub(super) struct Bar {
    pub(super) window: HWND,
    control: HWND,
    meters: RefCell<Vec<Meter>>,
    enabled: Cell<bool>,
}
impl Drop for Bar {
    fn drop(&mut self) {
        PAINT.with(|values| values.borrow_mut().remove(&(self.control as isize)));
        chrome::unregister(self.control);
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Bar {
    pub(super) fn new(owner: HWND) -> anyhow::Result<Self> {
        let mut bar = Self {
            window: make_window(owner, true)?,
            control: std::ptr::null_mut(),
            meters: RefCell::new(vec![]),
            enabled: Cell::new(false),
        };
        bar.control = child(bar.window, "BUTTON", "AI usage", 1, BS_OWNERDRAW as u32)?;
        chrome::register_control(bar.control, chrome::ControlRole::Caption);
        Ok(bar)
    }
    pub(super) fn update(&self, state: &UsagePanelState, enabled: bool) {
        let mut meters = vec![];
        for provider in [&state.claude, &state.codex] {
            let Some(limits) = &provider.limits else {
                continue;
            };
            let maximum = |duration: Option<u64>, scope: Option<&str>| {
                limits
                    .value
                    .iter()
                    .filter(|value| {
                        if let Some(duration) = duration {
                            value.duration_minutes == Some(duration)
                        } else {
                            value.scope.as_deref() == scope
                        }
                    })
                    .map(|value| value.used_percent)
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .max_by(f64::total_cmp)
            };
            let mut slots = vec![];
            for (duration, label) in [(300, "5h"), (10_080, "1W")] {
                if let Some(value) = maximum(Some(duration), None) {
                    slots.push((label, value));
                }
            }
            if slots.is_empty() {
                let (scope, label) = match provider.provider {
                    Provider::Claude => ("Extra usage", "Extra"),
                    Provider::Codex => ("Individual", "Individual"),
                };
                if let Some(value) = maximum(None, Some(scope)) {
                    slots.push((label, value));
                }
            }
            for (label, value) in slots {
                meters.push(Meter {
                    text: format!(
                        "{} {}({label}){}",
                        provider_name(provider.provider),
                        percent(value),
                        if provider.limits_error.is_some() {
                            " (last known)"
                        } else {
                            ""
                        }
                    ),
                    percent: value,
                    stale: provider.limits_error.is_some(),
                });
            }
        }
        caption(
            self.control,
            &meters
                .iter()
                .map(|meter| meter.text.as_str())
                .collect::<Vec<_>>()
                .join("; "),
        );
        PAINT.with(|values| {
            values
                .borrow_mut()
                .insert(self.control as isize, Paint::Bar(meters.clone()))
        });
        *self.meters.borrow_mut() = meters;
        self.enabled.set(enabled);
        unsafe {
            InvalidateRect(self.control, std::ptr::null(), 1);
        }
    }
    pub(super) fn height(&self, dpi: u32) -> i32 {
        if self.enabled.get() && !self.meters.borrow().is_empty() {
            (26 * dpi.max(96) as i32 + 48) / 96
        } else {
            0
        }
    }
    pub(super) fn layout(&self, area: RECT, background: bool) {
        let visible = self.height(unsafe { GetDpiForWindow(self.window) }) > 0
            && area.right > area.left
            && area.bottom > area.top;
        place(
            self.window,
            area.left,
            area.top,
            area.right - area.left,
            area.bottom - area.top,
        );
        place(
            self.control,
            0,
            0,
            area.right - area.left,
            area.bottom - area.top,
        );
        unsafe {
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
    pub(super) fn diagnostics(&self) -> Value {
        json!({"window":self.window as usize,"control":self.control as usize,"enabled":self.enabled.get(),"native_visible":unsafe{IsWindowVisible(self.window)!=0},"bounds":bounds(self.window),"meters":self.meters.borrow().iter().map(|value|json!({"text":value.text,"percent":value.percent,"stale":value.stale})).collect::<Vec<_>>()})
    }
}
