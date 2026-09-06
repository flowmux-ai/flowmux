// SPDX-License-Identifier: GPL-3.0-or-later
use crate::protocol::{Envelope, Payload, Request, Response};
use anyhow::{anyhow, Context};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

pub struct Client {
    inner: Mutex<Inner>,
    next_id: std::sync::atomic::AtomicU64,
}

struct Inner {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Client {
    pub async fn connect(socket: &Path) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket)
            .await
            .with_context(|| format!("connecting to flowmux socket at {}", socket.display()))?;
        let (r, w) = stream.into_split();
        Ok(Self {
            inner: Mutex::new(Inner {
                reader: BufReader::new(r),
                writer: w,
            }),
            next_id: std::sync::atomic::AtomicU64::new(1),
        })
    }

    pub async fn call(&self, req: Request) -> anyhow::Result<Response> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let env = Envelope {
            id,
            payload: Payload::Request(req),
        };
        let mut line = serde_json::to_string(&env)?;
        line.push('\n');

        let mut inner = self.inner.lock().await;
        inner.writer.write_all(line.as_bytes()).await?;
        inner.writer.flush().await?;

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = inner.reader.read_line(&mut buf).await?;
            if n == 0 {
                return Err(anyhow!("daemon closed the connection"));
            }
            let env: Envelope = serde_json::from_str(buf.trim_end())?;
            if env.id != id {
                continue; // out-of-order event; ignore
            }
            match env.payload {
                Payload::Response(r) => return Ok(r),
                Payload::Event(_) | Payload::Request(_) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Event, RpcError};
    use flowmux_core::{NotificationLevel, WorkspaceId};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn call_uses_matching_response_id_and_ignores_events() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("flowmux.sock");
        let listener = UnixListener::bind(&socket).unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut reader = BufReader::new(r);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let req: Envelope = serde_json::from_str(line.trim_end()).unwrap();
            assert_eq!(req.id, 1);
            assert!(matches!(req.payload, Payload::Request(Request::Ping)));

            let envelopes = [
                Envelope {
                    id: 1,
                    payload: Payload::Event(Event::NotificationRaised {
                        workspace: WorkspaceId::new(),
                        body: "ignored".into(),
                        level: NotificationLevel::Info,
                    }),
                },
                Envelope {
                    id: 999,
                    payload: Payload::Response(Response::Error(RpcError::Internal(
                        "wrong id".into(),
                    ))),
                },
                Envelope {
                    id: 1,
                    payload: Payload::Response(Response::Pong),
                },
            ];
            for env in envelopes {
                let mut out = serde_json::to_string(&env).unwrap();
                out.push('\n');
                w.write_all(out.as_bytes()).await.unwrap();
            }
            w.flush().await.unwrap();
        });

        let client = Client::connect(&socket).await.unwrap();
        let response = client.call(Request::Ping).await.unwrap();
        assert!(matches!(response, Response::Pong));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn call_reports_peer_close_and_invalid_json_without_hanging() {
        for reply in ["", "not-json\n", "{\"id\":1"] {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("flowmux.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (r, mut w) = stream.into_split();
                let mut request = String::new();
                BufReader::new(r).read_line(&mut request).await.unwrap();
                w.write_all(reply.as_bytes()).await.unwrap();
                w.shutdown().await.unwrap();
            });
            let client = Client::connect(&socket).await.unwrap();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                client.call(Request::Ping),
            )
            .await
            .expect("a broken peer must not leave the call waiting");
            let error = result.unwrap_err();
            if reply.is_empty() {
                assert!(error.to_string().contains("closed the connection"));
            } else {
                assert!(
                    error.downcast_ref::<serde_json::Error>().is_some(),
                    "{error}"
                );
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn concurrent_calls_on_one_client_keep_responses_paired() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("flowmux.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut reader = BufReader::new(r);
            let mut ids = Vec::new();
            for _ in 0..2 {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let request: Envelope = serde_json::from_str(&line).unwrap();
                ids.push(request.id);
                let response = match request.payload {
                    Payload::Request(Request::Ping) => Response::Pong,
                    Payload::Request(Request::WorkspaceList) => {
                        Response::Error(RpcError::Internal("list response".into()))
                    }
                    other => panic!("unexpected request: {other:?}"),
                };
                let reply = Envelope {
                    id: request.id,
                    payload: Payload::Response(response),
                };
                w.write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
                    .await
                    .unwrap();
            }
            ids.sort();
            assert_eq!(ids, [1, 2]);
        });
        let client = Client::connect(&socket).await.unwrap();
        let (ping, list) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(
                client.call(Request::Ping),
                client.call(Request::WorkspaceList)
            )
        })
        .await
        .unwrap();
        assert!(matches!(ping.unwrap(), Response::Pong));
        assert!(
            matches!(list.unwrap(), Response::Error(RpcError::Internal(message)) if message == "list response")
        );
        server.await.unwrap();
    }
}
