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
        let content = model::Rect {
            x: sidebar + px(4),
            y: px(4),
            width: (client.right
                - sidebar
                - px(8)
                - self.files_dock_width(workspace, (client.right - sidebar - px(8)).max(1), scale))
            .max(1),
            height: (client.bottom - px(8)).max(1),
        };
        let mut geometry = model::Layout::default();
        model::partition(
            &self.workspaces[workspace].root,
            content,
            px(5),
            &mut geometry,
        );
        Ok((geometry, content))
    }

    pub(super) fn cancel_drag(&mut self) {
        self.drag = None;
        unsafe {
            if !self.background_test && GetCapture() == self.window {
                ReleaseCapture();
            }
        }
    }

    pub(super) fn pointer(&mut self, pointer: Pointer) -> anyhow::Result<()> {
        // Hidden verification never captures or changes the desktop pointer.
        if self.close_request.is_some() {
            self.cancel_drag();
            return Ok(());
        }
        match pointer {
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
