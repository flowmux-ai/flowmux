// SPDX-License-Identifier: GPL-3.0-or-later
//! A GUI management connection, independent of any terminal view.

use flowmux_ipc::agent_host::{
    AgentInstance, AgentViewReport, ClientRole, HostClient, HostRequest, HostResponse,
    IncompatibleHost,
};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;

const RPC_TIMEOUT: Duration = Duration::from_secs(1);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);

/// Use a normal GUI process so the detached view retains IPC and layout ownership.
pub async fn open_in_new_window(
    agent: AgentInstance,
    executable: PathBuf,
    activation: Option<(&'static str, String)>,
) -> anyhow::Result<()> {
    use flowmux_ipc::{
        client::Client,
        protocol::{Request, Response},
    };
    let expected_attachment = agent
        .attachment
        .as_ref()
        .filter(|attachment| attachment.ready)
        .map(|attachment| attachment.key)
        .ok_or_else(|| anyhow::anyhow!("The agent's current view is not ready"))?;
    let mut command = tokio::process::Command::new(executable);
    command
        .current_dir(&agent.cwd)
        .stdin(std::process::Stdio::null())
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID");
    if let Some((name, token)) = activation {
        command.env(name, token);
    }
    let mut child = command.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("New window exited before startup"))?;
    tokio::spawn(async move {
        if let Err(error) = child.wait().await {
            tracing::warn!(%error, pid, "could not reap detached Agent window");
        }
    });
    let socket = flowmux_config::paths::runtime_socket_for_pid(pid);
    tokio::time::timeout(flowmux_ipc::agent_host::ATTACH_TIMEOUT + Duration::from_secs(15), async {
        let gui = loop {
            match Client::connect(&socket).await {
                Ok(gui) => break gui,
                Err(error) => {
                    anyhow::ensure!(flowmux_procmon::pid_alive(pid), "New window exited: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        };
        let Response::WorkspaceCreated { id: workspace } = gui.call(Request::WorkspaceCreate {
            name: Some(agent.label), root: agent.cwd, team: false, background: false,
        }).await? else {
            anyhow::bail!("Could not create the Agent workspace in the new window");
        };
        let Response::Tree { workspaces } = gui.call(Request::WorkspaceTree).await? else {
            anyhow::bail!("Could not inspect the new window");
        };
        let pane = workspaces.iter().find(|candidate| candidate.id == workspace)
            .and_then(|workspace| workspace.panes.first())
            .ok_or_else(|| anyhow::anyhow!("New workspace has no terminal pane"))?.id;
        match gui.call(Request::AgentViewOpen {
            pane, instance: agent.instance, expected_attachment: Some(expected_attachment),
        }).await? {
            Response::SurfaceCreated { .. } => Ok(()),
            response => anyhow::bail!("Could not connect the new Agent view: {response:?}"),
        }
    }).await.map_err(|_| anyhow::anyhow!(
        "New window connection timed out; the move was not retried. Check Agents for the current location."
    ))?
}

pub async fn notification_request(request: HostRequest) -> anyhow::Result<HostResponse> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let client = HostClient::connect(
            &flowmux_config::paths::agent_host_socket(),
            ClientRole::Observer,
        )
        .await?;
        client.call(request).await
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Agent notification response timed out; outcome unknown; request was not retried"
        )
    })?
}

pub async fn lookup_running(
    socket: &Path,
    instance: flowmux_core::AgentInstanceId,
    epoch: Option<uuid::Uuid>,
) -> Result<AgentInstance, flowmux_ipc::RpcError> {
    tokio::time::timeout(Duration::from_secs(2), async {
        let client = HostClient::connect(socket, ClientRole::Observer).await?;
        let HostResponse::List {
            agents, terminals, ..
        } = client.call(HostRequest::List).await?
        else {
            anyhow::bail!("unexpected Agent Host list response");
        };
        agents
            .into_iter()
            .chain(terminals)
            .find(|agent| {
                agent.instance == instance
                    && epoch.is_none_or(|epoch| epoch == agent.epoch)
                    && agent.state == flowmux_ipc::agent_host::ProcessState::Running
            })
            .ok_or_else(|| anyhow::anyhow!("Agent is not running in this Host: {instance}"))
    })
    .await
    .map_err(|_| flowmux_ipc::RpcError::Busy("Agent Host lookup timed out".into()))?
    .map_err(|error| flowmux_ipc::RpcError::InvalidArgument(error.to_string()))
}

#[derive(Clone, Debug, Default)]
pub struct RegistrySnapshot {
    pub agents: Vec<AgentInstance>,
    pub available: bool,
    pub epoch: Option<uuid::Uuid>,
    pub other_hosts: Vec<PathBuf>,
    pub notifications: Vec<flowmux_ipc::agent_host::ManagedNotification>,
    /// A running Host this version cannot manage; cleared once it is gone.
    pub incompatible: Option<IncompatibleHost>,
}

/// True only when nothing listens at `socket`: no Host, so none of its executions.
/// A Host that is slow, busy or of another version is not absent.
pub fn host_absent(socket: &Path) -> bool {
    matches!(
        std::os::unix::net::UnixStream::connect(socket),
        Err(error) if matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )
    )
}

pub async fn other_live_hosts(current: &Path) -> Vec<PathBuf> {
    let mut live = Vec::new();
    for socket in flowmux_config::paths::other_agent_host_sockets(current) {
        let result =
            tokio::time::timeout(RPC_TIMEOUT, tokio::net::UnixStream::connect(&socket)).await;
        if !matches!(result, Ok(Err(error)) if matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
        )) {
            live.push(socket);
        }
    }
    live
}

/// Whether the Host runs `binding`. `None` when a Host is there but cannot be asked: its
/// executions may be running.
pub fn execution_alive(
    registry: &RegistrySnapshot,
    binding: flowmux_core::AgentViewBinding,
    absent: impl FnOnce() -> bool,
) -> Option<bool> {
    if registry.available && registry.epoch == Some(binding.epoch) {
        Some(registry.agents.iter().any(|agent| {
            agent.instance == binding.instance
                && agent.epoch == binding.epoch
                && agent.state == flowmux_ipc::agent_host::ProcessState::Running
        }))
    } else if registry.other_hosts.is_empty() {
        if registry.available {
            return Some(false);
        }
        absent().then_some(false)
    } else {
        None
    }
}

async fn connect(socket: &Path, minutes: u32) -> (Option<HostClient>, Option<IncompatibleHost>) {
    let result = tokio::time::timeout(RPC_TIMEOUT, async {
        let client = HostClient::connect(socket, ClientRole::Gui).await?;
        anyhow::ensure!(
            matches!(
                client.call(HostRequest::SetIdleTimeout { minutes }).await?,
                HostResponse::Ok
            ),
            "unexpected Agent Host settings response"
        );
        Ok::<_, anyhow::Error>(client)
    })
    .await;
    match result {
        Ok(Ok(client)) => (Some(client), None),
        Ok(Err(error)) => (None, error.downcast_ref::<IncompatibleHost>().cloned()),
        Err(_) => (None, None),
    }
}

/// Try the first management handshake before restoring managed tabs.
/// Reconnection only looks for an existing Host; list/settings changes must
/// never restart a normally retired empty Host.
pub async fn start(
    socket: PathBuf,
    mut minutes: watch::Receiver<u32>,
    mut views: watch::Receiver<Vec<AgentViewReport>>,
    registry: watch::Sender<RegistrySnapshot>,
    updates: async_channel::Sender<crate::bridge::GtkCommand>,
) -> tokio::task::JoinHandle<()> {
    let initial_minutes = *minutes.borrow_and_update();
    // GUI relays require a registered management connection before attaching.
    // A failed handshake leaves saved bindings disconnected, never resumed.
    let (mut client, mut incompatible) = connect(&socket, initial_minutes).await;
    let other_hosts = other_live_hosts(&socket).await;
    let mut others_changed = !other_hosts.is_empty();
    registry.send_modify(|snapshot| snapshot.other_hosts = other_hosts);
    tokio::spawn(async move {
        let mut scan_after = tokio::time::Instant::now() + RECONNECT_INTERVAL;
        let mut last_revision = None;
        let mut report_pending = true;
        loop {
            if tokio::time::Instant::now() >= scan_after {
                let other_hosts = other_live_hosts(&socket).await;
                others_changed |= registry.send_if_modified(|snapshot| {
                    if snapshot.other_hosts == other_hosts {
                        return false;
                    }
                    snapshot.other_hosts = other_hosts;
                    true
                });
                scan_after = tokio::time::Instant::now() + RECONNECT_INTERVAL;
            }
            if std::mem::take(&mut others_changed)
                && updates
                    .send(crate::bridge::GtkCommand::AgentRegistryChanged)
                    .await
                    .is_err()
            {
                return;
            }
            if let Some(connected) = client.as_ref() {
                if report_pending {
                    report_pending = false;
                    let reports = views.borrow_and_update().clone();
                    if !matches!(
                        tokio::time::timeout(
                            RPC_TIMEOUT,
                            connected.call(HostRequest::ReportViews { views: reports })
                        )
                        .await,
                        Ok(Ok(HostResponse::Ok))
                    ) {
                        drop(client.take());
                        continue;
                    }
                }
                match tokio::time::timeout(RPC_TIMEOUT, connected.call(HostRequest::List)).await {
                    Ok(Ok(HostResponse::List {
                        mut agents,
                        epoch,
                        revision,
                        notifications,
                        terminals,
                    })) => {
                        agents.extend(terminals);
                        if last_revision != Some((epoch, revision)) || !registry.borrow().available
                        {
                            tracing::debug!(%epoch, revision, agents = agents.len(), "Agent Host registry updated");
                            // A terminal's shell is listed as the agent running in it.
                            let mut agents = agents;
                            for agent in &mut agents {
                                if let Some(presence) =
                                    agent.presence.as_ref().filter(|_| agent.shell)
                                {
                                    agent.provider = presence.name.clone();
                                }
                            }
                            registry.send_modify(|snapshot| {
                                snapshot.agents = agents;
                                snapshot.available = true;
                                snapshot.epoch = Some(epoch);
                                snapshot.notifications = notifications;
                                snapshot.incompatible = None;
                            });
                            last_revision = Some((epoch, revision));
                            if updates
                                .send(crate::bridge::GtkCommand::AgentRegistryChanged)
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                    _ => {
                        drop(client.take());
                    }
                }
            }
            if client.is_none() {
                let changed = registry.send_if_modified(|snapshot| {
                    // Keep the last observation; loss of contact is not process death.
                    let was_available = std::mem::replace(&mut snapshot.available, false);
                    let notice_changed = snapshot.incompatible != incompatible;
                    snapshot.incompatible.clone_from(&incompatible);
                    was_available || notice_changed
                });
                if changed
                    && updates
                        .send(crate::bridge::GtkCommand::AgentRegistryChanged)
                        .await
                        .is_err()
                {
                    return;
                }
            }
            let request = tokio::select! {
                changed = minutes.changed() => {
                    if changed.is_err() { return; }
                    let minutes = *minutes.borrow_and_update();
                    HostRequest::SetIdleTimeout { minutes }
                }
                changed = views.changed() => {
                    if changed.is_err() { return; }
                    report_pending = true;
                    HostRequest::KeepAlive
                }
                _ = tokio::time::sleep(RECONNECT_INTERVAL) => HostRequest::KeepAlive,
            };
            if let Some(connected) = client.as_ref() {
                if matches!(
                    tokio::time::timeout(RPC_TIMEOUT, connected.call(request)).await,
                    Ok(Ok(HostResponse::Ok))
                ) {
                    continue;
                }
                // A cancelled RPC may have a partial response; discard this
                // stream rather than reuse or replay an ambiguous request.
                tracing::warn!("Agent Host management connection lost; reconnecting");
                drop(client.take());
            }
            let current_minutes = *minutes.borrow_and_update();
            (client, incompatible) = connect(&socket, current_minutes).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_ipc::protocol::{Envelope, Payload, Request, Response};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn management_handshake_precedes_restore_and_settings_keep_same_connection() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let agent = AgentInstance {
            instance: flowmux_core::AgentInstanceId::new(),
            epoch: uuid::Uuid::new_v4(),
            provider: "codex".into(),
            label: "fixture".into(),
            cwd: "/tmp".into(),
            pid: Some(42),
            process_start: Some(1),
            boot_id: None,
            state: flowmux_ipc::agent_host::ProcessState::Running,
            force_terminate_allowed: false,
            exit_status: None,
            view: None,
            attachment: None,
            view_owner: None,
            error: None,
            presence: None,
            shell: false,
        };
        let observed_agent = agent.clone();
        let (disconnect, mut disconnect_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            loop {
                let line = tokio::select! {
                    _ = &mut disconnect_rx => break,
                    line = lines.next_line() => match line.unwrap() { Some(line) => line, None => break },
                };
                let envelope: Envelope = serde_json::from_str(&line).unwrap();
                let Payload::Request(Request::AgentHost { request }) = envelope.payload else {
                    panic!("unexpected management request");
                };
                let response = if matches!(request, HostRequest::Hello { .. }) {
                    HostResponse::Hello {
                        version: flowmux_ipc::agent_host::PROTOCOL_VERSION,
                        epoch: uuid::Uuid::new_v4(),
                        connection: 1,
                    }
                } else if matches!(request, HostRequest::List) {
                    HostResponse::List {
                        agents: vec![observed_agent.clone()],
                        epoch: observed_agent.epoch,
                        revision: 1,
                        notifications: Vec::new(),
                        terminals: Vec::new(),
                    }
                } else {
                    HostResponse::Ok
                };
                events.send(request).unwrap();
                let reply = Envelope {
                    id: envelope.id,
                    payload: Payload::Response(Response::AgentHost { response }),
                };
                write
                    .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
                    .await
                    .unwrap();
            }
        });
        let (settings, receiver) = watch::channel(30);
        let (updates, rx) = async_channel::unbounded();
        let (registry, _) = watch::channel(RegistrySnapshot::default());
        let (views, view_rx) = watch::channel(Vec::new());
        let task = start(socket, receiver, view_rx, registry.clone(), updates).await;
        assert!(matches!(
            received.try_recv().unwrap(),
            HostRequest::Hello {
                role: ClientRole::Gui,
                ..
            }
        ));
        assert!(matches!(
            received.try_recv().unwrap(),
            HostRequest::SetIdleTimeout { minutes: 30 }
        ));
        settings.send_replace(45);
        assert!(matches!(
            received.recv().await.unwrap(),
            HostRequest::ReportViews { .. }
        ));
        assert!(matches!(received.recv().await.unwrap(), HostRequest::List));
        assert!(matches!(
            tokio::time::timeout(RPC_TIMEOUT, received.recv())
                .await
                .unwrap()
                .unwrap(),
            HostRequest::SetIdleTimeout { minutes: 45 }
        ));
        assert!(matches!(
            rx.recv().await.unwrap(),
            crate::bridge::GtkCommand::AgentRegistryChanged
        ));
        assert!(registry.borrow().available);
        assert_eq!(registry.borrow().agents[0].instance, agent.instance);
        assert!(
            rx.try_recv().is_err(),
            "unchanged revision must not enqueue another refresh"
        );
        let report = AgentViewReport {
            key: flowmux_ipc::agent_host::AttachmentKey {
                instance: agent.instance,
                epoch: agent.epoch,
                lease: uuid::Uuid::new_v4(),
                generation: 1,
            },
            location: flowmux_ipc::agent_host::AgentViewLocation {
                window: uuid::Uuid::new_v4(),
                workspace: flowmux_core::WorkspaceId::new(),
                pane: flowmux_core::PaneId::new(),
                surface: flowmux_core::SurfaceId::new(),
                visible: true,
            },
        };
        views.send_replace(vec![report.clone()]);
        tokio::time::timeout(RPC_TIMEOUT, async {
            loop {
                if let HostRequest::ReportViews { views } = received.recv().await.unwrap() {
                    if views == vec![report.clone()] {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        assert!(matches!(received.recv().await.unwrap(), HostRequest::List));
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let request = received.recv().await.unwrap();
                assert!(
                    !matches!(request, HostRequest::ReportViews { .. }),
                    "cached views must not be renewed without GTK observation"
                );
                if matches!(request, HostRequest::List) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        disconnect.send(()).unwrap();
        server.await.unwrap();
        settings.send_replace(46);
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!registry.borrow().available);
        assert_eq!(
            registry.borrow().agents[0].instance,
            agent.instance,
            "disconnect must retain the last observation"
        );
        drop(settings);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn incompatible_host_notice_is_published_without_rows_and_clears_when_gone() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await.unwrap();
                let envelope: Envelope = serde_json::from_str(&line).unwrap();
                assert!(matches!(
                    envelope.payload,
                    Payload::Request(Request::AgentHost {
                        request: HostRequest::Hello { .. }
                    })
                ));
                // The exact rejection every Host before the versioned one sends.
                let reply = Envelope {
                    id: envelope.id,
                    payload: Payload::Response(Response::Error(
                        flowmux_ipc::RpcError::InvalidArgument(
                            "incompatible Agent Host protocol".into(),
                        ),
                    )),
                };
                write
                    .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
                    .await
                    .unwrap();
            }
        });
        let (settings, receiver) = watch::channel(30);
        let (updates, rx) = async_channel::unbounded();
        let (registry, _) = watch::channel(RegistrySnapshot::default());
        let (_views, view_rx) = watch::channel(Vec::new());
        let task = start(socket.clone(), receiver, view_rx, registry.clone(), updates).await;
        tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        {
            let snapshot = registry.borrow();
            assert!(!snapshot.available && snapshot.agents.is_empty());
            assert_eq!(
                snapshot.incompatible.as_ref().map(|host| host.host_version),
                Some(None)
            );
        }
        server.abort();
        let _ = server.await;
        std::fs::remove_file(&socket).unwrap();
        tokio::time::timeout(RECONNECT_INTERVAL * 3, rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(registry.borrow().incompatible.is_none());
        drop(settings);
        task.await.unwrap();
    }

    #[test]
    fn only_a_socket_nobody_answers_counts_as_an_absent_host() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let socket = root.path().join("host.sock");
        assert!(host_absent(&socket), "missing socket");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert!(!host_absent(&socket), "listening Host");
        drop(listener);
        assert!(host_absent(&socket), "stale socket file");
    }

    #[test]
    fn a_host_that_cannot_be_asked_leaves_its_executions_unknown() {
        let binding = flowmux_core::AgentViewBinding {
            instance: flowmux_core::AgentInstanceId::new(),
            epoch: uuid::Uuid::new_v4(),
        };
        let mut registry = RegistrySnapshot::default();
        assert_eq!(execution_alive(&registry, binding, || true), Some(false));
        assert_eq!(execution_alive(&registry, binding, || false), None);
        registry.available = true;
        registry.epoch = Some(binding.epoch);
        let never = || -> bool { panic!("a Host that answered is not probed") };
        assert_eq!(execution_alive(&registry, binding, never), Some(false));
        registry.other_hosts.push("other.sock".into());
        assert_eq!(execution_alive(&registry, binding, never), Some(false));
        registry.epoch = Some(uuid::Uuid::new_v4());
        assert_eq!(execution_alive(&registry, binding, never), None);
    }

    #[tokio::test]
    async fn absent_host_is_not_started_by_gui_connection_or_settings() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("absent.sock");
        let (settings, receiver) = watch::channel(30);
        let (updates, _rx) = async_channel::unbounded();
        let (_views, view_rx) = watch::channel(Vec::new());
        let task = start(
            socket.clone(),
            receiver,
            view_rx,
            watch::channel(RegistrySnapshot::default()).0,
            updates,
        )
        .await;
        settings.send_replace(45);
        drop(settings);
        task.await.unwrap();
        assert!(!socket.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
