// SPDX-License-Identifier: GPL-3.0-or-later
//! Pane operations retain every terminal view/session. Window callbacks only
//! queue pointer events; WebView calls stay on the outer message loop.
use super::*;

thread_local! { static SIDEBAR: RefCell<(i32, Option<model::Rect>)> = const { RefCell::new((0, None)) }; }
pub(super) fn cached_sidebar_width() -> i32 {
    SIDEBAR.with(|slot| slot.borrow().0)
}
pub(super) fn cache_sidebar(width: i32, height: i32, gutter: i32, desktop: bool) {
    SIDEBAR.with(|slot| {
        *slot.borrow_mut() = (
            width,
            desktop.then_some(model::Rect {
                x: width,
                y: 0,
                width: gutter,
                height,
            }),
        )
    });
}
thread_local! { static CURSOR_DIVIDERS: RefCell<Vec<model::Divider>> = const { RefCell::new(Vec::new()) }; }

pub(super) fn cursor_dividers(dividers: &[model::Divider]) {
    CURSOR_DIVIDERS.with(|slot| *slot.borrow_mut() = dividers.to_vec());
}

pub(super) unsafe fn set_cursor(window: HWND) -> bool {
    let mut point = POINT::default();
    if GetCursorPos(&mut point) == 0 || ScreenToClient(window, &mut point) == 0 {
        return false;
    }
    if SIDEBAR.with(|slot| {
        slot.borrow()
            .1
            .is_some_and(|r| r.contains(point.x, point.y))
    }) {
        divider_cursor(SplitDirection::Vertical);
        return true;
    }
    CURSOR_DIVIDERS.with(|slot| {
        if let Some(divider) = slot
            .borrow()
            .iter()
            .find(|d| d.bounds.contains(point.x, point.y))
        {
            divider_cursor(divider.direction);
            true
        } else {
            false
        }
    })
}

unsafe fn divider_cursor(direction: SplitDirection) {
    SetCursor(LoadCursorW(
        std::ptr::null_mut(),
        if direction == SplitDirection::Vertical {
            IDC_SIZEWE
        } else {
            IDC_SIZENS
        },
    ));
}

pub(super) enum Pointer {
    Down(i32, i32),
    TabDown {
        pane: PaneId,
        surface: SurfaceId,
        x: i32,
        y: i32,
        double_click: bool,
    },
    WorkspaceDown {
        workspace: WorkspaceId,
        x: i32,
        y: i32,
    },
    Move(i32, i32),
    Up(i32, i32),
    Cancel,
}

#[derive(Clone, Copy)]
pub(super) enum Drag {
    Pane {
        divider: model::Divider,
        offset: i32,
    },
    Sidebar {
        offset: i32,
        start_x: i32,
        moved: bool,
    },
    Tab {
        workspace: WorkspaceId,
        pane: PaneId,
        surface: SurfaceId,
        start_x: i32,
        start_y: i32,
        moved: bool,
    },
    Workspace {
        workspace: WorkspaceId,
        start_x: i32,
        start_y: i32,
        moved: bool,
    },
}

struct TabDrop {
    pane: PaneId,
    index: usize,
    marker: Option<(HWND, bool)>,
    split: Option<(SplitDirection, model::Rect)>,
}

impl App {
    pub(super) fn sidebar_width(&self, client_width: i32, dpi: u32) -> i32 {
        let scale = dpi.max(96) as f64 / 96.0;
        let preferred = (self.sidebar_width_dip as f64 * scale).round() as i32;
        preferred.min((client_width - (320.0 * scale).round() as i32).max(0))
    }
    pub(super) fn geometry(
        &self,
        workspace: usize,
    ) -> anyhow::Result<(model::Layout, model::Rect)> {
        let mut client = RECT::default();
        unsafe {
            checked(GetClientRect(self.window, &mut client))?;
        }
        let scale = unsafe { GetDpiForWindow(self.window) }.max(96) as f64 / 96.0;
        let px = |value: i32| (value as f64 * scale).round() as i32;
        let sidebar = self.sidebar_width(client.right, unsafe { GetDpiForWindow(self.window) });
        let ssh_bar = self.ssh_toolbar_height(workspace, client.right);
        let available = (client.right - sidebar - px(8)).max(1);
        let files_width = self.files_dock_width(workspace, available, scale);
        let worktrees_width = self.worktrees_width(available - files_width, scale);
        let content = model::Rect {
            x: sidebar + px(4),
            y: px(4) + ssh_bar,
            width: (available - files_width - worktrees_width).max(1),
            height: (client.bottom - px(8) - ssh_bar).max(1),
        };
        let mut geometry = model::Layout::default();
        if let Some(workspace) = self.workspaces.get(workspace) {
            model::partition(&workspace.root, content, px(5), &mut geometry);
        }
        Ok((geometry, content))
    }

    pub(super) fn cancel_drag(&mut self) {
        self.drag = None;
        chrome::set_tab_drop(None);
        if let Some(preview) = &mut self.drop_preview {
            preview.hide();
        }
        unsafe {
            if !self.background_test && GetCapture() == self.window {
                ReleaseCapture();
            }
        }
    }

    pub(super) fn pointer(&mut self, pointer: Pointer) -> anyhow::Result<()> {
        // Hidden verification never captures or changes the desktop pointer.
        if self.close_request.is_some()
            || self.close_accepted
            || self.closing
            || self.editor_barrier.is_some()
            || self.overview.is_open()
            || self.command_palette.is_open()
            || unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(self.window)
            } == 0
        {
            self.cancel_drag();
            return Ok(());
        }
        match pointer {
            Pointer::WorkspaceDown { workspace, x, y } => {
                self.cancel_drag();
                if !self
                    .workspaces
                    .iter()
                    .any(|candidate| candidate.id == workspace)
                    || !self.controls.iter().any(|control| {
                        matches!(control.action, Action::Workspace(id) if id == workspace)
                            && self
                                .tab_control_rect(control.hwnd)
                                .is_some_and(|rect| rect.contains(x, y))
                    })
                {
                    return Ok(());
                }
                self.drag = Some(Drag::Workspace {
                    workspace,
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
            Pointer::TabDown {
                pane,
                surface,
                x,
                y,
                double_click,
            } => {
                self.cancel_drag();
                let valid_source = self.locate(surface).is_some_and(|(workspace, source, _)| {
                    workspace == self.active_workspace && source == pane
                });
                let visible_source = self.controls.iter().any(|control| {
                    matches!(control.action, Action::Tab(p, s) if p == pane && s == surface)
                        && self
                            .tab_control_rect(control.hwnd)
                            .is_some_and(|r| r.contains(x, y))
                });
                if !valid_source || !visible_source {
                    return Ok(());
                }
                if double_click {
                    self.select(surface)?;
                    self.rebuild()?;
                    return self.edit_metadata(workspaces::EditTarget::TabName(surface));
                }
                self.drag = Some(Drag::Tab {
                    workspace: self.workspace().id,
                    pane,
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
            Pointer::Down(x, y) => {
                let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
                let mut client = RECT::default();
                unsafe {
                    checked(GetClientRect(self.window, &mut client))?;
                }
                let sidebar = self.sidebar_width(client.right, dpi);
                let gutter = (4.0 * dpi as f64 / 96.0).round() as i32;
                if x >= sidebar && x < sidebar + gutter && y >= 0 && y < client.bottom {
                    self.drag = Some(Drag::Sidebar {
                        offset: x - sidebar,
                        start_x: x,
                        moved: false,
                    });
                    if !self.background_test {
                        unsafe {
                            SetCapture(self.window);
                            divider_cursor(SplitDirection::Vertical);
                        }
                    }
                } else if let Some(divider) = self
                    .pane_layout
                    .dividers
                    .iter()
                    .find(|d| d.bounds.contains(x, y))
                    .copied()
                {
                    self.drag = Some(Drag::Pane {
                        divider,
                        offset: divider.coordinate(x, y)
                            - divider.coordinate(divider.bounds.x, divider.bounds.y),
                    });
                    if !self.background_test {
                        unsafe {
                            SetCapture(self.window);
                            divider_cursor(divider.direction);
                        }
                    }
                }
            }
            Pointer::Move(x, y) | Pointer::Up(x, y) => {
                if let Some(Drag::Workspace {
                    workspace,
                    start_x,
                    start_y,
                    moved,
                }) = self.drag
                {
                    let Some(source) = self
                        .main_workspace_indices()
                        .iter()
                        .position(|i| self.workspaces[*i].id == workspace)
                    else {
                        self.cancel_drag();
                        return Ok(());
                    };
                    let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
                    let moved = moved
                        || (i64::from(x) - i64::from(start_x)).abs()
                            >= i64::from(unsafe { GetSystemMetricsForDpi(SM_CXDRAG, dpi) }.max(1))
                        || (i64::from(y) - i64::from(start_y)).abs()
                            >= i64::from(unsafe { GetSystemMetricsForDpi(SM_CYDRAG, dpi) }.max(1));
                    self.drag = Some(Drag::Workspace {
                        workspace,
                        start_x,
                        start_y,
                        moved,
                    });
                    let target = moved.then(|| self.workspace_drop_target(x, y)).flatten();
                    chrome::set_workspace_drop(target.map(|(window, _, before)| (window, before)));
                    if matches!(pointer, Pointer::Up(..)) {
                        self.cancel_drag();
                        if moved {
                            if let Some((_, boundary, _)) = target {
                                let index = boundary.saturating_sub(usize::from(source < boundary));
                                if index != source {
                                    self.workspace_command(
                                        WorkspaceOp::Reorder {
                                            workspace: workspace.0,
                                            index,
                                        },
                                        None,
                                    )?;
                                }
                            }
                        } else if self.controls.iter().any(|control| {
                            matches!(control.action, Action::Workspace(id) if id == workspace)
                                && self
                                    .tab_control_rect(control.hwnd)
                                    .is_some_and(|rect| rect.contains(x, y))
                        }) {
                            self.action(Action::Workspace(workspace))?;
                        }
                    }
                    return Ok(());
                }
                if let Some(Drag::Tab {
                    workspace,
                    pane,
                    surface,
                    start_x,
                    start_y,
                    moved,
                }) = self.drag
                {
                    let current = self.locate(surface).is_some_and(|(index, source, _)| {
                        index == self.active_workspace
                            && self.workspaces[index].id == workspace
                            && source == pane
                    });
                    if !current {
                        self.cancel_drag();
                        return Ok(());
                    }
                    let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
                    let moved = moved
                        || (i64::from(x) - i64::from(start_x)).abs()
                            >= i64::from(unsafe { GetSystemMetricsForDpi(SM_CXDRAG, dpi) }.max(1))
                        || (i64::from(y) - i64::from(start_y)).abs()
                            >= i64::from(unsafe { GetSystemMetricsForDpi(SM_CYDRAG, dpi) }.max(1));
                    self.drag = Some(Drag::Tab {
                        workspace,
                        pane,
                        surface,
                        start_x,
                        start_y,
                        moved,
                    });
                    let target = moved.then(|| self.tab_drop_target(x, y)).flatten();
                    chrome::set_tab_drop(target.as_ref().and_then(|target| target.marker));
                    if let Some((direction, rect)) = target.as_ref().and_then(|target| target.split)
                    {
                        if self.drop_preview.is_none() {
                            self.drop_preview = Some(chrome::DropPreview::new(self.window)?);
                        }
                        self.drop_preview.as_mut().unwrap().show(
                            target.as_ref().unwrap().pane,
                            direction,
                            rect,
                            self.background_test,
                        )?;
                    } else if let Some(preview) = &mut self.drop_preview {
                        preview.hide();
                    }
                    if matches!(pointer, Pointer::Up(..)) {
                        self.cancel_drag();
                        if moved {
                            if let Some(target) = target {
                                if let Some((direction, _)) = target.split {
                                    self.active_workspace = model::split_move_surface(
                                        &mut self.workspaces,
                                        surface,
                                        target.pane,
                                        direction,
                                    )?;
                                    self.zoomed = None;
                                    return self.rebuild();
                                }
                                let source_index = self
                                    .workspace()
                                    .leaves()
                                    .into_iter()
                                    .find(|(id, _, _)| *id == pane)
                                    .and_then(|(_, _, tabs)| {
                                        tabs.iter().position(|tab| tab.id == surface)
                                    });
                                let index = target.index.saturating_sub(usize::from(
                                    target.pane == pane
                                        && source_index.is_some_and(|source| source < target.index),
                                ));
                                if target.pane != pane || source_index != Some(index) {
                                    self.move_tab(surface, target.pane, index)?;
                                }
                            } else {
                                let mut point = POINT { x, y };
                                let mut rect = RECT::default();
                                let outside = unsafe {
                                    ClientToScreen(self.window, &mut point) != 0
                                        && GetWindowRect(self.window, &mut rect) != 0
                                        && (point.x < rect.left
                                            || point.x >= rect.right
                                            || point.y < rect.top
                                            || point.y >= rect.bottom)
                                };
                                if outside {
                                    self.detach_tab(surface)?;
                                }
                            }
                        } else if self.controls.iter().any(|control| {
                            matches!(control.action, Action::Tab(p, s) if p == pane && s == surface)
                                && self
                                    .tab_control_rect(control.hwnd)
                                    .is_some_and(|r| r.contains(x, y))
                        }) {
                            self.select(surface)?;
                            self.rebuild()?;
                        }
                    }
                    return Ok(());
                }
                if let Some(drag) = self.drag {
                    let direction = match drag {
                        Drag::Pane { divider, offset } => {
                            if let Some(ratio) = divider.drag_ratio(x, y, offset) {
                                self.workspace_mut().resize(divider.split, ratio)?;
                                self.layout()?;
                            }
                            divider.direction
                        }
                        Drag::Sidebar {
                            offset,
                            start_x,
                            moved,
                        } => {
                            // A click on a temporarily clamped gutter must not replace
                            // the preferred width with the smaller displayed width.
                            if moved || x != start_x {
                                self.drag = Some(Drag::Sidebar {
                                    offset,
                                    start_x,
                                    moved: true,
                                });
                                let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
                                self.sidebar_width_dip =
                                    (((x - offset) as f64 * 96.0 / dpi as f64).round().max(0.0)
                                        as u32)
                                        .clamp(
                                            crate::state::MIN_SIDEBAR_WIDTH,
                                            crate::state::MAX_SIDEBAR_WIDTH,
                                        );
                                self.layout()?;
                            }
                            SplitDirection::Vertical
                        }
                        Drag::Tab { .. } | Drag::Workspace { .. } => {
                            unreachable!("item drag handled above")
                        }
                    };
                    if !self.background_test {
                        unsafe {
                            divider_cursor(direction);
                        }
                    }
                }
                if matches!(pointer, Pointer::Up(..)) {
                    self.cancel_drag();
                }
            }
            Pointer::Cancel => self.cancel_drag(),
        }
        Ok(())
    }

    fn workspace_drop_target(&self, x: i32, y: i32) -> Option<(HWND, usize, bool)> {
        self.controls.iter().find_map(|control| {
            let Action::Workspace(workspace) = control.action else {
                return None;
            };
            let rect = self.tab_control_rect(control.hwnd)?;
            if !rect.contains(x, y) {
                return None;
            }
            let index = self
                .main_workspace_indices()
                .iter()
                .position(|i| self.workspaces[*i].id == workspace)?;
            let before = y < rect.y + rect.height / 2;
            Some((control.hwnd, index + usize::from(!before), before))
        })
    }

    // Check the child's own WS_VISIBLE bit: hidden test hosts deliberately keep
    // their top-level window hidden while laying out the same visible controls.
    fn tab_control_rect(&self, window: HWND) -> Option<model::Rect> {
        unsafe {
            if GetParent(window) != self.window
                || GetWindowLongPtrW(window, GWL_STYLE) as u32 & WS_VISIBLE == 0
            {
                return None;
            }
            let mut rect = RECT::default();
            let mut client = RECT::default();
            if GetWindowRect(window, &mut rect) == 0 || GetClientRect(self.window, &mut client) == 0
            {
                return None;
            }
            let mut top_left = POINT {
                x: rect.left,
                y: rect.top,
            };
            let mut bottom_right = POINT {
                x: rect.right,
                y: rect.bottom,
            };
            if ScreenToClient(self.window, &mut top_left) == 0
                || ScreenToClient(self.window, &mut bottom_right) == 0
            {
                return None;
            }
            (top_left.x >= 0
                && top_left.y >= 0
                && bottom_right.x <= client.right
                && bottom_right.y <= client.bottom
                && bottom_right.x > top_left.x
                && bottom_right.y > top_left.y)
                .then_some(model::Rect {
                    x: top_left.x,
                    y: top_left.y,
                    width: bottom_right.x - top_left.x,
                    height: bottom_right.y - top_left.y,
                })
        }
    }

    fn tab_drop_target(&self, x: i32, y: i32) -> Option<TabDrop> {
        let mut client = RECT::default();
        if unsafe { GetClientRect(self.window, &mut client) } == 0
            || x < 0
            || y < 0
            || x >= client.right
            || y >= client.bottom
        {
            return None;
        }
        for control in &self.controls {
            let Some(mut rect) = self.tab_control_rect(control.hwnd) else {
                continue;
            };
            match control.action {
                Action::Tab(pane, surface) => {
                    let close = self.controls.iter().find(|candidate| matches!(candidate.action, Action::TabClose(p, s) if p == pane && s == surface))
                        .and_then(|candidate| self.tab_control_rect(candidate.hwnd).map(|rect| (candidate.hwnd, rect)));
                    if let Some((_, close)) = close {
                        rect.width = (close.x + close.width - rect.x).max(rect.width);
                    }
                    if !rect.contains(x, y) {
                        continue;
                    }
                    let before = x < rect.x + rect.width / 2;
                    let index = self
                        .workspace()
                        .leaves()
                        .into_iter()
                        .find(|(id, _, _)| *id == pane)
                        .and_then(|(_, _, tabs)| tabs.iter().position(|tab| tab.id == surface))?;
                    return Some(TabDrop {
                        pane,
                        index: index + usize::from(!before),
                        split: None,
                        marker: Some((
                            if before {
                                control.hwnd
                            } else {
                                close.map_or(control.hwnd, |(hwnd, _)| hwnd)
                            },
                            before,
                        )),
                    });
                }
                Action::Workspace(id) if rect.contains(x, y) => {
                    let workspace = self
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.id == id)?;
                    return Some(TabDrop {
                        pane: workspace.focused,
                        index: workspace.root.surface_count(workspace.focused)?,
                        marker: Some((control.hwnd, false)),
                        split: None,
                    });
                }
                _ => {}
            }
        }
        let bar =
            (28.0 * unsafe { GetDpiForWindow(self.window) }.max(96) as f64 / 96.0).round() as i32;
        self.pane_layout.panes.iter().find_map(|(pane, rect)| {
            let body = model::Rect { y: rect.y + bar, height: rect.height - bar, ..*rect };
            if !body.contains(x, y) { return None; }
            // Match Linux: lower half splits down, upper-right splits right,
            // upper-left appends a tab. The preview covers the new sibling.
            let split = if y >= body.y + body.height / 2 {
                Some((SplitDirection::Horizontal, model::Rect {
                    y: body.y + body.height / 2, height: body.height - body.height / 2, ..body
                }))
            } else if x >= body.x + body.width / 2 {
                Some((SplitDirection::Vertical, model::Rect {
                    x: body.x + body.width / 2, width: body.width - body.width / 2, ..body
                }))
            } else { None };
            if split.is_some() && (body.width < 2 || body.height < 2 ||
                matches!(self.drag, Some(Drag::Tab { pane: source, .. }) if source == *pane)
                    && self.workspace().root.surface_count(*pane) == Some(1)) {
                return None;
            }
            Some(TabDrop {
                pane: *pane,
                index: self.workspace().root.surface_count(*pane).unwrap_or(0),
                split,
                marker: if split.is_some() { None } else { self.controls.iter()
                    .filter(|control| matches!(control.action, Action::Tab(id, _) | Action::TabClose(id, _) if id == *pane))
                    .filter_map(|control| self.tab_control_rect(control.hwnd).map(|rect| (control.hwnd, rect)))
                    .max_by_key(|(_, rect)| rect.x + rect.width)
                    .map(|(window, _)| (window, false)) },
            })
        })
    }

    pub(super) fn resize_pane(
        &mut self,
        target: PaneId,
        ratio: f32,
    ) -> anyhow::Result<(PaneId, f32)> {
        let workspace = self
            .workspaces
            .iter()
            .position(|ws| matches!(&ws.root, flowmux_core::Pane::Leaf { id, .. } | flowmux_core::Pane::Split { id, .. } if *id == target) || ws.root.parent_split_id(target).is_some())
            .context("pane or split not found")?;
        let result = self.workspaces[workspace].resize(target, ratio)?;
        self.cancel_drag();
        if workspace == self.active_workspace {
            self.zoomed = None;
            self.layout()?;
        }
        Ok(result)
    }

    pub(super) fn focus_direction(
        &mut self,
        source: SurfaceId,
        direction: FocusDirection,
    ) -> anyhow::Result<Option<PaneId>> {
        let (workspace, pane, _) = self.locate(source).context("source surface not found")?;
        let (geometry, _) = self.geometry(workspace)?;
        let target = model::neighbor(&geometry.panes, pane, direction);
        if let Some(target) = target {
            let surface = self.workspaces[workspace]
                .root
                .active_surface_id(target)
                .context("destination pane not found")?;
            self.select(surface)?;
            self.rebuild()?;
        }
        Ok(target)
    }

    pub(super) fn toggle_zoom(&mut self, surface: SurfaceId) -> anyhow::Result<()> {
        self.ensure_attached(surface)?;
        self.select(surface)?;
        self.cancel_drag();
        let pane = self.workspace().focused;
        self.zoomed = if self.zoomed == Some(pane) || self.workspace().leaves().len() == 1 {
            None
        } else {
            Some(pane)
        };
        // Selection may activate another workspace/tab, so rebuild its native controls.
        self.rebuild()
    }
}
