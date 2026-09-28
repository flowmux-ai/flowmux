// SPDX-License-Identifier: GPL-3.0-or-later
//! In-window workspace overview. Previews come only from owned WebView2 instances.
use super::*;
use std::{
    collections::{HashSet, VecDeque},
    rc::Rc,
};
use webview2_com::{
    CapturePreviewCompletedHandler,
    Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
};
use windows::{
    core::Interface,
    Win32::System::Com::{
        IStream, StructuredStorage::CreateStreamOnHGlobal, STATFLAG_NONAME, STATSTG,
        STREAM_SEEK_SET,
    },
};
use windows_sys::Win32::UI::{
    Controls::{SetScrollInfo, DRAWITEMSTRUCT, ODS_FOCUS, ODT_BUTTON},
    Input::KeyboardAndMouse::GetFocus,
};

const DEADLINE: Duration = Duration::from_secs(2);
const GENERATION_DEADLINE: Duration = Duration::from_secs(5);
const MAX_PREVIEWS: usize = 128;
const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;
#[repr(C)]
struct Startup {
    version: u32,
    callback: *const (),
    suppress_thread: i32,
    suppress_codecs: i32,
}
// Windows' built-in image decoder; no desktop capture, files, or additional runtime.
#[link(name = "gdiplus")]
unsafe extern "system" {
    fn GdiplusStartup(token: *mut usize, input: *const Startup, output: *mut ()) -> i32;
    fn GdiplusShutdown(token: usize);
    fn GdipCreateBitmapFromStream(stream: *mut std::ffi::c_void, image: *mut isize) -> i32;
    fn GdipGetImageWidth(image: isize, width: *mut u32) -> i32;
    fn GdipGetImageHeight(image: isize, height: *mut u32) -> i32;
    fn GdipGetImageThumbnail(
        image: isize,
        width: u32,
        height: u32,
        thumb: *mut isize,
        callback: *const (),
        data: *const (),
    ) -> i32;
    fn GdipDisposeImage(image: isize) -> i32;
    fn GdipCreateFromHDC(dc: HDC, graphics: *mut isize) -> i32;
    fn GdipDeleteGraphics(graphics: isize) -> i32;
    fn GdipDrawImageRectI(
        graphics: isize,
        image: isize,
        x: i32,
        y: i32,
        width: i32,
        height: i32,
    ) -> i32;
}
struct Image {
    handle: isize,
    width: u32,
    height: u32,
    hash: String,
}
impl Drop for Image {
    fn drop(&mut self) {
        unsafe {
            GdipDisposeImage(self.handle);
        }
    }
}
fn decode(bytes: &[u8]) -> anyhow::Result<Image> {
    use sha2::{Digest, Sha256};
    let (width, height) = crate::browser_capture::png_dimensions(bytes)?;
    anyhow::ensure!(
        u64::from(width) * u64::from(height) <= 16_000_000,
        "preview exceeds 16 megapixels"
    );
    unsafe {
        let stream = CreateStreamOnHGlobal(Default::default(), true)?;
        let mut written = 0;
        stream
            .Write(
                bytes.as_ptr().cast(),
                bytes.len() as u32,
                Some(&mut written),
            )
            .ok()?;
        anyhow::ensure!(written as usize == bytes.len(), "incomplete preview stream");
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut full = 0;
        anyhow::ensure!(
            GdipCreateBitmapFromStream(stream.as_raw(), &mut full) == 0,
            "cannot decode preview"
        );
        let mut w = 0;
        let mut h = 0;
        let valid = GdipGetImageWidth(full, &mut w) == 0
            && GdipGetImageHeight(full, &mut h) == 0
            && w > 0
            && h > 0;
        let mut thumb = 0;
        let scale = (320.0 / w.max(1) as f64)
            .min(200.0 / h.max(1) as f64)
            .min(1.0);
        let tw = (w as f64 * scale).round().max(1.0) as u32;
        let th = (h as f64 * scale).round().max(1.0) as u32;
        let result = valid
            && GdipGetImageThumbnail(full, tw, th, &mut thumb, std::ptr::null(), std::ptr::null())
                == 0;
        GdipDisposeImage(full);
        anyhow::ensure!(result, "cannot reduce preview");
        Ok(Image {
            handle: thumb,
            width: tw,
            height: th,
            hash: format!("{:x}", Sha256::digest(bytes)),
        })
    }
}
fn read_stream(stream: &IStream) -> anyhow::Result<Vec<u8>> {
    unsafe {
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME)?;
        anyhow::ensure!(
            stat.cbSize > 0 && stat.cbSize <= MAX_CAPTURE_BYTES as u64,
            "preview is empty or exceeds 16 MiB"
        );
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut bytes = vec![0; stat.cbSize as usize];
        let mut read = 0;
        stream
            .Read(
                bytes.as_mut_ptr().cast(),
                bytes.len() as u32,
                Some(&mut read),
            )
            .ok()?;
        anyhow::ensure!(read as usize == bytes.len(), "incomplete preview");
        Ok(bytes)
    }
}
#[derive(Clone)]
struct Preview {
    surface: SurfaceId,
    area: model::Rect,
    image: Option<Rc<Image>>,
    status: String,
}
#[derive(Clone)]
struct Paint {
    workspace: WorkspaceId,
    name: String,
    selected: bool,
    previews: Vec<Preview>,
}
thread_local! { static CARDS: RefCell<HashMap<isize,Paint>> = RefCell::new(HashMap::new()); }
pub(super) enum Signal {
    Dismiss,
    Select(WorkspaceId),
    Close(WorkspaceId),
    Scroll(i32),
    ScrollTo(i32),
    Navigate(usize),
    Captured(Uuid, Result<Vec<u8>, String>),
}
fn emit(signal: Signal) {
    post(Event::Overview(signal));
}
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_CLOSE => {
            emit(Signal::Dismiss);
            return 0;
        }
        WM_MOUSEWHEEL => {
            emit(Signal::Scroll(-((w >> 16) as i16 as i32) * 80 / 120));
            return 0;
        }
        WM_VSCROLL => {
            if matches!(w & 0xffff, 4 | 5) {
                let mut info = SCROLLINFO {
                    cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_TRACKPOS,
                    ..Default::default()
                };
                if GetScrollInfo(hwnd, SB_VERT, &mut info) != 0 {
                    emit(Signal::ScrollTo(info.nTrackPos));
                }
                return 0;
            }
            let delta = match w & 0xffff {
                0 => -48,
                1 => 48,
                2 => -240,
                3 => 240,
                6 => -100000,
                7 => 100000,
                _ => 0,
            };
            if delta != 0 {
                emit(Signal::Scroll(delta));
            }
            return 0;
        }
        WM_COMMAND if (w >> 16) as u32 == BN_CLICKED => {
            let card = CARDS.with(|cards| cards.borrow().get(&l).cloned());
            if let Some(card) = card {
                emit(Signal::Select(card.workspace));
            } else if w & 0xffff == 1 {
                emit(Signal::Dismiss);
            } else {
                let target = GetWindowLongPtrW(l as HWND, GWLP_USERDATA) as HWND;
                let card = CARDS.with(|cards| cards.borrow().get(&(target as isize)).cloned());
                if let Some(card) = card {
                    emit(Signal::Close(card.workspace));
                }
            }
            return 0;
        }
        WM_DRAWITEM if l != 0 && draw(&*(l as *const DRAWITEMSTRUCT)) => return 1,
        _ => {}
    }
    if let Some(result) = chrome::message(hwnd, message, w, l) {
        return result;
    }
    DefWindowProcW(hwnd, message, w, l)
}
unsafe fn text(dc: HDC, value: &str, mut area: RECT, flags: u32) {
    let raw: Vec<u16> = value.encode_utf16().collect();
    let value = chrome::caption_for_paint(&raw);
    DrawTextW(
        dc,
        value.as_ptr(),
        value.len() as i32,
        &mut area,
        flags | DT_NOPREFIX,
    );
}
unsafe fn draw(item: &DRAWITEMSTRUCT) -> bool {
    if item.CtlType != ODT_BUTTON {
        return false;
    }
    let card = CARDS.with(|cards| cards.borrow().get(&(item.hwndItem as isize)).cloned());
    let Some(card) = card else {
        let target = GetWindowLongPtrW(item.hwndItem, GWLP_USERDATA);
        if !CARDS.with(|cards| cards.borrow().contains_key(&target)) {
            return false;
        }
        let saved = SaveDC(item.hDC);
        if saved == 0 {
            return false;
        }
        let p = chrome::palette();
        SetDCBrushColor(item.hDC, p.surface);
        FillRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
        SetDCPenColor(item.hDC, p.foreground);
        SelectObject(item.hDC, GetStockObject(DC_PEN));
        let x = (item.rcItem.left + item.rcItem.right) / 2;
        let y = (item.rcItem.top + item.rcItem.bottom) / 2;
        let arm = (5 * GetDpiForWindow(item.hwndItem).max(96) / 96) as i32;
        MoveToEx(item.hDC, x - arm, y - arm, std::ptr::null_mut());
        LineTo(item.hDC, x + arm + 1, y + arm + 1);
        MoveToEx(item.hDC, x + arm, y - arm, std::ptr::null_mut());
        LineTo(item.hDC, x - arm - 1, y + arm + 1);
        RestoreDC(item.hDC, saved);
        return true;
    };
    let saved = SaveDC(item.hDC);
    if saved == 0 {
        return false;
    }
    let p = chrome::palette();
    let r = item.rcItem;
    SetDCBrushColor(item.hDC, if card.selected { p.selected } else { p.surface });
    FillRect(item.hDC, &r, GetStockObject(DC_BRUSH));
    SetDCPenColor(
        item.hDC,
        if card.selected || item.itemState & ODS_FOCUS != 0 {
            p.accent
        } else {
            p.border
        },
    );
    SelectObject(item.hDC, GetStockObject(DC_PEN));
    SelectObject(item.hDC, GetStockObject(NULL_BRUSH));
    Rectangle(item.hDC, r.left, r.top, r.right, r.bottom);
    SetBkMode(item.hDC, TRANSPARENT as i32);
    SetTextColor(item.hDC, p.foreground);
    SelectObject(
        item.hDC,
        SendMessageW(item.hwndItem, WM_GETFONT, 0, 0) as HGDIOBJ,
    );
    let dpi = GetDpiForWindow(item.hwndItem).max(96);
    let px = |n: i32| (n * dpi as i32 + 48) / 96;
    let body = RECT {
        left: r.left + px(8),
        top: r.top + px(30),
        right: r.right - px(8),
        bottom: r.bottom - px(36),
    };
    for preview in card.previews {
        let x = body.left + preview.area.x * (body.right - body.left) / 320;
        let y = body.top + preview.area.y * (body.bottom - body.top) / 200;
        let width = (preview.area.width * (body.right - body.left) / 320).max(1);
        let height = (preview.area.height * (body.bottom - body.top) / 200).max(1);
        let bounds = RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + height,
        };
        SetDCBrushColor(item.hDC, p.background);
        FillRect(item.hDC, &bounds, GetStockObject(DC_BRUSH));
        if let Some(image) = preview.image {
            let ratio =
                (width as f64 / image.width as f64).min(height as f64 / image.height as f64);
            let iw = (image.width as f64 * ratio) as i32;
            let ih = (image.height as f64 * ratio) as i32;
            let mut graphics = 0;
            if GdipCreateFromHDC(item.hDC, &mut graphics) == 0 {
                GdipDrawImageRectI(
                    graphics,
                    image.handle,
                    x + (width - iw) / 2,
                    y + (height - ih) / 2,
                    iw,
                    ih,
                );
                GdipDeleteGraphics(graphics);
            }
        } else {
            SetTextColor(item.hDC, p.muted);
            text(item.hDC, &preview.status, bounds, DT_CENTER | DT_WORDBREAK);
            SetTextColor(item.hDC, p.foreground);
        }
    }
    text(
        item.hDC,
        &card.name,
        RECT {
            left: r.left + px(10),
            top: r.bottom - px(30),
            right: r.right - px(10),
            bottom: r.bottom - px(5),
        },
        DT_SINGLELINE | DT_END_ELLIPSIS | DT_CENTER,
    );
    if card.selected {
        text(
            item.hDC,
            "Active",
            RECT {
                left: r.left + px(10),
                top: r.top + px(6),
                right: r.right - px(40),
                bottom: r.top + px(27),
            },
            DT_SINGLELINE,
        );
    }
    RestoreDC(item.hDC, saved);
    true
}
struct Card {
    workspace: WorkspaceId,
    button: HWND,
    close: HWND,
}
struct Panel {
    window: HWND,
    heading: HWND,
    viewport: HWND,
    reveal_focus: bool,
    dismiss: HWND,
    status: HWND,
    cards: Vec<Card>,
    saved_focus: HWND,
    saved_surface: SurfaceId,
    offset: i32,
    columns: usize,
    focused: usize,
}
impl Drop for Panel {
    fn drop(&mut self) {
        unsafe {
            for card in &self.cards {
                CARDS.with(|c| {
                    c.borrow_mut().remove(&(card.button as isize));
                });
            }
            DestroyWindow(self.window);
        }
    }
}
impl Panel {
    fn child(&self, class: &str, label: &str, id: usize) -> anyhow::Result<HWND> {
        unsafe {
            let button = class == "BUTTON";
            let hwnd = CreateWindowExW(
                0,
                wide(class).as_ptr(),
                wide(label).as_ptr(),
                WS_CHILD
                    | WS_VISIBLE
                    | if button {
                        WS_TABSTOP | BS_OWNERDRAW as u32
                    } else {
                        0
                    },
                0,
                0,
                1,
                1,
                self.window,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            anyhow::ensure!(!hwnd.is_null(), "cannot create overview control");
            if button {
                chrome::register_button(hwnd, chrome::Role::Button);
            } else {
                chrome::register_control(hwnd, chrome::ControlRole::Static);
            }
            Ok(hwnd)
        }
    }
    fn new(parent: HWND, surface: SurfaceId) -> anyhow::Result<Self> {
        unsafe {
            let class = wide("flowmux.windows.overview");
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
                "cannot register overview"
            );
            let hwnd = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Workspace overview").as_ptr(),
                WS_CHILD | WS_CLIPCHILDREN | WS_VSCROLL,
                0,
                0,
                1,
                1,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!hwnd.is_null(), "cannot create overview overlay");
            let mut panel = Self {
                window: hwnd,
                heading: std::ptr::null_mut(),
                viewport: std::ptr::null_mut(),
                reveal_focus: true,
                dismiss: std::ptr::null_mut(),
                status: std::ptr::null_mut(),
                cards: Vec::new(),
                saved_focus: GetFocus(),
                saved_surface: surface,
                offset: 0,
                columns: 1,
                focused: 0,
            };
            panel.viewport = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide("Workspace cards").as_ptr(),
                WS_CHILD | WS_VISIBLE | WS_CLIPCHILDREN,
                0,
                0,
                1,
                1,
                hwnd,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!panel.viewport.is_null(), "cannot create overview viewport");
            panel.heading = panel.child("STATIC", "Workspaces", 10)?;
            panel.dismiss = panel.child("BUTTON", "Close overview", 1)?;
            panel.status = panel.child("STATIC", "", 11)?;
            Ok(panel)
        }
    }
}
struct Pending {
    surface: SurfaceId,
    workspace: WorkspaceId,
    generation: u64,
    started: Instant,
    expired: bool,
}
#[derive(Default)]
pub(super) struct Controller {
    panel: Option<Panel>,
    generation: u64,
    started: Option<Instant>,
    pending: HashMap<Uuid, Pending>,
    queued: VecDeque<(WorkspaceId, SurfaceId)>,
    attempted: HashSet<SurfaceId>,
    images: HashMap<SurfaceId, Rc<Image>>,
    errors: HashMap<SurfaceId, String>,
    gdiplus: usize,
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.panel.take();
        self.images.clear();
        if self.gdiplus != 0 {
            unsafe {
                GdiplusShutdown(self.gdiplus);
            }
        }
    }
}
impl Controller {
    fn budget_expired(&self) -> bool {
        self.started
            .is_some_and(|started| started.elapsed() >= GENERATION_DEADLINE)
    }
    fn expire_generation(&mut self) {
        if !self.budget_expired() {
            return;
        }
        for (_, surface) in self.queued.drain(..) {
            self.errors.insert(
                surface,
                "Overview preview deadline elapsed; reopen to capture".into(),
            );
        }
        for pending in self
            .pending
            .values_mut()
            .filter(|p| p.generation == self.generation)
        {
            pending.expired = true;
            self.errors
                .insert(pending.surface, "Overview preview deadline elapsed".into());
        }
    }
    pub(super) fn is_open(&self) -> bool {
        self.panel.is_some()
    }
}
impl App {
    pub(super) fn overview_toggle(&mut self) -> anyhow::Result<()> {
        if self.overview.is_open() {
            return self.overview_dismiss(false);
        }
        if self.editor_barrier.is_some() || self.close_request.is_some() {
            return Ok(());
        }
        let saved_surface = self
            .current_workspace()
            .context("No active workspace for overview")?
            .active();
        if self.overview.gdiplus == 0 {
            let input = Startup {
                version: 1,
                callback: std::ptr::null(),
                suppress_thread: 0,
                suppress_codecs: 0,
            };
            unsafe {
                anyhow::ensure!(
                    GdiplusStartup(&mut self.overview.gdiplus, &input, std::ptr::null_mut()) == 0,
                    "cannot initialize overview images"
                );
            }
        }
        self.cancel_drag();
        self.overview.generation = self.overview.generation.wrapping_add(1);
        self.overview.images.clear();
        self.overview.errors.clear();
        self.overview.attempted.clear();
        self.overview.queued.clear();
        self.overview.panel = Some(Panel::new(self.window, saved_surface)?);
        self.overview.started = Some(Instant::now());
        self.overview_refresh()?;
        self.overview_layout()?;
        if !self.background_test {
            unsafe {
                let p = self.overview.panel.as_ref().unwrap();
                ShowWindow(p.window, SW_SHOWNOACTIVATE);
                SetFocus(p.cards.get(p.focused).map_or(p.dismiss, |c| c.button));
            }
        }
        Ok(())
    }
    fn overview_dismiss(&mut self, selected: bool) -> anyhow::Result<()> {
        let Some(panel) = self.overview.panel.take() else {
            return Ok(());
        };
        self.overview.generation = self.overview.generation.wrapping_add(1);
        self.overview.queued.clear();
        self.overview.images.clear();
        self.overview.errors.clear();
        self.overview.attempted.clear();
        self.overview.started = None;
        let focus = panel.saved_focus;
        let surface = panel.saved_surface;
        drop(panel);
        self.layout()?;
        if !self.background_test {
            unsafe {
                if !selected
                    && self.current_surface() == Some(surface)
                    && IsWindow(focus) != 0
                    && (focus == self.window || IsChild(self.window, focus) != 0)
                {
                    SetFocus(focus);
                } else {
                    self.focus_active()?;
                }
            }
        }
        Ok(())
    }
    pub(super) fn overview_event(&mut self, signal: Signal) -> anyhow::Result<()> {
        match signal {
            Signal::Dismiss => return self.overview_dismiss(false),
            Signal::Select(id) => {
                if !self.overview.is_open() {
                    return Ok(());
                }
                let index = self.workspace_index(id)?;
                self.select(self.workspaces[index].active())?;
                self.overview_dismiss(true)?;
                self.rebuild_without_focus()?;
            }
            Signal::Close(id) => {
                if !self.overview.is_open() {
                    return Ok(());
                }
                let result = self.workspace_command(WorkspaceOp::Close { workspace: id.0 }, None);
                if let Some(panel) = self.overview.panel.as_ref() {
                    let message = match result {
                        Ok(value)
                            if value.get("pending").and_then(Value::as_bool) == Some(true) =>
                        {
                            "Resolve unsaved editor changes to close this workspace".to_owned()
                        }
                        Ok(_) => String::new(),
                        Err(e) => format!("{e:#}"),
                    };
                    unsafe {
                        SetWindowTextW(panel.status, wide(&message).as_ptr());
                    }
                }
            }
            Signal::Scroll(delta) => {
                if let Some(panel) = self.overview.panel.as_mut() {
                    panel.offset = panel.offset.saturating_add(delta);
                }
            }
            Signal::ScrollTo(offset) => {
                if let Some(panel) = self.overview.panel.as_mut() {
                    panel.offset = offset;
                }
            }
            Signal::Navigate(index) => {
                if let Some(panel) = self.overview.panel.as_mut() {
                    panel.focused = index.min(panel.cards.len().saturating_sub(1));
                    panel.reveal_focus = true;
                }
            }
            Signal::Captured(request, result) => {
                let Some(pending) = self.overview.pending.remove(&request) else {
                    return Ok(());
                };
                let current = self.overview.is_open()
                    && pending.generation == self.overview.generation
                    && self
                        .locate(pending.surface)
                        .is_some_and(|(i, _, _)| self.workspaces[i].id == pending.workspace);
                let late = pending.expired
                    || pending.started.elapsed() >= DEADLINE
                    || self.overview.budget_expired();
                if current && late {
                    // A callback can arrive between deadline expiry and the next Tick.
                    // Record a terminal error even when no Tick marked this slot yet.
                    self.overview
                        .errors
                        .insert(pending.surface, "CapturePreview timed out".into());
                } else if current {
                    match result
                        .map_err(anyhow::Error::msg)
                        .and_then(|bytes| decode(&bytes))
                    {
                        Ok(image) => {
                            self.overview.images.insert(pending.surface, Rc::new(image));
                        }
                        Err(e) => {
                            self.overview
                                .errors
                                .insert(pending.surface, format!("{e:#}"));
                        }
                    }
                }
            }
        }
        self.overview_refresh()?;
        self.overview_layout()?;
        Ok(())
    }
    pub(super) fn overview_refresh(&mut self) -> anyhow::Result<()> {
        if !self.overview.is_open() {
            return Ok(());
        }
        let Some(active_workspace) = self.current_workspace().map(|workspace| workspace.id) else {
            return self.overview_dismiss(false);
        };
        self.overview.expire_generation();
        let expired = self.overview.budget_expired();
        let ids: Vec<_> = self.workspaces.iter().map(|w| w.id).collect();
        let panel = self.overview.panel.as_mut().unwrap();
        if panel.cards.iter().map(|c| c.workspace).collect::<Vec<_>>() != ids {
            for card in panel.cards.drain(..) {
                CARDS.with(|map| {
                    map.borrow_mut().remove(&(card.button as isize));
                });
                unsafe {
                    DestroyWindow(card.button);
                    DestroyWindow(card.close);
                }
            }
            for (index, workspace) in self.workspaces.iter().enumerate() {
                let button = panel.child(
                    "BUTTON",
                    &format!("Open workspace {}", workspace.name),
                    100 + index * 2,
                )?;
                let close = panel.child(
                    "BUTTON",
                    &format!("Close workspace {}", workspace.name),
                    101 + index * 2,
                )?;
                unsafe {
                    SetParent(button, panel.viewport);
                    SetParent(close, panel.viewport);
                    SetWindowLongPtrW(close, GWLP_USERDATA, button as isize);
                }
                panel.cards.push(Card {
                    workspace: workspace.id,
                    button,
                    close,
                });
            }
            panel.focused = self
                .active_workspace
                .min(panel.cards.len().saturating_sub(1));
            panel.reveal_focus = true;
        }
        for (workspace, card) in self.workspaces.iter().zip(&panel.cards) {
            let mut areas = Vec::new();
            model::layout(
                &workspace.root,
                model::Rect {
                    x: 0,
                    y: 0,
                    width: 320,
                    height: 200,
                },
                3,
                &mut areas,
            );
            let mut previews = Vec::new();
            for (pane, surface, _) in workspace.leaves() {
                let Some((_, area)) = areas.iter().find(|(id, _)| *id == pane) else {
                    continue;
                };
                if self.overview.attempted.len() < MAX_PREVIEWS
                    && self.overview.attempted.insert(surface)
                {
                    if expired {
                        self.overview.errors.insert(
                            surface,
                            "Overview preview deadline elapsed; reopen to capture".into(),
                        );
                    } else {
                        self.overview.queued.push_back((workspace.id, surface));
                    }
                }
                let status = if self.overview.images.contains_key(&surface) {
                    "captured"
                } else if self.overview.errors.contains_key(&surface) {
                    "Preview unavailable"
                } else if self.overview.attempted.contains(&surface) {
                    "Loading preview…"
                } else {
                    "Preview limit reached"
                };
                previews.push(Preview {
                    surface,
                    area: *area,
                    image: self.overview.images.get(&surface).cloned(),
                    status: status.into(),
                });
            }
            CARDS.with(|map| {
                map.borrow_mut().insert(
                    card.button as isize,
                    Paint {
                        workspace: workspace.id,
                        name: workspace.name.clone(),
                        selected: workspace.id == active_workspace,
                        previews,
                    },
                );
            });
            unsafe {
                SetWindowTextW(
                    card.button,
                    wide(format!("Open workspace {}", workspace.name)).as_ptr(),
                );
                SetWindowTextW(
                    card.close,
                    wide(format!("Close workspace {}", workspace.name)).as_ptr(),
                );
                InvalidateRect(card.button, std::ptr::null(), 0);
            }
        }
        unsafe {
            SetWindowTextW(
                panel.heading,
                wide(format!("{} workspaces", self.workspaces.len())).as_ptr(),
            );
        }
        self.overview_pump();
        Ok(())
    }
    fn overview_pump(&mut self) {
        self.overview.expire_generation();
        while self.overview.is_open()
            && !self.overview.budget_expired()
            && self.overview.pending.len() < 2
        {
            let Some((workspace, surface)) = self.overview.queued.pop_front() else {
                break;
            };
            if self.overview.errors.contains_key(&surface) {
                continue;
            }
            let request = Uuid::new_v4();
            let result = (|| -> anyhow::Result<()> {
                let view = if let Some(terminal) = self.surfaces.get(&surface) {
                    anyhow::ensure!(terminal.ready, "terminal not ready");
                    anyhow::ensure!(terminal.visible, "Preview requires an active terminal tab");
                    &terminal.view
                } else if let Some(browser) = self.browsers.get(&surface) {
                    anyhow::ensure!(browser.visible, "Preview requires an active browser tab");
                    anyhow::ensure!(
                        !browser.loading && browser.error.is_none(),
                        "browser not ready"
                    );
                    &browser.view
                } else if let Some(editor) = self.editors.get(&surface) {
                    anyhow::ensure!(editor.ready, "editor not ready");
                    anyhow::ensure!(editor.view.visible, "Preview requires an active editor tab");
                    &editor.view.view
                } else {
                    anyhow::bail!("surface no longer exists")
                };
                let mut bounds = windows::Win32::Foundation::RECT::default();
                unsafe {
                    view.controller().Bounds(&mut bounds)?;
                }
                let pixels =
                    i64::from(bounds.right - bounds.left) * i64::from(bounds.bottom - bounds.top);
                anyhow::ensure!(
                    pixels > 0 && pixels <= 16_000_000,
                    "preview viewport is empty or too large"
                );
                let stream = unsafe { CreateStreamOnHGlobal(Default::default(), true)? };
                let completed = stream.clone();
                let sender = self.sender.clone();
                let callback = CapturePreviewCompletedHandler::create(Box::new(move |status| {
                    let result = status
                        .map_err(anyhow::Error::from)
                        .and_then(|()| read_stream(&completed))
                        .map_err(|e| format!("{e:#}"));
                    sender.send(Event::Overview(Signal::Captured(request, result)));
                    Ok(())
                }));
                unsafe {
                    view.controller().CoreWebView2()?.CapturePreview(
                        COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                        &stream,
                        &callback,
                    )?;
                }
                Ok(())
            })();
            match result {
                Ok(()) => {
                    self.overview.pending.insert(
                        request,
                        Pending {
                            surface,
                            workspace,
                            generation: self.overview.generation,
                            started: Instant::now(),
                            expired: false,
                        },
                    );
                }
                Err(e) => {
                    self.overview.errors.insert(surface, format!("{e:#}"));
                }
            }
        }
    }
    pub(super) fn overview_tick(&mut self) -> anyhow::Result<()> {
        self.overview.expire_generation();
        let mut changed = self.overview.is_open();
        for pending in self.overview.pending.values_mut() {
            if !pending.expired && pending.started.elapsed() > DEADLINE {
                pending.expired = true;
                if pending.generation == self.overview.generation {
                    self.overview
                        .errors
                        .insert(pending.surface, "CapturePreview timed out".into());
                    changed = true;
                }
            }
        }
        if self.overview.pending.len() == 2 && self.overview.pending.values().all(|p| p.expired) {
            for (_, surface) in self.overview.queued.drain(..) {
                self.overview.errors.insert(
                    surface,
                    "Preview capture slots are awaiting expired callbacks".into(),
                );
                changed = true;
            }
        }
        // Timed-out COM slots stay occupied until callbacks arrive: no unbounded retries.
        if changed {
            self.overview_refresh()?;
        }
        Ok(())
    }
    pub(super) fn overview_layout(&mut self) -> anyhow::Result<()> {
        let Some(panel) = self.overview.panel.as_mut() else {
            return Ok(());
        };
        unsafe {
            let mut client = RECT::default();
            checked(GetClientRect(self.window, &mut client))?;
            let dpi = GetDpiForWindow(self.window).max(96);
            let px = |n: i32| (n * dpi as i32 + 48) / 96;
            SetWindowPos(
                panel.window,
                HWND_TOP,
                0,
                0,
                client.right,
                client.bottom,
                SWP_NOACTIVATE,
            );
            let mut area = RECT::default();
            GetClientRect(panel.window, &mut area);
            let width = area.right;
            let height = area.bottom;
            MoveWindow(
                panel.heading,
                px(24),
                px(16),
                (width - px(200)).max(1),
                px(28),
                1,
            );
            MoveWindow(
                panel.dismiss,
                (width - px(168)).max(0),
                px(12),
                px(144).min(width),
                px(32),
                1,
            );
            MoveWindow(
                panel.status,
                px(24),
                (height - px(28)).max(0),
                (width - px(48)).max(1),
                px(24),
                1,
            );
            panel.columns = ((width - px(24)) / (px(280) + px(16))).clamp(1, 5) as usize;
            let cardw = ((width - px(24)) / panel.columns as i32 - px(16)).max(1);
            let cardh = (cardw * 200 / 320 + px(68)).max(px(100));
            let step = cardh + px(16);
            let rows = panel.cards.len().div_ceil(panel.columns) as i32;
            let viewport = (height - px(84)).max(1);
            MoveWindow(panel.viewport, 0, px(52), width, viewport, 1);
            if panel.reveal_focus {
                let top = (panel.focused / panel.columns) as i32 * step;
                if top < panel.offset {
                    panel.offset = top;
                }
                if top + cardh > panel.offset + viewport {
                    panel.offset = (top + cardh - viewport).max(0);
                }
            }
            let maximum = (rows * step - viewport).max(0);
            panel.offset = panel.offset.clamp(0, maximum);
            let scroll = SCROLLINFO {
                cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
                nMin: 0,
                nMax: rows * step,
                nPage: viewport as u32,
                nPos: panel.offset,
                ..Default::default()
            };
            SetScrollInfo(panel.window, SB_VERT, &scroll, 1);
            for (i, card) in panel.cards.iter().enumerate() {
                let x = px(16) + (i % panel.columns) as i32 * (cardw + px(16));
                let y = px(8) + (i / panel.columns) as i32 * step - panel.offset;
                let visible = y < viewport && y + cardh > 0;
                MoveWindow(card.button, x, y, cardw, cardh, 1);
                MoveWindow(card.close, x + cardw - px(34), y + px(3), px(28), px(26), 1);
                ShowWindow(card.button, if visible { SW_SHOWNA } else { SW_HIDE });
                ShowWindow(card.close, if visible { SW_SHOWNA } else { SW_HIDE });
            }
            if panel.reveal_focus && !self.background_test {
                if let Some(card) = panel.cards.get(panel.focused) {
                    SetFocus(card.button);
                }
            }
            panel.reveal_focus = false;
            InvalidateRect(panel.window, std::ptr::null(), 0);
        }
        Ok(())
    }
    pub(super) fn overview_handle_message(&self, message: &MSG) -> bool {
        let Some(panel) = self.overview.panel.as_ref() else {
            return false;
        };
        unsafe {
            if message.hwnd != panel.window && IsChild(panel.window, message.hwnd) == 0 {
                return false;
            }
            if message.message == WM_KEYDOWN {
                let target = if message.hwnd == panel.window || message.hwnd == panel.viewport {
                    GetFocus()
                } else {
                    message.hwnd
                };
                let actual = panel
                    .cards
                    .iter()
                    .position(|card| card.button == target || card.close == target);
                let index = actual.unwrap_or(panel.focused);
                let navigate = |delta: i32| {
                    Signal::Navigate(
                        (index as i32 + delta).clamp(0, panel.cards.len().saturating_sub(1) as i32)
                            as usize,
                    )
                };
                let action = match message.wParam {
                    0x1b => Some(Signal::Dismiss),
                    0x25 => Some(navigate(-1)),
                    0x27 => Some(navigate(1)),
                    0x26 => Some(navigate(-(panel.columns as i32))),
                    0x28 => Some(navigate(panel.columns as i32)),
                    0x0d if target == panel.dismiss => Some(Signal::Dismiss),
                    0x0d => actual.map(|index| {
                        let card = &panel.cards[index];
                        if target == card.close {
                            Signal::Close(card.workspace)
                        } else {
                            Signal::Select(card.workspace)
                        }
                    }),
                    _ => None,
                };
                if let Some(action) = action {
                    emit(action);
                    return true;
                }
            }
            IsDialogMessageW(panel.window, message) != 0
        }
    }
    #[cfg(debug_assertions)]
    pub(super) fn overview_capture_window(&self) -> Option<HWND> {
        self.overview.panel.as_ref().map(|panel| panel.window)
    }
    pub(super) fn overview_status(&self) -> Value {
        let Some(panel) = self.overview.panel.as_ref() else {
            return json!({"open":false,"pending_captures":self.overview.pending.len()});
        };
        let cards=panel.cards.iter().map(|card|{let paint=CARDS.with(|c|c.borrow().get(&(card.button as isize)).cloned());json!({"workspace":card.workspace,"button":card.button as usize,"close":card.close as usize,"name":paint.as_ref().map(|p|&p.name),"active":paint.as_ref().is_some_and(|p|p.selected),"previews":paint.map(|p|p.previews.into_iter().map(|preview|json!({"surface":preview.surface,"rect":preview.area,"status":preview.status,"error":self.overview.errors.get(&preview.surface),"width":preview.image.as_ref().map(|i|i.width),"height":preview.image.as_ref().map(|i|i.height),"source_png_sha256":preview.image.as_ref().map(|i|&i.hash)})).collect::<Vec<_>>())})}).collect::<Vec<_>>();
        json!({"open":true,"window":panel.window as usize,"viewport":panel.viewport as usize,"parent":self.window as usize,"dismiss":panel.dismiss as usize,"status":panel.status as usize,"generation":self.overview.generation,"capture_budget_ms":GENERATION_DEADLINE.as_millis(),"capture_elapsed_ms":self.overview.started.map(|start|start.elapsed().as_millis()),"capture_budget_expired":self.overview.budget_expired(),"pending_captures":self.overview.pending.len(),"queued_captures":self.overview.queued.len(),"offset":panel.offset,"columns":panel.columns,"focused_index":panel.focused,"cards":cards,"preview_source":"owned_webview2_capture_preview"})
    }
}
