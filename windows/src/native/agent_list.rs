// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned-process discovery and runtime-only, ordered activity snapshots.
use super::super::agent_process;
use super::*;
use crate::command::AgentReportArgs;
use flowmux_core::{AgentPresence, AgentStatus, AgentStatusReport};
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
}
pub(super) struct State {
    entry: Entry,
    presence: AgentPresence,
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
    receiver: Receiver<Result<Vec<Entry>, String>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
    report: Option<(SurfaceId, AgentReportArgs)>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
        if let Some(reply) = self.reply.take() {
            let _ =
                reply.try_send(json!({"error":"Window closed before agent discovery finished"}));
        }
    }
}
impl App {
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
            state.presence.mark_seen();
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
        self.agents_request(Some((surface, args)), reply)?;
        Ok(None)
    }

    fn apply_agent_report(
        &self,
        surface: SurfaceId,
        args: &AgentReportArgs,
        entry: Option<Entry>,
    ) -> anyhow::Result<Value> {
        let status: AgentStatus = serde_json::from_value(json!(args.status))?;
        let mut report = AgentStatusReport::from_activity(&args.agent, None, Some(args.pid));
        report.status = Some(status);
        report.source = Some("flowmux:report".into());
        report.seq = Some(args.seq);
        report.message = args.message.clone();
        let visible = self.source_is_focused(Some(surface));
        let accepted = {
            let mut states = self.agent_states.borrow_mut();
            if let Some(entry) = entry {
                anyhow::ensure!(
                    entry.current(self)
                        && entry.pid == args.pid
                        && entry.agent.name().eq_ignore_ascii_case(&args.agent),
                    "report does not match the live agent in this terminal"
                );
                let presence =
                    AgentPresence::from_report(report, visible).context("missing agent status")?;
                states.insert(surface, State { entry, presence });
                true
            } else {
                let state = states
                    .get_mut(&surface)
                    .context("agent process is no longer available")?;
                anyhow::ensure!(
                    state.entry.current(self)
                        && state.entry.pid == args.pid
                        && state.presence.name == args.agent,
                    "report does not match the live agent in this terminal"
                );
                state.presence.apply_report(report, visible)
            }
        };
        Ok(json!({"accepted":accepted,"surface":surface,"agent":self.agent_presence(surface)}))
    }

    pub(super) fn agents_request(
        &mut self,
        report: Option<(SurfaceId, AgentReportArgs)>,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        self.agents_poll();
        anyhow::ensure!(
            self.pending_agents.is_none(),
            "Agent discovery is already running; try again"
        );
        let mut jobs = Vec::new();
        for workspace in &self.workspaces {
            for (_, _, tabs) in workspace.leaves() {
                for tab in tabs {
                    if !matches!(tab.kind, SurfaceKind::Terminal { .. })
                        || report.as_ref().is_some_and(|(id, _)| *id != tab.id)
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
        let started = reply.received_at();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let sender = self.sender.clone();
        let (send, receiver) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("flowmux-agents".into())
            .spawn(move || {
                let result = (|| -> anyhow::Result<Vec<Entry>> {
                    let deadline = started + Duration::from_secs(2);
                    let mut entries = Vec::new();
                    for (surface, generation, job) in jobs {
                        if let Some(found) = agent_process::discover(&job, &token, deadline)? {
                            if let Some(process) = agent_process::live_handle(&job, &found) {
                                entries.push(Entry {
                                    surface,
                                    generation,
                                    process,
                                    pid: found.pid,
                                    agent: found.agent,
                                    cwd: found.cwd,
                                });
                            }
                        }
                    }
                    Ok(entries)
                })()
                .map_err(|error| format!("{error:#}"));
                let _ = send.send(result);
                sender.send(Event::AgentsReady);
            })?;
        self.pending_agents = Some(Pending {
            reply: Some(reply),
            receiver,
            cancel,
            started,
            report,
        });
        Ok(())
    }

    pub(super) fn agents_poll(&mut self) {
        {
            let mut states = self.agent_states.borrow_mut();
            states.retain(|_, state| state.entry.current(self));
            for (id, state) in states.iter_mut() {
                if self.source_is_focused(Some(*id)) {
                    state.presence.mark_seen();
                }
            }
        }
        let Some(pending) = &mut self.pending_agents else {
            return;
        };
        let result = match pending.receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Disconnected) => Err("Agent discovery worker stopped".into()),
            Err(mpsc::TryRecvError::Empty) => {
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
            Ok(entries) if pending.report.is_some() => {
                let (surface, args) = pending.report.take().unwrap();
                entries.into_iter().find(|entry| entry.surface == surface)
                    .context("no supported live agent in this terminal")
                    .and_then(|entry| self.apply_agent_report(surface, &args, Some(entry)))
                    .unwrap_or_else(|error| json!({"error":format!("{error:#}")}))
            }
            Ok(entries) => Value::Array(entries.into_iter().filter_map(|entry| {
                if !entry.current(self) {
                    return None;
                }
                let (workspace, pane, _) = self.locate(entry.surface)?;
                let workspace = &self.workspaces[workspace];
                let presence = self.agent_presence(entry.surface).filter(|p| p.pid == Some(entry.pid) && p.name.eq_ignore_ascii_case(entry.agent.name()));
                Some(json!({"workspace":workspace.name,"workspace_id":workspace.id,"root":workspace.cwd,
                    "pane":pane,"tab":entry.surface,"agent":entry.agent.name().to_ascii_lowercase(),"pid":entry.pid,"cwd":entry.cwd,
                    "status":presence.as_ref().map_or(AgentStatus::Unknown, |p|p.status),"message":presence.as_ref().and_then(|p|p.message.as_deref()),"session_name":null,"messaging":false}))
            }).collect()),
        };
        let _ = reply.try_send(result);
    }
}
