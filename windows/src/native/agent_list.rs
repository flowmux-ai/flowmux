// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned-process discovery and runtime-only, ordered activity snapshots.
use super::super::agent_process;
use super::*;
use crate::command::{AgentReportArgs, SessionHookArgs, SessionHookEvent};
use flowmux_core::{AgentPresence, AgentStatus, AgentStatusReport, AGENT_SOURCE_PROC};
use std::{
    os::windows::io::{AsRawHandle, OwnedHandle},
    sync::atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::System::Threading::WaitForSingleObject;

struct Entry {
    surface: SurfaceId,
    generation: Uuid,
    process: OwnedHandle,
    pid: u32,
    agent: crate::session_history::SessionAgent,
    cwd: PathBuf,
    session_id: Option<String>,
    launch: agent_process::AgentProcess,
}
pub(super) struct State {
    entry: Entry,
    presence: AgentPresence,
    session_reported: bool,
    last_hook: Option<Instant>,
    native: Option<crate::agent_activity::Activity>,
}
impl State {
    fn native_status(&mut self, snapshot: crate::agent_activity::Snapshot, visible: bool) -> bool {
        let before = (
            self.presence.public_status(),
            self.presence.seen,
            self.presence.status_text().map(str::to_string),
        );
        let mut report =
            AgentStatusReport::from_activity(&self.presence.name, None, Some(self.entry.pid));
        report.status = Some(snapshot.status);
        report.source = Some("flowmux:hook".into());
        report.message = Some(snapshot.text);
        report.session_id = self.presence.session_id.clone();
        self.presence.apply_report(report, visible);
        if snapshot.interrupted {
            self.presence.seen = true;
        }
        before
            != (
                self.presence.public_status(),
                self.presence.seen,
                self.presence.status_text().map(str::to_string),
            )
    }
}
pub(super) enum Report {
    Activity(SurfaceId, AgentReportArgs),
    Session(SurfaceId, SessionHookArgs),
}
impl Report {
    fn surface(&self) -> SurfaceId {
        match self {
            Self::Activity(id, _) | Self::Session(id, _) => *id,
        }
    }
}
struct Discovery {
    entries: Vec<Entry>,
    last: Option<SurfaceId>,
}
impl Entry {
    fn current(&self, app: &App) -> bool {
        app.surfaces.get(&self.surface).is_some_and(|surface| {
            surface.session_generation == self.generation
                && surface.exit_code.is_none()
                && surface.session.is_some()
                && unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } == WAIT_TIMEOUT
        })
    }
}
pub(super) struct Pending {
    reply: Option<ipc::Reply>,
    receiver: Receiver<Result<Discovery, String>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    report: Option<Report>,
    automatic: bool,
    next: Option<(ipc::Reply, Option<Report>)>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(reply) = self.reply.take() {
            let _ =
                reply.try_send(json!({"error":"Window closed before agent discovery finished"}));
        }
        if let Some((reply, _)) = self.next.take() {
            let _ =
                reply.try_send(json!({"error":"Window closed before agent discovery finished"}));
        }
    }
}
impl App {
    pub(super) fn saved_agent_sessions(
        &self,
    ) -> HashMap<SurfaceId, crate::state::SavedAgentSession> {
        let mut saved = self.restore_agents.clone();
        for (surface, state) in self.agent_states.borrow().iter() {
            let entry = &state.entry;
            if !entry.current(self)
                || self
                    .locate(*surface)
                    .is_none_or(|(workspace, _, _)| self.workspaces[workspace].ssh.is_some())
            {
                continue;
            }
            if let Some(id) = &state.presence.session_id {
                match entry.launch.saved_session(id, &entry.cwd) {
                    Ok(session) => {
                        saved.insert(*surface, session);
                    }
                    Err(_) => report("Cannot save an invalid agent session binding"),
                }
            }
        }
        saved
    }

    pub(super) fn agent_session_id(
        &self,
        id: SurfaceId,
        agent: &agent_process::AgentProcess,
    ) -> Option<Option<String>> {
        self.agent_states
            .borrow()
            .get(&id)
            .filter(|state| {
                state.entry.current(self)
                    && state.entry.pid == agent.pid
                    && state.entry.agent == agent.agent
                    && state.session_reported
            })
            .map(|state| state.presence.session_id.clone())
    }

    pub(super) fn agent_hook_request(
        &mut self,
        mut args: SessionHookArgs,
        caller: Option<SurfaceId>,
        reply: ipc::Reply,
    ) -> anyhow::Result<Option<Value>> {
        anyhow::ensure!(
            reply.received_at().elapsed() < Duration::from_secs(3),
            "Session hook expired"
        );
        let surface = args
            .surface
            .map(SurfaceId)
            .or(caller)
            .context("session hook requires --surface or an inherited terminal context")?;
        let agent = crate::session_history::SessionAgent::from_name(&args.agent)
            .context("unsupported local agent name")?;
        args.agent = agent.name().to_ascii_lowercase();
        args.session_id = Some(
            agent.canonical_session_id(
                args.session_id
                    .as_deref()
                    .context("session hook requires a conversation ID")?,
            )?,
        );
        args.details.validate(args.event, &args.agent)?;
        self.agents_poll();
        if self.agent_presence(surface).is_some() {
            return self
                .apply_agent_hook(surface, &args, None, &reply)
                .map(Some);
        }
        self.agents_request(Some(Report::Session(surface, args)), reply)?;
        Ok(None)
    }

    fn apply_agent_hook(
        &mut self,
        surface: SurfaceId,
        args: &SessionHookArgs,
        entry: Option<Entry>,
        reply: &ipc::Reply,
    ) -> anyhow::Result<Value> {
        let job = self
            .surfaces
            .get(&surface)
            .and_then(|s| s.session.as_ref())
            .context("Hook target is not a running terminal")?
            .process_job();
        {
            let states = self.agent_states.borrow();
            let entry = entry
                .as_ref()
                .or_else(|| states.get(&surface).map(|s| &s.entry))
                .context("Agent process is no longer available")?;
            anyhow::ensure!(
                entry.current(self) && entry.agent.name().eq_ignore_ascii_case(&args.agent),
                "Hook does not match the live agent in this terminal"
            );
            agent_process::verify_hook_caller(
                &job,
                reply
                    .client_process()
                    .context("Hook has no authenticated pipe caller")?,
                &entry.process,
            )?;
        }
        anyhow::ensure!(
            reply.received_at().elapsed() < Duration::from_secs(3),
            "Session hook expired"
        );
        if let Some(entry) = entry {
            self.record_agents(vec![entry]);
        }
        let mut states = self.agent_states.borrow_mut();
        let state = states
            .get_mut(&surface)
            .context("Agent process is no longer available")?;
        // Native hooks have no producer sequence. Order observed requests only;
        // an old conversation's end must never clear the current conversation.
        let visible = self.source_is_focused(Some(surface));
        let mut notice = None;
        let accepted = if args.event.is_session() {
            state
                .last_hook
                .is_none_or(|last| reply.received_at() > last)
                && (args.event == SessionHookEvent::SessionStart
                    || state.presence.session_id == args.session_id)
        } else if state.presence.session_id == args.session_id
            || (!state.session_reported && state.presence.session_id.is_none())
        {
            let activity = state
                .native
                .get_or_insert_with(|| crate::agent_activity::Activity::new(&args.agent));
            let accepted = activity.apply(args.event, &args.details, reply.received_at());
            if accepted {
                let snapshot = activity.snapshot();
                let identity_changed = state.presence.session_id != args.session_id;
                state.presence.session_id = args.session_id.clone();
                if state.native_status(snapshot, visible) || identity_changed {
                    self.sender.send(Event::AgentChanged);
                }
            }
            // These Claude notices describe an unattributed background session.
            // Retain their attention without changing the pane's root activity.
            let notify_only = args.agent == "claude"
                && args.event == SessionHookEvent::Notification
                && state.presence.session_id == args.session_id
                && matches!(
                    args.details.notification_type.as_deref(),
                    Some("agent_completed" | "agent_needs_input")
                );
            if accepted || notify_only {
                notice = crate::notifications::hook_notice(
                    state.entry.agent.name(),
                    args.event,
                    &args.details,
                );
            }
            accepted || notify_only
        } else {
            false
        };
        if accepted {
            state.last_hook = Some(
                state
                    .last_hook
                    .map_or(reply.received_at(), |last| last.max(reply.received_at())),
            );
            state.session_reported = true;
            let id = if args.event == SessionHookEvent::SessionEnd {
                None
            } else {
                args.session_id.clone()
            };
            if state.presence.session_id != id {
                if state.native.take().is_some() {
                    state.native_status(
                        crate::agent_activity::Snapshot {
                            status: AgentStatus::Unknown,
                            text: if id.is_some() {
                                "Ready"
                            } else {
                                "Session ended"
                            }
                            .into(),
                            interrupted: true,
                        },
                        visible,
                    );
                }
                state.presence.session_id = id;
                self.sender.send(Event::AgentChanged);
            }
        }
        let mut presence = state.presence.clone();
        presence.status = presence.public_status();
        let result = json!({"accepted":accepted,"surface":surface,"session_id":state.presence.session_id,"agent":presence});
        drop(states);
        if let Some(notice) = notice {
            self.add_agent_notification(surface, notice);
        }
        Ok(result)
    }

    pub(super) fn agent_presence(&self, id: SurfaceId) -> Option<AgentPresence> {
        self.agent_states
            .borrow()
            .get(&id)
            .filter(|state| state.entry.current(self))
            .map(|state| {
                let mut presence = state.presence.clone();
                presence.status = presence.public_status();
                presence
            })
    }

    pub(super) fn ack_focused_agent(&self, id: SurfaceId) {
        if let Some(state) = self.agent_states.borrow_mut().get_mut(&id) {
            if state.presence.mark_seen() {
                self.sender.send(Event::AgentChanged);
            }
        }
    }

    pub(super) fn agent_report_request(
        &mut self,
        mut args: AgentReportArgs,
        caller: Option<SurfaceId>,
        reply: ipc::Reply,
    ) -> anyhow::Result<Option<Value>> {
        anyhow::ensure!(
            reply.received_at().elapsed() < Duration::from_secs(3),
            "agent report expired before processing"
        );
        let surface = args
            .surface
            .map(SurfaceId)
            .or(caller)
            .context("report-agent requires --surface or an inherited terminal context")?;
        let agent = crate::session_history::SessionAgent::from_name(&args.agent)
            .context("unsupported local agent name")?;
        args.agent = agent.name().to_ascii_lowercase();
        args.session_id = args
            .session_id
            .as_deref()
            .map(|id| agent.canonical_session_id(id))
            .transpose()?;
        anyhow::ensure!(
            args.pid > 0 && args.seq > 0,
            "agent PID and sequence must be positive"
        );
        anyhow::ensure!(
            matches!(
                args.status.as_str(),
                "unknown" | "idle" | "working" | "blocked"
            ),
            "invalid agent status; done is derived from an unseen idle transition"
        );
        anyhow::ensure!(
            args.message
                .as_ref()
                .is_none_or(|text| text.len() <= 1024 && !text.chars().any(char::is_control)),
            "agent message must be one line of at most 1024 UTF-8 bytes"
        );
        self.agents_poll();
        if self.agent_presence(surface).is_some() {
            return self.apply_agent_report(surface, &args, None).map(Some);
        }
        self.agents_request(Some(Report::Activity(surface, args)), reply)?;
        Ok(None)
    }

    fn apply_agent_report(
        &mut self,
        surface: SurfaceId,
        args: &AgentReportArgs,
        entry: Option<Entry>,
    ) -> anyhow::Result<Value> {
        let before = self.agent_presence(surface);
        let status: AgentStatus = serde_json::from_value(json!(args.status))?;
        let mut report = AgentStatusReport::from_activity(&args.agent, None, Some(args.pid));
        report.status = Some(status);
        report.source = Some("flowmux:report".into());
        report.seq = Some(args.seq);
        report.message = args.message.clone();
        report.session_id = args.session_id.clone();
        let visible = self.source_is_focused(Some(surface));
        if let Some(entry) = entry {
            anyhow::ensure!(
                entry.current(self)
                    && entry.pid == args.pid
                    && entry.agent.name().eq_ignore_ascii_case(&args.agent),
                "report does not match the live agent in this terminal"
            );
            self.record_agents(vec![entry]);
        }
        let accepted = {
            let mut states = self.agent_states.borrow_mut();
            let state = states
                .get_mut(&surface)
                .context("agent process is no longer available")?;
            anyhow::ensure!(
                state.entry.current(self)
                    && state.entry.pid == args.pid
                    && state.presence.name == args.agent,
                "report does not match the live agent in this terminal"
            );
            let current_session = state.presence.session_id.clone();
            let accepted = if state.presence.source.as_deref() == Some(AGENT_SOURCE_PROC)
                && state.presence.seq.is_none()
            {
                // Process identity alone is not a started turn. The first
                // idle report establishes presence without inventing Done.
                state.presence =
                    AgentPresence::from_report(report, visible).context("missing agent status")?;
                true
            } else {
                state.presence.apply_report(report, visible)
            };
            if accepted {
                if args.session_id.is_some() && args.session_id != current_session {
                    state.native = None;
                }
                state.session_reported |= args.session_id.is_some();
                if args.session_id.is_none() {
                    state.presence.session_id = current_session;
                }
            }
            accepted
        };
        let after = self.agent_presence(surface);
        let display = |p: &AgentPresence| {
            (
                p.name.clone(),
                p.status,
                p.seen,
                p.status_text().map(str::to_string),
                p.session_id.clone(),
            )
        };
        if before.as_ref().map(display) != after.as_ref().map(display) {
            self.sender.send(Event::AgentChanged);
        }
        Ok(json!({"accepted":accepted,"surface":surface,"agent":after}))
    }

    pub(super) fn agents_request(
        &mut self,
        report: Option<Report>,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        self.agents_poll();
        if let Some(pending) = &mut self.pending_agents {
            if pending.automatic && pending.next.is_none() {
                pending.next = Some((reply, report));
                pending.cancel.store(true, Ordering::Release);
                return Ok(());
            }
        }
        anyhow::ensure!(
            self.pending_agents.is_none(),
            "Agent discovery is already running; try again"
        );
        self.start_agents(report, Some(reply))
    }

    pub(super) fn agents_tick(&mut self) {
        self.agents_poll();
        if self.pending_agents.is_none()
            && !self.closing
            && !self.close_accepted
            && self.last_agent_scan.elapsed() >= Duration::from_secs(1)
        {
            self.last_agent_scan = Instant::now();
            if let Err(error) = self.start_agents(None, None) {
                eprintln!("Agent discovery could not start: {error:#}");
            }
        }
    }

    fn start_agents(
        &mut self,
        report: Option<Report>,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<()> {
        let automatic = reply.is_none();
        let started = reply
            .as_ref()
            .map_or_else(Instant::now, ipc::Reply::received_at);
        anyhow::ensure!(
            started.elapsed() < Duration::from_secs(2),
            "Agent discovery expired before starting"
        );
        let mut jobs = Vec::new();
        for workspace in &self.workspaces {
            for (_, _, tabs) in workspace.leaves() {
                for tab in tabs {
                    if !matches!(tab.kind, SurfaceKind::Terminal { .. })
                        || report
                            .as_ref()
                            .is_some_and(|report| report.surface() != tab.id)
                        || (automatic && self.agent_presence(tab.id).is_some())
                    {
                        continue;
                    }
                    if let Some(surface) =
                        self.surfaces.get(&tab.id).filter(|s| s.exit_code.is_none())
                    {
                        if let Some(session) = &surface.session {
                            jobs.push((tab.id, surface.session_generation, session.process_job()));
                        }
                    }
                }
            }
        }
        anyhow::ensure!(
            report.is_none() || !jobs.is_empty(),
            "report target is not a running local terminal"
        );
        if automatic {
            if jobs.is_empty() {
                return Ok(());
            }
            jobs.sort_by_key(|(id, _, _)| id.0);
            let first = jobs.partition_point(|(id, _, _)| {
                self.agent_scan_after.is_some_and(|last| id.0 <= last.0)
            });
            let count = jobs.len();
            jobs.rotate_left(first % count);
            // ponytail: at most 16 uncached terminals per tick; the cursor keeps
            // unreadable/busy Jobs from starving later terminals.
            jobs.truncate(16);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let sender = self.sender.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("flowmux-agents".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<Discovery> {
                    let deadline = started + Duration::from_secs(2);
                    let mut entries = Vec::new();
                    let mut last = None;
                    for (surface, generation, job) in jobs {
                        if automatic
                            && (token.load(Ordering::Acquire) || Instant::now() >= deadline)
                        {
                            break;
                        }
                        last = Some(surface);
                        let found = match agent_process::discover(&job, &token, deadline) {
                            Ok(found) => found,
                            Err(_) if automatic => continue,
                            Err(error) => return Err(error),
                        };
                        if let Some(mut found) = found {
                            if let Some(process) = agent_process::live_handle(&job, &found) {
                                // Retain only the validated resume prefix and home, never raw prompts.
                                found.argv.clear();
                                entries.push(Entry {
                                    surface,
                                    generation,
                                    process,
                                    pid: found.pid,
                                    agent: found.agent,
                                    cwd: found.cwd.clone(),
                                    session_id: found.session_id.clone(),
                                    launch: found,
                                });
                            }
                        }
                    }
                    Ok(Discovery { entries, last })
                })()
                .map_err(|error| format!("{error:#}"));
                let _ = send.send(result);
                sender.send(Event::AgentsReady);
            })?;
        self.pending_agents = Some(Pending {
            reply,
            receiver,
            cancel,
            started,
            report,
            automatic,
            next: None,
        });
        Ok(())
    }

    pub(super) fn agents_poll(&mut self) {
        let mut completions = Vec::new();
        {
            let mut states = self.agent_states.borrow_mut();
            let before = states.len();
            states.retain(|_, state| state.entry.current(self));
            let mut changed = before != states.len();
            for (id, state) in states.iter_mut() {
                if let Some(activity) = &mut state.native {
                    if activity.settle(Instant::now()) {
                        let snapshot = activity.snapshot();
                        if let Some(body) = activity.take_completion() {
                            completions.push((*id, state.entry.agent.name(), body));
                        }
                        changed |= state.native_status(snapshot, self.source_is_focused(Some(*id)));
                    }
                }
                if self.source_is_focused(Some(*id)) {
                    changed |= state.presence.mark_seen();
                }
            }
            if changed {
                self.sender.send(Event::AgentChanged);
            }
        }
        for (surface, agent, body) in completions {
            self.add_agent_notification(
                surface,
                crate::notifications::agent_notice(
                    agent,
                    "ready",
                    &body,
                    flowmux_core::NotificationLevel::TurnCompleted,
                ),
            );
        }
        let Some(pending) = &mut self.pending_agents else {
            return;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Disconnected) => Err("Agent discovery worker stopped".into()),
            Err(mpsc::TryRecvError::Empty) => {
                if pending.next.as_ref().is_some_and(|(reply, _)| {
                    reply.received_at().elapsed() >= Duration::from_secs(3)
                }) {
                    let (reply, _) = pending.next.take().unwrap();
                    let _ = reply.try_send(json!({"error":"Agent discovery exceeded its three-second response budget"}));
                }
                if pending.started.elapsed() >= Duration::from_secs(3) {
                    pending.cancel.store(true, Ordering::Release);
                    if let Some(reply) = pending.reply.take() {
                        let _ = reply.try_send(json!({"error":"Agent discovery exceeded its three-second response budget"}));
                    }
                }
                // ponytail: one worker per window; a stalled native call keeps
                // admission closed until it returns, without blocking the UI.
                return;
            }
        };
        let mut pending = self.pending_agents.take().unwrap();
        if pending.automatic {
            if let Ok(found) = &result {
                self.agent_scan_after = found.last.or(self.agent_scan_after);
            }
            if let Some((reply, report)) = pending.next.take() {
                if let Err(error) = self.start_agents(report, Some(reply.clone())) {
                    let _ = reply.try_send(json!({"error":format!("{error:#}")}));
                }
            } else if !pending.cancel.load(Ordering::Acquire) {
                if let Ok(found) = result {
                    self.record_agents(found.entries);
                }
            }
            return;
        }
        let Some(reply) = pending.reply.take() else {
            return;
        };
        // A result queued behind a busy UI must not mutate status after the
        // reporting client has already exhausted its response budget.
        if pending.started.elapsed() >= Duration::from_secs(3) {
            let _ = reply.try_send(
                json!({"error":"Agent discovery exceeded its three-second response budget"}),
            );
            return;
        }
        let result = match result {
            Err(error) => json!({"error":error}),
            Ok(found) if pending.report.is_some() => {
                let report = pending.report.take().unwrap();
                let surface = report.surface();
                found
                    .entries
                    .into_iter()
                    .find(|entry| entry.surface == surface)
                    .context("no supported live agent in this terminal")
                    .and_then(|entry| match report {
                        Report::Activity(_, args) => {
                            self.apply_agent_report(surface, &args, Some(entry))
                        }
                        Report::Session(_, args) => {
                            self.apply_agent_hook(surface, &args, Some(entry), &reply)
                        }
                    })
                    .unwrap_or_else(|error| json!({"error":format!("{error:#}")}))
            }
            Ok(found) => {
                let result = Value::Array(found.entries.iter().filter_map(|entry| {
                if !entry.current(self) {
                    return None;
                }
                let (workspace, pane, _) = self.locate(entry.surface)?;
                let workspace = &self.workspaces[workspace];
                let presence = self.agent_presence(entry.surface).filter(|p| p.pid == Some(entry.pid) && p.name.eq_ignore_ascii_case(entry.agent.name()));
                // The first reply precedes record_agents. An existing presence
                // may deliberately clear its ID after a session-end hook.
                let session_id = presence.as_ref().map_or(entry.session_id.as_deref(), |p| p.session_id.as_deref());
                Some(json!({"workspace":workspace.name,"workspace_id":workspace.id,"root":workspace.cwd,
                    "pane":pane,"tab":entry.surface,"agent":entry.agent.name().to_ascii_lowercase(),"pid":entry.pid,"cwd":entry.cwd,
                    "status":presence.as_ref().map_or(AgentStatus::Unknown, |p|p.status),"message":presence.as_ref().and_then(|p|p.message.as_deref()),"session_id":session_id,"session_name":null,"messaging":false}))
                }).collect());
                self.record_agents(found.entries);
                result
            }
        };
        let _ = reply.try_send(result);
    }

    fn record_agents(&mut self, entries: Vec<Entry>) {
        let mut states = self.agent_states.borrow_mut();
        let mut changed = false;
        for entry in entries {
            if !entry.current(self)
                || states.get(&entry.surface).is_some_and(|state| {
                    state.entry.current(self)
                        && state.entry.pid == entry.pid
                        && state.entry.agent == entry.agent
                        && state.entry.generation == entry.generation
                })
            {
                continue;
            }
            // Direct agent launches are one-shot: a later restore or split
            // uses the ordinary shell, including after this agent has exited.
            if self.surfaces[&entry.surface]
                .session
                .as_ref()
                .is_some_and(|s| s.pid == entry.pid)
            {
                self.shells
                    .insert(entry.surface, self.settings.default_shell.clone());
            }
            let mut presence = AgentPresence::new(
                entry.agent.name().to_ascii_lowercase(),
                AgentStatus::Unknown.to_activity(),
                Some(entry.pid),
            );
            presence.status = AgentStatus::Unknown;
            presence.source = Some(AGENT_SOURCE_PROC.into());
            presence.session_id = entry.session_id.clone();
            states.insert(
                entry.surface,
                State {
                    entry,
                    presence,
                    session_reported: false,
                    last_hook: None,
                    native: None,
                },
            );
            changed = true;
        }
        if changed {
            self.sender.send(Event::AgentChanged);
        }
    }
}
