// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent native window for one retained surface; the App owns its lifecycle.
use super::*;

const TAB: usize = 100;
const CLOSE: usize = 101;
thread_local! {
    static ROUTES: RefCell<HashMap<isize, SurfaceId>> = RefCell::new(HashMap::new());
}

#[derive(Clone, Copy)]
pub(super) enum Signal {
    Layout,
    Moved,
    Activated,
    Close,
}

fn emit(window: HWND, signal: Signal) {
    let surface = ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied());
    if let Some(surface) = surface {
        post(Event::Detached(surface, signal));
    }
}

pub(super) struct Window {
    pub window: HWND,
    pub workspace: WorkspaceId,
    surface: SurfaceId,
    tab: HWND,
    close: HWND,
    tools: [HWND; 6],
}

impl Window {
    pub(super) fn new(
        surface: SurfaceId,
        workspace: WorkspaceId,
        title: &str,
    ) -> anyhow::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux.windows.detached");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..std::mem::zeroed()
            };
            anyhow::ensure!(
                RegisterClassW(&spec) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register detached surface window"
            );
            let window = CreateWindowExW(
                WS_EX_CONTROLPARENT,
                class.as_ptr(),
                wide(title).as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                1280,
                800,
                std::ptr::null_mut(), // Independent: main-window destruction must not own this HWND.
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            anyhow::ensure!(!window.is_null(), "cannot create detached surface window");
            let mut result = Self {
                window,
                workspace,
                surface,
                tab: std::ptr::null_mut(),
                close: std::ptr::null_mut(),
                tools: [std::ptr::null_mut(); 6],
            };
            ROUTES.with(|routes| routes.borrow_mut().insert(window as isize, surface));
            result.tab = result.button(
                title,
                TAB,
                chrome::Role::Tab {
                    selected: true,
                    focused: true,
                    kind: chrome::SurfaceIcon::Terminal,
                },
                true,
            )?;
            result.close = result.button("Close tab", CLOSE, chrome::Role::Tool, true)?;
            for (index, (label, role)) in [
                (
                    "Maximize pane",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::Maximize,
                        marked: false,
                    },
                ),
                (
                    "Split right",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::SplitRight,
                        marked: false,
                    },
                ),
                (
                    "Split down",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::SplitDown,
                        marked: false,
                    },
                ),
                ("+", chrome::Role::Tool),
                (
                    "New browser",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::Browser,
                        marked: false,
                    },
                ),
                (
                    "Pane actions",
                    chrome::Role::Icon {
                        kind: chrome::ChromeIcon::More,
                        marked: false,
                    },
                ),
            ]
            .into_iter()
            .enumerate()
            {
                result.tools[index] = result.button(label, CLOSE + 1 + index, role, false)?;
            }
            result.layout()?;
            Ok(result)
        }
    }

    fn button(
        &self,
        label: &str,
        id: usize,
        role: chrome::Role,
        enabled: bool,
    ) -> anyhow::Result<HWND> {
        let window = unsafe {
            CreateWindowExW(
                0,
                wide("BUTTON").as_ptr(),
                wide(label.replace('&', "&&")).as_ptr(),
                WS_CHILD
                    | WS_VISIBLE
                    | WS_TABSTOP
                    | BS_OWNERDRAW as u32
                    | if enabled { 0 } else { WS_DISABLED },
                0,
                0,
                1,
                1,
                self.window,
                id as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            )
        };
        anyhow::ensure!(!window.is_null(), "cannot create detached surface header");
        chrome::register_button(window, role);
        Ok(window)
    }

    pub(super) fn area(&self) -> anyhow::Result<model::Rect> {
        let mut rect = RECT::default();
        unsafe {
            checked(GetClientRect(self.window, &mut rect))?;
        }
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let bar = (28.0 * dpi as f64 / 96.0).round() as i32;
        Ok(model::Rect {
            x: 0,
            y: bar.min(rect.bottom),
            width: rect.right.max(1),
            height: (rect.bottom - bar).max(1),
        })
    }

    pub(super) fn layout(&self) -> anyhow::Result<()> {
        let mut client = RECT::default();
        unsafe {
            checked(GetClientRect(self.window, &mut client))?;
        }
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let px = |value: i32| (value as f64 * dpi as f64 / 96.0).round() as i32;
        let area = model::Rect {
            x: 0,
            y: 0,
            width: client.right,
            height: client.bottom.min(px(28)),
        };
        let header = workspaces::pane_header_layout(area, dpi);
        let width = header.tabs_width.min(px(190));
        let close_width = if width >= px(72) { px(22) } else { 0 };
        for (index, window) in self.tools.iter().enumerate() {
            place(*window, header.tools[index])?;
        }
        place(
            self.tab,
            (width > close_width).then_some(model::Rect {
                x: 0,
                y: 0,
                width: width - close_width,
                height: area.height,
            }),
        )?;
        place(
            self.close,
            (close_width > 0).then_some(model::Rect {
                x: width - close_width,
                y: 0,
                width: close_width,
                height: area.height,
            }),
        )?;
        Ok(())
    }

    pub(super) fn caption(&self, title: &str, kind: chrome::SurfaceIcon) {
        unsafe {
            SetWindowTextW(self.window, wide(title).as_ptr());
        }
        workspaces::set_caption(self.tab, title);
        chrome::set_role(
            self.tab,
            chrome::Role::Tab {
                selected: true,
                focused: true,
                kind,
            },
        );
    }

    pub(super) fn show(&self, background: bool) {
        unsafe {
            ShowWindow(
                self.window,
                if background {
                    SW_HIDE
                } else {
                    SW_SHOWNOACTIVATE
                },
            );
        }
    }

    pub(super) fn diagnostics(&self) -> Value {
        json!({"window_handle":self.window as usize,"workspace":self.workspace,"surface":self.surface,
            "owner":unsafe{GetWindow(self.window,GW_OWNER)} as usize,
            "native_visible":unsafe{IsWindowVisible(self.window)}!=0,"area":self.area().ok(),
            "tab":self.tab as usize,"close":self.close as usize,
            "tools":self.tools.map(|window| window as usize)})
    }
}

fn place(window: HWND, rect: Option<model::Rect>) -> anyhow::Result<()> {
    unsafe {
        if let Some(rect) = rect.filter(|rect| rect.width > 0 && rect.height > 0) {
            checked(SetWindowPos(
                window,
                std::ptr::null_mut(),
                rect.x,
                rect.y,
                rect.width,
                rect.height,
                SWP_NOZORDER | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            ))?;
        } else {
            ShowWindow(window, SW_HIDE);
        }
    }
    Ok(())
}

impl Drop for Window {
    fn drop(&mut self) {
        ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
        for window in [self.tab, self.close].into_iter().chain(self.tools) {
            if !window.is_null() {
                chrome::unregister(window);
            }
        }
        // Caller drops/reparents the surface's WebView and holder before this.
        unsafe {
            DestroyWindow(self.window);
        }
    }
}

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(result) = chrome::message(window, message, w, l) {
        return result;
    }
    match message {
        WM_CLOSE => {
            emit(window, Signal::Close);
            0
        }
        WM_SIZE => {
            emit(window, Signal::Layout);
            0
        }
        WM_MOVE => {
            emit(window, Signal::Moved);
            0
        }
        WM_ACTIVATE => {
            if w as u16 != WA_INACTIVE as u16 {
                emit(window, Signal::Activated);
            }
            0
        }
        WM_DPICHANGED => {
            if l != 0 {
                let rect = &*(l as *const RECT);
                SetWindowPos(
                    window,
                    std::ptr::null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            emit(window, Signal::Layout);
            0
        }
        WM_GETMINMAXINFO => {
            let info = &mut *(l as *mut MINMAXINFO);
            let dpi = GetDpiForWindow(window).max(96) as i32;
            info.ptMinTrackSize.x = 420 * dpi / 96;
            info.ptMinTrackSize.y = 240 * dpi / 96;
            0
        }
        WM_COMMAND => {
            if (w >> 16) as u32 == BN_CLICKED
                && l != 0
                && GetParent(l as HWND) == window
                && GetDlgCtrlID(l as HWND) == (w & 0xffff) as i32
            {
                match w & 0xffff {
                    CLOSE => emit(window, Signal::Close),
                    TAB => emit(window, Signal::Activated),
                    _ => {}
                }
            }
            0
        }
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(window, &mut paint);
            let mut rect = RECT::default();
            GetClientRect(window, &mut rect);
            let brush = CreateSolidBrush(chrome::palette().background);
            FillRect(dc, &rect, brush);
            DeleteObject(brush);
            EndPaint(window, &paint);
            0
        }
        WM_NCDESTROY => {
            ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
            DefWindowProcW(window, message, w, l)
        }
        // WM_DESTROY deliberately does not post WM_QUIT: this window owns one surface.
        _ => DefWindowProcW(window, message, w, l),
    }
}
