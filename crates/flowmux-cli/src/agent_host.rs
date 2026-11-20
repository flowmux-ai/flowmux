// SPDX-License-Identifier: GPL-3.0-or-later
//! Start only for an explicit launch; observers and reconnects never call this.

use anyhow::{bail, Context};
use flowmux_ipc::agent_host::{
    AgentInstance, ClientRole, HostClient, HostRequest, HostResponse, LaunchAgent, ProcessState,
    TerminateMode,
};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

#[cfg(any(target_os = "macos", test))]
mod launchd;

const HOST_ENVIRONMENT_KEYS: &[&str] = &[
    "HOME",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "XDG_DATA_HOME",
    "FLOWMUX_RUNTIME_DIR",
    "FLOWMUX_LOG",
];

/// Resolve the executable against the caller's launch environment, preserving shims.
/// Preparation never starts a Host or a process.
pub fn prepare_launch(
    mut argv: Vec<String>,
    cwd: &Path,
    environment: Vec<(String, String)>,
) -> anyhow::Result<LaunchAgent> {
    let cwd = std::path::absolute(cwd)?;
    anyhow::ensure!(cwd.is_dir(), "agent working directory does not exist");
    let first = argv
        .first()
        .filter(|arg| !arg.is_empty())
        .context("agent executable is required")?;
    let program = Path::new(first);
    let executable = |path: &Path| {
        path.metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    };
    let resolved = if program.components().count() > 1 || program.is_absolute() {
        cwd.join(program)
    } else {
        let path = environment
            .iter()
            .rev()
            .find(|(key, _)| key == "PATH")
            .map(|(_, value)| value)
            .context("launch environment has no PATH")?;
        std::env::split_paths(path)
            .map(|directory| cwd.join(directory).join(program))
            .find(|path| executable(path))
            .with_context(|| format!("agent executable not found: {first}"))?
    };
    anyhow::ensure!(executable(&resolved), "agent executable is not executable");
    let provider = match program.file_name().and_then(|name| name.to_str()) {
        Some("agy") => "antigravity",
        Some(name) => name,
        None => "agent",
    }
    .to_string();
    // Canonicalizing would bypass a shim's provider argument setup.
    argv[0] = resolved
        .to_str()
        .context("agent path must be UTF-8")?
        .into();
    anyhow::ensure!(
        argv.iter().all(|arg| !arg.contains('\0')),
        "agent arguments contain NUL"
    );
    anyhow::ensure!(
        environment.iter().all(|(key, value)| {
            !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0')
        }),
        "invalid launch environment"
    );
    Ok(LaunchAgent {
        request_id: uuid::Uuid::now_v7(),
        label: provider.clone(),
        provider,
        argv,
        cwd,
        env: environment,
        size: Default::default(),
        fork_source: None,
        // `agent run` has no view theme; Host defaults apply until a view attaches.
        colors: None,
        shell: false,
    })
}

/// The Host never ran the provider: no Launch was sent, or the Host reported `Failed`.
/// Only this error makes running the same native session elsewhere safe.
#[derive(Debug)]
pub struct LaunchNotStarted(anyhow::Error);

impl std::fmt::Display for LaunchNotStarted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for LaunchNotStarted {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

/// Submit once: a lost response must be inspected, never silently relaunched.
pub async fn launch(socket: &Path, launch: LaunchAgent) -> anyhow::Result<AgentInstance> {
    let request_id = launch.request_id;
    let host = connect_for_launch(socket).await?;
    let response = tokio::time::timeout(
        Duration::from_secs(15),
        host.call(HostRequest::Launch { launch }),
    )
    .await
    .context("Agent Host launch request timed out")
    .and_then(|response| response);
    // The Host answered that it is full: nothing ran, so the result is known.
    if let Err(error) = &response {
        if matches!(
            error.downcast_ref::<flowmux_ipc::agent_host::HostRpcError>(),
            Some(flowmux_ipc::agent_host::HostRpcError(flowmux_ipc::RpcError::Busy(reason)))
                if reason.starts_with(flowmux_ipc::agent_host::LAUNCH_CAPACITY_REJECTION)
        ) {
            return Err(LaunchNotStarted(response.unwrap_err()).into());
        }
    }
    let response = response.with_context(|| format!("Agent launch result unknown (request {request_id}); inspect `flowmux agent ls` before retrying"))?;
    let HostResponse::Launched { agent } = response else {
        bail!("unexpected Agent Host launch response; inspect `flowmux agent ls` before retrying");
    };
    let failure = || {
        anyhow::anyhow!(
            "Agent {} failed to start: {}",
            agent.instance,
            agent.error.as_deref().unwrap_or("unknown failure")
        )
    };
    match agent.state {
        ProcessState::Running => Ok(*agent),
        ProcessState::Failed => Err(LaunchNotStarted(failure()).into()),
        _ => Err(failure()),
    }
}

/// Control an exact existing execution; never starts a Host or retries a mutation.
pub async fn control(
    socket: &Path,
    binding: flowmux_core::AgentViewBinding,
    mode: TerminateMode,
) -> anyhow::Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        let host = HostClient::connect(socket, ClientRole::Relay)
            .await.context("Cannot connect to the existing Agent Host")?;
        let response = host.call(HostRequest::Terminate {
            instance: binding.instance,
            epoch: binding.epoch,
            mode,
        }).await.map_err(|error| {
            if error.downcast_ref::<flowmux_ipc::agent_host::HostRpcError>().is_some() {
                error
            } else {
                error.context("Control result unknown. Check Agents or `flowmux agent ls` before retrying")
            }
        })?;
        anyhow::ensure!(matches!(response, HostResponse::Ok),
            "Control result unknown: unexpected Agent Host response. Check Agents before retrying");
        Ok(())
    }).await.context("Control result unknown: Agent Host timed out. Check Agents or `flowmux agent ls` before retrying")?
}

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
    match start_host(socket).await {
        Ok(client) => Ok(client),
        // Only a socket no Host answers rules out an older Host running the same session.
        Err(error) if matches!(existing(socket).await, Ok(None)) => {
            Err(LaunchNotStarted(error).into())
        }
        Err(error) => Err(error),
    }
}

async fn start_host(socket: &Path) -> anyhow::Result<HostClient> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        anyhow::ensure!(
            std::env::var_os("FLATPAK_ID").is_none() && !Path::new("/.flatpak-info").exists(),
            "Independent Agent Host startup is not available inside Flatpak yet"
        );
        let daemon = flowmux_terminal::find_flowmux_daemon()
            .context("flowmux-daemon is missing from this installation")?;
        #[cfg(target_os = "linux")]
        let started = {
            let mut command = tokio::process::Command::from(service_command(socket, &daemon)?);
            command.kill_on_drop(true);
            let started = tokio::time::timeout(Duration::from_secs(5), command.output())
                .await
                .context("Agent Host service startup timed out; no agent was launched")?
                .context("Cannot start Agent Host through the user service manager")?;
            started
        };
        #[cfg(target_os = "macos")]
        let started = launchd::start(socket, &daemon).await?;
        // Another launcher may have submitted this unit already. Any other refusal is final: do
        // not wait out the deadline.
        let refused = cfg!(target_os = "linux")
            && !started.status.success()
            && !String::from_utf8_lossy(&started.stderr).contains("already");
        let deadline = tokio::time::Instant::now()
            + if refused {
                Duration::ZERO
            } else {
                Duration::from_secs(3)
            };
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
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    bail!("Independent Agent Host startup is not available on this platform yet")
}

#[cfg(target_os = "linux")]
fn service_command(socket: &Path, daemon: &Path) -> anyhow::Result<std::process::Command> {
    use std::os::unix::ffi::OsStrExt;

    anyhow::ensure!(socket.is_absolute(), "Agent Host socket must be absolute");
    anyhow::ensure!(
        socket.as_os_str().as_bytes().len() < 104,
        "Agent Host socket path is too long"
    );
    let unit = flowmux_ipc::agent_host::host_service_unit(socket);
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
    for name in HOST_ENVIRONMENT_KEYS {
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

    #[test]
    fn launch_preparation_uses_supplied_path_cwd_and_literal_environment() {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let shim = bin.join("agy");
        std::os::unix::fs::symlink("/bin/sh", &shim).unwrap();
        let env = vec![
            ("PATH".into(), "bin".into()),
            ("PROVIDER_CONFIG".into(), "literal '$HOME' ; 한글".into()),
        ];
        let argv = vec!["agy".into(), "literal '$HOME' ; 한글".into()];
        let launch = prepare_launch(argv.clone(), root.path(), env.clone()).unwrap();
        assert_eq!(launch.argv, [shim.to_str().unwrap(), &argv[1]]);
        assert_eq!(launch.env, env);
        assert_eq!(launch.cwd, root.path());
        assert_eq!(launch.provider, "antigravity");
        assert_eq!(launch.request_id.get_version_num(), 7);
        assert_eq!(launch.size, Default::default());
        assert_ne!(
            launch.request_id,
            prepare_launch(argv, root.path(), env).unwrap().request_id
        );
        assert_eq!(
            prepare_launch(vec!["bin/agy".into()], root.path(), vec![])
                .unwrap()
                .argv[0],
            shim.to_str().unwrap()
        );
        for (argv, cwd, env) in [
            (vec![], root.path(), vec![]),
            (vec![String::new()], root.path(), vec![]),
            (vec!["agy".into()], root.path(), vec![]),
            (vec!["/bin/sh".into()], shim.as_path(), vec![]),
            (
                vec!["/bin/sh".into(), "bad\0arg".into()],
                root.path(),
                vec![],
            ),
            (
                vec!["/bin/sh".into()],
                root.path(),
                vec![("bad=key".into(), "value".into())],
            ),
            (
                vec!["/bin/sh".into()],
                root.path(),
                vec![("KEY".into(), "bad\0value".into())],
            ),
        ] {
            assert!(prepare_launch(argv, cwd, env).is_err());
        }
    }

    #[tokio::test]
    async fn lost_launch_response_reports_request_without_retransmitting() {
        use flowmux_ipc::protocol::{Envelope, Payload, Request, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let root = tempfile::tempdir_in("/tmp").unwrap();
        let socket = root.path().join("host.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let launch_request = prepare_launch(vec!["/bin/sh".into()], root.path(), vec![]).unwrap();
        let request_id = launch_request.request_id;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let envelope: Envelope =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert!(matches!(
                envelope.payload,
                Payload::Request(Request::AgentHost {
                    request: HostRequest::Hello {
                        role: ClientRole::Relay,
                        ..
                    }
                })
            ));
            let response = Envelope {
                id: envelope.id,
                payload: Payload::Response(Response::AgentHost {
                    response: HostResponse::Hello {
                        version: flowmux_ipc::agent_host::PROTOCOL_VERSION,
                        epoch: uuid::Uuid::new_v4(),
                        connection: 1,
                    },
                }),
            };
            write
                .write_all(format!("{}\n", serde_json::to_string(&response).unwrap()).as_bytes())
                .await
                .unwrap();
            let envelope: Envelope =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert!(
                matches!(envelope.payload, Payload::Request(Request::AgentHost {
                request: HostRequest::Launch { launch }
            }) if launch.request_id == request_id)
            );
            drop(write);
            drop(lines);
            assert!(
                tokio::time::timeout(Duration::from_millis(150), listener.accept())
                    .await
                    .is_err()
            );
        });
        let error = launch(&socket, launch_request)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("result unknown"), "{error}");
        assert!(error.contains(&request_id.to_string()), "{error}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn only_a_capacity_rejection_is_a_known_unstarted_launch() {
        use flowmux_ipc::protocol::{Envelope, Payload, Request, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        for (reason, definite) in [
            (
                format!(
                    "{}: memory budget reserved; launch not started",
                    flowmux_ipc::agent_host::LAUNCH_CAPACITY_REJECTION
                ),
                true,
            ),
            // A fork source that changed must not be forked in an ordinary tab either.
            (
                "Fork source is no longer the same idle session".to_string(),
                false,
            ),
        ] {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let socket = root.path().join("host.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let request = prepare_launch(vec!["/bin/sh".into()], root.path(), vec![]).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                for response in [
                    Response::AgentHost {
                        response: HostResponse::Hello {
                            version: flowmux_ipc::agent_host::PROTOCOL_VERSION,
                            epoch: uuid::Uuid::new_v4(),
                            connection: 1,
                        },
                    },
                    Response::Error(flowmux_ipc::RpcError::Busy(reason)),
                ] {
                    let received: Envelope =
                        serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                    assert!(matches!(
                        received.payload,
                        Payload::Request(Request::AgentHost { .. })
                    ));
                    let envelope = Envelope {
                        id: received.id,
                        payload: Payload::Response(response),
                    };
                    write
                        .write_all(
                            format!("{}\n", serde_json::to_string(&envelope).unwrap()).as_bytes(),
                        )
                        .await
                        .unwrap();
                }
            });
            let error = launch(&socket, request).await.unwrap_err();
            assert_eq!(
                error.downcast_ref::<LaunchNotStarted>().is_some(),
                definite,
                "{error:#}"
            );
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn only_a_launch_the_host_never_ran_is_definitely_not_started() {
        use flowmux_ipc::protocol::{Envelope, Payload, Request, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        // None: an existing incompatible Host. Some(None): Launch sent, response lost.
        for reply in [
            None,
            Some(Some(ProcessState::Failed)),
            Some(Some(ProcessState::Exited)),
            Some(None),
        ] {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let socket = root.path().join("host.sock");
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let launch_request =
                prepare_launch(vec!["/bin/sh".into()], root.path(), vec![]).unwrap();
            let cwd = root.path().to_path_buf();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let mut send = async |id, response| {
                    let envelope = Envelope {
                        id,
                        payload: Payload::Response(Response::AgentHost { response }),
                    };
                    write
                        .write_all(
                            format!("{}\n", serde_json::to_string(&envelope).unwrap()).as_bytes(),
                        )
                        .await
                        .unwrap();
                };
                let hello: Envelope =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                let version = if reply.is_some() {
                    flowmux_ipc::agent_host::PROTOCOL_VERSION
                } else {
                    u32::MAX
                };
                let epoch = uuid::Uuid::new_v4();
                send(
                    hello.id,
                    HostResponse::Hello {
                        version,
                        epoch,
                        connection: 1,
                    },
                )
                .await;
                let Some(state) = reply else { return };
                let request: Envelope =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert!(matches!(
                    request.payload,
                    Payload::Request(Request::AgentHost {
                        request: HostRequest::Launch { .. }
                    })
                ));
                let Some(state) = state else { return };
                let agent = AgentInstance {
                    instance: flowmux_core::AgentInstanceId::new(),
                    epoch,
                    provider: "sh".into(),
                    label: "sh".into(),
                    cwd,
                    pid: None,
                    process_start: None,
                    boot_id: None,
                    state,
                    force_terminate_allowed: false,
                    exit_status: None,
                    view: None,
                    attachment: None,
                    view_owner: None,
                    error: Some("cgroup.kill unavailable".into()),
                    presence: None,
                    shell: false,
                };
                send(
                    request.id,
                    HostResponse::Launched {
                        agent: Box::new(agent),
                    },
                )
                .await;
            });
            let error = launch(&socket, launch_request).await.unwrap_err();
            let definite = error.downcast_ref::<LaunchNotStarted>().is_some();
            assert_eq!(
                definite,
                matches!(reply, Some(Some(ProcessState::Failed))),
                "{reply:?}: {error:#}"
            );
            if reply.is_none() {
                assert_eq!(
                    error
                        .downcast_ref::<flowmux_ipc::agent_host::IncompatibleHost>()
                        .map(|host| host.host_version),
                    Some(Some(u32::MAX)),
                    "{error:#}"
                );
            }
            if reply == Some(Some(ProcessState::Failed)) {
                assert!(format!("{error:#}").contains("cgroup.kill unavailable"));
            }
            server.await.unwrap();
        }
        // No Host answers, and startup fails before any service is submitted.
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let missing = Path::new("relative-missing-host.sock");
        assert!(!missing.exists());
        let request = prepare_launch(vec!["/bin/sh".into()], root.path(), vec![]).unwrap();
        let error = launch(missing, request).await.unwrap_err();
        assert!(
            error.downcast_ref::<LaunchNotStarted>().is_some(),
            "{error:#}"
        );
        // The marker keeps the original message and prints each cause once.
        let wrapped =
            anyhow::Error::new(LaunchNotStarted(anyhow::anyhow!("inner").context("outer")));
        assert_eq!(format!("{wrapped:#}"), "outer: inner");
    }

    #[tokio::test]
    async fn controls_send_exact_binding_once_and_distinguish_denial_from_lost_response() {
        use flowmux_ipc::protocol::{Envelope, Payload, Request, Response};
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        for (mode, response) in [
            (
                TerminateMode::Interrupt,
                Some(Response::AgentHost {
                    response: HostResponse::Ok,
                }),
            ),
            (TerminateMode::Terminate, None),
            (
                TerminateMode::Force,
                Some(Response::Error(flowmux_ipc::RpcError::InvalidArgument(
                    "force not allowed".into(),
                ))),
            ),
        ] {
            let root = tempfile::tempdir_in("/tmp").unwrap();
            let socket = root.path().join("host.sock");
            let binding = flowmux_core::AgentViewBinding {
                instance: flowmux_core::AgentInstanceId::new(),
                epoch: uuid::Uuid::new_v4(),
            };
            assert!(control(&socket, binding, mode).await.is_err());
            assert!(!socket.exists());
            let listener = tokio::net::UnixListener::bind(&socket).unwrap();
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let hello: Envelope =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert!(matches!(
                    hello.payload,
                    Payload::Request(Request::AgentHost {
                        request: HostRequest::Hello {
                            role: ClientRole::Relay,
                            ..
                        }
                    })
                ));
                let hello = Envelope {
                    id: hello.id,
                    payload: Payload::Response(Response::AgentHost {
                        response: HostResponse::Hello {
                            version: flowmux_ipc::agent_host::PROTOCOL_VERSION,
                            epoch: binding.epoch,
                            connection: 1,
                        },
                    }),
                };
                write
                    .write_all(format!("{}\n", serde_json::to_string(&hello).unwrap()).as_bytes())
                    .await
                    .unwrap();
                let request: Envelope =
                    serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
                assert!(
                    matches!(request.payload, Payload::Request(Request::AgentHost {
                    request: HostRequest::Terminate { instance, epoch, mode: received }
                }) if instance == binding.instance && epoch == binding.epoch && received == mode)
                );
                if let Some(response) = response {
                    let reply = Envelope {
                        id: request.id,
                        payload: Payload::Response(response),
                    };
                    write
                        .write_all(
                            format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes(),
                        )
                        .await
                        .unwrap();
                }
                drop(write);
                drop(lines);
                assert!(
                    tokio::time::timeout(Duration::from_millis(150), listener.accept())
                        .await
                        .is_err()
                );
            });
            let result = control(&socket, binding, mode).await;
            match mode {
                TerminateMode::Interrupt => result.unwrap(),
                TerminateMode::Terminate => {
                    assert!(format!("{:#}", result.unwrap_err()).contains("result unknown"))
                }
                TerminateMode::Force => {
                    let error = result.unwrap_err();
                    assert!(error
                        .downcast_ref::<flowmux_ipc::agent_host::HostRpcError>()
                        .is_some());
                    assert!(!format!("{error:#}").contains("result unknown"));
                }
            }
            server.await.unwrap();
        }
    }

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
            // The exact rejection every Host before the versioned one sends.
            let reply = Envelope {
                id: request.id,
                payload: Payload::Response(Response::Error(
                    flowmux_ipc::RpcError::InvalidArgument(
                        "incompatible Agent Host protocol".into(),
                    ),
                )),
            };
            write
                .write_all(format!("{}\n", serde_json::to_string(&reply).unwrap()).as_bytes())
                .await
                .unwrap();
        });
        let error = connect_for_launch(&socket).await.err().unwrap();
        // Never a definite non-start: that would resume the provider in a tab.
        assert!(error.downcast_ref::<LaunchNotStarted>().is_none());
        let host = error
            .downcast_ref::<flowmux_ipc::agent_host::IncompatibleHost>()
            .unwrap_or_else(|| panic!("{error:#}"));
        assert_eq!(host.host_version, None);
        let message = format!("{error:#}");
        for expected in [
            "The running Agent Host uses another protocol",
            "Its agents keep running, but this version cannot open them",
            "stop them all with `flowmux agent stop-host`",
        ] {
            assert!(message.contains(expected), "{message}");
        }
        #[cfg(target_os = "linux")]
        assert!(
            message.contains(&format!(
                "the Host is {}",
                flowmux_ipc::agent_host::host_service_unit(&socket)
            )),
            "{message}"
        );
        handle.await.unwrap();
        assert!(socket.exists());
    }
}
