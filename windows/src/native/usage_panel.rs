// SPDX-License-Identifier: GPL-3.0-or-later
//! Sidebar-anchored usage popover and the persistent main-content usage bar.
use super::*;
use crate::usage::{format_token_count, Provider, ProviderState, UsagePanelState};
use std::cell::Cell;
use windows_sys::Win32::UI::{
    Controls::{
        SetScrollInfo, CDDS_PREPAINT, CDIS_DISABLED, CDIS_FOCUS, CDIS_HOT, CDRF_SKIPDEFAULT,
        DRAWITEMSTRUCT, NMCUSTOMDRAW, NM_CUSTOMDRAW, ODS_DISABLED, ODS_FOCUS, ODS_HOTLIGHT,
        ODS_NOFOCUSRECT, ODT_BUTTON, ODT_STATIC,
    },
    Input::KeyboardAndMouse::{EnableWindow, GetFocus},
};
const SPINNER_TIMER: usize = 0x4655;

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
    provider: Provider,
    label: String,
    display_text: String,
    text: String,
    percent: f64,
    stale: bool,
}
#[derive(Clone)]
enum Paint {
    Card(Card),
    Bar(Vec<Meter>),
    Spinner(u8),
}
thread_local! {
    static PAINT: RefCell<HashMap<isize, Paint>> = RefCell::new(HashMap::new());
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
        WM_TIMER if wparam == SPINNER_TIMER => {
            let spinner = GetDlgItem(window, 4);
            if spinner.is_null() {
                return 0;
            }
            PAINT.with(|values| {
                if let Some(Paint::Spinner(phase)) =
                    values.borrow_mut().get_mut(&(spinner as isize))
                {
                    *phase = (*phase + 1) % 8;
                }
            });
            InvalidateRect(spinner, std::ptr::null(), 0);
            return 0;
        }
        WM_NOTIFY if lparam != 0 => {
            let header = &*(lparam as *const windows_sys::Win32::UI::Controls::NMHDR);
            if header.code == NM_CUSTOMDRAW && header.hwndFrom == GetDlgItem(window, 3) {
                let draw = &*(lparam as *const NMCUSTOMDRAW);
                if draw.dwDrawStage == CDDS_PREPAINT {
                    let item = DRAWITEMSTRUCT {
                        CtlType: ODT_BUTTON,
                        hwndItem: header.hwndFrom,
                        hDC: draw.hdc,
                        rcItem: draw.rc,
                        itemState: if draw.uItemState & CDIS_DISABLED != 0 {
                            ODS_DISABLED
                        } else {
                            0
                        } | if draw.uItemState & CDIS_FOCUS != 0 {
                            ODS_FOCUS
                        } else {
                            ODS_NOFOCUSRECT
                        } | if draw.uItemState & CDIS_HOT != 0 {
                            ODS_HOTLIGHT
                        } else {
                            0
                        },
                        ..Default::default()
                    };
                    if chrome::message(
                        window,
                        WM_DRAWITEM,
                        0,
                        (&item as *const DRAWITEMSTRUCT) as LPARAM,
                    )
                    .is_some()
                    {
                        return CDRF_SKIPDEFAULT as LRESULT;
                    }
                }
            }
        }
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
                    3 => emit(UiAction::ToggleBar(
                        SendMessageW(control, BM_GETCHECK, 0, 0) == 1,
                    )),
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
struct MeterLayout {
    icon: Option<RECT>,
    track: RECT,
    text: RECT,
}
fn provider_key(provider: Provider) -> &'static str {
    match provider {
        Provider::Claude => "claude",
        Provider::Codex => "codex",
    }
}
unsafe fn bar_layout(dc: HDC, window: HWND, area: RECT, meters: &[Meter]) -> Vec<MeterLayout> {
    if meters.is_empty() {
        return vec![];
    }
    let p = |n| px(window, n);
    let left = (area.left + p(6)).min(area.right);
    let right = (area.right - p(6)).max(left);
    let top = (area.top + p(1)).min(area.bottom);
    let cy = (top + area.bottom) / 2;
    let widths: Vec<_> = meters
        .iter()
        .map(|value| {
            let mut rect = RECT::default();
            text(
                dc,
                &value.display_text,
                &mut rect,
                DT_CALCRECT | DT_SINGLELINE,
            );
            (rect.right - rect.left).max(1)
        })
        .collect();
    let groups = meters
        .iter()
        .enumerate()
        .filter(|(index, value)| *index == 0 || meters[*index - 1].provider != value.provider)
        .count() as i32;
    let count = meters.len() as i32;
    let fixed = groups * p(22) + (groups - 1) * p(16) + (count - groups) * p(8) + count * p(4);
    let total_text = widths.iter().sum::<i32>().max(1);
    let available = (right - left - fixed).max(0);
    let track_width = ((available - total_text).max(0) / count).min(p(95));
    let text_budget = (available - count * track_width).max(0).min(total_text);
    let mut x = left;
    meters
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let first = index == 0 || meters[index - 1].provider != value.provider;
            if index > 0 {
                x = (x + p(if first { 16 } else { 8 })).min(right);
            }
            let icon = if first {
                let size = p(14).min((area.bottom - top).max(0));
                let rect = (right - x >= size && size > 0).then_some(RECT {
                    left: x,
                    top: cy - size / 2,
                    right: x + size,
                    bottom: cy - size / 2 + size,
                });
                x = (x + p(22)).min(right);
                rect
            } else {
                None
            };
            let height = p(4).min((area.bottom - top).max(0));
            let track = RECT {
                left: x,
                top: cy - height / 2,
                right: (x + track_width).min(right),
                bottom: cy - height / 2 + height,
            };
            x = (track.right + p(4)).min(right);
            let width =
                (i64::from(widths[index]) * i64::from(text_budget) / i64::from(total_text)) as i32;
            let text = RECT {
                left: x,
                top,
                right: (x + width).min(right),
                bottom: area.bottom,
            };
            x = text.right;
            MeterLayout { icon, track, text }
        })
        .collect()
}
unsafe fn draw(item: &DRAWITEMSTRUCT) -> bool {
    if !matches!(item.CtlType, ODT_BUTTON | ODT_STATIC) {
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
    SetDCBrushColor(
        item.hDC,
        if matches!(paint, Paint::Spinner(_) | Paint::Bar(_)) {
            palette.background
        } else {
            palette.surface
        },
    );
    FillRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
    match paint {
        Paint::Spinner(phase) => {
            let cx = (item.rcItem.left + item.rcItem.right) / 2;
            let cy = (item.rcItem.top + item.rcItem.bottom) / 2;
            let offset = |n: i32| p(n.abs()) * n.signum();
            SelectObject(item.hDC, GetStockObject(DC_PEN));
            for (index, (x, y)) in [
                (0, -6),
                (4, -4),
                (6, 0),
                (4, 4),
                (0, 6),
                (-4, 4),
                (-6, 0),
                (-4, -4),
            ]
            .into_iter()
            .enumerate()
            {
                SetDCPenColor(
                    item.hDC,
                    if index == phase as usize {
                        palette.foreground
                    } else {
                        palette.muted
                    },
                );
                MoveToEx(
                    item.hDC,
                    cx + offset(x) / 2,
                    cy + offset(y) / 2,
                    std::ptr::null_mut(),
                );
                LineTo(item.hDC, cx + offset(x), cy + offset(y));
            }
        }
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
            IntersectClipRect(
                item.hDC,
                item.rcItem.left,
                item.rcItem.top,
                item.rcItem.right,
                item.rcItem.bottom,
            );
            SetDCBrushColor(item.hDC, palette.border);
            FillRect(
                item.hDC,
                &RECT {
                    bottom: (item.rcItem.top + p(1)).min(item.rcItem.bottom),
                    ..item.rcItem
                },
                GetStockObject(DC_BRUSH),
            );
            for (value, layout) in
                meters
                    .iter()
                    .zip(bar_layout(item.hDC, item.hwndItem, item.rcItem, &meters))
            {
                if let Some(icon) = layout.icon {
                    chrome::draw_agent_icon(item.hDC, provider_key(value.provider), icon);
                }
                let fill = if palette.high_contrast {
                    palette.accent
                } else {
                    match value.provider {
                        Provider::Claude => 0x5777d9,
                        Provider::Codex => 0xff9d7a,
                    }
                };
                if layout.track.right > layout.track.left {
                    meter(item.hDC, layout.track, value.percent, fill, palette.border);
                }
                if layout.text.right > layout.text.left {
                    SetTextColor(item.hDC, palette.foreground);
                    let mut rect = layout.text;
                    text(
                        item.hDC,
                        &value.display_text,
                        &mut rect,
                        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                    );
                }
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
    spinner: HWND,
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
        for window in [
            self.title,
            self.refresh,
            self.toggle,
            self.spinner,
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
            spinner: std::ptr::null_mut(),
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
            "Refresh usage",
            2,
            WS_TABSTOP | BS_OWNERDRAW as u32,
        )?;
        panel.toggle = child(
            panel.window,
            "BUTTON",
            "Show bar",
            3,
            WS_TABSTOP | BS_PUSHLIKE as u32 | BS_AUTOCHECKBOX as u32,
        )?;
        chrome::register_button(
            panel.refresh,
            chrome::Role::Icon {
                kind: chrome::ChromeIcon::Reload,
                marked: false,
            },
        );
        chrome::register_button(
            panel.toggle,
            chrome::Role::Icon {
                kind: chrome::ChromeIcon::UsageBar,
                marked: false,
            },
        );
        panel.spinner = child(
            panel.window,
            "STATIC",
            "Refreshing usage",
            4,
            windows_sys::Win32::System::SystemServices::SS_OWNERDRAW,
        )?;
        PAINT.with(|values| {
            values
                .borrow_mut()
                .insert(panel.spinner as isize, Paint::Spinner(0))
        });
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
        self.spinner_timer();
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
        self.spinner_timer();
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
    fn spinner_timer(&self) {
        unsafe {
            if self.opened.get() && self.refreshing.get() {
                SetTimer(self.window, SPINNER_TIMER, 100, None);
            } else {
                KillTimer(self.window, SPINNER_TIMER);
            }
        }
    }
    pub(super) fn update(&self, state: &UsagePanelState, bar_enabled: bool) {
        self.enabled.set(bar_enabled);
        self.refreshing.set(state.refreshing);
        unsafe {
            SendMessageW(self.toggle, BM_SETCHECK, usize::from(bar_enabled), 0);
            InvalidateRect(self.toggle, std::ptr::null(), 0);
            EnableWindow(self.refresh, i32::from(!state.refreshing));
            ShowWindow(
                self.spinner,
                if state.refreshing { SW_SHOWNA } else { SW_HIDE },
            );
        }
        self.spinner_timer();
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
                (client.right - p(if self.refreshing.get() { 108 } else { 86 })).max(1),
                p(28),
            );
            place(
                self.toggle,
                (client.right - p(if self.refreshing.get() { 98 } else { 76 })).max(0),
                p(10),
                p(28),
                p(28),
            );
            place(
                self.refresh,
                (client.right - p(if self.refreshing.get() { 64 } else { 42 })).max(0),
                p(10),
                p(28),
                p(28),
            );
            place(
                self.spinner,
                (client.right - p(26)).max(0),
                p(16),
                p(16),
                p(16),
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
        json!({"window":self.window as usize,"owner":unsafe {GetWindow(self.window,GW_OWNER)} as usize,"open":self.opened.get(),"native_visible":unsafe {IsWindowVisible(self.window)!=0},"bounds":bounds(self.window),"anchor":self.anchor.get() as usize,"viewport":self.viewport as usize,"refresh":self.refresh as usize,"toggle":self.toggle as usize,"spinner":self.spinner as usize,"title":self.title as usize,"bar_enabled":self.enabled.get(),"refreshing":self.refreshing.get(),"scroll_offset":self.offset.get(),"colors":{"surface":palette.surface,"foreground":palette.foreground,"muted":palette.muted,"progress":palette.accent,"track":palette.border},"cards":self.cards.iter().zip(self.values.borrow().iter()).map(|(window,value)|json!({"window":*window as usize,"text":card_caption(value),"bounds":bounds(*window),"lines":value.lines.iter().map(|line|json!({"text":line.text,"muted":line.muted,"percent":line.percent})).collect::<Vec<_>>()})).collect::<Vec<_>>()})
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
        chrome::register_control(bar.control, chrome::ControlRole::UsageBar);
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
                    provider: provider.provider,
                    label: label.into(),
                    display_text: format!("{}({label})", percent(value)),
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
            (20 * dpi.max(96) as i32 + 48) / 96
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
        let meters = self.meters.borrow();
        let geometry = unsafe {
            let dc = GetDC(self.control);
            if dc.is_null() {
                Value::Null
            } else {
                let font =
                    SelectObject(dc, SendMessageW(self.control, WM_GETFONT, 0, 0) as HGDIOBJ);
                let mut area = RECT::default();
                GetClientRect(self.control, &mut area);
                let layout = bar_layout(dc, self.control, area, &meters);
                SelectObject(dc, font);
                ReleaseDC(self.control, dc);
                let rect = |r: RECT| json!({"x":r.left,"y":r.top,"width":r.right-r.left,"height":r.bottom-r.top});
                json!({
                    "icons":meters.iter().zip(&layout).filter_map(|(value, item)| item.icon.map(|icon|json!({"provider":provider_key(value.provider),"rect":rect(icon)}))).collect::<Vec<_>>(),
                    "meters":layout.iter().map(|item|json!({"track":rect(item.track),"text":rect(item.text)})).collect::<Vec<_>>()
                })
            }
        };
        json!({"window":self.window as usize,"control":self.control as usize,"enabled":self.enabled.get(),"native_visible":unsafe{IsWindowVisible(self.window)!=0},"bounds":bounds(self.window),"geometry":geometry,"tooltip":chrome::tooltip_text(self.control),"meters":meters.iter().map(|value|json!({"provider":provider_key(value.provider),"label":value.label,"display_text":value.display_text,"text":value.text,"percent":value.percent,"stale":value.stale})).collect::<Vec<_>>()})
    }
}
