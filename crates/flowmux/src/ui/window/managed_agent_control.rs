// SPDX-License-Identifier: GPL-3.0-or-later
//! Closing a view detaches; stopping an execution requires an explicit action.

use super::*;
use flowmux_ipc::agent_host::TerminateMode;

impl WindowController {
    pub(super) async fn control_managed_agent(
        &self,
        binding: flowmux_core::AgentViewBinding,
        mode: TerminateMode,
    ) -> Result<(), String> {
        let agent = {
            let registry = self.bridge.agent_registry.borrow();
            if !registry.available {
                return Err("Agent Host is unavailable; execution state is unknown.".into());
            }
            registry
                .agents
                .iter()
                .find(|agent| agent.instance == binding.instance && agent.epoch == binding.epoch)
                .cloned()
                .ok_or("Agent is no longer in this Host")?
        };
        if mode != TerminateMode::Interrupt {
            let force = mode == TerminateMode::Force;
            let dialog = adw::AlertDialog::new(
                Some(if force { "Force terminate agent?" } else { "Terminate agent?" }),
                Some(&format!(
                    "{} · {}\n{}\nExecution {}\n\n{}\nClosing a tab keeps the agent running in Background.",
                    agent.provider, agent.label, agent.cwd.display(), agent.instance,
                    if force {
                        "Normal termination timed out. Force termination stops this execution and its owned child processes immediately. Unsaved work may be lost."
                    } else {
                        "This stops this execution and its owned child processes. Work in progress may be lost."
                    }
                )),
            );
            dialog.add_response("cancel", "Cancel");
            dialog.add_response(
                "stop",
                if force {
                    "Force terminate"
                } else {
                    "Terminate agent"
                },
            );
            dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
            dialog.set_default_response(Some("cancel"));
            dialog.set_close_response("cancel");
            let _native_view_suspend =
                crate::ui::browser_pane::suspend_native_browser_views_for_window(
                    self.window.upcast_ref(),
                );
            if dialog.choose_future(&self.window).await != "stop" {
                return Ok(());
            }
        }
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
            })??;
        self.clipboard_toast.show_with_message(match mode {
            TerminateMode::Interrupt => "Ctrl+C sent. Waiting for the agent's next state.",
            TerminateMode::Terminate => {
                "Termination requested. Force termination becomes available if it times out."
            }
            TerminateMode::Force => "Force termination requested. Waiting for process exit.",
        });
        Ok(())
    }
}
