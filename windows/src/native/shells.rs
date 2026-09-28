// SPDX-License-Identifier: GPL-3.0-or-later
use super::super::shell;
use super::*;
use crate::shell::Shell;

pub(super) enum NewTerminal {
    Tab,
    Workspace,
    Split(SplitDirection),
}
impl App {
    pub(super) fn open_tig(&mut self, source: SurfaceId) -> anyhow::Result<()> {
        let (workspace, _, _) = self.locate(source).context("Tig source no longer exists")?;
        anyhow::ensure!(
            !self.closing
                && !self.close_accepted
                && self.close_request.is_none()
                && self.pending_save.is_none()
                && self.editor_barrier.is_none()
                && !self.overview.is_open()
                && !self.command_palette.is_open()
                && unsafe { IsWindowEnabled(self.surface_window(source)) } != 0
                && !self
                    .ssh_auth_window
                    .as_ref()
                    .is_some_and(|(id, _)| *id == source),
            "Window is busy"
        );
        anyhow::ensure!(
            !self
                .ssh_disconnected
                .contains(&self.workspaces[workspace].id),
            "Connect the SSH workspace before opening Tig"
        );
        // The ordinary new-tab path owns cwd, default shell and detached guards.
        let id = self.new_terminal(source, None, None, NewTerminal::Tab)?;
        self.surfaces
            .get_mut(&id)
            .context("Tig terminal was not created")?
            .pending_tig = true;
        self.flush_pending_tig(id)
    }

    pub(super) fn flush_pending_tig(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        let remote = self.remote_directory(id).is_some();
        let Some(surface) = self.surfaces.get_mut(&id) else {
            return Ok(());
        };
        if !surface.pending_tig
            || !surface.ready
            || surface.restoring
            || surface.session.is_none()
            || surface.exit_code.is_some()
            || (remote && !surface.ssh_connected)
        {
            return Ok(());
        }
        // Consume before enqueueing: a failed write must never replay on reconnect.
        if std::mem::take(&mut surface.pending_tig) {
            surface.session.as_ref().unwrap().input(b"tig\r".to_vec())?;
        }
        Ok(())
    }

    pub(super) fn new_workspace(
        &mut self,
        caller: Option<SurfaceId>,
        cwd: Option<PathBuf>,
        requested: Option<Shell>,
    ) -> anyhow::Result<SurfaceId> {
        if let Some(source) = caller.or_else(|| self.current_surface()) {
            return self.new_terminal(source, cwd, requested, NewTerminal::Workspace);
        }
        anyhow::ensure!(
            self.close_request.is_none() && !self.close_accepted && self.editor_barrier.is_none(),
            "Window is busy"
        );
        if let Some(path) = &cwd {
            anyhow::ensure!(
                path.is_absolute()
                    || (!path.has_root()
                        && !matches!(
                            path.components().next(),
                            Some(std::path::Component::Prefix(_))
                        )),
                "ambiguous cwd; use an absolute Windows path"
            );
        }
        let cwd = std::path::absolute(cwd.map_or_else(
            || self.initial_cwd.clone(),
            |path| self.initial_cwd.join(path),
        ))?;
        anyhow::ensure!(cwd.is_dir(), "working directory does not exist");
        let spec = requested.unwrap_or_else(|| self.settings.default_shell.clone());
        shell::resolve(&spec)?;
        let workspace = Workspace::new(cwd);
        let surface = workspace.active();
        self.workspaces.push(workspace);
        self.active_workspace = self.workspaces.len() - 1;
        self.detached_focus = None;
        self.shells.insert(surface, spec);
        self.rebuild()?;
        Ok(surface)
    }
    pub(super) fn new_terminal(
        &mut self,
        source: SurfaceId,
        cwd: Option<PathBuf>,
        requested: Option<Shell>,
        kind: NewTerminal,
    ) -> anyhow::Result<SurfaceId> {
        anyhow::ensure!(
            self.close_request.is_none(),
            "window is saving before close"
        );
        if !matches!(kind, NewTerminal::Workspace) {
            self.ensure_attached(source)?;
        }
        let (workspace, pane, fallback) = self.locate(source).context("source tab missing")?;
        if self.workspaces[workspace].ssh.is_some() && !matches!(kind, NewTerminal::Workspace) {
            anyhow::ensure!(requested.is_none(), "SSH tabs use their workspace connection; create a local workspace for a Windows shell");
            return self.new_ssh_terminal(source, cwd, kind);
        }
        let source_cwd = if self.browsers.contains_key(&source) {
            self.workspaces[workspace]
                .root
                .terminal_surface_cwd(pane)
                .unwrap_or(fallback)
        } else {
            // Explicit/calling inactive terminals retain their own cwd; the
            // pane's active tab can point at an unrelated directory.
            fallback
        };
        let cwd = match cwd {
            Some(path) if !path.is_absolute() => {
                anyhow::ensure!(
                    !path.has_root()
                        && !matches!(
                            path.components().next(),
                            Some(std::path::Component::Prefix(_))
                        ),
                    "ambiguous cwd; use an absolute Windows path"
                );
                std::path::absolute(source_cwd.join(path))?
            }
            Some(path) => path,
            None => source_cwd,
        };
        anyhow::ensure!(cwd.is_dir(), "working directory does not exist");
        let spec = requested.unwrap_or_else(|| match kind {
            NewTerminal::Split(_) => self
                .shells
                .get(&source)
                .cloned()
                .unwrap_or_else(|| self.settings.default_shell.clone()),
            _ => self.settings.default_shell.clone(),
        });
        shell::resolve(&spec)?; // Invalid requests leave layout and focus untouched.
        if !matches!(kind, NewTerminal::Workspace) || !self.detached.contains_key(&source) {
            self.select(source)?;
        }
        match kind {
            NewTerminal::Tab => {
                self.workspace_mut().new_tab();
            }
            NewTerminal::Workspace => {
                self.zoomed = None;
                self.workspaces.push(Workspace::new(cwd.clone()));
                self.active_workspace = self.workspaces.len() - 1;
            }
            NewTerminal::Split(direction) => {
                self.zoomed = None;
                self.workspace_mut().split(direction);
            }
        }
        self.detached_focus = None;
        if self.main_closed {
            self.main_closed = false;
            if !self.background_test {
                unsafe {
                    ShowWindow(self.window, SW_SHOWNOACTIVATE);
                }
            }
        }
        let id = self.active();
        let pane = self.workspace().focused;
        self.workspace_mut().root.set_surface_cwd(pane, id, cwd);
        self.shells.insert(id, spec);
        self.rebuild()?;
        Ok(id)
    }
    pub(super) fn retry_shell(
        &mut self,
        id: SurfaceId,
        shell: Option<Shell>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.close_request.is_none(),
            "window is saving before close"
        );
        let surface = self.surfaces.get(&id).context("terminal missing")?;
        anyhow::ensure!(
            self.remote_directory(id).is_none() || shell.is_none(),
            "An SSH terminal cannot be replaced with a local shell"
        );
        anyhow::ensure!(
            surface.startup_error.is_some() && surface.session.is_none(),
            "only a failed shell startup can be retried"
        );
        let shell = shell.unwrap_or_else(|| self.shells[&id].clone());
        super::super::shell::resolve(&shell)?;
        let (workspace, pane, _) = self.locate(id).context("terminal missing")?;
        self.workspaces[workspace]
            .root
            .set_surface_title_auto(pane, id, shell.program.clone());
        self.refresh_surface_metadata(id);
        self.shells.insert(id, shell);
        self.start_session(id)?;
        if let Some(error) = &self.surfaces[&id].startup_error {
            anyhow::bail!("Shell retry failed: {error}");
        }
        Ok(())
    }
    fn new_ssh_terminal(
        &mut self,
        source: SurfaceId,
        cwd: Option<PathBuf>,
        kind: NewTerminal,
    ) -> anyhow::Result<SurfaceId> {
        let (index, pane, _) = self.locate(source).context("SSH source missing")?;
        let mut workspace = self.workspaces[index].clone();
        workspace.focused = pane;
        workspace.root.set_active_surface(pane, source);
        match kind {
            NewTerminal::Tab => {
                workspace.new_tab();
            }
            NewTerminal::Split(direction) => {
                workspace.split(direction);
            }
            NewTerminal::Workspace => unreachable!(),
        }
        let id = workspace.active();
        let mut tab = workspace
            .root
            .find_surface(workspace.focused, id)
            .context("SSH terminal missing")?;
        let SurfaceKind::SshTerminal {
            cwd: remote,
            tmux_session,
        } = &mut tab.kind
        else {
            unreachable!()
        };
        if let Some(cwd) = cwd {
            *remote = Some(
                cwd.to_str()
                    .context("Remote directory must be UTF-8")?
                    .to_owned(),
            );
        }
        let shell = crate::ssh::terminal_shell(
            workspace.ssh.as_ref().unwrap(),
            remote.as_deref(),
            tmux_session.as_deref(),
            false,
        )?;
        super::super::shell::resolve(&shell)?;
        crate::ssh::set_remote_cwd(&mut workspace.root, workspace.focused, id, remote.clone());
        self.workspaces[index] = workspace;
        self.active_workspace = index;
        self.detached_focus = None;
        self.zoomed = None;
        self.shells.insert(id, shell);
        self.rebuild()?;
        Ok(id)
    }
    pub(super) fn shell_menu(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
        let labels = [
            "New Windows PowerShell tab",
            "New Command Prompt tab",
            "New PowerShell 7 tab",
        ];
        let choice = self.popup(&labels, &[], point)?;
        if let Some(name) = ["powershell", "cmd", "pwsh"].get(choice.wrapping_sub(1)) {
            self.new_terminal(
                self.active(),
                None,
                Some(Shell::profile(name)),
                NewTerminal::Tab,
            )?;
        }
        self.focus_active()
    }
}
