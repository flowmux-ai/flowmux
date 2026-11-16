// SPDX-License-Identifier: GPL-3.0-or-later
//! Local persistent-agent protocol. Terminal leases are scoped to one Host
//! epoch; persisted view bindings never establish process liveness.

use crate::{client::Client, Request, Response, RpcError};
use flowmux_core::{AgentInstanceId, AgentPresence, PaneId, SurfaceId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 2;
pub const MAX_INPUT_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalSize {
    pub rows: u16,
    pub cols: u16,
}

impl TerminalSize {
    pub fn validate(self) -> Result<Self, RpcError> {
        if self.rows == 0
            || self.cols < 2
            || self.rows > 300
            || self.cols > 500
            || usize::from(self.rows) * usize::from(self.cols) > 40_000
        {
            return Err(RpcError::InvalidArgument(
                "terminal size is out of bounds".into(),
            ));
        }
        Ok(self)
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchAgent {
    pub request_id: Uuid,
    pub provider: String,
    pub label: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Memory-only launch environment, never registry metadata or diagnostics.
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub size: TerminalSize,
}

impl std::fmt::Debug for LaunchAgent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LaunchAgent")
            .field("request_id", &self.request_id)
            .field("provider", &self.provider)
            .field("argv", &"[redacted]")
            .field("env", &"[redacted]")
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub struct HostRpcError(pub RpcError);

impl std::fmt::Display for HostRpcError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Agent Host: {:?}", self.0)
    }
}

impl std::error::Error for HostRpcError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    Gui,
    Relay,
    Observer,
    Hook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentKey {
    pub instance: AgentInstanceId,
    pub epoch: Uuid,
    pub lease: Uuid,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentViewLocation {
    pub window: Uuid,
    pub workspace: WorkspaceId,
    pub pane: PaneId,
    pub surface: SurfaceId,
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Starting,
    Running,
    Stopping,
    Exited,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInstance {
    pub instance: AgentInstanceId,
    pub epoch: Uuid,
    pub provider: String,
    pub label: String,
    pub cwd: PathBuf,
    pub pid: Option<u32>,
    pub process_start: Option<u64>,
    pub boot_id: Option<String>,
    pub state: ProcessState,
    pub exit_status: Option<i32>,
    pub view: Option<AgentViewLocation>,
    pub error: Option<String>,
    /// Host-owned hook state; independent of any attached terminal surface.
    #[serde(default)]
    pub presence: Option<AgentPresence>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminateMode {
    Interrupt,
    Terminate,
    Force,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostRequest {
    Hello {
        version: u32,
        role: ClientRole,
    },
    KeepAlive,
    Launch {
        launch: LaunchAgent,
    },
    List,
    Lifecycle {
        instance: AgentInstanceId,
        epoch: Uuid,
        pid: u32,
        process_start: u64,
        session_id: String,
        seq: u64,
        event: crate::protocol::AgentLifecycleEvent,
    },
    PrepareAttach {
        instance: AgentInstanceId,
        epoch: Uuid,
        request_id: Uuid,
        view: Option<AgentViewLocation>,
        /// GUI relays cannot attach before a management handshake cancels retirement.
        #[serde(default)]
        require_gui: bool,
        /// Explicit move from a known lease; never silently steal input.
        replace: Option<AttachmentKey>,
    },
    CommitAttach {
        key: AttachmentKey,
        size: TerminalSize,
    },
    AbortAttach {
        key: AttachmentKey,
    },
    Detach {
        key: AttachmentKey,
    },
    Frame {
        key: AttachmentKey,
        after: u64,
        #[serde(default)]
        offset: usize,
    },
    Input {
        key: AttachmentKey,
        sequence: u64,
        data: Vec<u8>,
    },
    Resize {
        key: AttachmentKey,
        size: TerminalSize,
    },
    Terminate {
        instance: AgentInstanceId,
        mode: TerminateMode,
    },
    SetIdleTimeout {
        minutes: u32,
    },
}

impl HostRequest {
    pub fn is_query(&self) -> bool {
        matches!(self, Self::List | Self::Frame { .. } | Self::KeepAlive)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalFrame {
    pub sequence: u64,
    pub snapshot: bool,
    pub size: TerminalSize,
    pub offset: usize,
    pub next_offset: Option<usize>,
    #[serde(default)]
    pub ended: bool,
    /// Rendered terminal state only. Original queries and OSC side effects
    /// must not be replayed when a new view attaches.
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostResponse {
    Hello {
        version: u32,
        epoch: Uuid,
        connection: u64,
    },
    Ok,
    LifecycleReceipt {
        /// Accepted by the ledger; a display transition is not required.
        applied: bool,
    },
    Launched {
        agent: Box<AgentInstance>,
    },
    List {
        epoch: Uuid,
        revision: u64,
        agents: Vec<AgentInstance>,
    },
    Prepared {
        key: AttachmentKey,
        frame: TerminalFrame,
    },
    Attached {
        key: AttachmentKey,
        frame: TerminalFrame,
    },
    Frame {
        frame: TerminalFrame,
    },
    InputAccepted {
        sequence: u64,
    },
}

/// One persistent control connection. A relay uses a second connection for
/// blocking frame reads so output cannot hold up keystrokes or resize.
pub struct HostClient(pub Client);

impl HostClient {
    pub async fn connect(socket: &Path, role: ClientRole) -> anyhow::Result<Self> {
        let client = Self(Client::connect(socket).await?);
        let hello = client
            .call(HostRequest::Hello {
                version: PROTOCOL_VERSION,
                role,
            })
            .await?;
        anyhow::ensure!(
            matches!(
                hello,
                HostResponse::Hello {
                    version: PROTOCOL_VERSION,
                    ..
                }
            ),
            "unexpected Agent Host handshake response"
        );
        Ok(client)
    }

    pub async fn call(&self, request: HostRequest) -> anyhow::Result<HostResponse> {
        match self.0.call(Request::AgentHost { request }).await? {
            Response::AgentHost { response } => Ok(response),
            Response::Error(error) => Err(HostRpcError(error).into()),
            _ => anyhow::bail!("unexpected agent host response"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn client_rejects_missing_or_incompatible_hello() {
        use crate::protocol::{Envelope, Payload};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        for response in [
            HostResponse::Ok,
            HostResponse::Hello {
                version: PROTOCOL_VERSION + 1,
                epoch: Uuid::new_v4(),
                connection: 1,
            },
        ] {
            let root = tempfile::tempdir().unwrap();
            let socket = root.path().join("host.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                BufReader::new(read).read_line(&mut line).await.unwrap();
                let request: Envelope = serde_json::from_str(&line).unwrap();
                let reply = Envelope {
                    id: request.id,
                    payload: Payload::Response(Response::AgentHost { response }),
                };
                write
                    .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
                    .await
                    .unwrap();
            });
            assert!(HostClient::connect(&socket, ClientRole::Gui).await.is_err());
            server.await.unwrap();
        }
    }

    #[test]
    fn lease_and_frame_roundtrip_keep_epoch_and_generation() {
        let key = AttachmentKey {
            instance: AgentInstanceId::new(),
            epoch: Uuid::new_v4(),
            lease: Uuid::new_v4(),
            generation: 3,
        };
        let request = Request::AgentHost {
            request: HostRequest::Input {
                key,
                sequence: 4,
                data: vec![0, 27, 255],
            },
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let Request::AgentHost {
            request:
                HostRequest::Input {
                    key: decoded,
                    sequence,
                    data,
                },
        } = serde_json::from_str(&encoded).unwrap()
        else {
            panic!("wrong request")
        };
        assert_eq!(key, decoded);
        assert_eq!(sequence, 4);
        assert_eq!(data, vec![0, 27, 255]);
        assert!(
            !request.uses_control_socket(),
            "persistent leases must use the persistent stream"
        );
        assert!(TerminalSize::default().validate().is_ok());
        for size in [
            TerminalSize { rows: 0, cols: 80 },
            TerminalSize {
                rows: 300,
                cols: 500,
            },
        ] {
            assert!(size.validate().is_err());
        }
    }
}
