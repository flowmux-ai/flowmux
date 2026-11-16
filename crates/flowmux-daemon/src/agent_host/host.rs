// SPDX-License-Identifier: GPL-3.0-or-later
//! Process/PTY ownership is independent of both GUI and terminal connections.

use super::retirement::RetirementPolicy;
use super::terminal::Terminal;
use crate::state_store::{AgentLifecycleRuntime, CodexGraceSettlement, LifecycleTransition};
use flowmux_core::{AgentInstanceId, AgentPresence};
use flowmux_ipc::agent_host::*;
use flowmux_ipc::server::{ConnectionContext, Handler};
use flowmux_ipc::{Request, Response, RpcError};
use flowmux_terminal::pty::Pty;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::unix::AsyncFd;
use tokio::sync::{mpsc, oneshot, Notify};
use uuid::Uuid;

const MAX_INSTANCES: usize = 32;
const MAX_ENDED_INSTANCES: usize = 64;
const MAX_INPUT_PENDING: usize = 256 * 1024;
const FRAME_CHUNK: usize = 64 * 1024;
const ATTACH_GRACE: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(5);
const VIEW_REPORT_TIMEOUT: Duration = Duration::from_secs(6);
const MAX_NOTIFICATIONS: usize = 50;
const MAX_NOTIFICATION_LOG_BYTES: usize = 128 * 1024;

type Result<T> = std::result::Result<T, RpcError>;

struct Entry {
    attachment_gate: Arc<tokio::sync::Mutex<()>>,
    agent: AgentInstance,
    commands: Option<mpsc::Sender<Command>>,
    final_frame: Option<(AttachmentKey, Transfer, Option<Transfer>)>,
    attachment_connection: Option<u64>,
    view_report: Option<(u64, Instant)>,
}

impl Entry {
    fn is_reaped(&self) -> bool {
        matches!(
            self.agent.state,
            ProcessState::Exited | ProcessState::Failed
        ) && self.final_frame.is_none()
    }
}

struct Peer {
    identity: Option<(u32, u64)>,
    parent: Option<(u32, u64)>,
    grandparent: Option<(u32, u64)>,
}

struct Registry {
    connections: HashMap<u64, ClientRole>,
    peers: HashMap<u64, Peer>,
    gui_list_revision: HashMap<u64, u64>,
    entries: HashMap<AgentInstanceId, Entry>,
    launches: HashMap<Uuid, ([u8; 32], AgentInstanceId)>,
    expired_launch_ms: Option<u64>,
    revision: u64,
    policy: RetirementPolicy,
    stopping: bool,
    accepted_launches: usize,
    launched: bool,
    empty_since: Option<Instant>,
    notifications: VecDeque<(ManagedNotification, bool)>,
}

impl Registry {
    fn prune_ended_launches(&mut self) {
        while self
            .entries
            .values()
            .filter(|entry| entry.is_reaped())
            .count()
            > MAX_ENDED_INSTANCES
        {
            let oldest = self
                .launches
                .iter()
                .filter(|(_, (_, instance))| self.entries[instance].is_reaped())
                .map(|(id, _)| launch_millis(*id))
                .min()
                .expect("reaped entries have launch identities");
            self.expired_launch_ms = Some(self.expired_launch_ms.unwrap_or(0).max(oldest));
            self.launches.retain(|id, (_, instance)| {
                if self.entries[instance].is_reaped() && launch_millis(*id) <= oldest {
                    self.entries.remove(instance);
                    false
                } else {
                    true
                }
            });
            self.revision += 1;
        }
    }

    fn require_execution_caller(
        &self,
        connection: u64,
        instance: AgentInstanceId,
    ) -> Result<&Entry> {
        let entry = self
            .entries
            .get(&instance)
            .ok_or_else(|| RpcError::NotFound("Agent is not in this Host".into()))?;
        let peer = self.peers.get(&connection).and_then(|peer| peer.identity);
        let root = entry.agent.pid.zip(entry.agent.process_start);
        if !matches!(
            entry.agent.state,
            ProcessState::Running | ProcessState::Stopping
        ) || !peer.zip(root).is_some_and(|((pid, birth), (root, start))| {
            flowmux_procmon::process_start_time(pid) == Some(birth)
                && process_belongs_to_root(pid, root, start)
        }) {
            return Err(RpcError::InvalidArgument(
                "managed context caller does not belong to this execution".into(),
            ));
        }
        Ok(entry)
    }

    fn require_notification_gui(&self, connection: u64) -> Result<()> {
        let identity = self.peers.get(&connection).and_then(|peer| peer.identity);
        if identity.is_some_and(|(pid, start)| {
            flowmux_procmon::process_start_time(pid) == Some(start)
                && self.connections.iter().any(|(id, role)| {
                    *role == ClientRole::Gui
                        && self.peers.get(id).and_then(|peer| peer.identity) == identity
                })
        }) {
            Ok(())
        } else {
            Err(RpcError::InvalidArgument(
                "notification changes require a registered GUI process".into(),
            ))
        }
    }

    fn push_notification(&mut self, mut notification: ManagedNotification) {
        if self
            .notifications
            .iter()
            .any(|(n, _)| n.notification.id == notification.notification.id)
        {
            return;
        }
        // Keep the bounded log below the IPC frame limit, including escaped control text.
        for (text, limit) in [
            (&mut notification.notification.title, 512),
            (&mut notification.notification.body, 4096),
        ] {
            if let Some((end, _)) = text.char_indices().nth(limit) {
                text.truncate(end);
                text.push('…');
            }
        }
        let bytes = serde_json::to_vec(&notification)
            .expect("notification serialization")
            .len();
        let mut retained_bytes: usize = self
            .notifications
            .iter()
            .map(|(n, _)| {
                serde_json::to_vec(n)
                    .expect("notification serialization")
                    .len()
            })
            .sum();
        while self.notifications.len() >= MAX_NOTIFICATIONS
            || retained_bytes + bytes > MAX_NOTIFICATION_LOG_BYTES
        {
            let oldest = self
                .notifications
                .iter()
                .position(|(n, _)| n.notification.read)
                .unwrap_or(0);
            let (old, _) = self
                .notifications
                .remove(oldest)
                .expect("bounded notification queue");
            retained_bytes -= serde_json::to_vec(&old)
                .expect("notification serialization")
                .len();
        }
        self.notifications.push_back((notification, false));
        self.revision += 1;
    }

    fn expire_views(&mut self, now: Instant) {
        let mut changed = false;
        for entry in self.entries.values_mut() {
            if entry
                .view_report
                .is_some_and(|(_, at)| now.duration_since(at) >= VIEW_REPORT_TIMEOUT)
            {
                entry.agent.view = None;
                entry.view_report = None;
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
        }
    }

    fn report_views(
        &mut self,
        connection: u64,
        views: Vec<AgentViewReport>,
        now: Instant,
    ) -> Result<()> {
        if self.connections.get(&connection) != Some(&ClientRole::Gui) {
            return Err(RpcError::InvalidArgument(
                "only a GUI may report view locations".into(),
            ));
        }
        let mut instances = std::collections::HashSet::new();
        if views.len() > MAX_INSTANCES
            || views.iter().any(|view| {
                !instances.insert(view.key.instance)
                    || view.location.window.is_nil()
                    || view.location.workspace.0.is_nil()
                    || view.location.pane.0.is_nil()
                    || view.location.surface.0.is_nil()
            })
        {
            return Err(RpcError::InvalidArgument(
                "invalid agent view snapshot".into(),
            ));
        }
        let gui = self.peers.get(&connection).and_then(|peer| peer.identity);
        for view in &views {
            let Some(entry) = self.entries.get(&view.key.instance) else {
                continue;
            };
            if entry
                .agent
                .attachment
                .as_ref()
                .is_none_or(|attachment| attachment.key != view.key)
            {
                continue;
            }
            let peer = entry
                .attachment_connection
                .and_then(|id| self.peers.get(&id));
            if gui.is_none()
                || !peer.is_some_and(|peer| {
                    peer.parent == gui || peer.grandparent == gui || peer.identity == gui
                })
            {
                return Err(RpcError::InvalidArgument(
                    "view relay does not belong to this GUI".into(),
                ));
            }
        }
        let mut changed = false;
        for entry in self.entries.values_mut() {
            let report = entry
                .agent
                .attachment
                .as_ref()
                .filter(|attachment| attachment.ready)
                .and_then(|attachment| views.iter().find(|view| view.key == attachment.key));
            if let Some(report) = report {
                let owner = AgentViewOwner {
                    key: report.key,
                    surface: report.location.surface,
                };
                if entry.agent.view_owner != Some(owner) {
                    entry.agent.view_owner = Some(owner);
                    changed = true;
                }
                if entry.agent.view.as_ref() != Some(&report.location) {
                    entry.agent.view = Some(report.location.clone());
                    changed = true;
                }
                entry.view_report = Some((connection, now));
            } else if entry
                .view_report
                .is_some_and(|(owner, _)| owner == connection)
            {
                entry.agent.view = None;
                entry.view_report = None;
                changed = true;
            }
        }
        if changed {
            self.revision += 1;
        }
        Ok(())
    }
}

pub struct AgentHost {
    epoch: Uuid,
    socket: PathBuf,
    registry: Mutex<Registry>,
    shutdown: Notify,
}

impl AgentHost {
    pub fn new(socket: PathBuf, idle_timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            epoch: Uuid::new_v4(),
            socket,
            registry: Mutex::new(Registry {
                connections: HashMap::new(),
                peers: HashMap::new(),
                gui_list_revision: HashMap::new(),
                entries: HashMap::new(),
                launches: HashMap::new(),
                expired_launch_ms: None,
                revision: 0,
                policy: RetirementPolicy::new(idle_timeout, Instant::now()),
                stopping: false,
                accepted_launches: 0,
                launched: false,
                empty_since: None,
                notifications: VecDeque::new(),
            }),
            shutdown: Notify::new(),
        })
    }

    fn registry(&self) -> MutexGuard<'_, Registry> {
        self.registry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    async fn monitor(self: Arc<Self>) {
        let started = Instant::now();
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        loop {
            tick.tick().await;
            let mut registry = self.registry();
            registry.expire_views(Instant::now());
            let empty = registry.accepted_launches == 0
                && registry
                    .entries
                    .values()
                    .all(|entry| entry.commands.is_none());
            if empty {
                let now = Instant::now();
                let empty_since = *registry.empty_since.get_or_insert(now);
                let transferring_final_frame = registry
                    .entries
                    .values()
                    .any(|entry| entry.final_frame.is_some());
                let pending_gui_snapshot = registry.connections.iter().any(|(connection, role)| {
                    *role == ClientRole::Gui
                        && registry
                            .gui_list_revision
                            .get(connection)
                            .is_none_or(|seen| *seen < registry.revision)
                });
                // Initial admission is bounded; retirement never restarts an empty Host.
                if (registry.launched
                    && (!(transferring_final_frame || pending_gui_snapshot)
                        || now.duration_since(empty_since) >= STOP_GRACE)
                    && now.duration_since(empty_since) >= Duration::from_millis(250))
                    || (!registry.launched
                        && now.duration_since(started) >= Duration::from_secs(10))
                {
                    registry.stopping = true;
                    self.shutdown.notify_one();
                    return;
                }
            } else {
                registry.empty_since = None;
            }
        }
    }

    fn require_connection(&self, connection: u64, request: &HostRequest) -> Result<()> {
        match self.registry().connections.get(&connection) {
            Some(ClientRole::Gui | ClientRole::Relay) => Ok(()),
            Some(ClientRole::Observer)
                if request.is_query()
                    || matches!(
                        request,
                        HostRequest::DispatchContext { .. }
                            | HostRequest::UpdateNotifications { .. }
                            | HostRequest::ClaimNotification { .. }
                            | HostRequest::Notify { .. }
                    ) =>
            {
                Ok(())
            }
            Some(ClientRole::Hook)
                if matches!(
                    request,
                    HostRequest::Lifecycle { .. } | HostRequest::NativeSession { .. }
                ) =>
            {
                Ok(())
            }
            _ => Err(RpcError::InvalidArgument(
                "Host handshake required for this connection".into(),
            )),
        }
    }

    async fn request(
        self: &Arc<Self>,
        request: HostRequest,
        connection: u64,
        peer_pid: Option<u32>,
    ) -> Result<HostResponse> {
        if let HostRequest::Hello { version, role } = request {
            if version != PROTOCOL_VERSION {
                return Err(RpcError::InvalidArgument(
                    "incompatible Agent Host protocol".into(),
                ));
            }
            let mut registry = self.registry();
            if registry.stopping {
                return Err(RpcError::Busy(
                    "Agent Host is stopping; request not started".into(),
                ));
            }
            if let Some(previous) = registry.connections.get(&connection) {
                if *previous != role {
                    return Err(RpcError::InvalidArgument(
                        "connection role cannot change".into(),
                    ));
                }
            } else {
                registry.connections.insert(connection, role);
                let identity = peer_pid.and_then(|pid| {
                    flowmux_procmon::process_start_time(pid).map(|start| (pid, start))
                });
                let parent = peer_pid
                    .and_then(flowmux_procmon::parent_pid)
                    .and_then(|pid| {
                        flowmux_procmon::process_start_time(pid).map(|start| (pid, start))
                    });
                // GUI terminals retain pty-tee between the widget and relay.
                let grandparent = parent
                    .and_then(|(pid, _)| flowmux_procmon::parent_pid(pid))
                    .and_then(|pid| {
                        flowmux_procmon::process_start_time(pid).map(|start| (pid, start))
                    });
                registry.peers.insert(
                    connection,
                    Peer {
                        identity,
                        parent,
                        grandparent,
                    },
                );
                if role == ClientRole::Gui {
                    registry.policy.gui_connected(connection);
                }
            }
            return Ok(HostResponse::Hello {
                version,
                epoch: self.epoch,
                connection,
            });
        }
        self.require_connection(connection, &request)?;
        match request {
            HostRequest::Hello { .. } => unreachable!(),
            HostRequest::KeepAlive => Ok(HostResponse::Ok),
            HostRequest::List => {
                let mut registry = self.registry();
                if registry.connections.get(&connection) == Some(&ClientRole::Gui) {
                    let revision = registry.revision;
                    registry.gui_list_revision.insert(connection, revision);
                }
                Ok(HostResponse::List {
                    epoch: self.epoch,
                    revision: registry.revision,
                    agents: registry
                        .entries
                        .values()
                        .map(|entry| entry.agent.clone())
                        .collect(),
                    notifications: registry
                        .notifications
                        .iter()
                        .map(|(n, _)| n.clone())
                        .collect(),
                })
            }
            HostRequest::Notify {
                instance,
                epoch,
                notification,
            } => {
                if epoch != self.epoch {
                    return Err(RpcError::InvalidArgument(
                        "invalid notification epoch".into(),
                    ));
                }
                let mut registry = self.registry();
                let entry = registry.require_execution_caller(connection, instance)?;
                let notification = ManagedNotification {
                    instance,
                    epoch,
                    provider: entry.agent.provider.clone(),
                    label: entry.agent.label.clone(),
                    notification: flowmux_core::Notification {
                        id: flowmux_core::NotificationId::new(),
                        title: notification.title,
                        body: notification.body,
                        level: notification.level,
                        source_pane: None,
                        created_at: chrono::Utc::now(),
                        read: false,
                    },
                };
                registry.push_notification(notification);
                Ok(HostResponse::Ok)
            }
            HostRequest::UpdateNotifications { epoch, ids, action } => {
                let mut registry = self.registry();
                registry.require_notification_gui(connection)?;
                if epoch != self.epoch || ids.len() > MAX_NOTIFICATIONS {
                    return Err(RpcError::InvalidArgument(
                        "invalid notification update identity or size".into(),
                    ));
                }
                match action {
                    NotificationAction::Read => {
                        for (notification, _) in &mut registry.notifications {
                            if ids.contains(&notification.notification.id) {
                                notification.notification.read = true;
                            }
                        }
                    }
                    NotificationAction::Dismiss => registry
                        .notifications
                        .retain(|(n, _)| !ids.contains(&n.notification.id)),
                }
                registry.revision += 1;
                Ok(HostResponse::Ok)
            }
            HostRequest::ClaimNotification { epoch, id } => {
                let mut registry = self.registry();
                registry.require_notification_gui(connection)?;
                if epoch != self.epoch {
                    return Err(RpcError::InvalidArgument(
                        "invalid notification epoch".into(),
                    ));
                }
                let claimed = registry
                    .notifications
                    .iter_mut()
                    .find(|(n, _)| n.notification.id == id && !n.notification.read)
                    .is_some_and(|(_, claimed)| !std::mem::replace(claimed, true));
                Ok(HostResponse::NotificationClaimed { claimed })
            }
            HostRequest::Launch { launch } => self.launch(launch).await,
            HostRequest::ResolveContext {
                instance,
                epoch,
                session_id,
            } => self.resolve_context(connection, instance, epoch, session_id.as_deref()),
            HostRequest::DispatchContext {
                context,
                session_id,
                request,
            } => {
                if !request.supports_agent_context() {
                    return Err(RpcError::InvalidArgument(
                        "operation does not support managed context; request not started".into(),
                    ));
                }
                // shortcut: long GUI operations hold attachment transfer until their reply; add admission ACKs if this blocks normal use.
                let _gate = self.attachment_gate(context.key.instance)?;
                let HostResponse::Context {
                    key,
                    location,
                    socket,
                } = self.resolve_context(
                    connection,
                    context.key.instance,
                    context.key.epoch,
                    session_id.as_deref(),
                )?
                else {
                    unreachable!()
                };
                if key != context.key || !location.same_target(&context.location) {
                    return Err(RpcError::NotFound(
                        "no_attached_view: source moved; request not started".into(),
                    ));
                }
                let client = flowmux_ipc::client::Client::connect(&socket)
                    .await
                    .map_err(|error| {
                        RpcError::Io(format!(
                            "no_attached_view: GUI unavailable; request not started: {error}"
                        ))
                    })?;
                let response = client.call(Request::InAgentView { context, request }).await
                    .map_err(|error| RpcError::Io(format!("managed GUI response lost; outcome unknown; request was not retried: {error}")))?;
                Ok(HostResponse::Dispatched {
                    response: Box::new(response),
                })
            }
            HostRequest::ReportViews { views } => {
                self.registry()
                    .report_views(connection, views, Instant::now())?;
                Ok(HostResponse::Ok)
            }
            HostRequest::SetIdleTimeout { minutes } => {
                if self.registry().connections.get(&connection) != Some(&ClientRole::Gui) {
                    return Err(RpcError::InvalidArgument(
                        "only a GUI may change the idle timeout".into(),
                    ));
                }
                self.registry().policy.set_timeout(Duration::from_secs(
                    u64::from(minutes.clamp(1, 10_080)) * 60,
                ));
                Ok(HostResponse::Ok)
            }
            request => {
                let instance = match &request {
                    HostRequest::PrepareAttach { instance, .. }
                    | HostRequest::Lifecycle { instance, .. }
                    | HostRequest::NativeSession { instance, .. }
                    | HostRequest::Terminate { instance, .. } => *instance,
                    HostRequest::CommitAttach { key, .. }
                    | HostRequest::ViewReady { key, .. }
                    | HostRequest::GuiViewReady { key, .. }
                    | HostRequest::AbortAttach { key }
                    | HostRequest::Detach { key }
                    | HostRequest::Frame { key, .. }
                    | HostRequest::Input { key, .. }
                    | HostRequest::Resize { key, .. } => key.instance,
                    _ => unreachable!(),
                };
                let _gate = if matches!(
                    &request,
                    HostRequest::PrepareAttach { .. }
                        | HostRequest::CommitAttach { .. }
                        | HostRequest::ViewReady { .. }
                        | HostRequest::GuiViewReady { .. }
                        | HostRequest::AbortAttach { .. }
                        | HostRequest::Detach { .. }
                ) {
                    Some(self.attachment_gate(instance)?)
                } else {
                    None
                };
                if let HostRequest::Detach { key } = &request {
                    let mut registry = self.registry();
                    if let Some(entry) = registry.entries.get_mut(&instance) {
                        if entry
                            .final_frame
                            .as_ref()
                            .is_some_and(|(final_key, _, _)| final_key == key)
                        {
                            entry.final_frame = None;
                            entry.attachment_connection = None;
                            return Ok(HostResponse::Ok);
                        }
                    }
                }
                if let HostRequest::Frame { key, after, offset } = &request {
                    let registry = self.registry();
                    if let Some((final_key, final_frame, previous)) = registry
                        .entries
                        .get(&instance)
                        .and_then(|entry| entry.final_frame.as_ref())
                    {
                        if key != final_key {
                            return Err(stale_lease());
                        }
                        if *offset > 0 {
                            if let Some(previous) =
                                previous.as_ref().filter(|frame| frame.sequence == *after)
                            {
                                return Ok(HostResponse::Frame {
                                    frame: previous.chunk(*offset)?,
                                });
                            }
                        }
                        let mut frame = final_frame.chunk(*offset)?;
                        if *offset == 0 && *after == final_frame.sequence {
                            frame.data.clear();
                            frame.next_offset = None;
                        }
                        frame.ended = true;
                        return Ok(HostResponse::Frame { frame });
                    }
                }
                let sender = self
                    .registry()
                    .entries
                    .get(&instance)
                    .and_then(|entry| entry.commands.clone())
                    .ok_or_else(|| RpcError::NotFound("Agent is no longer running".into()))?;
                let (reply, response) = oneshot::channel();
                sender
                    .try_send(Command {
                        connection,
                        request,
                        reply,
                    })
                    .map_err(|error| match error {
                        mpsc::error::TrySendError::Full(_) => RpcError::Busy(
                            "Agent command queue is full; request not started".into(),
                        ),
                        mpsc::error::TrySendError::Closed(_) => {
                            RpcError::NotFound("Agent is no longer running".into())
                        }
                    })?;
                response
                    .await
                    .map_err(|_| RpcError::Io("Agent terminal connection ended".into()))?
            }
        }
    }

    fn attachment_gate(
        &self,
        instance: AgentInstanceId,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        self.registry()
            .entries
            .get(&instance)
            .ok_or_else(|| RpcError::NotFound("Agent is not in this Host".into()))?
            .attachment_gate
            .clone()
            .try_lock_owned()
            .map_err(|_| {
                RpcError::Busy("Agent view operation in progress; request not started".into())
            })
    }

    fn resolve_context(
        &self,
        connection: u64,
        instance: AgentInstanceId,
        epoch: Uuid,
        session_id: Option<&str>,
    ) -> Result<HostResponse> {
        if epoch != self.epoch || session_id.is_some_and(|id| id.is_empty() || id.len() > 1024) {
            return Err(RpcError::InvalidArgument(
                "invalid managed context identity".into(),
            ));
        }
        let mut registry = self.registry();
        registry.expire_views(Instant::now());
        let entry = registry.require_execution_caller(connection, instance)?;
        if session_id.is_some_and(|id| {
            entry
                .agent
                .presence
                .as_ref()
                .and_then(|presence| presence.session_id.as_deref())
                != Some(id)
        }) {
            return Err(RpcError::InvalidArgument(
                "managed native session identity does not match".into(),
            ));
        }
        let no_view = || {
            RpcError::NotFound(
                "no_attached_view: managed execution has no verified current GUI view".into(),
            )
        };
        let attachment = entry
            .agent
            .attachment
            .as_ref()
            .filter(|a| a.ready)
            .ok_or_else(no_view)?;
        let location = entry.agent.view.as_ref().ok_or_else(no_view)?;
        let owner = entry
            .view_report
            .map(|(connection, _)| connection)
            .ok_or_else(no_view)?;
        let gui = registry
            .peers
            .get(&owner)
            .and_then(|peer| peer.identity)
            .ok_or_else(no_view)?;
        let relay = entry
            .attachment_connection
            .and_then(|id| registry.peers.get(&id))
            .ok_or_else(no_view)?;
        if registry.connections.get(&owner) != Some(&ClientRole::Gui)
            || flowmux_procmon::process_start_time(gui.0) != Some(gui.1)
            || !relay
                .identity
                .is_some_and(|(pid, start)| flowmux_procmon::process_start_time(pid) == Some(start))
            || !(relay.parent == Some(gui)
                || relay.grandparent == Some(gui)
                || relay.identity == Some(gui))
        {
            return Err(no_view());
        }
        // GUI and Host share runtime configuration; the Host socket itself may be overridden.
        let socket = flowmux_config::paths::runtime_socket_for_pid(gui.0);
        Ok(HostResponse::Context {
            key: attachment.key,
            location: location.clone(),
            socket,
        })
    }

    async fn launch(self: &Arc<Self>, mut launch: LaunchAgent) -> Result<HostResponse> {
        validate_launch(&launch)?;
        let fingerprint: [u8; 32] = Sha256::digest(
            serde_json::to_vec(&launch)
                .map_err(|_| RpcError::InvalidArgument("invalid launch request".into()))?,
        )
        .into();
        let instance = AgentInstanceId::new();
        {
            let mut registry = self.registry();
            if registry.stopping {
                return Err(RpcError::Busy(
                    "Agent Host is stopping; launch not started".into(),
                ));
            }
            if let Some((prior_fingerprint, instance)) = registry.launches.get(&launch.request_id) {
                if *prior_fingerprint != fingerprint {
                    return Err(RpcError::InvalidArgument(
                        "launch request ID was reused with different contents".into(),
                    ));
                }
                return Ok(HostResponse::Launched {
                    agent: Box::new(registry.entries[instance].agent.clone()),
                });
            }
            if launch_millis(launch.request_id)
                > (chrono::Utc::now().timestamp_millis() as u64).saturating_add(60_000)
            {
                return Err(RpcError::InvalidArgument(
                    "launch request timestamp is too far in the future".into(),
                ));
            }
            if let Some(source) = &launch.fork_source {
                let current = registry
                    .entries
                    .get(&source.instance)
                    .map(|entry| &entry.agent);
                if !current.is_some_and(|agent| {
                    agent.epoch == source.epoch
                        && agent.state == ProcessState::Running
                        && agent.provider.eq_ignore_ascii_case(&launch.provider)
                        && agent
                            .pid
                            .zip(agent.process_start)
                            .is_some_and(|(pid, start)| {
                                flowmux_procmon::process_start_time(pid) == Some(start)
                            })
                        && agent.presence.as_ref().is_some_and(|presence| {
                            presence.session_id.as_deref() == Some(source.session_id.as_str())
                                && matches!(
                                    presence.status,
                                    flowmux_core::AgentStatus::Idle
                                        | flowmux_core::AgentStatus::Done
                                )
                        })
                }) {
                    return Err(RpcError::Busy(
                        "Fork source is no longer the same idle session; launch not started".into(),
                    ));
                }
            }
            registry.prune_ended_launches();
            if registry
                .expired_launch_ms
                .is_some_and(|floor| launch_millis(launch.request_id) <= floor)
            {
                return Err(RpcError::InvalidArgument(
                    "Agent launch request expired; inspect existing executions before starting a new request".into(),
                ));
            }
            if registry
                .entries
                .values()
                .filter(|entry| !entry.is_reaped())
                .count()
                >= MAX_INSTANCES
            {
                return Err(RpcError::Busy(
                    "Agent Host instance limit reached; launch not started".into(),
                ));
            }
            registry
                .launches
                .insert(launch.request_id, (fingerprint, instance));
            registry.entries.insert(
                instance,
                Entry {
                    attachment_gate: Arc::new(tokio::sync::Mutex::new(())),
                    agent: AgentInstance {
                        instance,
                        epoch: self.epoch,
                        provider: launch.provider.clone(),
                        label: launch.label.clone(),
                        cwd: launch.cwd.clone(),
                        pid: None,
                        process_start: None,
                        boot_id: boot_id(),
                        state: ProcessState::Starting,
                        force_terminate_allowed: false,
                        exit_status: None,
                        view: None,
                        attachment: None,
                        view_owner: None,
                        error: None,
                        presence: None,
                    },
                    commands: None,
                    final_frame: None,
                    attachment_connection: None,
                    view_report: None,
                },
            );
            registry.policy.register(instance);
            registry.accepted_launches += 1;
            registry.revision += 1;
            registry.empty_since = None;
        }
        // Hook routing must survive the originating GUI. Never retain its socket.
        launch.env.retain(|(name, _)| {
            !matches!(
                name.as_str(),
                "FLOWMUX_SOCKET_PATH"
                    | "FLOWMUX_SOCKET"
                    | "FLOWMUX_AGENT_INSTANCE_ID"
                    | "FLOWMUX_AGENT_HOST_EPOCH"
                    | "FLOWMUX_PANE_ID"
                    | "FLOWMUX_SURFACE_ID"
                    | "FLOWMUX_WORKSPACE_ID"
                    | "FLOWMUX_TAB_ID"
                    | "FLOWMUX_AGENT_PID"
                    | "FLOWMUX_AGENT_NAME"
            )
        });
        launch.env.extend([
            (
                "FLOWMUX_SOCKET_PATH".into(),
                self.socket.to_string_lossy().into_owned(),
            ),
            ("FLOWMUX_AGENT_INSTANCE_ID".into(), instance.to_string()),
            ("FLOWMUX_AGENT_HOST_EPOCH".into(), self.epoch.to_string()),
        ]);
        let size = launch.size;
        let spawned = tokio::task::spawn_blocking(move || {
            let argv: Vec<_> = launch.argv.iter().map(String::as_str).collect();
            Pty::spawn_gated_with_environment(&argv, &launch.cwd, &launch.env, size.cols, size.rows)
        })
        .await;
        let spawned = match spawned {
            Ok(Ok((pty, gate))) => {
                let prepared = flowmux_procmon::process_start_time(pty.child_pid() as u32)
                    .ok_or("unable to identify agent process")
                    .and_then(|start| {
                        master_io(&pty)
                            .map(|io| (start, io))
                            .map_err(|_| "unable to register agent PTY")
                    });
                match prepared {
                    Ok((start, io)) => Ok((pty, gate, io, start)),
                    Err(error) => {
                        cancel_spawn(pty, gate).await;
                        Err(error)
                    }
                }
            }
            _ => Err("unable to spawn agent process"),
        };
        match spawned {
            Ok((pty, gate, io, start)) => {
                let pid = pty.child_pid() as u32;
                let (sender, receiver) = mpsc::channel(64);
                let mut registry = self.registry();
                registry.accepted_launches -= 1;
                registry.launched = true;
                registry.revision += 1;
                let entry = registry.entries.get_mut(&instance).unwrap();
                entry.agent.pid = Some(pid);
                entry.agent.process_start = Some(start);
                entry.agent.state = ProcessState::Running;
                entry.commands = Some(sender);
                let agent = entry.agent.clone();
                let actor =
                    TerminalActor::new(self.clone(), instance, pty, io, size, receiver, start);
                tokio::spawn(actor.run());
                // Identity and policy exist before provider hooks can fire.
                gate.release();
                Ok(HostResponse::Launched {
                    agent: Box::new(agent),
                })
            }
            Err(error) => {
                let mut registry = self.registry();
                registry.accepted_launches -= 1;
                registry.launched = true;
                registry.revision += 1;
                registry.policy.remove_reaped(instance);
                let entry = registry.entries.get_mut(&instance).unwrap();
                entry.agent.state = ProcessState::Failed;
                entry.agent.error = Some(error.into());
                Ok(HostResponse::Launched {
                    agent: Box::new(entry.agent.clone()),
                })
            }
        }
    }
}

// Keep Arc ownership explicit so an admitted launch survives client EOF.
struct HostHandler(Arc<AgentHost>);

impl Handler for HostHandler {
    fn handle<'a>(&'a self, _: Request) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        Box::pin(async {
            Response::Error(RpcError::InvalidArgument(
                "persistent Agent Host connection required".into(),
            ))
        })
    }
    fn handle_connection<'a>(
        &'a self,
        request: Request,
        context: ConnectionContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        Box::pin(async move {
            match request {
                Request::AgentHost { request } => {
                    match self.0.request(request, context.id, context.peer_pid).await {
                        Ok(response) => Response::AgentHost { response },
                        Err(error) => Response::Error(error),
                    }
                }
                Request::Ping => Response::Pong,
                _ => Response::Error(RpcError::Unimplemented(
                    "request is not supported by the Agent Host".into(),
                )),
            }
        })
    }
    fn connection_closed(&self, context: ConnectionContext) {
        let mut registry = self.0.registry();
        registry.gui_list_revision.remove(&context.id);
        registry.peers.remove(&context.id);
        let mut changed = false;
        for entry in registry.entries.values_mut() {
            if entry.attachment_connection == Some(context.id) {
                entry.attachment_connection = None;
                entry.final_frame = None;
                entry.agent.attachment = None;
                entry.agent.view = None;
                entry.view_report = None;
                changed = true;
            } else if entry
                .view_report
                .is_some_and(|(owner, _)| owner == context.id)
            {
                entry.agent.view = None;
                entry.view_report = None;
                changed = true;
            }
        }
        if changed {
            registry.revision += 1;
        }
        if registry.connections.remove(&context.id) == Some(ClientRole::Gui) {
            registry.policy.gui_disconnected(context.id, Instant::now());
        }
    }
}

fn launch_millis(id: Uuid) -> u64 {
    let (seconds, nanos) = id
        .get_timestamp()
        .expect("validated UUIDv7 launch ID")
        .to_unix();
    seconds * 1000 + u64::from(nanos / 1_000_000)
}

fn validate_launch(launch: &LaunchAgent) -> Result<()> {
    launch.size.validate()?;
    if launch.request_id.get_version_num() != 7 {
        return Err(RpcError::InvalidArgument(
            "launch request ID must be UUIDv7".into(),
        ));
    }
    if launch.argv.is_empty()
        || launch.argv.len() > 256
        || !Path::new(&launch.argv[0]).is_absolute()
        || launch
            .argv
            .iter()
            .any(|arg| arg.contains('\0') || arg.len() > 64 * 1024)
        || launch.argv.iter().map(String::len).sum::<usize>() > 256 * 1024
        || launch.env.len() > 512
        || launch
            .env
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains(['=', '\0']) || value.contains('\0'))
        || launch
            .env
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 256 * 1024
        || launch.provider.is_empty()
        || launch.provider.len() > 64
        || launch.label.len() > 256
        || !launch.cwd.is_absolute()
        || !launch.cwd.is_dir()
    {
        return Err(RpcError::InvalidArgument(
            "invalid agent launch arguments, environment or working directory".into(),
        ));
    }
    Ok(())
}

fn process_belongs_to_root(mut pid: u32, root: u32, start: u64) -> bool {
    if flowmux_procmon::process_start_time(root) != Some(start) {
        return false;
    }
    for _ in 0..64 {
        if pid == root {
            return flowmux_procmon::process_start_time(root) == Some(start);
        }
        let Some(parent) =
            flowmux_procmon::parent_pid(pid).filter(|parent| *parent > 1 && *parent != pid)
        else {
            return false;
        };
        pid = parent;
    }
    false
}

fn boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|value| value.trim().to_owned())
    }
    #[cfg(target_os = "macos")]
    {
        flowmux_procmon::boot_session_id()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

fn master_io(pty: &Pty) -> io::Result<AsyncFd<OwnedFd>> {
    // SAFETY: duplicate our PTY; the actor is the only reader/writer.
    let fd = unsafe { libc::fcntl(pty.master_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the registered descriptor is nonblocking and remains owned by
    // AsyncFd until deregistration; no other code closes or replaces it.
    Ok(unsafe { AsyncFd::register(fd) }?)
}

async fn cancel_spawn(mut pty: Pty, gate: flowmux_terminal::pty::SpawnGate) {
    gate.cancel();
    // A failed launch is not an empty Host until its parked child is reaped.
    let _ = tokio::task::spawn_blocking(move || loop {
        let mut status = 0;
        let result = unsafe { libc::waitpid(pty.child_pid(), &mut status, 0) };
        if result == pty.child_pid()
            || (result < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD))
        {
            pty.mark_child_exited();
            break;
        }
        if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            break;
        }
    })
    .await;
}

struct Command {
    connection: u64,
    request: HostRequest,
    reply: oneshot::Sender<Result<HostResponse>>,
}

struct Transfer {
    sequence: u64,
    snapshot: bool,
    size: TerminalSize,
    data: Arc<str>,
}

impl Transfer {
    fn chunk(&self, offset: usize) -> Result<TerminalFrame> {
        if offset > self.data.len() || !self.data.is_char_boundary(offset) {
            return Err(RpcError::InvalidArgument(
                "invalid terminal frame offset".into(),
            ));
        }
        let mut end = (offset + FRAME_CHUNK).min(self.data.len());
        while !self.data.is_char_boundary(end) {
            end -= 1;
        }
        Ok(TerminalFrame {
            sequence: self.sequence,
            snapshot: self.snapshot,
            size: self.size,
            offset,
            next_offset: (end < self.data.len()).then_some(end),
            ended: false,
            data: self.data[offset..end].into(),
        })
    }
}

struct Lease {
    key: AttachmentKey,
    connection: u64,
    request: Uuid,
    view: Option<AgentViewLocation>,
    surface: Option<flowmux_core::SurfaceId>,
    require_gui: bool,
    gui_ready: bool,
    prepared: Instant,
    transfer: Option<Transfer>,
    initial_sequence: Option<u64>,
    replace: Option<AttachmentKey>,
    commit_size: Option<TerminalSize>,
    ready: bool,
    input_sequence: u64,
    input_digest: Option<[u8; 32]>,
}

struct TerminalActor {
    host: Arc<AgentHost>,
    instance: AgentInstanceId,
    pty: Pty,
    io: AsyncFd<OwnedFd>,
    terminal: Terminal,
    commands: mpsc::Receiver<Command>,
    active: Option<Lease>,
    pending: Option<Lease>,
    generation: u64,
    input: VecDeque<u8>,
    replies: VecDeque<u8>,
    owned: HashMap<u32, u64>,
    root_status: Option<i32>,
    eof: bool,
    stopping: Option<(i32, Instant)>,
    lifecycle: AgentLifecycleRuntime<AgentInstanceId>,
    presence: Option<AgentPresence>,
    settle_codex: Option<(Instant, CodexGraceSettlement)>,
    native_session_seq: Option<u64>,
}

impl TerminalActor {
    fn new(
        host: Arc<AgentHost>,
        instance: AgentInstanceId,
        pty: Pty,
        io: AsyncFd<OwnedFd>,
        size: TerminalSize,
        commands: mpsc::Receiver<Command>,
        start: u64,
    ) -> Self {
        let owned = HashMap::from([(pty.child_pid() as u32, start)]);
        Self {
            host,
            instance,
            pty,
            io,
            terminal: Terminal::new(size),
            commands,
            active: None,
            pending: None,
            generation: 0,
            input: VecDeque::new(),
            replies: VecDeque::new(),
            owned,
            root_status: None,
            eof: false,
            stopping: None,
            lifecycle: AgentLifecycleRuntime::default(),
            presence: None,
            settle_codex: None,
            native_session_seq: None,
        }
    }

    async fn run(mut self) {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let count = self.replies.len().min(MAX_INPUT_PENDING - self.input.len());
            self.input.extend(self.replies.drain(..count));
            let sync_deadline = self.terminal.sync_deadline();
            tokio::select! {
                _ = async {
                    match sync_deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline.into()).await,
                        None => std::future::pending().await,
                    }
                } => self.replies.extend(self.terminal.flush_sync()),
                command = self.commands.recv() => if let Some(command) = command {
                    self.clear_disconnected();
                    let result = self.command(command.connection,command.request);
                    let _ = command.reply.send(result);
                },
                ready = self.io.readable(), if !self.eof && self.replies.is_empty() => match ready {
                    Ok(mut guard) => {
                        match guard.try_io(|io| read_fd(io.get_ref().as_raw_fd(),&mut buffer)) {
                            Ok(Ok(0)) => self.eof = true,
                            Ok(Ok(n)) => {
                                // At most one PTY read of replies waits behind accepted input.
                                // Pause reads until it drains instead of dropping provider queries.
                                self.replies.extend(self.terminal.feed(&buffer[..n]));
                            }
                            Ok(Err(_)) => { self.eof = true; self.fail("unable to read agent PTY"); }
                            Err(_) => {}
                        }
                    }
                    Err(_) => { self.eof = true; self.fail("agent PTY readiness failed"); }
                },
                ready = self.io.writable(), if !self.input.is_empty() => match ready {
                    Ok(mut guard) => {
                        let (data,_) = self.input.as_slices();
                        match guard.try_io(|io| write_fd(io.get_ref().as_raw_fd(),data)) {
                            Ok(Ok(n)) if n > 0 => { self.input.drain(..n); }
                            Ok(Ok(_)) | Ok(Err(_)) => { self.input.clear(); self.replies.clear(); self.fail("unable to write agent PTY"); }
                            Err(_) => {}
                        }
                    }
                    Err(_) => { self.input.clear(); self.replies.clear(); self.fail("agent PTY readiness failed"); }
                },
                _ = tick.tick() => {
                    self.clear_disconnected();
                    self.observe_processes();
                    if self.root_status.is_none() {
                        match self.pty.try_wait() {
                            Ok(Some(status)) => self.root_status = Some(status),
                            Ok(None) => {}
                            Err(error) if error.raw_os_error() == Some(libc::EINTR) => {}
                            Err(_) => self.fail("unable to reap agent root process"),
                        }
                    }
                    self.settle_lifecycle();
                    self.reap_descendants();
                    if let Some((signal,deadline)) = self.stopping {
                        self.signal_owned(signal);
                        if Instant::now() >= deadline {
                            let mut registry = self.host.registry();
                            let agent = &mut registry.entries.get_mut(&self.instance).unwrap().agent;
                            if !agent.force_terminate_allowed {
                                agent.force_terminate_allowed = true;
                                registry.revision += 1;
                            }
                        }
                    }
                    if self.root_status.is_some() && self.owned.is_empty() && self.eof {
                        self.finished();
                        break;
                    }
                }
            }
            // The final frame must not depend on a provider's missing ESU.
            if self.eof && self.terminal.sync_deadline().is_some() {
                self.replies.extend(self.terminal.flush_sync());
            }
        }
    }

    fn command(&mut self, mut connection: u64, request: HostRequest) -> Result<HostResponse> {
        let gui_admitted = matches!(&request, HostRequest::GuiViewReady { .. });
        let request = match request {
            HostRequest::GuiViewReady {
                key,
                surface,
                sequence,
                relay_pid,
                relay_start,
            } => {
                let lease = self
                    .pending
                    .as_ref()
                    .filter(|lease| lease.key == key && lease.gui_ready)
                    .or_else(|| {
                        self.active
                            .as_ref()
                            .filter(|lease| lease.key == key && lease.gui_ready)
                    })
                    .ok_or_else(stale_lease)?;
                let registry = self.host.registry();
                let gui = registry
                    .peers
                    .get(&connection)
                    .and_then(|peer| peer.identity);
                let relay = registry.peers.get(&lease.connection);
                if lease.surface != Some(surface)
                    || registry.connections.get(&connection) != Some(&ClientRole::Gui)
                    || gui.is_none_or(|(pid, birth)| {
                        flowmux_procmon::process_start_time(pid) != Some(birth)
                    })
                    || !relay.is_some_and(|peer| {
                        peer.identity == Some((relay_pid, relay_start))
                            && (peer.parent == gui
                                || peer.grandparent == gui
                                || peer.identity == gui)
                    })
                    || flowmux_procmon::process_start_time(relay_pid) != Some(relay_start)
                {
                    return Err(RpcError::InvalidArgument(
                        "Agent relay does not belong to the admitting GUI".into(),
                    ));
                }
                connection = lease.connection;
                HostRequest::ViewReady { key, sequence }
            }
            request => request,
        };
        match request {
            HostRequest::Lifecycle {
                instance,
                epoch,
                pid,
                process_start,
                session_id,
                seq,
                event,
                notification,
            } => {
                self.validate_hook(instance, epoch, pid, process_start, &session_id)?;
                if self.native_session_seq.is_some()
                    && self
                        .presence
                        .as_ref()
                        .and_then(|presence| presence.session_id.as_deref())
                        != Some(session_id.as_str())
                {
                    return Ok(HostResponse::LifecycleReceipt { applied: false });
                }
                if serde_json::to_vec(&event).map_or(true, |bytes| bytes.len() > 64 * 1024) {
                    return Err(RpcError::InvalidArgument(
                        "invalid agent lifecycle event".into(),
                    ));
                }
                if notification
                    .as_ref()
                    .is_some_and(|n| n.title.len() > 1024 || n.body.len() > 16 * 1024)
                {
                    return Err(RpcError::InvalidArgument(
                        "agent notification is too large".into(),
                    ));
                }
                let provider = self.host.registry().entries[&instance]
                    .agent
                    .provider
                    .clone();
                let transition = self.lifecycle.reduce(
                    instance,
                    &provider,
                    Some(pid),
                    Some(seq),
                    &session_id,
                    event,
                    self.presence.as_ref(),
                );
                let applied = transition.is_some();
                if let Some(transition) = transition {
                    self.project_lifecycle(transition, notification);
                }
                Ok(HostResponse::LifecycleReceipt { applied })
            }
            HostRequest::NativeSession {
                instance,
                epoch,
                pid,
                process_start,
                session_id,
                seq,
                event,
            } => {
                self.validate_hook(instance, epoch, pid, process_start, &session_id)?;
                if self.native_session_seq.is_some_and(|floor| seq <= floor) {
                    return Ok(HostResponse::LifecycleReceipt { applied: false });
                }
                let provider = self.host.registry().entries[&instance]
                    .agent
                    .provider
                    .clone();
                let started = matches!(event, NativeSessionEvent::Started { .. });
                let applied = self.lifecycle.native_session(
                    instance,
                    &provider,
                    pid,
                    &session_id,
                    seq,
                    started,
                    self.presence.as_ref(),
                );
                if applied {
                    self.native_session_seq = Some(seq);
                    self.settle_codex = None;
                    self.presence = match event {
                        NativeSessionEvent::Started { status } => {
                            let mut report = flowmux_core::AgentStatusReport::from_activity(
                                provider,
                                None,
                                Some(pid),
                            );
                            report.status = Some(status);
                            report.seq = Some(seq);
                            report.source = Some("flowmux:hook".into());
                            report.session_id = Some(session_id);
                            report.custom_status = Some("Ready".into());
                            AgentPresence::from_report(report, false)
                        }
                        NativeSessionEvent::Ended => None,
                    };
                    let mut registry = self.host.registry();
                    registry.entries.get_mut(&instance).unwrap().agent.presence =
                        self.presence.clone();
                    registry.policy.observation_uncertain(instance);
                    registry.revision += 1;
                }
                Ok(HostResponse::LifecycleReceipt { applied })
            }
            HostRequest::PrepareAttach {
                instance,
                epoch,
                request_id,
                view,
                surface,
                require_gui,
                gui_ready,
                replace,
            } => {
                if gui_ready && !require_gui {
                    return Err(RpcError::InvalidArgument(
                        "GUI admission requires a GUI attachment".into(),
                    ));
                }
                if surface.is_some_and(|surface| surface.0.is_nil())
                    || (require_gui && surface.is_none())
                {
                    return Err(RpcError::InvalidArgument(
                        "GUI surface is missing or invalid".into(),
                    ));
                }
                if view.is_some() {
                    return Err(RpcError::InvalidArgument(
                        "view locations require a GUI report".into(),
                    ));
                }
                if instance != self.instance || epoch != self.host.epoch {
                    return Err(RpcError::NotFound(
                        "Agent Host epoch or instance changed".into(),
                    ));
                }
                if self.stopping.is_some() || self.root_status.is_some() {
                    return Err(RpcError::Busy("Agent is already stopping".into()));
                }
                if require_gui {
                    self.require_gui_connection()?;
                }
                if !gui_ready && replace.is_none() {
                    if let Some(owner) = self.host.registry().entries[&instance].agent.view_owner {
                        if surface.is_some_and(|surface| surface != owner.surface) {
                            return Err(RpcError::NotFound(
                                "Saved Agent view was superseded; open explicitly from Agents"
                                    .into(),
                            ));
                        }
                    }
                }
                if let Some(pending) = self.pending.as_ref() {
                    if pending.request == request_id
                        && pending.connection == connection
                        && pending.view == view
                        && pending.surface == surface
                        && pending.require_gui == require_gui
                        && pending.gui_ready == gui_ready
                        && pending.replace == replace
                    {
                        let key = pending.key;
                        let frame = self.frame(key, 0, 0)?;
                        return Ok(HostResponse::Prepared { key, frame });
                    }
                    return Err(RpcError::Busy(
                        "another attachment is being prepared".into(),
                    ));
                }
                if let Some(active) = self.active.as_ref() {
                    if replace != Some(active.key) {
                        return Err(RpcError::Busy(
                            "agent already has an input owner; explicit move required".into(),
                        ));
                    }
                } else if replace.is_some() {
                    return Err(RpcError::NotFound(
                        "previous attachment is no longer active".into(),
                    ));
                }
                self.generation += 1;
                let key = AttachmentKey {
                    instance,
                    epoch,
                    lease: Uuid::new_v4(),
                    generation: self.generation,
                };
                self.pending = Some(Lease {
                    key,
                    connection,
                    request: request_id,
                    view,
                    surface,
                    require_gui,
                    gui_ready,
                    prepared: Instant::now(),
                    transfer: None,
                    initial_sequence: None,
                    replace,
                    commit_size: None,
                    ready: false,
                    input_sequence: 0,
                    input_digest: None,
                });
                let frame = self.frame(key, 0, 0)?;
                Ok(HostResponse::Prepared { key, frame })
            }
            HostRequest::CommitAttach { key, size } => {
                size.validate()?;
                if self
                    .active
                    .as_ref()
                    .is_some_and(|lease| lease.key == key && lease.connection == connection)
                {
                    let frame = self.frame(key, 0, 0)?;
                    return Ok(HostResponse::Attached { key, frame });
                }
                let pending = self
                    .pending
                    .as_ref()
                    .filter(|lease| lease.key == key && lease.connection == connection)
                    .ok_or_else(stale_lease)?;
                if pending.require_gui {
                    self.require_gui_connection()?;
                }
                if pending.replace != self.active.as_ref().map(|lease| lease.key) {
                    return Err(stale_lease());
                }
                if self.stopping.is_some() || self.root_status.is_some() {
                    return Err(RpcError::Busy("Agent is already stopping".into()));
                }
                if pending.replace.is_some() {
                    if pending.commit_size.is_some_and(|previous| previous != size) {
                        return Err(RpcError::InvalidArgument(
                            "attachment destination size changed; use Resize".into(),
                        ));
                    }
                    // Keep the source's input, geometry and location until the new snapshot is written.
                    let frame = self.frame(key, 0, 0)?;
                    let pending = self.pending.as_mut().unwrap();
                    pending.commit_size = Some(size);
                    pending.initial_sequence.get_or_insert(frame.sequence);
                    return Ok(HostResponse::Attached { key, frame });
                }
                self.resize(size)?;
                self.active = self.pending.take();
                let frame = self.frame(key, 0, 0)?;
                self.active.as_mut().unwrap().initial_sequence = Some(frame.sequence);
                self.update_view();
                Ok(HostResponse::Attached { key, frame })
            }
            HostRequest::ViewReady { key, sequence } => {
                let pending = self
                    .pending
                    .as_ref()
                    .is_some_and(|lease| lease.key == key && lease.connection == connection);
                let lease = if pending {
                    self.pending.as_ref().unwrap()
                } else {
                    self.input_lease(key, connection)?
                };
                if lease.gui_ready && !gui_admitted {
                    return Err(RpcError::InvalidArgument(
                        "Agent view requires destination GUI admission".into(),
                    ));
                }
                if lease
                    .initial_sequence
                    .is_none_or(|initial| sequence < initial)
                    || lease
                        .transfer
                        .as_ref()
                        .is_none_or(|frame| sequence > frame.sequence)
                {
                    return Err(RpcError::InvalidArgument(
                        "Agent snapshot acknowledgement is out of bounds".into(),
                    ));
                }
                if !lease.ready && (self.stopping.is_some() || self.root_status.is_some()) {
                    return Err(RpcError::Busy("Agent is already stopping".into()));
                }
                if pending {
                    if lease.require_gui {
                        self.require_gui_connection()?;
                    }
                    if lease.replace != self.active.as_ref().map(|lease| lease.key) {
                        return Err(stale_lease());
                    }
                    let size = lease.commit_size.ok_or_else(stale_lease)?;
                    self.resize(size)?;
                    self.active = self.pending.take();
                }
                let lease = self.active.as_ref().unwrap();
                if !lease.ready {
                    self.active.as_mut().unwrap().ready = true;
                    self.update_view();
                }
                Ok(HostResponse::Ok)
            }
            HostRequest::AbortAttach { key } => {
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|lease| lease.key == key && lease.connection == connection)
                {
                    self.pending = None;
                } else {
                    return Err(stale_lease());
                }
                Ok(HostResponse::Ok)
            }
            HostRequest::Detach { key } => {
                self.input_lease(key, connection)?;
                self.active = None;
                self.update_view();
                Ok(HostResponse::Ok)
            }
            HostRequest::Frame { key, after, offset } => Ok(HostResponse::Frame {
                frame: self.frame(key, after, offset)?,
            }),
            HostRequest::Input {
                key,
                sequence,
                data,
            } => {
                let lease = self.input_lease(key, connection)?;
                if !lease.ready {
                    return Err(RpcError::Busy(
                        "Agent view is not ready; input not accepted".into(),
                    ));
                }
                if data.is_empty() || data.len() > MAX_INPUT_BYTES {
                    return Err(RpcError::InvalidArgument(
                        "input size is out of bounds".into(),
                    ));
                }
                let digest: [u8; 32] = Sha256::digest(&data).into();
                if sequence == lease.input_sequence && lease.input_digest == Some(digest) {
                    return Ok(HostResponse::InputAccepted { sequence });
                }
                if lease.input_sequence.checked_add(1) != Some(sequence) {
                    return Err(RpcError::InvalidArgument(
                        "input sequence is stale, conflicting or out of order".into(),
                    ));
                }
                if !self.replies.is_empty() || self.input.len() + data.len() > MAX_INPUT_PENDING {
                    return Err(RpcError::Busy(
                        "agent input queue is full; input not accepted".into(),
                    ));
                }
                self.host
                    .registry()
                    .policy
                    .observation_uncertain(self.instance);
                self.input.extend(data);
                let lease = self.active.as_mut().unwrap();
                lease.input_sequence = sequence;
                lease.input_digest = Some(digest);
                Ok(HostResponse::InputAccepted { sequence })
            }
            HostRequest::Resize { key, size } => {
                let size = size.validate()?;
                if let Some(lease) = self.pending.as_mut().filter(|lease| {
                    lease.key == key
                        && lease.connection == connection
                        && lease.commit_size.is_some()
                }) {
                    lease.commit_size = Some(size);
                    return Ok(HostResponse::Ok);
                }
                self.input_lease(key, connection)?;
                self.resize(size)?;
                Ok(HostResponse::Ok)
            }
            HostRequest::Terminate { epoch, mode, .. } => {
                if epoch != self.host.epoch {
                    return Err(stale_lease());
                }
                if mode == TerminateMode::Interrupt {
                    if self.stopping.is_some() || self.root_status.is_some() {
                        return Err(RpcError::Busy("Agent is already stopping".into()));
                    }
                    if !self.replies.is_empty() || self.input.len() >= MAX_INPUT_PENDING {
                        return Err(RpcError::Busy(
                            "input queue full; interrupt not accepted".into(),
                        ));
                    }
                    self.input.push_back(3);
                    self.host
                        .registry()
                        .policy
                        .observation_uncertain(self.instance);
                } else {
                    if mode == TerminateMode::Force {
                        if self
                            .stopping
                            .is_some_and(|(signal, _)| signal == libc::SIGKILL)
                        {
                            return Ok(HostResponse::Ok);
                        }
                        if !self
                            .stopping
                            .is_some_and(|(_, deadline)| Instant::now() >= deadline)
                        {
                            return Err(RpcError::Busy(
                                "Request normal termination and wait for its timeout before forcing".into(),
                            ));
                        }
                    } else if self.stopping.is_some() {
                        return Ok(HostResponse::Ok);
                    }
                    self.observe_processes();
                    let signal = if mode == TerminateMode::Force {
                        libc::SIGKILL
                    } else {
                        libc::SIGTERM
                    };
                    self.stopping = Some((signal, Instant::now() + STOP_GRACE));
                    self.signal_owned(signal);
                    let mut registry = self.host.registry();
                    registry
                        .entries
                        .get_mut(&self.instance)
                        .unwrap()
                        .agent
                        .state = ProcessState::Stopping;
                    registry.revision += 1;
                }
                Ok(HostResponse::Ok)
            }
            _ => Err(RpcError::InvalidArgument(
                "unexpected agent terminal command".into(),
            )),
        }
    }

    fn require_gui_connection(&self) -> Result<()> {
        if self
            .host
            .registry()
            .connections
            .values()
            .any(|role| *role == ClientRole::Gui)
        {
            Ok(())
        } else {
            Err(RpcError::Busy(
                "Cannot reconnect Agent: GUI management connection is not registered".into(),
            ))
        }
    }

    fn input_lease(&self, key: AttachmentKey, connection: u64) -> Result<&Lease> {
        self.active
            .as_ref()
            .filter(|lease| lease.key == key && lease.connection == connection)
            .ok_or_else(stale_lease)
    }

    fn frame(&mut self, key: AttachmentKey, after: u64, offset: usize) -> Result<TerminalFrame> {
        let pending = self.pending.as_ref().is_some_and(|lease| lease.key == key);
        if !pending && !self.active.as_ref().is_some_and(|lease| lease.key == key) {
            return Err(stale_lease());
        }
        let transfer = if offset == 0 {
            let (sequence, snapshot, size, data) = self.terminal.frame(after);
            Some(Transfer {
                sequence,
                snapshot,
                size,
                data,
            })
        } else {
            None
        };
        let lease = if pending {
            self.pending.as_mut().unwrap()
        } else {
            self.active.as_mut().unwrap()
        };
        if let Some(transfer) = transfer {
            lease.transfer = Some(transfer);
        }
        let transfer = lease.transfer.as_ref().ok_or_else(|| {
            RpcError::InvalidArgument("no terminal frame transfer in progress".into())
        })?;
        if offset != 0 && transfer.sequence != after {
            return Err(RpcError::InvalidArgument(
                "terminal frame transfer changed".into(),
            ));
        }
        transfer.chunk(offset)
    }

    fn resize(&mut self, size: TerminalSize) -> Result<()> {
        self.pty
            .resize(size.cols, size.rows, 0, 0)
            .map_err(|_| RpcError::Io("unable to resize agent terminal".into()))?;
        self.terminal.resize(size);
        Ok(())
    }

    fn clear_disconnected(&mut self) {
        let registry = self.host.registry();
        let active_closed = self
            .active
            .as_ref()
            .is_some_and(|lease| !registry.connections.contains_key(&lease.connection));
        let pending_closed = self.pending.as_ref().is_some_and(|lease| {
            !registry.connections.contains_key(&lease.connection)
                || lease.prepared.elapsed() > ATTACH_GRACE
        });
        drop(registry);
        if pending_closed {
            self.pending = None;
        }
        if active_closed {
            self.active = None;
            self.update_view();
        }
    }

    fn update_view(&self) {
        let mut registry = self.host.registry();
        let active = self
            .active
            .as_ref()
            .filter(|lease| registry.connections.contains_key(&lease.connection));
        let attachment = active.map(|lease| {
            let identity = registry
                .peers
                .get(&lease.connection)
                .and_then(|peer| peer.identity);
            AgentAttachment {
                key: lease.key,
                relay_pid: identity.map(|(pid, _)| pid),
                relay_start: identity.map(|(_, start)| start),
                ready: lease.ready,
            }
        });
        let entry = registry.entries.get_mut(&self.instance).unwrap();
        entry.agent.view = None;
        entry.agent.attachment = attachment;
        if let Some(lease) = active.filter(|lease| lease.ready) {
            if let Some(surface) = lease.surface {
                entry.agent.view_owner = Some(AgentViewOwner {
                    key: lease.key,
                    surface,
                });
            }
        }
        entry.attachment_connection = active.map(|lease| lease.connection);
        entry.view_report = None;
        registry.revision += 1;
    }

    fn observe_processes(&mut self) {
        // shortcut: batch inventory and contain detached children before enabling managed GUI launch.
        let root = self.pty.child_pid() as u32;
        let mut observed = std::collections::HashSet::new();
        for (&pid, &start) in &self.owned {
            if flowmux_procmon::process_start_time(pid) == Some(start) {
                observed.extend(flowmux_procmon::descendants(pid).unwrap_or_default());
            }
        }
        // A forkpty session includes children orphaned when its root exits.
        #[cfg(target_os = "linux")]
        if self.owned.iter().any(|(&pid, &start)| {
            flowmux_procmon::process_start_time(pid) == Some(start)
                && unsafe { libc::getsid(pid as i32) } == root as i32
        }) {
            if let Ok(entries) = std::fs::read_dir("/proc") {
                for entry in entries.flatten() {
                    if let Some(pid) = entry
                        .file_name()
                        .to_str()
                        .and_then(|name| name.parse::<u32>().ok())
                        .filter(|pid| *pid <= i32::MAX as u32)
                    {
                        if unsafe { libc::getsid(pid as i32) } == root as i32 {
                            observed.insert(pid);
                        }
                    }
                }
            }
        }
        for pid in observed {
            if let Some(start) = flowmux_procmon::process_start_time(pid) {
                self.owned.entry(pid).or_insert(start);
            }
        }
    }

    fn reap_descendants(&mut self) {
        let root = self.pty.child_pid() as u32;
        self.owned.retain(|&pid, &mut start| {
            if pid == root && self.root_status.is_none() {
                return true;
            }
            if pid == root {
                return false;
            }
            if flowmux_procmon::process_start_time(pid) != Some(start) {
                return false;
            }
            let mut status = 0;
            // Only our adopted children can be reaped; ECHILD is harmless.
            if unsafe { libc::waitpid(pid as i32, &mut status, libc::WNOHANG) } == pid as i32 {
                return false;
            }
            #[cfg(target_os = "linux")]
            if std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(") ")
                        .map(|(_, fields)| fields.starts_with('Z'))
                })
                .unwrap_or(false)
            {
                return false;
            }
            true
        });
    }

    fn signal_owned(&self, signal: i32) {
        for (&pid, &start) in &self.owned {
            if let Err(error) = signal_process(pid, start, signal) {
                if error.raw_os_error() != Some(libc::ESRCH) {
                    self.fail("unable to signal owned agent process safely");
                }
            }
        }
    }

    fn fail(&self, reason: &str) {
        let mut registry = self.host.registry();
        registry.policy.observation_uncertain(self.instance);
        registry
            .entries
            .get_mut(&self.instance)
            .unwrap()
            .agent
            .error = Some(reason.into());
        registry.revision += 1;
    }

    fn validate_hook(
        &self,
        instance: AgentInstanceId,
        epoch: Uuid,
        pid: u32,
        process_start: u64,
        session_id: &str,
    ) -> Result<()> {
        if instance != self.instance || epoch != self.host.epoch {
            return Err(RpcError::NotFound(
                "Agent Host epoch or instance changed".into(),
            ));
        }
        // Only the launched root may replace the native session identity.
        if self.root_status.is_some()
            || self.stopping.is_some()
            || pid != self.pty.child_pid() as u32
            || self.owned.get(&pid) != Some(&process_start)
            || flowmux_procmon::process_start_time(pid) != Some(process_start)
        {
            return Err(RpcError::InvalidArgument(
                "agent process identity changed".into(),
            ));
        }
        if session_id.is_empty() || session_id.len() > 256 {
            return Err(RpcError::InvalidArgument(
                "invalid agent session identity".into(),
            ));
        }
        Ok(())
    }

    fn project_lifecycle(
        &mut self,
        transition: LifecycleTransition,
        notification: Option<flowmux_ipc::protocol::AgentLifecycleNotification>,
    ) {
        let notification_id = transition.report.as_ref().and_then(|report| {
            self.lifecycle.claim_transition_notification(
                self.instance,
                &report.name,
                report.session_id.as_deref()?,
                &transition,
            )
        });
        if let Some(grace) = transition.settle_codex_after_grace {
            self.settle_codex = Some((Instant::now() + Duration::from_millis(250), grace));
        }
        let completed = transition.completed;
        let completion_message = transition.completion_message;
        if let Some(report) = transition.report {
            let failed = report.activity == Some(flowmux_core::AgentActivity::Idle)
                && report.turn_outcome == Some(flowmux_core::AgentTurnOutcome::Failed);
            if let Some(presence) = &mut self.presence {
                presence.apply_report(report, false);
            } else {
                self.presence = AgentPresence::from_report(report, false);
            }
            let mut registry = self.host.registry();
            registry
                .entries
                .get_mut(&self.instance)
                .unwrap()
                .agent
                .presence = self.presence.clone();
            // Display completion is not proof that all provider work is finished.
            registry.policy.observation_uncertain(self.instance);
            registry.revision += 1;
            if let Some(id) = notification_id {
                let agent = &registry.entries[&self.instance].agent;
                let effect = notification.unwrap_or_else(|| {
                    flowmux_ipc::protocol::AgentLifecycleNotification {
                        title: format!(
                            "{} {}",
                            agent.provider,
                            if failed {
                                "failed"
                            } else if completed {
                                "ready"
                            } else {
                                "needs input"
                            }
                        ),
                        body: completion_message
                            .or_else(|| {
                                self.presence
                                    .as_ref()
                                    .and_then(|p| p.status_text().map(str::to_owned))
                            })
                            .unwrap_or_else(|| {
                                if failed {
                                    "Response failed"
                                } else if completed {
                                    "task complete"
                                } else {
                                    "Waiting for input"
                                }
                                .into()
                            }),
                        level: if failed {
                            flowmux_core::NotificationLevel::Error
                        } else if completed {
                            flowmux_core::NotificationLevel::TurnCompleted
                        } else {
                            flowmux_core::NotificationLevel::NeedsInput
                        },
                    }
                });
                let notification = ManagedNotification {
                    instance: self.instance,
                    epoch: agent.epoch,
                    provider: agent.provider.clone(),
                    label: agent.label.clone(),
                    notification: flowmux_core::Notification {
                        id,
                        level: effect.level,
                        title: effect.title,
                        body: effect.body,
                        source_pane: None,
                        created_at: chrono::Utc::now(),
                        read: false,
                    },
                };
                registry.push_notification(notification);
            }
        }
    }

    fn settle_lifecycle(&mut self) {
        if self.root_status.is_some() || self.stopping.is_some() {
            self.settle_codex = None;
            return;
        }
        if let Some(presence) = self
            .presence
            .as_ref()
            .filter(|presence| self.lifecycle.has_provider_turn(self.instance, presence))
        {
            let (screen, title) = self.terminal.status_signals();
            if let Some((seq, event)) = self.lifecycle.provider_screen_settlement(
                self.instance,
                presence,
                &screen,
                title.as_deref(),
            ) {
                if let Some(transition) = self.lifecycle.reduce(
                    self.instance,
                    &presence.name,
                    presence.pid,
                    seq,
                    presence.session_id.as_deref().unwrap(),
                    event,
                    Some(presence),
                ) {
                    self.project_lifecycle(transition, None);
                }
            }
        }
        if !self
            .settle_codex
            .as_ref()
            .is_some_and(|(deadline, _)| Instant::now() >= *deadline)
        {
            return;
        }
        let (_, grace) = self.settle_codex.take().unwrap();
        let Some(presence) = self.presence.as_ref() else {
            return;
        };
        let Some(session) = presence.session_id.as_deref() else {
            return;
        };
        if let Some(transition) = self.lifecycle.settle_codex(
            self.instance,
            presence.pid,
            grace.stop_seq,
            session,
            &grace.turn_id,
            Some(presence),
        ) {
            self.project_lifecycle(transition, None);
        }
    }

    fn finished(&mut self) {
        let (sequence, snapshot, size, data) = self.terminal.frame(0);
        let final_frame = self.active.as_mut().map(|lease| {
            (
                lease.key,
                Transfer {
                    sequence,
                    snapshot,
                    size,
                    data,
                },
                lease.transfer.take(),
            )
        });
        let mut registry = self.host.registry();
        registry.policy.remove_reaped(self.instance);
        let connection = self
            .active
            .as_ref()
            .map(|lease| lease.connection)
            .filter(|connection| registry.connections.contains_key(connection));
        let entry = registry.entries.get_mut(&self.instance).unwrap();
        entry.agent.state = ProcessState::Exited;
        entry.agent.presence = None;
        entry.agent.exit_status = self.root_status;
        entry.agent.view = None;
        entry.agent.attachment = None;
        entry.attachment_connection = connection;
        entry.view_report = None;
        entry.commands = None;
        entry.final_frame = connection.and(final_frame);
        registry.revision += 1;
    }
}

fn stale_lease() -> RpcError {
    RpcError::InvalidArgument(
        "terminal attachment is stale or belongs to another connection".into(),
    )
}

fn signal_process(pid: u32, start: u64, signal: i32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // Open first, then verify identity: reuse between verification and
        // signaling can only affect this pinned process, never a replacement.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        if flowmux_procmon::process_start_time(pid) != Some(start) {
            return Ok(());
        }
        if unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // shortcut: macOS has no pidfd; verify containment before enabling managed launch there.
        if flowmux_procmon::process_start_time(pid) == Some(start)
            && unsafe { libc::kill(pid as i32, signal) } < 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn read_fd(fd: i32, data: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(fd, data.as_mut_ptr().cast(), data.len()) };
    if n >= 0 {
        Ok(n as usize)
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EIO) {
            Ok(0)
        } else {
            Err(error)
        }
    }
}

fn write_fd(fd: i32, data: &[u8]) -> io::Result<usize> {
    let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
    if n >= 0 {
        Ok(n as usize)
    } else {
        Err(io::Error::last_os_error())
    }
}

pub async fn run(socket: &Path, idle_timeout: Duration) -> anyhow::Result<()> {
    let host = AgentHost::new(socket.to_owned(), idle_timeout);
    let monitor = tokio::spawn(host.clone().monitor());
    let result = flowmux_ipc::server::run_until(
        socket,
        Arc::new(HostHandler(host.clone())),
        host.shutdown.notified(),
    )
    .await;
    monitor.abort();
    let _ = monitor.await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, timeout};

    #[cfg(target_os = "linux")]
    #[test]
    fn pinned_signal_rejects_a_mismatched_process_birth_identity() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id();
        let start = flowmux_procmon::process_start_time(pid).unwrap();
        signal_process(pid, start.wrapping_add(1), libc::SIGTERM).unwrap();
        assert!(child.try_wait().unwrap().is_none());
        signal_process(pid, start, libc::SIGTERM).unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(child.wait().unwrap().signal(), Some(libc::SIGTERM));
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        host: Arc<AgentHost>,
        server: tokio::task::JoinHandle<anyhow::Result<()>>,
    }

    impl Fixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir_in("/tmp").unwrap();
            let socket = directory.path().join("host.sock");
            let host = AgentHost::new(socket.clone(), Duration::from_secs(1800));
            let handler = Arc::new(HostHandler(host.clone()));
            let task_host = host.clone();
            let server = tokio::spawn(async move {
                flowmux_ipc::server::run_until(&socket, handler, task_host.shutdown.notified())
                    .await
            });
            let fixture = Self {
                _directory: directory,
                host,
                server,
            };
            let _ = fixture.client(ClientRole::Observer).await;
            fixture
        }

        async fn client(&self, role: ClientRole) -> HostClient {
            timeout(Duration::from_secs(3), async {
                loop {
                    if let Ok(client) = HostClient::connect(&self.host.socket, role).await {
                        return client;
                    }
                    sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap()
        }

        fn launch(&self, script: &str) -> LaunchAgent {
            LaunchAgent {
                request_id: Uuid::now_v7(),
                provider: "fixture".into(),
                label: "isolated test agent".into(),
                argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
                cwd: self._directory.path().into(),
                env: vec![("TERM".into(), "xterm-256color".into())],
                size: TerminalSize { rows: 8, cols: 80 },
                fork_source: None,
            }
        }

        async fn launch_agent(&self, gui: &HostClient, script: &str) -> AgentInstance {
            let HostResponse::Launched { agent } = gui
                .call(HostRequest::Launch {
                    launch: self.launch(script),
                })
                .await
                .unwrap()
            else {
                panic!("expected launch")
            };
            assert_eq!(agent.state, ProcessState::Running);
            *agent
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.server.abort();
            for entry in self.host.registry().entries.values() {
                if let (Some(pid), Some(start)) = (entry.agent.pid, entry.agent.process_start) {
                    if flowmux_procmon::process_start_time(pid) == Some(start) {
                        unsafe {
                            libc::kill(-(pid as i32), libc::SIGKILL);
                        }
                    }
                }
            }
        }
    }

    fn lifecycle_event(
        agent: &AgentInstance,
        seq: u64,
        event: flowmux_ipc::protocol::AgentLifecycleEvent,
    ) -> HostRequest {
        HostRequest::Lifecycle {
            instance: agent.instance,
            epoch: agent.epoch,
            pid: agent.pid.unwrap(),
            process_start: agent.process_start.unwrap(),
            session_id: "same-native-session".into(),
            seq,
            event,
            notification: None,
        }
    }

    #[tokio::test]
    async fn explicit_notifications_require_execution_ancestry_without_changing_lifecycle() {
        let fixture = Fixture::new().await;
        let relay = fixture.client(ClientRole::Relay).await;
        let agent = fixture.launch_agent(&relay, "sleep 30").await;
        let observer = fixture.client(ClientRole::Observer).await;
        let notify = |epoch| HostRequest::Notify {
            instance: agent.instance,
            epoch,
            notification: flowmux_ipc::protocol::AgentLifecycleNotification {
                title: "explicit".into(),
                body: "completed a build, not the agent turn".into(),
                level: flowmux_core::NotificationLevel::TurnCompleted,
            },
        };
        assert!(observer
            .call(notify(agent.epoch))
            .await
            .unwrap_err()
            .to_string()
            .contains("does not belong"));
        let caller = connection_id(&observer, ClientRole::Observer).await;
        fixture
            .host
            .registry()
            .peers
            .get_mut(&caller)
            .unwrap()
            .identity = agent.pid.zip(agent.process_start);
        assert!(observer.call(notify(Uuid::new_v4())).await.is_err());
        let hook = fixture.client(ClientRole::Hook).await;
        let hook_id = connection_id(&hook, ClientRole::Hook).await;
        fixture
            .host
            .registry()
            .peers
            .get_mut(&hook_id)
            .unwrap()
            .identity = agent.pid.zip(agent.process_start);
        assert!(hook.call(notify(agent.epoch)).await.is_err());
        assert!(matches!(
            observer.call(notify(agent.epoch)).await.unwrap(),
            HostResponse::Ok
        ));
        let HostResponse::List {
            agents,
            notifications,
            ..
        } = observer.call(HostRequest::List).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(notifications.len(), 1);
        assert_eq!(notifications[0].instance, agent.instance);
        assert_eq!(notifications[0].notification.title, "explicit");
        assert_eq!(
            notifications[0].notification.level,
            flowmux_core::NotificationLevel::TurnCompleted
        );
        assert!(notifications[0].notification.source_pane.is_none());
        assert!(agents[0].view.is_none());
        assert!(
            agents[0].presence.is_none(),
            "explicit completion text is not lifecycle evidence"
        );
        assert_eq!(agents[0].state, ProcessState::Running);
        fixture
            .host
            .registry()
            .peers
            .get_mut(&caller)
            .unwrap()
            .identity
            .as_mut()
            .unwrap()
            .1 += 1;
        assert!(observer.call(notify(agent.epoch)).await.is_err());
    }

    #[tokio::test]
    async fn headless_notifications_are_causal_bounded_and_shared_by_registered_guis() {
        use flowmux_core::AgentWaitReason;
        use flowmux_ipc::protocol::AgentLifecycleEvent as Event;
        let fixture = Fixture::new().await;
        let launcher = fixture.client(ClientRole::Relay).await;
        let mut launch = fixture.launch("sleep 30");
        launch.provider = "claude".into();
        let HostResponse::Launched { agent } =
            launcher.call(HostRequest::Launch { launch }).await.unwrap()
        else {
            panic!()
        };
        drop(launcher);
        let hook = fixture.client(ClientRole::Hook).await;
        hook.call(lifecycle_event(
            &agent,
            1,
            Event::TurnStarted {
                turn_id: Some("root".into()),
                status_text: "Working".into(),
            },
        ))
        .await
        .unwrap();
        let wait = || Event::WaitStarted {
            item_id: "question".into(),
            reason: AgentWaitReason::Question,
            message: Some("Which branch?".into()),
            status_text: "Waiting for input".into(),
        };
        hook.call(lifecycle_event(&agent, 2, wait())).await.unwrap();
        let observer = fixture.client(ClientRole::Observer).await;
        let HostResponse::List { notifications, .. } =
            observer.call(HostRequest::List).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(notifications.len(), 1);
        let id = notifications[0].notification.id;
        assert_eq!(notifications[0].instance, agent.instance);
        assert!(notifications[0].notification.source_pane.is_none());
        let update = |action| HostRequest::UpdateNotifications {
            epoch: agent.epoch,
            ids: vec![id],
            action,
        };
        assert!(
            observer
                .call(update(NotificationAction::Read))
                .await
                .is_err(),
            "an observer alone must not impersonate a GUI"
        );
        assert!(hook.call(update(NotificationAction::Read)).await.is_err());
        let gui = fixture.client(ClientRole::Gui).await;
        let second = fixture.client(ClientRole::Gui).await;
        assert!(matches!(
            gui.call(HostRequest::ClaimNotification {
                epoch: agent.epoch,
                id
            })
            .await
            .unwrap(),
            HostResponse::NotificationClaimed { claimed: true }
        ));
        assert!(matches!(
            second
                .call(HostRequest::ClaimNotification {
                    epoch: agent.epoch,
                    id
                })
                .await
                .unwrap(),
            HostResponse::NotificationClaimed { claimed: false }
        ));
        observer
            .call(update(NotificationAction::Read))
            .await
            .unwrap();
        hook.call(lifecycle_event(&agent, 3, wait())).await.unwrap();
        let HostResponse::List { notifications, .. } =
            second.call(HostRequest::List).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(notifications.len(), 1);
        assert!(notifications[0].notification.read);
        observer
            .call(update(NotificationAction::Dismiss))
            .await
            .unwrap();
        hook.call(lifecycle_event(&agent, 4, wait())).await.unwrap();
        let HostResponse::List { notifications, .. } = gui.call(HostRequest::List).await.unwrap()
        else {
            panic!()
        };
        assert!(
            notifications.is_empty(),
            "a repeat must not resurrect dismissed effects"
        );
        hook.call(lifecycle_event(
            &agent,
            5,
            Event::WaitResolved {
                item_id: "question".into(),
            },
        ))
        .await
        .unwrap();
        hook.call(lifecycle_event(
            &agent,
            6,
            Event::TurnStopped {
                message: Some("finished".into()),
                status_text: "Ready".into(),
            },
        ))
        .await
        .unwrap();
        let HostResponse::List { notifications, .. } = gui.call(HostRequest::List).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(notifications.len(), 1);
        assert_eq!(
            notifications[0].notification.level,
            flowmux_core::NotificationLevel::TurnCompleted
        );
        let mut registry = fixture.host.registry();
        for _ in 0..100 {
            let mut n = notifications[0].clone();
            n.notification.id = flowmux_core::NotificationId::new();
            n.notification.body = "\u{1}".repeat(64 * 1024);
            registry.push_notification(n);
        }
        assert!(registry.notifications.len() <= MAX_NOTIFICATIONS);
        let bytes: usize = registry
            .notifications
            .iter()
            .map(|(n, _)| serde_json::to_vec(n).unwrap().len())
            .sum();
        assert!(bytes <= MAX_NOTIFICATION_LOG_BYTES);
    }

    #[tokio::test]
    async fn failed_turns_notify_once_without_a_gui_and_reset_for_new_work() {
        use flowmux_core::{AgentTurnOutcome, NotificationLevel};
        use flowmux_ipc::protocol::AgentLifecycleEvent as Event;
        let fixture = Fixture::new().await;
        let launcher = fixture.client(ClientRole::Relay).await;
        let mut launch = fixture.launch("sleep 30");
        launch.provider = "antigravity".into();
        let HostResponse::Launched { agent } =
            launcher.call(HostRequest::Launch { launch }).await.unwrap()
        else {
            panic!()
        };
        drop(launcher);
        let hook = fixture.client(ClientRole::Hook).await;
        let observer = fixture.client(ClientRole::Observer).await;
        let mut seq = 0;
        let mut ids = Vec::new();
        for turn in ["first", "second"] {
            seq += 1;
            hook.call(lifecycle_event(
                &agent,
                seq,
                Event::TurnStarted {
                    turn_id: Some(turn.into()),
                    status_text: "Working".into(),
                },
            ))
            .await
            .unwrap();
            for _ in 0..2 {
                seq += 1;
                hook.call(lifecycle_event(
                    &agent,
                    seq,
                    Event::TurnFinished {
                        outcome: AgentTurnOutcome::Failed,
                        message: Some("Fixture unauthorized".into()),
                        status_text: "Response failed".into(),
                    },
                ))
                .await
                .unwrap();
                let HostResponse::List { notifications, .. } =
                    observer.call(HostRequest::List).await.unwrap()
                else {
                    panic!()
                };
                assert_eq!(notifications.len(), ids.len() + 1);
                let notice = &notifications.last().unwrap().notification;
                assert_eq!(notice.level, NotificationLevel::Error);
                assert_eq!(notice.title, "antigravity failed");
                assert_eq!(notice.body, "Fixture unauthorized");
                assert!(notice.source_pane.is_none());
            }
            let HostResponse::List { notifications, .. } =
                observer.call(HostRequest::List).await.unwrap()
            else {
                panic!()
            };
            let id = notifications.last().unwrap().notification.id;
            assert!(!ids.contains(&id));
            ids.push(id);
        }
    }

    #[tokio::test]
    async fn explicit_stop_checks_epoch_and_waits_for_user_force_after_grace() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let agent = fixture.launch_agent(&gui,
            "trap '' TERM; stty raw -echo; printf READY; byte=$(dd bs=1 count=1 2>/dev/null | od -An -tu1); printf 'INTERRUPT:%s:END' \"$byte\"; while :; do sleep 1; done").await;
        // The unrelated execution must not expire while the stop/grace checks run.
        let other = fixture
            .launch_agent(&gui, "while IFS= read -r line; do :; done")
            .await;
        let relay = fixture.client(ClientRole::Relay).await;
        let key = attach(&relay, &agent).await;
        wait_text(&relay, key, "READY").await;
        assert!(gui
            .call(HostRequest::Terminate {
                instance: agent.instance,
                epoch: Uuid::new_v4(),
                mode: TerminateMode::Terminate,
            })
            .await
            .is_err());
        assert!(gui
            .call(HostRequest::Terminate {
                instance: agent.instance,
                epoch: agent.epoch,
                mode: TerminateMode::Force,
            })
            .await
            .is_err());
        assert_eq!(
            fixture.host.registry().entries[&agent.instance].agent.state,
            ProcessState::Running
        );
        gui.call(HostRequest::Terminate {
            instance: agent.instance,
            epoch: agent.epoch,
            mode: TerminateMode::Interrupt,
        })
        .await
        .unwrap();
        wait_text(&relay, key, "INTERRUPT:   3:END").await;
        assert_eq!(
            fixture.host.registry().entries[&agent.instance].agent.state,
            ProcessState::Running
        );
        for _ in 0..2 {
            gui.call(HostRequest::Terminate {
                instance: agent.instance,
                epoch: agent.epoch,
                mode: TerminateMode::Terminate,
            })
            .await
            .unwrap();
        }
        assert!(gui
            .call(HostRequest::Terminate {
                instance: agent.instance,
                epoch: agent.epoch,
                mode: TerminateMode::Force,
            })
            .await
            .is_err());
        assert!(gui
            .call(HostRequest::Terminate {
                instance: agent.instance,
                epoch: agent.epoch,
                mode: TerminateMode::Interrupt,
            })
            .await
            .is_err());
        timeout(Duration::from_secs(8), async {
            loop {
                if fixture.host.registry().entries[&agent.instance]
                    .agent
                    .force_terminate_allowed
                {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            fixture.host.registry().entries[&agent.instance].agent.state,
            ProcessState::Stopping
        );
        assert_eq!(
            flowmux_procmon::process_start_time(agent.pid.unwrap()),
            agent.process_start
        );
        gui.call(HostRequest::Terminate {
            instance: agent.instance,
            epoch: agent.epoch,
            mode: TerminateMode::Force,
        })
        .await
        .unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if fixture.host.registry().entries[&agent.instance].agent.state
                    == ProcessState::Exited
                {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(!flowmux_procmon::pid_alive(agent.pid.unwrap()));
        assert_eq!(
            fixture.host.registry().entries[&other.instance].agent.state,
            ProcessState::Running
        );
        assert_eq!(
            flowmux_procmon::process_start_time(other.pid.unwrap()),
            other.process_start
        );
    }

    #[tokio::test]
    async fn retirement_waits_for_gui_to_read_final_registry_but_not_for_observers() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let observer = fixture.client(ClientRole::Observer).await;
        let agent = fixture.launch_agent(&gui, "exit 0").await;
        let host = fixture.host.clone();
        let monitor = tokio::spawn(async move { host.monitor().await });
        timeout(Duration::from_secs(3), async {
            loop {
                if fixture.host.registry().entries[&agent.instance]
                    .commands
                    .is_none()
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let HostResponse::List { agents, .. } = observer.call(HostRequest::List).await.unwrap()
        else {
            panic!("expected final registry");
        };
        assert_eq!(agents[0].state, ProcessState::Exited);
        sleep(Duration::from_millis(350)).await;
        assert!(
            !monitor.is_finished(),
            "observer reads cannot acknowledge a GUI's final state"
        );
        let HostResponse::List { agents, .. } = gui.call(HostRequest::List).await.unwrap() else {
            panic!("expected final registry");
        };
        assert_eq!(agents[0].state, ProcessState::Exited);
        timeout(Duration::from_secs(1), monitor)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn fork_admission_checks_current_source_but_preserves_accepted_request_replay() {
        use flowmux_core::{AgentActivity, AgentStatus};
        use flowmux_ipc::agent_host::ForkSource;
        let fixture = Fixture::new().await;
        let launcher = fixture.client(ClientRole::Relay).await;
        let source = fixture.launch_agent(&launcher, "sleep 30").await;
        let mut presence = AgentPresence::new("fixture", AgentActivity::Idle, None);
        presence.session_id = Some("parent".into());
        fixture
            .host
            .registry()
            .entries
            .get_mut(&source.instance)
            .unwrap()
            .agent
            .presence = Some(presence);
        let mut launch = fixture.launch("sleep 30");
        launch.fork_source = Some(ForkSource {
            instance: source.instance,
            epoch: source.epoch,
            session_id: "parent".into(),
        });
        for variant in 0..4 {
            let mut invalid = launch.clone();
            let guard = invalid.fork_source.as_mut().unwrap();
            match variant {
                0 => guard.epoch = Uuid::new_v4(),
                1 => guard.instance = AgentInstanceId::new(),
                2 => guard.session_id = "different".into(),
                _ => invalid.provider = "other".into(),
            }
            assert!(launcher
                .call(HostRequest::Launch { launch: invalid })
                .await
                .is_err());
            assert_eq!(fixture.host.registry().entries.len(), 1);
        }
        for status in [
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Unknown,
        ] {
            fixture
                .host
                .registry()
                .entries
                .get_mut(&source.instance)
                .unwrap()
                .agent
                .presence
                .as_mut()
                .unwrap()
                .status = status;
            assert!(launcher
                .call(HostRequest::Launch {
                    launch: launch.clone()
                })
                .await
                .is_err());
            assert_eq!(fixture.host.registry().entries.len(), 1);
        }
        fixture
            .host
            .registry()
            .entries
            .get_mut(&source.instance)
            .unwrap()
            .agent
            .presence
            .as_mut()
            .unwrap()
            .status = AgentStatus::Idle;
        let HostResponse::Launched { agent } = launcher
            .call(HostRequest::Launch {
                launch: launch.clone(),
            })
            .await
            .unwrap()
        else {
            panic!("expected launch");
        };
        fixture
            .host
            .registry()
            .entries
            .get_mut(&source.instance)
            .unwrap()
            .agent
            .presence = None;
        let HostResponse::Launched { agent: replay } = launcher
            .call(HostRequest::Launch {
                launch: launch.clone(),
            })
            .await
            .unwrap()
        else {
            panic!("expected replay");
        };
        assert_eq!(agent.instance, replay.instance);
        assert_eq!(fixture.host.registry().entries.len(), 2);
        launch.request_id = Uuid::now_v7();
        assert!(launcher.call(HostRequest::Launch { launch }).await.is_err());
        assert_eq!(fixture.host.registry().entries.len(), 2);
    }

    #[tokio::test]
    async fn native_session_replacement_and_end_reject_stale_events_without_stopping_root() {
        use flowmux_core::{AgentStatus, AgentWaitReason};
        use flowmux_ipc::protocol::AgentLifecycleEvent as Event;
        let fixture = Fixture::new().await;
        let launcher = fixture.client(ClientRole::Relay).await;
        let agent = fixture.launch_agent(&launcher, "sleep 30").await;
        let hook = fixture.client(ClientRole::Hook).await;
        let session = |name: &str, seq, event| HostRequest::NativeSession {
            instance: agent.instance,
            epoch: agent.epoch,
            pid: agent.pid.unwrap(),
            process_start: agent.process_start.unwrap(),
            session_id: name.into(),
            seq,
            event,
        };
        let started = || NativeSessionEvent::Started {
            status: AgentStatus::Idle,
        };
        assert!(matches!(
            hook.call(session("same-native-session", 1, started()))
                .await
                .unwrap(),
            HostResponse::LifecycleReceipt { applied: true }
        ));
        hook.call(lifecycle_event(
            &agent,
            2,
            Event::WaitStarted {
                item_id: "question".into(),
                reason: AgentWaitReason::Question,
                message: None,
                status_text: "Waiting for input".into(),
            },
        ))
        .await
        .unwrap();
        assert!(matches!(
            hook.call(session("same-native-session", 3, started()))
                .await
                .unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        assert_eq!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .presence
                .as_ref()
                .unwrap()
                .pending_waits
                .len(),
            1
        );
        // Invalid boundaries must not erase the current wait ledger.
        let mut invalid = session("replacement", 4, started());
        if let HostRequest::NativeSession { process_start, .. } = &mut invalid {
            *process_start += 1;
        }
        assert!(hook.call(invalid).await.is_err());
        assert!(matches!(
            hook.call(session("replacement", 1, started()))
                .await
                .unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        assert!(matches!(
            hook.call(session("replacement", 4, started()))
                .await
                .unwrap(),
            HostResponse::LifecycleReceipt { applied: true }
        ));
        assert!(matches!(
            hook.call(lifecycle_event(
                &agent,
                5,
                Event::TurnStopped {
                    message: None,
                    status_text: "Completed".into(),
                }
            ))
            .await
            .unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        assert!(matches!(
            hook.call(session("same-native-session", 6, NativeSessionEvent::Ended))
                .await
                .unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        assert!(matches!(
            hook.call(session("replacement", 7, NativeSessionEvent::Ended))
                .await
                .unwrap(),
            HostResponse::LifecycleReceipt { applied: true }
        ));
        let mut late = lifecycle_event(
            &agent,
            8,
            Event::ProgressObserved {
                status_text: "Working".into(),
            },
        );
        if let HostRequest::Lifecycle { session_id, .. } = &mut late {
            *session_id = "replacement".into();
        }
        assert!(matches!(
            hook.call(late).await.unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        let mut unstarted = lifecycle_event(
            &agent,
            9,
            Event::ProgressObserved {
                status_text: "Working".into(),
            },
        );
        if let HostRequest::Lifecycle { session_id, .. } = &mut unstarted {
            *session_id = "session-without-a-start".into();
        }
        assert!(matches!(
            hook.call(unstarted).await.unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        assert!(matches!(
            hook.call(session("older", 6, started())).await.unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        {
            let registry = fixture.host.registry();
            let entry = &registry.entries[&agent.instance];
            assert!(entry.agent.presence.is_none());
            assert_eq!(entry.agent.state, ProcessState::Running);
            assert!(registry.policy.deadline(agent.instance).is_none());
        }
        assert!(matches!(
            hook.call(session("replacement", 9, started()))
                .await
                .unwrap(),
            HostResponse::LifecycleReceipt { applied: true }
        ));
    }

    #[tokio::test]
    async fn detached_lifecycle_keeps_waits_and_settles_codex_without_a_gui() {
        use flowmux_core::{AgentStatus, AgentWaitReason};
        use flowmux_ipc::protocol::AgentLifecycleEvent as Event;
        let fixture = Fixture::new().await;
        let launcher = fixture.client(ClientRole::Relay).await;
        let mut launch = fixture.launch("while IFS= read -r line; do :; done");
        launch.provider = "codex".into();
        let HostResponse::Launched { agent } =
            launcher.call(HostRequest::Launch { launch }).await.unwrap()
        else {
            panic!("expected launch");
        };
        drop(launcher);
        let hook = fixture.client(ClientRole::Hook).await;
        let events = [
            (
                1,
                Event::TurnStarted {
                    turn_id: Some("parent".into()),
                    status_text: "Working".into(),
                },
            ),
            (
                100,
                Event::CodexSubagentStarted {
                    agent_id: "child".into(),
                    turn_id: "child-turn".into(),
                },
            ),
            (
                101,
                Event::PermissionWaitStarted {
                    reason: AgentWaitReason::Approval,
                    scope: Some("child:child:child-turn".into()),
                    message: None,
                    status_text: "Approval required".into(),
                },
            ),
            (
                2,
                Event::CodexTurnStopped {
                    turn_id: "parent".into(),
                    message: None,
                    status_text: "Completed".into(),
                    stop_hook_active: false,
                },
            ),
        ];
        for (seq, event) in events {
            assert!(matches!(
                hook.call(lifecycle_event(&agent, seq, event))
                    .await
                    .unwrap(),
                HostResponse::LifecycleReceipt { applied: true }
            ));
        }
        sleep(Duration::from_millis(600)).await;
        {
            let registry = fixture.host.registry();
            assert!(!registry
                .connections
                .values()
                .any(|role| *role == ClientRole::Gui));
            let presence = registry.entries[&agent.instance]
                .agent
                .presence
                .as_ref()
                .unwrap();
            assert_eq!(presence.status, AgentStatus::Blocked);
            assert_eq!(presence.pending_waits.len(), 1);
        }
        hook.call(lifecycle_event(
            &agent,
            102,
            Event::CodexSubagentStopped {
                agent_id: "child".into(),
                turn_id: "child-turn".into(),
            },
        ))
        .await
        .unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if fixture.host.registry().entries[&agent.instance]
                    .agent
                    .presence
                    .as_ref()
                    .is_some_and(|presence| presence.status == AgentStatus::Idle)
                {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Host must settle Stop without a GUI timer");
        assert!(fixture
            .host
            .registry()
            .policy
            .deadline(agent.instance)
            .is_none());
        // A duplicate cannot complete again, and old settlement cannot finish new work.
        assert!(matches!(
            hook.call(lifecycle_event(
                &agent,
                2,
                Event::CodexTurnStopped {
                    turn_id: "parent".into(),
                    message: None,
                    status_text: "Completed".into(),
                    stop_hook_active: false
                }
            ))
            .await
            .unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        hook.call(lifecycle_event(
            &agent,
            3,
            Event::TurnStarted {
                turn_id: Some("next".into()),
                status_text: "Working".into(),
            },
        ))
        .await
        .unwrap();
        hook.call(lifecycle_event(
            &agent,
            4,
            Event::CodexTurnStopped {
                turn_id: "next".into(),
                message: None,
                status_text: "Completed".into(),
                stop_hook_active: false,
            },
        ))
        .await
        .unwrap();
        hook.call(lifecycle_event(
            &agent,
            5,
            Event::TurnStarted {
                turn_id: Some("newer".into()),
                status_text: "Working".into(),
            },
        ))
        .await
        .unwrap();
        sleep(Duration::from_millis(600)).await;
        let registry = fixture.host.registry();
        let entry = &registry.entries[&agent.instance];
        assert_eq!(
            entry.agent.presence.as_ref().unwrap().status,
            AgentStatus::Working
        );
        assert_eq!(entry.agent.state, ProcessState::Running);
    }

    #[tokio::test]
    async fn lifecycle_ingress_rejects_foreign_epoch_process_and_session_without_mutation() {
        use flowmux_ipc::protocol::AgentLifecycleEvent as Event;
        let fixture = Fixture::new().await;
        let launcher = fixture.client(ClientRole::Relay).await;
        let first = fixture.launch_agent(&launcher, "sleep 30").await;
        let second = fixture.launch_agent(&launcher, "sleep 30").await;
        let hook = fixture.client(ClientRole::Hook).await;
        let start = || Event::TurnStarted {
            turn_id: Some("turn".into()),
            status_text: "Working".into(),
        };
        hook.call(lifecycle_event(&first, 1, start()))
            .await
            .unwrap();
        let mut invalid = lifecycle_event(&first, 100, start());
        if let HostRequest::Lifecycle { epoch, .. } = &mut invalid {
            *epoch = Uuid::new_v4();
        }
        assert!(hook.call(invalid).await.is_err());
        let mut foreign = first.clone();
        foreign.instance = second.instance;
        assert!(hook
            .call(lifecycle_event(&foreign, 100, start()))
            .await
            .is_err());
        for session in [String::new(), "x".repeat(257)] {
            let mut invalid = lifecycle_event(&first, 100, start());
            if let HostRequest::Lifecycle { session_id, .. } = &mut invalid {
                *session_id = session;
            }
            assert!(hook.call(invalid).await.is_err());
        }
        assert!(hook
            .call(lifecycle_event(
                &first,
                100,
                Event::ProgressObserved {
                    status_text: "x".repeat(64 * 1024),
                }
            ))
            .await
            .is_err());
        foreign = first.clone();
        foreign.process_start = foreign.process_start.map(|start| start.wrapping_add(1));
        assert!(hook
            .call(lifecycle_event(&foreign, 100, start()))
            .await
            .is_err());
        assert!(hook
            .call(HostRequest::Terminate {
                instance: first.instance,
                epoch: first.epoch,
                mode: TerminateMode::Force
            })
            .await
            .is_err());
        let mut wrong_session = lifecycle_event(&first, 100, start());
        if let HostRequest::Lifecycle { session_id, .. } = &mut wrong_session {
            *session_id = "foreign-session".into();
        }
        assert!(matches!(
            hook.call(wrong_session).await.unwrap(),
            HostResponse::LifecycleReceipt { applied: false }
        ));
        let observer = fixture.client(ClientRole::Observer).await;
        assert!(observer
            .call(lifecycle_event(&first, 2, start()))
            .await
            .is_err());
        let HostResponse::List { agents, .. } = observer.call(HostRequest::List).await.unwrap()
        else {
            panic!("expected list");
        };
        assert_eq!(
            agents
                .iter()
                .find(|agent| agent.instance == first.instance)
                .unwrap()
                .presence
                .as_ref()
                .unwrap()
                .seq,
            Some(1)
        );
        assert!(agents
            .iter()
            .find(|agent| agent.instance == second.instance)
            .unwrap()
            .presence
            .is_none());
        assert!(matches!(
            hook.call(lifecycle_event(&first, 2, start()))
                .await
                .unwrap(),
            HostResponse::LifecycleReceipt { applied: true }
        ));
        hook.call(lifecycle_event(&second, 1, start()))
            .await
            .unwrap();
    }

    async fn prepare(
        client: &HostClient,
        agent: &AgentInstance,
        replace: Option<AttachmentKey>,
    ) -> AttachmentKey {
        let HostResponse::Prepared { key, .. } = client
            .call(HostRequest::PrepareAttach {
                instance: agent.instance,
                epoch: agent.epoch,
                request_id: Uuid::new_v4(),
                view: None,
                surface: None,
                require_gui: false,
                gui_ready: false,
                replace,
            })
            .await
            .unwrap()
        else {
            panic!("expected prepared")
        };
        key
    }

    async fn attach(client: &HostClient, agent: &AgentInstance) -> AttachmentKey {
        let key = prepare(client, agent, None).await;
        commit_ready(client, key).await;
        key
    }

    async fn commit_ready(client: &HostClient, key: AttachmentKey) {
        let HostResponse::Attached { frame, .. } = client
            .call(HostRequest::CommitAttach {
                key,
                size: TerminalSize { rows: 8, cols: 80 },
            })
            .await
            .unwrap()
        else {
            panic!("expected attached");
        };
        client
            .call(HostRequest::ViewReady {
                key,
                sequence: frame.sequence,
            })
            .await
            .unwrap();
    }

    async fn connection_id(client: &HostClient, role: ClientRole) -> u64 {
        let HostResponse::Hello { connection, .. } = client
            .call(HostRequest::Hello {
                version: PROTOCOL_VERSION,
                role,
            })
            .await
            .unwrap()
        else {
            panic!("expected hello");
        };
        connection
    }

    fn view_report(key: AttachmentKey) -> AgentViewReport {
        AgentViewReport {
            key,
            location: AgentViewLocation {
                window: Uuid::new_v4(),
                workspace: flowmux_core::WorkspaceId::new(),
                pane: flowmux_core::PaneId::new(),
                surface: flowmux_core::SurfaceId::new(),
                visible: true,
            },
        }
    }

    #[tokio::test]
    async fn snapshot_ready_requires_current_owner_and_gates_input_and_location() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let relay = fixture.client(ClientRole::Relay).await;
        let stranger = fixture.client(ClientRole::Relay).await;
        let caller = fixture.client(ClientRole::Observer).await;
        let agent = fixture
            .launch_agent(
                &relay,
                "stty -echo; while IFS= read -r line; do printf 'ACK:%s\\n' \"$line\"; done",
            )
            .await;
        let caller_id = connection_id(&caller, ClientRole::Observer).await;
        fixture
            .host
            .registry()
            .peers
            .get_mut(&caller_id)
            .unwrap()
            .identity = agent.pid.zip(agent.process_start);
        let context = || HostRequest::ResolveContext {
            instance: agent.instance,
            epoch: agent.epoch,
            session_id: None,
        };
        let key = prepare(&relay, &agent, None).await;
        assert!(relay
            .call(HostRequest::ViewReady { key, sequence: 0 })
            .await
            .is_err());
        let HostResponse::Attached { frame, .. } = relay
            .call(HostRequest::CommitAttach {
                key,
                size: TerminalSize { rows: 8, cols: 80 },
            })
            .await
            .unwrap()
        else {
            panic!("expected attached");
        };
        let report = view_report(key);
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        {
            let registry = fixture.host.registry();
            let agent = &registry.entries[&agent.instance].agent;
            assert!(!agent.attachment.as_ref().unwrap().ready);
            assert!(agent.view.is_none());
        }
        assert!(caller
            .call(context())
            .await
            .unwrap_err()
            .to_string()
            .contains("no_attached_view"));
        assert!(relay
            .call(HostRequest::Input {
                key,
                sequence: 1,
                data: b"premature\n".to_vec()
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("not ready"));
        let ready = HostRequest::ViewReady {
            key,
            sequence: frame.sequence,
        };
        assert!(stranger.call(ready.clone()).await.is_err());
        assert!(caller.call(ready.clone()).await.is_err());
        for sequence in [frame.sequence.saturating_sub(1), frame.sequence + 1] {
            assert!(relay
                .call(HostRequest::ViewReady { key, sequence })
                .await
                .is_err());
        }
        for stale in [
            AttachmentKey {
                generation: key.generation + 1,
                ..key
            },
            AttachmentKey {
                epoch: Uuid::new_v4(),
                ..key
            },
        ] {
            assert!(relay
                .call(HostRequest::ViewReady {
                    key: stale,
                    sequence: frame.sequence
                })
                .await
                .is_err());
        }
        relay.call(ready.clone()).await.unwrap();
        let revision = fixture.host.registry().revision;
        relay.call(ready).await.unwrap();
        assert_eq!(
            fixture.host.registry().revision,
            revision,
            "duplicate ready is idempotent"
        );
        assert!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .attachment
                .as_ref()
                .unwrap()
                .ready
        );
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        assert!(
            matches!(caller.call(context()).await.unwrap(), HostResponse::Context { key: current, .. } if current == key)
        );
        // The rejected input must not consume sequence 1 or reach the provider.
        relay
            .call(HostRequest::Input {
                key,
                sequence: 1,
                data: b"fresh\n".to_vec(),
            })
            .await
            .unwrap();
        wait_text(&relay, key, "ACK:fresh").await;
        let replacement = prepare(&stranger, &agent, Some(key)).await;
        commit_ready(&stranger, replacement).await;
        assert!(relay
            .call(HostRequest::ViewReady {
                key,
                sequence: frame.sequence
            })
            .await
            .is_err());
        gui.call(HostRequest::ReportViews {
            views: vec![report],
        })
        .await
        .unwrap();
        assert!(fixture.host.registry().entries[&agent.instance]
            .agent
            .view
            .is_none());
        stranger
            .call(HostRequest::Detach { key: replacement })
            .await
            .unwrap();
        assert!(stranger
            .call(HostRequest::ViewReady {
                key: replacement,
                sequence: frame.sequence
            })
            .await
            .is_err());
        assert_eq!(
            flowmux_procmon::process_start_time(agent.pid.unwrap()),
            agent.process_start
        );
    }

    #[test]
    fn managed_context_ancestry_checks_root_birth_and_descendants() {
        let root = std::process::id();
        let birth = flowmux_procmon::process_start_time(root).unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(process_belongs_to_root(root, root, birth));
        assert!(process_belongs_to_root(child.id(), root, birth));
        assert!(!process_belongs_to_root(
            child.id(),
            root,
            birth.wrapping_add(1)
        ));
        assert!(!process_belongs_to_root(
            root,
            child.id(),
            flowmux_procmon::process_start_time(child.id()).unwrap()
        ));
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[tokio::test]
    async fn managed_context_requires_owned_caller_and_current_live_view() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let relay = fixture.client(ClientRole::Relay).await;
        let observer = fixture.client(ClientRole::Observer).await;
        let agent = fixture.launch_agent(&relay, "sleep 30").await;
        let key = attach(&relay, &agent).await;
        let mut report = view_report(key);
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        let query = || HostRequest::ResolveContext {
            instance: agent.instance,
            epoch: agent.epoch,
            session_id: None,
        };
        // A real socket peer outside the execution cannot borrow its environment.
        assert!(observer
            .call(query())
            .await
            .unwrap_err()
            .to_string()
            .contains("does not belong"));
        let caller = connection_id(&observer, ClientRole::Observer).await;
        let gui_id = connection_id(&gui, ClientRole::Gui).await;
        let relay_id = connection_id(&relay, ClientRole::Relay).await;
        // Inject only the caller identity to exercise registry invalidation below.
        fixture
            .host
            .registry()
            .peers
            .get_mut(&caller)
            .unwrap()
            .identity = agent.pid.zip(agent.process_start);
        let HostResponse::Context {
            key: resolved,
            location,
            socket,
        } = observer.call(query()).await.unwrap()
        else {
            panic!("expected context")
        };
        assert_eq!(resolved, key);
        assert_eq!(location, report.location);
        assert_eq!(
            socket,
            flowmux_config::paths::runtime_socket_for_pid(std::process::id())
        );
        assert!(observer
            .call(HostRequest::ResolveContext {
                epoch: Uuid::new_v4(),
                instance: agent.instance,
                session_id: None
            })
            .await
            .is_err());
        for session_id in ["", "wrong-session"] {
            assert!(observer
                .call(HostRequest::ResolveContext {
                    epoch: agent.epoch,
                    instance: agent.instance,
                    session_id: Some(session_id.into())
                })
                .await
                .is_err());
        }
        let dispatch = |context, request| HostRequest::DispatchContext {
            context,
            session_id: None,
            request: Box::new(request),
        };
        assert!(observer
            .call(dispatch(
                report.clone(),
                Request::AgentHost {
                    request: HostRequest::KeepAlive
                }
            ))
            .await
            .unwrap_err()
            .to_string()
            .contains("does not support"));
        let gate = fixture.host.attachment_gate(agent.instance).unwrap();
        for request in [
            dispatch(report.clone(), Request::Ping),
            dispatch(
                report.clone(),
                Request::WorkspaceCreate {
                    name: None,
                    root: std::env::temp_dir(),
                    team: true,
                    background: true,
                },
            ),
            dispatch(
                report.clone(),
                Request::WorkspaceFocus {
                    workspace: report.location.workspace,
                },
            ),
            dispatch(
                report.clone(),
                Request::AgentViewOpen {
                    pane: report.location.pane,
                    instance: agent.instance,
                    expected_attachment: None,
                },
            ),
            HostRequest::PrepareAttach {
                instance: agent.instance,
                epoch: agent.epoch,
                request_id: Uuid::new_v4(),
                view: None,
                surface: Some(flowmux_core::SurfaceId::new()),
                require_gui: true,
                gui_ready: false,
                replace: None,
            },
            HostRequest::CommitAttach {
                key,
                size: TerminalSize::default(),
            },
            HostRequest::ViewReady { key, sequence: 0 },
            HostRequest::AbortAttach { key },
            HostRequest::Detach { key },
        ] {
            let connection = if matches!(request, HostRequest::DispatchContext { .. }) {
                caller
            } else {
                relay_id
            };
            assert!(
                matches!(fixture.host.request(request, connection, None).await,
                Err(RpcError::Busy(message)) if message.contains("request not started"))
            );
        }
        assert!(
            relay
                .call(HostRequest::Frame {
                    key,
                    after: 0,
                    offset: 0
                })
                .await
                .is_ok(),
            "view gates must not block terminal observations"
        );
        drop(gate);
        let mut stale = report.clone();
        stale.location.pane = flowmux_core::PaneId::new();
        for request in [
            Request::Ping,
            Request::WorkspaceCreate {
                name: None,
                root: std::env::temp_dir(),
                team: false,
                background: false,
            },
            Request::WorkspaceFocus {
                workspace: report.location.workspace,
            },
            Request::AgentViewOpen {
                pane: report.location.pane,
                instance: agent.instance,
                expected_attachment: None,
            },
        ] {
            assert!(observer
                .call(dispatch(stale.clone(), request))
                .await
                .unwrap_err()
                .to_string()
                .contains("source moved"));
        }
        report.location.pane = flowmux_core::PaneId::new();
        report.location.surface = flowmux_core::SurfaceId::new();
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        let HostResponse::Context { location, .. } = observer.call(query()).await.unwrap() else {
            panic!("expected moved context")
        };
        assert_eq!(location, report.location);
        for peer_id in [gui_id, relay_id] {
            let original = fixture.host.registry().peers[&peer_id].identity;
            fixture
                .host
                .registry()
                .peers
                .get_mut(&peer_id)
                .unwrap()
                .identity = original.map(|(pid, birth)| (pid, birth.wrapping_add(1)));
            assert!(observer
                .call(query())
                .await
                .unwrap_err()
                .to_string()
                .contains("no_attached_view"));
            fixture
                .host
                .registry()
                .peers
                .get_mut(&peer_id)
                .unwrap()
                .identity = original;
        }
        fixture
            .host
            .registry()
            .entries
            .get_mut(&agent.instance)
            .unwrap()
            .view_report = Some((gui_id, Instant::now() - VIEW_REPORT_TIMEOUT));
        assert!(observer
            .call(query())
            .await
            .unwrap_err()
            .to_string()
            .contains("no_attached_view"));
        gui.call(HostRequest::ReportViews {
            views: vec![report],
        })
        .await
        .unwrap();
        HostHandler(fixture.host.clone()).connection_closed(ConnectionContext {
            id: gui_id,
            peer_pid: None,
        });
        assert!(observer
            .call(query())
            .await
            .unwrap_err()
            .to_string()
            .contains("no_attached_view"));
        assert_eq!(
            fixture.host.registry().entries[&agent.instance].agent.pid,
            agent.pid
        );
    }

    #[tokio::test]
    async fn view_snapshots_require_gui_relay_ownership_and_are_atomic() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let foreign = fixture.client(ClientRole::Gui).await;
        let relay = fixture.client(ClientRole::Relay).await;
        let agent = fixture.launch_agent(&relay, "sleep 30").await;
        let key = attach(&relay, &agent).await;
        let report = view_report(key);
        let request = || HostRequest::ReportViews {
            views: vec![report.clone()],
        };
        for role in [ClientRole::Observer, ClientRole::Relay, ClientRole::Hook] {
            assert!(fixture.client(role).await.call(request()).await.is_err());
        }
        let foreign_id = connection_id(&foreign, ClientRole::Gui).await;
        fixture
            .host
            .registry()
            .peers
            .get_mut(&foreign_id)
            .unwrap()
            .identity = Some((1, 1));
        assert!(foreign.call(request()).await.is_err());
        assert!(fixture.host.registry().entries[&agent.instance]
            .agent
            .view
            .is_none());
        gui.call(request()).await.unwrap();
        let revision = fixture.host.registry().revision;
        gui.call(request()).await.unwrap();
        assert_eq!(
            fixture.host.registry().revision,
            revision,
            "heartbeat is not a location change"
        );
        for invalid in [
            vec![report.clone(), report.clone()],
            vec![AgentViewReport {
                location: AgentViewLocation {
                    pane: flowmux_core::PaneId(Uuid::nil()),
                    ..report.location.clone()
                },
                ..report.clone()
            }],
            vec![report.clone(); MAX_INSTANCES + 1],
        ] {
            assert!(gui
                .call(HostRequest::ReportViews { views: invalid })
                .await
                .is_err());
            assert_eq!(fixture.host.registry().revision, revision);
            assert_eq!(
                fixture.host.registry().entries[&agent.instance]
                    .agent
                    .view
                    .as_ref(),
                Some(&report.location)
            );
        }
        gui.call(HostRequest::ReportViews { views: Vec::new() })
            .await
            .unwrap();
        let registry = fixture.host.registry();
        assert!(registry.entries[&agent.instance].agent.view.is_none());
        assert_eq!(
            registry.entries[&agent.instance]
                .agent
                .attachment
                .as_ref()
                .unwrap()
                .key,
            key
        );
    }

    #[tokio::test]
    async fn view_heartbeat_expiry_and_gui_eof_clear_only_owned_locations() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let other_gui = fixture.client(ClientRole::Gui).await;
        let relay = fixture.client(ClientRole::Relay).await;
        let agent = fixture.launch_agent(&relay, "sleep 30").await;
        let key = attach(&relay, &agent).await;
        let mut report = view_report(key);
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        report.location.visible = false;
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        {
            let mut registry = fixture.host.registry();
            assert_eq!(
                registry.entries[&agent.instance].agent.view.as_ref(),
                Some(&report.location)
            );
            let at = registry.entries[&agent.instance].view_report.unwrap().1;
            registry.expire_views(at + VIEW_REPORT_TIMEOUT - Duration::from_nanos(1));
            assert!(registry.entries[&agent.instance].agent.view.is_some());
            registry.expire_views(at + VIEW_REPORT_TIMEOUT);
            assert!(registry.entries[&agent.instance].agent.view.is_none());
            assert!(registry.entries[&agent.instance].agent.attachment.is_some());
        }
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        let closed_id = connection_id(&other_gui, ClientRole::Gui).await;
        HostHandler(fixture.host.clone()).connection_closed(ConnectionContext {
            id: closed_id,
            peer_pid: None,
        });
        assert!(fixture.host.registry().entries[&agent.instance]
            .agent
            .view
            .is_some());
        let closed_id = connection_id(&gui, ClientRole::Gui).await;
        HostHandler(fixture.host.clone()).connection_closed(ConnectionContext {
            id: closed_id,
            peer_pid: None,
        });
        let registry = fixture.host.registry();
        assert!(registry.entries[&agent.instance].agent.view.is_none());
        assert!(registry.entries[&agent.instance].agent.attachment.is_some());
    }

    #[tokio::test]
    async fn replacing_or_closing_relay_invalidates_old_location_reports() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let old = fixture.client(ClientRole::Relay).await;
        let agent = fixture.launch_agent(&old, "sleep 30").await;
        let key = attach(&old, &agent).await;
        let report = view_report(key);
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        let relay = fixture.client(ClientRole::Relay).await;
        let next = prepare(&relay, &agent, Some(key)).await;
        commit_ready(&relay, next).await;
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        assert!(fixture.host.registry().entries[&agent.instance]
            .agent
            .view
            .is_none());
        let current = AgentViewReport {
            key: next,
            ..report.clone()
        };
        gui.call(HostRequest::ReportViews {
            views: vec![current],
        })
        .await
        .unwrap();
        // A late EOF from the replaced relay must not clear the new owner.
        let closed_id = connection_id(&old, ClientRole::Relay).await;
        HostHandler(fixture.host.clone()).connection_closed(ConnectionContext {
            id: closed_id,
            peer_pid: None,
        });
        assert!(fixture.host.registry().entries[&agent.instance]
            .agent
            .view
            .is_some());
        let closed_id = connection_id(&relay, ClientRole::Relay).await;
        HostHandler(fixture.host.clone()).connection_closed(ConnectionContext {
            id: closed_id,
            peer_pid: None,
        });
        gui.call(HostRequest::ReportViews {
            views: vec![report],
        })
        .await
        .unwrap();
        let registry = fixture.host.registry();
        assert!(registry.entries[&agent.instance].agent.view.is_none());
        assert!(registry.entries[&agent.instance].agent.attachment.is_none());
    }

    async fn snapshot(client: &HostClient, key: AttachmentKey) -> String {
        let mut data = String::new();
        let mut after = 0;
        let mut offset = 0;
        loop {
            let HostResponse::Frame { frame } = client
                .call(HostRequest::Frame { key, after, offset })
                .await
                .unwrap()
            else {
                panic!("expected frame")
            };
            data.push_str(&frame.data);
            if let Some(next) = frame.next_offset {
                offset = next;
                after = frame.sequence;
            } else {
                return data;
            }
        }
    }

    async fn wait_text(client: &HostClient, key: AttachmentKey, expected: &str) -> String {
        timeout(Duration::from_secs(4), async {
            loop {
                let data = snapshot(client, key).await;
                if data.contains(expected) {
                    return data;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("agent output did not contain {expected:?}"))
    }

    #[tokio::test]
    async fn disconnect_preserves_same_pty_and_reconnect_does_not_replay_input() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let agent = fixture.launch_agent(&gui,"stty -echo; printf 'READY:%s 한글\\n' \"$$\"; while IFS= read -r line; do printf 'ACK:%s\\n' \"$line\"; done").await;
        let relay = fixture.client(ClientRole::Relay).await;
        let frame_reader = fixture.client(ClientRole::Observer).await;
        let key = attach(&relay, &agent).await;
        wait_text(&frame_reader, key, &format!("READY:{}", agent.pid.unwrap())).await;
        let input = HostRequest::Input {
            key,
            sequence: 1,
            data: b"once\n".to_vec(),
        };
        relay.call(input.clone()).await.unwrap();
        relay.call(input).await.unwrap();
        let data = wait_text(&frame_reader, key, "ACK:once").await;
        assert_eq!(data.matches("ACK:once").count(), 1);
        assert!(frame_reader
            .call(HostRequest::Input {
                key,
                sequence: 2,
                data: b"forbidden\n".to_vec()
            })
            .await
            .is_err());
        drop(relay);
        drop(gui);
        timeout(Duration::from_secs(2), async {
            loop {
                if !fixture
                    .host
                    .registry()
                    .connections
                    .values()
                    .any(|role| *role == ClientRole::Gui)
                {
                    break;
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            flowmux_procmon::process_start_time(agent.pid.unwrap()),
            agent.process_start
        );
        let replacement = fixture.client(ClientRole::Relay).await;
        assert!(replacement
            .call(HostRequest::Input {
                key,
                sequence: 2,
                data: b"stale\n".to_vec()
            })
            .await
            .is_err());
        let new_key = attach(&replacement, &agent).await;
        assert!(new_key.generation > key.generation);
        wait_text(&frame_reader, new_key, "ACK:once").await;
        replacement
            .call(HostRequest::Input {
                key: new_key,
                sequence: 1,
                data: b"again\n".to_vec(),
            })
            .await
            .unwrap();
        let data = wait_text(&frame_reader, new_key, "ACK:again").await;
        assert!(!data.contains("ACK:stale"));
        assert_eq!(
            flowmux_procmon::process_start_time(agent.pid.unwrap()),
            agent.process_start
        );
    }

    #[tokio::test]
    async fn explicit_gui_move_requires_the_destination_gui_to_admit_its_actual_relay() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let old = fixture.client(ClientRole::Relay).await;
        let new = fixture.client(ClientRole::Relay).await;
        let observer = fixture.client(ClientRole::Observer).await;
        let agent = fixture
            .launch_agent(
                &gui,
                "stty -echo; while IFS= read -r line; do printf 'ACK:%s\\n' \"$line\"; done",
            )
            .await;
        let original = attach(&old, &agent).await;
        let report = view_report(original);
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        let surface = flowmux_core::SurfaceId::new();
        let request = HostRequest::PrepareAttach {
            instance: agent.instance,
            epoch: agent.epoch,
            request_id: Uuid::new_v4(),
            view: None,
            surface: Some(surface),
            require_gui: true,
            gui_ready: true,
            replace: Some(original),
        };
        let HostResponse::Prepared { key, .. } = new.call(request).await.unwrap() else {
            panic!("expected prepared");
        };
        let HostResponse::Attached { frame, .. } = new
            .call(HostRequest::CommitAttach {
                key,
                size: TerminalSize { rows: 12, cols: 40 },
            })
            .await
            .unwrap()
        else {
            panic!("expected attached");
        };
        let pid = std::process::id();
        let birth = flowmux_procmon::process_start_time(pid).unwrap();
        let ready = HostRequest::GuiViewReady {
            key,
            surface,
            sequence: frame.sequence,
            relay_pid: pid,
            relay_start: birth,
        };
        assert!(new
            .call(HostRequest::ViewReady {
                key,
                sequence: frame.sequence
            })
            .await
            .is_err());
        assert!(new.call(ready.clone()).await.is_err());
        assert!(observer.call(ready.clone()).await.is_err());
        for bad in [
            HostRequest::GuiViewReady {
                key,
                surface,
                sequence: frame.sequence,
                relay_pid: agent.pid.unwrap(),
                relay_start: agent.process_start.unwrap(),
            },
            HostRequest::GuiViewReady {
                key,
                surface,
                sequence: frame.sequence,
                relay_pid: pid,
                relay_start: birth.wrapping_add(1),
            },
            HostRequest::GuiViewReady {
                key,
                surface,
                sequence: u64::MAX,
                relay_pid: pid,
                relay_start: birth,
            },
            HostRequest::GuiViewReady {
                surface,
                key: AttachmentKey {
                    lease: Uuid::new_v4(),
                    ..key
                },
                sequence: frame.sequence,
                relay_pid: pid,
                relay_start: birth,
            },
        ] {
            assert!(gui.call(bad).await.is_err());
        }
        let gui_id = connection_id(&gui, ClientRole::Gui).await;
        fixture
            .host
            .registry()
            .peers
            .get_mut(&gui_id)
            .unwrap()
            .identity = agent.pid.zip(agent.process_start);
        assert!(gui.call(ready.clone()).await.is_err());
        fixture
            .host
            .registry()
            .peers
            .get_mut(&gui_id)
            .unwrap()
            .identity = Some((pid, birth));
        assert_eq!(
            fixture.host.registry().entries[&agent.instance].agent.view,
            Some(report.location)
        );
        old.call(HostRequest::Input {
            key: original,
            sequence: 1,
            data: b"source preserved\n".to_vec(),
        })
        .await
        .unwrap();
        wait_text(&old, original, "ACK:source preserved").await;
        gui.call(ready.clone()).await.unwrap();
        assert!(old
            .call(HostRequest::Input {
                key: original,
                sequence: 2,
                data: b"stale\n".to_vec()
            })
            .await
            .is_err());
        new.call(HostRequest::Input {
            key,
            sequence: 1,
            data: b"destination admitted\n".to_vec(),
        })
        .await
        .unwrap();
        wait_text(&new, key, "ACK:destination admitted").await;
        // A later pending move must not prevent an idempotent admission of the current owner.
        let third = fixture.client(ClientRole::Relay).await;
        let pending = prepare(&third, &agent, Some(key)).await;
        gui.call(ready).await.unwrap();
        third
            .call(HostRequest::AbortAttach { key: pending })
            .await
            .unwrap();
        assert_eq!(
            flowmux_procmon::process_start_time(agent.pid.unwrap()),
            agent.process_start
        );
    }

    #[tokio::test]
    async fn committed_surface_fences_stale_restores_even_after_the_winner_detaches() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let relay = fixture.client(ClientRole::Relay).await;
        let agent = fixture.launch_agent(&gui, "cat").await;
        let key = attach(&relay, &agent).await;
        let report = view_report(key);
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        relay.call(HostRequest::Detach { key }).await.unwrap();
        let original = AgentViewOwner {
            key,
            surface: report.location.surface,
        };
        assert_eq!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .view_owner,
            Some(original)
        );
        let prepare = |surface, explicit| HostRequest::PrepareAttach {
            instance: agent.instance,
            epoch: agent.epoch,
            request_id: Uuid::new_v4(),
            view: None,
            surface: Some(surface),
            require_gui: true,
            gui_ready: explicit,
            replace: None,
        };
        let destination = flowmux_core::SurfaceId::new();
        assert!(relay
            .call(prepare(destination, false))
            .await
            .unwrap_err()
            .to_string()
            .contains("superseded"));
        // An ordinary restore of the winning saved surface remains valid.
        let HostResponse::Prepared { key, .. } =
            relay.call(prepare(original.surface, false)).await.unwrap()
        else {
            panic!("expected prepared");
        };
        commit_ready(&relay, key).await;
        let restored = AgentViewOwner {
            key,
            surface: original.surface,
        };
        assert_eq!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .view_owner,
            Some(restored)
        );
        relay.call(HostRequest::Detach { key }).await.unwrap();
        // Opening a different surface from Background requires explicit GUI admission.
        let HostResponse::Prepared { key, .. } =
            relay.call(prepare(destination, true)).await.unwrap()
        else {
            panic!("expected prepared");
        };
        let HostResponse::Attached { frame, .. } = relay
            .call(HostRequest::CommitAttach {
                key,
                size: TerminalSize::default(),
            })
            .await
            .unwrap()
        else {
            panic!("expected attached");
        };
        let ready = |surface| HostRequest::GuiViewReady {
            key,
            surface,
            sequence: frame.sequence,
            relay_pid: std::process::id(),
            relay_start: flowmux_procmon::process_start_time(std::process::id()).unwrap(),
        };
        assert!(gui.call(ready(original.surface)).await.is_err());
        assert_eq!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .view_owner,
            Some(restored)
        );
        assert!(relay
            .call(HostRequest::ViewReady {
                key,
                sequence: frame.sequence
            })
            .await
            .is_err());
        gui.call(ready(destination)).await.unwrap();
        let winner = AgentViewOwner {
            key,
            surface: destination,
        };
        // This precedes any location report or GUI save.
        assert_eq!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .view_owner,
            Some(winner)
        );
        relay.call(HostRequest::Detach { key }).await.unwrap();
        assert!(relay
            .call(prepare(original.surface, false))
            .await
            .unwrap_err()
            .to_string()
            .contains("superseded"));
        assert!(fixture.host.registry().entries[&agent.instance]
            .agent
            .attachment
            .is_none());
        let HostResponse::Prepared { key, .. } =
            relay.call(prepare(original.surface, true)).await.unwrap()
        else {
            panic!("expected prepared");
        };
        relay.call(HostRequest::AbortAttach { key }).await.unwrap();
        assert_eq!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .view_owner,
            Some(winner)
        );
        assert_eq!(
            flowmux_procmon::process_start_time(agent.pid.unwrap()),
            agent.process_start
        );
    }

    #[tokio::test]
    async fn gui_attachment_requires_management_at_prepare_commit_and_ready() {
        let fixture = Fixture::new().await;
        let old = fixture.client(ClientRole::Relay).await;
        let agent = fixture
            .launch_agent(
                &old,
                "stty -echo; while IFS= read -r line; do printf 'ACK:%s\\n' \"$line\"; done",
            )
            .await;
        let new = fixture.client(ClientRole::Relay).await;
        let mut request = HostRequest::PrepareAttach {
            instance: agent.instance,
            epoch: agent.epoch,
            request_id: Uuid::new_v4(),
            view: None,
            surface: Some(flowmux_core::SurfaceId::new()),
            require_gui: true,
            gui_ready: false,
            replace: None,
        };
        assert!(new.call(request.clone()).await.is_err());
        // A rejected preparation must not reserve a lease or change ownership.
        let old_key = attach(&old, &agent).await;
        let gui = fixture.client(ClientRole::Gui).await;
        let HostRequest::PrepareAttach { replace, .. } = &mut request else {
            unreachable!()
        };
        *replace = Some(old_key);
        let HostResponse::Prepared { key, .. } = new.call(request).await.unwrap() else {
            panic!("expected prepared");
        };
        drop(gui);
        timeout(Duration::from_secs(2), async {
            while fixture
                .host
                .registry()
                .connections
                .values()
                .any(|role| *role == ClientRole::Gui)
            {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let commit = HostRequest::CommitAttach {
            key,
            size: TerminalSize { rows: 8, cols: 80 },
        };
        assert!(new.call(commit.clone()).await.is_err());
        old.call(HostRequest::Input {
            key: old_key,
            sequence: 1,
            data: b"old owner preserved\n".to_vec(),
        })
        .await
        .unwrap();
        wait_text(&old, old_key, "ACK:old owner preserved").await;
        let gui = fixture.client(ClientRole::Gui).await;
        let HostResponse::Attached { frame, .. } = new.call(commit).await.unwrap() else {
            panic!("expected attached");
        };
        drop(gui);
        timeout(Duration::from_secs(2), async {
            while fixture
                .host
                .registry()
                .connections
                .values()
                .any(|role| *role == ClientRole::Gui)
            {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert!(new
            .call(HostRequest::ViewReady {
                key,
                sequence: frame.sequence
            })
            .await
            .is_err());
        old.call(HostRequest::Input {
            key: old_key,
            sequence: 2,
            data: b"owner survives lost management\n".to_vec(),
        })
        .await
        .unwrap();
        wait_text(&old, old_key, "ACK:owner survives lost management").await;
        let _gui = fixture.client(ClientRole::Gui).await;
        commit_ready(&new, key).await;
        new.call(HostRequest::Input {
            key,
            sequence: 1,
            data: b"same execution\n".to_vec(),
        })
        .await
        .unwrap();
        wait_text(&new, key, "ACK:same execution").await;
        assert_eq!(
            flowmux_procmon::process_start_time(agent.pid.unwrap()),
            agent.process_start
        );
    }

    #[tokio::test]
    async fn attachment_move_commits_only_after_prepare_and_abort_preserves_owner() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let agent = fixture
            .launch_agent(
                &gui,
                "stty -echo; while IFS= read -r line; do printf '%s\\n' \"$line\"; done",
            )
            .await;
        let old = fixture.client(ClientRole::Relay).await;
        let new = fixture.client(ClientRole::Relay).await;
        let key = attach(&old, &agent).await;
        let report = view_report(key);
        gui.call(HostRequest::ReportViews {
            views: vec![report.clone()],
        })
        .await
        .unwrap();
        assert!(new
            .call(HostRequest::PrepareAttach {
                instance: agent.instance,
                epoch: agent.epoch,
                request_id: Uuid::new_v4(),
                view: None,
                surface: None,
                require_gui: false,
                gui_ready: false,
                replace: None
            })
            .await
            .is_err());
        let request = HostRequest::PrepareAttach {
            instance: agent.instance,
            epoch: agent.epoch,
            request_id: Uuid::new_v4(),
            view: None,
            surface: None,
            require_gui: false,
            gui_ready: false,
            replace: Some(key),
        };
        let HostResponse::Prepared { key: pending, .. } = new.call(request.clone()).await.unwrap()
        else {
            panic!("expected prepared")
        };
        assert!(
            matches!(new.call(request.clone()).await.unwrap(), HostResponse::Prepared { key: duplicate, .. } if duplicate == pending)
        );
        let mut changed = request;
        let HostRequest::PrepareAttach { replace, .. } = &mut changed else {
            unreachable!()
        };
        *replace = None;
        assert!(new.call(changed).await.is_err());
        new.call(HostRequest::CommitAttach {
            key: pending,
            size: TerminalSize { rows: 12, cols: 40 },
        })
        .await
        .unwrap();
        // A destination that has not written its snapshot must not revoke the source.
        assert_eq!(
            fixture.host.registry().entries[&agent.instance].agent.view,
            Some(report.location.clone())
        );
        assert_eq!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .attachment
                .as_ref()
                .unwrap()
                .key,
            key
        );
        new.call(HostRequest::Resize {
            key: pending,
            size: TerminalSize { rows: 16, cols: 60 },
        })
        .await
        .unwrap();
        let HostResponse::Frame { frame } = old
            .call(HostRequest::Frame {
                key,
                after: 0,
                offset: 0,
            })
            .await
            .unwrap()
        else {
            panic!("expected frame")
        };
        assert_eq!(frame.size, TerminalSize { rows: 8, cols: 80 });
        old.call(HostRequest::Input {
            key,
            sequence: 1,
            data: b"old still owns input\n".to_vec(),
        })
        .await
        .unwrap();
        assert!(new
            .call(HostRequest::Input {
                key: pending,
                sequence: 1,
                data: b"too early\n".to_vec()
            })
            .await
            .is_err());
        new.call(HostRequest::AbortAttach { key: pending })
            .await
            .unwrap();
        old.call(HostRequest::Input {
            key,
            sequence: 2,
            data: b"abort preserved input\n".to_vec(),
        })
        .await
        .unwrap();
        let pending = prepare(&new, &agent, Some(key)).await;
        let commit = HostRequest::CommitAttach {
            key: pending,
            size: TerminalSize { rows: 16, cols: 60 },
        };
        new.call(commit.clone()).await.unwrap();
        let HostResponse::Attached { frame, .. } = new.call(commit).await.unwrap() else {
            panic!("expected attached")
        };
        assert!(new
            .call(HostRequest::ViewReady {
                key: pending,
                sequence: u64::MAX
            })
            .await
            .is_err());
        old.call(HostRequest::Input {
            key,
            sequence: 3,
            data: b"still owns input before ACK\n".to_vec(),
        })
        .await
        .unwrap();
        new.call(HostRequest::ViewReady {
            key: pending,
            sequence: frame.sequence,
        })
        .await
        .unwrap();
        assert!(old
            .call(HostRequest::Input {
                key,
                sequence: 4,
                data: b"old owner revoked\n".to_vec()
            })
            .await
            .is_err());
        new.call(HostRequest::Input {
            key: pending,
            sequence: 1,
            data: b"new owner\n".to_vec(),
        })
        .await
        .unwrap();
        let data = wait_text(&new, pending, "new owner").await;
        assert!(!data.contains("too early") && !data.contains("old owner revoked"));
        let HostResponse::Frame { frame } = new
            .call(HostRequest::Frame {
                key: pending,
                after: 0,
                offset: 0,
            })
            .await
            .unwrap()
        else {
            panic!("expected frame")
        };
        assert_eq!(frame.size, TerminalSize { rows: 16, cols: 60 });
        for request in [
            HostRequest::Detach { key },
            HostRequest::Resize {
                key,
                size: TerminalSize::default(),
            },
        ] {
            assert!(old.call(request).await.is_err());
        }
        assert_eq!(
            flowmux_procmon::process_start_time(agent.pid.unwrap()),
            agent.process_start
        );
    }

    #[tokio::test]
    async fn replacement_ready_rejects_detached_source_and_stopping_execution() {
        let fixture = Fixture::new().await;
        let old = fixture.client(ClientRole::Relay).await;
        let new = fixture.client(ClientRole::Relay).await;
        let agent = fixture
            .launch_agent(
                &old,
                "trap '' TERM; printf TRANSFER_READY; while :; do sleep 1; done",
            )
            .await;
        let old_key = attach(&old, &agent).await;
        wait_text(&old, old_key, "TRANSFER_READY").await;
        let key = prepare(&new, &agent, Some(old_key)).await;
        let HostResponse::Attached { frame, .. } = new
            .call(HostRequest::CommitAttach {
                key,
                size: TerminalSize::default(),
            })
            .await
            .unwrap()
        else {
            panic!("expected attached")
        };
        old.call(HostRequest::Detach { key: old_key })
            .await
            .unwrap();
        assert!(new
            .call(HostRequest::ViewReady {
                key,
                sequence: frame.sequence
            })
            .await
            .is_err());
        assert!(new
            .call(HostRequest::CommitAttach {
                key,
                size: TerminalSize::default()
            })
            .await
            .is_err());
        assert!(fixture.host.registry().entries[&agent.instance]
            .agent
            .attachment
            .is_none());
        new.call(HostRequest::AbortAttach { key }).await.unwrap();
        let old_key = attach(&old, &agent).await;
        let key = prepare(&new, &agent, Some(old_key)).await;
        let HostResponse::Attached { frame, .. } = new
            .call(HostRequest::CommitAttach {
                key,
                size: TerminalSize::default(),
            })
            .await
            .unwrap()
        else {
            panic!("expected attached")
        };
        old.call(HostRequest::Terminate {
            instance: agent.instance,
            epoch: agent.epoch,
            mode: TerminateMode::Terminate,
        })
        .await
        .unwrap();
        assert!(new
            .call(HostRequest::ViewReady {
                key,
                sequence: frame.sequence
            })
            .await
            .is_err());
        new.call(HostRequest::AbortAttach { key }).await.unwrap();
        assert_eq!(
            fixture.host.registry().entries[&agent.instance]
                .agent
                .attachment
                .as_ref()
                .unwrap()
                .key,
            old_key
        );
    }

    #[tokio::test]
    async fn synchronized_output_expires_without_more_bytes_and_flushes_before_final_frame() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let relay = fixture.client(ClientRole::Relay).await;
        let reader = fixture.client(ClientRole::Observer).await;
        let agent = fixture
            .launch_agent(&gui, r"printf '\033[?2026hTIMEOUT_VISIBLE'; sleep 30")
            .await;
        let key = attach(&relay, &agent).await;
        wait_text(&reader, key, "TIMEOUT_VISIBLE").await;
        assert_eq!(
            fixture.host.registry().entries[&agent.instance].agent.state,
            ProcessState::Running
        );

        let agent = fixture
            .launch_agent(
                &gui,
                r"printf READY; while [ ! -e final-gate ]; do sleep 0.01; done; printf '\033[?2026hFINAL_VISIBLE'",
            )
            .await;
        let key = attach(&relay, &agent).await;
        wait_text(&reader, key, "READY").await;
        std::fs::write(fixture._directory.path().join("final-gate"), b"go").unwrap();
        timeout(Duration::from_secs(4), async {
            loop {
                if fixture.host.registry().entries[&agent.instance].agent.state
                    == ProcessState::Exited
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(snapshot(&reader, key).await.contains("FINAL_VISIBLE"));
        assert!(!fixture.host.registry().entries[&agent.instance].is_reaped());
        drop(relay);
        timeout(Duration::from_secs(3), async {
            while !fixture.host.registry().entries[&agent.instance].is_reaped() {
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("relay EOF must release the completed frame");
    }

    #[tokio::test]
    async fn query_replies_survive_a_full_input_queue_without_replaying_queries_to_views() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let agent = fixture
            .launch_agent(
                &gui,
                r#"
            stty raw -echo
            printf READY
            while [ ! -e query-gate ]; do sleep 0.01; done
            (i=0; while [ "$i" -lt 65536 ]; do printf '\033[6n'; i=$((i+1)); done) &
            sleep 1
            head -c 655360 >/dev/null
            wait
            printf QUERY_DRAINED
            sleep 30
        "#,
            )
            .await;
        let relay = fixture.client(ClientRole::Relay).await;
        let reader = fixture.client(ClientRole::Observer).await;
        let key = attach(&relay, &agent).await;
        wait_text(&reader, key, "READY").await;
        for sequence in 1..=16 {
            relay
                .call(HostRequest::Input {
                    key,
                    sequence,
                    data: vec![b'x'; MAX_INPUT_BYTES],
                })
                .await
                .unwrap();
        }
        // Start queries only after all input was accepted, even on a busy runner.
        std::fs::write(fixture._directory.path().join("query-gate"), b"go").unwrap();
        timeout(Duration::from_secs(12), async {
            loop {
                let screen = snapshot(&reader, key).await;
                assert!(!screen.contains("\x1b[6n"));
                if screen.contains("QUERY_DRAINED") {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("all accepted input and all query replies must reach the provider");
    }

    #[tokio::test]
    async fn launch_identity_is_idempotent_and_invalid_requests_have_no_process_effects() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let mut launch = fixture.launch("while IFS= read -r line; do :; done");
        let HostResponse::Launched { agent: first } = gui
            .call(HostRequest::Launch {
                launch: launch.clone(),
            })
            .await
            .unwrap()
        else {
            panic!()
        };
        let HostResponse::Launched { agent: second } = gui
            .call(HostRequest::Launch {
                launch: launch.clone(),
            })
            .await
            .unwrap()
        else {
            panic!()
        };
        assert_eq!(first.instance, second.instance);
        assert_eq!(first.pid, second.pid);
        launch.env.push(("changed".into(), "value".into()));
        assert!(gui.call(HostRequest::Launch { launch }).await.is_err());
        let mut invalid = fixture.launch("printf should-not-start");
        invalid.cwd = fixture._directory.path().join("does-not-exist");
        assert!(gui
            .call(HostRequest::Launch { launch: invalid })
            .await
            .is_err());
        assert_eq!(fixture.host.registry().entries.len(), 1);
    }

    #[tokio::test]
    async fn completed_launches_release_capacity_without_reexecuting_evicted_requests() {
        let fixture = Fixture::new().await;
        let anchor = fixture.launch("sleep 300");
        let first = fixture.host.launch(anchor.clone()).await.unwrap();
        let mut oldest = None;
        let mut latest = None;
        for _ in 0..MAX_ENDED_INSTANCES + MAX_INSTANCES + 2 {
            let launch = fixture.launch("exit 0");
            let HostResponse::Launched { agent } =
                fixture.host.launch(launch.clone()).await.unwrap()
            else {
                panic!()
            };
            timeout(Duration::from_secs(10), async {
                while fixture.host.registry().entries[&agent.instance].agent.state
                    != ProcessState::Exited
                {
                    sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            oldest.get_or_insert(launch.clone());
            latest = Some((launch, agent.instance));
            assert!(fixture.host.registry().entries.len() <= MAX_ENDED_INSTANCES + MAX_INSTANCES);
        }
        let count = fixture.host.registry().entries.len();
        assert_eq!(fixture.host.registry().launches.len(), count);
        assert!(fixture.host.registry().expired_launch_ms.is_some());
        assert!(
            matches!(fixture.host.launch(oldest.unwrap()).await, Err(RpcError::InvalidArgument(message)) if message.contains("expired"))
        );
        let count = fixture.host.registry().entries.len();
        let (launch, instance) = latest.unwrap();
        let HostResponse::Launched { agent } = fixture.host.launch(launch).await.unwrap() else {
            panic!()
        };
        assert_eq!(agent.instance, instance);
        assert_eq!(agent.state, ProcessState::Exited);
        assert_eq!(fixture.host.registry().entries.len(), count);
        let HostResponse::Launched { agent: before } = first else {
            panic!()
        };
        let HostResponse::Launched { agent: after } = fixture.host.launch(anchor).await.unwrap()
        else {
            panic!()
        };
        assert_eq!(before.instance, after.instance);
        assert_eq!(before.pid, after.pid);
        assert_eq!(before.process_start, after.process_start);
        assert_eq!(after.state, ProcessState::Running);
    }

    #[tokio::test]
    async fn concurrent_launch_capacity_and_request_identity_remain_enforced() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let agent = fixture.launch_agent(&gui, "sleep 120").await;
        {
            let mut registry = fixture.host.registry();
            for _ in 1..MAX_INSTANCES {
                let mut starting = agent.clone();
                starting.instance = AgentInstanceId::new();
                starting.state = ProcessState::Starting;
                starting.pid = None;
                starting.process_start = None;
                registry
                    .launches
                    .insert(Uuid::now_v7(), ([0; 32], starting.instance));
                registry.entries.insert(
                    starting.instance,
                    Entry {
                        attachment_gate: Arc::new(tokio::sync::Mutex::new(())),
                        agent: starting,
                        commands: None,
                        final_frame: None,
                        attachment_connection: None,
                        view_report: None,
                    },
                );
            }
        }
        assert!(matches!(
            fixture.host.launch(fixture.launch("exit 0")).await,
            Err(RpcError::Busy(_))
        ));
        let mut invalid = fixture.launch("exit 0");
        invalid.request_id = Uuid::new_v4();
        assert!(matches!(
            fixture.host.launch(invalid).await,
            Err(RpcError::InvalidArgument(_))
        ));
        let mut future = fixture.launch("exit 0");
        future.request_id = Uuid::new_v7(uuid::Timestamp::from_unix(
            uuid::NoContext,
            chrono::Utc::now().timestamp() as u64 + 120,
            0,
        ));
        assert!(matches!(
            fixture.host.launch(future).await,
            Err(RpcError::InvalidArgument(_))
        ));
        assert_eq!(fixture.host.registry().entries.len(), MAX_INSTANCES);
    }

    #[tokio::test]
    async fn natural_root_exit_waits_for_owned_background_child_and_reaps_without_idle_timeout() {
        let fixture = Fixture::new().await;
        let gui = fixture.client(ClientRole::Gui).await;
        let agent = fixture
            .launch_agent(
                &gui,
                "trap '' HUP; (sleep 0.8; printf child-finished) & exit 0",
            )
            .await;
        sleep(Duration::from_millis(350)).await;
        assert!(
            fixture.host.registry().entries[&agent.instance]
                .commands
                .is_some(),
            "root exit must preserve the child"
        );
        timeout(Duration::from_secs(4), async {
            loop {
                if fixture.host.registry().entries[&agent.instance].agent.state
                    == ProcessState::Exited
                {
                    break;
                }
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(fixture.host.registry().entries[&agent.instance]
            .commands
            .is_none());
    }

    #[test]
    fn frame_chunks_are_bounded_utf8_and_assemble_exactly() {
        let data: Arc<str> = Arc::from("한글😀".repeat(FRAME_CHUNK));
        let transfer = Transfer {
            sequence: 9,
            snapshot: true,
            size: TerminalSize::default(),
            data: data.clone(),
        };
        let mut assembled = String::new();
        let mut offset = 0;
        loop {
            let frame = transfer.chunk(offset).unwrap();
            assert!(frame.data.len() <= FRAME_CHUNK);
            assembled.push_str(&frame.data);
            let encoded = serde_json::to_vec(&Response::AgentHost {
                response: HostResponse::Frame {
                    frame: frame.clone(),
                },
            })
            .unwrap();
            assert!(encoded.len() < 1024 * 1024);
            if let Some(next) = frame.next_offset {
                offset = next;
            } else {
                break;
            }
        }
        assert_eq!(assembled.as_str(), data.as_ref());
        assert!(transfer.chunk(1).is_err());
    }
}
