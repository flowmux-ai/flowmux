// SPDX-License-Identifier: GPL-3.0-or-later
//! Native agent strip. Buttons retain surface identity through status and layout changes.
use super::*;
use flowmux_core::{AgentBarItem, AGENT_BAR_ITEM_MAX_WIDTH_PX};
use windows_sys::Win32::UI::{Controls::SetScrollInfo, Input::KeyboardAndMouse::GetKeyState};

thread_local! { static TARGETS: RefCell<HashMap<isize,SurfaceId>> = RefCell::new(HashMap::new()); }
pub(super) fn target(window: HWND) -> Option<SurfaceId> {
    TARGETS.with(|targets| targets.borrow().get(&(window as isize)).copied())
}
pub(super) enum UiAction {
    Open(SurfaceId),
    Reveal(SurfaceId),
    Scroll(i32),
    To(i32),
    Wheel(i32),
}
fn emit(action: UiAction) {
    post(Event::AgentBar(action));
}
unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    if let Some(result) = chrome::message(window, message, w, l) {
        return result;
    }
    match message {
        WM_COMMAND => {
            if let Some(id) = TARGETS.with(|targets| targets.borrow().get(&l).copied()) {
                match (w >> 16) as u32 {
                    BN_CLICKED => emit(UiAction::Open(id)),
                    BN_SETFOCUS => emit(UiAction::Reveal(id)),
                    _ => {}
                }
            }
            0
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = (w >> 16) as u16 as i16 as i32;
            emit(UiAction::Wheel(if message == WM_MOUSEWHEEL {
                -delta
            } else {
                delta
            }));
            0
        }
        WM_HSCROLL => {
            let mut rect = RECT::default();
            GetClientRect(window, &mut rect);
            let line = (32 * GetDpiForWindow(window).max(96) as i32 + 48) / 96;
            match (w & 0xffff) as i32 {
                SB_LINELEFT => emit(UiAction::Scroll(-line)),
                SB_LINERIGHT => emit(UiAction::Scroll(line)),
                SB_PAGELEFT => emit(UiAction::Scroll(-rect.right)),
                SB_PAGERIGHT => emit(UiAction::Scroll(rect.right)),
                SB_LEFT => emit(UiAction::To(0)),
                SB_RIGHT => emit(UiAction::To(i32::MAX)),
                SB_THUMBPOSITION | SB_THUMBTRACK => {
                    let mut info = SCROLLINFO {
                        cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
                        fMask: SIF_TRACKPOS,
                        ..Default::default()
                    };
                    if GetScrollInfo(window, SB_HORZ, &mut info) != 0 {
                        emit(UiAction::To(info.nTrackPos));
                    }
                }
                _ => {}
            }
            0
        }
        _ => DefWindowProcW(window, message, w, l),
    }
}
fn create(parent: HWND, class: &str, text: &str, style: u32) -> anyhow::Result<HWND> {
    let window = unsafe {
        CreateWindowExW(
            if class == "flowmux.windows.agents" {
                WS_EX_CONTROLPARENT
            } else {
                0
            },
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | style,
            0,
            0,
            1,
            1,
            parent,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        )
    };
    checked((!window.is_null()) as i32)?;
    Ok(window)
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
            SWP_NOACTIVATE | SWP_NOZORDER | SWP_SHOWWINDOW,
        );
    }
}
fn dispose(window: HWND) {
    TARGETS.with(|targets| targets.borrow_mut().remove(&(window as isize)));
    chrome::unregister(window);
    unsafe {
        DestroyWindow(window);
    }
}
pub(super) struct Bar {
    pub window: HWND,
    viewport: HWND,
    label: HWND,
    items: Vec<(AgentBarItem, HWND)>,
    offset: i32,
    remainder: i32,
}
impl Bar {
    fn new(parent: HWND) -> anyhow::Result<Self> {
        unsafe {
            let name = wide("flowmux.windows.agents");
            let class = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: GetModuleHandleW(std::ptr::null()),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: name.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&class) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "cannot register agent bar"
            );
        }
        let mut bar = Self {
            window: create(parent, "flowmux.windows.agents", "Agents", WS_CLIPCHILDREN)?,
            viewport: std::ptr::null_mut(),
            label: std::ptr::null_mut(),
            items: vec![],
            offset: 0,
            remainder: 0,
        };
        bar.label = create(
            bar.window,
            "STATIC",
            "Agents",
            WS_VISIBLE | windows_sys::Win32::System::SystemServices::SS_CENTERIMAGE,
        )?;
        chrome::register_control(bar.label, chrome::ControlRole::Static);
        bar.viewport = create(
            bar.window,
            "flowmux.windows.agents",
            "",
            WS_VISIBLE | WS_CLIPCHILDREN | WS_HSCROLL,
        )?;
        Ok(bar)
    }
    fn update(&mut self, items: Vec<AgentBarItem>) -> anyhow::Result<()> {
        self.items.retain(|(old, window)| {
            if items.iter().any(|item| item.surface == old.surface) {
                true
            } else {
                dispose(*window);
                false
            }
        });
        for item in items {
            let window = if let Some((old, window)) = self
                .items
                .iter_mut()
                .find(|(old, _)| old.surface == item.surface)
            {
                *old = item.clone();
                *window
            } else {
                let window = create(
                    self.viewport,
                    "BUTTON",
                    "",
                    WS_VISIBLE | WS_TABSTOP | BS_OWNERDRAW as u32 | BS_NOTIFY as u32,
                )?;
                TARGETS.with(|targets| targets.borrow_mut().insert(window as isize, item.surface));
                chrome::register_button(window, chrome::Role::Button);
                self.items.push((item.clone(), window));
                window
            };
            workspaces::set_caption(
                window,
                &format!("{}\n{}", item.agent_name, item.status_text),
            );
        }
        Ok(())
    }
    fn layout(&mut self, rect: RECT) {
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96) as i32;
        let px = |n: i32| (n * dpi + 48) / 96;
        place(
            self.window,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
        );
        place(self.label, px(6), px(3), px(52), px(47));
        place(
            self.viewport,
            px(64),
            px(3),
            (rect.right - rect.left - px(70)).max(1),
            (rect.bottom - rect.top - px(6)).max(1),
        );
        self.scroll(self.offset);
    }
    fn scroll(&mut self, offset: i32) {
        let dpi = unsafe { GetDpiForWindow(self.viewport) }.max(96) as i32;
        let px = |n: i32| (n * dpi + 48) / 96;
        let step = px(AGENT_BAR_ITEM_MAX_WIDTH_PX as i32 + 4);
        let total = (self.items.len() as i32 * step - px(4)).max(0);
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(self.viewport, &mut rect);
        }
        self.offset = offset.clamp(0, (total - rect.right).max(0));
        let info = SCROLLINFO {
            cbSize: std::mem::size_of::<SCROLLINFO>() as u32,
            fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
            nMin: 0,
            nMax: (total - 1).max(0),
            nPage: rect.right.max(1) as u32,
            nPos: self.offset,
            ..Default::default()
        };
        unsafe {
            SetScrollInfo(self.viewport, SB_HORZ, &info, 1);
        }
        let mut previous = HWND_TOP;
        for (index, (_, window)) in self.items.iter().enumerate() {
            // Native keyboard traversal follows the same order as the cards.
            unsafe {
                SetWindowPos(
                    *window,
                    previous,
                    index as i32 * step - self.offset,
                    0,
                    px(AGENT_BAR_ITEM_MAX_WIDTH_PX as i32),
                    px(47),
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
            }
            previous = *window;
        }
        unsafe {
            InvalidateRect(self.window, std::ptr::null(), 1);
            InvalidateRect(self.viewport, std::ptr::null(), 1);
        }
    }
    fn reveal(&mut self, id: SurfaceId) {
        let Some(index) = self.items.iter().position(|(item, _)| item.surface == id) else {
            return;
        };
        let dpi = unsafe { GetDpiForWindow(self.viewport) }.max(96) as i32;
        let width = AGENT_BAR_ITEM_MAX_WIDTH_PX as i32;
        let left = index as i32 * (((width + 4) * dpi + 48) / 96);
        let right = left + (width * dpi + 48) / 96;
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(self.viewport, &mut rect);
        }
        self.scroll(if left < self.offset {
            left
        } else if right > self.offset + rect.right {
            (right - rect.right).min(left)
        } else {
            self.offset
        });
    }
    fn hit(&self, root: HWND, x: i32, y: i32) -> Option<(usize, HWND, bool)> {
        unsafe {
            let mut bounds = RECT::default();
            if GetClientRect(root, &mut bounds) == 0
                || x < 0
                || y < 0
                || x >= bounds.right
                || y >= bounds.bottom
            {
                return None;
            }
            let mut point = POINT { x, y };
            MapWindowPoints(root, self.viewport, &mut point, 1);
            if GetClientRect(self.viewport, &mut bounds) == 0
                || point.x < 0
                || point.y < 0
                || point.x >= bounds.right
                || point.y >= bounds.bottom
            {
                return None;
            }
            self.items
                .iter()
                .enumerate()
                .find_map(|(index, (_, window))| {
                    let mut rect = chrome::visible_control_rect(*window)?;
                    MapWindowPoints(
                        std::ptr::null_mut(),
                        self.viewport,
                        (&mut rect as *mut RECT).cast(),
                        2,
                    );
                    (point.x >= rect.left
                        && point.x < rect.right
                        && point.y >= rect.top
                        && point.y < rect.bottom)
                        .then_some((
                            index,
                            *window,
                            point.x < rect.left + (rect.right - rect.left) / 2,
                        ))
                })
        }
    }
    pub(super) fn handle_message(&self, message: &MSG, background: bool) -> bool {
        if unsafe { IsChild(self.window, message.hwnd) } == 0 {
            return false;
        }
        if message.message == WM_KEYDOWN
            && (background
                || !unsafe {
                    [0x11, 0x12, 0x5b, 0x5c]
                        .iter()
                        .any(|key| GetKeyState(*key) < 0)
                })
        {
            match message.wParam {
                0x24 => {
                    emit(UiAction::To(0));
                    return true;
                }
                0x23 => {
                    emit(UiAction::To(i32::MAX));
                    return true;
                }
                0x21 | 0x22 => {
                    let mut rect = RECT::default();
                    unsafe {
                        GetClientRect(self.viewport, &mut rect);
                    }
                    emit(UiAction::Scroll(if message.wParam == 0x21 {
                        -rect.right
                    } else {
                        rect.right
                    }));
                    return true;
                }
                0x0d | 0x20 => {
                    if let Some(id) = TARGETS
                        .with(|targets| targets.borrow().get(&(message.hwnd as isize)).copied())
                    {
                        emit(UiAction::Open(id));
                        return true;
                    }
                }
                _ => {}
            }
        }
        !background && unsafe { IsDialogMessageW(self.window, message) } != 0
    }
    pub(super) fn diagnostics(&self) -> Value {
        json!({"window":self.window as usize,"viewport":self.viewport as usize,"label":self.label as usize,"native_visible":unsafe {IsWindowVisible(self.window)!=0},"layout_visible":unsafe {GetWindowLongPtrW(self.window,GWL_STYLE) as u32&WS_VISIBLE!=0},"offset":self.offset,"items":self.items.iter().map(|(item,window)|json!({"surface":item.surface,"workspace":item.workspace,"pane":item.pane,"agent":item.agent_name,"status":item.status,"message":item.status_text,"color":item.color,"handle":*window as usize,"tooltip":chrome::tooltip_text(*window),"attention":chrome::attention(*window)})).collect::<Vec<_>>()})
    }
}
impl Drop for Bar {
    fn drop(&mut self) {
        for (_, window) in self.items.drain(..) {
            dispose(window);
        }
        for window in [self.label, self.viewport, self.window] {
            if !window.is_null() {
                dispose(window);
            }
        }
    }
}
impl App {
    pub(super) fn agent_bar_pointer(&mut self, pointer: &panes::Pointer) -> anyhow::Result<bool> {
        use panes::{Drag, Pointer};
        if let Pointer::AgentDown { surface, x, y } = *pointer {
            self.cancel_drag();
            if self.agent_presence(surface).is_some()
                && self.agent_bar.as_ref().is_some_and(|bar| {
                    bar.hit(self.window, x, y)
                        .is_some_and(|(index, _, _)| bar.items[index].0.surface == surface)
                })
            {
                self.drag = Some(Drag::Agent {
                    surface,
                    start_x: x,
                    start_y: y,
                    moved: false,
                });
                if !self.background_test {
                    unsafe {
                        SetCapture(self.window);
                    }
                }
            }
            return Ok(true);
        }
        let (Pointer::Move(x, y) | Pointer::Up(x, y)) = *pointer else {
            return Ok(false);
        };
        let Some(Drag::Agent {
            surface,
            start_x,
            start_y,
            moved,
        }) = self.drag
        else {
            return Ok(false);
        };
        if self.agent_presence(surface).is_none() || self.agent_bar.is_none() {
            self.cancel_drag();
            return Ok(true);
        }
        let moved = moved || panes::drag_moved(self.window, start_x, start_y, x, y);
        self.drag = Some(Drag::Agent {
            surface,
            start_x,
            start_y,
            moved,
        });
        let target = self
            .agent_bar
            .as_ref()
            .and_then(|bar| bar.hit(self.window, x, y));
        chrome::set_tab_drop(
            target
                .filter(|_| moved)
                .map(|(_, window, before)| (window, before)),
        );
        if matches!(pointer, Pointer::Up(..)) {
            self.cancel_drag();
            if let Some((index, window, before)) = target {
                if self::target(window).is_some_and(|id| self.agent_presence(id).is_some()) {
                    let bar = self.agent_bar.as_mut().unwrap();
                    if moved {
                        if let Some(source) = bar
                            .items
                            .iter()
                            .position(|(item, _)| item.surface == surface)
                        {
                            let boundary = index + usize::from(!before);
                            let destination = boundary - usize::from(source < boundary);
                            if source != destination {
                                let item = bar.items.remove(source);
                                bar.items.insert(destination, item);
                                bar.scroll(bar.offset);
                            }
                        }
                    } else if bar.items[index].0.surface == surface {
                        self.agent_bar_ui(UiAction::Open(surface))?;
                    }
                }
            }
        }
        Ok(true)
    }
    pub(super) fn refresh_agent_bar(&self) {
        let Some(bar) = &self.agent_bar else {
            return;
        };
        let attention: HashSet<_> = self
            .notifications
            .store
            .entries()
            .into_iter()
            .filter(|entry| {
                !entry.read && entry.level == flowmux_core::NotificationLevel::NeedsInput
            })
            .filter_map(|entry| entry.surface)
            .collect();
        let enabled = flowmux_core::AgentNotificationVisualFlags::for_unread(
            self.settings.terminal.agent_notification_target,
            false,
        )
        .agent_bar;
        let focused = self.current_surface();
        for (item, window) in &bar.items {
            chrome::set_role(
                *window,
                chrome::Role::Agent {
                    selected: focused == Some(item.surface),
                    color: chrome::color_ref(&item.color),
                    status: item.status,
                    seen: item.seen,
                    attention: enabled && attention.contains(&item.surface),
                },
            );
        }
    }
    pub(super) fn agent_bar_sync(&mut self) -> anyhow::Result<()> {
        let mut items = vec![];
        if self.settings.terminal.agent_bar_mode && !self.main_closed {
            for workspace in &self.workspaces {
                for (pane, _, tabs) in workspace.leaves() {
                    for tab in tabs {
                        if let Some(agent) = self.agent_presence(tab.id) {
                            items.push(AgentBarItem {
                                workspace: workspace.id,
                                pane,
                                surface: tab.id,
                                surface_label: tab.title.clone(),
                                agent_name: agent.name.clone(),
                                status: agent.status,
                                visual_status: flowmux_core::agent_bar_visual_status(agent.status)
                                    .unwrap(),
                                seen: agent.seen,
                                status_text: agent
                                    .status_text()
                                    .unwrap_or(agent.status.as_str())
                                    .into(),
                                color: workspace.color.clone().unwrap_or_else(|| {
                                    flowmux_core::agent_bar_color_for_surface(tab.id)
                                }),
                            });
                        }
                    }
                }
            }
        }
        if matches!(self.drag, Some(panes::Drag::Agent { surface, .. }) if !items.iter().any(|item| item.surface == surface))
        {
            self.cancel_drag();
        }
        if items.is_empty() {
            if let Some(bar) = self.agent_bar.take() {
                self.agent_bar_order = bar.items.iter().map(|(item, _)| item.surface).collect();
            }
            return Ok(());
        }
        let restore = self.agent_bar.is_none();
        if restore {
            self.agent_bar = Some(Bar::new(self.window)?);
        }
        let bar = self.agent_bar.as_mut().unwrap();
        bar.update(items)?;
        if restore {
            bar.items.sort_by_key(|(item, _)| {
                self.agent_bar_order
                    .iter()
                    .position(|id| *id == item.surface)
                    .unwrap_or(usize::MAX)
            });
        }
        self.refresh_agent_bar();
        Ok(())
    }
    pub(super) fn agent_bar_height(&self, width: i32, height: i32) -> i32 {
        let Some(bar) = &self.agent_bar else {
            return 0;
        };
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let px = |n: i32| (n * dpi as i32 + 48) / 96;
        let available = width - self.sidebar_width(width, dpi) - px(74);
        let overflow =
            bar.items.len() as i32 * px(AGENT_BAR_ITEM_MAX_WIDTH_PX as i32 + 4) - px(4) > available;
        (px(54)
            + if overflow {
                unsafe { GetSystemMetricsForDpi(SM_CYHSCROLL, dpi) }
            } else {
                0
            })
        .min((height - self.usage_height(height) - px(80)).max(0))
    }
    pub(super) fn agent_bar_layout(&mut self, client: RECT, sidebar: i32) {
        let height = self.agent_bar_height(client.right, client.bottom);
        let bottom = client.bottom - self.usage_height(client.bottom);
        if let Some(bar) = &mut self.agent_bar {
            bar.layout(RECT {
                left: sidebar,
                top: bottom - height,
                right: client.right,
                bottom,
            });
        }
    }
    pub(super) fn agent_bar_ui(&mut self, action: UiAction) -> anyhow::Result<()> {
        if self.main_closed
            || self.closing
            || self.close_accepted
            || self.close_request.is_some()
            || self.editor_barrier.is_some()
            || self.overview.is_open()
            || self.command_palette.is_open()
            || unsafe { IsWindowEnabled(self.window) } == 0
        {
            return Ok(());
        }
        if let UiAction::Open(id) = action {
            if self.agent_presence(id).is_some() {
                self.select(id)?;
                self.rebuild()?;
            }
        } else if let Some(bar) = &mut self.agent_bar {
            match action {
                UiAction::Reveal(id) => bar.reveal(id),
                UiAction::To(offset) => bar.scroll(offset),
                UiAction::Scroll(delta) => bar.scroll(bar.offset.saturating_add(delta)),
                UiAction::Wheel(delta) => {
                    let dpi = unsafe { GetDpiForWindow(bar.window) }.max(96) as i32;
                    let mut chars = 3u32;
                    unsafe {
                        SystemParametersInfoW(
                            SPI_GETWHEELSCROLLCHARS,
                            0,
                            (&mut chars as *mut u32).cast(),
                            0,
                        );
                    }
                    let amount = bar.remainder.saturating_add(
                        delta
                            .saturating_mul((32 * dpi + 48) / 96)
                            .saturating_mul(chars.min(100) as i32),
                    );
                    bar.remainder = amount % 120;
                    bar.scroll(bar.offset.saturating_add(amount / 120));
                }
                UiAction::Open(_) => {}
            }
        }
        Ok(())
    }
}
