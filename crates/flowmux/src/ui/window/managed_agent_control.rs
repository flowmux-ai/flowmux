// SPDX-License-Identifier: GPL-3.0-or-later
//! Closing a view detaches; stopping an execution requires an explicit action.

use super::*;
use flowmux_ipc::agent_host::{HostClient, TerminateMode};
use std::os::unix::fs::{FileTypeExt, MetadataExt};

impl WindowController {
    pub(crate) fn show_stop_other_agent_host_dialog(&self, window: &adw::ApplicationWindow) {
        let controller = self.clone();
        let window = window.clone();
        glib::spawn_future_local(async move {
            if let Err(error) = controller.stop_other_agent_host(&window).await {
                show_error_dialog(&window, "Cannot stop Agent Host", &format!("{error:#}")).await;
            }
        });
    }

    async fn stop_other_agent_host(&self, window: &adw::ApplicationWindow) -> anyhow::Result<()> {
        let current = flowmux_config::paths::agent_host_socket();
        let hosts: Vec<_> = self
            .bridge
            .agent_registry
            .borrow()
            .other_hosts
            .iter()
            .filter(|socket| **socket != current)
            .filter_map(|socket| Some((socket.clone(), socket_identity(socket)?)))
            .collect();
        let paths: Vec<_> = hosts
            .iter()
            .map(|(socket, _)| socket.to_string_lossy())
            .collect();
        let body = "Stopping this Host ends its agents and the shells of its terminal tabs. Work in progress may be lost.";
        let body = match paths.as_slice() {
            [] => return Ok(()),
            [path] => format!("{body}\n\n{path}"),
            _ => format!("Choose which Host to stop. {body}"),
        };
        let dialog = adw::AlertDialog::new(Some("Stop other Agent Host?"), Some(&body));
        let picker = gtk::DropDown::from_strings(
            &paths.iter().map(|path| path.as_ref()).collect::<Vec<_>>(),
        );
        if hosts.len() > 1 {
            picker.update_property(&[gtk::accessible::Property::Label("Agent Host socket")]);
            dialog.set_extra_child(Some(&picker));
        }
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("stop", "Stop Host");
        dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let response = {
            let _native_view_suspend =
                crate::ui::browser_pane::suspend_native_browser_views_for_window(
                    window.upcast_ref(),
                );
            dialog.choose_future(window).await
        };
        if response != "stop" {
            return Ok(());
        }
        let Some((socket, identity)) = hosts.into_iter().nth(picker.selected() as usize) else {
            return Ok(());
        };
        if !self
            .bridge
            .agent_registry
            .borrow()
            .other_hosts
            .contains(&socket)
        {
            return Ok(());
        }
        let handle = self
            .tokio_handle
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Agent Host runtime unavailable"))?;
        let stopped = handle
            .spawn(async move {
                match socket_identity(&socket) {
                    None => return Ok(false),
                    Some(found) if found != identity => {
                        anyhow::bail!("The Agent Host changed. Reopen the dialog to stop it.")
                    }
                    _ => {}
                }
                let result = tokio::time::timeout(
                    Duration::from_secs(3),
                    HostClient::stop_all(&socket, false),
                )
                .await
                .map_err(|_| {
                    anyhow::anyhow!("Stop result unknown. Check Agents before retrying.")
                })?;
                if result.as_ref().is_err_and(|error| {
                    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                        matches!(
                            error.kind(),
                            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                        )
                    })
                }) {
                    return Ok(false);
                }
                result.map(|()| true)
            })
            .await??;
        if stopped {
            self.clipboard_toast
                .show_with_message("Stop requested for the selected Agent Host.");
        }
        Ok(())
    }

    pub(super) async fn control_managed_agent(
        &self,
        binding: flowmux_core::AgentViewBinding,
        mode: TerminateMode,
    ) -> Result<(), String> {
        {
            let registry = self.bridge.agent_registry.borrow();
            if !registry.available {
                return Err("Agent Host is unavailable; execution state is unknown.".into());
            }
            if !registry
                .agents
                .iter()
                .any(|agent| agent.instance == binding.instance && agent.epoch == binding.epoch)
            {
                return Err("Agent is no longer in this Host".into());
            }
        }
        self.send_agent_control(binding, mode).await?;
        self.clipboard_toast.show_with_message(match mode {
            TerminateMode::Interrupt => "Ctrl+C sent. Waiting for the agent's next state.",
            TerminateMode::Terminate => {
                "Termination requested. Force termination becomes available if it times out."
            }
            TerminateMode::Force => "Force termination requested. Waiting for process exit.",
        });
        Ok(())
    }

    async fn send_agent_control(
        &self,
        binding: flowmux_core::AgentViewBinding,
        mode: TerminateMode,
    ) -> Result<(), String> {
        let handle = self
            .tokio_handle
            .as_ref()
            .ok_or("Agent Host runtime unavailable")?;
        let socket = flowmux_config::paths::agent_host_socket();
        handle
            .spawn(async move {
                flowmux_cli::agent_host::control(&socket, binding, mode)
                    .await
                    .map_err(|error| format!("{error:#}"))
            })
            .await
            .map_err(|error| {
                format!("Control result unknown: {error}. Check Agents before retrying.")
            })?
    }

    /// Agents the Host is running: managed ones and terminals with an agent in their shell.
    pub(super) fn running_agents(&self) -> Vec<flowmux_ipc::agent_host::AgentInstance> {
        let registry = self.bridge.agent_registry.borrow();
        if !registry.available {
            return Vec::new();
        }
        registry
            .agents
            .iter()
            .filter(|agent| {
                agent.state == flowmux_ipc::agent_host::ProcessState::Running
                    && agent.exit_status.is_none()
                    && (!agent.shell || agent.presence.is_some())
            })
            .cloned()
            .collect()
    }

    /// Closing the window leaves its agents working in Background. Ask once whether
    /// that is wanted: `Some(true)` stops them, `Some(false)` keeps them, `None` cancels.
    pub(super) async fn confirm_close_with_agents(&self) -> Option<bool> {
        let count = self.running_agents().len();
        if count == 0 {
            return Some(false);
        }
        // Scripts and tests that close windows answer ahead of time.
        match std::env::var("FLOWMUX_CLOSE_AGENTS").as_deref() {
            Ok("keep") => return Some(false),
            Ok("stop") => return Some(true),
            _ => {}
        }
        if crate::ui::is_unattended_close() {
            return Some(false);
        }
        let dialog = close_with_agents_dialog(count);
        let _native_view_suspend = crate::ui::browser_pane::suspend_native_browser_views_for_window(
            self.window.upcast_ref(),
        );
        match dialog.choose_future(&self.window).await.as_str() {
            "keep" => Some(false),
            "stop" => Some(true),
            _ => None,
        }
    }

    /// Stop every agent, and the whole Host with them unless another window shows something from
    /// it.
    pub(super) async fn stop_agents(&self) {
        let Some(handle) = self.tokio_handle.clone() else {
            return;
        };
        let agents = self.running_agents();
        let shared = self
            .bridge
            .agent_registry
            .borrow()
            .agents
            .iter()
            .any(|agent| {
                agent
                    .view
                    .as_ref()
                    .is_some_and(|view| view.window != self.flow.owner)
            });
        let socket = flowmux_config::paths::agent_host_socket();
        let stopped = handle
            .spawn(async move {
                if !shared {
                    return HostClient::stop_all(&socket, false).await;
                }
                for agent in agents {
                    let binding = flowmux_core::AgentViewBinding {
                        instance: agent.instance,
                        epoch: agent.epoch,
                    };
                    flowmux_cli::agent_host::control(&socket, binding, TerminateMode::Terminate)
                        .await?;
                }
                Ok(())
            })
            .await;
        if let Ok(Err(error)) = stopped {
            tracing::warn!(%error, "agents could not be stopped while closing the window");
        }
    }

    /// Closing a managed tab only detaches, so ask whether the agent should keep running.
    /// Returns whether the tab should close.
    pub(super) async fn confirm_close_agent_tab(
        &self,
        binding: flowmux_core::AgentViewBinding,
        title: &str,
    ) -> bool {
        // Nothing to decide for an ended or unobservable execution.
        if self.managed_session_agent(binding).is_none() {
            return true;
        }
        let dialog = adw::AlertDialog::new(
            Some("Close agent tab?"),
            Some(&format!(
                "“{title}” is still running. Keep it running in Background and reopen it from Agents, or terminate it. Terminating may lose work in progress."
            )),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("stop", "Terminate agent");
        dialog.add_response("keep", "Keep running");
        dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("keep"));
        dialog.set_close_response("cancel");
        let _native_view_suspend = crate::ui::browser_pane::suspend_native_browser_views_for_window(
            self.window.upcast_ref(),
        );
        match dialog.choose_future(&self.window).await.as_str() {
            "keep" => true,
            "stop" => match self
                .send_agent_control(binding, TerminateMode::Terminate)
                .await
            {
                Ok(()) => true,
                Err(error) => {
                    show_error_dialog(&self.window, "Cannot terminate agent", &error).await;
                    false
                }
            },
            _ => false,
        }
    }
}

fn socket_identity(socket: &std::path::Path) -> Option<(u64, u64)> {
    let metadata = std::fs::symlink_metadata(socket).ok()?;
    metadata
        .file_type()
        .is_socket()
        .then(|| (metadata.dev(), metadata.ino()))
}

/// The one question a closing window asks about the agents it would leave behind.
pub(super) fn close_with_agents_dialog(count: usize) -> adw::AlertDialog {
    let (heading, them) = if count == 1 {
        ("1 agent is still running".to_owned(), "it")
    } else {
        (format!("{count} agents are still running"), "them")
    };
    let dialog = adw::AlertDialog::new(
        Some(&heading),
        Some(&format!(
            "Keep {them} working in Background, or stop {them} along with the Agent runtime. Stopping may lose work in progress."
        )),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("stop", "Stop agents");
    dialog.add_response("keep", "Keep running");
    dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("keep"));
    dialog.set_close_response("cancel");
    dialog
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    fn closing_window_asks_about_agents_with_keep_as_default_and_cancel_on_dismiss() {
        adw::init().expect("libadwaita should initialize in GTK test");
        let dialog = close_with_agents_dialog(2);
        assert_eq!(
            dialog.heading().as_deref(),
            Some("2 agents are still running")
        );
        assert_eq!(
            close_with_agents_dialog(1).heading().as_deref(),
            Some("1 agent is still running")
        );
        assert_eq!(dialog.default_response().as_deref(), Some("keep"));
        assert_eq!(dialog.close_response(), "cancel");
        assert_eq!(
            dialog.response_appearance("stop"),
            adw::ResponseAppearance::Destructive
        );
        for response in ["cancel", "stop", "keep"] {
            assert!(dialog.has_response(response), "missing response {response}");
        }
    }
}
