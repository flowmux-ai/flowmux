// SPDX-License-Identifier: GPL-3.0-or-later
use crate::protocol::{Envelope, Payload, Request, Response};
use anyhow::{anyhow, Context};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

pub struct Client {
    inner: Mutex<Inner>,
    socket: PathBuf,
    agent_view: Option<(crate::agent_host::AgentViewReport, Option<String>)>,
    agent_notification: Option<flowmux_core::AgentViewBinding>,
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
        let socket = socket.canonicalize().unwrap_or_else(|_| socket.to_owned());
        let credentials = match stream.peer_cred() {
            Ok(credentials) => credentials,
            // A saturated regular endpoint can close before macOS reads its
            // peer credentials. Authenticate the independent control endpoint
            // instead; regular calls still use the rejected stream and fail.
            // No request has been sent, and no mutation is rerouted or replayed.
            Err(error) if error.kind() == std::io::ErrorKind::NotConnected => {
                UnixStream::connect(crate::control_socket_path(&socket))
                    .await?
                    .peer_cred()?
            }
            Err(error) => return Err(error.into()),
        };
        anyhow::ensure!(
            credentials.uid() == unsafe { libc::geteuid() },
            "IPC server belongs to another user"
        );
        let (r, w) = stream.into_split();
        Ok(Self {
            inner: Mutex::new(Inner {
                reader: BufReader::new(r),
                writer: w,
            }),
            socket,
            agent_view: None,
            agent_notification: None,
            next_id: std::sync::atomic::AtomicU64::new(1),
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Reuse the authenticated Host connection; never send a managed effect directly to a GUI.
    pub fn in_agent_view(
        mut self,
        context: crate::agent_host::AgentViewReport,
        session_id: Option<String>,
    ) -> Self {
        self.agent_view = Some((context, session_id));
        self
    }

    /// Notifications belong to the execution and do not require an attached view.
    pub fn in_agent_notifications(mut self, binding: flowmux_core::AgentViewBinding) -> Self {
        self.agent_notification = Some(binding);
        self
    }

    pub async fn call(&self, req: Request) -> anyhow::Result<Response> {
        if let Some(binding) = self.agent_notification {
            let Request::Notify {
                title, body, level, ..
            } = req
            else {
                anyhow::bail!("managed notification connection only supports notifications");
            };
            let response = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                // Streams can be silent longer than the server's connection idle timeout.
                let host = crate::agent_host::HostClient::connect(
                    &self.socket,
                    crate::agent_host::ClientRole::Observer,
                )
                .await?;
                host.0
                    .call_raw(Request::AgentHost {
                        request: crate::agent_host::HostRequest::Notify {
                            instance: binding.instance,
                            epoch: binding.epoch,
                            notification: crate::protocol::AgentLifecycleNotification {
                                title,
                                body,
                                level,
                            },
                        },
                    })
                    .await
            })
            .await
            .context("managed notification timed out; outcome unknown; request was not retried")?
            .context(
                "managed notification response lost; outcome unknown; request was not retried",
            )?;
            return match response {
                Response::AgentHost {
                    response: crate::agent_host::HostResponse::Ok,
                } => Ok(Response::Ok),
                Response::Error(error) => Ok(Response::Error(error)),
                other => Err(anyhow!(
                    "unexpected managed notification response: {other:?}"
                )),
            };
        }
        let Some((context, session_id)) = &self.agent_view else {
            return self.call_raw(req).await;
        };
        anyhow::ensure!(
            req.supports_agent_context(),
            "operation does not support managed context; request not started"
        );
        let response = self
            .call_raw(Request::AgentHost {
                request: crate::agent_host::HostRequest::DispatchContext {
                    context: context.clone(),
                    session_id: session_id.clone(),
                    request: Box::new(req),
                },
            })
            .await
            .context("managed command response lost; outcome unknown; request was not retried")?;
        match response {
            Response::AgentHost {
                response: crate::agent_host::HostResponse::Dispatched { response },
            } => Ok(*response),
            Response::Error(error) => Ok(Response::Error(error)),
            other => Err(anyhow!("unexpected managed dispatch response: {other:?}")),
        }
    }

    pub(crate) async fn call_raw(&self, req: Request) -> anyhow::Result<Response> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut inner = self.inner.lock().await;
        if req.uses_control_socket() {
            match UnixStream::connect(crate::control_socket_path(&self.socket)).await {
                Ok(stream) => {
                    anyhow::ensure!(
                        stream.peer_cred()?.uid() == unsafe { libc::geteuid() },
                        "IPC control server belongs to another user"
                    );
                    let (r, w) = stream.into_split();
                    return Inner {
                        reader: BufReader::new(r),
                        writer: w,
                    }
                    .call(id, req)
                    .await;
                }
                // Older daemons and overlong companion paths use regular admission.
                // Fall back only before any request bytes were sent; never
                // replay an ambiguous effect.
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound
                            | std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::InvalidInput
                    ) => {}
                Err(error) => return Err(error.into()),
            }
        }
        inner.call(id, req).await
    }
}

impl Inner {
    async fn call(&mut self, id: u64, req: Request) -> anyhow::Result<Response> {
        let env = Envelope {
            id,
            payload: Payload::Request(req),
        };
        let mut line = serde_json::to_string(&env)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;

        let mut buf = String::new();
        loop {
            buf.clear();
            let n = self.reader.read_line(&mut buf).await?;
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
    async fn managed_dispatch_wraps_once_and_never_replays_a_lost_effect() {
        use crate::agent_host::{AgentViewLocation, AgentViewReport, AttachmentKey, HostRequest};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let context = AgentViewReport {
            key: AttachmentKey {
                instance: flowmux_core::AgentInstanceId::new(),
                epoch: uuid::Uuid::new_v4(),
                lease: uuid::Uuid::new_v4(),
                generation: 1,
            },
            location: AgentViewLocation {
                window: uuid::Uuid::new_v4(),
                workspace: WorkspaceId::new(),
                pane: flowmux_core::PaneId::new(),
                surface: flowmux_core::SurfaceId::new(),
                visible: true,
            },
        };
        let client = Client::connect(&socket)
            .await
            .unwrap()
            .in_agent_view(context.clone(), Some("native".into()));
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await.unwrap();
            let envelope: Envelope = serde_json::from_str(&line).unwrap();
            let Payload::Request(Request::AgentHost {
                request:
                    HostRequest::DispatchContext {
                        context: actual,
                        session_id,
                        request,
                    },
            }) = envelope.payload
            else {
                panic!("missing Host wrapper")
            };
            assert_eq!(actual, context);
            assert_eq!(session_id.as_deref(), Some("native"));
            assert!(matches!(*request, Request::PaneSendKeys { .. }));
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        assert!(client
            .call(Request::AgentHost {
                request: HostRequest::KeepAlive
            })
            .await
            .unwrap_err()
            .to_string()
            .contains("does not support"));
        let error = client
            .call(Request::PaneSendKeys {
                pane: flowmux_core::PaneId::new(),
                keys: "once".into(),
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("outcome unknown"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn managed_notification_needs_no_view_and_never_replays_a_lost_effect() {
        use crate::agent_host::HostRequest;
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let binding = flowmux_core::AgentViewBinding {
            instance: flowmux_core::AgentInstanceId::new(),
            epoch: uuid::Uuid::new_v4(),
        };
        let client = Client::connect(&socket)
            .await
            .unwrap()
            .in_agent_notifications(binding);
        let server = tokio::spawn(async move {
            let (initial, _) = listener.accept().await.unwrap();
            drop(initial);
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut reader = BufReader::new(r);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let envelope: Envelope = serde_json::from_str(&line).unwrap();
            assert!(matches!(
                envelope.payload,
                Payload::Request(Request::AgentHost {
                    request: HostRequest::Hello { .. }
                })
            ));
            let response = Envelope {
                id: envelope.id,
                payload: Payload::Response(Response::AgentHost {
                    response: crate::agent_host::HostResponse::Hello {
                        version: crate::agent_host::PROTOCOL_VERSION,
                        epoch: binding.epoch,
                        connection: 1,
                    },
                }),
            };
            w.write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
                .await
                .unwrap();
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let envelope: Envelope = serde_json::from_str(&line).unwrap();
            let Payload::Request(Request::AgentHost {
                request:
                    HostRequest::Notify {
                        instance,
                        epoch,
                        notification,
                    },
            }) = envelope.payload
            else {
                panic!("missing execution notification wrapper")
            };
            assert_eq!(instance, binding.instance);
            assert_eq!(epoch, binding.epoch);
            assert_eq!(notification.title, "build");
            assert_eq!(notification.body, "done");
            assert_eq!(notification.level, NotificationLevel::Info);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        assert!(client
            .call(Request::Ping)
            .await
            .unwrap_err()
            .to_string()
            .contains("only supports notifications"));
        let error = client
            .call(Request::Notify {
                pane: Some(flowmux_core::PaneId::new()),
                surface: Some(flowmux_core::SurfaceId::new()),
                title: "build".into(),
                body: "done".into(),
                level: NotificationLevel::Info,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("outcome unknown"));
        server.await.unwrap();
    }

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
    async fn disconnected_control_response_is_not_replayed_on_regular_socket() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("no-replay.sock");
        let regular = UnixListener::bind(&socket).unwrap();
        let control = UnixListener::bind(crate::control_socket_path(&socket)).unwrap();
        let client = Client::connect(&socket).await.unwrap();
        let (regular, _) = regular.accept().await.unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = control.accept().await.unwrap();
            let mut line = String::new();
            BufReader::new(stream).read_line(&mut line).await.unwrap();
            let request: Envelope = serde_json::from_str(&line).unwrap();
            assert!(matches!(
                request.payload,
                Payload::Request(Request::Notify { .. })
            ));
            // The effect may have happened; dropping its response cannot authorize retry.
        });
        assert!(client
            .call(Request::Notify {
                pane: None,
                surface: None,
                title: "hook".into(),
                body: "done".into(),
                level: NotificationLevel::Info,
            })
            .await
            .is_err());
        server.await.unwrap();
        assert!(tokio::time::timeout(
            std::time::Duration::from_millis(50),
            BufReader::new(regular).read_line(&mut String::new())
        )
        .await
        .is_err());
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
