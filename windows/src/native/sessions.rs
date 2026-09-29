// SPDX-License-Identifier: GPL-3.0-or-later
//! Focus-bound agent histories. A single worker owns discovery, reads and resume validation.
use super::super::agent_process::{self, AgentProcess};
use super::*;
use crate::session_history::{self, HistorySession};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) enum Signal {
    Ui(Uuid, session_panel::UiAction),
    Done(Uuid, Box<Result<Reply, String>>),
}
pub(super) enum Reply {
    Scanned(Option<AgentProcess>, Option<Vec<HistorySession>>),
    Preview(AgentProcess, String, String),
    Resume(AgentProcess, HistorySession, crate::shell::Shell),
}
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
struct Source {
    surface: SurfaceId,
    session: Uuid,
    pid: u32,
}
enum Task {
    Scan(bool),
    Preview(HistorySession),
    Resume(HistorySession),
}
struct Job {
    id: Uuid,
    source: Source,
    cancel: Arc<AtomicBool>,
}
#[derive(Default)]
pub(super) struct Controller {
    open: bool,
    id: Uuid,
    panel: Option<session_panel::Panel>,
    source: Option<Source>,
    agent: Option<AgentProcess>,
    current_session: Option<String>,
    rows: Vec<HistorySession>,
    selected: Option<String>,
    preview: String,
    status: String,
    resume_enabled: bool,
    preview_height_dip: i32,
    pending: Option<Task>,
    job: Option<Job>,
    next_probe: Option<Instant>,
}
impl Controller {
    fn can_resume(&self) -> bool {
        self.resume_enabled
            && self.job.is_none()
            && self.selected.is_some()
            && self.selected != self.current_session
    }
    pub(super) fn shutdown(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Release);
        }
        self.open = false;
        self.panel.take();
        self.pending = None;
        self.source = None;
        self.agent = None;
        self.current_session = None;
        self.rows.clear();
        self.selected = None;
        self.preview.clear();
        self.resume_enabled = false;
        self.next_probe = None;
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|p| p.handle_message(message))
    }
    pub(super) fn refresh_theme(&mut self) -> anyhow::Result<()> {
        if let Some(panel) = &mut self.panel {
            panel.refresh_theme()?;
        }
        Ok(())
    }
    pub(super) fn capture_window(&self) -> Option<HWND> {
        self.panel.as_ref().map(|p| p.capture_window())
    }
    pub(super) fn status(&self) -> Value {
        json!({"open":self.open,"id":self.id,"source":self.source,
            "agent":self.agent.as_ref().map(|a|json!({"name":a.agent.name(),"pid":a.pid,"home":a.home,"session_id":self.current_session})),
            "loading":self.job.is_some(),"status":self.status,"rows":self.rows,
            "selected":self.selected,"resume_enabled":self.can_resume(),
            "panel":self.panel.as_ref().map(session_panel::Panel::status)})
    }
    fn clear(&mut self) {
        self.agent = None;
        self.current_session = None;
        self.rows.clear();
        self.selected = None;
        self.preview.clear();
        self.resume_enabled = false;
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// Start an agent with argv and a child-only environment, then return to the user's
// configured shell. No history text is evaluated or written into a running agent.
fn resume_shell(
    agent: &AgentProcess,
    item: &HistorySession,
    normal: &crate::shell::Shell,
) -> anyhow::Result<crate::shell::Shell> {
    anyhow::ensure!(
        item.agent == agent.agent && item.cwd.is_absolute() && item.cwd.is_dir(),
        "The session project directory is unavailable"
    );
    anyhow::ensure!(
        agent.session_id.as_deref() != Some(&item.id),
        "This session is already active in the focused tab"
    );
    let mut args = agent.prefix.clone();
    args.extend(item.resume_argv()?.into_iter().skip(1));
    args.extend(agent.arguments.iter().cloned());
    let normal = super::super::shell::resolve(normal)?;
    let normal_args = normal
        .command
        .strip_prefix(&crate::shell::quote_arg(
            &normal.executable.to_string_lossy(),
        ))
        .context("Invalid configured shell command")?
        .trim_start();
    let payload = json!({"program":agent.executable,"args":args.iter().map(|a|crate::shell::quote_arg(a)).collect::<Vec<_>>().join(" "),
        "cwd":item.cwd,"environment":agent.environment,"unset":agent.agent.home_variables(),
        "shell":normal.executable,"shell_args":normal_args,"cmd_prompt":normal.cmd_prompt});
    let data = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&payload)?);
    let script = format!(
        r#"$ErrorActionPreference='Stop'
$p=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{data}'))|ConvertFrom-Json
function Start-TerminalProcess($program,$arguments,$agent) {{
 $s=New-Object Diagnostics.ProcessStartInfo
 $s.FileName=$program;$s.Arguments=$arguments;$s.WorkingDirectory=$p.cwd;$s.UseShellExecute=$false
 if($agent) {{
  foreach($key in $p.unset) {{$s.EnvironmentVariables.Remove($key)}}
  foreach($entry in $p.environment.PSObject.Properties) {{$s.EnvironmentVariables[$entry.Name]=[string]$entry.Value}}
 }} elseif($p.cmd_prompt) {{
  $prompt=$s.EnvironmentVariables['PROMPT'];if(-not $prompt){{$prompt='$P$G'}}
  $s.EnvironmentVariables['PROMPT']=[string][char]27+']9;9;$P'+[char]27+[char]92+$prompt
 }}
 $child=[Diagnostics.Process]::Start($s);try{{$child.WaitForExit();return $child.ExitCode}}finally{{$child.Dispose()}}
}}
try {{$null=Start-TerminalProcess $p.program $p.args $true}}catch{{[Console]::Error.WriteLine('Cannot resume session: '+$_.Exception.Message)}}
exit (Start-TerminalProcess $p.shell $p.shell_args $false)
"#
    );
    let executable =
        PathBuf::from(std::env::var_os("SystemRoot").context("SystemRoot unavailable")?)
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let shell = crate::shell::Shell {
        program: executable.to_string_lossy().into_owned(),
        args: vec![
            "-NoLogo".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            base64::engine::general_purpose::STANDARD.encode(
                script
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>(),
            ),
        ],
    };
    shell.validate()?;
    Ok(shell)
}

impl App {
    fn session_source(&self) -> Option<Source> {
        if self.main_closed || self.detached_focus.is_some() {
            return None;
        }
        let workspace = self.current_workspace()?;
        if workspace.ssh.is_some() {
            return None;
        }
        let surface = workspace.active();
        let terminal = self.surfaces.get(&surface)?;
        let process = terminal.session.as_ref()?;
        Some(Source {
            surface,
            session: terminal.session_generation,
            pid: process.pid,
        })
    }
    fn sessions_guard(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.main_closed
                && !self.closing
                && !self.close_accepted
                && self.close_request.is_none()
                && self.pending_save.is_none()
                && self.editor_barrier.is_none()
                && !self.overview.is_open()
                && unsafe { IsWindowEnabled(self.window) } != 0,
            "Window is busy"
        );
        Ok(())
    }
    pub(super) fn toggle_sessions(&mut self) -> anyhow::Result<()> {
        self.sessions_guard()?;
        if self.sessions.open {
            self.cancel_drag();
            self.sessions.shutdown();
        } else {
            self.sessions.id = Uuid::new_v4();
            self.sessions.panel = Some(session_panel::Panel::new(
                self.window,
                self.background_test,
                self.sessions.id,
                self.sessions.preview_height_dip,
            )?);
            self.sessions.open = true;
            self.sessions.source = None;
            self.sessions.pending = Some(Task::Scan(true));
        }
        self.sessions_reconcile()?;
        self.layout()
    }
    pub(super) fn sessions_width(&self, width: i32, scale: f64) -> i32 {
        if !self.sessions.open {
            return 0;
        }
        let available = (width - (160.0 * scale).round() as i32).max(0);
        if available < (200.0 * scale).round() as i32 {
            0
        } else {
            ((360.0 * scale).round() as i32).min(available)
        }
    }
    pub(super) fn sessions_preview_height(&self, id: Uuid) -> Option<i32> {
        if !self.sessions.open || self.sessions.id != id {
            return None;
        }
        self.sessions.panel.as_ref()?.preview_height()
    }
    pub(super) fn sessions_resize_preview(&mut self, id: Uuid, height: i32) -> anyhow::Result<()> {
        if self.sessions_preview_height(id).is_none() {
            self.cancel_drag();
            return Ok(());
        }
        self.sessions.preview_height_dip =
            self.sessions.panel.as_ref().unwrap().resize_preview(height);
        self.layout()
    }
    pub(super) fn sessions_layout(&mut self, area: Option<model::Rect>) {
        if let Some(panel) = &self.sessions.panel {
            panel.layout(area);
        }
        if matches!(self.drag, Some(panes::Drag::Sessions { .. }))
            && self.sessions_preview_height(self.sessions.id).is_none()
        {
            self.cancel_drag();
        }
    }
    fn sessions_render(&mut self) -> anyhow::Result<()> {
        let can_resume = self.sessions.can_resume();
        if let Some(panel) = &mut self.sessions.panel {
            panel.update(
                &self.sessions.rows,
                self.sessions.selected.as_deref(),
                &self.sessions.status,
                &self.sessions.preview,
                can_resume,
                self.sessions.job.is_some(),
            )?;
        }
        Ok(())
    }
    fn sessions_sync_current(&mut self) -> anyhow::Result<()> {
        let current = self.sessions.agent.as_ref().and_then(|agent| {
            self.sessions
                .source
                .as_ref()
                .and_then(|source| self.agent_session_id(source.surface, agent))
                .unwrap_or_else(|| agent.session_id.clone())
        });
        if self.sessions.current_session != current {
            self.sessions.current_session = current;
            self.sessions_render()?;
        }
        Ok(())
    }
    pub(super) fn sessions_reconcile(&mut self) -> anyhow::Result<()> {
        if !self.sessions.open {
            return Ok(());
        }
        let source = self.session_source();
        let changed = source != self.sessions.source;
        if changed {
            if let Some(job) = &self.sessions.job {
                job.cancel.store(true, Ordering::Release);
            }
            self.sessions.source = source;
            self.sessions.clear();
            self.sessions.pending = Some(Task::Scan(true));
            self.sessions.status = "Loading sessions…".into();
        }
        self.sessions_sync_current()?;
        if self.sessions.source.is_none() {
            self.sessions.pending = None;
            self.sessions.status="Focus a local Claude, Codex, OpenCode, Antigravity, or Cline terminal tab to browse sessions.".into();
            return self.sessions_render();
        }
        if self.sessions.job.is_some() {
            return if changed {
                self.sessions_render()
            } else {
                Ok(())
            };
        }
        if self.sessions.pending.is_none()
            && self
                .sessions
                .next_probe
                .is_some_and(|due| Instant::now() >= due)
        {
            self.sessions.pending = Some(Task::Scan(false));
        }
        let Some(task) = self.sessions.pending.take() else {
            return Ok(());
        };
        let source = self.sessions.source.clone().unwrap();
        let process = self
            .surfaces
            .get(&source.surface)
            .and_then(|s| s.session.as_ref())
            .context("Session source disappeared")?
            .process_job();
        let id = Uuid::new_v4();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let previous = self.sessions.agent.clone();
        let normal = self.settings.default_shell.clone();
        let sender = self.sender.clone();
        self.sessions.job = Some(Job { id, source, cancel });
        if let Err(error) = std::thread::Builder::new()
            .name("flowmux-sessions".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| -> anyhow::Result<Reply> {
                    let deadline = Instant::now() + Duration::from_secs(8);
                    let agent = agent_process::discover(&process, &token, deadline)?;
                    let Some(agent) = agent else {
                        return Ok(Reply::Scanned(None, Some(Vec::new())));
                    };
                    match task {
                        Task::Scan(force) => {
                            let rows = if force || previous.as_ref() != Some(&agent) {
                                Some(session_history::list_sessions(
                                    agent.agent,
                                    &agent.home,
                                    &token,
                                    deadline,
                                )?)
                            } else {
                                None
                            };
                            Ok(Reply::Scanned(Some(agent), rows))
                        }
                        Task::Preview(item) => {
                            anyhow::ensure!(
                                previous.as_ref() == Some(&agent),
                                "The focused agent changed; refresh sessions"
                            );
                            let text = item.preview(&token, deadline)?;
                            Ok(Reply::Preview(
                                agent,
                                item.id.clone(),
                                format!(
                                    "{}\r\n{}\r\n{}\r\n\r\n{}",
                                    item.title,
                                    item.cwd.display(),
                                    item.id,
                                    text.replace('\n', "\r\n")
                                ),
                            ))
                        }
                        Task::Resume(item) => {
                            anyhow::ensure!(
                                previous.as_ref() == Some(&agent),
                                "The focused agent changed; refresh sessions"
                            );
                            let shell = resume_shell(&agent, &item, &normal)?;
                            Ok(Reply::Resume(agent, item, shell))
                        }
                    }
                })
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Session history worker failed")))
                .map_err(|e| format!("{e:#}"));
                sender.send(Event::Sessions(Signal::Done(id, Box::new(result))));
            })
        {
            self.sessions.job = None;
            self.sessions.status = format!("Cannot load sessions: {error}");
        }
        self.sessions_render()
    }
    pub(super) fn sessions_event(&mut self, event: Signal) -> anyhow::Result<()> {
        self.sessions_sync_current()?;
        match event {
            Signal::Ui(id, action) => {
                if id != self.sessions.id || !self.sessions.open {
                    return Ok(());
                }
                use session_panel::UiAction;
                if matches!(action, UiAction::Refresh | UiAction::Resume(_))
                    && self
                        .sessions
                        .panel
                        .as_ref()
                        .is_some_and(|panel| panel.composition_pending())
                {
                    return Ok(());
                }
                match action {
                    UiAction::Close => {
                        self.cancel_drag();
                        self.sessions.shutdown();
                        self.focus_active()?;
                        return self.layout();
                    }
                    UiAction::Layout => return self.layout(),
                    UiAction::Resize(delta) => {
                        if let Some(height) = self.sessions_preview_height(id) {
                            let dpi = unsafe { GetDpiForWindow(self.window).max(96) } as i32;
                            let height = if delta == 0 {
                                150 * dpi / 96
                            } else {
                                height + delta * dpi / 96
                            };
                            return self.sessions_resize_preview(id, height);
                        }
                    }
                    UiAction::Filter(query) => {
                        let selected = self.sessions.panel.as_ref().and_then(|p| p.selected());
                        if selected != self.sessions.selected {
                            if let Some(job) = &self.sessions.job {
                                job.cancel.store(true, Ordering::Release);
                            }
                            self.sessions.selected = selected;
                            self.sessions.preview.clear();
                            self.sessions.resume_enabled = false;
                            self.sessions.pending = None;
                        }
                        let _ = query;
                    }
                    UiAction::Refresh => {
                        if self.sessions.job.is_some() {
                            return Ok(());
                        }
                        self.sessions.clear();
                        self.sessions.pending = Some(Task::Scan(true));
                        self.sessions.status = "Loading sessions…".into();
                    }
                    UiAction::Select(id) => {
                        let Some(item) = self.sessions.rows.iter().find(|r| r.id == id).cloned()
                        else {
                            return Ok(());
                        };
                        if let Some(job) = &self.sessions.job {
                            job.cancel.store(true, Ordering::Release);
                        }
                        self.sessions.selected = Some(id);
                        self.sessions.preview = "Loading conversation…".into();
                        self.sessions.resume_enabled = false;
                        self.sessions.pending = Some(Task::Preview(item));
                    }
                    UiAction::Resume(id) => {
                        self.sessions_guard()?;
                        if !self.sessions.can_resume()
                            || self.sessions.selected.as_deref() != Some(&id)
                        {
                            return Ok(());
                        }
                        let item = self
                            .sessions
                            .rows
                            .iter()
                            .find(|r| r.id == id)
                            .cloned()
                            .context("Session no longer listed")?;
                        self.sessions.resume_enabled = false;
                        self.sessions.pending = Some(Task::Resume(item));
                    }
                }
                self.sessions_reconcile()?;
            }
            Signal::Done(id, result) => {
                if self.sessions.job.as_ref().is_none_or(|j| j.id != id) {
                    return Ok(());
                }
                let job = self.sessions.job.take().unwrap();
                self.sessions.next_probe = Some(Instant::now() + Duration::from_secs(2));
                if !self.sessions.open
                    || job.cancel.load(Ordering::Acquire)
                    || self.session_source().as_ref() != Some(&job.source)
                {
                    return self.sessions_reconcile();
                }
                match *result {
                    Ok(Reply::Scanned(agent, rows)) => {
                        if let Some(rows) = rows {
                            self.sessions.rows = rows;
                            self.sessions.selected = None;
                            self.sessions.preview.clear();
                            self.sessions.resume_enabled = false;
                        }
                        self.sessions.status=agent.as_ref().map(|a|format!("{} · {} sessions · all projects",a.agent.name(),self.sessions.rows.len())).unwrap_or_else(||"Focus a local Claude, Codex, OpenCode, Antigravity, or Cline terminal tab to browse sessions.".into());
                        self.sessions.agent = agent;
                    }
                    Ok(Reply::Preview(agent, id, text)) => {
                        if self.sessions.selected.as_deref() == Some(&id)
                            && self.sessions.agent.as_ref() == Some(&agent)
                        {
                            self.sessions.preview = text;
                            self.sessions.resume_enabled = true;
                        }
                    }
                    Ok(Reply::Resume(agent, item, shell)) => {
                        if self.sessions.agent.as_ref() == Some(&agent)
                            && self.sessions.selected.as_deref() == Some(&item.id)
                            && self.sessions.current_session.as_deref() != Some(&item.id)
                        {
                            self.sessions.resume_enabled = true;
                            if self
                                .sessions
                                .panel
                                .as_ref()
                                .is_some_and(|panel| panel.composition_pending())
                            {
                                return self.sessions_render();
                            }
                            self.sessions_guard()?;
                            let normal = self.settings.default_shell.clone();
                            let surface = self.new_terminal(
                                job.source.surface,
                                Some(item.cwd),
                                Some(normal),
                                shells::NewTerminal::Tab,
                            )?;
                            self.startup_shells.insert(surface, shell);
                        }
                    }
                    Err(error) => {
                        self.sessions.status = error;
                        self.sessions.resume_enabled = false;
                    }
                }
                self.sessions_reconcile()?;
            }
        }
        self.sessions_render()
    }
}
