// SPDX-License-Identifier: GPL-3.0-or-later
//! On-demand discovery of local agents; only owned terminal Jobs are inspected.
use super::super::agent_process;
use super::*;
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
pub(super) struct Pending {
    reply: Option<ipc::Reply>,
    receiver: Receiver<Result<Vec<Entry>, String>>,
    cancel: Arc<AtomicBool>,
    started: Instant,
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
    pub(super) fn agents_request(&mut self, reply: ipc::Reply) -> anyhow::Result<()> {
        self.agents_poll();
        anyhow::ensure!(
            self.pending_agents.is_none(),
            "Agent discovery is already running; try again"
        );
        let mut jobs = Vec::new();
        for workspace in &self.workspaces {
            for (_, _, tabs) in workspace.leaves() {
                for tab in tabs {
                    if !matches!(tab.kind, SurfaceKind::Terminal { .. }) {
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
        });
        Ok(())
    }

    pub(super) fn agents_poll(&mut self) {
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
        let result = match result {
            Err(error) => json!({"error":error}),
            Ok(entries) => Value::Array(entries.into_iter().filter_map(|entry| {
                let surface = self.surfaces.get(&entry.surface)?;
                if surface.session_generation != entry.generation || surface.exit_code.is_some()
                    || surface.session.is_none()
                    || unsafe { WaitForSingleObject(entry.process.as_raw_handle(), 0) } != WAIT_TIMEOUT {
                    return None;
                }
                let (workspace, pane, _) = self.locate(entry.surface)?;
                let workspace = &self.workspaces[workspace];
                Some(json!({"workspace":workspace.name,"workspace_id":workspace.id,"root":workspace.cwd,
                    "pane":pane,"tab":entry.surface,"agent":entry.agent.name().to_ascii_lowercase(),"pid":entry.pid,"cwd":entry.cwd,
                    "status":"unknown","session_name":null,"messaging":false}))
            }).collect()),
        };
        let _ = reply.try_send(result);
    }
}
