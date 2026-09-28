// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

impl App {
    fn ssh_surface_ids(&self, workspace: WorkspaceId) -> anyhow::Result<Vec<SurfaceId>> {
        let workspace = self
            .workspaces
            .iter()
            .find(|candidate| candidate.id == workspace)
            .context("SSH workspace no longer exists")?;
        anyhow::ensure!(workspace.ssh.is_some(), "Target is not an SSH workspace");
        Ok(workspace
            .leaves()
            .into_iter()
            .flat_map(|(_, _, tabs)| tabs)
            .filter(|tab| matches!(tab.kind, SurfaceKind::SshTerminal { .. }))
            .map(|tab| tab.id)
            .collect())
    }

    pub(super) fn ssh_lifecycle_guard(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.pending_save.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && !self.closing
                && self.editor_barrier.is_none()
                && self
                    .ssh_auth_window
                    .as_ref()
                    .is_none_or(|(_, window)| unsafe { IsWindowEnabled(window.window) } != 0),
            "SSH connection cannot change while the window is saving or closing"
        );
        Ok(())
    }

    pub(super) fn ssh_action_enabled(&self, action: &Action) -> bool {
        let workspace = match *action {
            Action::SshConnect(id)
            | Action::SshDisconnect(id)
            | Action::SshAuthentication(id)
            | Action::SshPorts(id) => id,
            Action::SshStatus(_) => return false,
            _ => return true,
        };
        if self.current_workspace().is_none_or(|ws| ws.id != workspace) {
            return false;
        }
        let Ok(ids) = self.ssh_surface_ids(workspace) else {
            return false;
        };
        if matches!(action, Action::SshPorts(_)) {
            return true;
        }
        let forwards = self
            .workspaces
            .iter()
            .find(|w| w.id == workspace)
            .and_then(|w| w.ssh.as_ref())
            .is_some_and(|c| !c.forwards.is_empty());
        if !ids.iter().any(|id| self.surfaces.contains_key(id)) && !forwards {
            return false;
        }
        let status = self.ssh_status(workspace);
        match action {
            Action::SshConnect(_) => {
                !matches!(status["state"].as_str(), Some("connected" | "connecting"))
                    && !ids.iter().any(|id| {
                        self.surfaces.get(id).is_some_and(|surface| {
                            surface.session.is_some() && surface.exit_code.is_some()
                        })
                    })
            }
            Action::SshDisconnect(_) => {
                self.ssh_forwards.values().any(|f| {
                    f.workspace == workspace && matches!(f.state(), "connecting" | "connected")
                }) || status["tabs"].as_object().is_some_and(|tabs| {
                    tabs.values().any(|tab| {
                        matches!(tab["state"].as_str(), Some("connecting" | "connected"))
                    })
                })
            }
            Action::SshAuthentication(_) => true,
            _ => false,
        }
    }

    pub(super) fn ssh_status(&self, workspace: WorkspaceId) -> Value {
        let ids = match self.ssh_surface_ids(workspace) {
            Ok(ids) => ids,
            Err(error) => {
                return json!({"workspace":workspace,"state":"failed","error":error.to_string(),"tabs":{}});
            }
        };
        let disconnected = self.ssh_disconnected.contains(&workspace);
        let mut tabs = serde_json::Map::new();
        let mut error = None;
        let mut connecting = false;
        let mut connected = false;
        for id in ids {
            let surface = self.surfaces.get(&id);
            let tab_error = surface.and_then(|surface| {
                surface.startup_error.clone().or_else(|| {
                    surface
                        .exit_code
                        .filter(|code| *code != 0)
                        .map(|code| format!("SSH exited ({code}); see authentication output"))
                })
            });
            let state = if disconnected {
                "disconnected"
            } else if tab_error.is_some() {
                "failed"
            } else if surface.is_some_and(|surface| {
                surface.session.is_some() && surface.exit_code.is_none() && surface.ssh_connected
            }) {
                connected = true;
                "connected"
            } else if surface.is_none_or(|surface| {
                (!surface.ready || surface.restoring)
                    || (surface.session.is_some() && surface.exit_code.is_none())
            }) {
                connecting = true;
                "connecting"
            } else {
                "disconnected"
            };
            if !disconnected && error.is_none() {
                error.clone_from(&tab_error);
            }
            tabs.insert(
                id.to_string(),
                json!({
                    "state":state,
                    "pid":surface.and_then(|surface| surface.process_pid),
                    "error":if disconnected { None } else { tab_error },
                }),
            );
        }
        let forwards: Vec<_> = self.workspaces.iter().find(|w| w.id == workspace).and_then(|w| w.ssh.as_ref()).into_iter().flat_map(|c| &c.forwards).map(|spec| {
            let runtime = self.ssh_forwards.get(&spec.id);
            if !disconnected {
                if let Some(forward) = runtime {
                    connecting |= forward.state() == "connecting";
                    connected |= forward.state() == "connected";
                    if error.is_none() { error.clone_from(&forward.error); }
                }
            }
            json!({"id":spec.id,"remote_port":spec.remote_port,"local_port":runtime.filter(|f| f.state()=="connected").map(|f| f.port),"active":runtime.is_some_and(|f| f.state()=="connected"),"state":runtime.map_or("disconnected", |f| f.state())})
        }).collect();
        let state = if disconnected {
            "disconnected"
        } else if error.is_some() {
            "failed"
        } else if connecting {
            "connecting"
        } else if connected {
            "connected"
        } else {
            "disconnected"
        };
        json!({"workspace":workspace,"state":state,"error":error,"tabs":tabs,"forwards":forwards})
    }

    pub(super) fn ssh_connect(&mut self, workspace: WorkspaceId) -> anyhow::Result<()> {
        self.ssh_lifecycle_guard()?;
        let ids = self.ssh_surface_ids(workspace)?;
        anyhow::ensure!(
            !ids.is_empty()
                || self.workspaces[self.workspace_index(workspace)?]
                    .ssh
                    .as_ref()
                    .is_some_and(|c| !c.forwards.is_empty()),
            "SSH workspace has no terminal tabs or forwards"
        );
        anyhow::ensure!(
            !ids.iter().any(|id| self
                .surfaces
                .get(id)
                .is_some_and(|s| s.session.is_some() && s.exit_code.is_some())),
            "SSH is still draining its final output; retry Connect shortly"
        );
        let local_cwd = &self
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .unwrap()
            .cwd;
        anyhow::ensure!(
            local_cwd.is_dir(),
            "Local SSH process directory no longer exists"
        );
        // Validate every connection before changing the explicit disconnect flag.
        let shells = ids
            .iter()
            .map(|id| {
                let shell = self
                    .ssh_shell(*id)?
                    .context("SSH terminal configuration missing")?;
                super::super::shell::resolve(&shell)?;
                Ok((*id, shell))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        self.ssh_disconnected.remove(&workspace);
        self.ssh_forwards_connect(workspace)?;
        for (id, shell) in shells {
            self.shells.insert(id, shell);
        }
        for id in ids {
            // A session awaiting its final output/exit still owns this surface.
            // Ready/Restored callbacks start views which are not initialized yet.
            if self.surfaces.get(&id).is_some_and(|surface| {
                surface.ready && !surface.restoring && surface.session.is_none()
            }) {
                self.start_session(id)?;
            }
        }
        Ok(())
    }

    pub(super) fn ssh_disconnect(&mut self, workspace: WorkspaceId) -> anyhow::Result<()> {
        self.ssh_lifecycle_guard()?;
        let ids = self.ssh_surface_ids(workspace)?;
        self.ssh_disconnected.insert(workspace);
        let auth_failure = if self
            .ssh_auth_window
            .as_ref()
            .is_some_and(|(id, _)| ids.contains(id))
        {
            self.close_ssh_auth().err()
        } else {
            None
        };
        let forward_failure = self.ssh_forwards_disconnect(workspace).err();
        // Retire every process before any WebView notification can fail. Sequence
        // counters stay intact so already submitted parser ACKs remain valid.
        for id in &ids {
            if let Some(surface) = self.surfaces.get_mut(id) {
                surface.session_generation = Uuid::new_v4();
                surface.session.take();
                surface.ssh_connected = false;
                surface.startup_error = None;
                surface.exit_code = Some(0);
                surface.output_ended = true;
            }
        }
        let mut failure = auth_failure.or(forward_failure);
        for id in ids {
            self.cancel_surface_search(id);
            self.cancel_terminal_requests(id);
            if let Some(surface) = self.surfaces.get(&id) {
                if let Err(error) = surface.send(&HostMessage::SshStatus {
                    state: "disconnected".into(),
                    error: None,
                }) {
                    failure.get_or_insert(error);
                }
            }
        }
        self.refresh_ssh_toolbar();
        failure.map_or(Ok(()), Err)
    }

    pub(super) fn ssh_authentication(&mut self, workspace: WorkspaceId) -> anyhow::Result<()> {
        self.ssh_lifecycle_guard()?;
        if let Some(forward) = self
            .ssh_forwards
            .values_mut()
            .find(|f| f.workspace == workspace && f.state() == "connecting")
        {
            return forward.show();
        }
        let ids: Vec<_> = self
            .ssh_surface_ids(workspace)?
            .into_iter()
            .filter(|id| self.surfaces.contains_key(id))
            .collect();
        let authenticating = ids.iter().any(|id| {
            self.surfaces
                .get(id)
                .is_some_and(|s| s.session.is_some() && s.exit_code.is_none() && !s.ssh_connected)
        });
        if !authenticating {
            if let Some(forward) = self
                .ssh_forwards
                .values_mut()
                .find(|f| f.workspace == workspace && f.state() == "failed")
            {
                return forward.show();
            }
        }
        if ids.is_empty() {
            if let Some(forward) = self
                .ssh_forwards
                .values_mut()
                .find(|f| f.workspace == workspace)
            {
                return forward.show();
            }
        }
        let current = self.current_surface();
        let id = ids
            .iter()
            .copied()
            .find(|id| {
                self.surfaces.get(id).is_some_and(|surface| {
                    surface.session.is_some()
                        && surface.exit_code.is_none()
                        && !surface.ssh_connected
                })
            })
            .or_else(|| {
                ids.iter().copied().find(|id| {
                    self.surfaces.get(id).is_some_and(|surface| {
                        surface.startup_error.is_some()
                            || surface.exit_code.is_some_and(|code| code != 0)
                    })
                })
            })
            .or_else(|| current.filter(|id| ids.contains(id)))
            .or_else(|| ids.first().copied())
            .context("SSH workspace has no authentication terminal")?;
        self.show_ssh_auth(id)
    }

    fn show_ssh_auth(&mut self, id: SurfaceId) -> anyhow::Result<()> {
        if !self
            .ssh_auth_window
            .as_ref()
            .is_some_and(|(current, _)| *current == id)
        {
            self.close_ssh_auth()?;
            let (index, _, _) = self.locate(id).context("SSH terminal no longer exists")?;
            let target = &self.workspaces[index]
                .ssh
                .as_ref()
                .context("Workspace is not SSH")?
                .target;
            let window = ssh_auth::Window::new(
                self.window,
                &format!("SSH Authentication — {}", target.destination()),
                id,
                self.background_test,
            )?;
            window.theme(self.settings.terminal.theme);
            self.surface_holder(id)?.reparent(window.window)?;
            self.ssh_auth_window = Some((id, window));
        }
        self.refresh_terminal_menu(id)?;
        self.ssh_auth_layout()?;
        self.ssh_auth_window.as_mut().unwrap().1.show();
        if !self.background_test {
            let terminal = &self.surfaces[&id];
            terminal.view.focus()?;
            if terminal.ready {
                terminal.send(&HostMessage::Focus)?;
            }
        }
        Ok(())
    }

    pub(super) fn ssh_auth_layout(&mut self) -> anyhow::Result<()> {
        let Some((id, window)) = &self.ssh_auth_window else {
            return Ok(());
        };
        let terminal = self
            .surfaces
            .get_mut(id)
            .context("SSH authentication terminal disappeared")?;
        let area = window.area()?;
        terminal.holder.layout(Some(area), self.background_test)?;
        terminal.view.set_bounds(bounds(area))?;
        unsafe {
            terminal
                .view
                .controller()
                .NotifyParentWindowPositionChanged()?;
        }
        if !terminal.visible {
            terminal.view.set_visible(true)?;
            terminal.visible = true;
            if terminal.ready || terminal.restoring {
                terminal.send(&HostMessage::Visibility { visible: true })?;
            }
        }
        Ok(())
    }

    pub(super) fn close_ssh_auth(&mut self) -> anyhow::Result<()> {
        let Some((id, window)) = &self.ssh_auth_window else {
            return Ok(());
        };
        let (id, owner) = (*id, window.window);
        // Keep the owner alive if reparenting fails; never destroy a live WebView's parent.
        if let Some(terminal) = self.surfaces.get(&id) {
            terminal.holder.reparent(self.window)?;
        }
        self.tab_menu_surface_closing(id);
        self.metadata_owner_closing(owner);
        self.ssh_auth_window.take();
        self.layout()
    }

    pub(super) fn ssh_auth_status(&self) -> Value {
        let Some((id, window)) = &self.ssh_auth_window else {
            return Value::Null;
        };
        let mut status = window.status();
        status["surface"] = json!(id);
        status["workspace"] = json!(self
            .locate(*id)
            .map(|(index, _, _)| self.workspaces[index].id));
        status["session"] = json!(self
            .surfaces
            .get(id)
            .map(|terminal| terminal.session_generation));
        status
    }

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
            false,
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
        crate::ssh::terminal_shell(
            config,
            cwd.as_deref(),
            tmux_session.as_deref(),
            self.ssh_attempted.contains(&id),
        )
        .map(Some)
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
