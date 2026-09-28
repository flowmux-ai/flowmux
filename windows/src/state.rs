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
    pub active_workspace: WorkspaceId,
    pub screens: HashMap<SurfaceId, SavedScreen>,
    #[serde(default)]
    pub shells: HashMap<SurfaceId, crate::shell::Shell>,
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
            !self.workspaces.is_empty() && self.workspaces.len() <= 64,
            "invalid workspace count"
        );
        ensure!(
            self.workspaces
                .iter()
                .any(|w| w.id == self.active_workspace),
            "active workspace missing"
        );
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
        active_workspace: ws.id,
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
    }
}
#[cfg(test)]
mod tests {
    use super::*;
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
