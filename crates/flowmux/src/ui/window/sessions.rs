// SPDX-License-Identifier: GPL-3.0-or-later
//! Focus-bound session browsing and native resume commands.

use super::*;
use crate::ui::session_panel::{SessionPanel, SessionPanelAction};
use flowmux_state::session_history::{list_sessions, HistorySession, SessionAgent};
use vte::prelude::TerminalExt;

/// A conservative guard, not a terminal parser. Unknown layouts stay untouched.
/// Submission remains in the native TUI so its own resume checks still apply.
fn empty_resume_prompt(
    agent: SessionAgent,
    column: i64,
    text: &str,
    colored_placeholder: bool,
) -> bool {
    if column > 3 {
        return false;
    }
    let mut lines = text.lines();
    let Some(prompt) = lines.next() else {
        return false;
    };
    match agent {
        SessionAgent::Claude => {
            prompt.trim() == "❯"
                && lines.next().is_some_and(|line| {
                    let line = line.trim();
                    line.chars().count() >= 5 && line.chars().all(|c| c == '─')
                })
        }
        SessionAgent::Codex => {
            // Codex's idle particle animation can overlap its placeholder.
            let plain: String = prompt
                .chars()
                .filter(|c| !('\u{2800}'..='\u{28ff}').contains(c))
                .collect();
            let Some(input) = plain.trim_start().strip_prefix('›') else {
                return false;
            };
            let placeholder = colored_placeholder && input.trim() == "Ask Codex to do anything";
            let empty = prompt.trim() == "›";
            (placeholder || empty)
                && lines.next().is_some_and(|line| {
                    line.chars()
                        .all(|c| c.is_whitespace() || ('\u{2800}'..='\u{28ff}').contains(&c))
                })
                && lines.next().is_some_and(|line| line.contains('·'))
        }
    }
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
fn session_home(target: &SessionTarget) -> Result<PathBuf, String> {
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
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStringExt;
        let environment =
            std::fs::read(format!("/proc/{pid}/environ")).map_err(|e| e.to_string())?;
        let variable = if target.agent == SessionAgent::Codex {
            b"CODEX_HOME=".as_slice()
        } else {
            b"CLAUDE_CONFIG_DIR=".as_slice()
        };
        let value = |key: &[u8]| {
            environment
                .split(|b| *b == 0)
                .find_map(|entry| entry.strip_prefix(key))
                .filter(|v| !v.is_empty())
        };
        if let Some(home) = value(variable) {
            return Ok(PathBuf::from(std::ffi::OsString::from_vec(home.to_vec())));
        }
        if let Some(home) = value(b"HOME=") {
            let directory = if target.agent == SessionAgent::Codex {
                ".codex"
            } else {
                ".claude"
            };
            return Ok(PathBuf::from(std::ffi::OsString::from_vec(home.to_vec())).join(directory));
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
    target
        .agent
        .default_home()
        .ok_or_else(|| "Agent home directory is unavailable".into())
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
                .set_text("Focus a local Claude or Codex terminal tab to browse sessions.");
            return;
        };
        let controller = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let worker_target = target.clone();
            let result = gtk::gio::spawn_blocking(move || {
                let home = session_home(&worker_target)?;
                list_sessions(worker_target.agent, &home).map_err(|e| e.to_string())
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
                            let modified: chrono::DateTime<chrono::Local> = session.modified.into();
                            panel.preview.buffer().set_text(&format!(
                                "{}\n{}\n{}\nUpdated {}\n\n{}\n\n{}",
                                session.title,
                                session.id,
                                session.cwd.display(),
                                modified.format("%Y-%m-%d %H:%M"),
                                session.summary,
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
        let command = match session.resume_command() {
            Ok(command) => command,
            Err(error) => {
                panel.status.set_text(&error.to_string());
                return;
            }
        };
        // Stage a native slash command; the user submits it in the agent TUI.
        // Never synthesize Enter: terminal input can be a draft or a native dialog.
        let valid_target = target.clone();
        let home_check = gtk::gio::spawn_blocking(move || session_home(&valid_target)).await;
        if !matches!(home_check, Ok(Ok(_)))
            || self.session_target().await.as_ref() != Some(&target)
            || panel.generation.get() != generation
        {
            return;
        }
        let terminal = self
            .pane_registry
            .borrow()
            .active_terminal(target.pane)
            .cloned();
        if let Some(terminal) = terminal {
            let (column, row) = terminal.widget.cursor_position();
            let prompt = terminal
                .widget
                .text_range_format(vte::Format::Text, row, 0, row + 3, 0)
                .0;
            let colored_placeholder = target.agent == SessionAgent::Codex
                && terminal
                    .widget
                    .text_range_format(vte::Format::Html, row, 0, row + 1, 0)
                    .0
                    .is_some_and(|html| {
                        crate::ui::terminal_scrollback::vte_html_has_colored_text(
                            &html,
                            "Ask Codex to do anything",
                        )
                    });
            if !prompt.as_deref().is_some_and(|text| {
                empty_resume_prompt(target.agent, column, text, colored_placeholder)
            }) {
                panel.status.set_text("Clear the agent's input and close any open picker, then try Resume again. Existing input was kept.");
                return;
            }
            let _ = terminal.write_input(format!("\x1b[200~{command}\x1b[201~").as_bytes());
            panel.resume.set_sensitive(false);
            panel.status.set_text("Resume command inserted. Check the input, then press Enter in the agent to switch sessions.");
            self.focus_pane(target.pane);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_input_guard_preserves_drafts_multiline_prompts_and_native_dialogs() {
        assert!(command_dismisses_workspace_overview(
            &GtkCommand::SessionPanel(SessionPanelAction::Toggle)
        ));
        assert!(!command_dismisses_workspace_overview(
            &GtkCommand::SessionPanel(SessionPanelAction::Refresh)
        ));
        // A draft identical to the placeholder is still a draft, even at Home.
        let draft = "› Ask Codex to do anything\n\nmodel · /repo";
        assert!(!empty_resume_prompt(SessionAgent::Codex, 2, draft, false));
        let placeholder = crate::ui::terminal_scrollback::vte_html_has_colored_text(
            "<pre><b>›</b> <font color=\"#AAAAAA\">Ask Codex to do anything</font></pre>",
            "Ask Codex to do anything",
        );
        assert!(empty_resume_prompt(
            SessionAgent::Codex,
            2,
            draft,
            placeholder
        ));
        assert!(!crate::ui::terminal_scrollback::vte_html_has_colored_text(
            "<pre><b>›</b> Ask Codex to do anything</pre>",
            "Ask Codex to do anything"
        ));
        assert!(empty_resume_prompt(
            SessionAgent::Claude,
            2,
            "❯\u{a0}\n─────────\nshortcuts",
            true
        ));
        assert!(empty_resume_prompt(
            SessionAgent::Codex,
            2,
            "› Ask Codex to do anything\n\nmodel · /repo",
            true
        ));
        assert!(empty_resume_prompt(
            SessionAgent::Codex,
            2,
            "›⠁Ask Codex to do anything ⠄\n ⠠\nmodel · /repo",
            true
        ));
        for text in [
            "❯ draft\n─────────",
            "❯\nsecond draft line\n─────────",
            "❯ /resume abc\n─────────",
            "❯ 1. Allow\n─────────",
            "$ \n─────────",
        ] {
            assert!(
                !empty_resume_prompt(SessionAgent::Claude, 2, text, true),
                "{text}"
            );
        }
        for text in [
            "› draft\n\nmodel · /repo",
            "›\nsecond draft line\nmodel · /repo",
            "› ⠁\n\nmodel · /repo",
            "› Ask Codex to do anything\ntext\nmodel · /repo",
            "› Allow once\n\nmodel · /repo",
            "$\n\nmodel · /repo",
        ] {
            assert!(
                !empty_resume_prompt(SessionAgent::Codex, 2, text, true),
                "{text}"
            );
        }
        assert!(!empty_resume_prompt(
            SessionAgent::Codex,
            28,
            "› Ask Codex to do anything\n\nmodel · /repo",
            true
        ));
        assert!(!empty_resume_prompt(
            SessionAgent::Claude,
            10,
            "❯\n─────────",
            true
        ));
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
