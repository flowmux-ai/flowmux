// SPDX-License-Identifier: GPL-3.0-or-later
//! Native shell colors and typography. BUTTON/EDIT/LISTBOX remain native HWNDs;
//! painting does not replace keyboard, accessibility, or IME handling.

use super::*;
use crate::settings::Theme;
use windows_sys::Win32::UI::{
    Controls::{
        Dialogs::{ChooseColorW, CommDlgExtendedError, CC_FULLOPEN, CC_RGBINIT, CHOOSECOLORW},
        InitCommonControlsEx, DRAWITEMSTRUCT, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX, ODS_DISABLED,
        ODS_FOCUS, ODS_HOTLIGHT, ODS_NOACCEL, ODS_NOFOCUSRECT, ODS_SELECTED, ODT_BUTTON,
        TOOLTIPS_CLASSW, TTF_IDISHWND, TTF_SUBCLASS, TTM_ACTIVATE, TTM_ADDTOOLW, TTM_POP,
        TTM_UPDATETIPTEXTW, TTS_NOPREFIX, TTTOOLINFOW, WM_MOUSELEAVE,
    },
    Input::KeyboardAndMouse::{
        EnableWindow, IsWindowEnabled, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    },
    Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
};

const SUBCLASS: usize = 0x464d_4348;

pub(super) fn choose_color(
    owner: HWND,
    initial: COLORREF,
    custom: &mut [COLORREF; 16],
    background: bool,
) -> anyhow::Result<Option<COLORREF>> {
    if background {
        return Ok(None);
    }
    // The common dialog has its own modal loop. Keep custom storage alive
    // for the call and restore only owned ancestors that we disabled.
    struct Disabled(Vec<HWND>);
    impl Drop for Disabled {
        fn drop(&mut self) {
            unsafe {
                for hwnd in &self.0 {
                    if IsWindow(*hwnd) != 0 {
                        EnableWindow(*hwnd, 1);
                    }
                }
            }
        }
    }
    let mut disabled = Disabled(Vec::new());
    unsafe {
        let mut ancestor = GetWindow(owner, GW_OWNER);
        for _ in 0..8 {
            if ancestor.is_null() {
                break;
            }
            if IsWindowEnabled(ancestor) != 0 {
                EnableWindow(ancestor, 0);
                disabled.0.push(ancestor);
            }
            ancestor = GetWindow(ancestor, GW_OWNER);
        }
        let mut spec = CHOOSECOLORW {
            lStructSize: std::mem::size_of::<CHOOSECOLORW>() as u32,
            hwndOwner: owner,
            rgbResult: initial,
            lpCustColors: custom.as_mut_ptr(),
            Flags: CC_FULLOPEN | CC_RGBINIT,
            ..std::mem::zeroed()
        };
        if ChooseColorW(&mut spec) == 0 {
            let error = CommDlgExtendedError();
            anyhow::ensure!(error == 0, "native color dialog failed ({error})");
            return Ok(None);
        }
        Ok(Some(spec.rgbResult))
    }
}

thread_local! {
    static TAB_DROP: RefCell<Option<(isize, bool, bool)>> = const { RefCell::new(None) };
}

pub(super) fn set_tab_drop(target: Option<(HWND, bool)>) {
    set_drop_marker(target, false);
}
pub(super) fn set_workspace_drop(target: Option<(HWND, bool)>) {
    set_drop_marker(target, true);
}
fn set_drop_marker(target: Option<(HWND, bool)>, horizontal: bool) {
    let next = target.map(|(window, before)| (window as isize, before, horizontal));
    let previous = TAB_DROP.with(|slot| slot.replace(next));
    if previous != next {
        for (window, _, _) in previous.into_iter().chain(next) {
            unsafe {
                InvalidateRect(window as HWND, std::ptr::null(), 0);
            }
        }
    }
}

// A non-activating owned popup paints above WebView2 without changing its focus
// or requiring a Windows-8-aware manifest for layered child windows.
pub(super) struct DropPreview {
    window: HWND,
    owner: HWND,
    rect: model::Rect,
    pane: PaneId,
    direction: SplitDirection,
    active: bool,
}
impl DropPreview {
    pub(super) fn new(owner: HWND) -> anyhow::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux-tab-drop-preview");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(drop_preview_proc),
                hInstance: instance,
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register tab drop preview"
            );
            let window = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                class.as_ptr(),
                wide("").as_ptr(),
                WS_POPUP | WS_DISABLED,
                0,
                0,
                1,
                1,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create tab drop preview");
            let preview = Self {
                window,
                owner,
                rect: model::Rect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                pane: PaneId::new(),
                direction: SplitDirection::Vertical,
                active: false,
            };
            checked(SetLayeredWindowAttributes(window, 0, 26, LWA_ALPHA))?;
            Ok(preview)
        }
    }
    pub(super) fn show(
        &mut self,
        pane: PaneId,
        direction: SplitDirection,
        rect: model::Rect,
        background: bool,
    ) -> anyhow::Result<()> {
        if self.active && self.rect == rect && self.pane == pane && self.direction == direction {
            return Ok(());
        }
        unsafe {
            let mut point = POINT {
                x: rect.x,
                y: rect.y,
            };
            checked(ClientToScreen(self.owner, &mut point))?;
            checked(SetWindowPos(
                self.window,
                HWND_TOP,
                point.x,
                point.y,
                rect.width,
                rect.height,
                SWP_NOACTIVATE | if background { 0 } else { SWP_SHOWWINDOW },
            ))?;
            InvalidateRect(self.window, std::ptr::null(), 0);
        }
        self.rect = rect;
        self.pane = pane;
        self.direction = direction;
        self.active = true;
        Ok(())
    }
    pub(super) fn hide(&mut self) {
        self.active = false;
        unsafe {
            ShowWindow(self.window, SW_HIDE);
        }
    }
    pub(super) fn diagnostics(&self) -> Value {
        json!({"window":self.window as usize,"rect":self.rect,"pane":self.pane,
            "zone":if self.direction==SplitDirection::Vertical {"right"}else{"down"},
            "active":self.active,"native_visible":unsafe{IsWindowVisible(self.window)!=0}})
    }
}
impl Drop for DropPreview {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
        }
    }
}
unsafe extern "system" fn drop_preview_proc(
    window: HWND,
    message: u32,
    w: WPARAM,
    l: LPARAM,
) -> LRESULT {
    match message {
        WM_NCHITTEST => HTTRANSPARENT as LRESULT,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut paint);
            let mut rect = RECT::default();
            GetClientRect(window, &mut rect);
            FillRect(dc, &rect, GetStockObject(WHITE_BRUSH));
            EndPaint(window, &paint);
            0
        }
        _ => DefWindowProcW(window, message, w, l),
    }
}
// CommCtrl.h TTTOOLINFOW_V2_SIZE ends at lParam. Use the supported prefix
// consistently for add, update and read; the v6-only reserved tail is unused.
const TOOL_INFO_V2_SIZE: u32 = std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32;

#[derive(Clone, Copy, Debug)]
pub(super) enum Role {
    Button,
    Suggested,
    Destructive,
    Swatch(COLORREF),
    Workspace {
        tree: bool,
        selected: bool,
        color: Option<COLORREF>,
        unread: bool,
    },
    Choice {
        selected: bool,
    },
    Tab {
        selected: bool,
        multiple: bool,
        kind: SurfaceIcon,
    },
    TabClose {
        selected: bool,
        multiple: bool,
    },
    Tool,
    Icon {
        kind: ChromeIcon,
        marked: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ChromeIcon {
    Bookmarks,
    Delete,
    Settings,
    Files,
    Worktrees,
    Usage,
    UsageBar,
    Sessions,
    Search,
    CommandPalette,
    OpenFile,
    Notifications,
    Back,
    Forward,
    Reload,
    Stop,
    More,
    Maximize,
    Restore,
    SplitRight,
    SplitDown,
    Browser,
    Overview,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum SurfaceIcon {
    Terminal,
    Browser,
    Editor,
}

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // Shared entrypoints also serve separately integrated child panels.
pub(super) enum ControlRole {
    Static,
    Caption,
    UsageBar,
    EmptyState,
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
    pub destructive: COLORREF,
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
                    destructive: GetSysColor(COLOR_WINDOWTEXT),
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
                destructive: rgb(246, 97, 81),
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
                destructive: rgb(192, 28, 40),
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
    let dark = STATE.with(|slot| {
        slot.borrow()
            .resolved
            .as_ref()
            .map_or(theme == Theme::Dark, |colors| colors.dark)
    });
    let dark: i32 = i32::from(dark && !high_contrast());
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
    title: HFONT,
    agent_icons: [HICON; 5],
    background: HBRUSH,
    surface: HBRUSH,
    owns_body: bool,
    owns_caption: bool,
    owns_title: bool,
    owns_background: bool,
    owns_surface: bool,
}
impl Resources {
    fn new(palette: Palette, dpi: u32) -> Self {
        fn font(points: i32, dpi: u32, weight: i32) -> (HFONT, bool) {
            let font = unsafe {
                CreateFontW(
                    -((points * dpi as i32 + 36) / 72),
                    0,
                    0,
                    0,
                    weight,
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
        let (body, owns_body) = font(11, dpi, FW_NORMAL as i32);
        let (caption, owns_caption) = font(9, dpi, FW_NORMAL as i32);
        let (title, owns_title) = font(20, dpi, FW_EXTRABOLD as i32);
        // 64px rasterizations of the pinned SVGs in builtin_icons.rs. Lobe Icons
        // (MIT); attribution and terms ship in assets/editor/THIRD_PARTY_NOTICES.md.
        // CreateIconFromResourceEx requires DWORD-aligned resource bytes.
        #[repr(align(4))]
        struct IconBytes<const N: usize>([u8; N]);
        let claude = IconBytes(*include_bytes!("../../assets/claude.png"));
        let codex = IconBytes(*include_bytes!("../../assets/codex.png"));
        let opencode = IconBytes(*include_bytes!("../../assets/opencode.png"));
        let cline = IconBytes(*include_bytes!("../../assets/cline.png"));
        let antigravity = IconBytes(*include_bytes!("../../assets/antigravity.png"));
        let agent_icons = [
            claude.0.as_slice(),
            codex.0.as_slice(),
            opencode.0.as_slice(),
            cline.0.as_slice(),
            antigravity.0.as_slice(),
        ]
        .map(|bytes| {
            let size = (14 * dpi as i32 + 48) / 96;
            let icon = unsafe {
                CreateIconFromResourceEx(
                    bytes.as_ptr(),
                    bytes.len() as u32,
                    1,
                    0x30000,
                    size,
                    size,
                    0,
                )
            };
            if icon.is_null() {
                eprintln!("native agent icon: {}", std::io::Error::last_os_error());
            }
            icon
        });
        let (background, owns_background) = brush(palette.background);
        let (surface, owns_surface) = brush(palette.surface);
        Self {
            body,
            caption,
            title,
            agent_icons,
            background,
            surface,
            owns_body,
            owns_caption,
            owns_title,
            owns_background,
            owns_surface,
        }
    }
}
impl Drop for Resources {
    fn drop(&mut self) {
        unsafe {
            for icon in self.agent_icons {
                if !icon.is_null() {
                    DestroyIcon(icon);
                }
            }
            for (object, owned) in [
                (self.body, self.owns_body),
                (self.caption, self.owns_caption),
                (self.title, self.owns_title),
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

pub(super) fn draw_agent_icon(dc: HDC, agent: &str, area: RECT) -> bool {
    let index = match agent {
        "claude" => 0,
        "codex" => 1,
        "opencode" => 2,
        "cline" => 3,
        "antigravity" => 4,
        _ => return false,
    };
    let icon = STATE.with(|slot| slot.borrow().resources.agent_icons[index]);
    !icon.is_null()
        && area.right > area.left
        && area.bottom > area.top
        && unsafe {
            DrawIconEx(
                dc,
                area.left,
                area.top,
                icon,
                area.right - area.left,
                area.bottom - area.top,
                0,
                std::ptr::null_mut(),
                DI_NORMAL,
            ) != 0
        }
}

#[derive(Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct WorkspaceLine {
    pub text: String,
    pub path: bool,
    pub agent: Option<flowmux_core::AgentPresence>,
    /// None is a top-level row; Some records the outer branch continuation.
    pub parent: Option<bool>,
    pub continues: bool,
}

#[derive(Clone)]
struct Entry {
    button: Option<Role>,
    control: ControlRole,
    hot: bool,
    focused: bool,
    workspace_lines: Vec<WorkspaceLine>,
}
#[derive(Clone, Copy)]
struct WorkspaceClose {
    button: isize,
    available: bool,
}
struct State {
    theme: Theme,
    resolved: Option<crate::theme::ResolvedTheme>,
    dpi: u32,
    palette: Palette,
    resources: Resources,
    controls: HashMap<isize, Entry>,
    tooltips: HashMap<isize, Tooltip>,
    pane_headers: HashMap<isize, Vec<(model::Rect, bool)>>,
    zoom_frames: HashMap<isize, (model::Rect, model::Rect)>,
    workspace_closes: HashMap<isize, WorkspaceClose>,
}
impl State {
    fn new() -> Self {
        let palette = Palette::new(Theme::Dark, high_contrast());
        Self {
            theme: Theme::Dark,
            resolved: None,
            dpi: 96,
            palette,
            resources: Resources::new(palette, 96),
            controls: HashMap::new(),
            tooltips: HashMap::new(),
            pane_headers: HashMap::new(),
            zoom_frames: HashMap::new(),
            workspace_closes: HashMap::new(),
        }
    }
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::new()); }

pub(super) fn palette() -> Palette {
    STATE.with(|slot| slot.borrow().palette)
}

pub(super) fn register_workspace_close(row: HWND, button: HWND) {
    STATE.with(|slot| {
        slot.borrow_mut()
            .workspace_closes
            .entry(row as isize)
            .or_insert(WorkspaceClose {
                button: button as isize,
                available: false,
            });
    });
}

pub(super) fn layout_workspace_close(button: HWND, available: bool) {
    let row = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        let row = state.workspace_closes.iter_mut().find_map(|(row, close)| {
            if close.button != button as isize {
                return None;
            }
            close.available = available;
            Some(*row)
        });
        if !available {
            for window in row.into_iter().chain(Some(button as isize)) {
                if let Some(entry) = state.controls.get_mut(&window) {
                    entry.hot = false;
                    entry.focused = false;
                }
            }
        }
        row
    });
    if let Some(row) = row {
        refresh_workspace_close(row);
    }
}

pub(super) fn workspace_close_key(message: &MSG) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyState};
    if message.message != WM_KEYDOWN || message.wParam != 9 {
        return false;
    }
    let pair = STATE.with(|slot| {
        slot.borrow()
            .workspace_closes
            .iter()
            .find_map(|(row, close)| {
                (close.available
                    && (*row == message.hwnd as isize || close.button == message.hwnd as isize))
                    .then_some((*row as HWND, close.button as HWND))
            })
    });
    let Some((row, close)) = pair else {
        return false;
    };
    unsafe {
        let root = GetAncestor(row, GA_ROOT);
        if GetFocus() != message.hwnd
            || IsWindowVisible(root) == 0
            || IsWindowEnabled(root) == 0
            || IsWindowEnabled(close) == 0
            || [0x11, 0x12, 0x5b, 0x5c]
                .into_iter()
                .any(|key| GetKeyState(key) < 0)
        {
            return false;
        }
        let backwards = GetKeyState(0x10) < 0;
        let next = if message.hwnd == row && !backwards {
            close
        } else if message.hwnd == close && backwards {
            row
        } else {
            let next = GetNextDlgTabItem(root, row, i32::from(backwards));
            if next == close {
                GetNextDlgTabItem(root, close, i32::from(backwards))
            } else {
                next
            }
        };
        if next.is_null() {
            return false;
        }
        SetFocus(next);
    }
    true
}

fn refresh_workspace_close(row: isize) {
    let pair = STATE.with(|slot| {
        let state = slot.borrow();
        let close = state.workspace_closes.get(&row)?;
        let active = [row, close.button].iter().any(|window| {
            state
                .controls
                .get(window)
                .is_some_and(|entry| entry.hot || entry.focused)
        });
        Some((close.button, close.available && active))
    });
    if let Some((button, active)) = pair {
        unsafe {
            let show = active && GetWindowLongPtrW(row as HWND, GWL_STYLE) as u32 & WS_VISIBLE != 0;
            EnableWindow(button as HWND, i32::from(show));
            if (GetWindowLongPtrW(button as HWND, GWL_STYLE) as u32 & WS_VISIBLE != 0) != show {
                ShowWindow(button as HWND, if show { SW_SHOWNA } else { SW_HIDE });
                InvalidateRect(row as HWND, std::ptr::null(), 0);
            }
        }
    }
}

fn workspace_close_message(window: HWND, message: u32, wparam: WPARAM) {
    if !matches!(
        message,
        WM_MOUSEMOVE | WM_MOUSELEAVE | WM_SETFOCUS | WM_KILLFOCUS | WM_SHOWWINDOW
    ) {
        return;
    }
    let pair = STATE.with(|slot| {
        slot.borrow()
            .workspace_closes
            .iter()
            .find_map(|(row, close)| {
                (*row == window as isize || close.button == window as isize)
                    .then_some((*row, close.button))
            })
    });
    let Some((row, button)) = pair else {
        return;
    };
    if message == WM_SHOWWINDOW && window as isize == button {
        if wparam == 0 {
            STATE.with(|slot| {
                if let Some(entry) = slot.borrow_mut().controls.get_mut(&button) {
                    entry.hot = false;
                    entry.focused = false;
                }
            });
        }
        // ShowWindow sends this synchronously before its transition completes.
        return;
    }
    let other = if row == window as isize { button } else { row };
    // The row's leave can precede its overlapping button's move. Use the last
    // delivered mouse-message position only in a visible window, never the
    // desktop cursor, to keep the target clickable during that handoff.
    let entering_other = if message == WM_MOUSELEAVE && unsafe { IsWindowVisible(window) } != 0 {
        let mut rect = RECT::default();
        unsafe {
            let position = GetMessagePos();
            let x = position as u16 as i16 as i32;
            let y = (position >> 16) as u16 as i16 as i32;
            GetWindowRect(other as HWND, &mut rect) != 0
                && x >= rect.left
                && x < rect.right
                && y >= rect.top
                && y < rect.bottom
        }
    } else {
        false
    };
    STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if let Some(entry) = state.controls.get_mut(&(window as isize)) {
            match message {
                WM_SETFOCUS => entry.focused = true,
                WM_KILLFOCUS => entry.focused = false,
                WM_SHOWWINDOW if wparam == 0 => {
                    entry.hot = false;
                    entry.focused = false;
                }
                _ => {}
            }
        }
        if entering_other || (message == WM_KILLFOCUS && wparam as isize == other) {
            if let Some(entry) = state.controls.get_mut(&other) {
                if entering_other {
                    entry.hot = true;
                } else {
                    entry.focused = true;
                }
            }
        }
        if message == WM_SHOWWINDOW && wparam == 0 {
            if let Some(entry) = state.controls.get_mut(&other) {
                entry.hot = false;
                entry.focused = false;
            }
        }
    });
    if entering_other {
        unsafe {
            let mut event = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: other as HWND,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut event);
        }
    }
    refresh_workspace_close(row);
}

pub(super) fn set_zoom_frame(window: HWND, frame: Option<(model::Rect, model::Rect)>) {
    let changed = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if let Some(frame) = frame {
            state.zoom_frames.insert(window as isize, frame) != Some(frame)
        } else {
            state.zoom_frames.remove(&(window as isize)).is_some()
        }
    });
    if changed {
        unsafe {
            InvalidateRect(window, std::ptr::null(), 1);
        }
    }
}

pub(super) fn set_pane_headers(window: HWND, headers: Vec<(model::Rect, bool)>) {
    let changed = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if headers.is_empty() {
            state.pane_headers.remove(&(window as isize)).is_some()
        } else if state.pane_headers.get(&(window as isize)) == Some(&headers) {
            false
        } else {
            state.pane_headers.insert(window as isize, headers);
            true
        }
    });
    if changed {
        unsafe {
            InvalidateRect(window, std::ptr::null(), 1);
        }
    }
}

fn in_pane_header(window: HWND) -> bool {
    // Use actual native geometry so toolbar buttons share the header's surface
    // without adding separate roles or retaining stale HWND position metadata.
    let root = unsafe { GetAncestor(window, GA_ROOT) };
    if !STATE.with(|slot| slot.borrow().pane_headers.contains_key(&(root as isize))) {
        return false;
    }
    let mut rect = RECT::default();
    unsafe {
        if GetWindowRect(window, &mut rect) == 0 {
            return false;
        }
        MapWindowPoints(
            std::ptr::null_mut(),
            root,
            (&mut rect as *mut RECT).cast(),
            2,
        );
    }
    STATE.with(|slot| {
        slot.borrow()
            .pane_headers
            .get(&(root as isize))
            .is_some_and(|headers| {
                headers.iter().any(|(header, _)| {
                    rect.left >= header.x
                        && rect.top >= header.y
                        && rect.right <= header.x.saturating_add(header.width)
                        && rect.bottom <= header.y.saturating_add(header.height)
                })
            })
    })
}

pub(super) fn suggested_colors() -> (COLORREF, COLORREF) {
    let palette = palette();
    let foreground = if palette.high_contrast {
        unsafe { GetSysColor(COLOR_HIGHLIGHTTEXT) }
    } else {
        // Choose readable text even when a custom terminal theme supplies accent.
        let channel = |shift| {
            let value = ((palette.accent >> shift) & 255u32) as f64 / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        let luminance = 0.2126 * channel(0) + 0.7152 * channel(8) + 0.0722 * channel(16);
        if luminance > 0.179 {
            rgb(0, 0, 0)
        } else {
            rgb(255, 255, 255)
        }
    };
    (palette.accent, foreground)
}

pub(super) fn configure_settings(settings: &crate::settings::TerminalSettings, dpi: u32) {
    let colors = crate::theme::resolve(settings);
    let mode = if colors.dark {
        Theme::Dark
    } else {
        Theme::Light
    };
    // Keep the established legacy Windows chrome until a preset or custom color is selected.
    let custom = settings.theme_preset.is_some() || settings.theme_overrides != Default::default();
    STATE.with(|slot| slot.borrow_mut().resolved = custom.then_some(colors));
    configure(mode, dpi);
}
fn color_ref(hex: &str) -> COLORREF {
    let value = u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or_default();
    rgb(value >> 16, (value >> 8) & 255, value & 255)
}
fn blend(a: COLORREF, b: COLORREF, percent: u32) -> COLORREF {
    let channel = |shift: u32| {
        (((a >> shift) & 255u32) * percent + ((b >> shift) & 255u32) * (100 - percent)) / 100u32
    };
    rgb(channel(0), channel(8), channel(16))
}

pub(super) fn configure(theme: Theme, dpi: u32) {
    let dpi = dpi.clamp(48, 768);
    let contrast = high_contrast();
    let mut palette = Palette::new(theme, contrast);
    if !contrast {
        if let Some(colors) = STATE.with(|slot| slot.borrow().resolved.clone()) {
            let bg = color_ref(&colors.background);
            let fg = color_ref(&colors.foreground);
            palette.background = bg;
            palette.surface = blend(fg, bg, 4);
            palette.foreground = fg;
            palette.muted = blend(fg, bg, 65);
            palette.border = blend(fg, bg, 22);
            palette.hover = blend(fg, bg, 10);
            palette.selected = colors
                .selection_background
                .as_deref()
                .map(color_ref)
                .unwrap_or_else(|| blend(fg, bg, 18));
            palette.accent = color_ref(&colors.palette[4]);
            palette.destructive = color_ref(&colors.palette[1]);
        }
    }
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
                    if matches!(entry.control, ControlRole::Caption | ControlRole::UsageBar) {
                        resources.caption
                    } else {
                        resources.body
                    },
                )
            })
            .collect();
        let headers: Vec<_> = state.pane_headers.keys().copied().collect();
        let old = std::mem::replace(&mut state.resources, resources);
        Some((controls, headers, old))
    });
    if let Some((controls, headers, old)) = swap {
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
        for hwnd in headers {
            unsafe {
                if IsWindow(hwnd as HWND) != 0 {
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
pub(super) fn register_swatch(window: HWND, color: COLORREF) {
    register_button(window, Role::Swatch(color));
}
pub(super) fn set_swatch(window: HWND, color: COLORREF) {
    STATE.with(|slot| {
        if let Some(entry) = slot.borrow_mut().controls.get_mut(&(window as isize)) {
            entry.button = Some(Role::Swatch(color));
        }
    });
    unsafe {
        InvalidateRect(window, std::ptr::null(), 1);
    }
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
                focused: false,
                workspace_lines: Vec::new(),
            },
        );
        if matches!(control, ControlRole::Caption | ControlRole::UsageBar) {
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
    update_tooltip(window);
}

pub(super) fn unregister(window: HWND) {
    if TAB_DROP.with(|slot| {
        slot.borrow()
            .is_some_and(|(target, _, _)| target == window as isize)
    }) {
        set_tab_drop(None);
    }
    remove_tooltip(window);
    STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        state.controls.remove(&(window as isize));
        state
            .workspace_closes
            .retain(|row, close| *row != window as isize && close.button != window as isize);
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
    update_tooltip(window);
    unsafe {
        InvalidateRect(window, std::ptr::null(), 0);
    }
}

pub(super) fn set_workspace_lines(window: HWND, lines: Vec<WorkspaceLine>) {
    let changed = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        let Some(entry) = state.controls.get_mut(&(window as isize)) else {
            return false;
        };
        if entry.workspace_lines == lines {
            return false;
        }
        entry.workspace_lines = lines;
        true
    });
    if changed {
        unsafe {
            InvalidateRect(window, std::ptr::null(), 0);
        }
    }
}

pub(super) fn workspace_lines(window: HWND) -> Vec<WorkspaceLine> {
    STATE.with(|slot| {
        slot.borrow()
            .controls
            .get(&(window as isize))
            .map(|entry| entry.workspace_lines.clone())
            .unwrap_or_default()
    })
}

pub(super) fn shutdown() {
    let controls: Vec<_> = STATE.with(|slot| slot.borrow().controls.keys().copied().collect());
    for window in controls {
        unregister(window as HWND);
    }
    STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        state.pane_headers.clear();
        state.zoom_frames.clear();
    });
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
    super::keybindings::native_key_guard(window, message);
    let empty = STATE.with(|slot| {
        slot.borrow()
            .controls
            .get(&(window as isize))
            .is_some_and(|entry| matches!(entry.control, ControlRole::EmptyState))
    });
    if empty {
        match message {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(window, &mut paint);
                draw_empty_state(window, dc);
                EndPaint(window, &paint);
                return 0;
            }
            WM_PRINTCLIENT => {
                draw_empty_state(window, wparam as HDC);
                return 1;
            }
            WM_ERASEBKGND => return 1,
            _ => {}
        }
    }
    if row_pointer(window, message, lparam) {
        return 0;
    }
    if message == WM_SETTEXT {
        let result = DefSubclassProc(window, message, wparam, lparam);
        update_tooltip(window); // Caption changes, including unread counts, win immediately.
        return result;
    }
    match message {
        WM_MOUSEMOVE | WM_MOUSELEAVE => {
            activate_tooltip(window);
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
                if hot && IsWindowVisible(window) != 0 {
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
            remove_tooltip(window);
            STATE.with(|slot| {
                let mut state = slot.borrow_mut();
                state.controls.remove(&(window as isize));
                state.pane_headers.remove(&(window as isize));
                state.zoom_frames.remove(&(window as isize));
                state.workspace_closes.retain(|row, close| {
                    *row != window as isize && close.button != window as isize
                });
            });
            RemoveWindowSubclass(window, Some(control_proc), SUBCLASS);
        }
        WM_SHOWWINDOW if wparam == 0 => {
            let tooltip = STATE.with(|slot| {
                slot.borrow()
                    .tooltips
                    .get(&(window as isize))
                    .map(|tip| tip.window)
            });
            if let Some(tooltip) = tooltip {
                SendMessageW(tooltip, TTM_ACTIVATE, 0, 0);
                SendMessageW(tooltip, TTM_POP, 0, 0);
            }
        }
        _ => {}
    }
    workspace_close_message(window, message, wparam);
    DefSubclassProc(window, message, wparam, lparam)
}

struct Tooltip {
    window: HWND,
    text: Vec<u16>,
}
impl Drop for Tooltip {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(self.window) != 0 {
                DestroyWindow(self.window);
            }
        }
    }
}
fn remove_tooltip(window: HWND) {
    let tooltip = STATE.with(|slot| slot.borrow_mut().tooltips.remove(&(window as isize)));
    drop(tooltip); // No RefCell borrow may span native tooltip/subclass callbacks.
}
pub(super) fn tooltip_text(window: HWND) -> Option<String> {
    let tooltip = STATE.with(|slot| {
        slot.borrow()
            .tooltips
            .get(&(window as isize))
            .map(|tip| tip.window)
    })?;
    let mut text = vec![0u16; 2048];
    let mut info = TTTOOLINFOW {
        cbSize: TOOL_INFO_V2_SIZE,
        hwnd: unsafe { GetAncestor(window, GA_ROOT) },
        uId: window as usize,
        lpszText: text.as_mut_ptr(),
        ..Default::default()
    };
    // TTM_GETTEXTW copies up to wParam UTF-16 units including NUL. Both tooltip
    // and output buffer belong to this UI thread/process; no cross-process ptr.
    unsafe {
        SendMessageW(
            tooltip,
            windows_sys::Win32::UI::Controls::TTM_GETTEXTW,
            text.len(),
            (&mut info as *mut TTTOOLINFOW) as LPARAM,
        );
    }
    let end = text
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(text.len());
    String::from_utf16(&text[..end]).ok()
}
fn activate_tooltip(window: HWND) {
    let tooltip = STATE.with(|slot| {
        slot.borrow()
            .tooltips
            .get(&(window as isize))
            .map(|tip| tip.window)
    });
    if let Some(tooltip) = tooltip {
        unsafe {
            let visible =
                IsWindowVisible(window) != 0 && IsWindowVisible(GetAncestor(window, GA_ROOT)) != 0;
            SendMessageW(tooltip, TTM_ACTIVATE, usize::from(visible), 0);
            if !visible {
                SendMessageW(tooltip, TTM_POP, 0, 0);
            }
        }
    }
}
fn update_tooltip(window: HWND) {
    let icon = STATE.with(|slot| {
        slot.borrow()
            .controls
            .get(&(window as isize))
            .is_some_and(|entry| {
                matches!(entry.button, Some(Role::Icon { .. }))
                    || matches!(entry.control, ControlRole::UsageBar)
            })
    });
    if !icon {
        remove_tooltip(window);
        return;
    }
    let mut text = vec![0u16; 2048];
    let length =
        unsafe { GetWindowTextW(window, text.as_mut_ptr(), text.len() as i32) }.max(0) as usize;
    text.truncate(length);
    text.push(0);
    let unchanged = STATE.with(|slot| {
        slot.borrow()
            .tooltips
            .get(&(window as isize))
            .is_some_and(|tip| tip.text == text)
    });
    if unchanged {
        activate_tooltip(window);
        return;
    }
    let existing = STATE.with(|slot| slot.borrow_mut().tooltips.remove(&(window as isize)));
    let parent = unsafe { GetAncestor(window, GA_ROOT) };
    let mut info = TTTOOLINFOW {
        cbSize: TOOL_INFO_V2_SIZE,
        uFlags: TTF_IDISHWND | TTF_SUBCLASS,
        hwnd: parent,
        uId: window as usize,
        lpszText: text.as_mut_ptr(),
        ..Default::default()
    };
    let tip = if let Some(mut tip) = existing {
        unsafe {
            SendMessageW(
                tip.window,
                TTM_UPDATETIPTEXTW,
                0,
                (&mut info as *mut TTTOOLINFOW) as LPARAM,
            );
        }
        tip.text = text; // Old storage stays alive until the native update returns.
        tip
    } else {
        unsafe {
            let classes = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_BAR_CLASSES,
            };
            if InitCommonControlsEx(&classes) == 0 {
                #[cfg(debug_assertions)]
                eprintln!("native tooltip: InitCommonControlsEx failed");
                return;
            }
            let tooltip = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                TOOLTIPS_CLASSW,
                std::ptr::null(),
                WS_POPUP | TTS_NOPREFIX,
                0,
                0,
                0,
                0,
                parent,
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            if tooltip.is_null() {
                #[cfg(debug_assertions)]
                eprintln!("native tooltip: CreateWindowExW failed");
                return;
            }
            let tip = Tooltip {
                window: tooltip,
                text,
            };
            SendMessageW(tooltip, TTM_ACTIVATE, 0, 0);
            if SendMessageW(
                tooltip,
                TTM_ADDTOOLW,
                0,
                (&mut info as *mut TTTOOLINFOW) as LPARAM,
            ) == 0
            {
                #[cfg(debug_assertions)]
                eprintln!("native tooltip: TTM_ADDTOOLW failed (cbSize={TOOL_INFO_V2_SIZE})");
                return;
            }
            tip
        }
    };
    STATE.with(|slot| slot.borrow_mut().tooltips.insert(window as isize, tip));
    activate_tooltip(window);
}

fn fill(dc: HDC, rect: &RECT, color: COLORREF) {
    unsafe {
        SetDCBrushColor(dc, color);
        FillRect(dc, rect, GetStockObject(DC_BRUSH));
    }
}

/// Canonical equivalence for a transient GDI drawing buffer only. Native probes
/// showed decomposed Hangul rendered as separate Jamo even with font fallback;
/// painting the equivalent NFC sequence fixes that display without changing the
/// HWND caption, model/path identity, input, clipboard, or persisted codepoints.
/// ASCII avoids native work. Unsupported/invalid input or sizing failure keeps
/// the original drawing buffer, and allocation is capped independently of GDI.
pub(super) fn caption_for_paint(original: &[u16]) -> std::borrow::Cow<'_, [u16]> {
    use std::borrow::Cow;
    use windows_sys::Win32::Globalization::{NormalizationC, NormalizeString};
    const MAX_DRAWING_UNITS: usize = 8192;
    if original.len() > MAX_DRAWING_UNITS || original.iter().all(|unit| *unit <= 0x7f) {
        return Cow::Borrowed(original);
    }
    let required = unsafe {
        NormalizeString(
            NormalizationC,
            original.as_ptr(),
            original.len() as i32,
            std::ptr::null_mut(),
            0,
        )
    };
    if required <= 0 || required as usize > MAX_DRAWING_UNITS {
        return Cow::Borrowed(original);
    }
    let mut drawing = vec![0u16; required as usize];
    let written = unsafe {
        NormalizeString(
            NormalizationC,
            original.as_ptr(),
            original.len() as i32,
            drawing.as_mut_ptr(),
            drawing.len() as i32,
        )
    };
    if written <= 0 || written as usize > drawing.len() {
        return Cow::Borrowed(original);
    }
    drawing.truncate(written as usize);
    Cow::Owned(drawing)
}

// One painter serves both the real STATIC and owned, in-process capture. The
// native caption remains the accessible text; only the drawing copy is shaped.
fn draw_empty_state(window: HWND, dc: HDC) {
    let (palette, title, body, dpi) = STATE.with(|slot| {
        let state = slot.borrow();
        (
            state.palette,
            state.resources.title,
            state.resources.body,
            state.dpi,
        )
    });
    if dc.is_null() {
        return;
    }
    let pixel = |dip: i32| ((dip * dpi as i32 + 48) / 96).max(1);
    unsafe {
        let saved = SaveDC(dc);
        if saved == 0 {
            return;
        }
        let mut client = RECT::default();
        GetClientRect(window, &mut client);
        fill(dc, &client, palette.background);
        let top = ((client.bottom - pixel(228)) / 2).max(0);
        let cx = client.right / 2;
        let cy = top + pixel(64);
        let muted = if palette.high_contrast {
            palette.foreground
        } else {
            palette.muted
        };
        SelectObject(dc, GetStockObject(DC_PEN));
        SelectObject(dc, GetStockObject(NULL_BRUSH));
        SetDCPenColor(dc, muted);
        RoundRect(
            dc,
            cx - pixel(64),
            cy - pixel(50),
            cx + pixel(64),
            cy + pixel(50),
            pixel(16),
            pixel(16),
        );
        draw_terminal_glyph(dc, cx, cy, pixel(34), pixel(8));

        let mut caption = vec![0u16; 2048];
        let length =
            GetWindowTextW(window, caption.as_mut_ptr(), caption.len() as i32).max(0) as usize;
        caption.truncate(length);
        let drawing = caption_for_paint(&caption);
        let mut lines = drawing.splitn(2, |unit| *unit == b'\n' as u16);
        let heading = lines.next().unwrap_or_default();
        let description = lines.next().unwrap_or_default();
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, palette.foreground);
        SelectObject(dc, title);
        let mut text_rect = RECT {
            left: pixel(16).min(client.right / 2),
            right: (client.right - pixel(16)).max(client.right / 2),
            top: top + pixel(164),
            bottom: top + pixel(196),
        };
        DrawTextW(
            dc,
            heading.as_ptr(),
            heading.len() as i32,
            &mut text_rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
        );
        SetTextColor(dc, palette.foreground);
        SelectObject(dc, body);
        text_rect.top = top + pixel(208);
        text_rect.bottom = client.bottom.max(text_rect.top);
        DrawTextW(
            dc,
            description.as_ptr(),
            description.len() as i32,
            &mut text_rect,
            DT_CENTER | DT_WORDBREAK | DT_NOPREFIX,
        );
        RestoreDC(dc, saved);
    }
}

unsafe fn draw_terminal_glyph(dc: HDC, cx: i32, cy: i32, radius: i32, unit: i32) {
    MoveToEx(dc, cx - radius, cy - radius + unit, std::ptr::null_mut());
    LineTo(dc, cx - unit, cy);
    LineTo(dc, cx - radius, cy + radius - unit);
    MoveToEx(dc, cx + unit, cy + radius - unit, std::ptr::null_mut());
    LineTo(dc, cx + radius + 1, cy + radius - unit);
}

unsafe fn draw_agent_status(
    dc: HDC,
    status: flowmux_core::AgentStatus,
    seen: bool,
    cx: i32,
    cy: i32,
    unit: i32,
    color: COLORREF,
) {
    use flowmux_core::AgentStatus;
    SelectObject(dc, GetStockObject(DC_PEN));
    SelectObject(dc, GetStockObject(NULL_BRUSH));
    SetDCPenColor(dc, color);
    let line = |x1, y1, x2, y2| {
        MoveToEx(dc, cx + x1 * unit, cy + y1 * unit, std::ptr::null_mut());
        LineTo(dc, cx + x2 * unit, cy + y2 * unit);
    };
    match status {
        AgentStatus::Blocked => {
            line(0, -5, 5, 4);
            line(5, 4, -5, 4);
            line(-5, 4, 0, -5);
            line(0, -2, 0, 1);
            line(0, 2, 0, 3);
        }
        AgentStatus::Working => {
            for (x, y) in [
                (1, 0),
                (1, 1),
                (0, 1),
                (-1, 1),
                (-1, 0),
                (-1, -1),
                (0, -1),
                (1, -1),
            ] {
                line(x * 3, y * 3, x * 5, y * 5);
            }
        }
        AgentStatus::Done if !seen => {
            line(-5, 0, -1, 4);
            line(-1, 4, 5, -4);
        }
        AgentStatus::Done | AgentStatus::Idle => {
            line(-2, -4, -2, 5);
            line(2, -4, 2, 5);
        }
        AgentStatus::Unknown => {
            SetTextColor(dc, color);
            let mut area = RECT {
                left: cx - unit * 6,
                right: cx + unit * 6,
                top: cy - unit * 8,
                bottom: cy + unit * 8,
            };
            DrawTextW(
                dc,
                [b'?' as u16].as_ptr(),
                1,
                &mut area,
                DT_CENTER | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX,
            );
        }
    }
}

unsafe fn draw_tab_background(
    dc: HDC,
    rect: &RECT,
    color: COLORREF,
    palette: Palette,
    close: bool,
    accent: bool,
    dpi: u32,
) {
    let radius = ((4 * dpi as i32 + 48) / 96).max(1);
    fill(dc, rect, palette.surface);
    let saved = SaveDC(dc);
    if saved == 0 {
        fill(dc, rect, color);
        return;
    }
    // Extend the other corners beyond this control and clip to its bounds. The
    // adjacent body/close HWNDs then form one tab with only its top corners round.
    IntersectClipRect(dc, rect.left, rect.top, rect.right, rect.bottom);
    if BeginPath(dc) != 0 {
        RoundRect(
            dc,
            rect.left - if close { radius * 2 } else { 0 },
            rect.top,
            rect.right + if close { 0 } else { radius * 2 },
            rect.bottom + radius,
            radius * 2,
            radius * 2,
        );
        if EndPath(dc) != 0 {
            SelectClipPath(dc, RGN_AND);
        } else {
            AbortPath(dc);
        }
    }
    fill(dc, rect, color);
    if accent {
        fill(
            dc,
            &RECT {
                bottom: (rect.top + ((2 * dpi as i32 + 48) / 96).max(1)).min(rect.bottom),
                ..*rect
            },
            palette.accent,
        );
    }
    RestoreDC(dc, saved);
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
                        state.resources.caption,
                        state.dpi,
                    )
                })
            })
    });
    let Some((role, hot, palette, font, caption_font, dpi)) = paint else {
        return false;
    };
    let selected = matches!(
        role,
        Role::Workspace { selected: true, .. }
            | Role::Choice { selected: true }
            | Role::Tab { selected: true, .. }
            | Role::TabClose { selected: true, .. }
    ) || (matches!(
        role,
        Role::Icon {
            kind: ChromeIcon::UsageBar,
            ..
        }
    ) && unsafe { SendMessageW(item.hwndItem, BM_GETCHECK, 0, 0) } == 1);
    let pressed = item.itemState & ODS_SELECTED != 0;
    let hot = hot || item.itemState & ODS_HOTLIGHT != 0;
    let disabled = item.itemState & ODS_DISABLED != 0;
    let suggested = matches!(role, Role::Suggested) && !disabled;
    let workspace = matches!(role, Role::Workspace { .. });
    let rows = workspace_lines(item.hwndItem);
    let status = rows
        .iter()
        .filter_map(|line| line.agent.as_ref().map(|agent| agent.status))
        .max_by_key(|status| status.rollup_rank());
    let highlighted = (!workspace && selected) || pressed || hot;
    let color = if suggested {
        palette.accent
    } else if workspace {
        if hot || pressed {
            if palette.high_contrast {
                palette.hover
            } else {
                // Linux uses alpha(sidebar foreground, 0.055) for both states.
                let channel = |shift: u32| {
                    (((palette.foreground >> shift) & 255u32) * 55
                        + ((palette.background >> shift) & 255u32) * 945)
                        / 1000
                };
                rgb(channel(0), channel(8), channel(16))
            }
        } else {
            palette.background
        }
    } else if pressed || selected {
        palette.selected
    } else if hot {
        palette.hover
    } else if matches!(role, Role::Tab { .. } | Role::TabClose { .. })
        || (matches!(role, Role::Tool | Role::Icon { .. }) && in_pane_header(item.hwndItem))
    {
        palette.surface
    } else {
        palette.background
    };
    let color = if workspace && !palette.high_contrast {
        use flowmux_core::AgentStatus;
        let tint = match status {
            Some(AgentStatus::Blocked) => Some((rgb(239, 68, 68), 16u32)),
            Some(AgentStatus::Done) => Some((rgb(59, 130, 246), 14u32)),
            _ => None,
        };
        tint.map_or(color, |(ink, alpha)| {
            let channel = |shift| {
                (((ink >> shift) & 255u32) * alpha
                    + ((palette.background >> shift) & 255u32) * (100 - alpha))
                    / 100u32
            };
            rgb(channel(0), channel(8), channel(16))
        })
    } else {
        color
    };
    let text = if disabled {
        palette.muted
    } else if suggested {
        suggested_colors().1
    } else if palette.high_contrast && highlighted {
        unsafe { GetSysColor(COLOR_HIGHLIGHTTEXT) }
    } else if !palette.high_contrast && matches!(role, Role::Destructive) {
        palette.destructive
    } else if !palette.high_contrast && matches!(role, Role::Icon { marked: true, .. }) {
        palette.accent
    } else if !palette.high_contrast
        && ((matches!(role, Role::Icon { .. })
            && !hot
            && !selected
            && item.itemState & ODS_FOCUS == 0)
            || matches!(
                role,
                Role::Tab {
                    selected: false,
                    ..
                } | Role::TabClose {
                    selected: false,
                    ..
                }
            ))
    {
        palette.muted
    } else {
        palette.foreground
    };
    let pixel = |dip: i32| ((dip * dpi as i32 + 48) / 96).max(1);
    unsafe {
        let saved = SaveDC(item.hDC);
        if saved == 0 {
            return false;
        }
        if let Role::Swatch(swatch) = role {
            fill(item.hDC, &item.rcItem, swatch);
            if item.itemState & ODS_FOCUS != 0 {
                DrawFocusRect(item.hDC, &item.rcItem);
            }
            RestoreDC(item.hDC, saved);
            return true;
        }
        if matches!(role, Role::Tab { .. } | Role::TabClose { .. }) {
            let multiple = match role {
                Role::Tab { multiple, .. } | Role::TabClose { multiple, .. } => multiple,
                _ => false,
            };
            draw_tab_background(
                item.hDC,
                &item.rcItem,
                color,
                palette,
                matches!(role, Role::TabClose { .. }),
                selected && multiple,
                dpi,
            );
        } else if matches!(
            role,
            Role::Workspace { .. } | Role::Choice { .. } | Role::Suggested
        ) && !palette.high_contrast
        {
            fill(item.hDC, &item.rcItem, palette.background);
            SelectObject(item.hDC, GetStockObject(NULL_PEN));
            SelectObject(item.hDC, GetStockObject(DC_BRUSH));
            SetDCBrushColor(item.hDC, color);
            RoundRect(
                item.hDC,
                item.rcItem.left,
                item.rcItem.top,
                item.rcItem.right,
                item.rcItem.bottom,
                pixel(12),
                pixel(12),
            );
        } else {
            fill(item.hDC, &item.rcItem, color);
        }
        if matches!(role, Role::Choice { selected: true }) {
            fill(
                item.hDC,
                &RECT {
                    left: item.rcItem.left,
                    right: item.rcItem.left + pixel(5),
                    top: item.rcItem.top + pixel(6),
                    bottom: item.rcItem.bottom - pixel(6),
                },
                palette.accent,
            );
        }
        if let Role::Workspace {
            color: workspace_color,
            unread,
            ..
        } = role
        {
            if selected {
                fill(
                    item.hDC,
                    &RECT {
                        right: (item.rcItem.left + pixel(5)).min(item.rcItem.right),
                        ..item.rcItem
                    },
                    palette.accent,
                );
            }
            if let Some(workspace_color) = workspace_color {
                let left = item.rcItem.left + pixel(10) + if selected { pixel(5) } else { 0 };
                let top = item.rcItem.top + pixel(6);
                let bottom = item.rcItem.bottom - pixel(6);
                if left + pixel(4) <= item.rcItem.right && top < bottom {
                    SelectObject(item.hDC, GetStockObject(NULL_PEN));
                    SelectObject(item.hDC, GetStockObject(DC_BRUSH));
                    SetDCBrushColor(
                        item.hDC,
                        if palette.high_contrast {
                            text
                        } else {
                            workspace_color
                        },
                    );
                    RoundRect(
                        item.hDC,
                        left,
                        top,
                        left + pixel(4) + 1,
                        bottom + 1,
                        pixel(4),
                        pixel(4),
                    );
                }
            }
            if unread {
                let right = item.rcItem.right - pixel(7);
                let top = item.rcItem.top + pixel(7);
                if right - pixel(7) >= item.rcItem.left && top + pixel(7) <= item.rcItem.bottom {
                    SelectObject(item.hDC, GetStockObject(NULL_PEN));
                    SelectObject(item.hDC, GetStockObject(DC_BRUSH));
                    SetDCBrushColor(
                        item.hDC,
                        if palette.high_contrast && highlighted {
                            text
                        } else {
                            palette.accent
                        },
                    );
                    Ellipse(
                        item.hDC,
                        right - pixel(7),
                        top,
                        right + 1,
                        top + pixel(7) + 1,
                    );
                }
            }
        }
        if palette.high_contrast {
            SetDCBrushColor(item.hDC, palette.border);
            FrameRect(item.hDC, &item.rcItem, GetStockObject(DC_BRUSH));
        }
        SetTextColor(item.hDC, text);
        SetBkMode(item.hDC, TRANSPARENT as i32);
        SelectObject(item.hDC, font);
        let mut original =
            vec![0u16; GetWindowTextLengthW(item.hwndItem).clamp(0, 524_288) as usize + 1];
        let length = GetWindowTextW(item.hwndItem, original.as_mut_ptr(), original.len() as i32);
        original.truncate(length.max(0) as usize);
        let label = caption_for_paint(&original);
        let length = label.len() as i32;
        let label_text = String::from_utf16_lossy(&label);
        let symbol = matches!(role, Role::Icon { .. } | Role::TabClose { .. })
            || (matches!(role, Role::Tool)
                && matches!(
                    label_text.as_str(),
                    "Close"
                        | "Close tab"
                        | "Close workspace"
                        | "Pane actions"
                        | "+"
                        | "New tab"
                        | "Newtab"
                        | "New workspace"
                        | "Add workspace"
                ));
        if let Role::Icon { kind, marked } = role {
            let cx = (item.rcItem.left + item.rcItem.right) / 2;
            let cy = (item.rcItem.top + item.rcItem.bottom) / 2;
            let x = |dip: i32| {
                cx + if dip < 0 {
                    -pixel(-dip)
                } else if dip > 0 {
                    pixel(dip)
                } else {
                    0
                }
            };
            let y = |dip: i32| {
                cy + if dip < 0 {
                    -pixel(-dip)
                } else if dip > 0 {
                    pixel(dip)
                } else {
                    0
                }
            };
            SelectObject(item.hDC, GetStockObject(DC_PEN));
            SelectObject(item.hDC, GetStockObject(NULL_BRUSH));
            SetDCPenColor(item.hDC, text);
            let line = |a, b, c, d| {
                MoveToEx(item.hDC, x(a), y(b), std::ptr::null_mut());
                LineTo(item.hDC, x(c), y(d));
            };
            match kind {
                ChromeIcon::Bookmarks => {
                    MoveToEx(item.hDC, x(-5), y(-7), std::ptr::null_mut());
                    for (a, b) in [(5, -7), (5, 7), (0, 3), (-5, 7), (-5, -7)] {
                        LineTo(item.hDC, x(a), y(b));
                    }
                }
                ChromeIcon::Delete => {
                    Rectangle(item.hDC, x(-5), y(-4), x(5) + 1, y(7) + 1);
                    line(-7, -6, 7, -6);
                    line(-2, -8, 2, -8);
                    line(-2, -2, -2, 5);
                    line(2, -2, 2, 5);
                }
                ChromeIcon::UsageBar => {
                    Rectangle(item.hDC, x(-7), y(-6), x(7) + 1, y(6) + 1);
                    line(-7, 2, 7, 2);
                }
                ChromeIcon::Maximize => {
                    Rectangle(item.hDC, x(-6), y(-6), x(6) + 1, y(6) + 1);
                }
                ChromeIcon::Restore => {
                    Rectangle(item.hDC, x(-6), y(-2), x(2) + 1, y(6) + 1);
                    line(-2, -3, -2, -6);
                    line(-2, -6, 6, -6);
                    line(6, -6, 6, 2);
                    line(6, 2, 3, 2);
                }
                ChromeIcon::SplitRight | ChromeIcon::SplitDown => {
                    Rectangle(item.hDC, x(-7), y(-6), x(7) + 1, y(6) + 1);
                    if matches!(kind, ChromeIcon::SplitRight) {
                        line(0, -6, 0, 6);
                    } else {
                        line(-7, 0, 7, 0);
                    }
                }
                ChromeIcon::Browser => {
                    Ellipse(item.hDC, x(-7), y(-7), x(7) + 1, y(7) + 1);
                    Ellipse(item.hDC, x(-3), y(-7), x(3) + 1, y(7) + 1);
                    line(-7, 0, 7, 0);
                }
                ChromeIcon::Overview => {
                    for (left, top) in [(-7, -7), (1, -7), (-7, 1), (1, 1)] {
                        Rectangle(item.hDC, x(left), y(top), x(left + 6) + 1, y(top + 6) + 1);
                    }
                }
                ChromeIcon::Back | ChromeIcon::Forward => {
                    let sign = if matches!(kind, ChromeIcon::Back) {
                        -1
                    } else {
                        1
                    };
                    line(-6 * sign, 0, 6 * sign, 0);
                    line(6 * sign, 0, sign, -5);
                    line(6 * sign, 0, sign, 5);
                }
                ChromeIcon::Reload => {
                    windows_sys::Win32::Graphics::Gdi::Arc(
                        item.hDC,
                        x(-6),
                        y(-6),
                        x(6) + 1,
                        y(6) + 1,
                        x(6),
                        y(0),
                        x(0),
                        y(-6),
                    );
                    line(0, -6, 5, -6);
                    line(5, -6, 5, -1);
                }
                ChromeIcon::Stop => {
                    Rectangle(item.hDC, x(-5), y(-5), x(5) + 1, y(5) + 1);
                }
                ChromeIcon::More => {
                    for offset in [-5, 0, 5] {
                        Ellipse(item.hDC, x(offset - 1), y(-1), x(offset + 1) + 1, y(1) + 1);
                    }
                }
                ChromeIcon::Settings => {
                    Ellipse(item.hDC, x(-5), y(-5), x(5) + 1, y(5) + 1);
                    Ellipse(item.hDC, x(-2), y(-2), x(2) + 1, y(2) + 1);
                    for (a, b, c, d) in [
                        (-7, 0, -5, 0),
                        (5, 0, 8, 0),
                        (0, -7, 0, -5),
                        (0, 5, 0, 8),
                        (-5, -5, -4, -4),
                        (4, 4, 6, 6),
                        (-5, 5, -4, 4),
                        (4, -4, 6, -6),
                    ] {
                        line(a, b, c, d);
                    }
                }
                ChromeIcon::Sessions => {
                    Ellipse(item.hDC, x(-7), y(-7), x(7) + 1, y(7) + 1);
                    line(0, -4, 0, 0);
                    line(0, 0, 4, 2);
                }
                ChromeIcon::Usage => {
                    line(-6, 6, -6, 0);
                    line(0, 6, 0, -6);
                    line(6, 6, 6, -2);
                }
                ChromeIcon::Worktrees => {
                    line(-4, -4, -4, 4);
                    line(-4, 1, 4, -3);
                    for (a, b) in [(-4, -6), (-4, 6), (5, -5)] {
                        Ellipse(item.hDC, x(a - 2), y(b - 2), x(a + 2) + 1, y(b + 2) + 1);
                    }
                }
                ChromeIcon::Files => {
                    line(-7, -3, -7, 6);
                    line(-7, 6, 7, 6);
                    line(7, 6, 7, -3);
                    line(7, -3, -7, -3);
                    line(-7, -3, -7, -6);
                    line(-7, -6, -2, -6);
                    line(-2, -6, 0, -3);
                }
                ChromeIcon::Search => {
                    Ellipse(item.hDC, x(-6), y(-6), x(3) + 1, y(3) + 1);
                    line(2, 2, 7, 7);
                }
                ChromeIcon::CommandPalette => {
                    line(-6, -5, -1, 0);
                    line(-1, 0, -6, 5);
                    line(1, 5, 7, 5);
                }
                ChromeIcon::OpenFile => {
                    line(-6, -7, 2, -7);
                    line(2, -7, 6, -3);
                    line(6, -3, 6, 7);
                    line(6, 7, -6, 7);
                    line(-6, 7, -6, -7);
                    line(2, -7, 2, -3);
                    line(2, -3, 6, -3);
                    line(-3, 2, 3, 2);
                    line(0, -1, 3, 2);
                    line(3, 2, 0, 5);
                }
                ChromeIcon::Notifications => {
                    windows_sys::Win32::Graphics::Gdi::Arc(
                        item.hDC,
                        x(-5),
                        y(-6),
                        x(5) + 1,
                        y(4),
                        x(-5),
                        y(-1),
                        x(5),
                        y(-1),
                    );
                    line(-5, -1, -5, 4);
                    line(-5, 4, -7, 5);
                    line(-7, 5, 7, 5);
                    line(7, 5, 5, 4);
                    line(5, 4, 5, -1);
                    line(-2, 7, 3, 7);
                    line(0, -8, 0, -6);
                }
            }
            if marked {
                SelectObject(item.hDC, GetStockObject(DC_BRUSH));
                SetDCBrushColor(
                    item.hDC,
                    if palette.high_contrast {
                        palette.foreground
                    } else {
                        palette.accent
                    },
                );
                Ellipse(item.hDC, x(4), y(-8), x(8) + 1, y(-4) + 1);
            }
        } else if symbol {
            let cx = (item.rcItem.left + item.rcItem.right) / 2;
            let cy = (item.rcItem.top + item.rcItem.bottom) / 2;
            let radius =
                pixel(4).min(((item.rcItem.right - item.rcItem.left) / 2 - pixel(4)).max(1));
            SelectObject(item.hDC, GetStockObject(DC_PEN));
            SetDCPenColor(item.hDC, text);
            match if matches!(role, Role::TabClose { .. }) {
                "Close tab"
            } else {
                label_text.as_str()
            } {
                "Close" | "Close tab" | "Close workspace" => {
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
        let inset = pixel(if matches!(role, Role::Tool | Role::TabClose { .. }) {
            4
        } else {
            10
        });
        let mut rect = RECT {
            left: item.rcItem.left + inset,
            right: item.rcItem.right - inset,
            ..item.rcItem
        };
        if let Role::Workspace { color, unread, .. } = role {
            if selected {
                rect.left += pixel(5);
            }
            if color.is_some() {
                rect.left += pixel(10);
            }
            if unread {
                rect.right = rect.right.min(item.rcItem.right - pixel(18));
            }
        }
        if let Role::Tab { kind, .. } = role {
            if rect.right - rect.left >= pixel(24) {
                let cx = rect.left + pixel(6);
                let cy = (rect.top + rect.bottom) / 2;
                let r = pixel(5);
                SelectObject(item.hDC, GetStockObject(DC_PEN));
                SelectObject(item.hDC, GetStockObject(NULL_BRUSH));
                SetDCPenColor(item.hDC, text);
                match kind {
                    SurfaceIcon::Terminal => {
                        draw_terminal_glyph(item.hDC, cx, cy, r, pixel(1));
                    }
                    SurfaceIcon::Browser => {
                        Ellipse(item.hDC, cx - r, cy - r, cx + r + 1, cy + r + 1);
                        Ellipse(
                            item.hDC,
                            cx - pixel(2),
                            cy - r,
                            cx + pixel(2) + 1,
                            cy + r + 1,
                        );
                        MoveToEx(item.hDC, cx - r, cy, std::ptr::null_mut());
                        LineTo(item.hDC, cx + r + 1, cy);
                    }
                    SurfaceIcon::Editor => {
                        Rectangle(item.hDC, cx - r + pixel(1), cy - r, cx + r, cy + r + 1);
                        for y in [cy - pixel(2), cy + pixel(1), cy + pixel(3)] {
                            MoveToEx(item.hDC, cx - r + pixel(3), y, std::ptr::null_mut());
                            LineTo(item.hDC, cx + r - pixel(1), y);
                        }
                    }
                }
                rect.left += pixel(20);
            }
        }
        let align = if matches!(
            role,
            Role::Workspace { .. } | Role::Choice { .. } | Role::Tab { .. }
        ) {
            DT_LEFT
        } else {
            DT_CENTER
        };
        let accelerator = if item.itemState & ODS_NOACCEL != 0 {
            DT_HIDEPREFIX
        } else {
            0
        };
        if matches!(role, Role::Workspace { tree: true, .. }) && rect.right > rect.left {
            let mut lines = label_text.split('\n');
            let title: Vec<u16> = lines.next().unwrap_or_default().encode_utf16().collect();
            let mut title_rect = RECT {
                top: rect.top + pixel(5),
                bottom: (rect.top + pixel(27)).min(rect.bottom),
                ..rect
            };
            DrawTextW(
                item.hDC,
                title.as_ptr(),
                title.len() as i32,
                &mut title_rect,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | accelerator,
            );
            SelectObject(item.hDC, caption_font);
            SetTextColor(item.hDC, palette.muted);
            SelectObject(item.hDC, GetStockObject(DC_PEN));
            SetDCPenColor(item.hDC, palette.muted);
            for (index, line) in rows.iter().enumerate() {
                let top = rect.top + pixel(27 + index as i32 * 20);
                let bottom = (top + pixel(20)).min(rect.bottom - pixel(5));
                if top >= bottom {
                    break;
                }
                let mid = (top + bottom) / 2;
                let depth = i32::from(line.parent.is_some());
                let gutter = pixel(14 * (depth + 1));
                let x = rect.left + pixel(6 + depth * 14);
                SetDCPenColor(item.hDC, palette.muted);
                if rect.right - rect.left > gutter {
                    if line.parent == Some(true) {
                        MoveToEx(item.hDC, rect.left + pixel(6), top, std::ptr::null_mut());
                        LineTo(item.hDC, rect.left + pixel(6), bottom);
                    }
                    MoveToEx(item.hDC, x, top, std::ptr::null_mut());
                    LineTo(item.hDC, x, if line.continues { bottom } else { mid + 1 });
                    MoveToEx(item.hDC, x, mid, std::ptr::null_mut());
                    LineTo(item.hDC, rect.left + gutter, mid);
                }
                // Native text/accessibility retains full Unicode; only paths
                // are shortened in the painted metadata tree.
                let shortened = if line.path {
                    let names: Vec<_> = std::path::Path::new(&line.text)
                        .components()
                        .filter_map(|part| {
                            if let std::path::Component::Normal(name) = part {
                                Some(name.to_string_lossy())
                            } else {
                                None
                            }
                        })
                        .collect();
                    if names.len() > 3 {
                        format!(
                            "...{}{}",
                            std::path::MAIN_SEPARATOR,
                            names[names.len() - 3..].join(std::path::MAIN_SEPARATOR_STR)
                        )
                    } else {
                        line.text.clone()
                    }
                } else {
                    line.text.clone()
                };
                let mut line_rect = RECT {
                    left: rect.left + gutter,
                    top,
                    bottom,
                    ..rect
                };
                let mut ink = palette.muted;
                if let Some(agent) = &line.agent {
                    use flowmux_core::AgentStatus;
                    ink = if palette.high_contrast {
                        palette.foreground
                    } else {
                        match agent.status {
                            AgentStatus::Working => rgb(245, 158, 11),
                            AgentStatus::Blocked if !agent.seen => rgb(239, 68, 68),
                            AgentStatus::Done if !agent.seen => rgb(59, 130, 246),
                            _ => palette.muted,
                        }
                    };
                    if line_rect.right - line_rect.left > pixel(32) {
                        draw_agent_status(
                            item.hDC,
                            agent.status,
                            agent.seen,
                            line_rect.left + pixel(6),
                            mid,
                            pixel(1),
                            ink,
                        );
                        let area = RECT {
                            left: line_rect.left + pixel(16),
                            right: line_rect.left + pixel(30),
                            top: mid - pixel(7),
                            bottom: mid + pixel(7),
                        };
                        if !draw_agent_icon(item.hDC, &agent.name, area) {
                            SetDCPenColor(item.hDC, ink);
                            draw_terminal_glyph(
                                item.hDC,
                                area.left + pixel(7),
                                mid,
                                pixel(5),
                                pixel(1),
                            );
                        }
                        line_rect.left += pixel(34);
                    }
                }
                SetTextColor(item.hDC, ink);
                if line_rect.left < line_rect.right {
                    let text: Vec<u16> = shortened.encode_utf16().collect();
                    let text = caption_for_paint(&text);
                    DrawTextW(
                        item.hDC,
                        text.as_ptr(),
                        text.len() as i32,
                        &mut line_rect,
                        DT_LEFT
                            | DT_SINGLELINE
                            | DT_VCENTER
                            | DT_NOPREFIX
                            | if line.path {
                                DT_PATH_ELLIPSIS
                            } else {
                                DT_END_ELLIPSIS
                            },
                    );
                }
            }
            SelectObject(item.hDC, font);
        } else if matches!(role, Role::Workspace { .. } | Role::Choice { .. })
            && rect.right > rect.left
        {
            let (title, path) = label_text.split_once('\n').unwrap_or((&label_text, ""));
            let title: Vec<u16> = title.encode_utf16().collect();
            let path: Vec<u16> = path.trim_end_matches('\r').encode_utf16().collect();
            if !path.is_empty() && rect.bottom - rect.top >= pixel(44) {
                let mut title_rect = RECT {
                    top: rect.top + pixel(5),
                    bottom: rect.top + pixel(27),
                    ..rect
                };
                DrawTextW(
                    item.hDC,
                    title.as_ptr(),
                    title.len() as i32,
                    &mut title_rect,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | accelerator,
                );
                SelectObject(item.hDC, caption_font);
                SetTextColor(
                    item.hDC,
                    if palette.high_contrast && highlighted && !disabled {
                        text
                    } else {
                        palette.muted
                    },
                );
                let mut path_rect = RECT {
                    top: rect.top + pixel(27),
                    bottom: rect.bottom - pixel(5),
                    ..rect
                };
                DrawTextW(
                    item.hDC,
                    path.as_ptr(),
                    path.len() as i32,
                    &mut path_rect,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_PATH_ELLIPSIS | accelerator,
                );
                SelectObject(item.hDC, font);
            } else {
                DrawTextW(
                    item.hDC,
                    title.as_ptr(),
                    title.len() as i32,
                    &mut rect,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | accelerator,
                );
            }
        } else if !symbol && rect.right > rect.left {
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
        if let Some((_, before, horizontal)) = TAB_DROP
            .with(|slot| *slot.borrow())
            .filter(|(window, _, _)| *window == item.hwndItem as isize)
        {
            let stripe = if horizontal {
                let top = if before {
                    item.rcItem.top
                } else {
                    item.rcItem.bottom - pixel(3)
                };
                RECT {
                    top,
                    bottom: top + pixel(3),
                    ..item.rcItem
                }
            } else {
                let left = if before {
                    item.rcItem.left
                } else {
                    item.rcItem.right - pixel(3)
                };
                RECT {
                    left,
                    right: left + pixel(3),
                    ..item.rcItem
                }
            };
            fill(item.hDC, &stripe, palette.accent);
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
            let (brush, palette, dpi, headers, frame) = STATE.with(|slot| {
                let state = slot.borrow();
                (
                    state.resources.background,
                    state.palette,
                    state.dpi,
                    state
                        .pane_headers
                        .get(&(window as isize))
                        .cloned()
                        .unwrap_or_default(),
                    state.zoom_frames.get(&(window as isize)).copied(),
                )
            });
            let mut rect = RECT::default();
            unsafe {
                GetClientRect(window, &mut rect);
                FillRect(wparam as HDC, &rect, brush);
                if !headers.is_empty() {
                    let dc = wparam as HDC;
                    let saved = SaveDC(dc);
                    if saved != 0 {
                        IntersectClipRect(dc, rect.left, rect.top, rect.right, rect.bottom);
                        for (header, focused) in headers {
                            if header.width <= 0 || header.height <= 0 {
                                continue;
                            }
                            let area = RECT {
                                left: header.x,
                                top: header.y,
                                right: header.x.saturating_add(header.width),
                                bottom: header.y.saturating_add(header.height),
                            };
                            fill(dc, &area, palette.surface);
                            if focused {
                                fill(
                                    dc,
                                    &RECT {
                                        bottom: (area.top + ((2 * dpi as i32 + 48) / 96).max(1))
                                            .min(area.bottom),
                                        ..area
                                    },
                                    palette.accent,
                                );
                            }
                            fill(
                                dc,
                                &RECT {
                                    top: (area.bottom - ((dpi as i32 + 48) / 96).max(1))
                                        .max(area.top),
                                    ..area
                                },
                                palette.border,
                            );
                        }
                        RestoreDC(dc, saved);
                    }
                }
                if let Some((outer, inner)) = frame {
                    let right = outer.x + outer.width;
                    let bottom = outer.y + outer.height;
                    for (left, top, right, bottom) in [
                        (outer.x, outer.y, right, inner.y),
                        (outer.x, inner.y + inner.height, right, bottom),
                        (outer.x, inner.y, inner.x, inner.y + inner.height),
                        (
                            inner.x + inner.width,
                            inner.y,
                            right,
                            inner.y + inner.height,
                        ),
                    ] {
                        fill(
                            wparam as HDC,
                            &RECT {
                                left,
                                top,
                                right,
                                bottom,
                            },
                            palette.accent,
                        );
                    }
                }
            }
            Some(1)
        }
        WM_NCDESTROY => {
            STATE.with(|slot| {
                let mut state = slot.borrow_mut();
                state.pane_headers.remove(&(window as isize));
                state.zoom_frames.remove(&(window as isize));
            });
            None
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
    capture_impl(window, path, false)
}

/// Paint a real owned native subtree, including its custom WM_DRAWITEM handlers.
/// The root itself may be logically hidden; descendants retain normal visibility.
#[cfg(debug_assertions)]
pub(super) fn capture_subtree(window: HWND, path: &std::path::Path) -> anyhow::Result<Value> {
    capture_impl(window, path, true)
}

#[cfg(debug_assertions)]
fn capture_impl(window: HWND, path: &std::path::Path, subtree: bool) -> anyhow::Result<Value> {
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
    let root = unsafe { GetAncestor(window, GA_ROOT) };
    let mut root_owner = 0;
    let root_thread = unsafe { GetWindowThreadProcessId(root, &mut root_owner) };
    anyhow::ensure!(
        !root.is_null()
            && root_owner == owner
            && root_thread == thread
            && unsafe { IsWindowVisible(root) } == 0
            && (subtree || root == window),
        "native capture requires an owned hidden top-level ancestor"
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
    let registered = STATE.with(|slot| slot.borrow().controls.clone());
    let controls: Vec<_> = if subtree {
        // Native sibling z-order is top first. Reverse each sibling list so an
        // overlapping close button paints after the card beneath it.
        fn descendants(
            parent: HWND,
            depth: usize,
            remaining: &mut usize,
            result: &mut Vec<HWND>,
        ) -> anyhow::Result<()> {
            anyhow::ensure!(depth <= 64, "native capture subtree is too deep");
            let first = unsafe { GetWindow(parent, GW_CHILD) };
            let mut child = if first.is_null() {
                first
            } else {
                unsafe { GetWindow(first, GW_HWNDLAST) }
            };
            while !child.is_null() {
                anyhow::ensure!(*remaining > 0, "native capture exceeds 4096 child windows");
                *remaining -= 1;
                if unsafe { GetWindowLongPtrW(child, GWL_STYLE) } & WS_VISIBLE as isize != 0 {
                    result.push(child);
                    descendants(child, depth + 1, remaining, result)?;
                }
                child = unsafe { GetWindow(child, GW_HWNDPREV) };
            }
            Ok(())
        }
        let mut ordered = Vec::new();
        descendants(window, 0, &mut 4096, &mut ordered)?;
        ordered
            .into_iter()
            .filter_map(|hwnd| {
                registered
                    .get(&(hwnd as isize))
                    .map(|entry| (hwnd, entry.clone()))
            })
            .collect()
    } else {
        registered
            .into_iter()
            .map(|(hwnd, entry)| (hwnd as HWND, entry))
            .collect()
    };
    let mut rendered = Vec::new();
    for (child, entry) in controls {
        if (!subtree && unsafe { GetParent(child) } != window)
            || unsafe { GetWindowLongPtrW(child, GWL_STYLE) } & WS_VISIBLE as isize == 0
        {
            continue;
        }
        let mut child_owner = 0;
        anyhow::ensure!(
            unsafe { GetWindowThreadProcessId(child, &mut child_owner) } == thread
                && child_owner == owner,
            "native capture child left its owning UI thread"
        );
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
        let mut clip = rect;
        if subtree {
            let mut ancestor = unsafe { GetParent(child) };
            loop {
                anyhow::ensure!(!ancestor.is_null(), "native capture child was detached");
                let mut ancestor_owner = 0;
                anyhow::ensure!(
                    unsafe { GetWindowThreadProcessId(ancestor, &mut ancestor_owner) } == thread
                        && ancestor_owner == owner,
                    "native capture ancestor left its owning UI thread"
                );
                let mut bounds = RECT::default();
                checked(unsafe { GetClientRect(ancestor, &mut bounds) })?;
                unsafe {
                    MapWindowPoints(ancestor, window, (&mut bounds as *mut RECT).cast(), 2);
                }
                clip.left = clip.left.max(bounds.left);
                clip.top = clip.top.max(bounds.top);
                clip.right = clip.right.min(bounds.right);
                clip.bottom = clip.bottom.min(bounds.bottom);
                if ancestor == window {
                    break;
                }
                ancestor = unsafe { GetParent(ancestor) };
            }
            if clip.right <= clip.left || clip.bottom <= clip.top {
                continue;
            }
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
            IntersectClipRect(
                surface.dc,
                clip.left - rect.left,
                clip.top - rect.top,
                clip.right - rect.left,
                clip.bottom - rect.top,
            );
        }
        if entry.button.is_some()
            && unsafe { GetWindowLongPtrW(child, GWL_STYLE) } as u32 & 0xf == BS_OWNERDRAW as u32
        {
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
            if subtree {
                let painted = unsafe {
                    SendMessageW(
                        GetParent(child),
                        WM_DRAWITEM,
                        item.CtlID as WPARAM,
                        (&item as *const DRAWITEMSTRUCT) as LPARAM,
                    )
                };
                anyhow::ensure!(
                    painted != 0,
                    "production owner-draw handler did not paint the native button"
                );
            } else {
                anyhow::ensure!(draw_button(&item), "registered native button did not paint");
            }
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
        let kind = if entry.button.is_some() {
            "button"
        } else {
            match entry.control {
                ControlRole::Edit => "edit",
                ControlRole::Listbox => "listbox",
                ControlRole::EmptyState => "empty_state",
                _ => "static",
            }
        };
        rendered.push(
            json!({"handle":child as isize,"kind":kind,
            "x":rect.left,"y":rect.top,"width":child_width,"height":child_height,
            "clip":{"x":clip.left,"y":clip.top,"width":clip.right-clip.left,"height":clip.bottom-clip.top}}),
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
        "scope":if subtree {"production native subtree WM_DRAWITEM; may include cached owned WebView2 CapturePreview thumbnails; not a composed GPU or desktop capture"} else {"actual native chrome painters; WebView pixels omitted"},"format":"bmp-rgb32","subtree":subtree,"root_handle":window as usize}),
    )
}
