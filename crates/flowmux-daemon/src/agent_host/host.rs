// SPDX-License-Identifier: GPL-3.0-or-later
//! Process/PTY ownership is independent of both GUI and terminal connections.

use super::retirement::RetirementPolicy;
use super::terminal::Terminal;
use flowmux_core::AgentInstanceId;
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
const MAX_INPUT_PENDING: usize = 256 * 1024;
const FRAME_CHUNK: usize = 64 * 1024;
const ATTACH_GRACE: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(5);

type Result<T> = std::result::Result<T, RpcError>;

struct Entry {
    agent: AgentInstance,
    commands: Option<mpsc::Sender<Command>>,
    final_frame: Option<(AttachmentKey, Transfer, Option<Transfer>)>,
}

struct Registry {
    connections: HashMap<u64, ClientRole>,
    entries: HashMap<AgentInstanceId, Entry>,
    launches: HashMap<Uuid, ([u8; 32], AgentInstanceId)>,
    revision: u64,
    policy: RetirementPolicy,
    stopping: bool,
    accepted_launches: usize,
    launched: bool,
    empty_since: Option<Instant>,
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
                entries: HashMap::new(),
                launches: HashMap::new(),
                revision: 0,
                policy: RetirementPolicy::new(idle_timeout, Instant::now()),
                stopping: false,
                accepted_launches: 0,
                launched: false,
                empty_since: None,
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
                // Initial admission is bounded; retirement never restarts an empty Host.
                if (registry.launched
                    && (!transferring_final_frame || now.duration_since(empty_since) >= STOP_GRACE)
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

    fn require_connection(&self, connection: u64, mutate: bool) -> Result<()> {
        match self.registry().connections.get(&connection) {
            Some(ClientRole::Gui | ClientRole::Relay) => Ok(()),
            Some(ClientRole::Observer) if !mutate => Ok(()),
            _ => Err(RpcError::InvalidArgument(
                "Host handshake required for this connection".into(),
            )),
        }
    }

    async fn request(
        self: &Arc<Self>,
        request: HostRequest,
        connection: u64,
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
        self.require_connection(connection, !request.is_query())?;
        match request {
            HostRequest::Hello { .. } => unreachable!(),
            HostRequest::KeepAlive => Ok(HostResponse::Ok),
            HostRequest::List => {
                let registry = self.registry();
                Ok(HostResponse::List {
                    epoch: self.epoch,
                    revision: registry.revision,
                    agents: registry
                        .entries
                        .values()
                        .map(|entry| entry.agent.clone())
                        .collect(),
                })
            }
            HostRequest::Launch { launch } => self.launch(launch).await,
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
                    | HostRequest::Terminate { instance, .. } => *instance,
                    HostRequest::CommitAttach { key, .. }
                    | HostRequest::AbortAttach { key }
                    | HostRequest::Detach { key }
                    | HostRequest::Frame { key, .. }
                    | HostRequest::Input { key, .. }
                    | HostRequest::Resize { key, .. } => key.instance,
                    _ => unreachable!(),
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
                    agent: registry.entries[instance].agent.clone(),
                });
            }
            // Bound tombstones as well: they retain launch idempotence for this epoch.
            if registry.entries.len() >= MAX_INSTANCES {
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
                        exit_status: None,
                        view: None,
                        error: None,
                    },
                    commands: None,
                    final_frame: None,
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
                "FLOWMUX_SOCKET_PATH" | "FLOWMUX_AGENT_INSTANCE_ID" | "FLOWMUX_AGENT_HOST_EPOCH"
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
                Ok(HostResponse::Launched { agent })
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
                    agent: entry.agent.clone(),
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
                Request::AgentHost { request } => match self.0.request(request, context.id).await {
                    Ok(response) => Response::AgentHost { response },
                    Err(error) => Response::Error(error),
                },
                Request::Ping => Response::Pong,
                _ => Response::Error(RpcError::Unimplemented(
                    "request is not supported by the Agent Host".into(),
                )),
            }
        })
    }
    fn connection_closed(&self, context: ConnectionContext) {
        let mut registry = self.0.registry();
        if registry.connections.remove(&context.id) == Some(ClientRole::Gui) {
            registry.policy.gui_disconnected(context.id, Instant::now());
        }
    }
}

fn validate_launch(launch: &LaunchAgent) -> Result<()> {
    launch.size.validate()?;
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
    require_gui: bool,
    prepared: Instant,
    transfer: Option<Transfer>,
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
        }
    }

    async fn run(mut self) {
        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let mut buffer = vec![0u8; 16 * 1024];
        loop {
            let count = self.replies.len().min(MAX_INPUT_PENDING - self.input.len());
            self.input.extend(self.replies.drain(..count));
            tokio::select! {
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
                    self.reap_descendants();
                    if let Some((signal,deadline)) = self.stopping {
                        self.signal_owned(if Instant::now() >= deadline {libc::SIGKILL} else {signal});
                    }
                    if self.root_status.is_some() && self.owned.is_empty() && self.eof {
                        self.finished();
                        break;
                    }
                }
            }
        }
    }

    fn command(&mut self, connection: u64, request: HostRequest) -> Result<HostResponse> {
        match request {
            HostRequest::PrepareAttach {
                instance,
                epoch,
                request_id,
                view,
                require_gui,
                replace,
            } => {
                if instance != self.instance || epoch != self.host.epoch {
                    return Err(RpcError::NotFound(
                        "Agent Host epoch or instance changed".into(),
                    ));
                }
                if require_gui {
                    self.require_gui_connection()?;
                }
                if let Some(pending) = self.pending.as_ref() {
                    if pending.request == request_id
                        && pending.connection == connection
                        && pending.view == view
                        && pending.require_gui == require_gui
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
                    require_gui,
                    prepared: Instant::now(),
                    transfer: None,
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
                if !self
                    .pending
                    .as_ref()
                    .is_some_and(|lease| lease.key == key && lease.connection == connection)
                {
                    return Err(stale_lease());
                }
                if self.pending.as_ref().is_some_and(|lease| lease.require_gui) {
                    self.require_gui_connection()?;
                }
                self.resize(size)?;
                self.active = self.pending.take();
                self.update_view();
                let frame = self.frame(key, 0, 0)?;
                Ok(HostResponse::Attached { key, frame })
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
                self.input_lease(key, connection)?;
                self.resize(size.validate()?)?;
                Ok(HostResponse::Ok)
            }
            HostRequest::Terminate { mode, .. } => {
                if mode == TerminateMode::Interrupt {
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
        registry.entries.get_mut(&self.instance).unwrap().agent.view =
            self.active.as_ref().and_then(|lease| lease.view.clone());
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
        let entry = registry.entries.get_mut(&self.instance).unwrap();
        entry.agent.state = ProcessState::Exited;
        entry.agent.exit_status = self.root_status;
        entry.agent.view = None;
        entry.commands = None;
        entry.final_frame = final_frame;
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
                request_id: Uuid::new_v4(),
                provider: "fixture".into(),
                label: "isolated test agent".into(),
                argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
                cwd: self._directory.path().into(),
                env: vec![("TERM".into(), "xterm-256color".into())],
                size: TerminalSize { rows: 8, cols: 80 },
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
            agent
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
                require_gui: false,
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
        client
            .call(HostRequest::CommitAttach {
                key,
                size: TerminalSize { rows: 8, cols: 80 },
            })
            .await
            .unwrap();
        key
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
    async fn gui_attachment_requires_management_at_prepare_and_commit() {
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
            require_gui: true,
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
        let _gui = fixture.client(ClientRole::Gui).await;
        new.call(commit).await.unwrap();
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
        assert!(new
            .call(HostRequest::PrepareAttach {
                instance: agent.instance,
                epoch: agent.epoch,
                request_id: Uuid::new_v4(),
                view: None,
                require_gui: false,
                replace: None
            })
            .await
            .is_err());
        let pending = prepare(&new, &agent, Some(key)).await;
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
        new.call(HostRequest::CommitAttach {
            key: pending,
            size: TerminalSize::default(),
        })
        .await
        .unwrap();
        assert!(old
            .call(HostRequest::Input {
                key,
                sequence: 3,
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
