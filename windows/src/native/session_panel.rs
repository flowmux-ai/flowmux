// SPDX-License-Identifier: GPL-3.0-or-later
//! Native agent-history dock. All filesystem and resume work belongs to its controller.
use super::*;
use crate::session_history::HistorySession;
use std::{
    cell::Cell,
    hash::{Hash, Hasher},
};
use windows_sys::Win32::{
    System::SystemServices::{SS_EDITCONTROL, SS_ENDELLIPSIS, SS_NOPREFIX},
    UI::{
        Controls::{
            DRAWITEMSTRUCT, EM_LIMITTEXT, EM_SETCUEBANNER, ODS_FOCUS, ODS_NOFOCUSRECT,
            ODS_SELECTED, ODT_LISTBOX,
        },
        Input::KeyboardAndMouse::EnableWindow,
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
    },
};
const SEARCH: usize = 4;
const LIST: usize = 5;
const RESUME: usize = 7;
const SPLITTER: usize = 9;
const INPUT_SUBCLASS: usize = 0x464d5353;
#[derive(Clone, Debug)]
pub(super) enum UiAction {
    Refresh,
    Close,
    Filter(String),
    Select(String),
    Resume(String),
    Layout,
    Resize(i32),
}
#[derive(Clone, PartialEq)]
struct Row {
    id: String,
    agent: String,
    title: String,
    project: String,
    updated: String,
    caption: String,
    searchable: String,
    color: COLORREF,
}
#[derive(Clone)]
struct Route {
    id: Uuid,
    search: HWND,
    list: HWND,
    preview: HWND,
    resume: HWND,
    rows: Vec<Row>,
    filtered: Vec<usize>,
    query: String,
    selected: Option<String>,
    open: bool,
    loading: bool,
    can_resume: bool,
    composing: bool,
    settling: bool,
    updating: bool,
}
thread_local! {static ROUTES: RefCell<HashMap<isize,Route>> = RefCell::new(HashMap::new());}
fn route(window: HWND) -> Option<Route> {
    ROUTES.with(|routes| routes.borrow().get(&(window as isize)).cloned())
}
pub(super) fn splitter_target(window: HWND) -> Option<Uuid> {
    unsafe {
        if GetDlgCtrlID(window) != SPLITTER as i32
            || GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_VISIBLE == 0
            || !enabled(window)
        {
            return None;
        }
        ROUTES.with(|routes| {
            routes
                .borrow()
                .get(&(GetParent(window) as isize))
                .filter(|r| r.open)
                .map(|r| r.id)
        })
    }
}
fn read(window: HWND) -> String {
    unsafe {
        let length = GetWindowTextLengthW(window).clamp(0, 16 * 1024 * 1024);
        let mut text = vec![0u16; length as usize + 1];
        let count = GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32);
        String::from_utf16_lossy(&text[..count.max(0) as usize])
    }
}
fn set_text(window: HWND, value: &str) {
    if read(window) != value {
        unsafe {
            SetWindowTextW(window, wide(value).as_ptr());
        }
    }
}
fn enabled(window: HWND) -> bool {
    unsafe { IsWindowEnabled(window) != 0 && IsWindowEnabled(GetParent(window)) != 0 }
}
fn update_actions(window: HWND) {
    if let Some(r) = route(window) {
        unsafe {
            let ready = !r.loading && !r.composing && !r.settling;
            EnableWindow(GetDlgItem(window, 2), i32::from(ready));
            EnableWindow(r.resume, i32::from(ready && r.can_resume));
        }
    }
}
fn emit(window: HWND, action: UiAction) {
    let Some(r) = route(window) else {
        return;
    };
    if !r.open
        || !enabled(window)
        || ((r.loading || r.composing || r.settling)
            && matches!(action, UiAction::Refresh | UiAction::Resume(_)))
    {
        return;
    }
    if let UiAction::Resume(id) = &action {
        if !r.can_resume
            || r.selected.as_ref() != Some(id)
            || !r.filtered.iter().any(|i| &r.rows[*i].id == id)
        {
            return;
        }
    }
    post(Event::Sessions(sessions::Signal::Ui(r.id, action)));
}
fn rebuild(window: HWND, notify: bool) {
    let Some(old) = route(window) else {
        return;
    };
    if old.composing && notify {
        return;
    }
    // Collection results may arrive during IME composition. Rebuild the row
    // indices against the last committed filter while leaving the EDIT intact.
    let query = if old.composing {
        old.query.clone()
    } else {
        read(old.search)
    };
    let needle = chrome::search_key(&query);
    let filtered: Vec<_> = old
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.searchable.contains(&needle))
        .map(|(i, _)| i)
        .collect();
    let selected = old
        .selected
        .as_ref()
        .filter(|id| filtered.iter().any(|i| &old.rows[*i].id == *id))
        .cloned();
    let lost = old.selected.is_some() && selected.is_none();
    ROUTES.with(|routes| {
        if let Some(r) = routes.borrow_mut().get_mut(&(window as isize)) {
            r.updating = true;
            r.query = query.clone();
            r.filtered = filtered.clone();
            r.selected = selected.clone();
            if lost {
                r.can_resume = false;
            }
        }
    });
    unsafe {
        let top = SendMessageW(old.list, LB_GETTOPINDEX, 0, 0).max(0);
        SendMessageW(old.list, WM_SETREDRAW, 0, 0);
        SendMessageW(old.list, LB_RESETCONTENT, 0, 0);
        for index in &filtered {
            SendMessageW(
                old.list,
                LB_ADDSTRING,
                0,
                wide(&old.rows[*index].caption).as_ptr() as LPARAM,
            );
        }
        if let Some(index) = selected
            .as_ref()
            .and_then(|id| filtered.iter().position(|i| &old.rows[*i].id == id))
        {
            SendMessageW(old.list, LB_SETCURSEL, index, 0);
        }
        SendMessageW(old.list, LB_SETTOPINDEX, top as usize, 0);
        SendMessageW(old.list, WM_SETREDRAW, 1, 0);
        InvalidateRect(old.list, std::ptr::null(), 1);
        if lost {
            EnableWindow(old.resume, 0);
            set_text(old.preview, "");
        }
    }
    ROUTES.with(|routes| {
        if let Some(r) = routes.borrow_mut().get_mut(&(window as isize)) {
            r.updating = false;
        }
    });
    if notify {
        emit(window, UiAction::Filter(query));
    }
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
    let lost_composition = message == WM_KILLFOCUS && route(parent).is_some_and(|r| r.composing);
    let changed = ROUTES.with(|routes| {
        if let Some(r) = routes.borrow_mut().get_mut(&(parent as isize)) {
            let pending = r.composing || r.settling;
            match message {
                WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION => {
                    r.composing = message == WM_IME_STARTCOMPOSITION;
                    r.settling = true;
                }
                WM_KEYDOWN if w == 229 => r.settling = true,
                WM_KEYUP if crate::keybindings::native_modifier(w, l).is_none() => {
                    r.settling = false
                }
                WM_KILLFOCUS => {
                    r.composing = false;
                    r.settling = false;
                }
                _ => {}
            }
            pending != (r.composing || r.settling)
        } else {
            false
        }
    });
    let result = DefSubclassProc(window, message, w, l);
    if message == WM_IME_ENDCOMPOSITION || lost_composition {
        rebuild(parent, true);
    }
    if changed {
        update_actions(parent);
    }
    if message == WM_NCDESTROY {
        RemoveWindowSubclass(window, Some(input_proc), id);
    }
    result
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_COMMAND if l != 0 => {
            let child = l as HWND;
            let Some(r) = route(window) else {
                return 0;
            };
            if r.updating
                || GetParent(child) != window
                || !enabled(window)
                || IsWindowEnabled(child) == 0
            {
                return 0;
            }
            match (w & 0xffff, (w >> 16) as u32) {
                (2, BN_CLICKED) => emit(window, UiAction::Refresh),
                (3, BN_CLICKED) => emit(window, UiAction::Close),
                (SEARCH, EN_CHANGE) if !r.composing => rebuild(window, true),
                (LIST, LBN_SELCHANGE) => {
                    let index = SendMessageW(r.list, LB_GETCURSEL, 0, 0);
                    if index >= 0 {
                        if let Some(row) = r.filtered.get(index as usize).map(|i| &r.rows[*i]) {
                            ROUTES.with(|routes| {
                                if let Some(r) = routes.borrow_mut().get_mut(&(window as isize)) {
                                    r.selected = Some(row.id.clone());
                                    r.can_resume = false;
                                }
                            });
                            EnableWindow(r.resume, 0);
                            set_text(r.preview, "Loading preview…");
                            emit(window, UiAction::Select(row.id.clone()));
                        }
                    }
                }
                (RESUME, BN_CLICKED) => {
                    if let Some(id) = r.selected {
                        emit(window, UiAction::Resume(id));
                    }
                }
                _ => {}
            }
            return 0;
        }
        WM_CLOSE => {
            emit(window, UiAction::Close);
            return 0;
        }
        WM_SIZE | WM_DPICHANGED => {
            emit(window, UiAction::Layout);
            return 0;
        }
        WM_DRAWITEM if l != 0 && draw_row(&*(l as *const DRAWITEMSTRUCT)) => return 1,
        WM_NCDESTROY => {
            ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
        }
        _ => {}
    }
    chrome::message(window, message, w, l).unwrap_or_else(|| DefWindowProcW(window, message, w, l))
}
unsafe fn paint_text(dc: HDC, text: &str, rect: &mut RECT, flags: u32) {
    let original: Vec<u16> = text.encode_utf16().collect();
    let text = chrome::caption_for_paint(&original);
    DrawTextW(
        dc,
        text.as_ptr(),
        text.len() as i32,
        rect,
        flags | DT_NOPREFIX,
    );
}
unsafe fn draw_row(item: &DRAWITEMSTRUCT) -> bool {
    if item.CtlType != ODT_LISTBOX {
        return false;
    }
    let Some(r) = route(GetParent(item.hwndItem)).filter(|r| r.list == item.hwndItem) else {
        return false;
    };
    let palette = chrome::palette();
    let selected = item.itemState & ODS_SELECTED != 0;
    let saved = SaveDC(item.hDC);
    if saved == 0 {
        return false;
    }
    SetDCBrushColor(
        item.hDC,
        if selected {
            palette.selected
        } else {
            palette.surface
        },
    );
    FillRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
    if let Some(row) = r.filtered.get(item.itemID as usize).map(|i| &r.rows[*i]) {
        let p =
            |n: i32| chrome::sidebar_size(n) * GetDpiForWindow(item.hwndItem).max(96) as i32 / 96;
        SetBkMode(item.hDC, TRANSPARENT as i32);
        SelectObject(
            item.hDC,
            SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HGDIOBJ,
        );
        let fg = if selected && palette.high_contrast {
            GetSysColor(COLOR_HIGHLIGHTTEXT)
        } else {
            palette.foreground
        };
        SetDCBrushColor(item.hDC, if palette.high_contrast { fg } else { row.color });
        FillRect(
            item.hDC,
            &RECT {
                left: item.rcItem.left + p(4),
                top: item.rcItem.top + p(7),
                right: item.rcItem.left + p(7),
                bottom: item.rcItem.bottom - p(7),
            },
            GetStockObject(DC_BRUSH),
        );
        SetTextColor(item.hDC, fg);
        let mut rect = RECT {
            left: item.rcItem.left + p(14),
            top: item.rcItem.top + p(5),
            right: item.rcItem.right - p(8),
            bottom: item.rcItem.top + p(27),
        };
        paint_text(
            item.hDC,
            &row.title,
            &mut rect,
            DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        SetTextColor(
            item.hDC,
            if palette.high_contrast {
                fg
            } else {
                palette.muted
            },
        );
        rect.top = item.rcItem.top + p(29);
        rect.bottom = item.rcItem.top + p(48);
        paint_text(
            item.hDC,
            &format!("{} · {}", row.agent, row.project),
            &mut rect,
            DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        rect.top = item.rcItem.top + p(49);
        rect.bottom = item.rcItem.bottom - p(4);
        paint_text(
            item.hDC,
            &row.updated,
            &mut rect,
            DT_SINGLELINE | DT_END_ELLIPSIS,
        );
    }
    if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
        DrawFocusRect(item.hDC, &item.rcItem);
    }
    RestoreDC(item.hDC, saved);
    true
}
fn geometry(window: HWND, parent: HWND) -> Value {
    unsafe {
        let mut rect = RECT::default();
        GetWindowRect(window, &mut rect);
        let mut point = POINT {
            x: rect.left,
            y: rect.top,
        };
        ScreenToClient(parent, &mut point);
        json!({"x":point.x,"y":point.y,"width":rect.right-rect.left,"height":rect.bottom-rect.top})
    }
}
fn place(window: HWND, x: i32, y: i32, w: i32, h: i32) {
    unsafe {
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
pub(super) struct Panel {
    pub(super) window: HWND,
    owner: HWND,
    id: Uuid,
    background: bool,
    heading: HWND,
    message: HWND,
    search: HWND,
    list: HWND,
    splitter: HWND,
    preview: HWND,
    preview_dip: Cell<i32>,
    preview_limit: Cell<i32>,
    refresh: HWND,
    close: HWND,
    resume: HWND,
    area: Cell<Option<model::Rect>>,
    colors: HashMap<PathBuf, String>,
    status_text: String,
}
impl Drop for Panel {
    fn drop(&mut self) {
        ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
        for child in [
            self.heading,
            self.message,
            self.search,
            self.list,
            self.splitter,
            self.preview,
            self.refresh,
            self.close,
            self.resume,
        ] {
            if !child.is_null() {
                chrome::unregister(child);
            }
        }
        chrome::unregister(self.window);
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    pub(super) fn new(
        owner: HWND,
        background: bool,
        id: Uuid,
        preview_dip: i32,
    ) -> anyhow::Result<Self> {
        unsafe {
            anyhow::ensure!(IsWindow(owner) != 0, "Session panel owner is unavailable");
            let class = wide("flowmux.windows.sessions");
            let instance = GetModuleHandleW(std::ptr::null());
            let definition = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&definition) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "Cannot register session panel"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Agent sessions").as_ptr(),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            checked((!window.is_null()) as i32)?;
            let mut panel = Self {
                window,
                owner,
                id,
                background,
                heading: std::ptr::null_mut(),
                message: std::ptr::null_mut(),
                search: std::ptr::null_mut(),
                list: std::ptr::null_mut(),
                splitter: std::ptr::null_mut(),
                preview_dip: Cell::new(preview_dip.max(150)),
                preview_limit: Cell::new(0),
                preview: std::ptr::null_mut(),
                refresh: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                resume: std::ptr::null_mut(),
                area: Cell::new(None),
                colors: HashMap::new(),
                status_text: String::new(),
            };
            panel.heading =
                panel.child("STATIC", "Agent sessions", 1, SS_NOPREFIX | SS_ENDELLIPSIS)?;
            panel.refresh = panel.child(
                "BUTTON",
                "Refresh sessions",
                2,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.close = panel.child(
                "BUTTON",
                "Close sessions",
                3,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.search = panel.child(
                "EDIT",
                "",
                SEARCH,
                WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL as u32,
            )?;
            SendMessageW(panel.search, EM_LIMITTEXT, 1024, 0);
            SendMessageW(
                panel.search,
                EM_SETCUEBANNER,
                1,
                wide("Search sessions, paths, or IDs").as_ptr() as LPARAM,
            );
            panel.list = panel.child(
                "LISTBOX",
                "Agent sessions",
                LIST,
                WS_TABSTOP
                    | WS_VSCROLL
                    | LBS_NOTIFY as u32
                    | LBS_NOINTEGRALHEIGHT as u32
                    | LBS_OWNERDRAWFIXED as u32
                    | LBS_HASSTRINGS as u32,
            )?;
            panel.splitter = panel.child(
                "BUTTON",
                "Resize conversation preview (Up/Down; Home resets)",
                SPLITTER,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.preview = panel.child(
                "EDIT",
                "",
                6,
                WS_TABSTOP
                    | WS_BORDER
                    | WS_VSCROLL
                    | ES_MULTILINE as u32
                    | ES_READONLY as u32
                    | ES_AUTOVSCROLL as u32,
            )?;
            panel.resume = panel.child(
                "BUTTON",
                "Resume in new tab",
                RESUME,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.message = panel.child("STATIC", "", 8, SS_NOPREFIX | SS_EDITCONTROL)?;
            ROUTES.with(|routes| {
                routes.borrow_mut().insert(
                    window as isize,
                    Route {
                        id,
                        search: panel.search,
                        list: panel.list,
                        preview: panel.preview,
                        resume: panel.resume,
                        rows: vec![],
                        filtered: vec![],
                        query: String::new(),
                        selected: None,
                        open: false,
                        loading: false,
                        can_resume: false,
                        composing: false,
                        settling: false,
                        updating: false,
                    },
                )
            });
            anyhow::ensure!(
                SetWindowSubclass(panel.search, Some(input_proc), INPUT_SUBCLASS, 0) != 0,
                "Cannot preserve session search composition"
            );
            EnableWindow(panel.resume, 0);
            panel.refresh_theme()?;
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
            Ok(window)
        }
    }
    pub(super) fn selected(&self) -> Option<String> {
        ROUTES.with(|routes| {
            routes
                .borrow()
                .get(&(self.window as isize))
                .and_then(|r| r.selected.clone())
        })
    }
    pub(super) fn composition_pending(&self) -> bool {
        ROUTES.with(|routes| {
            routes
                .borrow()
                .get(&(self.window as isize))
                .is_some_and(|r| r.composing || r.settling)
        })
    }
    pub(super) fn update(
        &mut self,
        rows: &[HistorySession],
        selected: Option<&str>,
        status: &str,
        preview: &str,
        can_resume: bool,
        loading: bool,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            unsafe { IsWindow(self.window) } != 0,
            "Session panel is unavailable"
        );
        let mut used: Vec<_> = self.colors.values().cloned().collect();
        let values: Vec<_> = rows
            .iter()
            .map(|session| {
                let color = self.colors.entry(session.cwd.clone()).or_insert_with(|| {
                    let mut hash = std::collections::hash_map::DefaultHasher::new();
                    session.cwd.hash(&mut hash);
                    let value = flowmux_core::pick_workspace_color(&used, hash.finish() as u128);
                    used.push(value.clone());
                    value
                });
                let rgb =
                    u32::from_str_radix(color.trim_start_matches('#'), 16).unwrap_or(0x6b8afd);
                let color = ((rgb >> 16) & 255) | (rgb & 0xff00) | ((rgb & 255) << 16);
                let agent = session.agent.name().to_owned();
                let project = session
                    .cwd
                    .file_name()
                    .unwrap_or(session.cwd.as_os_str())
                    .to_string_lossy()
                    .into_owned();
                let modified: chrono::DateTime<chrono::Local> = session.modified.into();
                let updated = modified.format("%m-%d %H:%M").to_string();
                let caption = format!(
                    "{agent} · {}\n{project} · {updated}\n{}\n{}\n{}",
                    session.title,
                    session.summary,
                    session.cwd.display(),
                    session.id
                );
                Row {
                    id: session.id.clone(),
                    agent,
                    title: session.title.clone(),
                    project,
                    updated,
                    searchable: chrome::search_key(&caption),
                    caption,
                    color,
                }
            })
            .collect();
        let changed = ROUTES.with(|routes| {
            let mut routes = routes.borrow_mut();
            let Some(r) = routes.get_mut(&(self.window as isize)) else {
                return false;
            };
            let changed = r.rows != values || r.selected.as_deref() != selected;
            r.rows = values;
            r.selected = selected.map(str::to_owned);
            r.loading = loading;
            changed
        });
        if changed {
            rebuild(self.window, false);
        }
        let selected = route(self.window).and_then(|r| r.selected);
        let allow = can_resume && !loading && selected.is_some();
        ROUTES.with(|routes| {
            if let Some(r) = routes.borrow_mut().get_mut(&(self.window as isize)) {
                r.can_resume = allow;
            }
        });
        self.status_text = status.to_owned();
        set_text(self.message, status);
        set_text(self.preview, preview);
        update_actions(self.window);
        unsafe {
            InvalidateRect(self.list, std::ptr::null(), 1);
        }
        self.layout(self.area.get());
        Ok(())
    }
    pub(super) fn preview_height(&self) -> Option<i32> {
        splitter_target(self.splitter)?;
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(self.preview, &mut rect);
        }
        Some(rect.bottom - rect.top)
    }
    pub(super) fn resize_preview(&self, height: i32) -> i32 {
        let dpi = unsafe { GetDpiForWindow(self.window).max(96) } as i32;
        self.preview_dip
            .set((height.saturating_mul(96) / dpi).clamp(
                150,
                (self.preview_limit.get().saturating_mul(96) / dpi).max(150),
            ));
        self.preview_dip.get()
    }
    pub(super) fn layout(&self, rect: Option<model::Rect>) {
        self.area.set(rect);
        ROUTES.with(|routes| {
            if let Some(r) = routes.borrow_mut().get_mut(&(self.window as isize)) {
                r.open = rect.is_some();
            }
        });
        unsafe {
            if !self.background {
                ShowWindow(
                    self.window,
                    if rect.is_some() { SW_SHOWNA } else { SW_HIDE },
                );
            }
        }
        let Some(area) = rect else {
            return;
        };
        let p = |n: i32| (n * unsafe { GetDpiForWindow(self.window).max(96) } as i32 + 48) / 96;
        place(self.window, area.x, area.y, area.width, area.height);
        let width = area.width.max(1);
        let height = area.height.max(1);
        let margin = p(8).min(width / 4);
        let inner = (width - 2 * margin).max(1);
        let tool = p(28).min(inner / 3);
        let gap = p(4);
        place(
            self.heading,
            margin,
            p(6),
            (inner - tool * 2 - gap * 2).max(1),
            p(28),
        );
        place(
            self.refresh,
            width - margin - tool * 2 - gap,
            p(6),
            tool,
            p(28),
        );
        place(self.close, width - margin - tool, p(6), tool, p(28));
        let resume_height = p(30).min(height);
        let resume_y = (height - margin - resume_height).max(0);
        place(self.resume, margin, resume_y, inner, resume_height);
        let message_height = chrome::wrapped_text_height(self.message, inner)
            .max(p(34))
            .min((resume_y - p(40 + 6 + 28) - gap).max(0));
        place(self.message, margin, p(40), inner, message_height);
        let search_y = p(40 + 6) + message_height;
        place(self.search, margin, search_y, inner, p(28));
        let content_top = search_y + p(28 + 8);
        let available = (resume_y - gap - content_top).max(0);
        let splitter_height = p(5);
        let preview_limit = (available - p(140) - splitter_height).max(0);
        self.preview_limit.set(preview_limit);
        let preview_height = p(self.preview_dip.get()).min(preview_limit);
        let list_height = (available
            - preview_height
            - if preview_height > 0 {
                splitter_height
            } else {
                0
            })
        .max(0);
        place(self.list, margin, content_top, inner, list_height);
        place(
            self.splitter,
            margin,
            content_top + list_height,
            inner,
            splitter_height,
        );
        place(
            self.preview,
            margin,
            content_top + list_height + splitter_height,
            inner,
            preview_height,
        );
        unsafe {
            SendMessageW(
                self.list,
                LB_SETITEMHEIGHT,
                0,
                p(chrome::sidebar_size(72)) as LPARAM,
            );
            for (window, show) in [
                (self.list, list_height > 0),
                (self.preview, preview_height > 0),
                (self.splitter, preview_limit > p(150)),
                (self.message, height >= p(160)),
                (self.search, height >= p(160)),
                (self.heading, height >= p(68)),
                (self.refresh, height >= p(68)),
                (self.close, height >= p(68)),
            ] {
                if show {
                    SetWindowLongPtrW(
                        window,
                        GWL_STYLE,
                        (GetWindowLongPtrW(window, GWL_STYLE) as u32 | WS_VISIBLE) as isize,
                    );
                } else {
                    SetWindowLongPtrW(
                        window,
                        GWL_STYLE,
                        (GetWindowLongPtrW(window, GWL_STYLE) as u32 & !WS_VISIBLE) as isize,
                    );
                }
            }
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        let Some(r) = route(self.window) else {
            return false;
        };
        unsafe {
            if !r.open
                || !enabled(self.window)
                || (message.hwnd != self.window && IsChild(self.window, message.hwnd) == 0)
            {
                return false;
            }
            if message.message == WM_KEYDOWN
                && message.hwnd == self.splitter
                && splitter_target(self.splitter).is_some()
                && matches!(message.wParam, 36 | 38 | 40)
            {
                emit(
                    self.window,
                    UiAction::Resize(match message.wParam {
                        38 => 20,
                        40 => -20,
                        _ => 0,
                    }),
                );
                return true;
            }
            if message.message == WM_KEYDOWN && matches!(message.wParam, 13 | 27) {
                if r.composing || r.settling || message.lParam & (1 << 30) != 0 {
                    return false;
                }
                if message.wParam == 27 {
                    emit(self.window, UiAction::Close);
                } else if message.hwnd == self.resume {
                    if let Some(id) = r.selected {
                        emit(self.window, UiAction::Resume(id));
                    }
                } else if message.hwnd == self.refresh {
                    emit(self.window, UiAction::Refresh);
                } else if message.hwnd == self.close {
                    emit(self.window, UiAction::Close);
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
    pub(super) fn refresh_theme(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            unsafe { IsWindow(self.window) } != 0,
            "Session panel is unavailable"
        );
        for window in [self.window, self.heading] {
            chrome::register_control(window, chrome::ControlRole::Static);
        }
        chrome::register_sidebar_control(self.message, chrome::ControlRole::Caption);
        chrome::register_control(self.search, chrome::ControlRole::Edit);
        chrome::register_sidebar_control(self.preview, chrome::ControlRole::Edit);
        chrome::register_sidebar_control(self.list, chrome::ControlRole::Listbox);
        for (window, kind) in [
            (self.refresh, chrome::ChromeIcon::Reload),
            (self.close, chrome::ChromeIcon::Close),
            (self.splitter, chrome::ChromeIcon::More),
        ] {
            chrome::register_button(
                window,
                chrome::Role::Icon {
                    kind,
                    marked: false,
                },
            );
        }
        chrome::register_button(self.resume, chrome::Role::Button);
        unsafe {
            InvalidateRect(self.window, std::ptr::null(), 1);
            InvalidateRect(self.list, std::ptr::null(), 1);
        }
        self.layout(self.area.get());
        Ok(())
    }
    pub(super) fn capture_window(&self) -> HWND {
        self.window
    }
    pub(super) fn status(&self) -> Value {
        let r = route(self.window);
        let rows:Vec<_>=r.as_ref().map(|r|r.filtered.iter().enumerate().map(|(index,i)|{let row=&r.rows[*i];json!({"index":index,"id":row.id,"agent":row.agent,"title":row.title,"project":row.project,"updated":row.updated,"text":row.caption,"color":row.color,"selected":r.selected.as_ref()==Some(&row.id)})}).collect()).unwrap_or_default();
        json!({"id":self.id,"window":self.window as usize,"owner":self.owner as usize,"open":self.area.get().is_some(),"native_visible":unsafe{IsWindowVisible(self.window)!=0},"bounds":self.area.get(),"query":read(self.search),"filter":r.as_ref().map(|r|&r.query),"query_handle":self.search as usize,"list":self.list as usize,"preview":self.preview as usize,"splitter":self.splitter as usize,"splitter_tooltip":chrome::tooltip_text(self.splitter),"preview_text":read(self.preview),"preview_bounds":geometry(self.preview,self.window),"refresh":self.refresh as usize,"close":self.close as usize,"refresh_tooltip":chrome::tooltip_text(self.refresh),"close_tooltip":chrome::tooltip_text(self.close),"resume":self.resume as usize,"resume_enabled":unsafe{IsWindowEnabled(self.resume)!=0},"loading":r.as_ref().is_some_and(|r|r.loading),"selected":r.as_ref().and_then(|r|r.selected.as_ref()),"composing":r.as_ref().is_some_and(|r|r.composing),"status":self.status_text,"rows":rows})
    }
}
