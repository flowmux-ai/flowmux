// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit destinations preserve the running execution and the user's other tabs.

use super::*;
use crate::ui::workspace_view::ManagedAgentDndPayload;
use flowmux_ipc::agent_host::AgentInstance;

struct Destination {
    workspace: WorkspaceId,
    pane: PaneId,
    label: String,
}

fn destinations(workspaces: &[flowmux_core::Workspace]) -> Vec<Destination> {
    let mut result = Vec::new();
    for workspace in workspaces.iter().filter(|ws| ws.local_root().is_some()) {
        let mut index = 0;
        for root in &workspace.surfaces {
            root.root_pane.for_each_leaf(|pane| {
                index += 1;
                let title = match root.root_pane.find_leaf_content(pane) {
                    Some(flowmux_core::PaneContent::Tabs { surfaces, .. }) => surfaces
                        .into_iter()
                        .find(|tab| matches!(tab.kind, SurfaceKind::Terminal { .. }))
                        .map(|tab| tab.title),
                    Some(flowmux_core::PaneContent::Terminal { .. }) => Some("Terminal".into()),
                    _ => None,
                };
                if let Some(title) = title {
                    result.push(Destination {
                        workspace: workspace.id,
                        pane,
                        label: format!("{} / Pane {index} / {title}", workspace.display_title()),
                    });
                }
            });
        }
    }
    result
}

impl WindowController {
    pub(super) async fn tear_off_managed_agent(
        &self,
        pane: PaneId,
        surface: SurfaceId,
        binding: flowmux_core::AgentViewBinding,
    ) -> Result<(), String> {
        let handle = self
            .tokio_handle
            .as_ref()
            .ok_or("Agent Host runtime unavailable")?;
        let agent = handle
            .spawn(async move {
                crate::agent_host::lookup_running(
                    &flowmux_config::paths::agent_host_socket(),
                    binding.instance,
                    Some(binding.epoch),
                )
                .await
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| format!("{error:?}"))?;
        if !self
            .verified_agent_location(&agent)
            .await
            .is_some_and(|location| location.pane == pane && location.surface == surface)
        {
            return Err(
                "The agent's view changed or disconnected. Reopen it from Agents before moving it."
                    .into(),
            );
        }
        let executable = crate::keybindings::new_window_executable(
            std::env::current_exe().ok(),
            std::env::args_os().next().map(std::path::PathBuf::from),
        )
        .ok_or("Cannot find the Flowmux executable")?;
        #[cfg(not(target_os = "macos"))]
        let activation = {
            let display = gtk::prelude::WidgetExt::display(&self.window);
            let context = display.app_launch_context();
            let info = gtk::gio::AppInfo::create_from_commandline(
                glib::shell_quote(&executable),
                Some("Flowmux"),
                gtk::gio::AppInfoCreateFlags::SUPPORTS_STARTUP_NOTIFICATION,
            )
            .map_err(|error| error.to_string())?;
            context.startup_notify_id(Some(&info), &[]).map(|token| {
                (
                    if display.type_().name() == "GdkWaylandDisplay" {
                        "XDG_ACTIVATION_TOKEN"
                    } else {
                        "DESKTOP_STARTUP_ID"
                    },
                    token.to_string(),
                )
            })
        };
        #[cfg(target_os = "macos")]
        let activation = None;
        // The existing takeover closes the old view only after the new relay is ready.
        handle
            .spawn(crate::agent_host::open_in_new_window(
                agent, executable, activation,
            ))
            .await
            .map_err(|error| {
                format!("Move result unknown: {error}. Check Agents before retrying.")
            })?
            .map_err(|error| format!("{error:#}"))
    }

    pub(super) async fn commit_agent_view_ready(
        &self,
        surface: SurfaceId,
        token: uuid::Uuid,
        key: flowmux_ipc::agent_host::AttachmentKey,
        sequence: u64,
        relay_pid: u32,
        relay_start: u64,
    ) -> Result<(), String> {
        let takeover = {
            let registry = self.pane_registry.borrow();
            let terminal = registry
                .terminals
                .get(&surface)
                .ok_or("Agent destination closed")?;
            let connection = terminal.agent_connection.borrow();
            let connection = connection
                .as_ref()
                .ok_or("No explicit move is pending for this Agent view")?;
            let takeover = connection
                .takeover
                .ok_or("No explicit move is pending for this Agent view")?;
            if connection.started.elapsed() >= std::time::Duration::from_secs(10) {
                return Err("Agent destination connection timed out before admission".into());
            }
            let terminal_pid = terminal.pid.get().and_then(|pid| u32::try_from(pid).ok());
            if takeover.token != token
                || terminal.id() != takeover.pane
                || connection.binding.instance != key.instance
                || connection.binding.epoch != key.epoch
                || takeover.previous.is_some_and(|previous| {
                    previous.instance != key.instance
                        || previous.epoch != key.epoch
                        || previous.generation >= key.generation
                })
                || flowmux_procmon::process_start_time(relay_pid) != Some(relay_start)
                || terminal_pid.is_none()
                || (terminal_pid != Some(relay_pid)
                    && flowmux_procmon::parent_pid(relay_pid) != terminal_pid)
            {
                return Err("Agent destination or relay changed before admission".into());
            }
            takeover
        };
        let binding = flowmux_core::AgentViewBinding {
            instance: key.instance,
            epoch: key.epoch,
        };
        if self.store.workspace_for_pane(takeover.pane).await != Some(takeover.workspace)
            || self
                .store
                .surface_agent_binding(takeover.pane, surface)
                .await
                != Some(binding)
            || !self.window.is_realized()
        {
            return Err("Agent destination layout changed before admission".into());
        }
        let handle = self
            .tokio_handle
            .as_ref()
            .ok_or("Agent Host runtime unavailable")?;
        handle
            .spawn(async move {
                use flowmux_ipc::agent_host::{ClientRole, HostClient, HostRequest, HostResponse};
                tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    let host = HostClient::connect(
                        &flowmux_config::paths::agent_host_socket(),
                        ClientRole::Gui,
                    )
                    .await?;
                    let result = host
                        .call(HostRequest::GuiViewReady {
                            key,
                            surface,
                            sequence,
                            relay_pid,
                            relay_start,
                        })
                        .await?;
                    anyhow::ensure!(
                        matches!(result, HostResponse::Ok),
                        "Unexpected Agent readiness response"
                    );
                    Ok::<_, anyhow::Error>(())
                })
                .await
                .map_err(|_| {
                    anyhow::anyhow!("Agent admission timed out; outcome unknown, not retried")
                })?
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
    }

    pub(super) async fn lookup_managed_agent(
        &self,
        payload: &ManagedAgentDndPayload,
    ) -> Result<AgentInstance, String> {
        payload.validate_local()?;
        let handle = self
            .tokio_handle
            .as_ref()
            .ok_or("Agent Host runtime unavailable")?;
        let payload = payload.clone();
        handle
            .spawn(async move {
                crate::agent_host::lookup_running(
                    &payload.host,
                    payload.instance,
                    Some(payload.epoch),
                )
                .await
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|error| match error {
                flowmux_ipc::RpcError::Busy(message)
                | flowmux_ipc::RpcError::InvalidArgument(message) => message,
                other => format!("{other:?}"),
            })
    }

    pub(super) async fn choose_managed_agent_destination(
        &self,
        payload: ManagedAgentDndPayload,
    ) -> Result<(), String> {
        let agent = self.lookup_managed_agent(&payload).await?;
        self.show_managed_agent_destination(payload, agent).await;
        Ok(())
    }

    pub(super) async fn show_managed_agent_destination(
        &self,
        payload: ManagedAgentDndPayload,
        agent: AgentInstance,
    ) {
        if let Some(dialog) = self.agent_destination_dialog.borrow().as_ref() {
            dialog.present(Some(&self.window));
            return;
        }
        let targets = destinations(&self.store.ordered_workspaces().await);
        let visible = self.visible_workspace();
        let preferred = self
            .focused_pane
            .get()
            .filter(|pane| {
                targets
                    .iter()
                    .any(|target| Some(target.workspace) == visible && target.pane == *pane)
            })
            .or_else(|| {
                let mru = self.focus_mru.borrow();
                mru.get(&visible?).and_then(|panes| {
                    panes.iter().copied().find(|pane| {
                        targets
                            .iter()
                            .any(|target| Some(target.workspace) == visible && target.pane == *pane)
                    })
                })
            });
        let selected = targets
            .iter()
            .position(|target| Some(target.workspace) == visible && Some(target.pane) == preferred)
            .or_else(|| {
                targets
                    .iter()
                    .position(|target| Some(target.workspace) == visible)
            })
            .unwrap_or(targets.len());
        let moving = agent.attachment.is_some();
        let dialog = adw::AlertDialog::new(
            Some(if moving { "Move agent to…" } else { "Open agent in…" }),
            Some(&format!(
                "{} · {}\nChoose a local terminal pane. The same agent keeps running; existing tabs are preserved.",
                agent.provider, agent.label
            )),
        );
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let mut labels: Vec<_> = targets.iter().map(|target| target.label.as_str()).collect();
        labels.push("New workspace");
        let destination = gtk::DropDown::from_strings(&labels);
        destination.set_selected(selected as u32);
        let destination_label = gtk::Label::new(Some("Agent destination"));
        destination_label.set_xalign(0.0);
        destination.update_relation(&[gtk::accessible::Relation::LabelledBy(&[
            destination_label.upcast_ref()
        ])]);
        fields.append(&destination_label);
        fields.append(&destination);
        let directory = gtk::Label::new(Some(&format!(
            "New workspaces use the agent's project: {}",
            agent.cwd.display()
        )));
        directory.set_wrap(true);
        directory.set_xalign(0.0);
        fields.append(&directory);
        dialog.set_extra_child(Some(&fields));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("open", if moving { "Move agent" } else { "Open agent" });
        dialog.set_default_response(Some("open"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("open", adw::ResponseAppearance::Suggested);
        *self.agent_destination_dialog.borrow_mut() = Some(dialog.clone());
        let controller = self.clone();
        let submitted = Cell::new(false);
        let native_browser_suspend = Rc::new(RefCell::new(Some(
            crate::ui::browser_pane::suspend_native_browser_views_for_window(
                self.window.upcast_ref(),
            ),
        )));
        dialog.connect_response(None, move |_, response| {
            if submitted.replace(true) {
                return;
            }
            native_browser_suspend.borrow_mut().take();
            controller.agent_destination_dialog.borrow_mut().take();
            if response != "open" {
                return;
            }
            let selected = destination.selected() as usize;
            if selected > targets.len() {
                return;
            }
            let pane = targets.get(selected).map(|target| target.pane);
            let controller = controller.clone();
            let payload = payload.clone();
            let expected = agent.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Err(error) = controller
                    .open_agent_at_destination(payload, expected, pane)
                    .await
                {
                    show_error_dialog(&controller.window, "Cannot open agent", &error).await;
                }
            });
        });
        dialog.present(Some(&self.window));
    }

    async fn open_agent_at_destination(
        &self,
        payload: ManagedAgentDndPayload,
        expected: AgentInstance,
        pane: Option<PaneId>,
    ) -> Result<(), String> {
        let agent = self.lookup_managed_agent(&payload).await?;
        if agent.attachment.as_ref().map(|a| a.key) != expected.attachment.as_ref().map(|a| a.key) {
            return Err("The agent's view changed. Choose its destination again.".into());
        }
        let mut created = None;
        let (workspace, pane) = match pane {
            Some(pane) => destinations(&self.store.ordered_workspaces().await)
                .into_iter()
                .find(|target| target.pane == pane)
                .map(|target| (target.workspace, target.pane))
                .ok_or("The destination changed or closed. Choose a local terminal pane again.")?,
            None => {
                if !agent.cwd.is_absolute() || !agent.cwd.is_dir() {
                    return Err("The agent's project directory is unavailable.".into());
                }
                let workspace = self.store.create_workspace(None, agent.cwd.clone()).await;
                let ws = self
                    .store
                    .get_workspace_without_scrollback(workspace)
                    .await
                    .ok_or("New workspace is unavailable")?;
                let pane = ws
                    .surfaces
                    .first()
                    .and_then(|root| root.root_pane.first_leaf_id())
                    .ok_or("New workspace has no terminal pane")?;
                self.render_workspace_with_activation(&ws, false);
                created = Some(workspace);
                (workspace, pane)
            }
        };
        let surface = match self.open_agent_view(pane, agent).await {
            Ok((_, surface)) => surface,
            Err(error) => {
                if let Some(workspace) = created {
                    self.store.remove_workspace(workspace).await;
                    self.drop_workspace(workspace);
                }
                return Err(error);
            }
        };
        self.close_agent_office();
        self.dismiss_workspace_overview_immediately();
        self.activate_workspace_at(workspace, Some((pane, Some(surface))))
            .await;
        self.focus_pane(pane);
        self.refresh_agent_displays().await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn destinations_exclude_ssh_and_panes_without_terminals_and_keep_workspace_order() {
        let store = StateStore::new_lazy(flowmux_state::State::default());
        let first = store
            .create_workspace(Some("First".into()), "/tmp".into())
            .await;
        let second = store
            .create_workspace(Some("Second".into()), "/tmp".into())
            .await;
        store
            .create_ssh_workspace(
                None,
                flowmux_core::SshWorkspaceConfig {
                    target: flowmux_core::SshTarget::parse("devbox").unwrap(),
                    cwd: None,
                    tmux: false,
                    forwards: vec![],
                },
            )
            .await
            .unwrap();
        let mut workspaces = store.ordered_workspaces().await;
        let choices = destinations(&workspaces);
        assert_eq!(
            choices.iter().map(|d| d.workspace).collect::<Vec<_>>(),
            [first, second]
        );
        assert!(choices[0].label.starts_with("First / Pane 1 / "));
        let root = &mut workspaces[0].surfaces[0].root_pane;
        if let Pane::Leaf { content, .. } = root {
            let browser = PaneSurface::browser("Browser", "about:blank".into());
            *content = PaneContent::Tabs {
                active: browser.id,
                surfaces: vec![browser],
            };
        } else {
            panic!("new workspace should have one pane");
        }
        assert_eq!(
            destinations(&workspaces)
                .iter()
                .map(|d| d.workspace)
                .collect::<Vec<_>>(),
            [second]
        );
        assert!(destinations(&[]).is_empty());
    }
}
