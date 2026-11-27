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

fn provider_command(agent: SessionAgent) -> &'static str {
    match agent {
        SessionAgent::Claude => "claude",
        SessionAgent::Codex => "codex",
        SessionAgent::OpenCode => "opencode",
        SessionAgent::Antigravity => "agy",
        SessionAgent::Cline => "cline",
        SessionAgent::Gemini => "gemini",
    }
}

/// Which providers have a CLI on this process's PATH. When none is found the
/// PATH itself is the likely gap (the launch runs a login shell), so offer all.
fn installed_providers(installed: impl Fn(&str) -> bool) -> [bool; PROVIDERS.len()] {
    let found = PROVIDERS.map(|agent| installed(provider_command(agent)));
    if found.contains(&true) {
        found
    } else {
        [true; PROVIDERS.len()]
    }
}

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
    let command = provider_command(agent);
    let line = sessions::provider_shell_line(agent, cwd, &home, yolo, true, command)?;
    sessions::prepare_managed_launch(agent, cwd, line, agent.name().into(), shell)
}

impl WindowController {
    /// New Agent asks first whether to work with the agent in a chat or a terminal.
    pub(super) fn show_new_agent_dialog(&self) {
        if let Some(dialog) = self.new_agent_dialog.borrow().as_ref() {
            dialog.present(Some(&self.window));
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some("New Agent"),
            Some("Work with the agent in a chat, or run its CLI in a terminal tab."),
        );
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("cli", "Terminal (CLI)");
        dialog.add_response("chat", "Chat");
        dialog.set_response_appearance("chat", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("chat"));
        dialog.set_close_response("cancel");
        let controller = self.clone();
        dialog.connect_response(None, move |_, response| match response {
            "chat" => controller.show_new_agent_chat(),
            "cli" => controller.show_new_cli_agent_dialog(),
            _ => {}
        });
        dialog.present(Some(&self.window));
    }

    fn show_new_agent_chat(&self) {
        // A chat opens as a tab in a local pane of the visible workspace.
        let pane = self.focused_pane.get().filter(|pane| {
            !self.pane_registry.borrow().is_ssh_pane(*pane) && self.pane_in_visible_workspace(*pane)
        });
        let Some(pane) = pane else {
            self.clipboard_toast
                .show_with_message("Select a local pane to add a chat.");
            return;
        };
        let controller = self.clone();
        glib::spawn_future_local(async move {
            controller.show_new_chat_dialog(pane).await;
        });
    }

    fn show_new_cli_agent_dialog(&self) {
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
        // Without a local terminal pane the agent can still start in a new workspace.
        let destination = target.as_ref().map(|(pane, surface, _)| (*pane, *surface));
        let cwd = target
            .map(|(_, _, cwd)| cwd)
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        let dialog = adw::AlertDialog::new(
            Some("New Agent"),
            Some("Closing the agent's tab keeps it running in Background. Open it again from Agents."),
        );
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let providers: Vec<_> = PROVIDERS.iter().map(|agent| agent.name()).collect();
        let provider = gtk::DropDown::from_strings(&providers);
        provider.update_property(&[gtk::accessible::Property::Label("Agent provider")]);
        let installed = installed_providers(flowmux_cli::hook_install::agent_has_real_binary);
        // A provider without its CLI stays listed, but cannot be chosen.
        let rows = gtk::SignalListItemFactory::new();
        rows.connect_setup(|_, item| {
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            item.downcast_ref::<gtk::ListItem>()
                .unwrap()
                .set_child(Some(&label));
        });
        rows.connect_bind(move |_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let available = installed
                .get(item.position() as usize)
                .copied()
                .unwrap_or(true);
            let name = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .map(|name| name.string())
                .unwrap_or_default();
            let label = item.child().and_downcast::<gtk::Label>().unwrap();
            label.set_label(&if available {
                name.to_string()
            } else {
                format!("{name} (not installed)")
            });
            label.set_sensitive(available);
            item.set_selectable(available);
            item.set_activatable(available);
        });
        provider.set_list_factory(Some(&rows));
        provider.set_selected(installed.iter().position(|found| *found).unwrap_or(0) as u32);
        fields.append(&provider);
        let directory = gtk::Entry::new();
        directory.set_text(&cwd.to_string_lossy());
        directory.update_property(&[gtk::accessible::Property::Label("Project directory")]);
        directory.set_activates_default(true);
        fields.append(&directory);
        let new_workspace = gtk::CheckButton::with_label("Open in a new workspace");
        new_workspace.set_active(destination.is_none());
        new_workspace.set_sensitive(destination.is_some());
        fields.append(&new_workspace);
        let yolo = gtk::CheckButton::with_label("Automatically approve agent actions");
        fields.append(&yolo);
        dialog.set_extra_child(Some(&fields));
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("start", "Start agent");
        dialog.set_default_response(Some("start"));
        dialog.set_close_response("cancel");
        dialog.set_response_appearance("start", adw::ResponseAppearance::Suggested);
        let start = dialog.downgrade();
        provider.connect_selected_notify(move |provider| {
            if let Some(dialog) = start.upgrade() {
                let available = installed
                    .get(provider.selected() as usize)
                    .copied()
                    .unwrap_or(false);
                dialog.set_response_enabled("start", available);
            }
        });
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
            let destination = destination.filter(|_| !new_workspace.is_active());
            let controller = controller.clone();
            glib::MainContext::default().spawn_local(async move {
                if let Err(error) = controller
                    .launch_new_agent(destination, agent, cwd, yolo)
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
        // `None` opens the agent in a new workspace of its project directory.
        destination: Option<(PaneId, SurfaceId)>,
        agent: SessionAgent,
        cwd: PathBuf,
        yolo: bool,
    ) -> Result<(), String> {
        if let Some((pane, surface)) = destination {
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
        // The tab appears only once the Host has started the provider; say so meanwhile.
        self.clipboard_toast
            .show_with_message(&format!("Starting {}…", agent.name()));
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
        let opened = match destination {
            Some((pane, _)) => self.open_agent_view(pane, agent).await.map(|_| ()),
            None => self.open_agent_in_new_workspace(agent).await,
        };
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
    fn missing_provider_clis_are_disabled_unless_none_can_be_found() {
        assert_eq!(
            installed_providers(|command| matches!(command, "claude" | "agy")),
            [true, false, false, true, false, false]
        );
        assert_eq!(installed_providers(|_| false), [true; PROVIDERS.len()]);
    }

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
