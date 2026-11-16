// SPDX-License-Identifier: GPL-3.0-or-later
//! A GUI management connection, independent of any terminal view.

use flowmux_ipc::agent_host::{ClientRole, HostClient, HostRequest, HostResponse};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::watch;

const RPC_TIMEOUT: Duration = Duration::from_secs(1);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(2);

async fn connect(socket: &Path, minutes: u32) -> Option<HostClient> {
    let result = tokio::time::timeout(RPC_TIMEOUT, async {
        let client = HostClient::connect(socket, ClientRole::Gui).await?;
        anyhow::ensure!(
            matches!(
                client.call(HostRequest::SetIdleTimeout { minutes }).await?,
                HostResponse::Ok
            ),
            "unexpected Agent Host settings response"
        );
        Ok::<_, anyhow::Error>(client)
    })
    .await;
    match result {
        Ok(Ok(client)) => Some(client),
        _ => None,
    }
}

/// Try the first management handshake before restoring managed tabs.
/// Reconnection only looks for an existing Host; list/settings changes must
/// never restart a normally retired empty Host.
pub async fn start(
    socket: PathBuf,
    mut minutes: watch::Receiver<u32>,
) -> tokio::task::JoinHandle<()> {
    let initial_minutes = *minutes.borrow_and_update();
    // GUI relays require a registered management connection before attaching.
    // A failed handshake leaves saved bindings disconnected, never resumed.
    let mut client = connect(&socket, initial_minutes).await;
    tokio::spawn(async move {
        loop {
            let request = tokio::select! {
                changed = minutes.changed() => {
                    if changed.is_err() { return; }
                    let minutes = *minutes.borrow_and_update();
                    HostRequest::SetIdleTimeout { minutes }
                }
                _ = tokio::time::sleep(RECONNECT_INTERVAL) => HostRequest::KeepAlive,
            };
            if let Some(connected) = client.as_ref() {
                if matches!(
                    tokio::time::timeout(RPC_TIMEOUT, connected.call(request)).await,
                    Ok(Ok(HostResponse::Ok))
                ) {
                    continue;
                }
                // A cancelled RPC may have a partial response; discard this
                // stream rather than reuse or replay an ambiguous request.
                tracing::warn!("Agent Host management connection lost; reconnecting");
                drop(client.take());
            }
            let current_minutes = *minutes.borrow_and_update();
            client = connect(&socket, current_minutes).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_ipc::protocol::{Envelope, Payload, Request, Response};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn management_handshake_precedes_restore_and_settings_keep_same_connection() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("host.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let (events, mut received) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let envelope: Envelope = serde_json::from_str(&line).unwrap();
                let Payload::Request(Request::AgentHost { request }) = envelope.payload else {
                    panic!("unexpected management request");
                };
                let response = if matches!(request, HostRequest::Hello { .. }) {
                    HostResponse::Hello {
                        version: flowmux_ipc::agent_host::PROTOCOL_VERSION,
                        epoch: uuid::Uuid::new_v4(),
                        connection: 1,
                    }
                } else {
                    HostResponse::Ok
                };
                events.send(request).unwrap();
                let reply = Envelope {
                    id: envelope.id,
                    payload: Payload::Response(Response::AgentHost { response }),
                };
                write
                    .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
                    .await
                    .unwrap();
            }
        });
        let (settings, receiver) = watch::channel(30);
        let task = start(socket, receiver).await;
        assert!(matches!(
            received.try_recv().unwrap(),
            HostRequest::Hello {
                role: ClientRole::Gui,
                ..
            }
        ));
        assert!(matches!(
            received.try_recv().unwrap(),
            HostRequest::SetIdleTimeout { minutes: 30 }
        ));
        settings.send_replace(45);
        assert!(matches!(
            tokio::time::timeout(RPC_TIMEOUT, received.recv())
                .await
                .unwrap()
                .unwrap(),
            HostRequest::SetIdleTimeout { minutes: 45 }
        ));
        drop(settings);
        task.await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn absent_host_is_not_started_by_gui_connection_or_settings() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("absent.sock");
        let (settings, receiver) = watch::channel(30);
        let task = start(socket.clone(), receiver).await;
        settings.send_replace(45);
        drop(settings);
        task.await.unwrap();
        assert!(!socket.exists());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
}
