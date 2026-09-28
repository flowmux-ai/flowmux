// SPDX-License-Identifier: GPL-3.0-or-later
//! Stable native parent for a surface's WebView and optional browser toolbar.
use super::*;

pub struct Host {
    pub window: HWND,
}

impl Host {
    pub fn new(parent: HWND) -> anyhow::Result<Self> {
        unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let class = wide("flowmux.windows.surface");
            let spec = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..WNDCLASSW::default()
            };
            if RegisterClassW(&spec) == 0 {
                anyhow::ensure!(
                    GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                    "cannot register surface container"
                );
            }
            let window = CreateWindowExW(
                0,
                class.as_ptr(),
                wide("flowmux surface").as_ptr(),
                WS_CHILD | WS_CLIPCHILDREN | WS_CLIPSIBLINGS,
                0,
                0,
                1,
                1,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            checked((!window.is_null()) as i32)?;
            Ok(Self { window })
        }
    }

    pub fn layout(&self, area: Option<model::Rect>, background: bool) -> anyhow::Result<()> {
        unsafe {
            if let Some(area) = area {
                checked(SetWindowPos(
                    self.window,
                    std::ptr::null_mut(),
                    area.x,
                    area.y,
                    area.width.max(1),
                    area.height.max(1),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                ))?;
            }
            // Avoid hide/show cycles on unchanged surfaces: they can interrupt IME.
            let show = area.is_some() && !background;
            let shown = GetWindowLongW(self.window, GWL_STYLE) as u32 & WS_VISIBLE != 0;
            if shown != show {
                ShowWindow(self.window, if show { SW_SHOWNA } else { SW_HIDE });
            }
        }
        Ok(())
    }

    /// Preserve the IPC geometry contract in the current top-level client space.
    pub fn bounds(&self) -> Option<model::Rect> {
        unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(self.window, &mut rect) == 0 {
                return None;
            }
            let mut origin = POINT {
                x: rect.left,
                y: rect.top,
            };
            let root = GetAncestor(self.window, GA_ROOT);
            if root.is_null() || ScreenToClient(root, &mut origin) == 0 {
                return None;
            }
            Some(model::Rect {
                x: origin.x,
                y: origin.y,
                width: rect.right - rect.left,
                height: rect.bottom - rect.top,
            })
        }
    }

    pub fn diagnostics(&self) -> Value {
        json!({"window":self.window as usize,
            "parent":unsafe { GetParent(self.window) } as usize,
            "root":unsafe { GetAncestor(self.window, GA_ROOT) } as usize,
            "bounds":self.bounds(),
            "native_visible":unsafe { IsWindowVisible(self.window) } != 0})
    }

    pub fn view_bounds(&self, view: &WebView) -> Option<model::Rect> {
        let holder = self.bounds()?;
        let rect = view.bounds().ok()?;
        let position = rect.position.to_physical::<i32>(1.0);
        let size = rect.size.to_physical::<i32>(1.0);
        Some(model::Rect {
            x: holder.x + position.x,
            y: holder.y + position.y,
            width: size.width,
            height: size.height,
        })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.window);
        }
    }
}

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    match message {
        WM_ERASEBKGND => 1,
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
        _ => DefWindowProcW(window, message, w, l),
    }
}
