// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_core::SshForwardSpec;

impl App {
    fn ssh_port_rows(&self, workspace: WorkspaceId) -> Vec<ssh_ports_panel::Row> {
        self.workspaces
            .iter()
            .find(|w| w.id == workspace)
            .and_then(|w| w.ssh.as_ref())
            .into_iter()
            .flat_map(|c| &c.forwards)
            .map(|spec| {
                let runtime = self
                    .ssh_forwards
                    .get(&spec.id)
                    .filter(|f| f.workspace == workspace);
                ssh_ports_panel::Row {
                    id: spec.id,
                    remote_port: spec.remote_port,
                    local_port: runtime.filter(|f| f.state() == "connected").map(|f| f.port),
                    state: runtime.map_or("disconnected", |f| f.state()).into(),
                    error: runtime.and_then(|f| f.error.clone()),
                    authentication_available: runtime.is_some(),
                }
            })
            .collect()
    }

    pub(super) fn show_ssh_ports(&mut self, workspace: WorkspaceId) -> anyhow::Result<()> {
        self.ssh_lifecycle_guard()?;
        anyhow::ensure!(
            self.workspaces
                .iter()
                .any(|w| w.id == workspace && w.ssh.is_some()),
            "SSH workspace no longer exists"
        );
        let panel = ssh_ports_panel::Panel::new(
            self.window,
            workspace,
            self.background_test,
            self.ssh_port_rows(workspace),
        )?;
        self.ssh_ports = Some(panel);
        Ok(())
    }

    pub(super) fn ssh_ports_action(
        &mut self,
        id: Uuid,
        action: ssh_ports_panel::UiAction,
    ) -> anyhow::Result<()> {
        let Some(panel) = self.ssh_ports.as_ref().filter(|p| p.id == id) else {
            return Ok(());
        };
        let workspace = panel.workspace;
        let result = match action {
            ssh_ports_panel::UiAction::Close => {
                self.ssh_ports.take();
                return Ok(());
            }
            ssh_ports_panel::UiAction::Layout => {
                panel.relayout();
                return Ok(());
            }
            ssh_ports_panel::UiAction::Add => panel
                .draft()
                .and_then(|spec| self.ssh_forward_add(workspace, spec)),
            ssh_ports_panel::UiAction::Remove(forward) => {
                self.ssh_forward_remove(workspace, forward)
            }
            ssh_ports_panel::UiAction::Preview(forward) => {
                self.open_ssh_preview(workspace, forward)
            }
            ssh_ports_panel::UiAction::Authentication(forward) => {
                self.ssh_forward_authentication(workspace, forward)
            }
        };
        let rows = self.ssh_port_rows(workspace);
        if let Some(panel) = self.ssh_ports.as_mut() {
            panel.set_rows(rows);
            panel.set_error(&result.err().map(|e| e.to_string()).unwrap_or_default());
        }
        self.refresh_ssh_toolbar();
        Ok(())
    }

    fn ssh_forward_authentication(
        &mut self,
        workspace: WorkspaceId,
        id: Uuid,
    ) -> anyhow::Result<()> {
        self.ssh_lifecycle_guard()?;
        let config = self.workspaces[self.workspace_index(workspace)?]
            .ssh
            .as_ref()
            .context("Target is not an SSH workspace")?;
        anyhow::ensure!(
            config.forwards.iter().any(|forward| forward.id == id),
            "Forward no longer exists"
        );
        self.ssh_forwards
            .get_mut(&id)
            .filter(|forward| forward.workspace == workspace)
            .context("Forward authentication is unavailable; connect the workspace first")?
            .show()
    }

    fn ssh_forward_start(
        &mut self,
        workspace: WorkspaceId,
        spec: SshForwardSpec,
    ) -> anyhow::Result<()> {
        let port = match spec.local_port {
            Some(port) => port,
            None => std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?
                .local_addr()?
                .port(),
        };
        // The reservation cannot be transferred to OpenSSH. ExitOnForwardFailure
        // and PID ownership checks report a raced bind as failure, never success.
        let runtime = ssh_forward::Forward::new(self, workspace, spec.clone(), port)?;
        self.ssh_forwards.insert(spec.id, runtime);
        Ok(())
    }

    fn ssh_forward_add(
        &mut self,
        workspace: WorkspaceId,
        spec: SshForwardSpec,
    ) -> anyhow::Result<()> {
        self.ssh_lifecycle_guard()?;
        spec.validate().map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            !self.ssh_disconnected.contains(&workspace),
            "Connect this SSH workspace before adding forwarding"
        );
        let index = self.workspace_index(workspace)?;
        let config = self.workspaces[index]
            .ssh
            .as_ref()
            .context("Target is not an SSH workspace")?;
        anyhow::ensure!(
            config.forwards.len() < 16,
            "An SSH workspace supports up to 16 forwards"
        );
        anyhow::ensure!(
            !self
                .workspaces
                .iter()
                .filter_map(|w| w.ssh.as_ref())
                .flat_map(|c| &c.forwards)
                .any(|f| f.id == spec.id),
            "Forward identity already exists"
        );
        self.ssh_forward_start(workspace, spec.clone())?;
        self.workspaces[index]
            .ssh
            .as_mut()
            .unwrap()
            .forwards
            .push(spec);
        Ok(())
    }

    fn ssh_forward_remove(&mut self, workspace: WorkspaceId, id: Uuid) -> anyhow::Result<()> {
        self.ssh_lifecycle_guard()?;
        let index = self.workspace_index(workspace)?;
        let config = self.workspaces[index]
            .ssh
            .as_mut()
            .context("Target is not an SSH workspace")?;
        anyhow::ensure!(
            config.forwards.iter().any(|f| f.id == id),
            "Forward not found"
        );
        config.forwards.retain(|f| f.id != id);
        let retired = self.ssh_forwards.remove(&id);
        let result = self.refresh_ssh_previews();
        drop(retired);
        result
    }

    pub(super) fn ssh_forwards_connect(&mut self, workspace: WorkspaceId) -> anyhow::Result<()> {
        let specs = self.workspaces[self.workspace_index(workspace)?]
            .ssh
            .as_ref()
            .context("Target is not an SSH workspace")?
            .forwards
            .clone();
        for spec in specs {
            if self
                .ssh_forwards
                .get(&spec.id)
                .is_some_and(|f| matches!(f.state(), "connecting" | "connected"))
            {
                continue;
            }
            self.ssh_forwards.remove(&spec.id);
            self.ssh_forward_start(workspace, spec)?;
        }
        Ok(())
    }

    pub(super) fn ssh_forwards_disconnect(&mut self, workspace: WorkspaceId) -> anyhow::Result<()> {
        let ids: Vec<_> = self
            .ssh_forwards
            .iter()
            .filter(|(_, f)| f.workspace == workspace)
            .map(|(id, _)| *id)
            .collect();
        let retired: Vec<_> = ids
            .into_iter()
            .filter_map(|id| self.ssh_forwards.remove(&id))
            .collect();
        let result = self.refresh_ssh_previews();
        drop(retired);
        result
    }

    pub(super) fn ssh_preview_target(
        &self,
        binding: &str,
        surface: SurfaceId,
    ) -> Option<(SurfaceId, String)> {
        let id = crate::browser::ssh_preview_id(binding)?;
        let (index, _, _) = self.locate(surface)?;
        let ws = &self.workspaces[index];
        if self.ssh_disconnected.contains(&ws.id) {
            return None;
        }
        let spec = ws.ssh.as_ref()?.forwards.iter().find(|s| s.id == id)?;
        let runtime = self
            .ssh_forwards
            .get(&id)
            .filter(|f| f.workspace == ws.id && f.state() == "connected")?;
        Some((
            runtime.surface,
            format!(
                "{}://127.0.0.1:{}",
                if spec.https { "https" } else { "http" },
                runtime.port
            ),
        ))
    }

    pub(super) fn ssh_ports_tick(&mut self) -> anyhow::Result<()> {
        let live: HashSet<_> = self.workspaces.iter().map(|w| w.id).collect();
        self.ssh_forwards.retain(|_, f| live.contains(&f.workspace));
        if self
            .ssh_ports
            .as_ref()
            .is_some_and(|p| !live.contains(&p.workspace))
        {
            self.ssh_ports.take();
        }
        let mut changed = false;
        for forward in self.ssh_forwards.values_mut() {
            changed |= forward.tick()?;
        }
        if changed {
            self.ssh_forward_refresh()?;
        }
        Ok(())
    }

    pub(super) fn ssh_forward_refresh(&mut self) -> anyhow::Result<()> {
        self.refresh_ssh_previews()?;
        if let Some(workspace) = self.ssh_ports.as_ref().map(|p| p.workspace) {
            let rows = self.ssh_port_rows(workspace);
            self.ssh_ports.as_mut().unwrap().set_rows(rows);
        }
        self.refresh_ssh_toolbar();
        Ok(())
    }
}
