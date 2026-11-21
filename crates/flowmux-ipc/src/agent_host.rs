// SPDX-License-Identifier: GPL-3.0-or-later
//! Local persistent-agent protocol. Terminal leases are scoped to one Host
//! epoch; persisted view bindings never establish process liveness.

use crate::{client::Client, Request, Response, RpcError};
use flowmux_core::{AgentInstanceId, AgentPresence, PaneId, SurfaceId, WorkspaceId};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u32 = 17;
pub const MAX_INPUT_BYTES: usize = 16 * 1024;
/// A bounded 16 MiB snapshot must reach the terminal before input ownership changes.
pub const ATTACH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Includes completed GUI operations whose responses are still being sent.
pub const MAX_FORWARDED_GUI_RESPONSES: usize = 7;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum NativeSessionEvent {
    Started { status: flowmux_core::AgentStatus },
    Ended,
}

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

/// A view's default colors as RGB triples, used for provider color query replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalColors {
    pub foreground: [u8; 3],
    pub background: [u8; 3],
    /// ANSI colors 0-15; None keeps the Host's defaults.
    #[serde(default)]
    pub palette: Option<[[u8; 3]; 16]>,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self { rows: 24, cols: 80 }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchAgent {
    /// UUIDv7; an evicted result's timestamp cannot be reused to launch again.
    pub request_id: Uuid,
    pub provider: String,
    pub label: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Memory-only launch environment, never registry metadata or diagnostics.
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub size: TerminalSize,
    /// Check the managed fork source at Host admission, before starting a child.
    #[serde(default)]
    pub fork_source: Option<ForkSource>,
    /// Default colors of the intended view, answering startup color queries before attach.
    #[serde(default)]
    pub colors: Option<TerminalColors>,
    /// An ordinary terminal's shell. It lives with its view unless an agent runs inside it.
    #[serde(default)]
    pub shell: bool,
    /// The launcher's file creation mask. The Host's own is private and is not for its children.
    #[serde(default)]
    pub umask: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkSource {
    pub instance: AgentInstanceId,
    pub epoch: Uuid,
    pub session_id: String,
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

/// Prefix of a Host launch rejection caused only by its capacity. Nothing ran,
/// so starting the same session elsewhere is safe.
pub const LAUNCH_CAPACITY_REJECTION: &str = "Agent Host capacity reached";

/// Every Host before the versioned rejection sends exactly this text.
const INCOMPATIBLE_REJECTION: &str = "incompatible Agent Host protocol";

/// A running Host speaks another protocol. Its executions are left untouched:
/// it is never replaced, and a provider resume must not stand in for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncompatibleHost {
    /// `None` when the Host did not name its version (every Host before this field).
    pub host_version: Option<u32>,
    pub socket: PathBuf,
}

impl IncompatibleHost {
    /// The Host's Hello rejection; it names the Host's own version.
    pub fn rejection() -> RpcError {
        RpcError::InvalidArgument(format!(
            "{INCOMPATIBLE_REJECTION}; Host version {PROTOCOL_VERSION}"
        ))
    }

    /// `Some(version)` for a version rejection, with the version when the Host named it.
    fn rejected_version(error: &RpcError) -> Option<Option<u32>> {
        let RpcError::InvalidArgument(message) = error else {
            return None;
        };
        let rest = message.strip_prefix(INCOMPATIBLE_REJECTION)?;
        Some(
            rest.strip_prefix("; Host version ")
                .and_then(|version| version.parse().ok()),
        )
    }
}

impl std::fmt::Display for IncompatibleHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.host_version {
            Some(version) => write!(f, "The running Agent Host uses protocol {version}")?,
            None => write!(f, "The running Agent Host uses another protocol")?,
        }
        write!(
            f,
            " and this Flowmux uses protocol {PROTOCOL_VERSION}. Its agents keep running, \
             but this version cannot open them. Finish them from the Flowmux version that \
             started them, or stop them all with `flowmux agent stop-host`; the Host exits \
             after its last agent ends, and this version then starts a new one."
        )?;
        #[cfg(target_os = "linux")]
        write!(
            f,
            " They are systemd user units: the Host is {} and each agent is a \
             flowmux-agent-….scope; `systemctl --user list-units 'flowmux-agent-*'` lists them, \
             and stopping an agent's scope ends that agent.",
            host_service_unit(&self.socket)
        )?;
        Ok(())
    }
}

impl std::error::Error for IncompatibleHost {}

/// The systemd user service that runs the Host for this socket.
#[cfg(target_os = "linux")]
pub fn host_service_unit(socket: &Path) -> String {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;
    let mut unit = "flowmux-agent-host-".to_string();
    for byte in socket.as_os_str().as_bytes() {
        let _ = write!(unit, "{byte:02x}");
    }
    unit + ".service"
}

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

impl AgentViewLocation {
    /// Visibility changes do not change the source of an admitted command.
    pub fn same_target(&self, other: &Self) -> bool {
        self.window == other.window
            && self.workspace == other.workspace
            && self.pane == other.pane
            && self.surface == other.surface
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAttachment {
    pub key: AttachmentKey,
    pub relay_pid: Option<u32>,
    pub relay_start: Option<u64>,
    /// The owning relay acknowledged writing the initial snapshot to its PTY.
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentViewReport {
    pub key: AttachmentKey,
    pub location: AgentViewLocation,
}

/// Last committed GUI surface, retained while detached to reject stale restores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentViewOwner {
    pub key: AttachmentKey,
    pub surface: SurfaceId,
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
    /// Graceful termination timed out; explicit force is now available.
    #[serde(default)]
    pub force_terminate_allowed: bool,
    pub exit_status: Option<i32>,
    pub view: Option<AgentViewLocation>,
    #[serde(default)]
    pub attachment: Option<AgentAttachment>,
    #[serde(default)]
    pub view_owner: Option<AgentViewOwner>,
    pub error: Option<String>,
    /// Host-owned hook state; independent of any attached terminal surface.
    #[serde(default)]
    pub presence: Option<AgentPresence>,
    /// An ordinary terminal's shell; an agent is running in it only while `presence` is set.
    #[serde(default)]
    pub shell: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminateMode {
    Interrupt,
    Terminate,
    Force,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedNotification {
    pub instance: AgentInstanceId,
    pub epoch: Uuid,
    pub provider: String,
    pub label: String,
    pub notification: flowmux_core::Notification,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationAction {
    Read,
    Dismiss,
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
    /// Hide ended metadata without deleting launch receipts or native history.
    DismissEnded {
        instance: AgentInstanceId,
        epoch: Uuid,
    },
    /// Explicit notification from the managed execution, even without a GUI view.
    Notify {
        instance: AgentInstanceId,
        epoch: Uuid,
        notification: crate::protocol::AgentLifecycleNotification,
    },
    UpdateNotifications {
        epoch: Uuid,
        ids: Vec<flowmux_core::NotificationId>,
        action: NotificationAction,
    },
    ClaimNotification {
        epoch: Uuid,
        id: flowmux_core::NotificationId,
    },
    /// Read the calling managed process's current verified view; never starts or attaches.
    ResolveContext {
        instance: AgentInstanceId,
        epoch: Uuid,
        session_id: Option<String>,
    },
    /// Forward one view operation without racing an attachment transfer.
    DispatchContext {
        context: AgentViewReport,
        session_id: Option<String>,
        request: Box<crate::Request>,
    },
    /// Full live view snapshot for this management connection; never claims input ownership.
    ReportViews {
        views: Vec<AgentViewReport>,
    },
    Lifecycle {
        instance: AgentInstanceId,
        epoch: Uuid,
        pid: u32,
        process_start: u64,
        session_id: String,
        seq: u64,
        event: crate::protocol::AgentLifecycleEvent,
        #[serde(default)]
        notification: Option<crate::protocol::AgentLifecycleNotification>,
    },
    NativeSession {
        instance: AgentInstanceId,
        epoch: Uuid,
        pid: u32,
        process_start: u64,
        session_id: String,
        seq: u64,
        event: NativeSessionEvent,
    },
    PrepareAttach {
        instance: AgentInstanceId,
        epoch: Uuid,
        request_id: Uuid,
        /// Must be None; only the owning GUI can publish verified locations.
        view: Option<AgentViewLocation>,
        /// GUI surface being restored or explicitly opened, never a liveness claim.
        #[serde(default)]
        surface: Option<SurfaceId>,
        /// GUI relays cannot attach before a management handshake cancels retirement.
        #[serde(default)]
        require_gui: bool,
        /// Explicit GUI opens/moves finalize through the destination's GTK lane.
        #[serde(default)]
        gui_ready: bool,
        /// Explicit move from a known lease; never silently steal input.
        replace: Option<AttachmentKey>,
    },
    CommitAttach {
        key: AttachmentKey,
        size: TerminalSize,
    },
    /// A replacement retains the old owner until this snapshot-write ACK.
    ViewReady {
        key: AttachmentKey,
        sequence: u64,
    },
    GuiViewReady {
        key: AttachmentKey,
        surface: SurfaceId,
        sequence: u64,
        relay_pid: u32,
        relay_start: u64,
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
    /// Default colors of the attaching view; never forces a snapshot.
    ViewColors {
        key: AttachmentKey,
        colors: TerminalColors,
    },
    Terminate {
        instance: AgentInstanceId,
        epoch: Uuid,
        mode: TerminateMode,
    },
    /// Stop every execution of this Host. Accepted without a handshake so a
    /// Flowmux of another protocol version can still stop a Host it cannot manage.
    /// Keep this variant's wire form stable across protocol versions.
    StopAll {
        force: bool,
    },
    SetIdleTimeout {
        minutes: u32,
    },
}

impl HostRequest {
    pub fn is_query(&self) -> bool {
        matches!(
            self,
            Self::List | Self::ResolveContext { .. } | Self::Frame { .. } | Self::KeepAlive
        )
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
    /// The root process's wait status, on the frame that ends the execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<i32>,
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
    NotificationClaimed {
        claimed: bool,
    },
    Launched {
        agent: Box<AgentInstance>,
    },
    List {
        epoch: Uuid,
        revision: u64,
        agents: Vec<AgentInstance>,
        notifications: Vec<ManagedNotification>,
        /// The shells of ordinary terminals, apart from the agents so that a count
        /// or the first entry of `agents` still means an agent.
        #[serde(default)]
        terminals: Vec<AgentInstance>,
    },
    Context {
        key: AttachmentKey,
        location: AgentViewLocation,
        socket: PathBuf,
    },
    Dispatched {
        response: Box<crate::Response>,
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
        let incompatible = |host_version| IncompatibleHost {
            host_version,
            socket: socket.to_owned(),
        };
        let hello = client
            .call(HostRequest::Hello {
                version: PROTOCOL_VERSION,
                role,
            })
            .await
            .map_err(|error| {
                match error
                    .downcast_ref::<HostRpcError>()
                    .and_then(|rejected| IncompatibleHost::rejected_version(&rejected.0))
                {
                    Some(version) => incompatible(version).into(),
                    None => error,
                }
            })?;
        match hello {
            HostResponse::Hello {
                version: PROTOCOL_VERSION,
                ..
            } => Ok(client),
            HostResponse::Hello { version, .. } => Err(incompatible(Some(version)).into()),
            _ => anyhow::bail!("unexpected Agent Host handshake response"),
        }
    }

    /// Ask the Host at `socket` to stop all its executions, whatever its protocol version.
    pub async fn stop_all(socket: &Path, force: bool) -> anyhow::Result<()> {
        let client = Self(Client::connect(socket).await?);
        match client.call(HostRequest::StopAll { force }).await? {
            HostResponse::Ok => Ok(()),
            _ => anyhow::bail!("unexpected Agent Host stop response"),
        }
    }

    pub async fn call(&self, request: HostRequest) -> anyhow::Result<HostResponse> {
        match self.0.call_raw(Request::AgentHost { request }).await? {
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

    #[tokio::test]
    async fn version_rejection_is_an_incompatible_host_not_an_unreachable_one() {
        use crate::protocol::{Envelope, Payload};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        assert_eq!(
            IncompatibleHost::rejected_version(&IncompatibleHost::rejection()),
            Some(Some(PROTOCOL_VERSION))
        );
        for (reply, expected) in [
            // Every Host built before the versioned rejection.
            (
                Response::Error(RpcError::InvalidArgument(
                    "incompatible Agent Host protocol".into(),
                )),
                Some(None),
            ),
            (
                Response::Error(RpcError::InvalidArgument(
                    "incompatible Agent Host protocol; Host version 14".into(),
                )),
                Some(Some(14)),
            ),
            (
                Response::Error(RpcError::InvalidArgument(
                    "incompatible Agent Host protocol: other wording".into(),
                )),
                Some(None),
            ),
            (
                Response::AgentHost {
                    response: HostResponse::Hello {
                        version: PROTOCOL_VERSION + 1,
                        epoch: Uuid::new_v4(),
                        connection: 1,
                    },
                },
                Some(Some(PROTOCOL_VERSION + 1)),
            ),
            (
                Response::Error(RpcError::Busy(
                    "Agent Host is stopping; request not started".into(),
                )),
                None,
            ),
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
                    payload: Payload::Response(reply),
                };
                write
                    .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
                    .await
                    .unwrap();
            });
            let error = HostClient::connect(&socket, ClientRole::Gui)
                .await
                .err()
                .unwrap();
            let incompatible = error.downcast_ref::<IncompatibleHost>();
            assert_eq!(
                incompatible.map(|host| host.host_version),
                expected,
                "{error:#}"
            );
            if let Some(host) = incompatible {
                assert_eq!(host.socket, socket);
                let message = host.to_string();
                assert!(message.contains("flowmux agent stop-host"), "{message}");
                assert!(
                    message.contains(&format!("this Flowmux uses protocol {PROTOCOL_VERSION}")),
                    "{message}"
                );
                #[cfg(target_os = "linux")]
                assert!(message.contains(&host_service_unit(&socket)), "{message}");
            }
            server.await.unwrap();
        }
    }

    #[test]
    fn view_report_roundtrip_keeps_current_attachment_and_location() {
        let report = AgentViewReport {
            key: AttachmentKey {
                instance: AgentInstanceId::new(),
                epoch: Uuid::new_v4(),
                lease: Uuid::new_v4(),
                generation: 7,
            },
            location: AgentViewLocation {
                window: Uuid::new_v4(),
                workspace: WorkspaceId::new(),
                pane: PaneId::new(),
                surface: SurfaceId::new(),
                visible: false,
            },
        };
        let request = HostRequest::ReportViews {
            views: vec![report.clone()],
        };
        let encoded = serde_json::to_string(&request).unwrap();
        let HostRequest::ReportViews { views } = serde_json::from_str(&encoded).unwrap() else {
            panic!("wrong request");
        };
        assert_eq!(views, vec![report]);
    }

    #[test]
    fn view_colors_decode_only_bounded_rgb_triples() {
        let key = serde_json::to_value(AttachmentKey {
            instance: AgentInstanceId::new(),
            epoch: Uuid::new_v4(),
            lease: Uuid::new_v4(),
            generation: 1,
        })
        .unwrap();
        let decode = |colors: serde_json::Value| {
            serde_json::from_value::<HostRequest>(
                serde_json::json!({"op": "view_colors", "key": key, "colors": colors}),
            )
        };
        let Ok(HostRequest::ViewColors { colors, .. }) =
            decode(serde_json::json!({"foreground": [0, 0, 0], "background": [255, 255, 255]}))
        else {
            panic!("palette is optional");
        };
        assert_eq!(colors.palette, None);
        let palette = |entries: usize| vec![[1, 2, 3]; entries];
        assert!(decode(
            serde_json::json!({"foreground": [0, 0, 0], "background": [0, 0, 0],
            "palette": palette(16)})
        )
        .is_ok());
        for malformed in [
            serde_json::json!({"foreground": [256, 0, 0], "background": [0, 0, 0]}),
            serde_json::json!({"foreground": [-1, 0, 0], "background": [0, 0, 0]}),
            serde_json::json!({"foreground": [0, 0], "background": [0, 0, 0]}),
            serde_json::json!({"foreground": "#ffffff", "background": [0, 0, 0]}),
            serde_json::json!({"foreground": [0, 0, 0]}),
            serde_json::json!({"foreground": [0, 0, 0], "background": [0, 0, 0], "palette": palette(15)}),
            serde_json::json!({"foreground": [0, 0, 0], "background": [0, 0, 0], "palette": palette(17)}),
        ] {
            assert!(decode(malformed.clone()).is_err());
            let mut launch = serde_json::json!({"op": "launch", "launch": {
                "request_id": Uuid::now_v7(), "provider": "fixture", "label": "fixture",
                "argv": ["/bin/true"], "cwd": "/", "env": []}});
            assert!(matches!(
                serde_json::from_value(launch.clone()),
                Ok(HostRequest::Launch { launch }) if launch.colors.is_none()
            ));
            launch["launch"]["colors"] = malformed;
            assert!(serde_json::from_value::<HostRequest>(launch).is_err());
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
