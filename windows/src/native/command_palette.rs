// SPDX-License-Identifier: GPL-3.0-or-later
//! Search existing commands and live workspace/pane/tab identities.
use super::*;
use crate::keybindings::ActionId;
use std::collections::VecDeque;
#[path = "command_palette_panel.rs"]
mod panel;
pub(super) use panel::UiAction;

#[derive(Clone)]
pub(super) enum Target {
    Native(Action),
    Keybinding(ActionId),
    Workspace(WorkspaceId),
    Pane(WorkspaceId, PaneId),
    Surface(SurfaceId),
    Metadata(workspaces::EditTarget),
}
#[derive(Clone)]
pub(super) struct Entry {
    pub(super) id: String,
    pub(super) label: String,
    pub(super) shortcut: String,
    pub(super) target: Target,
}
#[derive(Default)]
pub(super) struct Controller {
    panel: Option<panel::Panel>,
    recent: VecDeque<String>,
}
impl Controller {
    pub(super) fn is_open(&self) -> bool {
        self.panel.as_ref().is_some_and(panel::Panel::is_open)
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|p| p.handle_message(message))
    }
    pub(super) fn diagnostics(&self) -> Value {
        self.panel
            .as_ref()
            .map_or(Value::Null, panel::Panel::diagnostics)
    }
}
impl App {
    fn palette_entries(&self) -> anyhow::Result<Vec<Entry>> {
        let mut entries = Vec::new();
        if let Some((workspace, _, _)) = self
            .current_surface()
            .and_then(|surface| self.locate(surface))
        {
            let workspace = self.workspaces[workspace].id;
            for (id, label, target) in [
                (
                    "workspace-name",
                    "Rename workspace…",
                    workspaces::EditTarget::WorkspaceName(workspace),
                ),
                (
                    "workspace-color",
                    "Workspace color…",
                    workspaces::EditTarget::WorkspaceColor(workspace),
                ),
                (
                    "tab-name",
                    "Rename tab…",
                    workspaces::EditTarget::TabName(self.active()),
                ),
            ] {
                entries.push(Entry {
                    id: format!("metadata:{id}"),
                    label: label.into(),
                    shortcut: String::new(),
                    target: Target::Metadata(target),
                });
            }
        }
        for (id, label, action) in [
            (
                "new-ssh-workspace",
                "New SSH Workspace",
                Action::NewSshWorkspace,
            ),
            ("settings", "Options", Action::Settings),
            ("open-file", "Open file…", Action::OpenEditor),
            ("notifications", "Notifications", Action::Notifications),
            ("move-tab", "Move tab…", Action::MoveTabMenu),
            ("detach-tab", "Move to new window", Action::DetachTab),
        ] {
            if self.current_surface().is_none() && !Self::empty_action(&action) {
                continue;
            }
            entries.push(Entry {
                id: format!("native:{id}"),
                label: label.into(),
                shortcut: String::new(),
                target: Target::Native(action),
            });
        }
        for binding in crate::keybindings::catalog(&self.settings.keybindings)? {
            if !binding.supported || binding.action == ActionId::CommandPalette.as_str() {
                continue;
            }
            let Some(action) = ActionId::from_wire(&binding.action) else {
                continue;
            };
            if self
                .validate_palette_target(&Target::Keybinding(action))
                .is_err()
            {
                continue;
            }
            entries.push(Entry {
                id: format!("action:{}", binding.action),
                label: binding.label,
                shortcut: binding.accels.join(", "),
                target: Target::Keybinding(action),
            });
        }
        for workspace in &self.workspaces {
            entries.push(Entry {
                id: format!("workspace:{}", workspace.id),
                label: format!("Workspace: {}", workspace.name),
                shortcut: String::new(),
                target: Target::Workspace(workspace.id),
            });
            for (pane, active, tabs) in workspace.leaves() {
                let title = tabs
                    .iter()
                    .find(|tab| tab.id == active)
                    .map_or("Pane", |tab| tab.title.as_str())
                    .to_owned();
                entries.push(Entry {
                    id: format!("pane:{pane}"),
                    label: format!("Pane: {} / {title}", workspace.name),
                    shortcut: String::new(),
                    target: Target::Pane(workspace.id, pane),
                });
                for tab in tabs {
                    entries.push(Entry {
                        id: format!("surface:{}", tab.id),
                        label: format!("Tab: {} / {title} / {}", workspace.name, tab.title),
                        shortcut: String::new(),
                        target: Target::Surface(tab.id),
                    });
                }
            }
        }
        entries.sort_by_key(|entry| {
            self.command_palette
                .recent
                .iter()
                .position(|id| id == &entry.id)
                .unwrap_or(usize::MAX)
        });
        Ok(entries)
    }
    fn validate_palette_target(&self, target: &Target) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.close_accepted && self.close_request.is_none(),
            "window is saving before close"
        );
        anyhow::ensure!(
            self.editor_barrier.is_none(),
            "editor synchronization is in progress"
        );
        if self.current_surface().is_none() {
            match target {
                Target::Keybinding(action) => {
                    anyhow::ensure!(
                        matches!(action, ActionId::NewWorkspace | ActionId::QuitApp),
                        "No active workspace"
                    );
                    return Ok(());
                }
                Target::Native(action) => {
                    anyhow::ensure!(Self::empty_action(action), "No active workspace")
                }
                _ => {}
            }
        }
        match target {
            Target::Metadata(
                workspaces::EditTarget::WorkspaceName(id)
                | workspaces::EditTarget::WorkspaceColor(id),
            ) => {
                self.workspace_index(*id)?;
            }
            Target::Metadata(workspaces::EditTarget::TabName(id)) => {
                anyhow::ensure!(self.locate(*id).is_some(), "Tab no longer exists");
            }
            Target::Keybinding(ActionId::TerminalSearch) => {
                anyhow::ensure!(
                    self.surfaces
                        .get(&self.active())
                        .is_some_and(|s| s.ready && !s.restoring),
                    "Find requires a ready terminal"
                );
            }
            Target::Keybinding(action) if action.as_str().starts_with("workspace-") => {
                let index = action
                    .as_str()
                    .rsplit('-')
                    .next()
                    .and_then(|n| n.parse::<usize>().ok())
                    .unwrap_or(0);
                anyhow::ensure!(
                    index > 0 && index <= self.workspaces.len(),
                    "Workspace no longer exists"
                );
            }
            Target::Workspace(id) => {
                self.workspace_index(*id)?;
            }
            Target::Pane(workspace, pane) => {
                let index = self.workspace_index(*workspace)?;
                anyhow::ensure!(
                    self.workspaces[index]
                        .root
                        .active_surface_id(*pane)
                        .is_some(),
                    "Pane no longer exists"
                );
            }
            Target::Surface(id) => {
                anyhow::ensure!(self.locate(*id).is_some(), "Tab no longer exists");
            }
            _ => {}
        }
        Ok(())
    }
    pub(super) fn command_palette_ui(&mut self, signal: UiAction) -> anyhow::Result<()> {
        match signal {
            UiAction::Show => {
                if self.command_palette.is_open() {
                    return Ok(());
                }
                anyhow::ensure!(
                    !self.overview.is_open()
                        && self.editor_barrier.is_none()
                        && self.close_request.is_none()
                        && !self.close_accepted,
                    "window is busy"
                );
                anyhow::ensure!(
                    unsafe {
                        windows_sys::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(
                            self.window,
                        )
                    } != 0,
                    "another dialog is open"
                );
                let entries = self.palette_entries()?;
                if self.command_palette.panel.is_none() {
                    self.command_palette.panel = Some(panel::Panel::new(self.window)?);
                }
                self.command_palette
                    .panel
                    .as_mut()
                    .unwrap()
                    .show(entries, self.background_test);
            }
            UiAction::Changed => {
                if let Some(panel) = self.command_palette.panel.as_mut() {
                    panel.changed();
                }
            }
            UiAction::Layout => {
                if let Some(panel) = &self.command_palette.panel {
                    panel.layout();
                }
            }
            UiAction::Close => {
                if let Some(panel) = self.command_palette.panel.as_mut() {
                    panel.hide(self.background_test);
                }
                self.focus_active()?;
            }
            UiAction::Activate => {
                let Some(entry) = self
                    .command_palette
                    .panel
                    .as_ref()
                    .and_then(panel::Panel::selected)
                else {
                    return Ok(());
                };
                if let Err(error) = self.validate_palette_target(&entry.target) {
                    self.command_palette
                        .panel
                        .as_ref()
                        .unwrap()
                        .status(&error.to_string());
                    return Ok(());
                }
                self.command_palette
                    .panel
                    .as_mut()
                    .unwrap()
                    .hide(self.background_test);
                match entry.target {
                    Target::Metadata(target) => self.edit_metadata(target)?,
                    Target::Native(action) => self.action(action)?,
                    Target::Keybinding(ActionId::NewWorkspace) => {
                        self.action(Action::NewWorkspace)?
                    }
                    Target::Keybinding(ActionId::QuitApp) => {
                        self.request_close(CloseRequest::Native)?
                    }
                    Target::Keybinding(action) => {
                        self.keybinding_action(self.target(None, None)?, action)?
                    }
                    Target::Workspace(id) => self.action(Action::Workspace(id))?,
                    Target::Surface(id) => {
                        self.select(id)?;
                        self.rebuild()?;
                    }
                    Target::Pane(workspace, pane) => {
                        let index = self.workspace_index(workspace)?;
                        let surface = self.workspaces[index]
                            .root
                            .active_surface_id(pane)
                            .context("Pane no longer exists")?;
                        self.select(surface)?;
                        self.rebuild()?;
                    }
                }
                self.command_palette.recent.retain(|id| id != &entry.id);
                self.command_palette.recent.push_front(entry.id);
                self.command_palette.recent.truncate(20);
            }
        }
        Ok(())
    }
}
