// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned authentication window; callers retain the terminal and session.
use super::*;

#[derive(Clone, Copy)]
pub(super) enum UiAction {
    Close,
    Layout,
}

thread_local! {
    static ROUTES: RefCell<HashMap<isize, (SurfaceId, Uuid)>> = RefCell::new(HashMap::new());
}

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let action = match message {
        WM_CLOSE => Some(UiAction::Close),
        WM_SIZE | WM_MOVE => Some(UiAction::Layout),
        WM_DPICHANGED if l != 0 => {
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
            Some(UiAction::Layout)
        }
        WM_NCDESTROY => {
            ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
            None
        }
        _ => None,
    };
    if let Some(action) = action {
        let route = ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied());
        if let Some((surface, id)) = route {
            post(Event::SshAuthenticationUi(surface, id, action));
        }
        return 0;
    }
    chrome::message(window, message, w, l).unwrap_or_else(|| DefWindowProcW(window, message, w, l))
}

// Reparent or drop any terminal holder before this window is dropped.
pub(super) struct Window {
    pub(super) window: HWND,
    pub(super) id: Uuid,
    owner: HWND,
    background: bool,
    open: bool,
}

impl Window {
    pub(super) fn new(
        parent: HWND,
        title: &str,
        surface: SurfaceId,
        background: bool,
    ) -> anyhow::Result<Self> {
        let window = unsafe {
            anyhow::ensure!(
                IsWindow(parent) != 0,
                "SSH authentication owner is unavailable"
            );
            let class = wide("flowmux.windows.ssh.authentication");
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
                "Cannot register SSH authentication window"
            );
            let dpi = GetDpiForWindow(parent).max(96);
            let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
            let mut frame = RECT {
                left: 0,
                top: 0,
                right: (740 * dpi / 96) as i32,
                bottom: (360 * dpi / 96) as i32,
            };
            checked(AdjustWindowRectExForDpi(
                &mut frame,
                style,
                0,
                WS_EX_TOOLWINDOW,
                dpi,
            ))?;
            let mut owner = RECT::default();
            checked(GetWindowRect(parent, &mut owner))?;
            let width = frame.right - frame.left;
            let height = frame.bottom - frame.top;
            let handle = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                wide(title).as_ptr(),
                style,
                owner.left + (owner.right - owner.left - width) / 2,
                owner.top + (owner.bottom - owner.top - height) / 2,
                width,
                height,
                parent,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            checked((!handle.is_null()) as i32)?;
            handle
        };
        let id = Uuid::new_v4();
        ROUTES.with(|routes| routes.borrow_mut().insert(window as isize, (surface, id)));
        Ok(Self {
            window,
            id,
            owner: parent,
            background,
            open: false,
        })
    }

    pub(super) fn show(&mut self) {
        self.open = true;
        if !self.background {
            unsafe { ShowWindow(self.window, SW_SHOWNORMAL) };
        }
    }

    pub(super) fn hide(&mut self) {
        self.open = false;
        unsafe { ShowWindow(self.window, SW_HIDE) };
    }

    pub(super) fn area(&self) -> anyhow::Result<model::Rect> {
        let mut client = RECT::default();
        unsafe { checked(GetClientRect(self.window, &mut client))? };
        Ok(model::Rect {
            x: 0,
            y: 0,
            width: client.right.max(1),
            height: client.bottom.max(1),
        })
    }

    pub(super) fn theme(&self, theme: crate::settings::Theme) {
        chrome::window_theme(self.window, theme);
    }

    pub(super) fn status(&self) -> Value {
        json!({"id":self.id,"window":self.window as usize,"owner":self.owner as usize,
            "open":self.open,"native_visible":unsafe { IsWindowVisible(self.window) } != 0,
            "area":self.area().ok()})
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        ROUTES.with(|routes| routes.borrow_mut().remove(&(self.window as isize)));
        unsafe { DestroyWindow(self.window) };
    }
}
