// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

impl App {
    pub(super) fn show_ssh_dialog(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.ssh_dialog.is_none()
                && !self.main_closed
                && self.close_request.is_none()
                && !self.close_accepted
                && self.editor_barrier.is_none()
                && unsafe { IsWindowEnabled(self.window) } != 0,
            "Window is busy"
        );
        self.tab_menu.take();
        self.ssh_dialog = Some(ssh_panel::Panel::new(self.window, self.background_test)?);
        Ok(())
    }

    pub(super) fn ssh_dialog_action(
        &mut self,
        id: Uuid,
        action: ssh_panel::UiAction,
    ) -> anyhow::Result<()> {
        let Some(panel) = self.ssh_dialog.as_ref().filter(|panel| panel.id == id) else {
            return Ok(());
        };
        match action {
            ssh_panel::UiAction::Close => {
                self.ssh_dialog.take();
            }
            ssh_panel::UiAction::Layout => panel.relayout(),
            ssh_panel::UiAction::Connect => {
                let result = panel
                    .values()
                    .and_then(|(name, config)| self.new_ssh_workspace(name, config));
                match result {
                    Ok(_) => {
                        self.ssh_dialog.take();
                        self.focus_active()?;
                    }
                    Err(error) => self.ssh_dialog.as_ref().unwrap().error(&error.to_string()),
                }
            }
        }
        Ok(())
    }

    fn new_ssh_workspace(
        &mut self,
        name: Option<String>,
        config: flowmux_core::SshWorkspaceConfig,
    ) -> anyhow::Result<SurfaceId> {
        anyhow::ensure!(
            self.close_request.is_none() && !self.close_accepted && self.editor_barrier.is_none(),
            "Window is busy"
        );
        let local = self
            .current_surface()
            .and_then(|id| self.locate(id))
            .map(|(_, _, cwd)| cwd)
            .filter(|cwd| cwd.is_dir())
            .unwrap_or_else(|| self.initial_cwd.clone());
        let workspace = Workspace::new_ssh(local, config, name)?;
        let surface = workspace.active();
        let tab = workspace
            .root
            .find_surface(workspace.focused, surface)
            .context("SSH terminal missing")?;
        let SurfaceKind::SshTerminal { cwd, tmux_session } = &tab.kind else {
            unreachable!()
        };
        let shell = crate::ssh::terminal_shell(
            workspace.ssh.as_ref().unwrap(),
            cwd.as_deref(),
            tmux_session.as_deref(),
        )?;
        super::super::shell::resolve(&shell)?;
        self.workspaces.push(workspace);
        self.active_workspace = self.workspaces.len() - 1;
        self.detached_focus = None;
        self.zoomed = None;
        self.shells.insert(surface, shell);
        self.rebuild_without_focus()?;
        Ok(surface)
    }

    pub(super) fn ssh_shell(&self, id: SurfaceId) -> anyhow::Result<Option<crate::shell::Shell>> {
        let (index, pane, _) = self.locate(id).context("Terminal missing")?;
        let workspace = &self.workspaces[index];
        let Some(tab) = workspace.root.find_surface(pane, id) else {
            return Ok(None);
        };
        let SurfaceKind::SshTerminal { cwd, tmux_session } = &tab.kind else {
            return Ok(None);
        };
        let config = workspace
            .ssh
            .as_ref()
            .context("SSH workspace configuration missing")?;
        crate::ssh::terminal_shell(config, cwd.as_deref(), tmux_session.as_deref()).map(Some)
    }

    // Outer Option identifies a remote terminal; inner None means login home.
    pub(super) fn remote_directory(&self, id: SurfaceId) -> Option<Option<String>> {
        let (index, pane, _) = self.locate(id)?;
        match &self.workspaces[index].root.find_surface(pane, id)?.kind {
            SurfaceKind::SshTerminal { cwd, .. } => Some(cwd.clone()),
            _ => None,
        }
    }

    pub(super) fn update_remote_directory(&mut self, id: SurfaceId, path: String) {
        if path.len() > 32767 || flowmux_core::ssh::validate_remote_cwd(Some(&path)).is_err() {
            return;
        }
        let Some((index, pane, _)) = self.locate(id) else {
            return;
        };
        if !crate::ssh::set_remote_cwd(&mut self.workspaces[index].root, pane, id, Some(path)) {
            return;
        }
        if let Some(surface) = self.surfaces.get_mut(&id) {
            surface.cwd_reported = true;
        }
        self.refresh_chrome_metadata();
    }
}
