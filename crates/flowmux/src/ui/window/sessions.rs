// SPDX-License-Identifier: GPL-3.0-or-later
//! Selected-agent or focus-bound session browsing and native resume commands.

use super::*;
use crate::ui::session_panel::{SessionEntry, SessionPanel, SessionPanelAction};
use flowmux_state::chat_history::{self, Record, Summary as ChatSummary};
use flowmux_state::session_history::{list_sessions, HistorySession, SessionAgent};

fn chat_history_preview(chat: &ChatSummary) -> Result<String, String> {
    let history = chat_history::Store::existing()
        .and_then(|store| store.ok_or(chat_history::Error::InvalidChat)?.load(chat.id))
        .map_err(|error| error.to_string())?;
    const LIMIT: usize = 64 * 1024;
    let mut preview = format!("{} · Chat\n{}\n", chat.agent, chat.cwd.display());
    let mut blocks = Vec::new();
    let mut answers = std::collections::HashMap::new();
    for record in history.records {
        match record {
            Record::Begin { prompt } => {
                answers.clear();
                blocks.push(format!("You\n{prompt}"));
            }
            Record::Text {
                id,
                kind: chat_history::TextKind::Answer,
                text,
                append,
            } if !text.is_empty() => {
                let index = *answers.entry(id).or_insert_with(|| {
                    blocks.push(String::new());
                    blocks.len() - 1
                });
                let answer = &mut blocks[index];
                if !append {
                    answer.clear();
                }
                let remaining = (LIMIT + 1).saturating_sub(answer.len());
                answer.push_str(&text[..text.floor_char_boundary(remaining)]);
            }
            _ => {}
        }
    }
    for block in blocks {
        preview.push('\n');
        let length = block.floor_char_boundary(LIMIT.saturating_sub(preview.len()));
        preview.push_str(&block[..length]);
        preview.push('\n');
        if length < block.len() {
            preview.push('…');
            break;
        }
    }
    Ok(preview)
}

#[cfg(target_os = "macos")]
mod macos;

#[derive(Clone)]
pub(super) struct SessionHome {
    pub(super) path: PathBuf,
    pub(super) environment: std::collections::BTreeMap<String, String>,
    pub(super) arguments: Vec<String>,
}

#[cfg(test)]
pub(super) fn resume_shell_line(
    session: &HistorySession,
    home: &SessionHome,
    yolo: bool,
) -> Result<String, String> {
    session_shell_line(session, home, yolo, false)
}

#[cfg(test)]
pub(super) fn session_shell_line(
    session: &HistorySession,
    home: &SessionHome,
    yolo: bool,
    fork: bool,
) -> Result<String, String> {
    native_session_line(session, home, yolo, fork, false)
}

pub(super) fn native_session_line(
    session: &HistorySession,
    home: &SessionHome,
    yolo: bool,
    fork: bool,
    managed: bool,
) -> Result<String, String> {
    provider_shell_line(
        session.agent,
        &session.cwd,
        home,
        yolo,
        managed,
        &if fork {
            session.fork_command()
        } else {
            session.resume_command()
        }
        .map_err(|e| e.to_string())?,
    )
}

pub(super) fn provider_shell_line(
    agent: SessionAgent,
    directory: &std::path::Path,
    home: &SessionHome,
    yolo: bool,
    managed: bool,
    command: &str,
) -> Result<String, String> {
    let cwd = directory.to_str().ok_or("Project directory is not UTF-8")?;
    if !directory.is_absolute()
        || cwd
            .chars()
            .chain(home.environment.values().flat_map(|v| v.chars()))
            .chain(home.arguments.iter().flat_map(|v| v.chars()))
            .any(char::is_control)
    {
        return Err("Cannot safely open this session's directory in a terminal".into());
    }
    let unset = agent
        .home_variables()
        .iter()
        .filter(|key| **key != "HOME")
        .map(|key| format!("-u {key}"))
        .collect::<Vec<_>>()
        .join(" ");
    let environment = home
        .environment
        .iter()
        .map(|(key, value)| {
            if key == "PATH" {
                // Retain the provider's PATH, with this pane's wrapper first.
                format!(
                    "PATH=\"${{FLOWMUX_AGENT_SHIM_DIR:+$FLOWMUX_AGENT_SHIM_DIR:}}\"{}",
                    shell_quote(value)
                )
            } else {
                shell_quote(&format!("{key}={value}"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let arguments = home
        .arguments
        .iter()
        .map(|arg| shell_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let permissions = if yolo {
        match agent {
            SessionAgent::Claude => {
                "--allow-dangerously-skip-permissions --permission-mode bypassPermissions"
            }
            SessionAgent::Codex => "--dangerously-bypass-approvals-and-sandbox",
            SessionAgent::OpenCode => "--auto",
            SessionAgent::Antigravity => "--dangerously-skip-permissions",
            SessionAgent::Cline | SessionAgent::Gemini => "--yolo",
        }
    } else {
        ""
    };
    Ok(format!(
        "cd {} && {}env {unset} {environment} {} {arguments} {permissions}",
        shell_quote(cwd),
        if managed { "exec " } else { "" },
        command
    ))
}

/// Only a launch the Host definitely never ran may continue in an ordinary tab;
/// an unknown result could run the same native session twice.
pub(super) fn tab_fallback_reason(error: &anyhow::Error) -> Option<String> {
    error
        .downcast_ref::<flowmux_cli::agent_host::LaunchNotStarted>()
        .map(|_| format!("{error:#}"))
}

pub(super) fn prepare_session_launch(
    session: &HistorySession,
    home: &SessionHome,
    yolo: bool,
    fork: bool,
    shell: String,
) -> Result<flowmux_ipc::agent_host::LaunchAgent, String> {
    let line = native_session_line(session, home, yolo, fork, true)?;
    prepare_managed_launch(
        session.agent,
        &session.cwd,
        line,
        format!("{} · {}", session.agent.name(), session.id),
        shell,
    )
}

pub(super) fn prepare_managed_launch(
    agent: SessionAgent,
    cwd: &std::path::Path,
    line: String,
    label: String,
    shell: String,
) -> Result<flowmux_ipc::agent_host::LaunchAgent, String> {
    let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os()
        // Like a regular PTY, advertise color independently of the launching application.
        .filter(|(key, _)| key != "NO_COLOR")
        .map(|(key, value)| {
            Ok((
                key.into_string()
                    .map_err(|_| "Launch environment key is not UTF-8")?,
                value
                    .into_string()
                    .map_err(|_| "Launch environment value is not UTF-8")?,
            ))
        })
        .collect::<Result<_, String>>()?;
    environment.extend([
        ("TERM".into(), "xterm-256color".into()),
        ("COLORTERM".into(), "truecolor".into()),
        ("CLAUDE_CODE_NATIVE_CURSOR".into(), "1".into()),
    ]);
    let prefix = if let Some(shim) = flowmux_config::paths::agent_shim_dir().filter(|p| p.is_dir())
    {
        environment.insert(
            "FLOWMUX_AGENT_SHIM_DIR".into(),
            shim.to_string_lossy().into(),
        );
        "export PATH=\"$FLOWMUX_AGENT_SHIM_DIR:$PATH\"; "
    } else {
        ""
    };
    // Initialize the user's shell, then replace it so hooks identify the Host root.
    let mut launch = flowmux_cli::agent_host::prepare_launch(
        vec![shell, "-lic".into(), format!("{prefix}{line}")],
        cwd,
        environment.into_iter().collect(),
    )
    .map_err(|e| e.to_string())?;
    launch.provider = agent.name().to_ascii_lowercase();
    launch.label = label;
    // The same resolver as the window's theme, so startup queries match the tab.
    launch.colors = Some(crate::theme::ResolvedTheme::load().terminal_colors());
    Ok(launch)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SessionTarget {
    pub(super) agent: SessionAgent,
    pub(super) focused: Option<FocusedSession>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FocusedSession {
    pub(super) pane: PaneId,
    pub(super) surface: SurfaceId,
    pub(super) root_pid: u32,
    pub(super) process_start: Option<u64>,
    pub(super) session_id: Option<String>,
}

#[derive(Clone)]
pub(super) struct SessionPanelState {
    pub panel: SessionPanel,
    target: Rc<RefCell<Option<SessionTarget>>>,
    submitted: Rc<RefCell<std::collections::HashSet<(u64, String)>>>,
}

impl SessionPanelState {
    pub fn new(panel: SessionPanel) -> Self {
        Self {
            panel,
            target: Rc::new(RefCell::new(None)),
            submitted: Default::default(),
        }
    }
}

/// Resolve the actual agent's home, including overrides set inside the shell.
pub(super) fn session_home(target: &SessionTarget) -> Result<SessionHome, String> {
    let Some(focused) = &target.focused else {
        let environment: std::collections::BTreeMap<_, _> = target
            .agent
            .home_variables()
            .iter()
            .copied()
            .filter_map(|key| {
                std::env::var(key)
                    .ok()
                    .map(|value| (key.to_string(), value))
            })
            .collect();
        let path = target
            .agent
            .history_home(|key| environment.get(key).map(PathBuf::from))
            .ok_or("Agent home directory is unavailable")?;
        return Ok(SessionHome {
            path,
            environment,
            arguments: Vec::new(),
        });
    };
    let (_, _, home) = process_session_home(target.agent, focused.root_pid, focused.process_start)?;
    Ok(home)
}

pub(super) fn process_session_home(
    provider: SessionAgent,
    root_pid: u32,
    process_start: Option<u64>,
) -> Result<(u32, u64, SessionHome), String> {
    let valid_process = || {
        process_start
            .is_none_or(|start| flowmux_procmon::process_start_time(root_pid) == Some(start))
    };
    if !valid_process() {
        return Err("The agent process was replaced".into());
    }
    let is_agent = |pid: &u32| {
        flowmux_procmon::agent_name_for_pid(*pid).and_then(SessionAgent::from_name)
            == Some(provider)
    };
    // A managed root is the provider, or a launcher with the provider beneath it.
    let pid = if process_start.is_some() && is_agent(&root_pid) {
        root_pid
    } else {
        flowmux_procmon::descendants(root_pid)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(is_agent)
            .min()
            .ok_or("The agent is no longer running in this tab")?
    };
    let start = flowmux_procmon::process_start_time(pid).ok_or("The agent is no longer running")?;
    let flowmux_state::session_history::SessionLaunchContext {
        environment,
        arguments,
    } = flowmux_state::session_history::SessionLaunchContext::read(provider, pid)?;
    if !valid_process() || flowmux_procmon::process_start_time(pid) != Some(start) {
        return Err("The agent process was replaced".into());
    }
    let path = provider
        .history_home(|key| environment.get(key).map(PathBuf::from))
        .ok_or("Agent home directory is unavailable")?;
    Ok((
        pid,
        start,
        SessionHome {
            path,
            environment,
            arguments,
        },
    ))
}

impl WindowController {
    pub(super) fn session_resume_pane(&self) -> Option<(PaneId, SurfaceId)> {
        let pane = self.focused_pane.get()?;
        let registry = self.pane_registry.borrow();
        if registry.is_ssh_pane(pane) {
            return None;
        }
        registry.active_terminal(pane)?;
        Some((pane, registry.active_surface(pane)?))
    }

    async fn session_target(&self) -> Option<SessionTarget> {
        if let Some(agent) = self.sessions.panel.selected_agent.get() {
            return Some(SessionTarget {
                agent,
                focused: None,
            });
        }
        if let Some(agent) = (|| {
            let registry = self.pane_registry.borrow();
            let surface = registry.active_surface(self.focused_pane.get()?)?;
            SessionAgent::from_name(registry.chats.get(&surface)?.agent_name()?)
        })() {
            return Some(SessionTarget {
                agent,
                focused: None,
            });
        }
        let (pane, surface) = self.session_resume_pane()?;
        self.terminal_session_target(pane, surface).await
    }

    pub(super) fn managed_session_agent(
        &self,
        binding: flowmux_core::AgentViewBinding,
    ) -> Option<flowmux_ipc::agent_host::AgentInstance> {
        let snapshot = self.bridge.agent_registry.borrow();
        let agent = snapshot.agents.iter().find(|agent| {
            snapshot.available
                && agent.instance == binding.instance
                && agent.epoch == binding.epoch
                && agent.state == flowmux_ipc::agent_host::ProcessState::Running
        })?;
        let (pid, start) = agent.pid.zip(agent.process_start)?;
        (flowmux_procmon::process_start_time(pid) == Some(start)).then(|| agent.clone())
    }

    pub(super) async fn terminal_session_presence(
        &self,
        pane: PaneId,
        surface: SurfaceId,
    ) -> Option<flowmux_core::AgentPresence> {
        if let Some(binding) = self.store.surface_agent_binding(pane, surface).await {
            self.managed_session_agent(binding)?.presence
        } else {
            self.store
                .located_agent_presence(surface)
                .await
                .map(|p| p.presence)
        }
    }

    pub(super) async fn running_session(
        &self,
        provider: SessionAgent,
        id: &str,
        home: &std::path::Path,
    ) -> Result<Option<flowmux_ipc::agent_host::AgentInstance>, String> {
        let handle = self
            .tokio_handle
            .as_ref()
            .ok_or("Agent Host runtime unavailable")?;
        let socket = flowmux_config::paths::agent_host_socket();
        // Resume must read the Host now; its periodically refreshed GUI snapshot can lag a launch.
        let agents = handle
            .spawn(async move {
                use flowmux_ipc::agent_host::{ClientRole, HostClient, HostRequest, HostResponse};
                tokio::time::timeout(Duration::from_secs(2), async move {
                    let client = match HostClient::connect(&socket, ClientRole::Observer).await {
                        Ok(client) => client,
                        Err(error)
                            if error.chain().any(|cause| {
                                cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
                                    matches!(
                                        io.kind(),
                                        std::io::ErrorKind::NotFound
                                            | std::io::ErrorKind::ConnectionRefused
                                    )
                                })
                            }) =>
                        {
                            return Ok(Vec::new())
                        }
                        Err(error) => return Err(error),
                    };
                    let HostResponse::List { agents, .. } = client.call(HostRequest::List).await?
                    else {
                        anyhow::bail!("Unexpected Agent Host list response");
                    };
                    Ok(agents)
                })
                .await
                .map_err(|_| anyhow::anyhow!("Agent Host session lookup timed out"))?
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let check_cached = agents.is_empty();
        let candidates = if check_cached {
            self.bridge.agent_registry.borrow().agents.clone()
        } else {
            agents
        };
        let id = id.to_owned();
        let home = home.to_path_buf();
        let found = gtk::gio::spawn_blocking(move || -> Result<_, String> {
            let home = home.canonicalize().map_err(|e| e.to_string())?;
            let mut found = None;
            for agent in candidates {
                if agent.state != flowmux_ipc::agent_host::ProcessState::Running
                    || SessionAgent::from_name(&agent.provider) != Some(provider)
                {
                    continue;
                }
                let reported = agent.presence.as_ref().and_then(|p| p.session_id.as_deref());
                // A detached Codex fork can have a writable rollout before its first hook.
                let probe = reported.is_none() && provider == SessionAgent::Codex && !agent.shell;
                if reported != Some(id.as_str()) && !probe {
                    continue;
                }
                let Some((root_pid, process_start)) = agent.pid.zip(agent.process_start) else {
                    if check_cached || probe {
                        continue;
                    }
                    return Err("Running session identity is not yet available".into());
                };
                let alive = || -> Result<bool, String> {
                    match flowmux_procmon::process_start_time(root_pid) {
                        Some(start) => Ok(start == process_start),
                        None if !flowmux_procmon::pid_alive(root_pid) => Ok(false),
                        None => Err("Running session process identity is unavailable".into()),
                    }
                };
                if !alive()? {
                    continue;
                }
                let matches = (|| -> Result<bool, String> {
                    if probe
                        && flowmux_state::session_flow::codex_process_session(root_pid, &home)
                            .map_err(|e| e.to_string())?
                            .as_deref()
                            != Some(id.as_str())
                    {
                        return Ok(false);
                    }
                    let (_, _, current_home) =
                        process_session_home(provider, root_pid, Some(process_start))?;
                    let current = current_home
                        .path
                        .canonicalize()
                        .map_err(|e| e.to_string())?;
                    Ok(current == home)
                })();
                if !alive()? || !matches? {
                    continue;
                }
                if check_cached {
                    return Err("Agent Host unavailable; the previously observed session process is still alive".into());
                }
                if found.replace(agent).is_some() {
                    return Err(
                        "This session has multiple running executions; select one in Agents".into(),
                    );
                }
            }
            Ok(found)
        })
        .await
        .map_err(|_| "Running session identity reader failed")??;
        if found.is_none() {
            let other_hosts = handle
                .spawn(async {
                    crate::agent_host::other_live_hosts(&flowmux_config::paths::agent_host_socket())
                        .await
                })
                .await
                .map_err(|e| e.to_string())?;
            if !other_hosts.is_empty() {
                return Err(
                    "Another Flowmux Agent Host is running and may already own this session. Resume it from the Flowmux build that started that Host."
                        .into(),
                );
            }
        }
        Ok(found)
    }

    pub(super) async fn terminal_session_target(
        &self,
        pane: PaneId,
        surface: SurfaceId,
    ) -> Option<SessionTarget> {
        if let Some(binding) = self.store.surface_agent_binding(pane, surface).await {
            let agent = self.managed_session_agent(binding)?;
            let root_pid = agent.pid?;
            let process_start = agent.process_start?;
            return Some(SessionTarget {
                agent: SessionAgent::from_name(&agent.provider)?,
                focused: Some(FocusedSession {
                    pane,
                    surface,
                    root_pid,
                    process_start: Some(process_start),
                    session_id: agent.presence.as_ref().and_then(|p| p.session_id.clone()),
                }),
            });
        }
        let root_pid = self
            .pane_registry
            .borrow()
            .terminals
            .get(&surface)?
            .process_root()?;
        let presence = self.store.located_agent_presence(surface).await?.presence;
        Some(SessionTarget {
            agent: SessionAgent::from_name(&presence.name)?,
            focused: Some(FocusedSession {
                pane,
                surface,
                root_pid,
                process_start: None,
                session_id: presence.session_id,
            }),
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
        let choose_agent = "Select an agent above or focus a local agent tab to browse sessions.";
        let generation = panel.clear(if target.is_some() {
            "Loading sessions…"
        } else {
            choose_agent
        });
        let controller = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let worker_target = target.clone();
            let result = gtk::gio::spawn_blocking(move || {
                let agent = worker_target.as_ref().map(|target| target.agent.name());
                let chats = chat_history::Store::existing()
                    .and_then(|store| store.map_or(Ok(Vec::new()), |store| store.list(agent)))
                    .map_err(|e| e.to_string());
                let native = worker_target.as_ref().map_or(Ok(Vec::new()), |target| {
                    let home = session_home(target)?;
                    list_sessions(target.agent, &home.path).map_err(|e| e.to_string())
                });
                let (chats, history_error) = match chats {
                    Ok(chats) => (chats, None),
                    Err(error) => (Vec::new(), Some(error)),
                };
                let (native, native_error) = match native {
                    Ok(native) => (native, None),
                    Err(error) => (Vec::new(), Some(error)),
                };
                let chat_native_ids: std::collections::HashSet<_> = chats
                    .iter()
                    .filter_map(|chat| Some((chat.agent.as_str(), chat.native_id.as_deref()?)))
                    .collect();
                let mut rows: Vec<_> = native
                    .into_iter()
                    .filter(|item| {
                        !chat_native_ids.contains(&(item.agent.name(), item.id.as_str()))
                    })
                    .map(SessionEntry::Native)
                    .collect();
                rows.extend(chats.into_iter().map(SessionEntry::Chat));
                rows.sort_by_key(|item| std::cmp::Reverse(item.modified()));
                (rows, history_error.or(native_error))
            })
            .await;
            let panel = &controller.sessions.panel;
            if panel.generation.get() != generation
                || !panel.root.is_visible()
                || controller.session_target().await != target
            {
                return;
            }
            match result {
                Ok((items, error)) => {
                    if let Some(error) = error {
                        panel.status.set_text(&format!(
                            "{} sessions · Could not load some history: {error}",
                            items.len()
                        ));
                    } else if let Some(target) = target {
                        panel.status.set_text(&format!(
                            "{} · {} sessions · all projects",
                            target.agent.name(),
                            items.len()
                        ));
                    } else if items.is_empty() {
                        panel.status.set_text(choose_agent);
                    } else {
                        panel
                            .status
                            .set_text(&format!("{} saved chats · all projects", items.len()));
                    }
                    panel.set_rows(items, generation);
                }
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
                    if let Some(split) = panel.root.parent().and_downcast::<gtk::Paned>() {
                        if !split.is_position_set() {
                            split.set_position((split.width() - self.window.width() / 4).max(0));
                        }
                    }
                    panel.root.set_visible(true);
                    self.refresh_session_panel(true).await;
                }
            }
            SessionPanelAction::Close => self.close_session_panel(),
            SessionPanelAction::Refresh => self.refresh_session_panel(true).await,
            SessionPanelAction::ToggleAgent(agent) => {
                panel.toggle_agent(agent);
                self.refresh_session_panel(true).await;
            }
            SessionPanelAction::Select {
                session,
                generation,
            } => {
                if panel.generation.get() != generation {
                    return;
                }
                *panel.selection.borrow_mut() = Some(session.id.clone());
                panel.show_chat_action(false);
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
                yolo,
            } => {
                self.resume_history_session(session, generation, yolo).await;
            }
            SessionPanelAction::SelectChat { chat, generation } => {
                self.select_chat_history(chat, generation)
            }
            SessionPanelAction::OpenChat { chat, generation } => {
                self.open_chat_history(chat, generation).await;
            }
        }
    }

    fn select_chat_history(&self, chat: ChatSummary, generation: u64) {
        let panel = &self.sessions.panel;
        if panel.generation.get() != generation {
            return;
        }
        let key = format!("chat:{}", chat.id);
        *panel.selection.borrow_mut() = Some(key.clone());
        panel.show_chat_action(true);
        panel.resume.set_sensitive(true);
        panel.preview.buffer().set_text("Loading conversation…");
        let panel = panel.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || chat_history_preview(&chat)).await;
            if panel.generation.get() != generation
                || panel.selection.borrow().as_ref() != Some(&key)
            {
                return;
            }
            match result {
                Ok(Ok(text)) => panel.preview.buffer().set_text(&text),
                Ok(Err(error)) => panel
                    .preview
                    .buffer()
                    .set_text(&format!("Cannot read chat: {error}")),
                Err(_) => panel
                    .preview
                    .buffer()
                    .set_text("Conversation worker failed. Try selecting again."),
            }
        });
    }

    async fn open_chat_history(&self, chat: ChatSummary, generation: u64) {
        let panel = &self.sessions.panel;
        if panel.generation.get() != generation {
            return;
        }
        let existing = {
            let registry = self.pane_registry.borrow();
            registry.chats.iter().find_map(|(&surface, pane_chat)| {
                if pane_chat.history_id() != chat.id {
                    return None;
                }
                let pane = registry.pane_for_surface(surface)?;
                Some((registry.workspace_of_pane(pane)?, pane, surface))
            })
        };
        if let Some((workspace, pane, surface)) = existing {
            self.activate_workspace_at(workspace, Some((pane, Some(surface))))
                .await;
            if let Some(chat) = self.pane_registry.borrow().chats.get(&surface) {
                chat.resume();
            }
            return;
        }
        let Some(pane) = self.focused_pane.get().filter(|&pane| {
            !self.pane_registry.borrow().is_ssh_pane(pane) && self.pane_in_visible_workspace(pane)
        }) else {
            panel
                .status
                .set_text("Focus a local pane to open this chat.");
            return;
        };
        panel.resume.set_sensitive(false);
        let opened = self
            .add_chat_surface(pane, chat.agent, chat.cwd, Some(chat.id))
            .await;
        if panel.generation.get() == generation {
            if let Some(chat) = opened {
                chat.resume();
                panel.status.set_text("Opened saved chat.");
            }
            if panel.selection.borrow().as_deref() == Some(&format!("chat:{}", chat.id)) {
                panel.resume.set_sensitive(true);
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

    async fn resume_history_session(&self, session: HistorySession, generation: u64, yolo: bool) {
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
        let Some((pane, surface)) = self.session_resume_pane() else {
            panel
                .status
                .set_text("Focus a local terminal tab to resume this session.");
            return;
        };
        if self
            .terminal_session_target(pane, surface)
            .await
            .is_some_and(|target| {
                target.agent == session.agent
                    && target
                        .focused
                        .as_ref()
                        .and_then(|f| f.session_id.as_deref())
                        == Some(&session.id)
            })
        {
            panel
                .status
                .set_text("This session is already active in the focused tab.");
            return;
        }
        if !session.cwd.is_dir() {
            let error = format!(
                "The project directory does not exist or cannot be opened:\n\n{}",
                session.cwd.display()
            );
            panel.status.set_text(&error);
            let window = self.window.clone();
            glib::MainContext::default().spawn_local(async move {
                show_error_dialog(&window, "Cannot resume session", &error).await;
            });
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
            || self.session_resume_pane() != Some((pane, surface))
        {
            return;
        }
        match self
            .running_session(session.agent, &session.id, &home.path)
            .await
        {
            Ok(Some(agent)) => {
                let binding = flowmux_core::AgentViewBinding {
                    instance: agent.instance,
                    epoch: agent.epoch,
                };
                let destination = if let Some((workspace, pane, surface)) =
                    self.store.agent_view(binding).await
                {
                    self.activate_workspace_at(workspace, Some((pane, Some(surface))))
                        .await;
                    pane
                } else {
                    pane
                };
                match self.open_agent_view(destination, agent).await {
                    Ok(_) => panel.status.set_text("Opened the same running session."),
                    Err(error) => panel.status.set_text(&error),
                }
                return;
            }
            Ok(None) => {}
            Err(error) => {
                panel.status.set_text(&error);
                return;
            }
        }
        if self.session_target().await.as_ref() != Some(&target)
            || panel.generation.get() != generation
            || self.session_resume_pane() != Some((pane, surface))
        {
            return;
        }
        let Some(handle) = self.tokio_handle.as_ref() else {
            panel.status.set_text("Agent Host runtime unavailable.");
            return;
        };
        let shell = self
            .options
            .borrow()
            .default_shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/bash".into());
        let launch = match prepare_session_launch(&session, &home, yolo, false, shell) {
            Ok(launch) => launch,
            Err(error) => {
                panel.status.set_text(&error);
                return;
            }
        };
        let key = (generation, session.id.clone());
        self.sessions
            .submitted
            .borrow_mut()
            .retain(|(g, _)| *g == generation);
        if !self.sessions.submitted.borrow_mut().insert(key) {
            panel.status.set_text("This resume request was already submitted. Check Agents before refreshing and retrying.");
            return;
        }
        panel.resume.set_sensitive(false);
        panel.status.set_text("Starting session in Agent Host…");
        let socket = flowmux_config::paths::agent_host_socket();
        let result = handle
            .spawn(async move { flowmux_cli::agent_host::launch(&socket, launch).await })
            .await;
        match result {
            Ok(Ok(agent)) => {
                let instance = agent.instance;
                // Losing the destination after launch must preserve the background process.
                let opened = self.open_agent_view(pane, agent).await;
                if panel.generation.get() == generation {
                    match opened {
                        Ok(_) => panel.status.set_text("Session opened. Closing its tab keeps the agent running in Background."),
                        Err(error) => panel.status.set_text(&format!("Agent {instance} is running in Background; cannot open tab: {error}")),
                    }
                }
                self.refresh_agent_displays().await;
            }
            Ok(Err(error)) => match tab_fallback_reason(&error) {
                Some(reason) => {
                    self.resume_in_tab(&session, &home, yolo, pane, &reason, generation)
                        .await
                }
                None => panel
                    .status
                    .set_text(&format!("Cannot start session: {error:#}")),
            },
            Err(error) => panel.status.set_text(&format!(
                "Launch result unknown: {error}. Check Agents before retrying."
            )),
        }
    }

    /// Start the native CLI in its own ordinary tab; never write into the existing agent.
    async fn resume_in_tab(
        &self,
        session: &HistorySession,
        home: &SessionHome,
        yolo: bool,
        pane: PaneId,
        reason: &str,
        generation: u64,
    ) {
        let panel = &self.sessions.panel;
        // Like the managed path, a stale panel keeps its newer status.
        let status = |text: &str| {
            if panel.generation.get() == generation {
                panel.status.set_text(text);
            }
        };
        let line = match native_session_line(session, home, yolo, false, false) {
            Ok(line) => line,
            Err(error) => {
                status(&error);
                return;
            }
        };
        let Some((workspace, surface)) = self
            .store
            // Keep the normal configured shell and prompt after the agent exits.
            .add_terminal_surface_to_pane(pane, Some(session.cwd.clone()))
            .await
        else {
            status("Cannot create a terminal tab for this session.");
            return;
        };
        #[cfg(target_os = "macos")]
        self.pane_registry
            .borrow_mut()
            .pending_terminal_commands
            .insert(surface, line.clone());
        if let Err(error) = self
            .attach_or_rerender_surface(workspace, pane, surface, None)
            .await
        {
            #[cfg(target_os = "macos")]
            self.pane_registry
                .borrow_mut()
                .pending_terminal_commands
                .remove(&surface);
            self.store.close_surface(pane, surface).await;
            self.clipboard_toast.show_with_message(&error);
            return;
        }
        let terminal = self.pane_registry.borrow().terminals.get(&surface).cloned();
        let Some(terminal) = terminal else {
            status("Cannot create a terminal tab for this session.");
            return;
        };
        #[cfg(not(target_os = "macos"))]
        let started = terminal.write_input(format!("{line}\r").as_bytes());
        #[cfg(target_os = "macos")]
        let started: std::io::Result<()> = {
            let _ = terminal;
            Ok(())
        };
        match started {
            Ok(()) => {
                status(&format!(
                    "Opened {} in an ordinary tab of this window; it ends when the tab closes. Managed launch unavailable: {reason}",
                    session.agent.name()
                ));
                self.focus_pane(pane);
            }
            Err(error) => status(&format!("Cannot start {}: {error}", session.agent.name())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_agent_resume_keeps_the_new_shell_path() {
        let home = session_home(&SessionTarget {
            agent: SessionAgent::Codex,
            focused: None,
        })
        .unwrap();
        assert!(
            !home.environment.contains_key("PATH"),
            "the GUI process PATH must not replace the initialized shell PATH"
        );
    }

    #[test]
    fn native_resume_and_fork_use_selected_project_and_config_without_shell_expansion() {
        for (agent, binary, variable, flag, directory, yolo_arguments) in [
            (
                SessionAgent::Claude,
                "claude",
                "CLAUDE_CONFIG_DIR",
                "--resume",
                ".claude",
                "--allow-dangerously-skip-permissions\n--permission-mode\nbypassPermissions\n",
            ),
            (
                SessionAgent::Codex,
                "codex",
                "CODEX_HOME",
                "resume",
                ".codex",
                "--dangerously-bypass-approvals-and-sandbox\n",
            ),
            (
                SessionAgent::OpenCode,
                "opencode",
                "XDG_DATA_HOME",
                "--session",
                ".local/share",
                "--auto\n",
            ),
            (
                SessionAgent::Antigravity,
                "agy",
                "HOME",
                "--conversation",
                ".gemini",
                "--dangerously-skip-permissions\n",
            ),
            (
                SessionAgent::Cline,
                "cline",
                "CLINE_DATA_DIR",
                "--id",
                ".cline",
                "--yolo\n",
            ),
            (
                SessionAgent::Gemini,
                "gemini",
                "GEMINI_CLI_HOME",
                "--resume",
                ".gemini",
                "--yolo\n",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let cwd = root.path().join("other project ' 한글 $(exit 9)");
            std::fs::create_dir(&cwd).unwrap();
            let mut home = SessionHome {
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
            home.environment
                .insert("PATH".into(), "/usr/bin:/bin".into());
            for yolo in [false, true] {
                let output = std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg(resume_shell_line(&session, &home, yolo).unwrap())
                    .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                    .env("FLOWMUX_AGENT_SHIM_DIR", &bin)
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    format!(
                        "{}\n{}\n{flag}\n{}\n{}{}",
                        cwd.display(),
                        home.path.display(),
                        session.id,
                        if agent == SessionAgent::Cline {
                            "--tui\n--data-dir\ndata ' $(exit 7)\n"
                        } else {
                            ""
                        },
                        if yolo { yolo_arguments } else { "" }
                    )
                );
                let launch =
                    prepare_session_launch(&session, &home, yolo, false, "/bin/sh".into()).unwrap();
                let output = std::process::Command::new("/bin/sh")
                    .args(["-c", &launch.argv[2]])
                    .envs(launch.env)
                    .env("FLOWMUX_AGENT_SHIM_DIR", &bin)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
                assert_eq!(launch.provider, agent.name().to_ascii_lowercase());
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    format!(
                        "{}\n{}\n{flag}\n{}\n{}{}",
                        cwd.display(),
                        home.path.display(),
                        session.id,
                        if agent == SessionAgent::Cline {
                            "--tui\n--data-dir\ndata ' $(exit 7)\n"
                        } else {
                            ""
                        },
                        if yolo { yolo_arguments } else { "" }
                    )
                );
            }
            let fork_arguments = match agent {
                SessionAgent::Claude => Some(format!("--resume\n{}\n--fork-session\n", session.id)),
                SessionAgent::Codex => Some(format!("fork\n{}\n", session.id)),
                SessionAgent::OpenCode => Some(format!("--session\n{}\n--fork\n", session.id)),
                _ => None,
            };
            if let Some(fork_arguments) = fork_arguments {
                let output = std::process::Command::new("/bin/sh")
                    .arg("-c")
                    .arg(session_shell_line(&session, &home, true, true).unwrap())
                    .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
                    .env("FLOWMUX_AGENT_SHIM_DIR", &bin)
                    .output()
                    .unwrap();
                assert!(output.status.success());
                assert_eq!(
                    String::from_utf8(output.stdout).unwrap(),
                    format!(
                        "{}\n{}\n{fork_arguments}{yolo_arguments}",
                        cwd.display(),
                        home.path.display()
                    )
                );
            } else {
                assert!(session_shell_line(&session, &home, true, true).is_err());
            }
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
                .arg(resume_shell_line(&session, &default_home, false).unwrap())
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
            assert!(resume_shell_line(&session, &home, false).is_err());
            session.cwd = cwd;
            session.id = "bad\rcommand".into();
            assert!(resume_shell_line(&session, &home, false).is_err());
        }
    }

    #[test]
    fn managed_resume_replaces_the_initialized_shell_with_the_provider() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let provider = root.path().join("codex");
        std::fs::write(
            &provider,
            "#!/bin/sh\nprintf '%s\\n' \"$$\" \"$STARTUP_VALUE\" \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700)).unwrap();
        let shell = root.path().join("configured-shell");
        std::fs::write(&shell, format!(
            "#!/bin/sh\n[ \"$1\" = -lic ] || exit 9\nexport STARTUP_VALUE=initialized\nexport PATH={}:$PATH\nexec /bin/sh -c \"$2\"\n", shell_quote(root.path().to_str().unwrap())
        )).unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = SessionHome {
            path: root.path().into(),
            environment: Default::default(),
            arguments: vec![],
        };
        let session = HistorySession {
            agent: SessionAgent::Codex,
            id: "12345678-1234-4234-8234-123456789abc".into(),
            title: "test".into(),
            summary: String::new(),
            cwd: root.path().into(),
            path: "/unused".into(),
            modified: std::time::SystemTime::now(),
        };
        for fork in [false, true] {
            let launch = prepare_session_launch(
                &session,
                &home,
                false,
                fork,
                shell.to_str().unwrap().into(),
            )
            .unwrap();
            let child = std::process::Command::new(&launch.argv[0])
                .args(&launch.argv[1..])
                .envs(launch.env)
                .env("FLOWMUX_AGENT_SHIM_DIR", root.path())
                .current_dir(&launch.cwd)
                .stdout(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let pid = child.id();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success());
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                format!(
                    "{pid}\ninitialized\n{}\n{}\n",
                    if fork { "fork" } else { "resume" },
                    session.id
                )
            );
        }
    }

    #[test]
    fn only_a_launch_the_host_never_received_falls_back_to_an_ordinary_tab() {
        use flowmux_ipc::agent_host::HostResponse;
        use flowmux_ipc::protocol::{Envelope, Payload, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        // An older incompatible Host may run this session; a compatible one loses the response.
        let [incompatible, unknown] =
            [u32::MAX, flowmux_ipc::agent_host::PROTOCOL_VERSION].map(|version| {
                let root = tempfile::tempdir_in("/tmp").unwrap();
                let socket = root.path().join("host.sock");
                let launch = flowmux_cli::agent_host::prepare_launch(
                    vec!["/bin/sh".into()],
                    root.path(),
                    vec![],
                )
                .unwrap();
                runtime.block_on(async {
                    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
                    let server = async {
                        let (stream, _) = listener.accept().await.unwrap();
                        let (read, mut write) = stream.into_split();
                        let mut lines = BufReader::new(read).lines();
                        let hello: Envelope =
                            serde_json::from_str(&lines.next_line().await.unwrap().unwrap())
                                .unwrap();
                        let reply = Envelope {
                            id: hello.id,
                            payload: Payload::Response(Response::AgentHost {
                                response: HostResponse::Hello {
                                    version,
                                    epoch: uuid::Uuid::new_v4(),
                                    connection: 1,
                                },
                            }),
                        };
                        let reply = format!("{}\n", serde_json::to_string(&reply).unwrap());
                        write.write_all(reply.as_bytes()).await.unwrap();
                        let _launch = lines.next_line().await;
                    };
                    tokio::join!(server, flowmux_cli::agent_host::launch(&socket, launch))
                        .1
                        .unwrap_err()
                })
            });
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let launch =
            flowmux_cli::agent_host::prepare_launch(vec!["/bin/sh".into()], root.path(), vec![])
                .unwrap();
        // No Host answers and startup fails before any service is submitted.
        let definite = runtime
            .block_on(flowmux_cli::agent_host::launch(
                std::path::Path::new("relative-missing-host.sock"),
                launch,
            ))
            .unwrap_err();
        let reason = tab_fallback_reason(&definite).expect("unstarted Host is definite");
        assert!(tab_fallback_reason(&definite.context("Cannot start session")).is_some());
        let refused = format!("{incompatible:#}");
        assert!(
            incompatible.is::<flowmux_ipc::agent_host::IncompatibleHost>(),
            "{refused}"
        );
        assert!(refused.contains("cannot open them"), "{refused}");
        assert!(tab_fallback_reason(&incompatible).is_none());
        let lost = format!("{unknown:#}");
        assert!(lost.contains("result unknown"), "{lost}");
        assert!(tab_fallback_reason(&unknown).is_none());
        // Classification is structural; matching message text never permits a second launch.
        assert!(tab_fallback_reason(&anyhow::anyhow!("{reason}")).is_none());
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    async fn panel_keeps_other_panels_and_blocks_current_and_stale_targets() {
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
            .contains("Select an agent"));
        controller.focused_pane.set(None);
        for agent in [
            SessionAgent::Claude,
            SessionAgent::Codex,
            SessionAgent::OpenCode,
            SessionAgent::Antigravity,
            SessionAgent::Cline,
            SessionAgent::Gemini,
        ] {
            controller
                .dispatch_session_panel(SessionPanelAction::ToggleAgent(agent))
                .await;
            let target = controller.session_target().await.unwrap();
            assert_eq!(target.agent, agent);
            assert!(target.focused.is_none());
            assert_eq!(
                session_home(&target).unwrap().path,
                agent.env_history_home().unwrap()
            );
        }
        controller
            .dispatch_session_panel(SessionPanelAction::ToggleAgent(SessionAgent::Gemini))
            .await;
        assert!(controller.session_target().await.is_none());
        controller.focused_pane.set(Some(pane));
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
                        turn_outcome: None,
                        pending_waits: None,
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
                .resume_history_session(session.clone(), generation.wrapping_sub(1), false)
                .await;
            assert_eq!(panel.status.text(), "before");
            controller
                .resume_history_session(session.clone(), generation, false)
                .await;
            assert!(panel.status.text().contains("already active"));
            controller
                .dispatch_session_panel(SessionPanelAction::ToggleAgent(SessionAgent::Codex))
                .await;
            controller
                .resume_history_session(session.clone(), panel.generation.get(), false)
                .await;
            assert!(panel.status.text().contains("already active"));
            controller
                .dispatch_session_panel(SessionPanelAction::ToggleAgent(SessionAgent::Codex))
                .await;
            let target = controller.session_target().await.unwrap();
            assert_eq!(target.agent, SessionAgent::Codex);
            assert_eq!(target.focused.unwrap().surface, surface);
        }
        let directory = tempfile::tempdir().unwrap();
        let mut missing = session.clone();
        missing.id = "23456789-1234-4234-8234-123456789abc".into();
        missing.cwd = directory.path().join("missing project");
        let tab_count = controller.pane_registry.borrow().terminals.len();
        let generation = controller.sessions.panel.generation.get();
        controller
            .resume_history_session(missing.clone(), generation, false)
            .await;
        for _ in 0..100 {
            if controller.window.visible_dialog().is_some() {
                break;
            }
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        let dialog = controller
            .window
            .visible_dialog()
            .expect("missing project directory must show an error popup")
            .downcast::<adw::AlertDialog>()
            .unwrap();
        assert_eq!(dialog.heading().as_deref(), Some("Cannot resume session"));
        assert!(dialog.body().contains(missing.cwd.to_str().unwrap()));
        assert_eq!(controller.pane_registry.borrow().terminals.len(), tab_count);
        dialog.close();
        controller.focused_pane.set(None);
        let panel = &controller.sessions.panel;
        let generation = panel.clear("focus moved");
        controller
            .resume_history_session(session, generation, false)
            .await;
        assert_eq!(panel.status.text(), "focus moved");
        controller.window.close();
    }
}
