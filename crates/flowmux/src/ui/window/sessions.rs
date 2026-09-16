// SPDX-License-Identifier: GPL-3.0-or-later
//! Focus-bound session browsing and native resume commands.

use super::*;
use crate::ui::session_panel::{SessionPanel, SessionPanelAction};
use flowmux_state::session_history::{list_sessions, HistorySession, SessionAgent};

struct SessionHome {
    path: PathBuf,
    environment: std::collections::BTreeMap<String, String>,
    arguments: Vec<String>,
}

fn resume_shell_line(session: &HistorySession, home: &SessionHome) -> Result<String, String> {
    let cwd = session
        .cwd
        .to_str()
        .ok_or("Project directory is not UTF-8")?;
    if !session.cwd.is_absolute()
        || cwd
            .chars()
            .chain(home.environment.values().flat_map(|v| v.chars()))
            .chain(home.arguments.iter().flat_map(|v| v.chars()))
            .any(char::is_control)
    {
        return Err("Cannot safely open this session's directory in a terminal".into());
    }
    let unset = session
        .agent
        .home_variables()
        .iter()
        .filter(|key| **key != "HOME")
        .map(|key| format!("-u {key}"))
        .collect::<Vec<_>>()
        .join(" ");
    let environment = home
        .environment
        .iter()
        .map(|(key, value)| shell_quote(&format!("{key}={value}")))
        .collect::<Vec<_>>()
        .join(" ");
    let arguments = home
        .arguments
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    Ok(format!(
        "cd {} && env {unset} {environment} {} {arguments}",
        shell_quote(cwd),
        session.resume_command().map_err(|e| e.to_string())?
    ))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SessionTarget {
    pane: PaneId,
    surface: SurfaceId,
    agent: SessionAgent,
    root_pid: u32,
    session_id: Option<String>,
}

#[derive(Clone)]
pub(super) struct SessionPanelState {
    pub panel: SessionPanel,
    target: Rc<RefCell<Option<SessionTarget>>>,
}

impl SessionPanelState {
    pub fn new(panel: SessionPanel) -> Self {
        Self {
            panel,
            target: Rc::new(RefCell::new(None)),
        }
    }
}

/// Resolve the actual agent's home, including overrides set inside the shell.
fn session_home(target: &SessionTarget) -> Result<SessionHome, String> {
    let pids = flowmux_procmon::descendants(target.root_pid).map_err(|e| e.to_string())?;
    let mut matching: Vec<_> = pids
        .into_iter()
        .filter(|pid| {
            flowmux_procmon::agent_name_for_pid(*pid).and_then(SessionAgent::from_name)
                == Some(target.agent)
        })
        .collect();
    matching.sort_unstable();
    let pid = matching
        .first()
        .ok_or("The agent is no longer running in this tab")?;
    let mut environment = std::collections::BTreeMap::new();
    let mut arguments = Vec::new();
    #[cfg(target_os = "linux")]
    {
        let bytes = std::fs::read(format!("/proc/{pid}/environ")).map_err(|e| e.to_string())?;
        for key in target
            .agent
            .home_variables()
            .iter()
            .copied()
            .chain(["PATH"])
        {
            let prefix = format!("{key}=");
            if let Some(value) = bytes
                .split(|b| *b == 0)
                .find_map(|entry| entry.strip_prefix(prefix.as_bytes()))
                .filter(|v| !v.is_empty())
            {
                environment.insert(
                    key.to_string(),
                    String::from_utf8(value.to_vec()).map_err(|e| e.to_string())?,
                );
            }
        }
        if target.agent == SessionAgent::Cline {
            let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).map_err(|e| e.to_string())?;
            let argv: Vec<_> = bytes
                .split(|b| *b == 0)
                .filter_map(|v| std::str::from_utf8(v).ok())
                .collect();
            for (flag, variable) in [("--config", "CLINE_DIR"), ("--data-dir", "CLINE_DATA_DIR")] {
                if let Some(value) = argv.iter().enumerate().find_map(|(i, arg)| {
                    arg.strip_prefix(&format!("{flag}="))
                        .or_else(|| (*arg == flag).then(|| argv.get(i + 1).copied()).flatten())
                }) {
                    let path = PathBuf::from(value);
                    let path = if path.is_absolute() {
                        path
                    } else {
                        std::fs::read_link(format!("/proc/{pid}/cwd"))
                            .map_err(|e| e.to_string())?
                            .join(path)
                    };
                    let value = path
                        .to_str()
                        .ok_or("Agent directory is not UTF-8")?
                        .to_string();
                    environment.insert(variable.into(), value.clone());
                    if flag == "--data-dir" {
                        environment.insert(
                            "CLINE_DB_DATA_DIR".into(),
                            path.join("db").to_string_lossy().into_owned(),
                        );
                    }
                    arguments.extend([flag.to_string(), value]);
                }
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        for key in target.agent.home_variables() {
            if let Ok(value) = std::env::var(key) {
                if !value.is_empty() {
                    environment.insert((*key).into(), value);
                }
            }
        }
    }
    let path = target
        .agent
        .history_home(|key| environment.get(key).map(PathBuf::from))
        .ok_or("Agent home directory is unavailable")?;
    Ok(SessionHome {
        path,
        environment,
        arguments,
    })
}

impl WindowController {
    async fn session_target(&self) -> Option<SessionTarget> {
        let pane = self.focused_pane.get()?;
        let (surface, root_pid) = {
            let registry = self.pane_registry.borrow();
            if registry.is_ssh_pane(pane) {
                return None;
            }
            (
                registry.active_surface(pane)?,
                registry.active_terminal(pane)?.pid.get()? as u32,
            )
        };
        let presence = self.store.located_agent_presence(surface).await?.presence;
        Some(SessionTarget {
            pane,
            surface,
            root_pid,
            agent: SessionAgent::from_name(&presence.name)?,
            session_id: presence.session_id,
        })
    }

    pub(super) async fn refresh_session_panel(&self, force: bool) {
        let panel = &self.sessions.panel;
        if !panel.root.is_visible() {
            return;
        }
        let target = self.session_target().await;
        if !force && *self.sessions.target.borrow() == target {
            return;
        }
        *self.sessions.target.borrow_mut() = target.clone();
        let generation = panel.clear("Loading sessions…");
        let Some(target) = target else {
            panel
                .status
                .set_text("Focus a local Claude, Codex, OpenCode, Antigravity, or Cline terminal tab to browse sessions.");
            return;
        };
        let controller = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let worker_target = target.clone();
            let result = gtk::gio::spawn_blocking(move || {
                let home = session_home(&worker_target)?;
                list_sessions(worker_target.agent, &home.path).map_err(|e| e.to_string())
            })
            .await;
            let panel = &controller.sessions.panel;
            if panel.generation.get() != generation
                || !panel.root.is_visible()
                || controller.session_target().await.as_ref() != Some(&target)
            {
                return;
            }
            match result {
                Ok(Ok(items)) => {
                    panel.status.set_text(&format!(
                        "{} · {} sessions · all projects",
                        target.agent.name(),
                        items.len()
                    ));
                    panel.set_rows(items, generation);
                }
                Ok(Err(error)) => panel
                    .status
                    .set_text(&format!("Cannot load sessions: {error}")),
                Err(_) => panel
                    .status
                    .set_text("Session history worker failed. Try Refresh."),
            }
        });
    }

    pub(super) async fn dispatch_session_panel(&self, action: SessionPanelAction) {
        let panel = &self.sessions.panel;
        match action {
            SessionPanelAction::Toggle => {
                if panel.root.is_visible() {
                    self.close_session_panel();
                } else {
                    panel.root.set_visible(true);
                    self.refresh_session_panel(true).await;
                }
            }
            SessionPanelAction::Close => self.close_session_panel(),
            SessionPanelAction::Refresh => self.refresh_session_panel(true).await,
            SessionPanelAction::Select {
                session,
                generation,
            } => {
                if panel.generation.get() != generation {
                    return;
                }
                *panel.selection.borrow_mut() = Some(session.id.clone());
                panel.resume.set_sensitive(false);
                panel.preview.buffer().set_text("Loading conversation…");
                let controller = self.clone();
                glib::MainContext::default().spawn_local(async move {
                    let worker_session = session.clone();
                    let result = gtk::gio::spawn_blocking(move || worker_session.preview()).await;
                    let panel = &controller.sessions.panel;
                    if panel.generation.get() != generation
                        || panel.selection.borrow().as_deref() != Some(&session.id)
                    {
                        return;
                    }
                    match result {
                        Ok(Ok(text)) => {
                            panel.preview.buffer().set_text(&format!(
                                "{}\n{}\n{}\n\n{}",
                                session.title,
                                session.cwd.display(),
                                session.id,
                                text
                            ));
                            panel.resume.set_sensitive(true);
                        }
                        Ok(Err(error)) => panel
                            .preview
                            .buffer()
                            .set_text(&format!("Cannot read session: {error}")),
                        Err(_) => panel
                            .preview
                            .buffer()
                            .set_text("Conversation worker failed. Try selecting again."),
                    }
                });
            }
            SessionPanelAction::Resume {
                session,
                generation,
            } => {
                self.resume_history_session(session, generation).await;
            }
        }
    }

    fn close_session_panel(&self) {
        self.sessions.panel.clear("");
        self.sessions.panel.root.set_visible(false);
        self.sessions.target.borrow_mut().take();
        if let Some(pane) = self.focused_pane.get() {
            self.focus_pane(pane);
        }
    }

    async fn resume_history_session(&self, session: HistorySession, generation: u64) {
        let panel = &self.sessions.panel;
        if panel.generation.get() != generation {
            return;
        }
        let Some(target) = self.session_target().await else {
            return;
        };
        if self.sessions.target.borrow().as_ref() != Some(&target) || target.agent != session.agent
        {
            return;
        }
        let Some(located) = self.store.located_agent_presence(target.surface).await else {
            return;
        };
        if !matches!(
            located.presence.public_status(),
            flowmux_core::AgentStatus::Idle | flowmux_core::AgentStatus::Done
        ) {
            panel.status.set_text(
                "Wait for the agent to finish and return to its input prompt before resuming.",
            );
            return;
        }
        if target.session_id.as_deref() == Some(&session.id) {
            panel
                .status
                .set_text("This session is already active in the focused tab.");
            return;
        }
        let valid_target = target.clone();
        let Ok(Ok(home)) = gtk::gio::spawn_blocking(move || session_home(&valid_target)).await
        else {
            panel
                .status
                .set_text("The agent is no longer available. Refresh sessions and try again.");
            return;
        };
        if self.session_target().await.as_ref() != Some(&target)
            || panel.generation.get() != generation
        {
            return;
        }
        // Start the native CLI in its own tab; never write into the existing agent.
        let line = match resume_shell_line(&session, &home) {
            Ok(line) if session.cwd.is_dir() => line,
            Ok(_) => {
                panel
                    .status
                    .set_text("This session's project directory no longer exists.");
                return;
            }
            Err(error) => {
                panel.status.set_text(&error);
                return;
            }
        };
        let Some((workspace, surface)) = self
            .store
            .add_terminal_surface_to_pane_with_shell(
                target.pane,
                Some(session.cwd),
                Some("/bin/sh".into()),
            )
            .await
        else {
            panel
                .status
                .set_text("Cannot create a terminal tab for this session.");
            return;
        };
        self.attach_or_rerender_surface(workspace, target.pane, surface)
            .await;
        let terminal = self.pane_registry.borrow().terminals.get(&surface).cloned();
        if let Some(terminal) = terminal {
            match terminal.write_input(format!("{line}\r").as_bytes()) {
                Ok(()) => {
                    panel.resume.set_sensitive(false);
                    panel.status.set_text(&format!(
                        "Opening {} in a new tab in the session's project directory.",
                        session.agent.name()
                    ));
                    self.focus_pane(target.pane);
                }
                Err(error) => panel
                    .status
                    .set_text(&format!("Cannot start {}: {error}", session.agent.name())),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_resume_uses_selected_project_and_config_without_shell_expansion() {
        for (agent, binary, variable, flag, directory) in [
            (
                SessionAgent::Claude,
                "claude",
                "CLAUDE_CONFIG_DIR",
                "--resume",
                ".claude",
            ),
            (
                SessionAgent::Codex,
                "codex",
                "CODEX_HOME",
                "resume",
                ".codex",
            ),
            (
                SessionAgent::OpenCode,
                "opencode",
                "XDG_DATA_HOME",
                "--session",
                ".local/share",
            ),
            (
                SessionAgent::Antigravity,
                "agy",
                "HOME",
                "--conversation",
                ".gemini",
            ),
            (
                SessionAgent::Cline,
                "cline",
                "CLINE_DATA_DIR",
                "--id",
                ".cline",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let cwd = root.path().join("other project ' 한글 $(exit 9)");
            std::fs::create_dir(&cwd).unwrap();
            let home = SessionHome {
                path: root.path().join("config ' $(exit 8)"),
                environment: [(
                    variable.into(),
                    root.path()
                        .join("config ' $(exit 8)")
                        .to_string_lossy()
                        .into_owned(),
                )]
                .into(),
                arguments: if agent == SessionAgent::Cline {
                    vec!["--data-dir".into(), "data ' $(exit 7)".into()]
                } else {
                    Vec::new()
                },
            };
            let mut session = HistorySession {
                agent,
                id: if agent == SessionAgent::OpenCode {
                    "ses_abc123"
                } else {
                    "12345678-1234-4234-8234-123456789abc"
                }
                .into(),
                title: String::new(),
                summary: String::new(),
                cwd: cwd.clone(),
                path: "/unused".into(),
                modified: std::time::SystemTime::now(),
            };
            let bin = root.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            let executable = bin.join(binary);
            std::fs::write(
                &executable,
                format!("#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"${variable}\" \"$@\"\n"),
            )
            .unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
            let output = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(resume_shell_line(&session, &home).unwrap())
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "{}\n{}\n{flag}\n{}\n{}",
                    cwd.display(),
                    home.path.display(),
                    session.id,
                    if agent == SessionAgent::Cline {
                        "--tui\n--data-dir\ndata ' $(exit 7)\n"
                    } else {
                        ""
                    }
                )
            );
            let default_home = SessionHome {
                path: root.path().join(directory),
                environment: [("HOME".into(), root.path().to_string_lossy().into_owned())].into(),
                arguments: Vec::new(),
            };
            std::fs::write(
                &executable,
                format!("#!/bin/sh\nprintf '%s\\n' \"$HOME\" \"${{{variable}-unset}}\"\n"),
            )
            .unwrap();
            let output = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(resume_shell_line(&session, &default_home).unwrap())
                .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                .env(variable, "/wrong/inherited/config")
                .output()
                .unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                if variable == "HOME" {
                    format!("{0}\n{0}\n", root.path().display())
                } else {
                    format!("{}\nunset\n", root.path().display())
                }
            );
            session.cwd = "/tmp/unsafe\ncommand".into();
            assert!(resume_shell_line(&session, &home).is_err());
            session.cwd = cwd;
            session.id = "bad\rcommand".into();
            assert!(resume_shell_line(&session, &home).is_err());
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    async fn panel_keeps_other_panels_and_blocks_busy_current_and_stale_targets() {
        let (controller, _, pane) = super::super::tests::build_single_workspace_controller(
            "com.flowmux.App.UiTest.SessionSafety",
        )
        .await;
        controller.window.present();
        glib::timeout_future(Duration::from_millis(50)).await;
        controller.focused_pane.set(Some(pane));
        assert!(!controller.sessions.panel.root.is_visible());
        controller.worktrees.panel.widget().set_visible(true);
        controller.file_browser.panel.widget().set_visible(true);
        controller
            .dispatch_session_panel(SessionPanelAction::Toggle)
            .await;
        assert!(controller
            .sessions
            .panel
            .status
            .text()
            .contains("Focus a local"));
        controller
            .dispatch_session_panel(SessionPanelAction::Close)
            .await;
        assert!(controller.worktrees.panel.widget().is_visible());
        assert!(controller.file_browser.panel.widget().is_visible());

        let surface = controller
            .pane_registry
            .borrow()
            .active_surface(pane)
            .unwrap();
        let session = HistorySession {
            agent: SessionAgent::Codex,
            id: "12345678-1234-4234-8234-123456789abc".into(),
            title: "test".into(),
            summary: String::new(),
            cwd: "/tmp".into(),
            path: "/unused".into(),
            modified: std::time::SystemTime::now(),
        };
        for (seq, status) in [
            flowmux_core::AgentStatus::Working,
            flowmux_core::AgentStatus::Blocked,
            flowmux_core::AgentStatus::Idle,
        ]
        .into_iter()
        .enumerate()
        {
            controller
                .store
                .report_agent_status(
                    surface,
                    flowmux_core::AgentStatusReport {
                        name: "codex".into(),
                        status: Some(status),
                        activity: None,
                        pid: None,
                        source: Some("flowmux:hook".into()),
                        seq: Some(seq as u64 + 1),
                        message: None,
                        custom_status: None,
                        session_id: Some(session.id.clone()),
                        session_name: None,
                        messaging_socket: None,
                    },
                )
                .await;
            let panel = &controller.sessions.panel;
            panel.root.set_visible(true);
            *controller.sessions.target.borrow_mut() = controller.session_target().await;
            let generation = panel.clear("before");
            controller
                .resume_history_session(session.clone(), generation.wrapping_sub(1))
                .await;
            assert_eq!(panel.status.text(), "before");
            controller
                .resume_history_session(session.clone(), generation)
                .await;
            if status == flowmux_core::AgentStatus::Idle {
                assert!(panel.status.text().contains("already active"));
            } else {
                assert!(panel.status.text().contains("Wait for the agent"));
            }
        }
        controller.focused_pane.set(None);
        let panel = &controller.sessions.panel;
        let generation = panel.clear("focus moved");
        controller.resume_history_session(session, generation).await;
        assert_eq!(panel.status.text(), "focus moved");
        controller.window.close();
    }
}
