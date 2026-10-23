// SPDX-License-Identifier: GPL-3.0-or-later
//! Unix socket server dispatching requests through a supplied [`Handler`].

use crate::protocol::{Envelope, Payload, Request, Response, RpcError, SshRequest};
use std::future::Future;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tracing::{info, warn};

/// Hard cap on a single envelope, including snapshots. Bounds memory when a
/// peer streams without a terminating `\n`.
pub(crate) const MAX_LINE_BYTES: usize = 1024 * 1024;
// Independent connection pools protect control admission from ordinary peers.
// One request at a time per connection bounds queued/in-flight handlers.
const MAX_CONNECTIONS: usize = 64;
const MAX_CONTROL_CONNECTIONS: usize = 16;
const MAX_MUTATIONS: usize = 32;
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

fn query_timeout(request: &Request) -> Option<Duration> {
    match request {
        Request::BrowserOnSurface { request, .. }
            if !matches!(request.as_ref(), Request::BrowserOnSurface { .. }) =>
        {
            query_timeout(request)
        }
        Request::BrowserWait { timeout_ms, .. } => {
            Some(Duration::from_millis(*timeout_ms).saturating_add(QUERY_TIMEOUT))
        }
        Request::Ping
        | Request::WorkspaceList
        | Request::WorkspaceTree
        | Request::WorkspaceCurrent
        | Request::PaneReadScreen { .. }
        | Request::NotificationsList { .. }
        | Request::AgentSessionGet { .. }
        | Request::AgentSurfaceCurrent { .. }
        | Request::AgentSurfaceResolve { .. }
        | Request::Ssh {
            request: SshRequest::Status { .. },
        }
        | Request::BrowserSnapshot { .. }
        | Request::BrowserDialog {
            action: crate::protocol::BrowserDialogAction::Status,
            ..
        }
        | Request::BrowserDownload {
            action: crate::protocol::BrowserDownloadAction::List,
            ..
        }
        | Request::BrowserMedia {
            action: crate::protocol::BrowserMediaAction::Status,
            ..
        }
        | Request::BrowserUrl { .. }
        | Request::BrowserTitle { .. }
        | Request::BrowserText { .. }
        | Request::BrowserValue { .. }
        | Request::BrowserAttr { .. }
        | Request::BrowserIsVisible { .. }
        | Request::BrowserIsEnabled { .. }
        | Request::BrowserIsChecked { .. }
        | Request::BrowserCount { .. } => Some(QUERY_TIMEOUT),
        // Mutations (including raw JS and screenshot file writes) may already
        // have effects or be waiting for user confirmation. Do not cancel them.
        _ => None,
    }
}

pub trait Handler: Send + Sync + 'static {
    fn handle<'a>(&'a self, req: Request) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>>;

    /// PID supplied by the kernel, never by the request payload.
    fn handle_from<'a>(
        &'a self,
        req: Request,
        _peer_pid: Option<u32>,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        self.handle(req)
    }
}

struct SocketOwner {
    _lock: std::fs::File,
    paths: Vec<(PathBuf, u64, u64)>,
}

impl SocketOwner {
    fn acquire(socket: &Path) -> anyhow::Result<Self> {
        let mut path = socket.as_os_str().to_os_string();
        path.push(".lock");
        // Keep the lock file: unlinking it would let later owners lock a
        // different inode while an earlier process still owns this one.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        lock.try_lock()?;
        Ok(Self {
            _lock: lock,
            paths: Vec::new(),
        })
    }

    fn bind(&mut self, path: &Path) -> std::io::Result<UnixListener> {
        let listener = UnixListener::bind(path)?;
        let metadata = std::fs::symlink_metadata(path)?;
        self.paths
            .push((path.to_owned(), metadata.dev(), metadata.ino()));
        Ok(listener)
    }
}

impl Drop for SocketOwner {
    fn drop(&mut self) {
        for (path, dev, ino) in &self.paths {
            if std::fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.dev() == *dev && metadata.ino() == *ino)
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

async fn remove_stale_socket(path: &Path) -> std::io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let in_use = || {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("IPC path is in use: {}", path.display()),
        )
    };
    if !metadata.file_type().is_socket() {
        return Err(in_use());
    }
    match timeout(Duration::from_millis(250), UnixStream::connect(path)).await {
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {}
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Ok(Err(error)) => return Err(error),
        _ => return Err(in_use()),
    }
    let current = std::fs::symlink_metadata(path)?;
    if current.dev() != metadata.dev() || current.ino() != metadata.ino() {
        return Err(in_use());
    }
    std::fs::remove_file(path)
}

pub async fn run<H: Handler>(socket: &Path, handler: Arc<H>) -> anyhow::Result<()> {
    let mut owner = SocketOwner::acquire(socket)?;
    let control_path = crate::control_socket_path(socket);
    remove_stale_socket(socket).await?;
    remove_stale_socket(&control_path).await?;
    // Bind control first so normal paths publish both endpoints together.
    let control_listener = match owner.bind(&control_path) {
        Ok(listener) => Some(listener),
        // The original socket can fit sun_path while its companion does not.
        // ponytail: overlong custom paths keep regular admission; shorten the
        // runtime path to restore control capacity.
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
            warn!(path = %control_path.display(), %error, "control socket unavailable; using regular admission");
            None
        }
        Err(error) => return Err(error.into()),
    };
    let listener = owner.bind(socket)?;
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let control_connections = Arc::new(Semaphore::new(MAX_CONTROL_CONNECTIONS));
    let mutations = Arc::new(Semaphore::new(MAX_MUTATIONS));
    info!(path = %socket.display(), "flowmux daemon listening");
    loop {
        let (accepted, control) = tokio::select! {
            result = listener.accept() => (result, false),
            result = async { control_listener.as_ref().unwrap().accept().await },
                if control_listener.is_some() => (result, true),
        };
        let (stream, _) = match accepted {
            Ok(connection) => connection,
            Err(error) => {
                // Resource exhaustion must not permanently disconnect a live GUI.
                warn!(control, %error, "IPC accept failed; retrying");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let pool = if control {
            &control_connections
        } else {
            &connections
        };
        let Ok(permit) = pool.clone().try_acquire_owned() else {
            warn!(control, "IPC connection limit reached; dropping connection");
            continue;
        };
        let h = handler.clone();
        let mutations = mutations.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = serve_connection(stream, h, control, mutations).await {
                warn!(error = %e, "client disconnected with error");
            }
        });
    }
}

#[cfg(test)]
async fn serve_one<H: Handler>(stream: UnixStream, handler: Arc<H>) -> anyhow::Result<()> {
    serve_connection(
        stream,
        handler,
        false,
        Arc::new(Semaphore::new(MAX_MUTATIONS)),
    )
    .await
}

async fn serve_connection<H: Handler>(
    stream: UnixStream,
    handler: Arc<H>,
    control: bool,
    mutations: Arc<Semaphore>,
) -> anyhow::Result<()> {
    let peer_pid = stream
        .peer_cred()
        .ok()
        .and_then(|cred| cred.pid())
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0);
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut buf = String::new();
    loop {
        match timeout(
            IO_TIMEOUT,
            read_line_bounded(&mut reader, &mut buf, MAX_LINE_BYTES),
        )
        .await?
        {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                // Oversized or non-utf8 line. Closing is the safe response: the
                // peer has either misbehaved or is on a corrupt stream we can't
                // resync on.
                warn!(error = %e, "client sent malformed framing; dropping connection");
                return Ok(());
            }
            Err(e) => return Err(e.into()),
        }
        let env: Envelope = match serde_json::from_str(buf.trim_end()) {
            Ok(e) => e,
            Err(e) => {
                warn!(error = %e, raw = %buf, "malformed envelope");
                // A newer client can send a verb this build predates. Reply
                // when the id is readable so it fails now, not at IO_TIMEOUT.
                if let Ok(RequestId { id }) = serde_json::from_str(buf.trim_end()) {
                    let response = Response::Error(RpcError::InvalidArgument(
                        "request not recognized; not started. If flowmux was just updated, restart it".into(),
                    ));
                    write_response(&mut w, id, response).await?;
                }
                continue;
            }
        };
        let response = match env.payload {
            Payload::Request(req) => {
                let is_control = req.uses_control_socket();
                if control && !is_control {
                    Response::Error(RpcError::InvalidArgument(
                        "request requires the regular socket; not started".into(),
                    ))
                } else if let Some(budget) = query_timeout(&req) {
                    timeout(budget, handler.handle_from(req, peer_pid))
                        .await
                        .unwrap_or_else(|_| {
                            Response::Error(RpcError::Io(format!(
                                "query response timed out after {} ms",
                                budget.as_millis()
                            )))
                        })
                } else if is_control {
                    handler.handle_from(req, peer_pid).await
                } else {
                    // No hidden admission queue: reject before calling the handler.
                    // After admission, even a dropped client or a confirmation
                    // dialog cannot cause us to cancel/replay an ambiguous effect.
                    match mutations.try_acquire() {
                        Ok(_permit) => handler.handle_from(req, peer_pid).await,
                        Err(_) => Response::Error(RpcError::Busy(
                            "mutation capacity reached; request not started".into(),
                        )),
                    }
                }
            }
            Payload::Response(_) | Payload::Event(_) => Response::Error(RpcError::InvalidArgument(
                "client sent non-request payload".into(),
            )),
        };
        write_response(&mut w, env.id, response).await?;
    }
}

#[derive(serde::Deserialize)]
struct RequestId {
    id: u64,
}

async fn write_response(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    id: u64,
    response: Response,
) -> anyhow::Result<()> {
    let out = Envelope {
        id,
        payload: Payload::Response(response),
    };
    let mut line = serde_json::to_string(&out)?;
    line.push('\n');
    timeout(IO_TIMEOUT, async {
        w.write_all(line.as_bytes()).await?;
        w.flush().await
    })
    .await??;
    Ok(())
}

/// Read one `\n`-terminated line into `out`, refusing to grow past `max`
/// bytes. Mirrors `BufRead::read_line` in shape but caps the buffer so a
/// peer that never sends a newline cannot drive memory growth.
///
/// Returns `Ok(0)` on clean EOF, `Ok(n)` on a complete line of length `n`
/// (newline included). On overflow returns `InvalidData`; the caller is
/// expected to drop the connection.
async fn read_line_bounded<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    out: &mut String,
    max: usize,
) -> std::io::Result<usize> {
    out.clear();
    let mut bytes: Vec<u8> = Vec::new();
    loop {
        let (consumed, found_newline) = {
            let chunk = reader.fill_buf().await?;
            if chunk.is_empty() {
                if bytes.is_empty() {
                    return Ok(0);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stream ended without newline",
                ));
            }
            match chunk.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    let take = pos + 1;
                    if bytes.len() + take > max {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("line exceeded {max} bytes"),
                        ));
                    }
                    bytes.extend_from_slice(&chunk[..take]);
                    (take, true)
                }
                None => {
                    if bytes.len() + chunk.len() > max {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("line exceeded {max} bytes"),
                        ));
                    }
                    let n = chunk.len();
                    bytes.extend_from_slice(chunk);
                    (n, false)
                }
            }
        };
        reader.consume(consumed);
        if found_newline {
            break;
        }
    }
    match String::from_utf8(bytes) {
        Ok(s) => {
            let n = s.len();
            *out = s;
            Ok(n)
        }
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "non-utf8 line",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn duplicate_server_preserves_both_live_endpoints() {
        let dir = tempfile::Builder::new()
            .prefix("fm-owner-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("ipc.sock");
        let server_socket = socket.clone();
        let first = tokio::spawn(async move { run(&server_socket, Arc::new(PingHandler)).await });
        let client = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(client) = crate::client::Client::connect(&socket).await {
                    break client;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            client.call(Request::Ping).await.unwrap(),
            Response::Pong
        ));
        let duplicate = timeout(Duration::from_secs(1), run(&socket, Arc::new(PingHandler))).await;
        assert!(
            matches!(duplicate, Ok(Err(_))),
            "duplicate server must fail: {duplicate:?}"
        );
        for endpoint in [&socket, &crate::control_socket_path(&socket)] {
            let client = crate::client::Client::connect(endpoint).await.unwrap();
            assert!(matches!(
                client.call(Request::Ping).await.unwrap(),
                Response::Pong
            ));
        }
        assert!(matches!(
            client.call(Request::Ping).await.unwrap(),
            Response::Pong
        ));
        first.abort();
        let _ = first.await;
    }

    #[tokio::test]
    async fn startup_preserves_foreign_endpoints_and_cleans_its_own() {
        let dir = tempfile::Builder::new()
            .prefix("fm-owner-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("ipc.sock");
        let control = crate::control_socket_path(&socket);
        for occupied in [&socket, &control] {
            let other = if occupied == &socket {
                &control
            } else {
                &socket
            };
            let foreign = UnixListener::bind(occupied).unwrap();
            let result = timeout(Duration::from_secs(1), run(&socket, Arc::new(PingHandler))).await;
            assert!(matches!(result, Ok(Err(_))), "live endpoint was replaced");
            assert!(UnixStream::connect(occupied).await.is_ok());
            assert!(!other.exists());
            drop(foreign);
            std::fs::remove_file(occupied).unwrap();
        }
        std::fs::write(&socket, b"keep me").unwrap();
        assert!(run(&socket, Arc::new(PingHandler)).await.is_err());
        assert_eq!(std::fs::read(&socket).unwrap(), b"keep me");
        assert!(!control.exists());
        std::fs::remove_file(&socket).unwrap();
        // Stale socket files left by a crashed process can be reclaimed.
        drop(UnixListener::bind(&socket).unwrap());
        drop(UnixListener::bind(&control).unwrap());
        let server_socket = socket.clone();
        let server = tokio::spawn(async move { run(&server_socket, Arc::new(PingHandler)).await });
        timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(client) = crate::client::Client::connect(&socket).await {
                    assert!(matches!(
                        client.call(Request::Ping).await.unwrap(),
                        Response::Pong
                    ));
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        server.abort();
        let _ = server.await;
        assert!(!socket.exists());
        assert!(!control.exists());
    }

    #[tokio::test]
    async fn partial_bind_cleanup_preserves_replaced_paths() {
        let dir = tempfile::Builder::new()
            .prefix("fm-owner-")
            .tempdir_in("/tmp")
            .unwrap();
        let socket = dir.path().join("ipc.sock");
        let control = crate::control_socket_path(&socket);
        let mut owner = SocketOwner::acquire(&socket).unwrap();
        let listener = owner.bind(&control).unwrap();
        std::fs::write(&socket, b"occupied").unwrap();
        assert!(owner.bind(&socket).is_err());
        drop(listener);
        drop(owner);
        assert!(!control.exists());
        assert_eq!(std::fs::read(&socket).unwrap(), b"occupied");
        std::fs::remove_file(&socket).unwrap();

        let mut owner = SocketOwner::acquire(&socket).unwrap();
        let listener = owner.bind(&socket).unwrap();
        std::fs::remove_file(&socket).unwrap();
        std::os::unix::fs::symlink("missing-target", &socket).unwrap();
        drop(listener);
        drop(owner);
        assert_eq!(
            std::fs::read_link(&socket).unwrap(),
            Path::new("missing-target")
        );
        assert!(run(&socket, Arc::new(PingHandler)).await.is_err());
        assert!(std::fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_symlink());
    }
    use crate::protocol::Event;
    use flowmux_core::{NotificationLevel, WorkspaceId};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    struct PingHandler;

    #[test]
    fn surface_target_preserves_browser_wait_timeout() {
        let request = Request::BrowserOnSurface {
            surface: flowmux_core::SurfaceId::new(),
            request: Box::new(Request::BrowserWait {
                pane: flowmux_core::PaneId::new(),
                condition: crate::protocol::BrowserWaitCondition::Text("ready".into()),
                timeout_ms: 60_000,
                poll_ms: 100,
            }),
        };
        assert_eq!(query_timeout(&request), Some(Duration::from_secs(70)));
    }

    #[test]
    fn explicit_browser_wait_keeps_its_budget_and_effects_are_not_timed_out() {
        let pane = flowmux_core::PaneId::new();
        assert_eq!(
            query_timeout(&Request::BrowserWait {
                pane,
                condition: crate::protocol::BrowserWaitCondition::Text("ready".into()),
                timeout_ms: 60_000,
                poll_ms: 100,
            }),
            Some(Duration::from_secs(70))
        );
        for request in [
            Request::SurfaceClose {
                pane,
                surface: flowmux_core::SurfaceId::new(),
            },
            Request::BrowserEval {
                pane,
                source: "run()".into(),
            },
            Request::BrowserScreenshot {
                pane,
                path: "/tmp/screenshot.png".into(),
                full_page: false,
                element: None,
            },
        ] {
            assert_eq!(query_timeout(&request), None);
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dispatch_passes_kernel_peer_pid() {
        struct PeerHandler;
        impl Handler for PeerHandler {
            fn handle<'a>(
                &'a self,
                _: Request,
            ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
                panic!("must dispatch with peer credentials")
            }
            fn handle_from<'a>(
                &'a self,
                _: Request,
                peer: Option<u32>,
            ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
                Box::pin(async move {
                    assert_eq!(peer, Some(std::process::id()));
                    Response::Ok
                })
            }
        }
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_one(server, Arc::new(PeerHandler)));
        write_envelope(
            &mut client,
            Envelope {
                id: 1,
                payload: Payload::Request(Request::AgentSurfaceCurrent {
                    agent: "codex".into(),
                }),
            },
        )
        .await;
        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(matches!(
            serde_json::from_str::<Envelope>(&line).unwrap().payload,
            Payload::Response(Response::Ok)
        ));
        drop(reader);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn client_that_never_reads_cannot_hold_a_response_forever() {
        struct LargeReply(tokio::sync::Notify);
        impl Handler for LargeReply {
            fn handle<'a>(
                &'a self,
                _: Request,
            ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
                Box::pin(async move {
                    self.0.notify_one();
                    Response::ScreenContents {
                        text: "x".repeat(2 * MAX_LINE_BYTES),
                    }
                })
            }
        }
        let handler = Arc::new(LargeReply(tokio::sync::Notify::new()));
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_one(server, handler.clone()));
        write_envelope(
            &mut client,
            Envelope {
                id: 1,
                payload: Payload::Request(Request::Ping),
            },
        )
        .await;
        handler.0.notified().await;
        tokio::task::yield_now().await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::resume();
        assert!(tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("blocked writes must expire")
            .unwrap()
            .is_err());
    }

    #[tokio::test]
    async fn excess_connections_are_rejected_and_capacity_recovers() {
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("bounded.sock");
        let server_socket = socket.clone();
        let server = tokio::spawn(async move { run(&server_socket, Arc::new(PingHandler)).await });
        for (socket, limit) in [
            (socket.clone(), MAX_CONNECTIONS),
            (crate::control_socket_path(&socket), MAX_CONTROL_CONNECTIONS),
        ] {
            let mut clients = Vec::new();
            for _ in 0..limit {
                let stream = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if let Ok(stream) = UnixStream::connect(&socket).await {
                            break stream;
                        }
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                })
                .await
                .unwrap();
                let mut reader = BufReader::new(stream);
                write_envelope(
                    reader.get_mut(),
                    Envelope {
                        id: 1,
                        payload: Payload::Request(Request::Ping),
                    },
                )
                .await;
                assert!(matches!(
                    read_envelope(&mut reader).await.payload,
                    Payload::Response(Response::Pong)
                ));
                clients.push(reader);
            }
            let extra = UnixStream::connect(&socket).await.unwrap();
            let mut extra = BufReader::new(extra);
            let closed =
                tokio::time::timeout(Duration::from_secs(1), extra.read_line(&mut String::new()))
                    .await;
            // Abort the accept loop even when the regression assertion fails.
            if closed.is_err() {
                server.abort();
            }
            assert_eq!(
                closed
                    .expect("excess connections must be rejected")
                    .unwrap(),
                0
            );
            drop(clients.pop());
            tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let mut reader = BufReader::new(UnixStream::connect(&socket).await.unwrap());
                    let request = b"{\"id\":2,\"kind\":\"request\",\"verb\":\"ping\"}\n";
                    if reader.get_mut().write_all(request).await.is_ok() {
                        let mut line = String::new();
                        if matches!(reader.read_line(&mut line).await, Ok(n) if n > 0) {
                            let reply: Envelope = serde_json::from_str(&line).unwrap();
                            assert!(matches!(reply.payload, Payload::Response(Response::Pong)));
                            break;
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("closing a client must release capacity");
            drop(clients);
        }
        server.abort();
    }

    #[tokio::test]
    async fn mutation_admission_rejects_before_effects_and_preserves_started_work() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Gate {
            entered: Semaphore,
            release: Semaphore,
            completed: AtomicUsize,
        }
        impl Handler for Gate {
            fn handle<'a>(
                &'a self,
                req: Request,
            ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
                Box::pin(async move {
                    match req {
                        Request::Ping => Response::Pong,
                        Request::Notify { .. } => Response::Ok,
                        Request::PaneSendKeys { .. } => {
                            self.entered.add_permits(1);
                            self.release.acquire().await.unwrap().forget();
                            self.completed.fetch_add(1, Ordering::SeqCst);
                            Response::Ok
                        }
                        other => panic!("unexpected {other:?}"),
                    }
                })
            }
        }
        let handler = Arc::new(Gate {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            completed: AtomicUsize::new(0),
        });
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("admit.sock");
        let task_socket = socket.clone();
        let task_handler = handler.clone();
        let server = tokio::spawn(async move { run(&task_socket, task_handler).await });
        let request = Request::PaneSendKeys {
            pane: flowmux_core::PaneId::new(),
            keys: "effect".into(),
        };
        let mut pending = Vec::new();
        timeout(Duration::from_secs(2), async {
            for _ in 0..MAX_MUTATIONS {
                let mut stream = loop {
                    if let Ok(stream) = UnixStream::connect(&socket).await {
                        break stream;
                    }
                    tokio::task::yield_now().await;
                };
                write_envelope(
                    &mut stream,
                    Envelope {
                        id: 1,
                        payload: Payload::Request(request.clone()),
                    },
                )
                .await;
                handler.entered.acquire().await.unwrap().forget();
                pending.push(BufReader::new(stream));
            }
        })
        .await
        .unwrap();
        let client = crate::client::Client::connect(&socket).await.unwrap();
        assert!(matches!(
            client.call(request.clone()).await.unwrap(),
            Response::Error(RpcError::Busy(_))
        ));
        assert!(matches!(
            client.call(Request::Ping).await.unwrap(),
            Response::Pong
        ));
        assert!(matches!(
            client
                .call(Request::Notify {
                    pane: None,
                    surface: None,
                    title: "hook".into(),
                    body: "done".into(),
                    level: NotificationLevel::Info,
                })
                .await
                .unwrap(),
            Response::Ok
        ));
        // A caller cannot bypass the mutation cap by selecting the control socket.
        let mut control = BufReader::new(
            UnixStream::connect(crate::control_socket_path(&socket))
                .await
                .unwrap(),
        );
        write_envelope(
            control.get_mut(),
            Envelope {
                id: 9,
                payload: Payload::Request(request),
            },
        )
        .await;
        assert!(matches!(
            read_envelope(&mut control).await.payload,
            Payload::Response(Response::Error(RpcError::InvalidArgument(_)))
        ));
        assert_eq!(handler.completed.load(Ordering::SeqCst), 0);
        // Losing a client after admission must not cancel its already-started effect.
        drop(pending.pop());
        handler.release.add_permits(MAX_MUTATIONS);
        timeout(Duration::from_secs(2), async {
            for mut reader in pending {
                assert!(matches!(
                    read_envelope(&mut reader).await.payload,
                    Payload::Response(Response::Ok)
                ));
            }
            while handler.completed.load(Ordering::SeqCst) < MAX_MUTATIONS {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        // Neither rejected request was left waiting to run after capacity returned.
        assert_eq!(handler.entered.available_permits(), 0);
        assert_eq!(handler.release.available_permits(), 0);
        assert_eq!(handler.completed.load(Ordering::SeqCst), MAX_MUTATIONS);
        server.abort();
    }

    #[tokio::test]
    async fn longest_supported_regular_socket_remains_usable() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::Builder::new()
            .prefix("fm-long-")
            .tempdir_in("/tmp")
            .unwrap();
        let max = if cfg!(target_os = "macos") { 103 } else { 107 };
        let filename_len = max - dir.path().as_os_str().as_bytes().len() - 1;
        let socket = dir
            .path()
            .join(format!("{}.sock", "s".repeat(filename_len - 5)));
        // This path is valid for the original, regular endpoint.
        drop(UnixListener::bind(&socket).unwrap());
        let server_socket = socket.clone();
        let server = tokio::spawn(async move { run(&server_socket, Arc::new(PingHandler)).await });
        let response = timeout(Duration::from_secs(1), async {
            loop {
                if let Ok(client) = crate::client::Client::connect(&socket).await {
                    break client.call(Request::Ping).await;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        server.abort();
        assert!(matches!(response, Ok(Ok(Response::Pong))), "{response:?}");
    }

    #[tokio::test]
    async fn status_queries_survive_idle_connection_saturation() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("idle.sock");
        let server_socket = socket.clone();
        let server = tokio::spawn(async move { run(&server_socket, Arc::new(PingHandler)).await });
        let mut held = Vec::new();
        for _ in 0..MAX_CONNECTIONS {
            let stream = timeout(Duration::from_secs(2), async {
                loop {
                    if let Ok(stream) = UnixStream::connect(&socket).await {
                        break stream;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let mut reader = BufReader::new(stream);
            write_envelope(
                reader.get_mut(),
                Envelope {
                    id: 1,
                    payload: Payload::Request(Request::Ping),
                },
            )
            .await;
            assert!(matches!(
                read_envelope(&mut reader).await.payload,
                Payload::Response(Response::Pong)
            ));
            held.push(reader);
        }
        let alias = dir.path().join("current.sock");
        std::os::unix::fs::symlink(&socket, &alias).unwrap();
        let client = crate::client::Client::connect(&alias).await.unwrap();
        let response = timeout(Duration::from_secs(2), client.call(Request::Ping)).await;
        server.abort();
        assert!(matches!(response, Ok(Ok(Response::Pong))), "{response:?}");
    }

    #[tokio::test]
    async fn incomplete_request_cannot_hold_a_connection_forever() {
        use std::time::Duration;
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_one(server, Arc::new(PingHandler)));
        client.write_all(b"{").await.unwrap();
        tokio::task::yield_now().await;
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(31)).await;
        tokio::time::resume();
        let result = tokio::time::timeout(Duration::from_secs(2), task).await;
        assert!(result
            .expect("incomplete requests must expire")
            .unwrap()
            .is_err());
    }

    #[tokio::test]
    async fn query_deadline_does_not_cancel_a_pending_mutation() {
        use std::time::Duration;
        struct WaitingHandler {
            entered: tokio::sync::Notify,
            release: tokio::sync::Notify,
        }
        impl Handler for WaitingHandler {
            fn handle<'a>(
                &'a self,
                req: Request,
            ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
                Box::pin(async move {
                    if matches!(req, Request::Ping) {
                        return Response::Pong;
                    }
                    self.entered.notify_one();
                    self.release.notified().await;
                    Response::Ok
                })
            }
        }
        for request in [
            Request::WorkspaceList,
            Request::PaneClose {
                pane: flowmux_core::PaneId::new(),
            },
        ] {
            let query = matches!(request, Request::WorkspaceList);
            let handler = Arc::new(WaitingHandler {
                entered: tokio::sync::Notify::new(),
                release: tokio::sync::Notify::new(),
            });
            let (server, mut client) = UnixStream::pair().unwrap();
            let task = tokio::spawn(serve_one(server, handler.clone()));
            write_envelope(
                &mut client,
                Envelope {
                    id: 9,
                    payload: Payload::Request(request),
                },
            )
            .await;
            handler.entered.notified().await;
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(if query { 11 } else { 31 })).await;
            tokio::time::resume();
            let mut reader = BufReader::new(client);
            if !query {
                assert!(
                    tokio::time::timeout(
                        Duration::from_millis(30),
                        reader.read_line(&mut String::new())
                    )
                    .await
                    .is_err(),
                    "mutations must still wait for confirmation"
                );
                handler.release.notify_one();
            }
            let reply = tokio::time::timeout(Duration::from_secs(2), read_envelope(&mut reader))
                .await
                .expect("queries must expire; approved mutations must finish");
            assert_eq!(reply.id, 9);
            assert!(if query {
                matches!(
                    reply.payload,
                    Payload::Response(Response::Error(RpcError::Io(_)))
                )
            } else {
                matches!(reply.payload, Payload::Response(Response::Ok))
            });
            write_envelope(
                reader.get_mut(),
                Envelope {
                    id: 10,
                    payload: Payload::Request(Request::Ping),
                },
            )
            .await;
            assert!(matches!(
                read_envelope(&mut reader).await.payload,
                Payload::Response(Response::Pong)
            ));
            drop(reader);
            task.await.unwrap().unwrap();
        }
    }

    impl Handler for PingHandler {
        fn handle<'a>(
            &'a self,
            req: Request,
        ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
            Box::pin(async move {
                match req {
                    Request::Ping => Response::Pong,
                    other => Response::Error(RpcError::Unimplemented(format!("{other:?}"))),
                }
            })
        }
    }

    async fn write_envelope(stream: &mut UnixStream, env: Envelope) {
        let mut line = serde_json::to_string(&env).unwrap();
        line.push('\n');
        stream.write_all(line.as_bytes()).await.unwrap();
    }

    async fn read_envelope(reader: &mut BufReader<UnixStream>) -> Envelope {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        serde_json::from_str(line.trim_end()).unwrap()
    }

    #[tokio::test]
    async fn serves_request_envelopes_with_matching_response_ids() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_one(server, Arc::new(PingHandler)));

        write_envelope(
            &mut client,
            Envelope {
                id: 7,
                payload: Payload::Request(Request::Ping),
            },
        )
        .await;
        let mut reader = BufReader::new(client);
        let env = read_envelope(&mut reader).await;

        assert_eq!(env.id, 7);
        assert!(matches!(env.payload, Payload::Response(Response::Pong)));
        drop(reader);
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn drops_connection_on_oversized_line_without_oom() {
        // A peer that streams forever without `\n` must not be able to drive
        // unbounded memory growth: the bounded reader caps the buffer at
        // MAX_LINE_BYTES and the connection closes.
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_one(server, Arc::new(PingHandler)));

        // 2x the cap, no newline, then close the writer.
        let payload = vec![b'A'; MAX_LINE_BYTES + 1];
        client.write_all(&payload).await.unwrap();
        drop(client); // trigger EOF on the server side after the overflow

        // serve_one must return Ok (connection closed cleanly) rather than
        // panicking or running out of memory.
        let result = task.await.unwrap();
        assert!(
            result.is_ok(),
            "expected clean shutdown on oversized line, got {result:?}"
        );
    }

    #[tokio::test]
    async fn read_line_bounded_returns_invalid_data_when_line_exceeds_max() {
        use tokio::io::BufReader as TokioBufReader;
        // Feed bytes from one half of a unix pair while the other half
        // exercises read_line_bounded directly so we can assert the exact
        // error kind without going through serve_one.
        let (a, mut b) = UnixStream::pair().unwrap();
        let writer_task = tokio::spawn(async move {
            let payload = vec![b'X'; 32];
            b.write_all(&payload).await.unwrap();
            // No newline, no close yet — but the read should fail before
            // we hit EOF because the cap is hit first.
            b.shutdown().await.unwrap();
        });

        let mut reader = TokioBufReader::new(a);
        let mut buf = String::new();
        let err = read_line_bounded(&mut reader, &mut buf, 16)
            .await
            .expect_err("expected overflow error");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        writer_task.await.unwrap();
    }

    #[tokio::test]
    async fn read_line_bounded_returns_ok_for_complete_line_within_cap() {
        use tokio::io::BufReader as TokioBufReader;
        let (a, mut b) = UnixStream::pair().unwrap();
        let writer = tokio::spawn(async move {
            b.write_all(b"hello world\n").await.unwrap();
        });

        let mut reader = TokioBufReader::new(a);
        let mut buf = String::new();
        let n = read_line_bounded(&mut reader, &mut buf, 1024)
            .await
            .unwrap();
        assert_eq!(buf, "hello world\n");
        assert_eq!(n, "hello world\n".len());
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn read_line_bounded_returns_zero_on_clean_eof_before_first_byte() {
        use tokio::io::BufReader as TokioBufReader;
        let (a, b) = UnixStream::pair().unwrap();
        drop(b);

        let mut reader = TokioBufReader::new(a);
        let mut buf = String::new();
        let n = read_line_bounded(&mut reader, &mut buf, 1024)
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn read_line_bounded_errors_on_eof_mid_line() {
        // A peer that wrote data but never sent `\n` then closed should be
        // surfaced as UnexpectedEof, not silently treated as success.
        use tokio::io::BufReader as TokioBufReader;
        let (a, mut b) = UnixStream::pair().unwrap();
        let writer = tokio::spawn(async move {
            b.write_all(b"partial").await.unwrap();
            b.shutdown().await.unwrap();
        });

        let mut reader = TokioBufReader::new(a);
        let mut buf = String::new();
        let err = read_line_bounded(&mut reader, &mut buf, 1024)
            .await
            .expect_err("expected UnexpectedEof");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_client_non_request_payloads() {
        let (server, mut client) = UnixStream::pair().unwrap();
        let task = tokio::spawn(serve_one(server, Arc::new(PingHandler)));

        write_envelope(
            &mut client,
            Envelope {
                id: 9,
                payload: Payload::Event(Event::NotificationRaised {
                    workspace: WorkspaceId::new(),
                    body: "body".into(),
                    level: NotificationLevel::Info,
                }),
            },
        )
        .await;
        let mut reader = BufReader::new(client);
        let env = read_envelope(&mut reader).await;

        assert_eq!(env.id, 9);
        assert!(matches!(
            env.payload,
            Payload::Response(Response::Error(RpcError::InvalidArgument(_)))
        ));
        drop(reader);
        task.await.unwrap().unwrap();
    }
}
