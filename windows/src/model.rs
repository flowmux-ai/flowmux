// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Context;
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

/// Relocate domain state only. The host retains the existing view and PTY by SurfaceId.
/// Validate both endpoints before removing anything; moving the final source tab
/// collapses its empty pane/workspace without creating a replacement process.
pub fn move_surface(
    workspaces: &mut Vec<Workspace>,
    surface: SurfaceId,
    target: PaneId,
    index: usize,
) -> anyhow::Result<usize> {
    let (source_ws, source_pane) = workspaces
        .iter()
        .enumerate()
        .find_map(|(i, ws)| {
            ws.leaves().into_iter().find_map(|(pane, _, tabs)| {
                tabs.iter()
                    .any(|tab| tab.id == surface)
                    .then_some((i, pane))
            })
        })
        .context("source surface not found")?;
    let mut target_ws = workspaces
        .iter()
        .position(|ws| {
            matches!(
                ws.root.find_leaf_content(target),
                Some(PaneContent::Tabs { .. })
            )
        })
        .context("destination pane not found")?;
    let (tab, empty) = workspaces[source_ws]
        .root
        .take_surface_from_leaf(source_pane, surface)
        .expect("validated source");
    workspaces[target_ws]
        .root
        .insert_surface_into_leaf(target, tab, index)
        .expect("validated destination");
    workspaces[target_ws].focused = target;
    if empty && source_pane != target {
        match workspaces[source_ws].root.clone().remove_leaf(source_pane) {
            RemoveOutcome::Replaced(root) => {
                let ws = &mut workspaces[source_ws];
                ws.root = root;
                if ws.focused == source_pane {
                    ws.focused = ws.root.first_leaf_id().expect("remaining pane");
                }
            }
            RemoveOutcome::EntirelyRemoved => {
                workspaces.remove(source_ws);
                if source_ws < target_ws {
                    target_ws -= 1;
                }
            }
            RemoveOutcome::NotFound(_) => unreachable!("validated source"),
        }
    }
    Ok(target_ws)
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
    fn live_tab_move_preserves_surface_and_cwd_and_collapses_empty_sources() {
        let mut workspaces = vec![
            Workspace::new("source".into()),
            Workspace::new("target".into()),
        ];
        let surface = workspaces[0].active();
        let destination = workspaces[1].focused;
        assert_eq!(
            move_surface(&mut workspaces, surface, destination, 0).unwrap(),
            0
        );
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].active(), surface);
        assert_eq!(
            workspaces[0].root.terminal_surface_cwd(destination),
            Some("source".into())
        );
        let other = workspaces[0].split(SplitDirection::Vertical);
        let moved = workspaces[0].active();
        move_surface(&mut workspaces, moved, destination, 0).unwrap();
        assert_eq!(workspaces[0].leaves().len(), 1);
        assert_ne!(workspaces[0].focused, other);
        assert_eq!(workspaces[0].leaves()[0].2[0].id, moved);
        move_surface(&mut workspaces, moved, destination, usize::MAX).unwrap();
        assert_eq!(workspaces[0].leaves()[0].2.last().unwrap().id, moved);
    }

    #[test]
    fn invalid_move_leaves_layout_and_focus_unchanged() {
        let mut workspaces = vec![Workspace::new("source".into())];
        let before = serde_json::to_value(&workspaces).unwrap();
        let surface = workspaces[0].active();
        assert!(move_surface(&mut workspaces, surface, PaneId::new(), 0).is_err());
        assert_eq!(serde_json::to_value(&workspaces).unwrap(), before);
    }

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
