// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows Monaco assets: a tokenized loopback-only server with four workers.
//! Static embedded bytes only; no document, directory, or filesystem API.
use anyhow::Context;
use flowmux_core::SurfaceId;
use std::{
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

const WORKERS: usize = 4;
const MAX_HEADERS: usize = 8 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
const SECURITY_HEADERS: &str = concat!(
    "Content-Security-Policy: default-src 'none'; script-src 'self'; ",
    "style-src 'self' 'unsafe-inline'; worker-src 'self'; ",
    "font-src 'self' data:; img-src data:; connect-src 'none'; ",
    "frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'\r\n",
    "X-Content-Type-Options: nosniff\r\n",
    "Cross-Origin-Resource-Policy: same-origin\r\n",
    "Referrer-Policy: no-referrer\r\n",
    "Cache-Control: no-store\r\n",
);

fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match path {
        "index.html" => (
            include_bytes!("../assets/editor/index.html"),
            "text/html; charset=utf-8",
        ),
        "main.js" => (
            include_bytes!("../assets/editor/main.js"),
            "text/javascript; charset=utf-8",
        ),
        "main.css" => (
            include_bytes!("../assets/editor/main.css"),
            "text/css; charset=utf-8",
        ),
        "editor.worker.js" => (
            include_bytes!("../assets/editor/editor.worker.js"),
            "text/javascript; charset=utf-8",
        ),
        "json.worker.js" => (
            include_bytes!("../assets/editor/json.worker.js"),
            "text/javascript; charset=utf-8",
        ),
        "css.worker.js" => (
            include_bytes!("../assets/editor/css.worker.js"),
            "text/javascript; charset=utf-8",
        ),
        "html.worker.js" => (
            include_bytes!("../assets/editor/html.worker.js"),
            "text/javascript; charset=utf-8",
        ),
        "ts.worker.js" => (
            include_bytes!("../assets/editor/ts.worker.js"),
            "text/javascript; charset=utf-8",
        ),
        "THIRD_PARTY_NOTICES.md" => (
            include_bytes!("../assets/editor/THIRD_PARTY_NOTICES.md"),
            "text/markdown; charset=utf-8",
        ),
        "MONACO_THIRD_PARTY_NOTICES.txt" => (
            include_bytes!("../assets/editor/MONACO_THIRD_PARTY_NOTICES.txt"),
            "text/plain; charset=utf-8",
        ),
        _ => return None,
    })
}

pub struct EditorAssets {
    origin: String,
    token: String,
    stopping: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}

impl EditorAssets {
    pub fn start() -> anyhow::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .context("cannot bind editor asset server")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let token = Uuid::new_v4().simple().to_string();
        let stopping = Arc::new(AtomicBool::new(false));
        let mut server = Self {
            origin: format!("http://{address}"),
            token,
            stopping,
            workers: Vec::with_capacity(WORKERS),
        };
        for index in 0..WORKERS {
            let listener = listener.try_clone()?;
            let stopping = server.stopping.clone();
            let token = server.token.clone();
            let worker = thread::Builder::new()
                .name(format!("flowmux-editor-assets-{index}"))
                .spawn(move || serve(listener, address, &token, &stopping))
                .context("cannot start editor asset worker")?;
            server.workers.push(worker);
        }
        Ok(server)
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    pub fn url(&self, surface: SurfaceId) -> String {
        format!(
            "{}/{}/index.html?surface={}",
            self.origin, self.token, surface.0
        )
    }
}

impl Drop for EditorAssets {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        for worker in &self.workers {
            worker.thread().unpark();
        }
        // Never join socket I/O from the UI thread. Fixed workers own their
        // listeners and stop after bounded I/O; there are no per-request threads.
    }
}

fn serve(listener: TcpListener, address: SocketAddr, token: &str, stopping: &AtomicBool) {
    while !stopping.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, peer)) if peer.ip().is_loopback() => {
                if !stopping.load(Ordering::Acquire) {
                    let _ = connection(stream, address, token, stopping);
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::park_timeout(Duration::from_millis(10));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
}

fn connection(
    mut stream: TcpStream,
    address: SocketAddr,
    token: &str,
    stopping: &AtomicBool,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_millis(200)))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let started = Instant::now();
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0; 1024];
    while bytes.len() < MAX_HEADERS
        && started.elapsed() < IO_TIMEOUT
        && !stopping.load(Ordering::Acquire)
    {
        let count = match stream.read(&mut buffer) {
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                continue
            }
            Err(error) => return Err(error),
        };
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);
        if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
    }
    if stopping.load(Ordering::Acquire) {
        return Ok(());
    }
    let headers = std::str::from_utf8(&bytes).ok();
    let Some(headers) =
        headers.filter(|value| value.len() < MAX_HEADERS && value.contains("\r\n\r\n"))
    else {
        return response(
            &mut stream,
            "400 Bad Request",
            "text/plain",
            b"bad request",
            false,
        );
    };
    let mut lines = headers.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split_whitespace();
    let method = request.next().unwrap_or_default();
    let target = request.next().unwrap_or_default();
    let version = request.next().unwrap_or_default();
    if request.next().is_some() || !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
        return response(
            &mut stream,
            "400 Bad Request",
            "text/plain",
            b"bad request",
            false,
        );
    }
    let hosts: Vec<_> = lines
        .filter_map(|line| line.split_once(':'))
        .filter(|(key, _)| key.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim())
        .collect();
    if hosts.len() != 1 || hosts[0] != address.to_string() {
        return response(
            &mut stream,
            "403 Forbidden",
            "text/plain",
            b"forbidden",
            method == "HEAD",
        );
    }
    if !matches!(method, "GET" | "HEAD") {
        return response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"method not allowed",
            false,
        );
    }
    let prefix = format!("/{token}/");
    let entry = target
        .split('?')
        .next()
        .and_then(|path| path.strip_prefix(&prefix))
        .and_then(asset);
    match entry {
        Some((body, mime)) => response(&mut stream, "200 OK", mime, body, method == "HEAD"),
        None => response(
            &mut stream,
            "404 Not Found",
            "text/plain",
            b"not found",
            method == "HEAD",
        ),
    }
}

fn response(
    stream: &mut TcpStream,
    status: &str,
    mime: &str,
    body: &[u8],
    head: bool,
) -> io::Result<()> {
    let header = format!("HTTP/1.1 {status}\r\n{SECURITY_HEADERS}Content-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    let deadline = Instant::now() + IO_TIMEOUT;
    for mut remaining in [header.as_bytes(), if head { &[] } else { body }] {
        while !remaining.is_empty() {
            let budget = deadline
                .checked_duration_since(Instant::now())
                .filter(|budget| !budget.is_zero())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::TimedOut, "editor asset response deadline")
                })?;
            stream.set_write_timeout(Some(budget))?;
            let count = stream.write(remaining)?;
            if count == 0 {
                return Err(io::ErrorKind::WriteZero.into());
            }
            remaining = &remaining[count..];
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_embedded_editor_assets_are_addressable() {
        assert!(asset("main.js").is_some());
        assert!(asset("ts.worker.js").is_some());
        for path in [
            "../Cargo.toml",
            "/main.js",
            "%2e%2e/main.js",
            "C:/secret",
            "main.js/extra",
        ] {
            assert!(asset(path).is_none());
        }
    }
}
