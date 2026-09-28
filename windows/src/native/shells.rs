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
