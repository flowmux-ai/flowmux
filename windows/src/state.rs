// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows-only state schema. Shell argv is explicit; history is display data only.
use crate::model::Workspace;
use anyhow::ensure;
use flowmux_core::{Pane, PaneContent, SurfaceId, SurfaceKind, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

pub const MAX_STATE_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_SCREEN_BYTES: usize = 128 * 1024;
pub(crate) const DEFAULT_SIDEBAR_WIDTH: u32 = 260;
pub(crate) const MIN_SIDEBAR_WIDTH: u32 = 160;
pub(crate) const MAX_SIDEBAR_WIDTH: u32 = 640;

fn default_sidebar_width() -> u32 {
    DEFAULT_SIDEBAR_WIDTH
}

fn is_default_sidebar_width(width: &u32) -> bool {
    *width == DEFAULT_SIDEBAR_WIDTH
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// Normal workspace coordinates from Win32 WINDOWPLACEMENT, not screen bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedPlacement {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_width_dip: Option<u32>,
}
impl SavedPlacement {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.sidebar_width_dip
                .is_none_or(|width| (MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH).contains(&width)),
            "invalid saved sidebar width"
        );
        ensure!(
            (1..=32768).contains(&self.width) && (1..=32768).contains(&self.height),
            "invalid saved window dimensions"
        );
        ensure!(
            self.left.unsigned_abs() <= 1_000_000 && self.top.unsigned_abs() <= 1_000_000,
            "invalid saved window position"
        );
        ensure!(
            self.left.checked_add(self.width as i32).is_some()
                && self.top.checked_add(self.height as i32).is_some(),
            "saved window bounds overflow"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedScreen {
    pub format: String,
    pub cols: u16,
    pub rows: u16,
    pub data: String,
    pub truncated: bool,
}
impl SavedScreen {
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(self.format == "xterm-ansi-v1", "unsupported history format");
        ensure!(
            (2..=1000).contains(&self.cols) && (1..=1000).contains(&self.rows),
            "invalid history dimensions"
        );
        ensure!(
            self.data.len() <= MAX_SCREEN_BYTES,
            "history exceeds size limit"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowState {
    pub version: u32,
    pub window: Uuid,
    pub workspaces: Vec<Workspace>,
    // Some serializes as the existing UUID string; null represents an alive empty window.
    pub active_workspace: Option<WorkspaceId>,
    pub screens: HashMap<SurfaceId, SavedScreen>,
    #[serde(default)]
    pub shells: HashMap<SurfaceId, crate::shell::Shell>,
    #[serde(
        default = "default_sidebar_width",
        skip_serializing_if = "is_default_sidebar_width"
    )]
    pub sidebar_width_dip: u32,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub detached_windows: HashMap<SurfaceId, SavedPlacement>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub main_closed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detached_focus: Option<SurfaceId>,
}
impl WindowState {
    pub fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        ensure!(bytes.len() <= MAX_STATE_BYTES, "state exceeds size limit");
        let state: Self = serde_json::from_slice(bytes)?;
        state.validate()?;
        Ok(state)
    }
    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        ensure!(bytes.len() <= MAX_STATE_BYTES, "state exceeds size limit");
        Ok(bytes)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(self.version == 1, "unsupported Windows state version");
        ensure!(
            (MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH).contains(&self.sidebar_width_dip),
            "invalid saved sidebar width"
        );
        ensure!(self.workspaces.len() <= 64, "invalid workspace count");
        if self.workspaces.is_empty() {
            ensure!(
                self.active_workspace.is_none(),
                "empty window has an active workspace"
            );
            ensure!(
                self.screens.is_empty()
                    && self.shells.is_empty()
                    && self.detached_windows.is_empty()
                    && self.detached_focus.is_none()
                    && !self.main_closed,
                "empty window retains session data or has no live main window"
            );
        } else {
            ensure!(
                self.active_workspace
                    .is_some_and(|active| self.workspaces.iter().any(|w| w.id == active)),
                "active workspace missing"
            );
        }
        let mut ids = HashSet::from([self.window]);
        let mut surfaces = HashSet::new();
        for ws in &self.workspaces {
            ensure!(ids.insert(ws.id.0), "duplicate workspace identity");
            if let Some(color) = &ws.color {
                ensure!(
                    crate::model::parse_color(color)?.is_some(),
                    "invalid saved workspace color"
                );
            }
            ensure!(
                ws.name.len() <= 4096 && ws.cwd.as_os_str().len() <= 32767,
                "workspace metadata exceeds limit"
            );
            validate_pane(&ws.root, 0, &mut ids, &mut surfaces)?;
            ensure!(
                ws.root.find_leaf_content(ws.focused).is_some(),
                "focused pane missing"
            );
        }
        ensure!(surfaces.len() <= 128, "too many saved surfaces");
        let single_surfaces: HashSet<_> = self
            .workspaces
            .iter()
            .filter_map(|ws| match &ws.root {
                Pane::Leaf {
                    content: PaneContent::Tabs { surfaces, .. },
                    ..
                } if surfaces.len() == 1 => Some(surfaces[0].id),
                _ => None,
            })
            .collect();
        for (surface, placement) in &self.detached_windows {
            ensure!(surfaces.contains(surface), "orphaned separate window");
            ensure!(
                single_surfaces.contains(surface),
                "separate window requires a single-pane, single-surface workspace"
            );
            placement.validate()?;
        }
        ensure!(
            !self.main_closed || self.detached_windows.len() == self.workspaces.len(),
            "closed main window contains attached workspaces"
        );
        ensure!(
            self.detached_focus
                .is_none_or(|surface| self.detached_windows.contains_key(&surface)),
            "focused separate window missing"
        );
        let terminals: HashSet<_> = self
            .workspaces
            .iter()
            .flat_map(|ws| ws.leaves())
            .flat_map(|(_, _, tabs)| tabs)
            .filter(|tab| matches!(tab.kind, SurfaceKind::Terminal { .. }))
            .map(|tab| tab.id)
            .collect();
        ensure!(
            self.screens.len() == terminals.len(),
            "history missing from saved layout"
        );
        for (id, screen) in &self.screens {
            ensure!(terminals.contains(id), "orphaned history");
            screen.validate()?;
        }
        for (id, shell) in &self.shells {
            ensure!(terminals.contains(id), "orphaned shell specification");
            shell.validate()?;
        }
        Ok(())
    }
}
fn validate_pane(
    pane: &Pane,
    depth: usize,
    ids: &mut HashSet<Uuid>,
    surfaces: &mut HashSet<SurfaceId>,
) -> anyhow::Result<()> {
    ensure!(depth <= 24, "pane tree too deep");
    match pane {
        Pane::Split {
            id,
            ratio,
            first,
            second,
            ..
        } => {
            ensure!(ids.insert(id.0), "duplicate pane identity");
            ensure!(
                ratio.is_finite() && *ratio > 0.0 && *ratio < 1.0,
                "invalid split ratio"
            );
            validate_pane(first, depth + 1, ids, surfaces)?;
            validate_pane(second, depth + 1, ids, surfaces)?;
        }
        Pane::Leaf {
            id,
            content:
                PaneContent::Tabs {
                    active,
                    surfaces: tabs,
                },
        } => {
            ensure!(ids.insert(id.0), "duplicate pane identity");
            ensure!(
                !tabs.is_empty() && tabs.iter().any(|t| t.id == *active),
                "active tab missing"
            );
            for tab in tabs {
                ensure!(
                    ids.insert(tab.id.0) && surfaces.insert(tab.id),
                    "duplicate surface identity"
                );
                ensure!(
                    tab.title.len() <= 4096 && tab.scrollback.is_none(),
                    "unsupported tab metadata"
                );
                match &tab.kind {
                    SurfaceKind::Terminal { shell: None, cwd } => ensure!(
                        cwd.as_ref().is_none_or(|p| p.as_os_str().len() <= 32767),
                        "cwd exceeds limit"
                    ),
                    SurfaceKind::Editor {
                        workspace_root,
                        session,
                    } => {
                        crate::editor::validate_path(workspace_root)?;
                        crate::editor::validate_session(session)?;
                    }
                    SurfaceKind::Browser { initial_url } => {
                        crate::browser::url(initial_url.as_deref().unwrap_or("about:blank"))?;
                    }
                    _ => anyhow::bail!("unsupported saved surface; original file was preserved"),
                }
            }
        }
        _ => anyhow::bail!("unsupported saved pane"),
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn sample() -> WindowState {
    let ws = Workspace::new("history 한글".into());
    WindowState {
        version: 1,
        window: Uuid::new_v4(),
        active_workspace: Some(ws.id),
        screens: HashMap::from([(
            ws.active(),
            SavedScreen {
                format: "xterm-ansi-v1".into(),
                cols: 80,
                rows: 24,
                data: "\u{1b}[32m한글\u{1b}[0m".into(),
                truncated: false,
            },
        )]),
        workspaces: vec![ws],
        shells: HashMap::new(),
        sidebar_width_dip: DEFAULT_SIDEBAR_WIDTH,
        detached_windows: HashMap::new(),
        main_closed: false,
        detached_focus: None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_main_window_roundtrips_and_existing_active_uuid_still_loads() {
        let original = sample();
        let id = original.workspaces[0].id;
        let legacy = serde_json::to_value(&original).unwrap();
        assert_eq!(legacy["active_workspace"], serde_json::to_value(id).unwrap());
        assert_eq!(
            WindowState::decode(&serde_json::to_vec(&legacy).unwrap())
                .unwrap()
                .active_workspace,
            Some(id)
        );
        let empty = WindowState {
            workspaces: Vec::new(),
            active_workspace: None,
            screens: HashMap::new(),
            ..original
        };
        let encoded = empty.encode().unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&encoded).unwrap()["active_workspace"]
                .is_null()
        );
        let restored = WindowState::decode(&encoded).unwrap();
        assert!(restored.workspaces.is_empty());
        assert!(restored.active_workspace.is_none());
        assert!(!restored.main_closed);
        assert_eq!(restored.window, empty.window);
    }
    #[test]
    fn empty_main_window_rejects_orphaned_state_and_nonempty_requires_active_workspace() {
        let original = sample();
        let surface = original.workspaces[0].active();
        let empty = WindowState {
            workspaces: Vec::new(),
            active_workspace: None,
            screens: HashMap::new(),
            ..original.clone()
        };
        for change in 0..6 {
            let mut invalid = empty.clone();
            match change {
                0 => invalid.active_workspace = original.active_workspace,
                1 => invalid.screens = original.screens.clone(),
                2 => {
                    invalid.shells.insert(surface, crate::shell::Shell::default());
                }
                3 => {
                    invalid.detached_windows.insert(
                        surface,
                        SavedPlacement {
                            left: 0,
                            top: 0,
                            width: 800,
                            height: 600,
                            maximized: false,
                            sidebar_width_dip: None,
                        },
                    );
                }
                4 => invalid.detached_focus = Some(surface),
                _ => invalid.main_closed = true,
            }
            assert!(invalid.encode().is_err());
            assert!(WindowState::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        for active in [None, Some(WorkspaceId::new())] {
            let invalid = WindowState {
                active_workspace: active,
                ..original.clone()
            };
            assert!(invalid.encode().is_err());
            assert!(WindowState::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }
    #[test]
    fn separate_windows_preserve_old_state_and_validate_placement_and_ownership() {
        let old = sample();
        let json = serde_json::to_value(&old).unwrap();
        for key in ["detached_windows", "main_closed", "detached_focus"] {
            assert!(json.get(key).is_none());
        }
        let restored = WindowState::decode(&old.encode().unwrap()).unwrap();
        assert!(restored.detached_windows.is_empty());
        assert!(!restored.main_closed);
        assert!(restored.detached_focus.is_none());

        let surface = old.workspaces[0].active();
        let placement = SavedPlacement {
            left: -1920,
            top: -120,
            width: 1280,
            height: 800,
            maximized: true,
            sidebar_width_dip: Some(320),
        };
        let mut state = old.clone();
        state.detached_windows.insert(surface, placement);
        // A live empty main window may coexist with only detached workspaces.
        assert!(state.encode().is_ok());
        state.main_closed = true;
        state.detached_focus = Some(surface);
        let restored = WindowState::decode(&state.encode().unwrap()).unwrap();
        assert_eq!(restored.detached_windows[&surface], placement);
        assert!(restored.main_closed);
        assert_eq!(restored.detached_focus, Some(surface));
        let mut legacy = serde_json::to_value(placement).unwrap();
        legacy.as_object_mut().unwrap().remove("sidebar_width_dip");
        assert_eq!(
            serde_json::from_value::<SavedPlacement>(legacy).unwrap().sidebar_width_dip,
            None
        );
        for position in [-1_000_000, 1_000_000] {
            SavedPlacement { left: position, top: position, width: 1, height: 32768, ..placement }
                .validate().unwrap();
        }
        for invalid in [
            SavedPlacement { sidebar_width_dip: Some(MIN_SIDEBAR_WIDTH - 1), ..placement },
            SavedPlacement { sidebar_width_dip: Some(MAX_SIDEBAR_WIDTH + 1), ..placement },
            SavedPlacement { width: 0, ..placement },
            SavedPlacement { height: 0, ..placement },
            SavedPlacement { width: 32769, ..placement },
            SavedPlacement { height: 32769, ..placement },
            SavedPlacement { left: -1_000_001, ..placement },
            SavedPlacement { top: 1_000_001, ..placement },
            SavedPlacement { left: i32::MIN, ..placement },
            SavedPlacement { top: i32::MAX, ..placement },
            SavedPlacement { width: u32::MAX, ..placement },
        ] {
            let mut invalid_state = state.clone();
            invalid_state.detached_windows.insert(surface, invalid);
            assert!(invalid_state.encode().is_err());
            assert!(WindowState::decode(&serde_json::to_vec(&invalid_state).unwrap()).is_err());
        }
        let mut extra = serde_json::to_value(placement).unwrap();
        extra["screen_coordinates"] = true.into();
        assert!(serde_json::from_value::<SavedPlacement>(extra).is_err());

        let mut orphan = state.clone();
        orphan.detached_windows.insert(SurfaceId::new(), placement);
        assert!(orphan.encode().is_err());
        for split in [false, true] {
            let mut invalid = state.clone();
            let added = if split {
                invalid.workspaces[0].split(flowmux_core::SplitDirection::Vertical);
                invalid.workspaces[0].active()
            } else {
                invalid.workspaces[0].new_tab()
            };
            invalid.screens.insert(added, old.screens[&surface].clone());
            assert!(invalid.encode().is_err());
        }
        let mut attached = old.clone();
        attached.main_closed = true;
        assert!(attached.encode().is_err());
        attached.main_closed = false;
        attached.detached_focus = Some(surface);
        assert!(attached.encode().is_err());
        let mut mixed = state.clone();
        let workspace = Workspace::new("attached 한글".into());
        mixed.screens.insert(workspace.active(), old.screens[&surface].clone());
        mixed.workspaces.push(workspace);
        assert!(mixed.encode().is_err());
        mixed.main_closed = false;
        assert!(mixed.encode().is_ok());
        mixed.detached_focus = Some(SurfaceId::new());
        assert!(mixed.encode().is_err());
    }
    #[test]
    fn sidebar_width_preserves_old_state_and_rejects_invalid_saved_preferences() {
        let mut state = sample();
        let old = serde_json::to_value(&state).unwrap();
        assert!(old.get("sidebar_width_dip").is_none());
        assert_eq!(
            WindowState::decode(&serde_json::to_vec(&old).unwrap())
                .unwrap()
                .sidebar_width_dip,
            DEFAULT_SIDEBAR_WIDTH
        );
        for width in [MIN_SIDEBAR_WIDTH, 340, MAX_SIDEBAR_WIDTH] {
            state.sidebar_width_dip = width;
            let encoded = state.encode().unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&encoded).unwrap()["sidebar_width_dip"],
                width
            );
            assert_eq!(WindowState::decode(&encoded).unwrap().sidebar_width_dip, width);
        }
        for width in [MIN_SIDEBAR_WIDTH - 1, MAX_SIDEBAR_WIDTH + 1] {
            state.sidebar_width_dip = width;
            assert!(state.encode().is_err());
            let mut invalid = old.clone();
            invalid["sidebar_width_dip"] = width.into();
            assert!(WindowState::decode(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }
    #[test]
    fn browser_checkpoints_require_history_only_for_terminals() {
        let mut state = sample();
        let source = state.workspaces[0].focused;
        let terminal = state.workspaces[0].active();
        let browser = crate::browser::open(
            &mut state.workspaces[0],
            source,
            "https://example.com/한글".into(),
            false,
        )
        .unwrap();
        let encoded = state.encode().unwrap();
        assert_eq!(WindowState::decode(&encoded).unwrap().screens.len(), 1);
        state
            .screens
            .insert(browser.surface, state.screens[&terminal].clone());
        assert!(state.encode().is_err());
        state.screens.remove(&browser.surface);
        state
            .shells
            .insert(browser.surface, crate::shell::Shell::default());
        assert!(state.encode().is_err());
        state.shells.clear();
        state.workspaces[0].focused = source;
        assert_eq!(state.workspaces[0].close_active(), Some(terminal));
        state.screens.clear();
        assert!(state.encode().is_ok());
        state.workspaces[0].root.set_surface_browser_url(
            browser.pane,
            browser.surface,
            "file:///C:/private".into(),
        );
        assert!(state.encode().is_err());
    }
    #[test]
    fn shell_specs_roundtrip_without_using_history_as_commands_and_old_state_still_loads() {
        let mut state = sample();
        let id = state.workspaces[0].active();
        let mut old = serde_json::to_value(&state).unwrap();
        old.as_object_mut().unwrap().remove("shells");
        assert!(WindowState::decode(&serde_json::to_vec(&old).unwrap())
            .unwrap()
            .shells
            .is_empty());
        let shell = crate::shell::Shell {
            program: "C:\\한글\\shell.exe".into(),
            args: vec!["".into(), "한 😀 & \" \\".into()],
        };
        state.shells.insert(id, shell.clone());
        assert_eq!(
            WindowState::decode(&state.encode().unwrap())
                .unwrap()
                .shells[&id],
            shell
        );
        state.shells.insert(SurfaceId::new(), shell);
        assert!(state.encode().is_err());
    }
    #[test]
    fn old_state_without_color_loads_and_custom_metadata_roundtrips_without_normalization() {
        let mut state = sample();
        let old = serde_json::to_value(&state).unwrap();
        assert!(old["workspaces"][0].get("color").is_none());
        assert!(WindowState::decode(&serde_json::to_vec(&old).unwrap())
            .unwrap()
            .workspaces[0]
            .color
            .is_none());
        state.workspaces[0].color = Some("#1234ab".into());
        state.workspaces[0].name = "한글 한 e\u{301} 😀 &".into();
        let pane = state.workspaces[0].focused;
        let surface = state.workspaces[0].active();
        state.workspaces[0]
            .root
            .rename_surface(pane, surface, "사용자 이름".into());
        state.workspaces[0]
            .root
            .set_surface_title_auto(pane, surface, "automatic".into());
        let decoded = WindowState::decode(&state.encode().unwrap()).unwrap();
        assert_eq!(decoded.workspaces[0].name, state.workspaces[0].name);
        assert_eq!(decoded.workspaces[0].color, state.workspaces[0].color);
        assert_eq!(
            decoded.workspaces[0].root.surface_title(pane, surface),
            Some("사용자 이름")
        );
        for color in ["", "red", "#12zz99"] {
            state.workspaces[0].color = Some(color.into());
            assert!(state.encode().is_err());
        }
    }
    #[test]
    fn validates_whole_layout_before_restore_and_preserves_styled_korean() {
        let state = sample();
        let restored = WindowState::decode(&state.encode().unwrap()).unwrap();
        assert_eq!(
            restored.screens[&restored.workspaces[0].active()].data,
            "\x1b[32m한글\x1b[0m"
        );
        for change in [0, 1, 2, 3, 4] {
            let mut invalid = state.clone();
            match change {
                0 => invalid.version += 1,
                1 => invalid.workspaces.push(invalid.workspaces[0].clone()),
                2 => invalid.workspaces[0].focused = flowmux_core::PaneId::new(),
                3 => invalid.screens.clear(),
                _ => {
                    invalid.screens.values_mut().next().unwrap().data =
                        "x".repeat(MAX_SCREEN_BYTES + 1)
                }
            }
            assert!(invalid.encode().is_err());
        }
    }
}
