// SPDX-License-Identifier: GPL-3.0-or-later
//! Profile-local bookmarks; disk work never blocks the native popup.
use super::super::chrome as shell;
use super::*;
use flowmux_browser::Bookmark;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    sync::mpsc::{self, Receiver, TryRecvError},
};
use windows_sys::Win32::{
    Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH},
    UI::{
        Controls::SetScrollInfo,
        Input::KeyboardAndMouse::{EnableWindow, GetFocus},
    },
};

const MAX_BYTES: usize = 1024 * 1024;
const MAX_ROWS: usize = 256;
static NEXT: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy)]
pub(crate) enum UiAction {
    Close,
    Choose(u16),
    Scroll(i32),
    ScrollTo(i32),
    Layout,
}
enum Change {
    Load,
    Add(Bookmark),
    Remove(String),
}
fn store(path: &std::path::Path, change: Change) -> anyhow::Result<Vec<Bookmark>> {
    let _lease = if matches!(change, Change::Load) {
        None
    } else {
        Some(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(path.with_extension("lock"))
                .context("Another window is updating bookmarks; try again")?,
        )
    };
    let mut bytes = Vec::new();
    let mut missing = false;
    match File::open(path) {
        Ok(file) => {
            file.take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => missing = true,
        Err(error) => return Err(error.into()),
    }
    anyhow::ensure!(bytes.len() <= MAX_BYTES, "Bookmark file exceeds 1 MiB");
    let mut values: Vec<Bookmark> = if missing {
        vec![]
    } else {
        serde_json::from_slice(&bytes)?
    };
    anyhow::ensure!(
        values.len() <= MAX_ROWS,
        "Bookmark file exceeds 256 entries"
    );
    match change {
        Change::Load => return Ok(values),
        Change::Add(mut value) => {
            value.url = domain::url(&value.url)?;
            anyhow::ensure!(value.url != "about:blank", "This page cannot be bookmarked");
            values.retain(|old| old.url != value.url);
            anyhow::ensure!(
                values.len() < MAX_ROWS,
                "Remove a bookmark before adding another (256 entries)"
            );
            values.insert(0, value);
        }
        Change::Remove(url) => values.retain(|value| value.url != url),
    }
    let bytes = serde_json::to_vec(&values)?;
    anyhow::ensure!(bytes.len() <= MAX_BYTES, "Bookmark file exceeds 1 MiB");
    let destination = std::fs::canonicalize(path.parent().context("Bookmark directory missing")?)?
        .join(path.file_name().context("Bookmark filename missing")?);
    let temporary = destination.with_extension(format!("{}.tmp", Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        checked(unsafe {
            MoveFileExW(
                wide(&temporary).as_ptr(),
                wide(&destination).as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result?;
    Ok(values)
}

fn emit(window: HWND, action: UiAction) {
    let generation = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) } as u64;
    post(Event::Browser(Signal::Bookmarks(generation, action)));
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let popup = if GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_CHILD != 0 {
        GetParent(window)
    } else {
        window
    };
    match message {
        WM_CLOSE => {
            emit(popup, UiAction::Close);
            return 0;
        }
        WM_ACTIVATE if w & 0xffff == WA_INACTIVE as usize => emit(popup, UiAction::Close),
        WM_DPICHANGED => emit(popup, UiAction::Layout),
        WM_MOUSEWHEEL => {
            emit(popup, UiAction::Scroll(-((w >> 16) as i16 as i32) / 120));
            return 0;
        }
        WM_VSCROLL => {
            let mut info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_TRACKPOS,
                ..Default::default()
            };
            GetScrollInfo(window, SB_VERT, &mut info);
            match (w & 0xffff) as i32 {
                SB_LINEUP => emit(popup, UiAction::Scroll(-1)),
                SB_LINEDOWN => emit(popup, UiAction::Scroll(1)),
                SB_PAGEUP => emit(popup, UiAction::Scroll(-5)),
                SB_PAGEDOWN => emit(popup, UiAction::Scroll(5)),
                SB_TOP => emit(popup, UiAction::ScrollTo(0)),
                SB_BOTTOM => emit(popup, UiAction::ScrollTo(i32::MAX)),
                SB_THUMBTRACK | SB_THUMBPOSITION => emit(popup, UiAction::ScrollTo(info.nTrackPos)),
                _ => {}
            }
            return 0;
        }
        WM_COMMAND if w >> 16 == BN_CLICKED as usize => {
            let child = l as HWND;
            if !child.is_null()
                && GetParent(child) == window
                && GetDlgItem(window, (w & 0xffff) as i32) == child
                && IsWindowEnabled(child) != 0
            {
                emit(popup, UiAction::Choose(w as u16));
            }
            return 0;
        }
        _ => {}
    }
    shell::message(window, message, w, l).unwrap_or_else(|| DefWindowProcW(window, message, w, l))
}
struct Panel {
    window: HWND,
    owner: HWND,
    viewport: HWND,
    add: HWND,
    status: HWND,
    separator: HWND,
    status_text: RefCell<String>,
    last_layout: Option<(model::Rect, u32, usize, bool, bool, u64)>,
    surface: SurfaceId,
    generation: u64,
    created: u64,
    rows: Vec<(HWND, HWND)>,
    values: Vec<Bookmark>,
    first: usize,
    error: bool,
    background: bool,
}
impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    fn new(owner: HWND, surface: SurfaceId, background: bool) -> anyhow::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux.windows.bookmarks");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "Cannot register bookmarks"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT | WS_EX_TOOLWINDOW,
                class.as_ptr(),
                wide("Bookmarks").as_ptr(),
                WS_POPUP | WS_BORDER | WS_CLIPCHILDREN,
                0,
                0,
                336,
                200,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "Cannot create bookmarks");
            let mut panel = Self {
                window,
                owner,
                surface,
                background,
                viewport: std::ptr::null_mut(),
                add: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                separator: std::ptr::null_mut(),
                status_text: RefCell::new("Loading bookmarks…".into()),
                last_layout: None,
                generation: 0,
                created: 0,
                rows: vec![],
                values: vec![],
                first: 0,
                error: false,
            };
            panel.viewport = panel.child(
                window,
                "flowmux.windows.bookmarks",
                "Bookmarks",
                20,
                WS_CLIPCHILDREN | WS_VSCROLL,
            )?;
            // WS_EX_CONTROLPARENT is an extended style, not a child style.
            SetWindowLongPtrW(panel.viewport, GWL_EXSTYLE, WS_EX_CONTROLPARENT as isize);
            panel.add = panel.child(
                window,
                "BUTTON",
                "Bookmark this page",
                1,
                WS_TABSTOP | BS_OWNERDRAW as u32,
            )?;
            panel.status = panel.child(
                window,
                "STATIC",
                "Loading bookmarks…",
                2,
                windows_sys::Win32::System::SystemServices::SS_NOPREFIX,
            )?;
            panel.separator = panel.child(
                window,
                "STATIC",
                "",
                3,
                windows_sys::Win32::System::SystemServices::SS_ETCHEDHORZ,
            )?;
            shell::register_button(panel.add, shell::Role::Button);
            panel.invalidate_actions();
            panel.created = panel.generation;
            Ok(panel)
        }
    }
    unsafe fn child(
        &self,
        parent: HWND,
        class: &str,
        text: &str,
        id: usize,
        style: u32,
    ) -> anyhow::Result<HWND> {
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
        anyhow::ensure!(!window.is_null(), "Cannot create bookmark control");
        shell::register_control(window, shell::ControlRole::Static);
        Ok(window)
    }
    fn invalidate_actions(&mut self) {
        self.generation = NEXT.fetch_add(1, Ordering::Relaxed);
        unsafe {
            SetWindowLongPtrW(self.window, GWLP_USERDATA, self.generation as isize);
        }
    }
    fn message(&self, text: &str) {
        if self.status_text.borrow().as_str() == text {
            return;
        }
        *self.status_text.borrow_mut() = text.to_string();
        unsafe {
            SetWindowTextW(self.status, wide(text).as_ptr());
        }
    }
    fn render(&mut self, result: Result<Vec<Bookmark>, String>) -> anyhow::Result<()> {
        self.invalidate_actions();
        for (open, remove) in self.rows.drain(..) {
            unsafe {
                DestroyWindow(open);
                DestroyWindow(remove);
            }
        }
        self.first = 0;
        self.error = result.is_err();
        self.values = match result {
            Ok(values) => {
                self.message(if values.is_empty() {
                    "No bookmarks yet"
                } else {
                    ""
                });
                values
            }
            Err(error) => {
                self.message(&error);
                vec![]
            }
        };
        for (index, value) in self.values.iter().enumerate() {
            let title = value.title.replace(['\n', '\r'], " ");
            let open = unsafe {
                self.child(
                    self.viewport,
                    "BUTTON",
                    &format!("{title}\n{}", value.url),
                    100 + index * 2,
                    WS_TABSTOP | BS_OWNERDRAW as u32,
                )?
            };
            shell::register_button(
                open,
                shell::Role::Workspace {
                    tree: false,
                    selected: false,
                    color: None,
                    unread: false,
                    attention: false,
                },
            );
            let remove = unsafe {
                self.child(
                    self.viewport,
                    "BUTTON",
                    "Remove bookmark",
                    101 + index * 2,
                    WS_TABSTOP | BS_OWNERDRAW as u32,
                )?
            };
            shell::register_button(
                remove,
                shell::Role::Icon {
                    kind: shell::ChromeIcon::Delete,
                    marked: false,
                },
            );
            self.rows.push((open, remove));
        }
        Ok(())
    }
    fn layout(&mut self, anchor: (i32, i32), busy: bool, can_add: bool) {
        unsafe {
            let dpi = GetDpiForWindow(self.owner).max(96) as i32;
            let p = |v: i32| (v * dpi + 48) / 96;
            let mut monitor = MONITORINFO {
                cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if GetMonitorInfoW(
                MonitorFromWindow(self.owner, MONITOR_DEFAULTTONEAREST),
                &mut monitor,
            ) == 0
            {
                return;
            }
            let work = monitor.rcWork;
            let width = p(336).min((work.right - work.left).max(1));
            let status_visible = !self.status_text.borrow().is_empty();
            let header = if status_visible { 88 } else { 52 };
            let height = p(header + (self.rows.len().min(7) as i32) * 54)
                .min((work.bottom - work.top).max(1));
            let x = anchor
                .0
                .clamp(work.left, (work.right - width).max(work.left));
            let y = if anchor.1 + height <= work.bottom {
                anchor.1
            } else {
                (anchor.1 - height - p(30)).max(work.top)
            };
            let key = (
                model::Rect {
                    x,
                    y,
                    width,
                    height,
                },
                dpi as u32,
                self.first,
                busy,
                can_add,
                self.generation,
            );
            if self.last_layout == Some(key) {
                return;
            }
            self.last_layout = Some(key);
            SetWindowPos(self.window, HWND_TOP, x, y, width, height, SWP_NOACTIVATE);
            let mut client = RECT::default();
            GetClientRect(self.window, &mut client);
            let place = |window: HWND, x: i32, y: i32, w: i32, h: i32| {
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    x,
                    y,
                    w.max(1),
                    h.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            };
            place(self.add, p(8), p(8), client.right - p(16), p(28));
            place(self.separator, p(8), p(39), client.right - p(16), p(1));
            place(self.status, p(8), p(40), client.right - p(16), p(36));
            EnableWindow(self.add, i32::from(!busy && !self.error && can_add));
            ShowWindow(
                self.status,
                if status_visible { SW_SHOWNA } else { SW_HIDE },
            );
            let viewport_height = (client.bottom - p(header)).max(1);
            place(
                self.viewport,
                p(8),
                p(header - 8),
                client.right - p(16),
                viewport_height,
            );
            let visible = ((viewport_height + p(6)) / p(54)).max(1) as usize;
            self.first = self.first.min(self.rows.len().saturating_sub(visible));
            let info = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: self.rows.len().saturating_sub(1) as i32,
                nPage: visible as u32,
                nPos: self.first as i32,
                ..Default::default()
            };
            SetScrollInfo(self.viewport, SB_VERT, &info, 1);
            let mut viewport = RECT::default();
            GetClientRect(self.viewport, &mut viewport);
            for (index, (open, remove)) in self.rows.iter().enumerate() {
                let shown = index >= self.first && index < self.first + visible;
                let top = (index as i32 - self.first as i32) * p(54);
                place(*open, 0, top, (viewport.right - p(34)).max(1), p(48));
                place(
                    *remove,
                    (viewport.right - p(28)).max(0),
                    top + p(10),
                    p(28),
                    p(28),
                );
                ShowWindow(*open, if shown { SW_SHOWNA } else { SW_HIDE });
                ShowWindow(*remove, if shown { SW_SHOWNA } else { SW_HIDE });
                EnableWindow(
                    *open,
                    i32::from(shown && !busy && domain::url(&self.values[index].url).is_ok()),
                );
                EnableWindow(*remove, i32::from(shown && !busy));
            }
            if !self.background {
                ShowWindow(self.window, SW_SHOWNA);
            }
        }
    }
    fn handle_message(&self, message: &MSG) -> bool {
        if unsafe { GetAncestor(message.hwnd, GA_ROOT) } != self.window {
            return false;
        }
        if message.message == WM_KEYDOWN && message.wParam == 0x1b {
            emit(self.window, UiAction::Close);
            return true;
        }
        if message.message == WM_KEYDOWN && matches!(message.wParam, 0x21..=0x24) {
            let action = match message.wParam {
                0x21 => UiAction::Scroll(-5),
                0x22 => UiAction::Scroll(5),
                0x23 => UiAction::ScrollTo(i32::MAX),
                _ => UiAction::ScrollTo(0),
            };
            emit(self.window, action);
            return true;
        }
        !self.background && unsafe { IsDialogMessageW(self.window, message) != 0 }
    }
    fn diagnostics(&self) -> Value {
        json!({"window":self.window as usize,"owner":self.owner as usize,"surface":self.surface,"generation":self.generation,
            "native_visible":unsafe{IsWindowVisible(self.window)!=0},"add":self.add as usize,"status":self.status as usize,
            "viewport":self.viewport as usize,"first":self.first,"error":self.error,"message":self.status_text.borrow().as_str(),
            "rows":self.values.iter().zip(&self.rows).map(|(value,(open,remove))|json!({"title":value.title,"url":value.url,"open":*open as usize,"remove":*remove as usize})).collect::<Vec<_>>()})
    }
}
struct Pending {
    receiver: Receiver<Result<Vec<Bookmark>, String>>,
    started: Instant,
}
#[derive(Default)]
pub(crate) struct Controller {
    panel: Option<Panel>,
    pending: Option<Pending>,
}
impl Controller {
    pub(crate) fn handle_message(&self, message: &MSG) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|p| p.handle_message(message))
    }
    pub(crate) fn capture_window(&self) -> Option<HWND> {
        self.panel.as_ref().map(|p| p.window)
    }
    pub(crate) fn diagnostics(&self) -> Value {
        json!({"busy":self.pending.is_some(),"panel":self.panel.as_ref().map(Panel::diagnostics)})
    }
    fn close(&mut self) {
        self.panel.take();
    }
    pub(crate) fn close_surface(&mut self, id: SurfaceId) {
        if self.panel.as_ref().is_some_and(|p| p.surface == id) {
            self.close();
        }
    }
}
impl App {
    pub(crate) fn browser_bookmarks_show(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        self.browser_bookmarks.close();
        self.browser_bookmarks.panel = Some(Panel::new(
            self.surface_window(id),
            id,
            self.background_test,
        )?);
        if self.browser_bookmarks.pending.is_none() {
            self.browser_bookmarks_request(Change::Load)?;
        }
        self.browser_bookmarks_layout();
        if !self.background_test {
            if let Some(panel) = &self.browser_bookmarks.panel {
                unsafe {
                    SetFocus(panel.window);
                }
            }
        }
        Ok(())
    }
    fn browser_bookmarks_request(&mut self, change: Change) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.browser_bookmarks.pending.is_none(),
            "Bookmark operation is still finishing"
        );
        let path = self
            .browser_context
            .as_ref()
            .and_then(WebContext::data_directory)
            .context("Browser profile is unavailable")?
            .join("bookmarks.json");
        let (send, receive) = mpsc::sync_channel(1);
        // ponytail: one in-flight disk worker per host; a stalled filesystem keeps
        // mutations disabled, never spawns more workers or blocks popup dismissal.
        std::thread::Builder::new()
            .name("flowmux-bookmarks".into())
            .spawn(move || {
                let _ = send.send(store(&path, change).map_err(|error| format!("{error:#}")));
            })?;
        self.browser_bookmarks.pending = Some(Pending {
            receiver: receive,
            started: Instant::now(),
        });
        if let Some(panel) = &mut self.browser_bookmarks.panel {
            panel.invalidate_actions();
            panel.message("Loading bookmarks…");
        }
        self.browser_bookmarks_layout();
        Ok(())
    }
    pub(crate) fn browser_bookmarks_layout(&mut self) {
        let valid = self.browser_bookmarks.panel.as_ref().is_none_or(|panel| {
            self.browsers.get(&panel.surface).is_some_and(|b| {
                b.visible
                    && !b.native_closed.get()
                    && self.surface_window(panel.surface) == panel.owner
                    && unsafe { IsWindowEnabled(panel.owner) != 0 }
            })
        });
        if !valid {
            self.browser_bookmarks.close();
            return;
        }
        let busy = self.browser_bookmarks.pending.is_some();
        if let Some(panel) = &mut self.browser_bookmarks.panel {
            if let Some(browser) = self.browsers.get(&panel.surface) {
                panel.layout(
                    browser.chrome.bookmarks_anchor(),
                    busy,
                    !browser.preview_blocked.get()
                        && browser.url != "about:blank"
                        && domain::url(&browser.url).is_ok(),
                );
            }
        }
    }
    pub(crate) fn browser_bookmarks_tick(&mut self) {
        let result = self.browser_bookmarks.pending.as_ref().and_then(|pending| {
            match pending.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(Err("Bookmark worker stopped".into())),
                Err(TryRecvError::Empty) => {
                    if pending.started.elapsed() > Duration::from_secs(5) {
                        if let Some(panel) = &self.browser_bookmarks.panel {
                            panel.message("Storage is still busy. You can close this menu.");
                        }
                    }
                    None
                }
            }
        });
        if let Some(result) = result {
            self.browser_bookmarks.pending.take();
            if let Some(panel) = &mut self.browser_bookmarks.panel {
                if let Err(error) = panel.render(result) {
                    panel.error = true;
                    panel.message(&error.to_string());
                }
            }
        }
        self.browser_bookmarks_layout();
    }
    pub(crate) fn browser_bookmarks_ui(
        &mut self,
        generation: u64,
        action: UiAction,
    ) -> anyhow::Result<()> {
        self.browser_bookmarks_layout();
        let Some(panel) = self.browser_bookmarks.panel.as_ref().filter(|p| {
            p.generation == generation
                || (matches!(action, UiAction::Close | UiAction::Layout)
                    && generation >= p.created
                    && generation <= p.generation)
        }) else {
            return Ok(());
        };
        let surface = panel.surface;
        match action {
            UiAction::Close => {
                let owner = panel.owner;
                let focused = unsafe { GetAncestor(GetFocus(), GA_ROOT) == panel.window };
                self.browser_bookmarks.close();
                if !self.background_test && focused {
                    unsafe {
                        SetFocus(owner);
                    }
                }
            }
            UiAction::Layout => self.browser_bookmarks_layout(),
            UiAction::Scroll(delta) => {
                let panel = self.browser_bookmarks.panel.as_mut().unwrap();
                panel.first = (panel.first as i64 + i64::from(delta)).max(0) as usize;
                self.browser_bookmarks_layout();
            }
            UiAction::ScrollTo(value) => {
                self.browser_bookmarks.panel.as_mut().unwrap().first = value.max(0) as usize;
                self.browser_bookmarks_layout();
            }
            UiAction::Choose(code) if self.browser_bookmarks.pending.is_none() => {
                let control = unsafe {
                    GetDlgItem(
                        if code >= 100 {
                            panel.viewport
                        } else {
                            panel.window
                        },
                        code as i32,
                    )
                };
                if control.is_null()
                    || unsafe {
                        IsWindowEnabled(control) == 0
                            || GetWindowLongPtrW(control, GWL_STYLE) as u32 & WS_VISIBLE == 0
                    }
                {
                    return Ok(());
                }
                let change = if code == 1 {
                    let browser = self.browsers.get(&surface).context("Browser closed")?;
                    if browser.preview_blocked.get() || browser.url == "about:blank" {
                        return Ok(());
                    }
                    Some(Change::Add(Bookmark {
                        title: if browser.title.is_empty() {
                            browser.url.clone()
                        } else {
                            browser.title.clone()
                        },
                        url: browser.url.clone(),
                    }))
                } else if code >= 100 {
                    let index = (code as usize - 100) / 2;
                    let Some(value) = panel.values.get(index) else {
                        return Ok(());
                    };
                    if code % 2 == 0 {
                        let url = value.url.clone();
                        self.browsers
                            .get_mut(&surface)
                            .context("Browser closed")?
                            .navigate(&url)?;
                        self.browser_bookmarks.close();
                        self.browser_refresh(surface)?;
                        None
                    } else {
                        Some(Change::Remove(value.url.clone()))
                    }
                } else {
                    None
                };
                if let Some(change) = change {
                    if let Err(error) = self.browser_bookmarks_request(change) {
                        if let Some(panel) = &mut self.browser_bookmarks.panel {
                            panel.error = true;
                            panel.message(&error.to_string());
                        }
                    }
                }
            }
            UiAction::Choose(_) => {}
        }
        Ok(())
    }
}
