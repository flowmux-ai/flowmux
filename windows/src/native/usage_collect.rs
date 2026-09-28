// SPDX-License-Identifier: GPL-3.0-or-later
//! Account reads run on the usage worker, never on the native UI thread.
use super::wide;
use crate::usage::{self, FieldRefresh, Provider, ProviderRefresh, UsageError, UsageErrorKind};
use chrono::{Local, Offset, Utc};
use serde_json::Value;
use std::{
    ffi::c_void,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use windows_sys::Win32::Networking::WinHttp::*;

#[path = "usage_collect_codex.rs"]
mod codex;

const TIMEOUT: Duration = Duration::from_secs(10);
const BODY_LIMIT: usize = 1024 * 1024;
type Result<T> = std::result::Result<T, UsageError>;

fn error(kind: UsageErrorKind, message: &str) -> UsageError {
    UsageError::new(kind, message)
}
fn check(cancel: &AtomicBool, deadline: Instant) -> Result<()> {
    if cancel.load(Ordering::Acquire) {
        return Err(error(UsageErrorKind::Io, "Usage collection was cancelled."));
    }
    if Instant::now() >= deadline {
        return Err(error(
            UsageErrorKind::Timeout,
            "Usage collection timed out.",
        ));
    }
    Ok(())
}
fn field<T>(result: Result<T>) -> FieldRefresh<T> {
    match result {
        Ok(value) => FieldRefresh::Success(value),
        Err(error) => FieldRefresh::Failure(error),
    }
}
fn failed(provider: Provider, error: UsageError) -> ProviderRefresh {
    ProviderRefresh {
        provider,
        tokens: FieldRefresh::Failure(error.clone()),
        limits: FieldRefresh::Failure(error),
        collected_at: Utc::now(),
    }
}

pub(super) fn collect(cancel: &AtomicBool) -> [ProviderRefresh; 2] {
    let deadline = Instant::now() + TIMEOUT;
    std::thread::scope(|scope| {
        let codex = scope.spawn(|| codex::collect(cancel, deadline));
        let claude = collect_claude(cancel, deadline);
        let codex = codex.join().unwrap_or_else(|_| {
            failed(
                Provider::Codex,
                error(UsageErrorKind::Io, "The Codex usage collector failed."),
            )
        });
        [claude, codex]
    })
}

fn collect_claude(cancel: &AtomicBool, deadline: Instant) -> ProviderRefresh {
    let root = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".claude"))
        });
    let Some(root) = root else {
        return failed(
            Provider::Claude,
            error(
                UsageErrorKind::NotLoggedIn,
                "The Windows home directory was not found.",
            ),
        );
    };
    let root = match local_path(&root) {
        Ok(root) => root,
        Err(error) => return failed(Provider::Claude, error),
    };
    let now = Local::now();
    let (tokens, limits) = std::thread::scope(|scope| {
        let tokens = scope.spawn(|| {
            usage::claude_tokens(
                &root.join("projects"),
                now.date_naive(),
                now.offset().fix(),
                cancel,
                deadline,
            )
        });
        let limits = claude_limits(&root.join(".credentials.json"), cancel, deadline);
        let tokens = tokens.join().unwrap_or_else(|_| {
            Err(error(
                UsageErrorKind::Io,
                "Could not read the Claude token logs.",
            ))
        });
        (tokens, limits)
    });
    ProviderRefresh {
        provider: Provider::Claude,
        tokens: field(tokens),
        limits: field(limits),
        collected_at: Utc::now(),
    }
}

fn local_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|_| {
                error(
                    UsageErrorKind::Io,
                    "The local usage directory was not found.",
                )
            })?
            .join(path)
    };
    if let Some(std::path::Component::Prefix(prefix)) = path.components().next() {
        if let std::path::Prefix::Disk(drive) | std::path::Prefix::VerbatimDisk(drive) =
            prefix.kind()
        {
            let root = wide(format!("{}:\\", drive as char));
            if unsafe { windows_sys::Win32::Storage::FileSystem::GetDriveTypeW(root.as_ptr()) } != 4
            {
                return Ok(path);
            }
        }
    }
    Err(error(
        UsageErrorKind::Io,
        "Usage configuration must be on a local Windows drive; network paths are unsupported.",
    ))
}

fn claude_limits(
    path: &Path,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Vec<usage::UsageWindow>> {
    check(cancel, deadline)?;
    let file = std::fs::File::open(path).map_err(|e| {
        error(
            if e.kind() == std::io::ErrorKind::NotFound {
                UsageErrorKind::NotLoggedIn
            } else {
                UsageErrorKind::Io
            },
            "The local Claude login could not be read.",
        )
    })?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| {
            error(
                UsageErrorKind::Io,
                "The local Claude login could not be read.",
            )
        })?;
    if bytes.len() > 64 * 1024 {
        return Err(error(
            UsageErrorKind::InvalidData,
            "The local Claude login is too large.",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        error(
            UsageErrorKind::InvalidData,
            "The local Claude login is invalid.",
        )
    })?;
    let token = value
        .get("claudeAiOauth")
        .and_then(|v| v.get("accessToken"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            error(
                UsageErrorKind::NotLoggedIn,
                "The local Claude login was not found.",
            )
        })?;
    // Tokens only enter a fixed-host HTTPS Authorization header, never an error or command line.
    if !token.bytes().all(|c| c.is_ascii_graphic()) {
        return Err(error(
            UsageErrorKind::InvalidData,
            "The local Claude login token is invalid.",
        ));
    }
    let value = http_json(
        "api.anthropic.com",
        443,
        "/api/oauth/usage",
        true,
        token,
        cancel,
        deadline,
    )?;
    usage::claude_limits(&value)
}

struct Http(*mut c_void);
impl Drop for Http {
    fn drop(&mut self) {
        unsafe {
            WinHttpCloseHandle(self.0);
        }
    }
}
struct Request(AtomicUsize);
impl Request {
    fn close(&self) {
        let raw = self.0.swap(0, Ordering::AcqRel);
        if raw != 0 {
            unsafe {
                WinHttpCloseHandle(raw as *mut c_void);
            }
        }
    }
}
impl Drop for Request {
    fn drop(&mut self) {
        self.close();
    }
}
fn handle(raw: *mut c_void) -> Result<Http> {
    if raw.is_null() {
        Err(error(
            UsageErrorKind::Network,
            "Could not open the usage connection.",
        ))
    } else {
        Ok(Http(raw))
    }
}
fn network(ok: i32, cancel: &AtomicBool, deadline: Instant) -> Result<()> {
    check(cancel, deadline)?;
    if ok == 0 {
        Err(error(
            UsageErrorKind::Network,
            "Could not connect to Claude usage. Check the Windows proxy and trusted certificates.",
        ))
    } else {
        Ok(())
    }
}

// Arguments are private so native tests can use an owned loopback endpoint;
// production has no URL/environment override and always calls the fixed HTTPS endpoint above.
fn http_json(
    host: &str,
    port: u16,
    path: &str,
    secure: bool,
    token: &str,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Value> {
    check(cancel, deadline)?;
    unsafe {
        let access = if secure {
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
        } else {
            WINHTTP_ACCESS_TYPE_NO_PROXY
        };
        let session = handle(WinHttpOpen(
            wide("flowmux-usage").as_ptr(),
            access,
            std::ptr::null(),
            std::ptr::null(),
            0,
        ))?;
        network(
            WinHttpSetTimeouts(session.0, 2000, 2000, 2000, 2000),
            cancel,
            deadline,
        )?;
        let connection = handle(WinHttpConnect(session.0, wide(host).as_ptr(), port, 0))?;
        let request = handle(WinHttpOpenRequest(
            connection.0,
            wide("GET").as_ptr(),
            wide(path).as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            if secure { WINHTTP_FLAG_SECURE } else { 0 },
        ))?;
        let disabled =
            WINHTTP_DISABLE_REDIRECTS | WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_AUTHENTICATION;
        network(
            WinHttpSetOption(
                request.0,
                WINHTTP_OPTION_DISABLE_FEATURE,
                (&disabled as *const u32).cast(),
                4,
            ),
            cancel,
            deadline,
        )?;
        let raw = request.0;
        // Closing an in-flight WinHTTP request aborts it, including slow/drip-fed bodies.
        // Exactly one side owns the close; scope joins the watcher before parent handles drop.
        let owner = Request(AtomicUsize::new(raw as usize));
        std::mem::forget(request);
        let (done, receive) = mpsc::channel();
        std::thread::scope(|scope| {
            let owner_ref = &owner;
            std::thread::Builder::new()
                .name("usage-http-cancel".into())
                .spawn_scoped(scope, move || loop {
                    if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
                        owner_ref.close();
                        return;
                    }
                    match receive.recv_timeout(Duration::from_millis(10)) {
                        Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                })
                .map_err(|_| {
                    error(
                        UsageErrorKind::Io,
                        "Could not start the usage cancellation guard.",
                    )
                })?;
            let result = (|| {
                let headers = wide(format!("Authorization: Bearer {token}\r\nanthropic-beta: oauth-2025-04-20\r\nAccept: application/json\r\n"));
                network(
                    WinHttpSendRequest(
                        raw,
                        headers.as_ptr(),
                        (headers.len() - 1) as u32,
                        std::ptr::null(),
                        0,
                        0,
                        0,
                    ),
                    cancel,
                    deadline,
                )?;
                network(
                    WinHttpReceiveResponse(raw, std::ptr::null_mut()),
                    cancel,
                    deadline,
                )?;
                let mut status = 0u32;
                let mut size = 4u32;
                network(
                    WinHttpQueryHeaders(
                        raw,
                        WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                        std::ptr::null(),
                        (&mut status as *mut u32).cast(),
                        &mut size,
                        std::ptr::null_mut(),
                    ),
                    cancel,
                    deadline,
                )?;
                if matches!(status, 401 | 403) {
                    return Err(error(
                        UsageErrorKind::Unauthorized,
                        "Run Claude once to refresh the local login.",
                    ));
                }
                if status != 200 {
                    return Err(error(
                        UsageErrorKind::Network,
                        &format!("Claude usage returned HTTP {status}."),
                    ));
                }
                let mut body = Vec::new();
                let mut chunk = [0u8; 8192];
                loop {
                    check(cancel, deadline)?;
                    let mut read = 0;
                    network(
                        WinHttpReadData(
                            raw,
                            chunk.as_mut_ptr().cast(),
                            chunk.len() as u32,
                            &mut read,
                        ),
                        cancel,
                        deadline,
                    )?;
                    if read == 0 {
                        break;
                    }
                    if body.len() + read as usize > BODY_LIMIT {
                        return Err(error(
                            UsageErrorKind::InvalidData,
                            "Claude usage response exceeded 1 MiB.",
                        ));
                    }
                    body.extend_from_slice(&chunk[..read as usize]);
                }
                serde_json::from_slice(&body).map_err(|_| {
                    error(
                        UsageErrorKind::InvalidData,
                        "The Claude usage response is invalid.",
                    )
                })
            })();
            // The watcher may already have stopped after a normal timeout/cancel.
            let _ = done.send(());
            owner.close();
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, net::TcpListener, sync::Arc};

    struct Server {
        port: u16,
        stop: mpsc::Sender<()>,
        thread: Option<std::thread::JoinHandle<()>>,
    }
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.stop.send(());
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }
    fn server(response: Vec<u8>, hold: bool) -> (Server, mpsc::Receiver<()>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (stop, stopped) = mpsc::channel();
        let (connected, accepted) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut stream = loop {
                if stopped.try_recv().is_ok() || Instant::now() >= deadline {
                    return;
                }
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2))
                    }
                    Err(_) => return,
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut headers = Vec::new();
            let mut chunk = [0u8; 1024];
            while headers.len() < 8192 && !headers.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => return,
                    Ok(n) => headers.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = connected.send(());
            if hold {
                let _ = stopped.recv_timeout(Duration::from_secs(3));
            } else {
                let _ = stream.write_all(&response);
            }
        });
        (
            Server {
                port,
                stop,
                thread: Some(thread),
            },
            accepted,
        )
    }
    fn response(status: &str, headers: &str, body: &[u8]) -> Vec<u8> {
        let mut bytes = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn native_http_fixture_bounds_redirects_body_deadline_and_cancellation() {
        let token = "owned-fixture-token";
        let cancel = AtomicBool::new(false);
        let (fixture, _) = server(
            response("200 OK", "", br#"{"five_hour":{"utilization":37.5}}"#),
            false,
        );
        let value = http_json(
            "127.0.0.1",
            fixture.port,
            "/usage",
            false,
            token,
            &cancel,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap();
        assert_eq!(usage::claude_limits(&value).unwrap()[0].used_percent, 37.5);
        drop(fixture);
        let (fixture, _) = server(
            response(
                "302 Found",
                "Location: http://127.0.0.1:9/forbidden\r\n",
                b"",
            ),
            false,
        );
        let failure = http_json(
            "127.0.0.1",
            fixture.port,
            "/usage",
            false,
            token,
            &cancel,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(failure.message.contains("HTTP 302"));
        assert!(!failure.message.contains(token));
        drop(fixture);
        let (fixture, _) = server(response("200 OK", "", &vec![b'x'; BODY_LIMIT + 1]), false);
        let failure = http_json(
            "127.0.0.1",
            fixture.port,
            "/usage",
            false,
            token,
            &cancel,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(failure.message.contains("1 MiB"));
        drop(fixture);
        let (fixture, _) = server(Vec::new(), true);
        let started = Instant::now();
        let failure = http_json(
            "127.0.0.1",
            fixture.port,
            "/usage",
            false,
            token,
            &cancel,
            started + Duration::from_millis(200),
        )
        .unwrap_err();
        assert_eq!(failure.kind, UsageErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(2));
        drop(fixture);
        let (fixture, accepted) = server(Vec::new(), true);
        let cancel = Arc::new(AtomicBool::new(false));
        let signal = cancel.clone();
        let worker = std::thread::spawn(move || {
            accepted.recv_timeout(Duration::from_secs(2)).unwrap();
            signal.store(true, Ordering::Release);
        });
        let failure = http_json(
            "127.0.0.1",
            fixture.port,
            "/usage",
            false,
            token,
            &cancel,
            Instant::now() + Duration::from_secs(3),
        )
        .unwrap_err();
        worker.join().unwrap();
        assert!(failure.message.contains("cancelled"));
        assert!(!failure.message.contains(token));
    }
}
