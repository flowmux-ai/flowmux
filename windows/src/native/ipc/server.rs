// SPDX-License-Identifier: GPL-3.0-or-later
use super::{
    transport::{Event, Pipe},
    *,
};
use std::{
    path::Path,
    sync::atomic::{AtomicUsize, Ordering},
    thread::JoinHandle,
    time::Instant,
};

pub(super) const MAX_CLIENTS: u32 = 16;
pub(super) const MAX_REPLY_BYTES: usize = 16 * MAX_MESSAGE_BYTES;

// Keep a permit until the UI releases every clone of the reply. A timed-out
// client cannot keep filling a stalled UI's event queue with new commands.
struct CommandLease(Arc<AtomicUsize>);
impl Drop for CommandLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}
#[derive(Clone)]
pub struct Reply {
    sender: mpsc::SyncSender<Value>,
    _lease: Arc<CommandLease>,
}
impl Reply {
    fn new(sender: mpsc::SyncSender<Value>, pending: &Arc<AtomicUsize>) -> Option<Self> {
        pending
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_CLIENTS as usize).then_some(count + 1)
            })
            .ok()?;
        Some(Self {
            sender,
            _lease: Arc::new(CommandLease(pending.clone())),
        })
    }
    pub fn try_send(&self, value: Value) -> Result<(), mpsc::TrySendError<Value>> {
        self.sender.try_send(value)
    }
}
#[derive(Clone, Copy)]
struct Limits {
    request: Duration,
    command: Duration,
    write: Duration,
    close: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            request: Duration::from_secs(5),
            command: Duration::from_secs(15),
            write: Duration::from_secs(5),
            close: Duration::from_secs(2),
        }
    }
}

pub struct Server {
    pub name: String,
    discovery: PathBuf,
    stop: Arc<Event>,
    workers: Vec<JoinHandle<()>>,
}
impl Server {
    pub fn start(
        emit: impl Fn(Request, Reply) + Send + Sync + 'static,
        shutdown: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        Self::start_in(
            &data_dir()?.join("instances"),
            emit,
            shutdown,
            Limits::default(),
        )
    }
    fn start_in(
        directory: &Path,
        emit: impl Fn(Request, Reply) + Send + Sync + 'static,
        shutdown: impl Fn() + Send + Sync + 'static,
        limits: Limits,
    ) -> anyhow::Result<Self> {
        let name = format!(
            r"\\.\pipe\flowmux-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let descriptor = user_descriptor()?;
        // Allocate the complete fixed pool before advertising it. Any failure
        // drops the handles; partial thread startup is rolled back by Server::drop.
        let instances = (0..MAX_CLIENTS)
            .map(|i| Pipe::new(make_pipe(&name, &descriptor, i == 0)?).map_err(anyhow::Error::from))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut server = Self {
            name,
            discovery: directory.join(format!("{}.json", std::process::id())),
            stop: Arc::new(Event::new()?),
            workers: Vec::new(),
        };
        let emit = Arc::new(emit);
        let shutdown = Arc::new(shutdown);
        let pending = Arc::new(AtomicUsize::new(0));
        for (index, mut pipe) in instances.into_iter().enumerate() {
            let emit = emit.clone();
            let shutdown = shutdown.clone();
            let pending = pending.clone();
            let stop = server.stop.clone();
            let discovery = server.discovery.clone();
            let name = server.name.clone();
            server.workers.push(
                std::thread::Builder::new()
                    .name(format!("flowmux-ipc-{index}"))
                    .spawn(move || {
                        while !stop.is_set() {
                            if let Err(error) = pipe.accept(&stop) {
                                if !stop.is_set() {
                                    super::super::host::report(&format!(
                                        "IPC accept failed: {error}"
                                    ));
                                    stop.signal();
                                    discovery::remove_if_current(&discovery, &name);
                                }
                                break;
                            }
                            serve(&mut pipe, &stop, limits, &pending, &*emit, &*shutdown);
                            if let Err(error) = pipe.disconnect() {
                                super::super::host::report(&format!(
                                    "IPC disconnect failed: {error}"
                                ));
                                stop.signal();
                                discovery::remove_if_current(&discovery, &name);
                                break;
                            }
                        }
                    })?,
            );
        }
        discovery::publish(directory, &server.name)?;
        anyhow::ensure!(!server.stop.is_set(), "IPC pool stopped during startup");
        Ok(server)
    }
    pub fn shutdown(&mut self) {
        discovery::remove_if_current(&self.discovery, &self.name);
        self.stop.signal();
        // All outstanding kernel operations drain before their buffers are freed.
        // GUI reply waits also check stop, so joining never requires the UI loop.
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve(
    pipe: &mut Pipe,
    stop: &Event,
    limits: Limits,
    pending: &Arc<AtomicUsize>,
    emit: &dyn Fn(Request, Reply),
    shutdown: &dyn Fn(),
) {
    let Ok(frame) = pipe.read_frame(MAX_MESSAGE_BYTES, Instant::now() + limits.request, stop)
    else {
        return;
    };
    let mut quitting = false;
    let response = (|| -> anyhow::Result<Value> {
        let command: Request = serde_json::from_slice(&frame)?;
        let budget = match &command.command {
            Command::Browser { op: crate::browser::Op::Wait { options, .. } } => options.ipc_budget(limits.command, 5)?,
            _ => limits.command,
        };
        quitting = matches!(command.command, Command::Quit { .. });
        let (send, receive) = mpsc::sync_channel(1);
        let reply = Reply::new(send, pending)
            .context("window request queue is full; request was not dispatched")?;
        emit(command, reply);
        let deadline = Instant::now() + budget;
        loop {
            anyhow::ensure!(!stop.is_set(), "IPC server is stopping");
            let left = deadline.saturating_duration_since(Instant::now());
            anyhow::ensure!(!left.is_zero(), "window did not answer within the IPC command deadline; command outcome may be unknown");
            match receive.recv_timeout(left.min(Duration::from_millis(50))) {
                Ok(value) => return Ok(value),
                Err(mpsc::RecvTimeoutError::Timeout) => {},
                Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("window closed before answering"),
            }
        }
    })().unwrap_or_else(|error| json!({"error":error.to_string()}));
    let quit_accepted = quitting && response.get("error").is_none();
    let bytes = encode_reply(&response);
    if pipe
        .write_all(&bytes, Instant::now() + limits.write, stop)
        .is_ok()
    {
        pipe.wait_for_close(Instant::now() + limits.close, stop);
    }
    // A successful quit must not be held hostage by a peer that never consumes
    // the reply. Normal clients close after reading; the close wait is bounded.
    if quit_accepted && !stop.is_set() {
        shutdown();
    }
}

fn encode_reply(value: &Value) -> Vec<u8> {
    struct Capped(Vec<u8>);
    impl Write for Capped {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > (MAX_REPLY_BYTES - 1).saturating_sub(self.0.len()) {
                return Err(std::io::Error::other("IPC reply exceeds 16 MiB"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Capped(Vec::new());
    if serde_json::to_writer(&mut output, value).is_err() {
        return b"{\"error\":\"IPC reply exceeds 16 MiB\"}\n".to_vec();
    }
    output.0.push(b'\n');
    output.0
}

#[cfg(test)]
mod tests;
