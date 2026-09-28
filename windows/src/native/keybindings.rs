// SPDX-License-Identifier: GPL-3.0-or-later
//! Terminal shortcuts are authorized against the renderer's acknowledged settings.
use super::*;
use crate::keybindings::{ActionId, Chord};

impl App {
    #[cfg(debug_assertions)]
    pub(super) fn test_terminal_menu(
        &mut self,
        source: SurfaceId,
        event: &str,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.background_test,
            "terminal menu tests require an owned hidden debug host"
        );
        anyhow::ensure!(
            event.len() <= 2048,
            "terminal menu test event exceeds limit"
        );
        let event: Value = serde_json::from_str(event)?;
        let fields = event
            .as_object()
            .context("terminal menu event must be an object")?;
        anyhow::ensure!(
            matches!(event["action"].as_str(), Some("open" | "click" | "key")),
            "invalid terminal menu test action"
        );
        for (key, value) in fields {
            let valid = match key.as_str() {
                "action" => true,
                "item" => matches!(
                    value.as_str(),
                    Some("split_right" | "split_down" | "close_pane")
                ),
                "key" => matches!(
                    value.as_str(),
                    Some("ArrowUp" | "ArrowDown" | "Home" | "End" | "Escape" | "Tab")
                ),
                "shiftKey" | "isComposing" => value.is_boolean(),
                "keyCode" => value.as_u64().is_some_and(|code| code <= 255),
                "x" | "y" => value
                    .as_u64()
                    .is_some_and(|coordinate| coordinate <= 1000000),
                _ => false,
            };
            anyhow::ensure!(valid, "invalid terminal menu event field: {key}");
        }
        anyhow::ensure!(
            event["action"] != "click" || event.get("item").is_some(),
            "menu click requires an item"
        );
        anyhow::ensure!(
            event["action"] != "key" || event.get("key").is_some(),
            "menu key requires a key"
        );
        anyhow::ensure!(
            self.pending_terminal_ui_tests.len() < 2,
            "terminal UI test already pending"
        );
        let terminal = self
            .surfaces
            .get(&source)
            .context("terminal menu test requires a terminal surface")?;
        anyhow::ensure!(
            terminal.ready && !terminal.restoring,
            "terminal menu test terminal is not ready"
        );
        let request = Uuid::new_v4();
        terminal.send(&HostMessage::TestTerminalMenu { request, event })?;
        self.pending_terminal_ui_tests.insert(
            request,
            PendingRead {
                surface: source,
                after: 0,
                reply,
                started: Instant::now(),
            },
        );
        Ok(())
    }

    pub(super) fn shortcut(
        &mut self,
        source: SurfaceId,
        action: &str,
        chord: &Chord,
        revision: Uuid,
    ) -> anyhow::Result<()> {
        let Some(surface) = self.surfaces.get(&source) else {
            return Ok(());
        };
        if self.current_surface() != Some(source)
            || !surface.visible
            || !surface.ready
            || surface.restoring
            || self.close_accepted
            || self.close_request.is_some()
            || self.editor_barrier.is_some()
            || self.overview.is_open()
            || revision != self.settings.revision
            || surface
                .applied_settings
                .as_ref()
                .and_then(|s| s.get("revision"))
                != Some(&json!(revision))
        {
            return Ok(());
        }
        if !crate::keybindings::resolved(&self.settings.keybindings)?
            .iter()
            .any(|binding| binding.action == action && &binding.chord == chord)
        {
            return Ok(());
        }
        let Some(action) = ActionId::from_wire(action) else {
            return Ok(());
        };
        self.keybinding_action(source, action)
    }

    pub(super) fn keybinding_action(
        &mut self,
        source: SurfaceId,
        action: ActionId,
    ) -> anyhow::Result<()> {
        let (source_workspace, _, _) = self
            .locate(source)
            .context("No active workspace for shortcut source")?;
        anyhow::ensure!(
            !self.close_accepted
                && self.close_request.is_none()
                && self.editor_barrier.is_none()
                && !self.overview.is_open()
                && !self.command_palette.is_open(),
            "window is busy"
        );
        use ActionId::*;
        if self.detached.contains_key(&source) {
            match action {
                CloseSurface => return self.close_detached(source),
                TerminalSearch => {
                    self.surfaces
                        .get(&source)
                        .context("Shortcut requires a terminal surface")?
                        .send(&HostMessage::OpenFind {
                            focus: !self.background_test,
                        })?;
                    return Ok(());
                }
                QuitApp => return self.request_close(CloseRequest::Native),
                _ => return Ok(()), // Single-surface window tools match Linux's disabled controls.
            }
        }
        let action = action.as_str();
        let action = match ActionId::from_wire(action) {
            Some(SplitRight) => Action::Vertical,
            Some(SplitDown) => Action::Horizontal,
            Some(FocusLeft) => {
                return self
                    .focus_direction(source, FocusDirection::Left)
                    .map(|_| ())
            }
            Some(FocusRight) => {
                return self
                    .focus_direction(source, FocusDirection::Right)
                    .map(|_| ())
            }
            Some(FocusUp) => return self.focus_direction(source, FocusDirection::Up).map(|_| ()),
            Some(FocusDown) => {
                return self
                    .focus_direction(source, FocusDirection::Down)
                    .map(|_| ())
            }
            Some(CloseSurface) => Action::CloseTab,
            Some(QuitApp) => return self.request_close(CloseRequest::Native),
            Some(NextSurface | PrevSurface) => {
                let previous = ActionId::from_wire(action) == Some(PrevSurface);
                let (_, pane, _) = self.locate(source).context("shortcut source disappeared")?;
                let (_, _, tabs) = self.workspaces[source_workspace]
                    .leaves()
                    .into_iter()
                    .find(|(id, _, _)| *id == pane)
                    .context("shortcut pane disappeared")?;
                let current = tabs
                    .iter()
                    .position(|tab| tab.id == source)
                    .context("shortcut tab disappeared")?;
                let next = (current + if previous { tabs.len() - 1 } else { 1 }) % tabs.len();
                self.select(tabs[next].id)?;
                return self.rebuild();
            }
            Some(NextWorkspace | PrevWorkspace) => {
                let previous = ActionId::from_wire(action) == Some(PrevWorkspace);
                let indices = self.main_workspace_indices();
                if indices.is_empty() {
                    return Ok(());
                }
                let at = indices
                    .iter()
                    .position(|i| *i == self.active_workspace)
                    .unwrap_or(0);
                let next = (at + if previous { indices.len() - 1 } else { 1 }) % indices.len();
                Action::Workspace(self.workspaces[indices[next]].id)
            }
            Some(
                Workspace1 | Workspace2 | Workspace3 | Workspace4 | Workspace5 | Workspace6
                | Workspace7 | Workspace8,
            ) => {
                let index = action
                    .rsplit('-')
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                let indices = self.main_workspace_indices();
                let Some(workspace) = index
                    .checked_sub(1)
                    .and_then(|i| indices.get(i))
                    .map(|i| &self.workspaces[*i])
                else {
                    return Ok(());
                };
                Action::Workspace(workspace.id)
            }
            Some(CommandPalette) => Action::CommandPalette,
            Some(NewSurface) => Action::NewTab,
            Some(NewBrowserSurface) => Action::NewBrowser,
            Some(NewWorkspace) => Action::NewWorkspace,
            Some(TerminalSearch) => Action::Find,
            Some(SearchAllTerminals) => Action::SearchAll,
            Some(TogglePaneZoom) => Action::TogglePaneZoom,
            Some(ToggleWorkspaceOverview) => Action::Overview,
            Some(ToggleFileBrowser) => Action::ShowFiles,
            _ => return Ok(()),
        };
        self.action(action)
    }

    #[cfg(debug_assertions)]
    pub(super) fn test_shortcut(
        &mut self,
        source: SurfaceId,
        event: &str,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.background_test,
            "shortcut tests require an owned hidden debug host"
        );
        anyhow::ensure!(event.len() <= 2048, "shortcut test event exceeds limit");
        let event: Value = serde_json::from_str(event)?;
        let fields = event
            .as_object()
            .context("shortcut test event must be an object")?;
        for (key, value) in fields {
            let valid = match key.as_str() {
                "type" => matches!(value.as_str(), Some("keydown" | "keyup")),
                "key" | "code" => value.as_str().is_some_and(|s| s.len() <= 64),
                "ctrlKey" | "altKey" | "shiftKey" | "metaKey" | "repeat" | "isComposing"
                | "altGraph" => value.is_boolean(),
                "keyCode" => value.as_u64().is_some_and(|n| n <= 255),
                _ => false,
            };
            anyhow::ensure!(valid, "invalid shortcut test event field: {key}");
        }
        anyhow::ensure!(
            self.pending_terminal_ui_tests.len() < 2,
            "shortcut test already pending"
        );
        let surface = self
            .surfaces
            .get(&source)
            .context("shortcut test requires a terminal surface")?;
        anyhow::ensure!(
            surface.ready && !surface.restoring,
            "shortcut test terminal is not ready"
        );
        let request = Uuid::new_v4();
        surface.send(&HostMessage::TestShortcut { request, event })?;
        self.pending_terminal_ui_tests.insert(
            request,
            PendingRead {
                surface: source,
                after: 0,
                reply,
                started: Instant::now(),
            },
        );
        Ok(())
    }
}
