// SPDX-License-Identifier: GPL-3.0-or-later
//! Start only for an explicit launch; observers and reconnects never call this.

use anyhow::bail;
#[cfg(target_os = "linux")]
use anyhow::Context;
use flowmux_ipc::agent_host::{ClientRole, HostClient};
use std::path::Path;
use std::time::Duration;

async fn existing(socket: &Path) -> anyhow::Result<Option<HostClient>> {
    match tokio::time::timeout(
        Duration::from_secs(1),
        HostClient::connect(socket, ClientRole::Relay),
    )
    .await?
    {
        Ok(client) => Ok(Some(client)),
        Err(error)
            if error.chain().any(|cause| {
                cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
                    matches!(
                        io.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    )
                })
            }) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// An existing, incompatible or unresponsive Host is never replaced.
pub async fn connect_for_launch(socket: &Path) -> anyhow::Result<HostClient> {
    if let Some(client) = existing(socket).await? {
        return Ok(client);
    }
    #[cfg(target_os = "linux")]
    {
        anyhow::ensure!(
            std::env::var_os("FLATPAK_ID").is_none() && !Path::new("/.flatpak-info").exists(),
            "Independent Agent Host startup is not available inside Flatpak yet"
        );
        let daemon = flowmux_terminal::find_flowmux_daemon()
            .context("flowmux-daemon is missing from this installation")?;
        let mut command = tokio::process::Command::from(service_command(socket, &daemon)?);
        command.kill_on_drop(true);
        let started = tokio::time::timeout(Duration::from_secs(5), command.output())
            .await
            .context("Agent Host service startup timed out; no agent was launched")?
            .context("Cannot start Agent Host through the user service manager")?;
        // A simultaneous launcher may already have submitted this same unit.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(client) = existing(socket).await? {
                return Ok(client);
            }
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "Agent Host service did not become ready ({}): {}",
                    started.status,
                    String::from_utf8_lossy(&started.stderr).trim()
                );
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
    #[cfg(not(target_os = "linux"))]
    bail!("Independent Agent Host startup is not available on this platform yet")
}

#[cfg(target_os = "linux")]
fn service_command(socket: &Path, daemon: &Path) -> anyhow::Result<std::process::Command> {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;

    anyhow::ensure!(socket.is_absolute(), "Agent Host socket must be absolute");
    let socket_bytes = socket.as_os_str().as_bytes();
    anyhow::ensure!(
        socket_bytes.len() < 104,
        "Agent Host socket path is too long"
    );
    let mut unit = "flowmux-agent-host-".to_string();
    for byte in socket_bytes {
        write!(unit, "{byte:02x}")?;
    }
    unit.push_str(".service");
    let daemon = daemon.canonicalize()?;
    let mut command = std::process::Command::new("systemd-run");
    command.args([
        "--user",
        "--quiet",
        "--collect",
        "--no-ask-password",
        "--service-type=exec",
        "--expand-environment=no",
        "--property=Restart=no",
        "--property=UMask=0077",
        "--property=StandardInput=null",
        "--working-directory=/",
    ]);
    command.arg(format!("--unit={unit}"));
    // Provider credentials travel only in LaunchAgent, never in unit properties.
    for name in [
        "HOME",
        "XDG_RUNTIME_DIR",
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "XDG_DATA_HOME",
        "FLOWMUX_RUNTIME_DIR",
        "FLOWMUX_LOG",
    ] {
        if let Some(value) = std::env::var_os(name) {
            let mut arg = std::ffi::OsString::from(format!("--setenv={name}="));
            arg.push(value);
            command.arg(arg);
        }
    }
    command
        .arg("--")
        .arg(daemon)
        .arg("--agent-host")
        .arg("--socket")
        .arg(socket);
    command.stdin(std::process::Stdio::null());
    Ok(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn service_is_separate_literal_and_does_not_restart_an_empty_host() {
        let command = service_command(
            Path::new("/tmp/한글 $HOME/host.sock"),
            Path::new("/bin/true"),
        )
        .unwrap();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy())
            .collect();
        assert!(args.iter().any(|arg| arg == "--user"));
        assert!(args.iter().any(|arg| arg == "--collect"));
        assert!(args.iter().any(|arg| arg == "--service-type=exec"));
        assert!(args.iter().any(|arg| arg == "--expand-environment=no"));
        assert!(args.iter().any(|arg| arg == "--property=Restart=no"));
        assert!(!args.iter().any(|arg| arg == "--scope"));
        assert_eq!(args.last().unwrap(), "/tmp/한글 $HOME/host.sock");
        assert!(args
            .iter()
            .filter(|arg| arg.starts_with("--setenv="))
            .all(|arg| {
                !arg.contains("FLOWMUX_PANE_ID=")
                    && !arg.contains("TOKEN=")
                    && !arg.contains("API_KEY=")
            }));
        let again = service_command(
            Path::new("/tmp/한글 $HOME/host.sock"),
            Path::new("/bin/true"),
        )
        .unwrap();
        assert_eq!(
            command
                .get_args()
                .find(|arg| arg.to_string_lossy().starts_with("--unit=")),
            again
                .get_args()
                .find(|arg| arg.to_string_lossy().starts_with("--unit="))
        );
        assert!(service_command(Path::new("relative.sock"), Path::new("/bin/true")).is_err());
    }

    #[tokio::test]
    async fn incompatible_host_is_reported_without_starting_another() {
        use flowmux_ipc::agent_host::HostResponse;
        use flowmux_ipc::protocol::{Envelope, Payload, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let root = tempfile::tempdir_in("/tmp").unwrap();
        let socket = root.path().join("host.sock");
        let server = tokio::net::UnixListener::bind(&socket).unwrap();
        let handle = tokio::spawn(async move {
            let (stream, _) = server.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read).read_line(&mut line).await.unwrap();
            let request: Envelope = serde_json::from_str(&line).unwrap();
            let reply = Envelope {
                id: request.id,
                payload: Payload::Response(Response::AgentHost {
                    response: HostResponse::Hello {
                        version: u32::MAX,
                        epoch: uuid::Uuid::new_v4(),
                        connection: 1,
                    },
                }),
            };
            write
                .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
                .await
                .unwrap();
        });
        assert!(connect_for_launch(&socket)
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("handshake"));
        handle.await.unwrap();
        assert!(socket.exists());
    }
}
