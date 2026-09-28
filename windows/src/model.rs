// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Context;
use flowmux_core::{
    CloseSurfaceOutcome, Pane, PaneContent, PaneId, PaneSurface, RemoveOutcome, SplitDirection,
    SurfaceId, WorkspaceId,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: String,
    #[serde(default = "legacy_name_locked")]
    pub name_locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<flowmux_core::SshWorkspaceConfig>,
    pub cwd: PathBuf,
    pub root: Pane,
    pub focused: PaneId,
}

fn legacy_name_locked() -> bool {
    true
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
            name_locked: false,
            cwd,
            color: None,
            ssh: None,
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

    pub fn new_ssh(
        local_cwd: PathBuf,
        config: flowmux_core::SshWorkspaceConfig,
        name: Option<String>,
    ) -> anyhow::Result<Self> {
        config.validate().map_err(anyhow::Error::msg)?;
        let name = name
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| config.target.destination());
        validate_name(&name)?;
        let surface = config.terminal(None);
        let pane = PaneId::new();
        Ok(Self {
            id: WorkspaceId::new(),
            name,
            name_locked: true,
            color: None,
            ssh: Some(config),
            cwd: local_cwd,
            root: Pane::Leaf {
                id: pane,
                content: PaneContent::Tabs {
                    active: surface.id,
                    surfaces: vec![surface],
                },
            },
            focused: pane,
        })
    }

    pub fn rename(&mut self, raw: String) -> anyhow::Result<()> {
        let name = raw.trim();
        if name.is_empty() {
            self.name_locked = false;
            self.refresh_name();
        } else {
            validate_name(name)?;
            self.name = name.to_owned();
            self.name_locked = true;
        }
        Ok(())
    }

    pub fn refresh_name(&mut self) -> bool {
        if self.name_locked {
            return false;
        }
        if let Some(config) = &self.ssh {
            let name = config.target.destination();
            let changed = self.name != name;
            self.name = name;
            return changed;
        }
        let Some(active) = self.root.active_surface_id(self.focused) else {
            return false;
        };
        let Some(surface) = self.root.find_surface(self.focused, active) else {
            return false;
        };
        let mut name = surface.title.clone();
        if !surface.title_locked {
            if let flowmux_core::SurfaceKind::Terminal { cwd: Some(cwd), .. } = &surface.kind {
                if surface.title == flowmux_core::terminal_tab_title_for_cwd(Some(cwd)) {
                    if let Some(folder) = cwd.file_name().and_then(|value| value.to_str()) {
                        if !folder.is_empty() {
                            name = folder.to_owned();
                        }
                    }
                }
            }
        }
        if self.name == name {
            return false;
        }
        self.name = name;
        true
    }

    fn new_terminal(&self) -> PaneSurface {
        if let Some(config) = &self.ssh {
            let cwd = self
                .root
                .find_surface(self.focused, self.active())
                .and_then(|surface| match surface.kind {
                    flowmux_core::SurfaceKind::SshTerminal { cwd, .. } => cwd,
                    _ => None,
                });
            return config.terminal(cwd);
        }
        let cwd = self
            .root
            .terminal_surface_cwd(self.focused)
            .unwrap_or_else(|| self.cwd.clone());
        PaneSurface::terminal("PowerShell", Some(cwd))
    }

    pub fn new_tab(&mut self) -> SurfaceId {
        let surface = self.new_terminal();
        let id = surface.id;
        self.root
            .add_surface_to_leaf(self.focused, surface)
            .expect("focused pane must exist");
        self.root.set_active_surface(self.focused, id);
        id
    }

    pub fn split(&mut self, direction: SplitDirection) -> PaneId {
        let surface = self.new_terminal();
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

    /// The ratio always describes the first child, including when a leaf is targeted.
    pub fn resize(&mut self, target: PaneId, ratio: f32) -> anyhow::Result<(PaneId, f32)> {
        anyhow::ensure!(
            ratio.is_finite() && ratio > 0.0 && ratio < 1.0,
            "ratio must be finite and between 0 and 1 (exclusive)"
        );
        fn is_split(pane: &Pane, target: PaneId) -> bool {
            match pane {
                Pane::Leaf { .. } => false,
                Pane::Split {
                    id, first, second, ..
                } => *id == target || is_split(first, target) || is_split(second, target),
            }
        }
        let split = if is_split(&self.root, target) {
            Some(target)
        } else {
            self.root.parent_split_id(target)
        }
        .context("pane has no divider or target was not found")?;
        let ratio = ratio.clamp(0.05, 0.95);
        self.root.set_split_ratio(split, ratio);
        Ok((split, ratio))
    }
}

/// Native captions and persisted metadata preserve Unicode without normalization.
pub fn validate_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!name.trim().is_empty(), "name must not be empty");
    anyhow::ensure!(
        name.encode_utf16().count() <= 256,
        "name exceeds 256 UTF-16 units"
    );
    anyhow::ensure!(
        !name.chars().any(char::is_control),
        "name must not contain control characters"
    );
    Ok(())
}

pub fn parse_color(color: &str) -> anyhow::Result<Option<String>> {
    if color.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(
        color.len() == 7
            && color.starts_with('#')
            && color[1..].bytes().all(|b| b.is_ascii_hexdigit()),
        "color must be #RRGGBB or empty to clear"
    );
    Ok(Some(color.to_ascii_lowercase()))
}

pub fn reorder_workspace(
    workspaces: &mut Vec<Workspace>,
    active: &mut usize,
    target: WorkspaceId,
    index: usize,
) -> anyhow::Result<()> {
    let source = workspaces
        .iter()
        .position(|w| w.id == target)
        .context("workspace not found")?;
    anyhow::ensure!(
        index < workspaces.len(),
        "workspace index is outside the list"
    );
    let focused = workspaces[*active].id;
    let workspace = workspaces.remove(source);
    workspaces.insert(index, workspace);
    *active = workspaces.iter().position(|w| w.id == focused).unwrap();
    Ok(())
}

pub fn remove_workspace(
    workspaces: &mut Vec<Workspace>,
    active: &mut usize,
    target: WorkspaceId,
) -> anyhow::Result<Workspace> {
    let index = workspaces
        .iter()
        .position(|w| w.id == target)
        .context("workspace not found")?;
    anyhow::ensure!(
        workspaces.len() > 1,
        "cannot close the final workspace; close the window with quit"
    );
    let focused = workspaces[*active].id;
    let removed = workspaces.remove(index);
    *active = workspaces
        .iter()
        .position(|w| w.id == focused)
        .unwrap_or(index.min(workspaces.len() - 1));
    Ok(removed)
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
    anyhow::ensure!(
        workspaces[source_ws].ssh == workspaces[target_ws].ssh,
        "cannot move a tab between different SSH or local workspace contexts"
    );
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

/// Split beside an existing pane and relocate the same surface into its new
/// sibling. Commit a complete candidate layout so errors cannot leave an empty
/// destination behind. Splitting a pane's sole tab onto itself is a no-op.
pub fn split_move_surface(
    workspaces: &mut Vec<Workspace>,
    surface: SurfaceId,
    target: PaneId,
    direction: SplitDirection,
) -> anyhow::Result<usize> {
    let (source_ws, source_pane, source_count) = workspaces
        .iter()
        .enumerate()
        .find_map(|(index, workspace)| {
            workspace.leaves().into_iter().find_map(|(pane, _, tabs)| {
                tabs.iter()
                    .any(|tab| tab.id == surface)
                    .then_some((index, pane, tabs.len()))
            })
        })
        .context("source surface not found")?;
    let target_ws = workspaces
        .iter()
        .position(|workspace| {
            matches!(
                workspace.root.find_leaf_content(target),
                Some(PaneContent::Tabs { .. })
            )
        })
        .context("destination pane not found")?;
    anyhow::ensure!(
        workspaces[source_ws].ssh == workspaces[target_ws].ssh,
        "cannot move a tab between different SSH or local workspace contexts"
    );
    if source_pane == target && source_count == 1 {
        return Ok(source_ws);
    }

    let mut candidate = workspaces.clone();
    let sibling = candidate[target_ws]
        .root
        .split_leaf(
            target,
            direction,
            0.5,
            PaneContent::Tabs {
                active: surface,
                surfaces: Vec::new(),
            },
        )
        .context("destination pane disappeared")?;
    let active = move_surface(&mut candidate, surface, sibling, 0)?;
    *workspaces = candidate;
    Ok(active)
}

/// Give an existing surface its own workspace without creating a replacement
/// terminal. The host must attach its existing holder to the new window before
/// publishing the candidate model; this function only relocates domain state.
pub fn detach_surface(
    workspaces: &mut Vec<Workspace>,
    surface: SurfaceId,
) -> anyhow::Result<usize> {
    let (title, name_locked, cwd, ssh) = workspaces
        .iter()
        .find_map(|workspace| {
            workspace.leaves().into_iter().find_map(|(_, _, tabs)| {
                tabs.into_iter().find(|tab| tab.id == surface).map(|tab| {
                    if workspace.ssh.is_some() {
                        return (
                            workspace.name.clone(),
                            workspace.name_locked,
                            workspace.cwd.clone(),
                            workspace.ssh.clone(),
                        );
                    }
                    let cwd = match tab.kind {
                        flowmux_core::SurfaceKind::Terminal { cwd, .. } => cwd,
                        _ => None,
                    }
                    .unwrap_or_else(|| workspace.cwd.clone());
                    (tab.title, false, cwd, None)
                })
            })
        })
        .context("source surface not found")?;
    let pane = PaneId::new();
    let mut candidate = workspaces.clone();
    candidate.push(Workspace {
        id: WorkspaceId::new(),
        name: title,
        name_locked,
        color: None,
        ssh,
        cwd,
        root: Pane::Leaf {
            id: pane,
            content: PaneContent::Tabs {
                active: surface,
                surfaces: Vec::new(),
            },
        },
        focused: pane,
    });
    let active = move_surface(&mut candidate, surface, pane, 0)?;
    *workspaces = candidate;
    Ok(active)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.width && y < self.y + self.height
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Divider {
    pub split: PaneId,
    pub direction: SplitDirection,
    pub bounds: Rect,
    pub parent: Rect,
}

impl Divider {
    pub fn coordinate(self, x: i32, y: i32) -> i32 {
        if self.direction == SplitDirection::Vertical {
            x
        } else {
            y
        }
    }

    /// Preserve the grabbed offset so pressing at either edge never jumps the split.
    pub fn drag_ratio(self, x: i32, y: i32, offset: i32) -> Option<f32> {
        let vertical = self.direction == SplitDirection::Vertical;
        let start = if vertical {
            self.parent.x
        } else {
            self.parent.y
        };
        let available = if vertical {
            self.parent.width - self.bounds.width
        } else {
            self.parent.height - self.bounds.height
        };
        (available > 0).then(|| {
            ((self.coordinate(x, y) - start - offset) as f32 / available as f32).clamp(0.05, 0.95)
        })
    }
}

#[derive(Default, Serialize)]
pub struct Layout {
    pub panes: Vec<(PaneId, Rect)>,
    pub dividers: Vec<Divider>,
}

pub fn layout(pane: &Pane, area: Rect, gap: i32, out: &mut Vec<(PaneId, Rect)>) {
    let mut result = Layout::default();
    partition(pane, area, gap, &mut result);
    out.extend(result.panes);
}

pub fn partition(pane: &Pane, area: Rect, gap: i32, out: &mut Layout) {
    match pane {
        Pane::Leaf { id, .. } => out.panes.push((*id, area)),
        Pane::Split {
            id,
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
                ratio.clamp(0.05, 0.95)
            } else {
                0.5
            };
            let a = (available as f32 * ratio).round() as i32;
            let mut left = area;
            let mut right = area;
            let mut bounds = area;
            if vertical {
                left.width = a;
                right.x += a + gap;
                right.width = available - a;
                bounds.x += a;
                bounds.width = gap;
            } else {
                left.height = a;
                right.y += a + gap;
                right.height = available - a;
                bounds.y += a;
                bounds.height = gap;
            }
            out.dividers.push(Divider {
                split: *id,
                direction: *direction,
                bounds,
                parent: area,
            });
            partition(first, left, gap, out);
            partition(second, right, gap, out);
        }
    }
}

/// Use the unmaximized layout; a hidden sibling remains a directional destination.
pub fn neighbor(
    panes: &[(PaneId, Rect)],
    source: PaneId,
    direction: crate::command::FocusDirection,
) -> Option<PaneId> {
    use crate::command::FocusDirection::*;
    let source_rect = panes.iter().find(|(id, _)| *id == source)?.1;
    panes
        .iter()
        .filter_map(|(id, rect)| {
            if *id == source {
                return None;
            }
            // Doubled centers retain half-pixel precision and avoid float ordering.
            let dx = i64::from(2 * (rect.x - source_rect.x) + rect.width - source_rect.width);
            let dy = i64::from(2 * (rect.y - source_rect.y) + rect.height - source_rect.height);
            let aligned_y = dy.abs() < i64::from(2 * rect.height.max(source_rect.height));
            let aligned_x = dx.abs() < i64::from(2 * rect.width.max(source_rect.width));
            let eligible = match direction {
                Left => dx < -2 && aligned_y,
                Right => dx > 2 && aligned_y,
                Up => dy < -2 && aligned_x,
                Down => dy > 2 && aligned_x,
            };
            eligible.then_some((*id, dx * dx + dy * dy))
        })
        .min_by_key(|(_, distance)| *distance)
        .map(|(id, _)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_tabs_splits_detach_keep_remote_context_and_moves_are_atomic() {
        let config = flowmux_core::SshWorkspaceConfig {
            target: flowmux_core::SshTarget::parse("user@example.test").unwrap(),
            cwd: Some("/원격/한 e\u{301}".into()),
            tmux: true,
            forwards: Vec::new(),
        };
        let local = PathBuf::from("C:/local 작업");
        let mut ws = Workspace::new_ssh(local.clone(), config.clone(), None).unwrap();
        assert_eq!(ws.name, "user@example.test");
        assert!(ws.name_locked);
        let first = ws.active();
        let focused = ws.focused;
        let remote = "/다른/한 e\u{301}";
        let Pane::Leaf {
            content: PaneContent::Tabs { surfaces, .. },
            ..
        } = &mut ws.root
        else {
            panic!("expected SSH leaf")
        };
        let flowmux_core::SurfaceKind::SshTerminal { cwd, .. } = &mut surfaces[0].kind else {
            panic!("expected SSH terminal")
        };
        *cwd = Some(remote.into());
        ws.root
            .set_surface_title_auto(focused, first, "remote shell title".into());
        ws.rename("  ".into()).unwrap();
        assert_eq!(ws.name, "user@example.test");
        let tab = ws.new_tab();
        ws.split(SplitDirection::Vertical);
        for (_, _, tabs) in ws.leaves() {
            for surface in tabs {
                let flowmux_core::SurfaceKind::SshTerminal { cwd, tmux_session } = surface.kind
                else {
                    panic!("SSH operation created local terminal")
                };
                assert_eq!(cwd.as_deref(), Some(remote));
                assert_eq!(
                    tmux_session,
                    Some(format!("flowmux-{}", surface.id.0.simple()))
                );
            }
        }
        ws.rename("  서버 한  ".into()).unwrap();
        let mut workspaces = vec![ws, Workspace::new(local.clone())];
        let target = workspaces[1].focused;
        let before = serde_json::to_value(&workspaces).unwrap();
        assert!(move_surface(&mut workspaces, tab, target, 0).is_err());
        assert!(
            split_move_surface(&mut workspaces, tab, target, SplitDirection::Horizontal).is_err()
        );
        assert_eq!(serde_json::to_value(&workspaces).unwrap(), before);
        let detached = detach_surface(&mut workspaces, tab).unwrap();
        assert_eq!(workspaces[detached].ssh, Some(config));
        assert_eq!(workspaces[detached].cwd, local);
        assert_eq!(workspaces[detached].active(), tab);
        assert_eq!(workspaces[detached].name, "서버 한");
        let target = workspaces[0].focused;
        move_surface(&mut workspaces, tab, target, 0).unwrap();
        assert_eq!(workspaces.len(), 2);
    }

    #[test]
    fn workspace_names_follow_focused_titles_unless_locked_and_preserve_legacy_names() {
        let folder = "한글_한_e\u{301}_아주긴프로젝트폴더이름";
        let cwd = PathBuf::from("C:/작업").join(folder);
        let mut workspace = Workspace::new(cwd.clone());
        let first = workspace.focused;
        let surface = workspace.active();
        assert!(!workspace.name_locked);
        assert_eq!(workspace.name, folder);
        workspace.root.set_surface_title_auto(
            first,
            surface,
            flowmux_core::terminal_tab_title_for_cwd(Some(&cwd)),
        );
        workspace.name = "previous".into();
        assert!(workspace.refresh_name());
        assert_eq!(workspace.name, folder);
        assert!(!workspace.refresh_name());

        let custom = "수동 한 e\u{301} 이름";
        workspace.rename(format!("  {custom}  ")).unwrap();
        assert!(workspace.name_locked);
        assert_eq!(workspace.name, custom);
        let second = workspace.split(SplitDirection::Vertical);
        let second_surface = workspace.active();
        workspace
            .root
            .set_surface_title_auto(second, second_surface, "OSC 한글 제목".into());
        assert!(!workspace.refresh_name());
        assert_eq!(workspace.name, custom);
        workspace.rename(" \t\n ".into()).unwrap();
        assert!(!workspace.name_locked);
        assert_eq!(workspace.name, "OSC 한글 제목");
        workspace.focused = first;
        assert!(workspace.refresh_name());
        assert_eq!(workspace.name, folder);

        workspace
            .root
            .rename_surface(first, surface, "탭 한 제목".into());
        workspace
            .root
            .set_surface_cwd(first, surface, "C:/다른폴더".into());
        assert!(workspace.refresh_name());
        assert_eq!(workspace.name, "탭 한 제목");
        let before = serde_json::to_value(&workspace).unwrap();
        assert!(workspace.rename("잘못된\n이름".into()).is_err());
        assert_eq!(serde_json::to_value(&workspace).unwrap(), before);

        let roundtrip: Workspace = serde_json::from_value(before.clone()).unwrap();
        assert!(!roundtrip.name_locked);
        assert_eq!(roundtrip.name, workspace.name);
        let mut legacy = before;
        legacy.as_object_mut().unwrap().remove("name_locked");
        let mut legacy: Workspace = serde_json::from_value(legacy).unwrap();
        assert!(legacy.name_locked);
        legacy.focused = second;
        assert!(!legacy.refresh_name());
        assert_eq!(legacy.name, workspace.name);
        workspace.rename(custom.into()).unwrap();
        let roundtrip: Workspace =
            serde_json::from_value(serde_json::to_value(workspace).unwrap()).unwrap();
        assert!(roundtrip.name_locked);
        assert_eq!(roundtrip.name, custom);
    }

    #[test]
    fn unicode_metadata_keeps_original_codepoints_and_rejects_invalid_native_captions() {
        for name in ["한글 한 e\u{301} 😀 & 탭", "  공백 보존  "] {
            assert!(validate_name(name).is_ok());
        }
        assert!(validate_name(&"😀".repeat(128)).is_ok());
        for name in ["", " \t", "한\n글", "nul\0name", &"😀".repeat(129)] {
            assert!(validate_name(name).is_err());
        }
        assert_eq!(parse_color("#Ab12EF").unwrap().as_deref(), Some("#ab12ef"));
        assert_eq!(parse_color("").unwrap(), None);
        for color in ["red", "#abc", "#abcxyz", "#12345678", "#한글"] {
            assert!(parse_color(color).is_err());
        }
    }

    #[test]
    fn workspace_reorder_and_close_preserve_active_identity_and_select_nearest_survivor() {
        let mut list: Vec<_> = (0..3)
            .map(|i| Workspace::new(format!("workspace-{i}").into()))
            .collect();
        let ids: Vec<_> = list.iter().map(|w| w.id).collect();
        let original: Vec<_> = list.iter().map(|w| w.active()).collect();
        let mut active = 1;
        reorder_workspace(&mut list, &mut active, ids[0], 2).unwrap();
        assert_eq!(list[active].id, ids[1]);
        assert_eq!(list[2].active(), original[0]);
        let snapshot = serde_json::to_value(&list).unwrap();
        assert!(reorder_workspace(&mut list, &mut active, ids[0], 3).is_err());
        assert!(reorder_workspace(&mut list, &mut active, WorkspaceId::new(), 0).is_err());
        assert_eq!(serde_json::to_value(&list).unwrap(), snapshot);
        let removed = remove_workspace(&mut list, &mut active, ids[0]).unwrap();
        assert_eq!(removed.active(), original[0]);
        assert_eq!(list[active].id, ids[1]);
        remove_workspace(&mut list, &mut active, ids[1]).unwrap();
        assert_eq!(list[active].id, ids[2]);
        assert!(remove_workspace(&mut list, &mut active, ids[2]).is_err());
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn resizing_targets_only_the_owning_split_and_rejects_invalid_requests() {
        let mut ws = Workspace::new("project".into());
        let left = ws.focused;
        assert!(ws.resize(left, 0.7).is_err());
        ws.split(SplitDirection::Vertical);
        let bottom = ws.split(SplitDirection::Horizontal);
        let Pane::Split { id: root, .. } = ws.root else {
            panic!()
        };
        let inner = ws.root.parent_split_id(bottom).unwrap();
        assert_eq!(ws.resize(bottom, 0.7).unwrap(), (inner, 0.7));
        assert_eq!(ws.resize(root, 0.001).unwrap(), (root, 0.05));
        assert_eq!(ws.resize(left, 0.999).unwrap(), (root, 0.95));
        let before = serde_json::to_value(&ws).unwrap();
        for ratio in [0.0, 1.0, -0.1, f32::NAN, f32::INFINITY] {
            assert!(ws.resize(root, ratio).is_err());
        }
        assert!(ws.resize(PaneId::new(), 0.5).is_err());
        assert_eq!(serde_json::to_value(&ws).unwrap(), before);
        let Pane::Split { second, .. } = &ws.root else {
            panic!()
        };
        assert!(matches!(**second, Pane::Split { ratio, .. } if ratio == 0.7));
    }

    #[test]
    fn divider_drag_respects_grab_offset_clamps_and_partitions_tiny_areas() {
        let mut ws = Workspace::new("project".into());
        ws.split(SplitDirection::Vertical);
        ws.split(SplitDirection::Horizontal);
        for (width, height) in [(1005, 805), (1, 1), (0, 0)] {
            let area = Rect {
                x: 10,
                y: 20,
                width,
                height,
            };
            let mut result = Layout::default();
            partition(&ws.root, area, 5, &mut result);
            let total: i32 = result
                .panes
                .iter()
                .map(|(_, r)| r.width * r.height)
                .sum::<i32>()
                + result
                    .dividers
                    .iter()
                    .map(|d| d.bounds.width * d.bounds.height)
                    .sum::<i32>();
            assert_eq!(total, width * height);
            for divider in result.dividers {
                assert!(divider.bounds.width >= 0 && divider.bounds.height >= 0);
                if width > 10 {
                    let (x, y) = (divider.bounds.x + 2, divider.bounds.y + 2);
                    assert!(divider.bounds.contains(x, y));
                    assert_eq!(divider.drag_ratio(x, y, 2), Some(0.5));
                    assert_eq!(divider.drag_ratio(-1000, -1000, 2), Some(0.05));
                    assert_eq!(divider.drag_ratio(5000, 5000, 2), Some(0.95));
                } else {
                    assert_eq!(divider.drag_ratio(10, 20, 0), None);
                }
            }
        }
    }

    #[test]
    fn directional_focus_uses_nearest_aligned_pane_and_stops_at_edges() {
        use crate::command::FocusDirection::*;
        let mut ws = Workspace::new("project".into());
        let left = ws.focused;
        let top = ws.split(SplitDirection::Vertical);
        let bottom = ws.split(SplitDirection::Horizontal);
        let mut panes = vec![];
        layout(
            &ws.root,
            Rect {
                x: 0,
                y: 0,
                width: 1005,
                height: 805,
            },
            5,
            &mut panes,
        );
        assert_eq!(neighbor(&panes, top, Down), Some(bottom));
        assert_eq!(neighbor(&panes, bottom, Up), Some(top));
        assert_eq!(neighbor(&panes, bottom, Left), Some(left));
        assert_eq!(neighbor(&panes, top, Right), None);
        assert_eq!(neighbor(&panes, top, Up), None);
        assert_eq!(neighbor(&panes, left, Left), None);
        assert_eq!(neighbor(&panes, PaneId::new(), Right), None);
    }

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
    fn split_move_retains_original_pane_and_surface_in_both_directions() {
        for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
            let mut workspaces = vec![Workspace::new("한글-project".into())];
            let target = workspaces[0].focused;
            let original = workspaces[0].active();
            let moved = workspaces[0].new_tab();
            let before = workspaces[0].leaves()[0].2[1].clone();
            assert_eq!(
                split_move_surface(&mut workspaces, moved, target, direction).unwrap(),
                0
            );
            let workspace = &workspaces[0];
            let flowmux_core::Pane::Split {
                first,
                second,
                direction: actual,
                ratio,
                ..
            } = &workspace.root
            else {
                panic!("expected split");
            };
            assert_eq!(*actual, direction);
            assert_eq!(*ratio, 0.5);
            assert_eq!(first.first_leaf_id(), Some(target));
            assert_eq!(first.active_surface_id(target), Some(original));
            let sibling = second.first_leaf_id().unwrap();
            assert_ne!(sibling, target);
            assert_eq!(workspace.focused, sibling);
            assert_eq!(workspace.active(), moved);
            let tabs = workspace
                .leaves()
                .into_iter()
                .find(|(pane, _, _)| *pane == sibling)
                .unwrap()
                .2;
            assert_eq!(tabs.len(), 1);
            assert_eq!(
                serde_json::to_value(&tabs[0]).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
        }
    }

    #[test]
    fn split_move_is_atomic_and_collapses_last_source_pane_or_workspace() {
        let mut workspaces = vec![Workspace::new("source".into())];
        let source = workspaces[0].active();
        let source_pane = workspaces[0].focused;
        let before = serde_json::to_value(&workspaces).unwrap();
        assert!(split_move_surface(
            &mut workspaces,
            source,
            PaneId::new(),
            SplitDirection::Vertical
        )
        .is_err());
        assert!(split_move_surface(
            &mut workspaces,
            SurfaceId::new(),
            source_pane,
            SplitDirection::Vertical
        )
        .is_err());
        assert_eq!(serde_json::to_value(&workspaces).unwrap(), before);
        assert_eq!(
            split_move_surface(
                &mut workspaces,
                source,
                source_pane,
                SplitDirection::Vertical
            )
            .unwrap(),
            0
        );
        assert_eq!(serde_json::to_value(&workspaces).unwrap(), before);

        workspaces.push(Workspace::new("target".into()));
        let target_workspace = workspaces[1].id;
        let target = workspaces[1].focused;
        let target_surface = workspaces[1].active();
        assert_eq!(
            split_move_surface(&mut workspaces, source, target, SplitDirection::Horizontal)
                .unwrap(),
            0
        );
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].id, target_workspace);
        assert_eq!(
            workspaces[0].root.active_surface_id(target),
            Some(target_surface)
        );
        let moved_pane = workspaces[0].focused;
        assert_eq!(
            workspaces[0].root.terminal_surface_cwd(moved_pane),
            Some("source".into())
        );
        assert_eq!(
            split_move_surface(&mut workspaces, source, target, SplitDirection::Vertical).unwrap(),
            0
        );
        assert_eq!(workspaces[0].leaves().len(), 2);
        assert!(workspaces[0].root.find_leaf_content(moved_pane).is_none());
        assert_eq!(
            workspaces[0].root.active_surface_id(target),
            Some(target_surface)
        );
        assert_eq!(workspaces[0].active(), source);
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
    fn detach_retains_unicode_metadata_and_handles_last_tab_tabs_and_split_sources() {
        for shape in 0..3 {
            let mut workspaces = vec![Workspace::new("원본 root".into())];
            match shape {
                1 => {
                    workspaces[0].new_tab();
                }
                2 => {
                    workspaces[0].split(SplitDirection::Vertical);
                }
                _ => {}
            }
            let source_workspace = workspaces[0].id;
            let source_pane = workspaces[0].focused;
            let surface = workspaces[0].active();
            let title = "한글 한 e\u{301} 😀 & 탭";
            let cwd = PathBuf::from("C:/작업/한/😀");
            assert!(workspaces[0]
                .root
                .rename_surface(source_pane, surface, title.into()));
            assert!(workspaces[0]
                .root
                .set_surface_cwd(source_pane, surface, cwd.clone()));
            let original = workspaces[0]
                .root
                .find_surface(source_pane, surface)
                .unwrap();
            let original_count: usize = workspaces[0]
                .leaves()
                .iter()
                .map(|(_, _, tabs)| tabs.len())
                .sum();
            let before = serde_json::to_value(&workspaces).unwrap();
            assert!(detach_surface(&mut workspaces, SurfaceId::new()).is_err());
            assert_eq!(serde_json::to_value(&workspaces).unwrap(), before);

            let detached = detach_surface(&mut workspaces, surface).unwrap();
            assert_eq!(detached, workspaces.len() - 1);
            let workspace = &workspaces[detached];
            assert_ne!(workspace.id, source_workspace);
            assert_ne!(workspace.focused, source_pane);
            assert_eq!(workspace.name, title);
            assert!(!workspace.name_locked);
            assert_eq!(workspace.cwd, cwd);
            assert_eq!(workspace.active(), surface);
            assert_eq!(workspace.leaves().len(), 1);
            assert_eq!(
                serde_json::to_value(
                    workspace
                        .root
                        .find_surface(workspace.focused, surface)
                        .unwrap()
                )
                .unwrap(),
                serde_json::to_value(original).unwrap()
            );
            assert_eq!(
                workspaces
                    .iter()
                    .flat_map(Workspace::leaves)
                    .map(|(_, _, tabs)| tabs.len())
                    .sum::<usize>(),
                original_count
            );
            if shape == 0 {
                assert_eq!(workspaces.len(), 1);
            } else {
                assert_eq!(workspaces.len(), 2);
                assert_eq!(workspaces[0].id, source_workspace);
                assert_eq!(workspaces[0].leaves().len(), 1);
                assert_eq!(
                    workspaces[0].root.find_leaf_content(source_pane).is_none(),
                    shape == 2
                );
            }
        }
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
