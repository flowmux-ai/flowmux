// SPDX-License-Identifier: GPL-3.0-or-later
//! Pane operations retain every terminal view/session. Window callbacks only
//! queue pointer events; WebView calls stay on the outer message loop.
use super::*;

thread_local! { static CURSOR_DIVIDERS: RefCell<Vec<model::Divider>> = const { RefCell::new(Vec::new()) }; }

pub(super) fn cursor_dividers(dividers: &[model::Divider]) {
    CURSOR_DIVIDERS.with(|slot| *slot.borrow_mut() = dividers.to_vec());
}

pub(super) unsafe fn set_cursor(window: HWND) -> bool {
    let mut point = POINT::default();
    if GetCursorPos(&mut point) == 0 || ScreenToClient(window, &mut point) == 0 {
        return false;
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
pub(super) struct Drag {
    divider: model::Divider,
    offset: i32,
}

impl App {
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
        let sidebar = px(185).min((client.right / 3).max(0));
        let content = model::Rect {
            x: sidebar + px(4),
            y: px(4),
            width: (client.right - sidebar - px(8)).max(1),
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
            if GetCapture() == self.window {
                ReleaseCapture();
            }
        }
    }

    pub(super) fn pointer(&mut self, pointer: Pointer) -> anyhow::Result<()> {
        // Hidden verification never captures or changes the desktop pointer.
        if self.background_test || self.close_request.is_some() {
            self.cancel_drag();
            return Ok(());
        }
        match pointer {
            Pointer::Down(x, y) => {
                if let Some(divider) = self
                    .pane_layout
                    .dividers
                    .iter()
                    .find(|d| d.bounds.contains(x, y))
                    .copied()
                {
                    self.drag = Some(Drag {
                        divider,
                        offset: divider.coordinate(x, y)
                            - divider.coordinate(divider.bounds.x, divider.bounds.y),
                    });
                    unsafe {
                        SetCapture(self.window);
                        divider_cursor(divider.direction);
                    }
                }
            }
            Pointer::Move(x, y) | Pointer::Up(x, y) => {
                if let Some(drag) = self.drag {
                    if let Some(ratio) = drag.divider.drag_ratio(x, y, drag.offset) {
                        self.workspace_mut().resize(drag.divider.split, ratio)?;
                        self.layout()?;
                        unsafe {
                            divider_cursor(drag.divider.direction);
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
