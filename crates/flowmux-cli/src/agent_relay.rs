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
    #[arg(long)]
    pub instance: AgentInstanceId,
    #[arg(long)]
    pub epoch: Uuid,
    #[arg(long)]
    pub require_gui: bool,
    #[arg(long, requires = "replace_generation")]
    pub replace_lease: Option<Uuid>,
    #[arg(long, requires = "replace_lease", value_parser = clap::value_parser!(u64).range(1..))]
    pub replace_generation: Option<u64>,
    #[arg(long, requires = "require_gui")]
    pub ready_token: Option<Uuid>,
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

pub async fn run(
    socket: &Path,
    instance: AgentInstanceId,
    epoch: Uuid,
    require_gui: bool,
    replace: Option<AttachmentKey>,
    ready_token: Option<Uuid>,
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
        loop {
            tokio::select! {
                _ = hangup.recv() => break,
                _ = terminate.recv() => break,
                _ = interrupt.recv() => break,
                _ = resize.recv() => {
                    control.call(HostRequest::Resize {key,size:size()?}).await?;
                }
                _ = keepalive.tick() => {control.call(HostRequest::KeepAlive).await?;}
                result = async {pending_input.as_mut().unwrap().await}, if pending_input.is_some() => {
                    pending_input = None;
                    result??;
                }
                next = incoming.recv(), if current.is_none() => {
                    let Some(next) = next else {break;};
                    let frame = next?;
                    if frame.data.is_empty() {
                        if frame.ended {let _ = control.call(HostRequest::Detach {key}).await; break;}
                        if !initialized && frame.next_offset.is_none() {
                            acknowledge_snapshot(&control, key, frame.sequence, ready_token).await?;
                            initialized = true;
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
                                    if frame.ended {let _ = control.call(HostRequest::Detach {key}).await; break;}
                                    if !initialized {
                                        acknowledge_snapshot(&control, key, frame.sequence, ready_token).await?;
                                        initialized = true;
                                    }
                                }
                                current = None;
                            }
                        }
                        Ok(Err(error)) => return Err(error.into()),
                        Err(_) => {}
                    }
                }
                ready = stdin.readable(), if initialized && pending_input.is_none() => {
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
