// SPDX-License-Identifier: GPL-3.0-or-later
//! A view of an existing Host PTY. Closing this process never signals the agent.

use anyhow::{bail, Context};
use flowmux_core::AgentInstanceId;
use flowmux_ipc::agent_host::*;
use flowmux_ipc::RpcError;
use flowmux_terminal::TerminalInputModes;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(clap::Args)]
pub(crate) struct AgentRelayArgs {
    #[arg(long, required_unless_present = "shell", requires = "epoch")]
    pub instance: Option<AgentInstanceId>,
    #[arg(long, required_unless_present = "shell", requires = "instance")]
    pub epoch: Option<Uuid>,
    /// Run this terminal's shell in the Agent Host, reusing `--instance` while it lives.
    #[arg(long)]
    pub shell: bool,
    /// The shell command of `--shell`, preserved without shell expansion.
    #[arg(last = true, requires = "shell")]
    pub argv: Vec<String>,
    #[arg(long)]
    pub require_gui: bool,
    #[arg(long, requires = "replace_generation")]
    pub replace_lease: Option<Uuid>,
    #[arg(long, requires = "replace_lease", value_parser = clap::value_parser!(u64).range(1..))]
    pub replace_generation: Option<u64>,
    #[arg(long, requires = "require_gui")]
    pub ready_token: Option<Uuid>,
    /// JSON default colors of the displaying view, for provider color query replies.
    #[arg(long)]
    pub colors: Option<String>,
}

struct TerminalGuard {
    termios: libc::termios,
    flags: [(i32, i32); 2],
}

impl TerminalGuard {
    fn raw() -> io::Result<Self> {
        let mut termios = std::mem::MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(0, termios.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let termios = unsafe { termios.assume_init() };
        let mut raw = termios;
        unsafe { libc::cfmakeraw(&mut raw) };
        let flags = [
            (0, unsafe { libc::fcntl(0, libc::F_GETFL) }),
            (1, unsafe { libc::fcntl(1, libc::F_GETFL) }),
        ];
        if flags.iter().any(|(_, flags)| *flags < 0) {
            return Err(io::Error::last_os_error());
        }
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let guard = Self { termios, flags };
        for (fd, flags) in guard.flags {
            if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        for (fd, flags) in self.flags {
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
        }
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.termios) };
    }
}

fn io_fd(source: i32) -> io::Result<AsyncFd<OwnedFd>> {
    let fd = unsafe { libc::fcntl(source, libc::F_DUPFD_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: raw() enabled nonblocking I/O, and AsyncFd exclusively owns
    // this duplicate until it is deregistered. The guard outlives it.
    Ok(unsafe { AsyncFd::register(fd) }?)
}

fn size() -> anyhow::Result<TerminalSize> {
    let mut size = std::mem::MaybeUninit::<libc::winsize>::zeroed();
    if unsafe { libc::ioctl(0, libc::TIOCGWINSZ, size.as_mut_ptr()) } < 0 {
        return Err(io::Error::last_os_error().into());
    }
    let size = unsafe { size.assume_init() };
    TerminalSize {
        rows: size.ws_row,
        cols: size.ws_col,
    }
    .validate()
    .map_err(|error| HostRpcError(error).into())
}

async fn frames(
    client: HostClient,
    key: AttachmentKey,
    first: TerminalFrame,
    sender: mpsc::Sender<anyhow::Result<TerminalFrame>>,
) {
    let result = async {
        let mut frame = first;
        loop {
            let sequence = frame.sequence;
            let next = frame.next_offset;
            let ended = frame.ended;
            if sender.send(Ok(frame)).await.is_err() {
                return Ok(());
            }
            if next.is_none() && ended {
                return Ok(());
            }
            if next.is_none() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            let response = tokio::time::timeout(
                Duration::from_secs(15),
                client.call(HostRequest::Frame {
                    key,
                    after: sequence,
                    offset: next.unwrap_or(0),
                }),
            )
            .await??;
            let HostResponse::Frame { frame: next_frame } = response else {
                bail!("unexpected Agent Host frame response");
            };
            if let Some(offset) = next {
                anyhow::ensure!(
                    next_frame.sequence == sequence && next_frame.offset == offset,
                    "Agent terminal frame transfer changed"
                );
            }
            frame = next_frame;
        }
    }
    .await;
    if let Err(error) = result {
        let _ = sender.send(Err(error)).await;
    }
}

async fn send_input(
    client: Arc<HostClient>,
    key: AttachmentKey,
    sequence: u64,
    data: Vec<u8>,
) -> anyhow::Result<()> {
    loop {
        let response = tokio::time::timeout(
            Duration::from_secs(15),
            client.call(HostRequest::Input {
                key,
                sequence,
                data: data.clone(),
            }),
        )
        .await?;
        match response {
            Ok(HostResponse::InputAccepted { sequence: accepted }) if accepted == sequence => {
                return Ok(())
            }
            Err(error)
                if matches!(
                    error.downcast_ref::<HostRpcError>(),
                    Some(HostRpcError(RpcError::Busy(_)))
                ) =>
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
            _ => bail!("unexpected agent input acknowledgement"),
        }
    }
}

async fn acknowledge_snapshot(
    client: &HostClient,
    key: AttachmentKey,
    sequence: u64,
    ready_token: Option<Uuid>,
) -> anyhow::Result<()> {
    let result = async {
        if let Some(token) = ready_token {
            let socket = std::env::var_os("FLOWMUX_SOCKET_PATH")
                .context("Agent destination GUI socket is missing")?;
            let surface = std::env::var("FLOWMUX_SURFACE_ID")
                .context("Agent destination surface is missing")?
                .parse()?;
            let response = tokio::time::timeout(Duration::from_secs(3), async {
                flowmux_ipc::client::Client::connect(Path::new(&socket))
                    .await?
                    .call(flowmux_ipc::Request::AgentViewReady {
                        surface,
                        token,
                        key,
                        sequence,
                    })
                    .await
            })
            .await
            .context("Agent destination admission timed out; not retried")??;
            anyhow::ensure!(
                matches!(response, flowmux_ipc::Response::Ok),
                "Agent destination admission failed: {response:?}"
            );
            return Ok(());
        }
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            client.call(HostRequest::ViewReady { key, sequence }),
        )
        .await
        .context("Agent view ready acknowledgement timed out; not retried")??;
        anyhow::ensure!(
            matches!(response, HostResponse::Ok),
            "unexpected Agent view ready response"
        );
        Ok(())
    }
    .await;
    if let Err(error) = result {
        // A lost ACK must not disconnect a relay which already owns input.
        tracing::warn!(%error, "Agent admission response lost; checking the existing lease");
        loop {
            match tokio::time::timeout(Duration::from_secs(3), client.call(HostRequest::List)).await
            {
                Ok(Ok(HostResponse::List {
                    epoch,
                    agents,
                    terminals,
                    ..
                })) => {
                    if epoch == key.epoch
                        && agents.iter().chain(&terminals).any(|agent| {
                            agent.instance == key.instance
                                && agent.epoch == key.epoch
                                && agent.attachment.as_ref().is_some_and(|attachment| {
                                    attachment.key == key && attachment.ready
                                })
                        })
                    {
                        return Ok(());
                    }
                    return Err(error);
                }
                // Preserve the authenticated relay connection and input ownership while
                // read-only recovery is inconclusive. Never replay the ready mutation.
                Err(_) => {}
                Ok(Err(query_error))
                    if matches!(
                        query_error.downcast_ref::<HostRpcError>(),
                        Some(HostRpcError(RpcError::Busy(_) | RpcError::Io(_)))
                    ) => {}
                Ok(Err(query_error)) => {
                    return Err(
                        error.context(format!("Agent lease recovery failed: {query_error}"))
                    );
                }
                Ok(Ok(_)) => return Err(error.context("Unexpected Agent lease recovery response")),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    Ok(())
}

/// Show a terminal's shell from the Agent Host so its agent outlives the window. Without a usable
/// Host the shell runs here.
pub async fn run_shell(
    socket: &Path,
    previous: Option<(AgentInstanceId, Uuid)>,
    replace: Option<AttachmentKey>,
    ready_token: Option<Uuid>,
    colors: Option<&str>,
    argv: Vec<String>,
) -> anyhow::Result<()> {
    let end = ViewEnd::default();
    let kept = std::sync::atomic::AtomicBool::new(false);
    let hosted = async {
        // Without its window nothing could show the shell; start nothing in the Host.
        let surface = std::env::var("FLOWMUX_SURFACE_ID")
            .context("terminal surface is missing")?
            .parse()
            .context("terminal surface is invalid")?;
        let gui = std::env::var_os("FLOWMUX_SOCKET_PATH").context("terminal window is missing")?;
        let window = tokio::time::timeout(
            Duration::from_secs(5),
            flowmux_ipc::client::Client::connect(Path::new(&gui)),
        )
        .await
        .context("terminal window did not answer")??;
        let mut binding = None;
        if let Some((instance, epoch)) = previous {
            // Only an existing Host can still run the shell an agent kept alive.
            if let Ok(host) = HostClient::connect(socket, ClientRole::Observer).await {
                if let Ok(HostResponse::List { terminals, .. }) = host.call(HostRequest::List).await
                {
                    binding = terminals
                        .iter()
                        .find(|agent| {
                            agent.instance == instance
                                && agent.epoch == epoch
                                && agent.state == ProcessState::Running
                        })
                        .map(|agent| (instance, epoch, agent.pid));
                }
            }
        }
        kept.store(binding.is_some(), std::sync::atomic::Ordering::Relaxed);
        let (instance, epoch, shell) = match binding {
            Some(binding) => binding,
            None => {
                let environment = std::env::vars_os()
                    .filter_map(|(key, value)| {
                        Some((key.into_string().ok()?, value.into_string().ok()?))
                    })
                    .collect();
                let mut launch = flowmux_cli::agent_host::prepare_launch(
                    argv.clone(),
                    Path::new("."),
                    environment,
                )?;
                launch.shell = true;
                launch.size = size()?;
                launch.colors = colors.and_then(|colors| serde_json::from_str(colors).ok());
                let agent = flowmux_cli::agent_host::launch(socket, launch).await?;
                (agent.instance, agent.epoch, agent.pid)
            }
        };
        // The terminal reads its child's directory as its shell's, for the tab's
        // title and working directory. This process is that child; follow the shell.
        if let Some(shell) = shell {
            tokio::spawn(async move {
                loop {
                    if let Some(cwd) = flowmux_procmon::process_cwd(shell) {
                        if std::env::current_dir().ok().as_ref() != Some(&cwd)
                            && std::env::set_current_dir(&cwd).is_ok()
                        {
                            // The terminal looks again when it wakes; SIGWINCH is
                            // the wakeup it already expects and otherwise ignores.
                            unsafe { libc::kill(libc::getppid(), libc::SIGWINCH) };
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            });
        }
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            window.call(flowmux_ipc::Request::ShellBind {
                surface,
                instance,
                epoch,
                pid: shell,
            }),
        )
        .await
        .context("terminal window did not accept its shell")??;
        anyhow::ensure!(
            matches!(response, flowmux_ipc::Response::Ok),
            "terminal window rejected its shell: {response:?}"
        );
        view(
            socket,
            instance,
            epoch,
            true,
            replace,
            ready_token,
            colors,
            &end,
        )
        .await
    }
    .await;
    // Once the shell was shown, this terminal ends when and how the shell did.
    if end.shown.load(std::sync::atomic::Ordering::Relaxed) {
        hosted?;
        if let Some(status) = *end.status.lock().unwrap() {
            crate::pty_tee::exit_with(if libc::WIFSIGNALED(status) {
                crate::pty_tee::TeeExit::Signal(libc::WTERMSIG(status))
            } else {
                crate::pty_tee::TeeExit::Code(libc::WEXITSTATUS(status))
            });
        }
        return Ok(());
    }
    let Err(error) = hosted else { return Ok(()) };
    // The shell an agent kept alive still runs; another shell here would not be it.
    if kept.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(error.context("cannot show this terminal's shell; open it from Agents"));
    }
    // A shell left in the Host ends by itself. Not a warning: it would print into this terminal.
    tracing::debug!(%error, "terminal shell runs in this window, not in the Agent Host");
    let (program, arguments) = argv.split_first().context("shell command is required")?;
    Err(std::os::unix::process::CommandExt::exec(
        std::process::Command::new(program).args(arguments),
    )
    .into())
}

pub async fn run(
    socket: &Path,
    instance: AgentInstanceId,
    epoch: Uuid,
    require_gui: bool,
    replace: Option<AttachmentKey>,
    ready_token: Option<Uuid>,
    colors: Option<&str>,
) -> anyhow::Result<()> {
    view(
        socket,
        instance,
        epoch,
        require_gui,
        replace,
        ready_token,
        colors,
        &ViewEnd::default(),
    )
    .await
}

/// What a view saw of its execution.
#[derive(Default)]
struct ViewEnd {
    /// Set once the view has drawn the execution's current screen.
    shown: std::sync::atomic::AtomicBool,
    /// The wait status of an execution that ended while it was viewed.
    status: std::sync::Mutex<Option<i32>>,
}

#[allow(clippy::too_many_arguments)]
async fn view(
    socket: &Path,
    instance: AgentInstanceId,
    epoch: Uuid,
    require_gui: bool,
    replace: Option<AttachmentKey>,
    ready_token: Option<Uuid>,
    colors: Option<&str>,
    end: &ViewEnd,
) -> anyhow::Result<()> {
    let mut hangup = signal(SignalKind::hangup())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut resize = signal(SignalKind::window_change())?;
    let control = Arc::new(
        HostClient::connect(socket, ClientRole::Relay)
            .await
            .context("Cannot reconnect Agent: Agent Host is unavailable")?,
    );
    let frame_reader = HostClient::connect(socket, ClientRole::Observer).await?;
    let request = HostRequest::PrepareAttach {
        instance,
        epoch,
        request_id: Uuid::new_v4(),
        view: None,
        surface: require_gui
            .then(|| {
                std::env::var("FLOWMUX_SURFACE_ID")
                    .context("Agent GUI surface is missing")?
                    .parse()
                    .context("Agent GUI surface is invalid")
            })
            .transpose()?,
        require_gui,
        gui_ready: ready_token.is_some(),
        replace,
    };
    // A newly started Host can precede the GUI's next management reconnect.
    let prepare = async {
        loop {
            match control.call(request.clone()).await {
                Err(error)
                    if require_gui
                        && replace.is_none()
                        && matches!(
                            error.downcast_ref::<HostRpcError>(),
                            Some(HostRpcError(RpcError::Busy(_)))
                        ) =>
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                response => return response,
            }
        }
    };
    let response = tokio::select! {
        _ = hangup.recv() => return Ok(()),
        _ = terminate.recv() => return Ok(()),
        _ = interrupt.recv() => return Ok(()),
        response = tokio::time::timeout(Duration::from_secs(5), prepare) =>
            response.context("Cannot reconnect Agent: GUI management or attachment remained unavailable")??,
    };
    let HostResponse::Prepared { key, .. } = response else {
        bail!("unexpected Agent Host attachment response");
    };
    let HostResponse::Attached { frame, .. } = control
        .call(HostRequest::CommitAttach { key, size: size()? })
        .await?
    else {
        bail!("unexpected Agent Host commit response");
    };
    // Colors only refine query replies; a rejected value keeps the Host defaults.
    if let Some(colors) = colors.and_then(|colors| {
        serde_json::from_str(colors)
            .inspect_err(|error| tracing::warn!(%error, "ignoring invalid Agent view colors"))
            .ok()
    }) {
        if let Err(error) = control.call(HostRequest::ViewColors { key, colors }).await {
            tracing::warn!(%error, "Agent Host did not accept the view colors");
        }
    }
    let _raw = TerminalGuard::raw().context("opening agent terminal view")?;
    let stdin = io_fd(0)?;
    let stdout = io_fd(1)?;
    let (sender, mut incoming) = mpsc::channel(2);
    let reader = tokio::spawn(frames(frame_reader, key, frame, sender));
    let mut pending_input: Option<tokio::task::JoinHandle<anyhow::Result<()>>> = None;
    let result = async {
        let mut current: Option<(TerminalFrame,usize)> = None;
        let mut initialized = false;
        let mut input_sequence = 0;
        let mut input_modes = TerminalInputModes::default();
        let mut input_buffer = vec![0u8;MAX_INPUT_BYTES];
        let mut keepalive = tokio::time::interval(Duration::from_secs(10));
        // A control call can fail because the execution just ended; its last frame then arrives.
        // Report the failure only if none does.
        let mut failed: Option<(anyhow::Error, tokio::time::Instant)> = None;
        let grace = || tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            tokio::select! {
                _ = hangup.recv() => break,
                _ = terminate.recv() => break,
                _ = interrupt.recv() => break,
                _ = async {tokio::time::sleep_until(failed.as_ref().unwrap().1).await}, if failed.is_some() => {
                    return Err(failed.unwrap().0);
                }
                _ = resize.recv(), if failed.is_none() => {
                    if let Err(error) = control.call(HostRequest::Resize {key,size:size()?}).await {
                        failed = Some((error, grace()));
                    }
                }
                _ = keepalive.tick(), if failed.is_none() => {
                    if let Err(error) = control.call(HostRequest::KeepAlive).await {
                        failed = Some((error, grace()));
                    }
                }
                result = async {pending_input.as_mut().unwrap().await}, if pending_input.is_some() => {
                    pending_input = None;
                    if let Err(error) = result? {
                        failed = Some((error, grace()));
                    }
                }
                next = incoming.recv(), if current.is_none() => {
                    let Some(next) = next else {break;};
                    let frame = next?;
                    if frame.data.is_empty() {
                        if frame.ended {*end.status.lock().unwrap() = frame.exit_status; let _ = control.call(HostRequest::Detach {key}).await; break;}
                        if !initialized && frame.next_offset.is_none() {
                            tokio::select! {
                                _ = hangup.recv() => break,
                                _ = terminate.recv() => break,
                                _ = interrupt.recv() => break,
                                result = acknowledge_snapshot(&control, key, frame.sequence, ready_token) => result?,
                            }
                            initialized = true;
                            end.shown.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    } else {
                        input_modes.observe_output(frame.data.as_bytes());
                        current = Some((frame,0));
                    }
                }
                ready = stdout.writable(), if current.is_some() => {
                    let mut ready = ready?;
                    let (frame,offset) = current.as_mut().unwrap();
                    match ready.try_io(|fd| {
                        let data = &frame.data.as_bytes()[*offset..];
                        let n = unsafe {libc::write(fd.get_ref().as_raw_fd(),data.as_ptr().cast(),data.len())};
                        if n < 0 {Err(io::Error::last_os_error())} else {Ok(n as usize)}
                    }) {
                        Ok(Ok(0)) => bail!("agent view output closed"),
                        Ok(Ok(n)) => {
                            *offset += n;
                            if *offset == frame.data.len() {
                                if frame.next_offset.is_none() {
                                    if frame.ended {*end.status.lock().unwrap() = frame.exit_status; let _ = control.call(HostRequest::Detach {key}).await; break;}
                                    if !initialized {
                                        tokio::select! {
                                            _ = hangup.recv() => break,
                                            _ = terminate.recv() => break,
                                            _ = interrupt.recv() => break,
                                            result = acknowledge_snapshot(&control, key, frame.sequence, ready_token) => result?,
                                        }
                                        initialized = true;
                                        end.shown.store(true, std::sync::atomic::Ordering::Relaxed);
                                    }
                                }
                                current = None;
                            }
                        }
                        Ok(Err(error)) => return Err(error.into()),
                        Err(_) => {}
                    }
                }
                ready = stdin.readable(), if initialized && pending_input.is_none() && failed.is_none() => {
                    let mut ready = ready?;
                    match ready.try_io(|fd| {
                        let n = unsafe {libc::read(fd.get_ref().as_raw_fd(),input_buffer.as_mut_ptr().cast(),input_buffer.len())};
                        if n < 0 {Err(io::Error::last_os_error())} else {Ok(n as usize)}
                    }) {
                        Ok(Ok(0)) => break,
                        Ok(Ok(n)) => {
                            let data = input_modes.rewrite_input(&input_buffer[..n]).into_owned();
                            input_sequence += 1;
                            // Keep rendering and handling signals while Host input is full.
                            pending_input = Some(tokio::spawn(send_input(control.clone(),key,input_sequence,data)));
                        }
                        Ok(Err(error)) if error.raw_os_error() == Some(libc::EIO) => break,
                        Ok(Err(error)) => return Err(error.into()),
                        Err(_) => {}
                    }
                }
            }
        }
        Ok(())
    }.await;
    reader.abort();
    let _ = reader.await;
    if let Some(pending) = pending_input {
        pending.abort();
        let _ = pending.await;
    }
    // EOF releases the input lease. Never retry an ambiguous control mutation.
    drop(control);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Args {
        #[command(flatten)]
        relay: AgentRelayArgs,
    }

    #[tokio::test]
    async fn lost_ready_ack_requires_the_exact_committed_lease() {
        use flowmux_ipc::protocol::{Envelope, Payload, Request, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        for outcome in [
            "committed",
            "fragmented",
            "query_timeout",
            "query_busy",
            "query_closed",
            "pending",
            "superseded",
            "restarted",
        ] {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let socket = root.path().join("host.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let key = AttachmentKey {
                instance: AgentInstanceId::new(),
                epoch: Uuid::new_v4(),
                lease: Uuid::new_v4(),
                generation: 1,
            };
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let mut requests = 0;
                let mut pending_reply = None;
                while let Some(line) = lines.next_line().await.unwrap() {
                    let envelope: Envelope = serde_json::from_str(&line).unwrap();
                    let Payload::Request(Request::AgentHost { request }) = envelope.payload else {
                        panic!("unexpected request");
                    };
                    requests += 1;
                    let response = match request {
                        HostRequest::Hello {
                            role: ClientRole::Relay,
                            ..
                        } if requests == 1 => HostResponse::Hello {
                            version: PROTOCOL_VERSION,
                            epoch: key.epoch,
                            connection: 1,
                        },
                        HostRequest::ViewReady {
                            key: received,
                            sequence: 7,
                        } if requests == 2 => {
                            assert_eq!(received, key);
                            // Commit can happen before its full response reaches the relay.
                            if outcome == "fragmented" {
                                let reply = format!(
                                    "{}\n",
                                    serde_json::to_string(&Envelope {
                                        id: envelope.id,
                                        payload: Payload::Response(Response::AgentHost {
                                            response: HostResponse::Ok,
                                        }),
                                    })
                                    .unwrap()
                                );
                                write.write_all(&reply.as_bytes()[..8]).await.unwrap();
                                pending_reply = Some(reply[8..].to_owned());
                            }
                            continue;
                        }
                        HostRequest::List if requests == 3 && outcome == "query_closed" => break,
                        HostRequest::List if requests == 3 && outcome == "query_timeout" => {
                            continue;
                        }
                        HostRequest::List if requests == 3 && outcome == "query_busy" => {
                            let response = Envelope {
                                id: envelope.id,
                                payload: Payload::Response(Response::Error(RpcError::Busy(
                                    "fixture query unavailable".into(),
                                ))),
                            };
                            write
                                .write_all(
                                    format!("{}\n", serde_json::to_string(&response).unwrap())
                                        .as_bytes(),
                                )
                                .await
                                .unwrap();
                            continue;
                        }
                        HostRequest::List
                            if requests == 3
                                || (requests == 4
                                    && matches!(outcome, "query_timeout" | "query_busy")) =>
                        {
                            if let Some(suffix) = pending_reply.take() {
                                write.write_all(suffix.as_bytes()).await.unwrap();
                            }
                            let agent = AgentInstance {
                                instance: key.instance,
                                epoch: key.epoch,
                                provider: "fixture".into(),
                                label: "fixture".into(),
                                cwd: root.path().into(),
                                pid: None,
                                process_start: None,
                                boot_id: None,
                                state: ProcessState::Running,
                                force_terminate_allowed: false,
                                exit_status: None,
                                view: None,
                                view_owner: None,
                                error: None,
                                presence: None,
                                shell: false,
                                attachment: Some(AgentAttachment {
                                    key: if outcome == "superseded" {
                                        AttachmentKey {
                                            lease: Uuid::new_v4(),
                                            generation: 2,
                                            ..key
                                        }
                                    } else {
                                        key
                                    },
                                    ready: outcome != "pending",
                                    relay_pid: None,
                                    relay_start: None,
                                }),
                            };
                            HostResponse::List {
                                epoch: if outcome == "restarted" {
                                    Uuid::new_v4()
                                } else {
                                    key.epoch
                                },
                                revision: 1,
                                agents: vec![agent],
                                notifications: vec![],
                                terminals: Vec::new(),
                            }
                        }
                        HostRequest::KeepAlive if requests == 4 && outcome == "fragmented" => {
                            HostResponse::Ok
                        }
                        _ => panic!("admission must not be retried: {request:?}"),
                    };
                    let response = Envelope {
                        id: envelope.id,
                        payload: Payload::Response(Response::AgentHost { response }),
                    };
                    write
                        .write_all(
                            format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes(),
                        )
                        .await
                        .unwrap();
                }
                assert_eq!(
                    requests,
                    if matches!(outcome, "fragmented" | "query_timeout" | "query_busy") {
                        4
                    } else {
                        3
                    }
                );
            });
            let client = HostClient::connect(&socket, ClientRole::Relay)
                .await
                .unwrap();
            let result = acknowledge_snapshot(&client, key, 7, None).await;
            assert_eq!(
                result.is_ok(),
                matches!(
                    outcome,
                    "committed" | "fragmented" | "query_timeout" | "query_busy"
                ),
                "{outcome}: {result:?}"
            );
            if outcome == "fragmented" {
                assert!(matches!(
                    client.call(HostRequest::KeepAlive).await.unwrap(),
                    HostResponse::Ok
                ));
            }
            drop(client);
            server.await.unwrap();
        }
    }

    #[test]
    fn takeover_requires_both_lease_and_nonzero_generation() {
        let id = "00000000-0000-4000-8000-000000000001";
        let base = ["relay", "--instance", id, "--epoch", id];
        let ordinary = Args::try_parse_from(base).unwrap();
        assert!(ordinary.relay.replace_lease.is_none());
        assert!(ordinary.relay.replace_generation.is_none());
        assert!(ordinary.relay.ready_token.is_none());
        for suffix in [
            vec!["--replace-lease", id],
            vec!["--replace-generation", "1"],
            vec!["--replace-lease", id, "--replace-generation", "0"],
            vec!["--replace-lease", "invalid", "--replace-generation", "1"],
            vec!["--ready-token", id],
            vec![
                "--ready-token",
                id,
                "--replace-lease",
                id,
                "--replace-generation",
                "1",
            ],
        ] {
            assert!(Args::try_parse_from(base.into_iter().chain(suffix)).is_err());
        }
        let explicit = Args::try_parse_from(base.into_iter().chain([
            "--replace-lease",
            id,
            "--replace-generation",
            "42",
        ]))
        .unwrap();
        assert_eq!(
            explicit.relay.replace_lease,
            Some(Uuid::parse_str(id).unwrap())
        );
        assert_eq!(explicit.relay.replace_generation, Some(42));
        let admitted = Args::try_parse_from(base.into_iter().chain([
            "--require-gui",
            "--replace-lease",
            id,
            "--replace-generation",
            "42",
            "--ready-token",
            id,
        ]))
        .unwrap();
        assert_eq!(
            admitted.relay.ready_token,
            Some(Uuid::parse_str(id).unwrap())
        );
        let opened =
            Args::try_parse_from(
                base.into_iter()
                    .chain(["--require-gui", "--ready-token", id]),
            )
            .unwrap();
        assert!(opened.relay.replace_lease.is_none());
        assert_eq!(opened.relay.ready_token, Some(Uuid::parse_str(id).unwrap()));
    }
}
