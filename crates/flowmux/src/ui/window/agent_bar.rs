// SPDX-License-Identifier: GPL-3.0-or-later
//! Agent bar + notification attention tracking and rendering.

use super::*;
use vte::prelude::TerminalExt;

impl WindowController {
    pub(super) async fn agent_display_model(
        &self,
        mut model: flowmux_core::AgentBarModel,
    ) -> flowmux_core::AgentBarModel {
        let mut items = Vec::new();
        for item in model.items {
            let binding = self
                .store
                .surface_agent_binding(item.pane, item.surface)
                .await;
            if binding.is_none() {
                items.push(item);
            }
        }
        model.items = items;
        model.visible = !model.items.is_empty();
        model
    }

    fn hide_agent_bar(&self) {
        self.agent_bar.bar.render(
            &flowmux_core::AgentBarModel {
                visible: false,
                items: Vec::new(),
            },
            &HashSet::new(),
            None,
        );
    }

    fn render_agent_bar(&self, model: &flowmux_core::AgentBarModel) {
        let live_surfaces: HashSet<SurfaceId> =
            model.items.iter().map(|item| item.surface).collect();
        let focused_surface = self
            .focused_pane
            .get()
            .and_then(|pane| self.pane_registry.borrow().active_surface(pane))
            .filter(|surface| live_surfaces.contains(surface));
        let attentions = {
            let mut attentions = self.agent_bar.attentions.borrow_mut();
            attentions.retain(|surface| live_surfaces.contains(surface));
            attentions.clone()
        };
        self.agent_bar
            .bar
            .render(model, &attentions, focused_surface);
    }

    fn render_activity_panel(&self, model: &flowmux_core::AgentBarModel) {
        let mut current = current_activity_entries(model);
        self.activities
            .sort_entries(&mut current, self.options.borrow().agent_sort_mode);
        let focused_surface = self
            .focused_pane
            .get()
            .and_then(|pane| self.pane_registry.borrow().active_surface(pane));
        self.sidebar
            .refresh_activity_panel(&current, focused_surface, |id| {
                self.sidebar.workspace_color(id)
            });
        let mut rendered_windows = HashSet::new();
        for host in self.detached_workspaces.borrow().values() {
            if rendered_windows.insert(host.window.clone()) {
                host.sidebar
                    .refresh_activity_panel(&current, focused_surface, |id| {
                        self.sidebar.workspace_color(id)
                    });
            }
        }
    }

    pub(super) async fn verified_agent_location(
        &self,
        agent: &flowmux_ipc::agent_host::AgentInstance,
    ) -> Option<flowmux_ipc::agent_host::AgentViewLocation> {
        let attachment = agent.attachment.as_ref()?;
        if !attachment.ready {
            return None;
        }
        let (workspace, pane, surface) = self
            .store
            .agent_view(flowmux_core::AgentViewBinding {
                instance: agent.instance,
                epoch: agent.epoch,
            })
            .await?;
        let registry = self.pane_registry.borrow();
        let terminal = registry.terminals.get(&surface)?;
        let terminal_pid = terminal.pid.get().and_then(|pid| u32::try_from(pid).ok());
        let relay_pid = attachment.relay_pid;
        // The terminal owns either the relay directly or its pty-tee parent.
        if terminal_pid.is_none()
            || attachment.relay_start.is_none()
            || relay_pid.and_then(flowmux_procmon::process_start_time) != attachment.relay_start
            || (terminal_pid != relay_pid
                && relay_pid.and_then(flowmux_procmon::parent_pid) != terminal_pid)
        {
            return None;
        }
        Some(flowmux_ipc::agent_host::AgentViewLocation {
            window: self.flow.owner,
            workspace,
            pane,
            surface,
            visible: false,
        })
    }

    pub(super) async fn agent_context_is_current(
        &self,
        context: &flowmux_ipc::agent_host::AgentViewReport,
    ) -> bool {
        let agent = {
            let registry = self.bridge.agent_registry.borrow();
            if !registry.available {
                return false;
            }
            registry
                .agents
                .iter()
                .find(|agent| {
                    agent.instance == context.key.instance
                        && agent.epoch == context.key.epoch
                        && agent
                            .attachment
                            .as_ref()
                            .is_some_and(|attachment| attachment.key == context.key)
                })
                .cloned()
        };
        let Some(agent) = agent else {
            return false;
        };
        self.verified_agent_location(&agent)
            .await
            .is_some_and(|location| location.same_target(&context.location))
    }

    pub(super) async fn publish_agent_views(&self) {
        use flowmux_ipc::agent_host::AgentViewReport;

        let agents = self.bridge.agent_registry.borrow().agents.clone();
        let mut reports = Vec::new();
        for agent in agents {
            let Some(mut location) = self.verified_agent_location(&agent).await else {
                continue;
            };
            let window = self.workspace_window(location.workspace);
            let minimized = window
                .surface()
                .and_then(|surface| surface.downcast::<gtk::gdk::Toplevel>().ok())
                .is_some_and(|toplevel| {
                    toplevel
                        .state()
                        .contains(gtk::gdk::ToplevelState::MINIMIZED)
                });
            let registry = self.pane_registry.borrow();
            location.visible = window.is_mapped()
                && !minimized
                && self.visible_workspace_in_window(&window) == Some(location.workspace)
                && registry.active_surface(location.pane) == Some(location.surface)
                && registry
                    .terminals
                    .get(&location.surface)
                    .is_some_and(|terminal| terminal.widget.is_mapped());
            reports.push(AgentViewReport {
                key: agent.attachment.unwrap().key,
                location,
            });
        }
        reports.sort_by_key(|report| report.key.instance.0);
        if !self.window.is_realized() {
            reports.clear();
        }
        // Each GTK observation is a heartbeat; the manager must not renew cached locations.
        self.bridge.agent_view_reports.send_replace(reports);
    }

    pub(super) async fn refresh_agent_displays(&self) {
        self.refresh_agent_connections().await;
        crate::ui::set_running_agents(self.running_agents().len());
        crate::ui::set_hosted_terminals(
            self.pane_registry
                .borrow()
                .terminals
                .iter()
                .filter(|(_, terminal)| {
                    terminal
                        .agent_connection
                        .borrow()
                        .as_ref()
                        .is_some_and(|connection| connection.shell)
                })
                .map(|(surface, _)| *surface)
                .collect(),
        );
        self.publish_agent_views().await;
        self.refresh_managed_workspace_rows();
        if self
            .reviews
            .borrow()
            .values()
            .any(|r| r.root_widget.is_mapped())
        {
            self.refresh_review_targets().await;
        }
        self.refresh_session_panel(false).await;
        self.refresh_session_flow(false).await;
        let model = self.store.agent_bar_model().await;
        if let Some(office) = self.agent_office.borrow().as_ref() {
            office
                .view
                .render(&self.sidebar.workspace_titles().borrow(), &model);
        }
        let model = self.agent_display_model(model).await;
        if self.options.borrow().agent_bar_mode {
            self.render_agent_bar(&model);
        } else {
            self.hide_agent_bar();
        }
        self.agent_bar
            .bar
            .render_managed(&model, self.options.borrow().agent_bar_mode, |id| {
                self.sidebar.workspace_color(id)
            });
        self.render_activity_panel(&model);
    }

    pub(super) async fn refresh_agent_connections(&self) {
        let snapshot = self.bridge.agent_registry.borrow().clone();
        let terminals: Vec<_> = self
            .pane_registry
            .borrow()
            .terminals
            .iter()
            .map(|(surface, terminal)| (*surface, terminal.clone()))
            .collect();
        let mut finished = Vec::new();
        for (surface, terminal) in terminals {
            let binding = terminal
                .agent_connection
                .borrow()
                .as_ref()
                .map(|connection| connection.binding);
            let Some(binding) = binding else { continue };
            let agent = snapshot
                .agents
                .iter()
                .find(|agent| agent.instance == binding.instance && agent.epoch == binding.epoch);
            let ready = if snapshot.available && terminal.pid.get().is_some() {
                if let Some(agent) = agent {
                    self.verified_agent_location(agent)
                        .await
                        .is_some_and(|location| location.surface == surface)
                } else {
                    false
                }
            } else {
                false
            };
            let connection = terminal.agent_connection.borrow();
            let Some(connection) = connection.as_ref() else {
                continue;
            };
            // Until the Host lists the shell, the pid its relay reported stands.
            if let Some(pid) = agent
                .and_then(|agent| agent.pid)
                .filter(|_| connection.shell)
            {
                connection.host_pid.set(Some(pid));
            }
            if ready {
                connection.last_ready.set(
                    agent
                        .and_then(|agent| agent.attachment.as_ref())
                        .map(|a| a.key),
                );
            } else if snapshot.available
                && agent.is_some_and(|agent| {
                    let terminal_pid = terminal.pid.get().and_then(|pid| u32::try_from(pid).ok());
                    let elsewhere =
                        crate::ui::agent_connection::shown_elsewhere(terminal_pid, agent);
                    agent
                        .view_owner
                        .is_some_and(|owner| connection.superseded_by(surface, owner, elsewhere))
                })
            {
                self.bridge.send_from_ui(GtkCommand::CloseMovedAgentView {
                    pane: terminal.id(),
                    surface,
                    owner: agent.and_then(|agent| agent.view_owner).unwrap(),
                    terminal_generation: terminal.program_status_generation,
                });
            }
            let ended = agent.is_some_and(|agent| {
                matches!(
                    agent.state,
                    flowmux_ipc::agent_host::ProcessState::Exited
                        | flowmux_ipc::agent_host::ProcessState::Failed
                )
            });
            // Keep the ordinary resume binding in step with the execution: a start
            // without any Host restores this tab as an ordinary tab and resumes from it.
            let session = agent
                .filter(|_| ready)
                .and_then(|agent| Some((agent, agent.presence.as_ref()?.session_id.clone()?)));
            if ended {
                connection.recorded_session.take();
                // Also for a tab restored after its agent ended: what was saved for it
                // outlives this window's memory of saving it.
                if connection.session_cleared.get().is_none() {
                    let cleared = connection.session_cleared.clone();
                    cleared.set(Some(false));
                    match flowmux_state::default_agent_session_store() {
                        Some(store) => {
                            glib::spawn_future_local(async move {
                                let _ =
                                    gtk::gio::spawn_blocking(move || store.forget_surface(surface))
                                        .await;
                                cleared.set(Some(true));
                            });
                        }
                        None => cleared.set(Some(true)),
                    }
                }
            } else if let Some((agent, session)) = session {
                if connection.recorded_session.borrow().as_deref() != Some(session.as_str()) {
                    *connection.recorded_session.borrow_mut() = Some(session.clone());
                    if let Some(store) = flowmux_state::default_agent_session_store() {
                        let (provider, pid) = (agent.provider.clone(), agent.pid);
                        gtk::gio::spawn_blocking(move || {
                            if let Err(error) =
                                store.record_from_process(&provider, surface, &session, pid)
                            {
                                tracing::debug!(%surface, %error, "managed session not saved for resume");
                            }
                        });
                    }
                }
            }
            // Once the execution has ended and its relay is gone, the tab goes on as a shell:
            // after its saved session is removed, or the new shell would resume it.
            if terminal.pid.get().is_none()
                && ended
                && connection.session_cleared.get() == Some(true)
            {
                finished.push((terminal.id(), surface));
            }
            let other_host =
                !snapshot.other_hosts.is_empty() && snapshot.epoch != Some(binding.epoch);
            let error = if other_host && terminal.pid.get().is_none() {
                Some("This tab may still be running in another Agent Host. Reopen it with the Flowmux build that started it.")
            } else if terminal.pid.get().is_none() {
                Some("Agent connection failed. Reopen from Agents to retry.")
            } else if agent.is_some_and(|agent| {
                matches!(
                    agent.state,
                    flowmux_ipc::agent_host::ProcessState::Exited
                        | flowmux_ipc::agent_host::ProcessState::Failed
                )
            }) {
                Some("Agent execution ended.")
            } else if !snapshot.available && connection.was_ready.get() {
                Some("Agent Host unavailable. Waiting for the existing Host.")
            } else if !ready
                && connection.started.elapsed() >= flowmux_ipc::agent_host::ATTACH_TIMEOUT
            {
                Some("Agent connection timed out. Close this tab and reopen from Agents to retry.")
            } else {
                None
            };
            if !connection.shell {
                terminal.widget.set_input_enabled(ready);
                connection.render(ready, error);
            }
        }
        for (pane, surface) in finished {
            // Like an agent typed into a shell: when it exits, the terminal stays.
            let Some(workspace) = self.store.clear_surface_agent_binding(pane, surface).await
            else {
                continue;
            };
            // The agent's last screen often says why it ended; carry it into the shell's view.
            // Rebuilding the tab must not move it, select it or take the keyboard.
            let (last_screen, position) = {
                let registry = self.pane_registry.borrow();
                (
                    registry
                        .terminals
                        .get(&surface)
                        .and_then(|terminal| terminal.scrollback_snapshot()),
                    registry
                        .surface_tabs
                        .get(&pane)
                        .and_then(|tabs| tabs.iter().position(|(id, _)| *id == surface)),
                )
            };
            let focused = self.focused_pane.get();
            self.pane_registry
                .borrow_mut()
                .detach_surface_widget(pane, surface);
            if let Err(error) = self
                .attach_or_rerender_surface(workspace, pane, surface, None)
                .await
            {
                tracing::warn!(%surface, %error, "ended Agent tab could not continue as a terminal");
                continue;
            }
            if let Some(last_screen) = last_screen {
                if let Some(terminal) = self.pane_registry.borrow().terminals.get(&surface) {
                    terminal.restore_scrollback(&last_screen);
                }
            }
            // These refresh the Agents displays themselves, so they run after this pass.
            let controller = self.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Some(position) = position {
                    let _ = controller
                        .move_surface(pane, surface, None, pane, position)
                        .await;
                }
                // Attaching showed the rebuilt tab. The store still names the pane's active
                // tab, including one opened or selected while this tab was being rebuilt.
                controller
                    .sync_pane_active_from_store(workspace, pane)
                    .await;
                // Attaching queued a focus grab for the rebuilt tab; answer it.
                if let Some(focused) = focused {
                    controller.focus_pane(focused);
                }
            });
        }
    }

    pub(super) fn schedule_agent_display_refresh(&self, workspace: WorkspaceId) {
        self.agent_bar
            .pending_workspaces
            .borrow_mut()
            .insert(workspace);
        if self.agent_bar.display_refresh_task.borrow().is_some() {
            return;
        }
        let controller = self.clone();
        let task = glib::MainContext::default().spawn_local_with_priority(
            glib::Priority::DEFAULT_IDLE,
            async move {
                loop {
                    // Coalesce across terminals and let input/layout run before overview work.
                    glib::timeout_future_with_priority(
                        glib::Priority::DEFAULT_IDLE,
                        Duration::from_millis(100),
                    )
                    .await;
                    let workspaces =
                        std::mem::take(&mut *controller.agent_bar.pending_workspaces.borrow_mut());
                    for workspace in workspaces {
                        controller.update_workspace_agent_status(workspace).await;
                        // A ready store lock does not yield between workspace rows.
                        glib::timeout_future_with_priority(
                            glib::Priority::DEFAULT_IDLE,
                            Duration::ZERO,
                        )
                        .await;
                    }
                    controller.refresh_agent_displays().await;
                    if controller.agent_bar.pending_workspaces.borrow().is_empty() {
                        break;
                    }
                }
                controller
                    .agent_bar
                    .display_refresh_task
                    .borrow_mut()
                    .take();
            },
        );
        *self.agent_bar.display_refresh_task.borrow_mut() = Some(task);
    }

    pub(super) async fn refresh_activity_panel(&self) {
        let model = self
            .agent_display_model(self.store.agent_bar_model().await)
            .await;
        self.render_activity_panel(&model);
    }

    pub(super) async fn open_activity_target(
        &self,
        workspace: WorkspaceId,
        pane: PaneId,
        surface: SurfaceId,
    ) {
        // Activity entries retain workspace navigation after their tab closes.
        let target = {
            let registry = self.pane_registry.borrow();
            (registry.workspace_of_pane(pane) == Some(workspace)
                && registry.pane_for_surface(surface) == Some(pane))
            .then_some((pane, Some(surface)))
        };
        self.activate_workspace_at(workspace, target).await;
        self.workspace_window(workspace).present();
    }

    pub(super) async fn sync_workspace_agent_status(&self, workspace: WorkspaceId) {
        self.update_workspace_agent_status(workspace).await;
        self.refresh_agent_displays().await;
    }

    /// A Host agent's status lives in the Host, not in the stored tab. Put it on
    /// this copy of the workspace so the workspace row shows it like any agent's.
    pub(super) fn overlay_managed_presence(&self, ws: &mut flowmux_core::Workspace) {
        for (view, presence) in self.managed_presences() {
            if view.workspace == ws.id {
                for root in &mut ws.surfaces {
                    if root
                        .root_pane
                        .set_surface_agent(view.surface, Some(presence.clone()))
                    {
                        break;
                    }
                }
            }
        }
    }

    /// Status of every running Host agent whose view is in this window.
    fn managed_presences(
        &self,
    ) -> Vec<(
        flowmux_ipc::agent_host::AgentViewLocation,
        flowmux_core::AgentPresence,
    )> {
        let snapshot = self.bridge.agent_registry.borrow();
        if !snapshot.available {
            return Vec::new();
        }
        snapshot
            .agents
            .iter()
            .filter(|agent| {
                // A provider that already exited has no status of its own left.
                agent.state == flowmux_ipc::agent_host::ProcessState::Running
                    && agent.exit_status.is_none()
            })
            .filter_map(|agent| Some((agent.view.clone()?, agent.presence.clone()?)))
            .filter(|(view, _)| view.window == self.flow.owner)
            .collect()
    }

    /// Workspaces that hold a view of a running Host agent in this window.
    pub(super) fn managed_presence_workspaces(&self) -> HashSet<WorkspaceId> {
        let snapshot = self.bridge.agent_registry.borrow();
        snapshot
            .agents
            .iter()
            .filter(|agent| agent.state == flowmux_ipc::agent_host::ProcessState::Running)
            .filter(|agent| !agent.shell || agent.presence.is_some())
            .filter_map(|agent| agent.view.as_ref())
            .filter(|view| view.window == self.flow.owner)
            .map(|view| view.workspace)
            .collect()
    }

    /// Refresh the workspace rows whose Host agents changed status.
    fn refresh_managed_workspace_rows(&self) {
        let mut current: HashMap<WorkspaceId, String> = HashMap::new();
        for (view, presence) in self.managed_presences() {
            use std::fmt::Write;
            let _ = write!(
                current.entry(view.workspace).or_default(),
                "{}:{presence:?};",
                view.surface
            );
        }
        let previous = self.agent_bar.managed_status.replace(current.clone());
        for workspace in previous.keys().chain(current.keys()) {
            if previous.get(workspace) != current.get(workspace) {
                self.schedule_agent_display_refresh(*workspace);
            }
        }
    }

    async fn update_workspace_agent_status(&self, workspace: WorkspaceId) {
        let stored = self.store.get_workspace_without_scrollback(workspace).await;
        let attention = stored.clone().and_then(|mut shown| {
            self.overlay_managed_presence(&mut shown);
            shown.agent_attention_rollup()
        });
        self.sidebar.set_agent_status(workspace, attention);
        // Teardown can restore titles without a cwd change or a new OSC title.
        if let Some(ws) = stored {
            let registry = self.pane_registry.borrow();
            for surface in &ws.surfaces {
                surface.root_pane.for_each_leaf(|pane| {
                    if let Some(PaneContent::Tabs { surfaces, .. }) =
                        surface.root_pane.find_leaf_content(pane)
                    {
                        for tab in surfaces {
                            registry.set_surface_title(tab.id, &tab.title);
                            if let flowmux_core::SurfaceKind::Terminal { cwd, .. } = &tab.kind {
                                if tab.agent.is_none()
                                    && !tab.title_locked
                                    && tab.title
                                        == flowmux_core::terminal_tab_title_for_cwd(cwd.as_deref())
                                {
                                    if let Some(terminal) = registry.terminals.get(&tab.id) {
                                        // Reset VTE and its title coalescer too: otherwise a
                                        // restarted agent's identical OSC title is deduplicated.
                                        let title: String =
                                            tab.title.chars().filter(|c| !c.is_control()).collect();
                                        if terminal.widget.window_title().as_deref()
                                            != Some(title.as_str())
                                        {
                                            terminal
                                                .widget
                                                .feed(format!("\x1b]2;{title}\x07").as_bytes());
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
            }
        }
        self.refresh_window_title().await;
        self.update_workspace_label(workspace).await;
    }
    pub(super) async fn sync_workspace_agent_status_from_store(&self, workspace: WorkspaceId) {
        self.sync_workspace_agent_status(workspace).await;
    }
    pub(super) fn mark_agent_bar_attention(&self, surface: SurfaceId) {
        if self.agent_bar.attentions.borrow_mut().insert(surface) {
            self.agent_bar.bar.mark_attention(surface);
        }
    }
    pub(super) fn clear_agent_bar_attention(&self, surface: SurfaceId) {
        if self.agent_bar.attentions.borrow_mut().remove(&surface) {
            self.agent_bar.bar.clear_attention(surface);
        }
    }
    pub(super) fn acknowledge_source_notifications(
        &self,
        workspace: Option<WorkspaceId>,
        pane: Option<PaneId>,
        surface: Option<SurfaceId>,
    ) {
        let unread_before = self.notifications.unread_count();
        let to_close = self
            .notifications
            .mark_source_read(workspace, pane, surface);
        let changed = self.notifications.unread_count() != unread_before;
        if let Some(surface) = surface {
            self.clear_agent_bar_attention(surface);
        }
        if let Some(workspace) = workspace {
            if !self.notifications.has_unread_workspace(workspace) {
                self.sidebar.clear_attention(workspace);
            }
        }
        if !to_close.is_empty() {
            self.close_desktop_notifications(to_close);
        }
        if changed {
            self.refresh_launcher_badge();
        }
    }
    pub(super) fn clear_notification_attention_for_entry(&self, entry: &NotificationEntry) {
        if let Some(surface) = entry.surface {
            if !self.notifications.has_unread_surface(surface) {
                self.clear_agent_bar_attention(surface);
            }
        }
        if let Some(workspace) = entry.workspace {
            if !self.notifications.has_unread_workspace(workspace) {
                self.sidebar.clear_attention(workspace);
            }
        }
    }
    pub(super) fn clear_all_notification_attention_for_entries(
        &self,
        entries: &[NotificationEntry],
    ) {
        for entry in entries {
            if let Some(surface) = entry.surface {
                self.clear_agent_bar_attention(surface);
            }
            if let Some(workspace) = entry.workspace {
                self.sidebar.clear_attention(workspace);
            }
        }
    }
    pub(super) async fn refresh_agent_screen_status(
        &self,
        surface: SurfaceId,
        title: Option<String>,
    ) -> Option<WorkspaceId> {
        let (screen, title, is_ssh) = {
            let registry = self.pane_registry.borrow();
            let terminal = registry.terminals.get(&surface);
            let ssh = registry
                .surface_workspace
                .get(&surface)
                .and_then(|workspace| registry.ssh.get(workspace));
            // ponytail: remote identity follows screen/title heuristics; use
            // remote lifecycle hooks if exact identity and teardown are needed.
            if let Some(ssh) = ssh {
                if ssh.borrow().agent_signals_active(surface) {
                    (
                        terminal.and_then(|terminal| terminal.agent_status_text()),
                        terminal.and_then(|terminal| {
                            terminal.widget.window_title().map(|s| s.to_string())
                        }),
                        true,
                    )
                } else {
                    // Disconnect/exit can leave an agent frame on screen.
                    (None, None, true)
                }
            } else {
                (
                    terminal.and_then(|terminal| terminal.agent_status_text()),
                    title.or_else(|| registry.surface_title_text(surface)),
                    false,
                )
            }
        };
        if screen.is_none() && title.is_none() && !is_ssh {
            return None;
        }
        let settled = if !is_ssh {
            self.store
                .settle_provider_turn_from_screen(
                    surface,
                    screen.as_deref().unwrap_or_default(),
                    title.as_deref(),
                    self.is_agent_surface_visible(surface),
                )
                .await
        } else {
            Default::default()
        };
        if settled.workspace.is_some() {
            if let Some(located) = self.store.located_agent_presence(surface).await {
                let presence = &located.presence;
                let failed = presence.turn_outcome == Some(flowmux_core::AgentTurnOutcome::Failed);
                let deliver = if let (Some(id), Some(session)) =
                    (settled.notification_id, presence.session_id.as_deref())
                {
                    self.store
                        .claim_agent_notification(surface, &presence.name, session, id)
                        .await
                } else {
                    failed
                };
                if deliver {
                    let (ack, _) = tokio::sync::oneshot::channel();
                    Box::pin(
                        self.dispatch_notification_command(GtkCommand::AddNotification {
                            id: settled.notification_id,
                            pane: Some(located.pane),
                            surface: Some(surface),
                            workspace: Some(located.workspace),
                            title: format!(
                                "{} {}",
                                presence.name,
                                if failed { "failed" } else { "ready" }
                            ),
                            body: settled.completion_message.clone().unwrap_or_else(|| {
                                presence
                                    .status_text()
                                    .unwrap_or("Response ended")
                                    .to_string()
                            }),
                            level: if failed {
                                flowmux_core::NotificationLevel::Error
                            } else {
                                flowmux_core::NotificationLevel::TurnCompleted
                            },
                            ack,
                        }),
                    )
                    .await;
                }
                if let Some(entry) = crate::activity::ActivityEntry::from_hook_presence(located) {
                    self.activities.push(entry);
                }
            }
        }
        let changed = self
            .store
            .report_agent_screen_signals_with_visibility(
                surface,
                screen.as_deref(),
                title.as_deref(),
                self.is_agent_surface_visible(surface),
            )
            .await;
        let ws_id = changed
            .map(|(workspace, _)| workspace)
            .or(settled.workspace)?;
        self.schedule_agent_display_refresh(ws_id);
        Some(ws_id)
    }
    pub(super) async fn refresh_all_agent_screen_statuses(&self) {
        let surfaces: Vec<SurfaceId> = self
            .pane_registry
            .borrow()
            .terminals
            .keys()
            .copied()
            .collect();
        for surface in surfaces {
            self.refresh_agent_screen_status(surface, None).await;
        }
        self.refresh_agent_displays().await;
    }
    pub(super) async fn open_agent_bar_item(
        &self,
        workspace: WorkspaceId,
        pane: PaneId,
        surface: SurfaceId,
    ) {
        self.open_activity_target(workspace, pane, surface).await;
    }
    pub(super) async fn open_notification_id(&self, id: flowmux_core::NotificationId) -> bool {
        let Some(entry) = self.notifications.find(id) else {
            tracing::debug!(%id, "open notification: id not found");
            return false;
        };

        if let Some(binding) = entry.managed {
            let payload = crate::ui::workspace_view::ManagedAgentDndPayload {
                version: 1,
                host: flowmux_config::paths::agent_host_socket(),
                instance: binding.instance,
                epoch: binding.epoch,
                source_tab: None,
            };
            if let Err(error) = self.open_managed_agent(None, None, payload).await {
                self.clipboard_toast.show_with_message(&error);
                return false;
            }
            if let Some((_, pane, surface)) = self.store.agent_view(binding).await {
                self.reveal_notification_prompt(pane, surface);
            }
            if self.notifications.mark_read(id) {
                if let Some(desktop_id) = entry.desktop_id.clone() {
                    self.close_desktop_notifications(vec![desktop_id]);
                }
                self.clear_notification_attention_for_entry(&entry);
                self.refresh_launcher_badge();
            }
            self.window.present();
            return true;
        }

        // A tab can move to another pane/workspace after its notification was recorded.
        let target = {
            let registry = self.pane_registry.borrow();
            let pane = match entry.surface {
                Some(surface) => registry.pane_for_surface(surface),
                None => entry.pane,
            };
            pane.and_then(|pane| {
                let workspace = registry.workspace_of_pane(pane)?;
                let surface = entry.surface.or_else(|| registry.active_surface(pane))?;
                Some((workspace, pane, surface))
            })
        };
        let Some((workspace, pane, surface)) = target else {
            self.window.present();
            self.clipboard_toast.show_with_message(
                if entry.surface.is_some() || entry.pane.is_some() || entry.workspace.is_some() {
                    "The source tab is no longer open."
                } else {
                    "This notification has no source tab."
                },
            );
            return false;
        };

        if !self
            .activate_workspace_at(workspace, Some((pane, Some(surface))))
            .await
        {
            self.clipboard_toast
                .show_with_message("Could not open the source tab.");
            return false;
        }
        self.reveal_notification_prompt(pane, surface);

        let desktop_id = entry.desktop_id.clone();
        let changed = self.notifications.mark_read(id);
        if changed {
            if let Some(desktop_id) = desktop_id {
                self.close_desktop_notifications(vec![desktop_id]);
            }
            self.clear_notification_attention_for_entry(&entry);
            self.refresh_launcher_badge();
        }

        self.window.present();
        true
    }
    fn reveal_notification_prompt(&self, pane: PaneId, surface: SurfaceId) {
        let review = self.reviews.borrow().get(&pane).cloned();
        if let Some(review) = review {
            review.hide();
            self.focus_pane(pane);
        }
        if let Some(terminal) = self.pane_registry.borrow().terminals.get(&surface) {
            terminal.scroll_to_bottom();
        }
    }
    pub(super) fn close_desktop_notifications(&self, desktop_ids: Vec<String>) {
        self.notifications.close_desktop_notifications(desktop_ids);
    }
}
