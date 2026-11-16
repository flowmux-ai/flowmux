// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit Host-owned launch from Agents; ordinary shell commands stay tab-bound.

use super::*;
use flowmux_state::session_history::SessionAgent;

const PROVIDERS: [SessionAgent; 6] = [
    SessionAgent::Claude,
    SessionAgent::Codex,
    SessionAgent::OpenCode,
    SessionAgent::Antigravity,
    SessionAgent::Cline,
    SessionAgent::Gemini,
];

fn prepare_new_agent_launch(
    agent: SessionAgent,
    cwd: &std::path::Path,
    yolo: bool,
    shell: String,
) -> Result<flowmux_ipc::agent_host::LaunchAgent, String> {
    let home = sessions::session_home(&sessions::SessionTarget {
        agent,
        focused: None,
    })?;
    let command = match agent {
        SessionAgent::Claude => "claude",
        SessionAgent::Codex => "codex",
        SessionAgent::OpenCode => "opencode",
        SessionAgent::Antigravity => "agy",
        SessionAgent::Cline => "cline",
        SessionAgent::Gemini => "gemini",
    };
    let line = sessions::provider_shell_line(agent, cwd, &home, yolo, true, command)?;
    sessions::prepare_managed_launch(agent, cwd, line, agent.name().into(), shell)
}

impl WindowController {
    pub(super) fn show_new_agent_dialog(&self) {
        if let Some(dialog) = self.new_agent_dialog.borrow().as_ref() {
            dialog.present(Some(&self.window));
            return;
        }
        let target = self.focused_pane.get().and_then(|pane| {
            let registry = self.pane_registry.borrow();
            if registry.is_ssh_pane(pane) || !self.pane_in_visible_workspace(pane) {
                return None;
            }
            registry.active_terminal(pane)?;
            Some((
                pane,
                registry.active_surface(pane)?,
                registry.current_dir_for_pane(pane)?,
            ))
        });
        let Some((pane, surface, cwd)) = target else {
            self.clipboard_toast
                .show_with_message("Focus a local terminal pane to start an agent");
            return;
        };
        let dialog = adw::AlertDialog::new(
            Some("New Agent"),
            Some("Closing the agent's tab keeps it running in Background. Open it again from Agents."),
        );
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let providers: Vec<_> = PROVIDERS.iter().map(|agent| agent.name()).collect();
        let provider = gtk::DropDown::from_strings(&providers);
        provider.update_property(&[gtk::accessible::Property::Label("Agent provider")]);
        fields.append(&provider);
        let directory = gtk::Entry::new();
        directory.set_text(&cwd.to_string_lossy());
        directory.update_property(&[gtk::accessible::Property::Label("Project directory")]);
        directory.set_activates_default(true);
        fields.append(&directory);
        let yolo = gtk::CheckButton::with_label("Automatically approve agent actions");
        fields.append(&yolo);
        dialog.set_extra_child(Some(&fields));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("start", "Start agent");
        dialog.set_default_response(Some("start"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("start", adw::ResponseAppearance::Suggested);
        *self.new_agent_dialog.borrow_mut() = Some(dialog.clone());
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
            controller.new_agent_dialog.borrow_mut().take();
            if response != "start" {
                return;
            }
            let Some(agent) = PROVIDERS.get(provider.selected() as usize).copied() else {
                return;
            };
            let cwd = PathBuf::from(directory.text().as_str());
            let yolo = yolo.is_active();
            let controller = controller.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Err(error) = controller
                    .launch_new_agent(pane, surface, agent, cwd, yolo)
                    .await
                {
                    show_error_dialog(&controller.window, "Cannot start agent", &error).await;
                }
            });
        });
        dialog.present(Some(&self.window));
    }

    async fn launch_new_agent(
        &self,
        pane: PaneId,
        surface: SurfaceId,
        agent: SessionAgent,
        cwd: PathBuf,
        yolo: bool,
    ) -> Result<(), String> {
        {
            let registry = self.pane_registry.borrow();
            if registry.is_ssh_pane(pane) || !registry.has_surface(pane, surface) {
                return Err(
                    "The destination changed or closed. Choose a local pane and try again.".into(),
                );
            }
        }
        let handle = self
            .tokio_handle
            .as_ref()
            .ok_or("Agent Host runtime unavailable")?;
        let shell = self
            .options
            .borrow()
            .default_shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/bash".into());
        let launch = prepare_new_agent_launch(agent, &cwd, yolo, shell)?;
        let socket = flowmux_config::paths::agent_host_socket();
        let agent = handle
            .spawn(async move {
                flowmux_cli::agent_host::launch(&socket, launch)
                    .await
                    .map_err(|e| format!("{e:#}"))
            })
            .await
            .map_err(|error| {
                format!("Launch result unknown: {error}. Check Agents before retrying.")
            })??;
        let instance = agent.instance;
        let opened = self.open_agent_view(pane, agent).await;
        self.refresh_agent_displays().await;
        opened.map_err(|error| {
            format!("Agent {instance} is running in Background; cannot open tab: {error}")
        })?;
        self.clipboard_toast
            .show_with_message("Agent started. Closing its tab keeps it running in Background.");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_agent_launch_validates_directory_and_preserves_explicit_permissions() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project ' 한글 $(exit 9)");
        std::fs::create_dir(&cwd).unwrap();
        for agent in PROVIDERS {
            let safe = prepare_new_agent_launch(agent, &cwd, false, "/bin/sh".into()).unwrap();
            let yolo = prepare_new_agent_launch(agent, &cwd, true, "/bin/sh".into()).unwrap();
            assert_eq!(safe.provider, agent.name().to_ascii_lowercase());
            assert_eq!(safe.cwd, cwd);
            assert!(safe.argv[2].contains(&format!(
                "cd {} && exec env",
                shell_quote(cwd.to_str().unwrap())
            )));
            assert!(!safe.argv[2].contains("--resume"));
            assert_ne!(safe.argv[2], yolo.argv[2]);
        }
        for invalid in [PathBuf::from("relative"), root.path().join("missing")] {
            assert!(prepare_new_agent_launch(
                SessionAgent::Claude,
                &invalid,
                false,
                "/bin/sh".into()
            )
            .is_err());
        }
    }
}
