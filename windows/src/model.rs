// SPDX-License-Identifier: GPL-3.0-or-later
use flowmux_core::{
    CloseSurfaceOutcome, Pane, PaneContent, PaneId, PaneSurface, RemoveOutcome, SplitDirection,
    SurfaceId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    pub cwd: PathBuf,
    pub root: Pane,
    pub focused: PaneId,
}

impl Workspace {
    pub fn new(cwd: PathBuf) -> Self {
        let surface = PaneSurface::terminal("PowerShell", Some(cwd.clone()));
        let pane = PaneId::new();
        Self {
            id: WorkspaceId::new(),
            name: cwd
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Workspace".into()),
            cwd,
            root: Pane::Leaf {
                id: pane,
                content: PaneContent::Tabs {
                    active: surface.id,
                    surfaces: vec![surface],
                },
            },
            focused: pane,
        }
    }

    pub fn active(&self) -> SurfaceId {
        self.root
            .active_surface_id(self.focused)
            .expect("focused pane must be a leaf")
    }

    pub fn new_tab(&mut self) -> SurfaceId {
        let cwd = self
            .root
            .terminal_surface_cwd(self.focused)
            .unwrap_or_else(|| self.cwd.clone());
        let surface = PaneSurface::terminal("PowerShell", Some(cwd));
        let id = surface.id;
        self.root
            .add_surface_to_leaf(self.focused, surface)
            .expect("focused pane must exist");
        self.root.set_active_surface(self.focused, id);
        id
    }

    pub fn split(&mut self, direction: SplitDirection) -> PaneId {
        let cwd = self
            .root
            .terminal_surface_cwd(self.focused)
            .unwrap_or_else(|| self.cwd.clone());
        let surface = PaneSurface::terminal("PowerShell", Some(cwd));
        let pane = self
            .root
            .split_leaf(
                self.focused,
                direction,
                0.5,
                PaneContent::Tabs {
                    active: surface.id,
                    surfaces: vec![surface],
                },
            )
            .expect("focused pane must exist");
        self.focused = pane;
        pane
    }

    /// Closing the last terminal is refused; a tab move never calls this.
    pub fn close_active(&mut self) -> Option<SurfaceId> {
        let mut leaves = Vec::new();
        self.root.for_each_leaf(|id| leaves.push(id));
        if leaves.len() == 1 && self.root.surface_count(self.focused) == Some(1) {
            return None;
        }
        let surface = self.active();
        let result = self.root.close_surface_in_leaf(self.focused, surface);
        if result == CloseSurfaceOutcome::LastSurfaceRemoved {
            // remove_leaf owns its input; preserve the root if the target was absent.
            let old = self.root.clone();
            self.root = match old.remove_leaf(self.focused) {
                RemoveOutcome::Replaced(root) | RemoveOutcome::NotFound(root) => root,
                RemoveOutcome::EntirelyRemoved => unreachable!("last pane protected above"),
            };
            self.focused = self
                .root
                .first_leaf_id()
                .expect("workspace retains one pane");
        }
        Some(surface)
    }

    pub fn leaves(&self) -> Vec<(PaneId, SurfaceId, Vec<PaneSurface>)> {
        let mut result = Vec::new();
        self.root.for_each_leaf(|id| {
            if let Some(PaneContent::Tabs { active, surfaces }) = self.root.find_leaf_content(id) {
                result.push((id, active, surfaces));
            }
        });
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub fn layout(pane: &Pane, area: Rect, gap: i32, out: &mut Vec<(PaneId, Rect)>) {
    match pane {
        Pane::Leaf { id, .. } => out.push((*id, area)),
        Pane::Split {
            direction,
            ratio,
            first,
            second,
            ..
        } => {
            let vertical = *direction == SplitDirection::Vertical;
            let length = if vertical { area.width } else { area.height };
            let gap = gap.max(0).min(length.max(0));
            let available = (length - gap).max(0);
            let ratio = if ratio.is_finite() {
                ratio.clamp(0.1, 0.9)
            } else {
                0.5
            };
            let a = (available as f32 * ratio).round() as i32;
            let mut left = area;
            let mut right = area;
            if vertical {
                left.width = a;
                right.x += a + gap;
                right.width = available - a;
            } else {
                left.height = a;
                right.y += a + gap;
                right.height = available - a;
            }
            layout(first, left, gap, out);
            layout(second, right, gap, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_keeps_original_surface_and_closing_never_removes_last_pane() {
        let mut workspace = Workspace::new(PathBuf::from("project"));
        let original = workspace.active();
        let original_pane = workspace.focused;
        assert_eq!(workspace.close_active(), None);
        workspace.split(SplitDirection::Vertical);
        assert_eq!(
            workspace.root.active_surface_id(original_pane),
            Some(original)
        );
        assert!(workspace.close_active().is_some());
        assert_eq!(workspace.active(), original);
        let tab = workspace.new_tab();
        assert_eq!(workspace.close_active(), Some(tab));
        assert_eq!(workspace.active(), original);
    }

    #[test]
    fn nested_splits_partition_available_pixels_without_overlap() {
        let mut workspace = Workspace::new(PathBuf::from("project"));
        workspace.split(SplitDirection::Vertical);
        workspace.split(SplitDirection::Horizontal);
        let mut panes = vec![];
        layout(
            &workspace.root,
            Rect {
                x: 10,
                y: 20,
                width: 1001,
                height: 601,
            },
            5,
            &mut panes,
        );
        assert_eq!(panes.len(), 3);
        for (_, r) in &panes {
            assert!(r.width > 0 && r.height > 0 && r.x >= 10 && r.y >= 20);
        }
        for (i, (_, a)) in panes.iter().enumerate() {
            for (_, b) in panes.iter().skip(i + 1) {
                assert!(
                    a.x + a.width <= b.x
                        || b.x + b.width <= a.x
                        || a.y + a.height <= b.y
                        || b.y + b.height <= a.y
                );
            }
        }
    }
}
