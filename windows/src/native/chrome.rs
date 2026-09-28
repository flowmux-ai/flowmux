// SPDX-License-Identifier: GPL-3.0-or-later
//! Native shell colors and typography. BUTTON/EDIT/LISTBOX remain native HWNDs;
//! painting does not replace keyboard, accessibility, or IME handling.

use super::*;
use crate::settings::Theme;
use windows_sys::Win32::UI::{
    Controls::{
        DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_HOTLIGHT, ODS_NOACCEL, ODS_NOFOCUSRECT,
        ODS_SELECTED, ODT_BUTTON, WM_MOUSELEAVE,
    },
    Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT},
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};

const SUBCLASS: usize = 0x464d_4348;

#[derive(Clone, Copy, Debug)]
pub(super) enum Role {
    Button,
    Workspace { selected: bool },
    Tab { selected: bool },
    Tool,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Shared entrypoints also serve separately integrated child panels.
pub(super) enum ControlRole {
    Static,
    Caption,
    Edit,
    Listbox,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Palette {
    pub background: COLORREF,
    pub surface: COLORREF,
    pub foreground: COLORREF,
    pub muted: COLORREF,
    pub border: COLORREF,
    pub hover: COLORREF,
    pub selected: COLORREF,
    pub accent: COLORREF,
    pub high_contrast: bool,
}

const fn rgb(red: u32, green: u32, blue: u32) -> COLORREF {
    red | (green << 8) | (blue << 16)
}

impl Palette {
    fn new(theme: Theme, contrast: bool) -> Self {
        if contrast {
            return unsafe {
                Self {
                    background: GetSysColor(COLOR_WINDOW),
                    surface: GetSysColor(COLOR_WINDOW),
                    foreground: GetSysColor(COLOR_WINDOWTEXT),
                    muted: GetSysColor(COLOR_GRAYTEXT),
                    border: GetSysColor(COLOR_WINDOWTEXT),
                    hover: GetSysColor(COLOR_HIGHLIGHT),
                    selected: GetSysColor(COLOR_HIGHLIGHT),
                    accent: GetSysColor(COLOR_HIGHLIGHT),
                    high_contrast: true,
                }
            };
        }
        match theme {
            Theme::Dark => Self {
                background: rgb(36, 39, 46),
                surface: rgb(40, 44, 52),
                foreground: rgb(242, 243, 245),
                muted: rgb(171, 177, 188),
                border: rgb(69, 74, 85),
                hover: rgb(55, 60, 70),
                selected: rgb(49, 55, 65),
                accent: rgb(120, 174, 237),
                high_contrast: false,
            },
            Theme::Light => Self {
                background: rgb(242, 241, 240),
                surface: rgb(255, 255, 255),
                foreground: rgb(40, 40, 43),
                muted: rgb(95, 98, 105),
                border: rgb(209, 209, 211),
                hover: rgb(226, 227, 230),
                selected: rgb(218, 230, 245),
                accent: rgb(32, 102, 186),
                high_contrast: false,
            },
        }
    }
}

#[repr(C)]
struct HighContrast {
    size: u32,
    flags: u32,
    scheme: *mut u16,
}
fn high_contrast() -> bool {
    let mut value = HighContrast {
        size: std::mem::size_of::<HighContrast>() as u32,
        flags: 0,
        scheme: std::ptr::null_mut(),
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETHIGHCONTRAST,
            value.size,
            (&mut value as *mut HighContrast).cast(),
            0,
        ) != 0
            && value.flags & 1 != 0
    }
}

#[link(name = "dwmapi")]
unsafe extern "system" {
    // Exact windows-sys 0.61.2 Graphics/Dwm ABI; keeping the narrow import avoids
    // enabling another crate feature merely for this optional window attribute.
    fn DwmSetWindowAttribute(
        window: HWND,
        attribute: u32,
        value: *const std::ffi::c_void,
        bytes: u32,
    ) -> i32;
}

pub(super) fn window_theme(window: HWND, theme: Theme) {
    let dark: i32 = i32::from(theme == Theme::Dark && !high_contrast());
    // DWMWA_USE_IMMERSIVE_DARK_MODE = 20 takes a 32-bit BOOL. Documented support
    // starts with Windows 11 build 22000; an unsupported attribute leaves the
    // normal system title bar intact. Never replace the native non-client frame.
    // https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute
    unsafe {
        let _ = DwmSetWindowAttribute(
            window,
            20,
            (&dark as *const i32).cast(),
            std::mem::size_of::<i32>() as u32,
        );
    }
}

struct Resources {
    body: HFONT,
    caption: HFONT,
    background: HBRUSH,
    surface: HBRUSH,
    owns_body: bool,
    owns_caption: bool,
    owns_background: bool,
    owns_surface: bool,
}
impl Resources {
    fn new(palette: Palette, dpi: u32) -> Self {
        fn font(points: i32, dpi: u32) -> (HFONT, bool) {
            let font = unsafe {
                CreateFontW(
                    -((points * dpi as i32 + 36) / 72),
                    0,
                    0,
                    0,
                    FW_NORMAL as i32,
                    0,
                    0,
                    0,
                    DEFAULT_CHARSET as u32,
                    OUT_DEFAULT_PRECIS as u32,
                    CLIP_DEFAULT_PRECIS as u32,
                    CLEARTYPE_QUALITY as u32,
                    DEFAULT_PITCH as u32,
                    wide("Segoe UI").as_ptr(),
                )
            };
            if font.is_null() {
                (unsafe { GetStockObject(DEFAULT_GUI_FONT) }, false)
            } else {
                (font, true)
            }
        }
        fn brush(color: COLORREF) -> (HBRUSH, bool) {
            let brush = unsafe { CreateSolidBrush(color) };
            if brush.is_null() {
                (unsafe { GetSysColorBrush(COLOR_WINDOW) }, false)
            } else {
                (brush, true)
            }
        }
        let (body, owns_body) = font(11, dpi);
        let (caption, owns_caption) = font(9, dpi);
        let (background, owns_background) = brush(palette.background);
        let (surface, owns_surface) = brush(palette.surface);
        Self {
            body,
            caption,
            background,
            surface,
            owns_body,
            owns_caption,
            owns_background,
            owns_surface,
        }
    }
}
impl Drop for Resources {
    fn drop(&mut self) {
        unsafe {
            for (object, owned) in [
                (self.body, self.owns_body),
                (self.caption, self.owns_caption),
                (self.background, self.owns_background),
                (self.surface, self.owns_surface),
            ] {
                if owned {
                    DeleteObject(object);
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
struct Entry {
    button: Option<Role>,
    control: ControlRole,
    hot: bool,
}
struct State {
    theme: Theme,
    dpi: u32,
    palette: Palette,
    resources: Resources,
    controls: HashMap<isize, Entry>,
}
impl State {
    fn new() -> Self {
        let palette = Palette::new(Theme::Dark, high_contrast());
        Self {
            theme: Theme::Dark,
            dpi: 96,
            palette,
            resources: Resources::new(palette, 96),
            controls: HashMap::new(),
        }
    }
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::new()); }

pub(super) fn configure(theme: Theme, dpi: u32) {
    let dpi = dpi.clamp(48, 768);
    let contrast = high_contrast();
    let palette = Palette::new(theme, contrast);
    let swap = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if state.theme == theme && state.dpi == dpi && state.palette == palette {
            return None;
        }
        let resources = Resources::new(palette, dpi);
        state.theme = theme;
        state.dpi = dpi;
        state.palette = palette;
        let controls: Vec<_> = state
            .controls
            .iter()
            .map(|(hwnd, entry)| {
                (
                    *hwnd,
                    if matches!(entry.control, ControlRole::Caption) {
                        resources.caption
                    } else {
                        resources.body
                    },
                )
            })
            .collect();
        let old = std::mem::replace(&mut state.resources, resources);
        Some((controls, old))
    });
    if let Some((controls, old)) = swap {
        // Assign every live control its replacement font before deleting the old
        // fonts; release the RefCell borrow before synchronous window callbacks.
        for (hwnd, font) in controls {
            unsafe {
                if IsWindow(hwnd as HWND) != 0 {
                    SendMessageW(hwnd as HWND, WM_SETFONT, font as WPARAM, 0);
                    InvalidateRect(hwnd as HWND, std::ptr::null(), 1);
                }
            }
        }
        drop(old);
    }
}

pub(super) fn register_button(window: HWND, role: Role) {
    register(window, Some(role), ControlRole::Static);
}
pub(super) fn register_control(window: HWND, role: ControlRole) {
    register(window, None, role);
}
fn register(window: HWND, button: Option<Role>, control: ControlRole) {
    let font = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        state.controls.insert(
            window as isize,
            Entry {
                button,
                control,
                hot: false,
            },
        );
        if matches!(control, ControlRole::Caption) {
            state.resources.caption
        } else {
            state.resources.body
        }
    });
    unsafe {
        if SetWindowSubclass(window, Some(control_proc), SUBCLASS, 0) == 0 {
            STATE.with(|slot| {
                slot.borrow_mut().controls.remove(&(window as isize));
            });
            return;
        }
        SendMessageW(window, WM_SETFONT, font as WPARAM, 0);
    }
}

pub(super) fn unregister(window: HWND) {
    STATE.with(|slot| {
        slot.borrow_mut().controls.remove(&(window as isize));
    });
    unsafe {
        if IsWindow(window) != 0 {
            RemoveWindowSubclass(window, Some(control_proc), SUBCLASS);
            SendMessageW(
                window,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as WPARAM,
                0,
            );
        }
    }
}

#[allow(dead_code)]
pub(super) fn set_role(window: HWND, role: Role) {
    STATE.with(|slot| {
        if let Some(entry) = slot.borrow_mut().controls.get_mut(&(window as isize)) {
            entry.button = Some(role);
        }
    });
    unsafe {
        InvalidateRect(window, std::ptr::null(), 0);
    }
}

pub(super) fn shutdown() {
    let controls: Vec<_> = STATE.with(|slot| slot.borrow().controls.keys().copied().collect());
    for window in controls {
        unregister(window as HWND);
    }
    // All controls now hold a stock font. Retained state resources remain valid
    // until the UI thread exits; no registered HWND references them at teardown.
}

unsafe extern "system" fn control_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    match message {
        WM_MOUSEMOVE | WM_MOUSELEAVE => {
            let hot = message == WM_MOUSEMOVE;
            let changed = STATE.with(|slot| {
                let mut state = slot.borrow_mut();
                state
                    .controls
                    .get_mut(&(window as isize))
                    .is_some_and(|entry| {
                        if entry.button.is_none() || entry.hot == hot {
                            return false;
                        }
                        entry.hot = hot;
                        true
                    })
            });
            if changed {
                if hot {
                    let mut event = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: window,
                        dwHoverTime: 0,
                    };
                    TrackMouseEvent(&mut event);
                }
                InvalidateRect(window, std::ptr::null(), 0);
            }
        }
        WM_NCDESTROY => {
            STATE.with(|slot| {
                slot.borrow_mut().controls.remove(&(window as isize));
            });
            RemoveWindowSubclass(window, Some(control_proc), SUBCLASS);
        }
        _ => {}
    }
    DefSubclassProc(window, message, wparam, lparam)
}

fn fill(dc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        SetDCBrushColor(dc, color);
        FillRect(dc, rect, GetStockObject(DC_BRUSH));
    }
}

fn draw_button(item: &DRAWITEMSTRUCT) -> bool {
    let paint = STATE.with(|slot| {
        let state = slot.borrow();
        state
            .controls
            .get(&(item.hwndItem as isize))
            .and_then(|entry| {
                entry.button.map(|role| {
                    (
                        role,
                        entry.hot,
                        state.palette,
                        state.resources.body,
                        state.dpi,
                    )
                })
            })
    });
    let Some((role, hot, palette, font, dpi)) = paint else {
        return false;
    };
    let selected = matches!(
        role,
        Role::Workspace { selected: true } | Role::Tab { selected: true }
    );
    let pressed = item.itemState & ODS_SELECTED != 0;
    let hot = hot || item.itemState & ODS_HOTLIGHT != 0;
    let disabled = item.itemState & ODS_DISABLED != 0;
    let highlighted = selected || pressed || hot;
    let color = if pressed || selected {
        palette.selected
    } else if hot {
        palette.hover
    } else if matches!(role, Role::Tab { .. }) {
        palette.surface
    } else {
        palette.background
    };
    let text = if disabled {
        palette.muted
    } else if palette.high_contrast && highlighted {
        unsafe { GetSysColor(COLOR_HIGHLIGHTTEXT) }
    } else {
        palette.foreground
    };
    let pixel = |dip: i32| ((dip * dpi as i32 + 48) / 96).max(1);
    unsafe {
        let saved = SaveDC(item.hDC);
        if saved == 0 {
            return false;
        }
        fill(item.hDC, &item.rcItem, color);
        if selected {
            let stripe = if matches!(role, Role::Workspace { .. }) {
                RECT {
                    right: item.rcItem.left + pixel(4),
                    ..item.rcItem
                }
            } else {
                RECT {
                    top: item.rcItem.bottom - pixel(2),
                    ..item.rcItem
                }
            };
            fill(item.hDC, &stripe, palette.accent);
        }
        if palette.high_contrast {
            SetDCBrushColor(item.hDC, palette.border);
            FrameRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
        }
        SetTextColor(item.hDC, text);
        SetBkMode(item.hDC, TRANSPARENT as i32);
        SelectObject(item.hDC, font);
        let mut label = vec![0u16; 2048];
        let length = GetWindowTextW(item.hwndItem, label.as_mut_ptr(), label.len() as i32);
        let label_text = String::from_utf16_lossy(&label[..length.max(0) as usize]);
        let symbol = matches!(role, Role::Tool)
            && matches!(
                label_text.as_str(),
                "Close tab"
                    | "Pane actions"
                    | "+"
                    | "New tab"
                    | "Newtab"
                    | "New workspace"
                    | "Add workspace"
            );
        if symbol {
            let cx = (item.rcItem.left + item.rcItem.right) / 2;
            let cy = (item.rcItem.top + item.rcItem.bottom) / 2;
            let radius =
                pixel(4).min(((item.rcItem.right - item.rcItem.left) / 2 - pixel(4)).max(1));
            SelectObject(item.hDC, GetStockObject(DC_PEN));
            SetDCPenColor(item.hDC, text);
            match label_text.as_str() {
                "Close tab" => {
                    MoveToEx(item.hDC, cx - radius, cy - radius, std::ptr::null_mut());
                    LineTo(item.hDC, cx + radius + 1, cy + radius + 1);
                    MoveToEx(item.hDC, cx - radius, cy + radius, std::ptr::null_mut());
                    LineTo(item.hDC, cx + radius + 1, cy - radius - 1);
                }
                "Pane actions" => {
                    let size = pixel(2);
                    for offset in [-radius, 0, radius] {
                        fill(
                            item.hDC,
                            &RECT {
                                left: cx + offset - size / 2,
                                top: cy - size / 2,
                                right: cx + offset - size / 2 + size,
                                bottom: cy - size / 2 + size,
                            },
                            text,
                        );
                    }
                }
                _ => {
                    MoveToEx(item.hDC, cx - radius, cy, std::ptr::null_mut());
                    LineTo(item.hDC, cx + radius + 1, cy);
                    MoveToEx(item.hDC, cx, cy - radius, std::ptr::null_mut());
                    LineTo(item.hDC, cx, cy + radius + 1);
                }
            }
        }
        let inset = pixel(if matches!(role, Role::Tool) { 4 } else { 10 });
        let mut rect = RECT {
            left: item.rcItem.left + inset,
            right: item.rcItem.right - inset,
            ..item.rcItem
        };
        let align = if matches!(role, Role::Workspace { .. } | Role::Tab { .. }) {
            DT_LEFT
        } else {
            DT_CENTER
        };
        let accelerator = if item.itemState & ODS_NOACCEL != 0 {
            DT_HIDEPREFIX
        } else {
            0
        };
        if !symbol && rect.right > rect.left {
            DrawTextW(
                item.hDC,
                label.as_ptr(),
                length,
                &mut rect,
                align | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | accelerator,
            );
        }
        if item.itemState & ODS_FOCUS != 0 && item.itemState & ODS_NOFOCUSRECT == 0 {
            let inset = pixel(3);
            let focus = RECT {
                left: item.rcItem.left + inset,
                top: item.rcItem.top + inset,
                right: item.rcItem.right - inset,
                bottom: item.rcItem.bottom - inset,
            };
            DrawFocusRect(item.hDC, &focus);
        }
        RestoreDC(item.hDC, saved);
    }
    true
}

pub(super) fn message(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    match message {
        WM_DRAWITEM if lparam != 0 => {
            let item = unsafe { &*(lparam as *const DRAWITEMSTRUCT) };
            (item.CtlType == ODT_BUTTON && draw_button(item)).then_some(1)
        }
        WM_ERASEBKGND | WM_PRINTCLIENT => {
            let brush = STATE.with(|slot| slot.borrow().resources.background);
            let mut rect = RECT::default();
            unsafe {
                GetClientRect(window, &mut rect);
                FillRect(wparam as HDC, &rect, brush);
            }
            Some(1)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX | WM_CTLCOLORBTN => {
            let (palette, brush, role) = STATE.with(|slot| {
                let state = slot.borrow();
                let role = state
                    .controls
                    .get(&lparam)
                    .map(|entry| entry.control)
                    .unwrap_or(ControlRole::Static);
                let surface = matches!(role, ControlRole::Edit | ControlRole::Listbox)
                    || matches!(message, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX);
                (
                    state.palette,
                    if surface {
                        state.resources.surface
                    } else {
                        state.resources.background
                    },
                    role,
                )
            });
            let surface = matches!(role, ControlRole::Edit | ControlRole::Listbox)
                || matches!(message, WM_CTLCOLOREDIT | WM_CTLCOLORLISTBOX);
            unsafe {
                SetTextColor(
                    wparam as HDC,
                    if matches!(role, ControlRole::Caption) {
                        palette.muted
                    } else {
                        palette.foreground
                    },
                );
                SetBkColor(
                    wparam as HDC,
                    if surface {
                        palette.surface
                    } else {
                        palette.background
                    },
                );
            }
            Some(brush as LRESULT)
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            let (theme, dpi) = STATE.with(|slot| {
                let state = slot.borrow();
                (state.theme, state.dpi)
            });
            configure(theme, dpi);
            let root = unsafe { GetAncestor(window, GA_ROOT) };
            if !root.is_null() {
                window_theme(root, theme);
            }
            unsafe {
                InvalidateRect(window, std::ptr::null(), 1);
            }
            None
        }
        _ => None,
    }
}

/// Render the actual registered native chrome on its owning UI thread. This
/// debug-only diagnostic invokes the production button painter and native static
/// control print handling; WebView contents are outside this artifact's scope.
#[cfg(debug_assertions)]
pub(super) fn capture(window: HWND, path: &std::path::Path) -> anyhow::Result<Value> {
    use std::io::Write;
    use windows_sys::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled;

    struct BitmapDc {
        dc: HDC,
        bitmap: HBITMAP,
        previous: HGDIOBJ,
    }
    impl Drop for BitmapDc {
        fn drop(&mut self) {
            unsafe {
                if !self.previous.is_null() {
                    SelectObject(self.dc, self.previous);
                }
                if !self.bitmap.is_null() {
                    DeleteObject(self.bitmap);
                }
                if !self.dc.is_null() {
                    DeleteDC(self.dc);
                }
            }
        }
    }
    struct SavedDc {
        dc: HDC,
        token: i32,
    }
    impl Drop for SavedDc {
        fn drop(&mut self) {
            unsafe {
                RestoreDC(self.dc, self.token);
            }
        }
    }

    let mut owner = 0;
    let thread = unsafe { GetWindowThreadProcessId(window, &mut owner) };
    anyhow::ensure!(
        owner == unsafe { GetCurrentProcessId() } && thread == unsafe { GetCurrentThreadId() },
        "native capture must run on the exact owning UI thread"
    );
    anyhow::ensure!(
        unsafe { IsWindowVisible(window) } == 0
            && unsafe { GetAncestor(window, GA_ROOT) } == window,
        "native capture requires an owned hidden top-level window"
    );
    let mut client = RECT::default();
    checked(unsafe { GetClientRect(window, &mut client) })?;
    let (width, height) = (client.right - client.left, client.bottom - client.top);
    anyhow::ensure!(
        (1..=4096).contains(&width) && (1..=4096).contains(&height),
        "native capture dimensions exceed 4096 pixels"
    );
    let bytes = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .context("native bitmap size overflow")?;
    let mut surface = BitmapDc {
        dc: unsafe { CreateCompatibleDC(std::ptr::null_mut()) },
        bitmap: std::ptr::null_mut(),
        previous: std::ptr::null_mut(),
    };
    checked((!surface.dc.is_null()) as i32)?;
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: bytes as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    surface.bitmap = unsafe {
        CreateDIBSection(
            surface.dc,
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        )
    };
    checked((!surface.bitmap.is_null() && !bits.is_null()) as i32)?;
    surface.previous = unsafe { SelectObject(surface.dc, surface.bitmap) };
    checked((!surface.previous.is_null() && surface.previous as isize != -1) as i32)?;
    // Sentinel proves an actual paint took place; all GDI work uses this process's
    // own DC. Rust does not borrow the DIB storage while GDI is writing into it.
    unsafe {
        for pixel in std::slice::from_raw_parts_mut(bits.cast::<u8>(), bytes).chunks_exact_mut(4) {
            pixel.copy_from_slice(&[255, 0, 255, 255]);
        }
    }
    anyhow::ensure!(
        message(window, WM_ERASEBKGND, surface.dc as WPARAM, 0) == Some(1),
        "native background painter was unavailable"
    );
    let controls: Vec<_> = STATE.with(|slot| {
        slot.borrow()
            .controls
            .iter()
            .map(|(hwnd, entry)| (*hwnd as HWND, *entry))
            .collect()
    });
    let mut rendered = Vec::new();
    for (child, entry) in controls {
        if unsafe { GetParent(child) } != window
            || unsafe { GetWindowLongPtrW(child, GWL_STYLE) } & WS_VISIBLE as isize == 0
        {
            continue;
        }
        if entry.button.is_none()
            && !matches!(entry.control, ControlRole::Static | ControlRole::Caption)
        {
            continue;
        }
        let mut rect = RECT::default();
        checked(unsafe { GetWindowRect(child, &mut rect) })?;
        unsafe {
            MapWindowPoints(
                std::ptr::null_mut(),
                window,
                (&mut rect as *mut RECT).cast(),
                2,
            );
        }
        let (child_width, child_height) = (rect.right - rect.left, rect.bottom - rect.top);
        if child_width <= 0 || child_height <= 0 {
            continue;
        }
        let token = unsafe { SaveDC(surface.dc) };
        checked((token != 0) as i32)?;
        let _restore = SavedDc {
            dc: surface.dc,
            token,
        };
        checked(unsafe {
            SetViewportOrgEx(surface.dc, rect.left, rect.top, std::ptr::null_mut())
        })?;
        unsafe {
            IntersectClipRect(surface.dc, 0, 0, child_width, child_height);
        }
        if entry.button.is_some() {
            let native_state = unsafe { SendMessageW(child, BM_GETSTATE, 0, 0) } as u32;
            let mut state = 0;
            if unsafe { IsWindowEnabled(child) } == 0 {
                state |= ODS_DISABLED;
            }
            if native_state & 4 != 0 {
                state |= ODS_SELECTED;
            } // BST_PUSHED
            if native_state & 8 != 0 {
                state |= ODS_FOCUS;
            } // BST_FOCUS
            let item = DRAWITEMSTRUCT {
                CtlType: ODT_BUTTON,
                CtlID: unsafe { GetDlgCtrlID(child) } as u32,
                itemAction: 1,
                itemState: state,
                hwndItem: child,
                hDC: surface.dc,
                rcItem: RECT {
                    left: 0,
                    top: 0,
                    right: child_width,
                    bottom: child_height,
                },
                ..Default::default()
            };
            anyhow::ensure!(draw_button(&item), "registered native button did not paint");
        } else {
            unsafe {
                SendMessageW(
                    child,
                    WM_PRINTCLIENT,
                    surface.dc as WPARAM,
                    PRF_CLIENT as LPARAM,
                );
            }
        }
        rendered.push(
            json!({"handle":child as isize,"kind":if entry.button.is_some(){"button"}else{"static"},
            "x":rect.left,"y":rect.top,"width":child_width,"height":child_height}),
        );
    }
    checked(unsafe { GdiFlush() })?;
    let data = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), bytes) };
    for pixel in data.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    let sample =
        (((height - 1).min(100) as usize * width as usize) + (width - 1).min(1) as usize) * 4;
    let background = format!(
        "#{:02x}{:02x}{:02x}",
        data[sample + 2],
        data[sample + 1],
        data[sample]
    );
    anyhow::ensure!(
        background != "#ff00ff",
        "native background sentinel was not painted"
    );
    let mut header = Vec::with_capacity(54);
    header.extend_from_slice(b"BM");
    header.extend_from_slice(&(54u32 + bytes as u32).to_le_bytes());
    header.extend_from_slice(&[0u8; 4]);
    header.extend_from_slice(&54u32.to_le_bytes());
    header.extend_from_slice(&40u32.to_le_bytes());
    header.extend_from_slice(&width.to_le_bytes());
    header.extend_from_slice(&(-height).to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&32u16.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&(bytes as u32).to_le_bytes());
    header.extend_from_slice(&[0u8; 16]);
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .context("cannot create native capture artifact")?;
    output.write_all(&header)?;
    output.write_all(data)?;
    output.flush()?;
    Ok(
        json!({"path":path,"width":width,"height":height,"background":background,"controls":rendered,
        "scope":"actual native chrome painters; WebView pixels omitted","format":"bmp-rgb32"}),
    )
}
