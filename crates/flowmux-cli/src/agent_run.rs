// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit managed launches, separate from ordinary shell/shim execution.

use super::*;
use flowmux_ipc::agent_host::{
    ClientRole, HostClient, HostRequest, HostResponse, LaunchAgent, ProcessState, TerminateMode,
};
use std::path::Path;
use std::time::Duration;

#[derive(clap::Args)]
pub(crate) struct AgentRunArgs {
    #[arg(long, conflicts_with = "background")]
    pane: Option<PaneId>,
    /// Start without opening a tab; use `agent open` later.
    #[arg(long)]
    background: bool,
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Agent executable and arguments, preserved without shell expansion.
    #[arg(last = true, required = true)]
    argv: Vec<String>,
}

fn launch_request(args: &AgentRunArgs) -> anyhow::Result<LaunchAgent> {
    let environment = std::env::vars_os()
        .map(|(key, value)| {
            Ok((
                key.into_string()
                    .map_err(|_| anyhow::anyhow!("launch environment must be UTF-8"))?,
                value
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("launch environment must be UTF-8"))?,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    flowmux_cli::agent_host::prepare_launch(
        args.argv.clone(),
        args.cwd.as_deref().unwrap_or(Path::new(".")),
        environment,
    )
}

pub(crate) async fn run(
    args: &AgentRunArgs,
    socket: Option<PathBuf>,
    json: bool,
) -> anyhow::Result<()> {
    let launch = launch_request(args)?;
    // Validate the destination before starting anything. A later GUI failure
    // leaves the accepted execution in the Host, never retries its launch.
    let destination = if args.background {
        None
    } else {
        // Inside a managed agent the Host knows the pane and the window that show it now;
        // its environment names neither.
        let (gui, pane) = match managed_context(None, socket.clone()).await? {
            Some(managed) => {
                let pane = args.pane.unwrap_or(managed.report.location.pane);
                (managed.client, pane)
            }
            None => {
                let pane = resolve_pane(args.pane)?;
                let socket = socket
                    .or_else(|| std::env::var_os("FLOWMUX_SOCKET").map(PathBuf::from))
                    .unwrap_or_else(paths::runtime_socket);
                (Client::connect(&socket).await?, pane)
            }
        };
        let Response::Tree { workspaces } = gui.call(Request::WorkspaceTree).await? else {
            anyhow::bail!("could not validate the agent destination");
        };
        let workspace = workspaces
            .iter()
            .find(|workspace| workspace.panes.iter().any(|candidate| candidate.id == pane))
            .context("agent destination pane does not exist")?;
        anyhow::ensure!(
            workspace.location.local_root().is_some(),
            "managed agents require a local workspace"
        );
        Some((gui, pane))
    };
    let agent = flowmux_cli::agent_host::launch(&paths::agent_host_socket(), launch).await?;
    let view = if let Some((gui, pane)) = destination {
        match gui.call(Request::AgentViewOpen { pane, instance: agent.instance, expected_attachment: None }).await {
            Ok(response @ Response::SurfaceCreated { .. }) => Some(response),
            result => anyhow::bail!("Agent {} is running in the background; opening its view failed: {result:?}. Use `flowmux agent open {}` to reconnect", agent.instance, agent.instance),
        }
    } else {
        None
    };
    if json {
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({"agent": agent, "view": view}))?
        );
    } else {
        println!("{}", agent.instance);
    }
    Ok(())
}

pub(crate) async fn list(json: bool) -> anyhow::Result<()> {
    let socket = paths::agent_host_socket();
    // A missing endpoint is an empty list, and must never start a Host.
    let agents = if !socket.try_exists()? {
        Vec::new()
    } else {
        let host = tokio::time::timeout(
            Duration::from_secs(2),
            HostClient::connect(&socket, ClientRole::Observer),
        )
        .await??;
        let HostResponse::List {
            mut agents,
            terminals,
            ..
        } = tokio::time::timeout(Duration::from_secs(2), host.call(HostRequest::List)).await??
        else {
            anyhow::bail!("unexpected Agent Host list response");
        };
        // A terminal is an agent to list only while one runs in its shell.
        agents.extend(
            terminals
                .into_iter()
                .filter(|terminal| terminal.presence.is_some()),
        );
        agents
    };
    if json {
        println!("{}", serde_json::to_string(&agents)?);
    } else {
        for agent in agents {
            println!("{}\t{}\t{:?}", agent.instance, agent.provider, agent.state);
        }
    }
    Ok(())
}

/// Never starts a Host: a missing endpoint means there is nothing to stop.
pub(crate) async fn stop_host(force: bool) -> anyhow::Result<()> {
    let socket = paths::agent_host_socket();
    if !socket.try_exists()? {
        println!("No Agent Host is running.");
        return Ok(());
    }
    tokio::time::timeout(Duration::from_secs(3), HostClient::stop_all(&socket, force))
        .await
        .context("Stop result unknown: Agent Host timed out. Check `flowmux agent ls`")??;
    println!(
        "Stop accepted. Agents and the shells of terminal tabs in the Host end; \
         the Host exits after the last of them."
    );
    Ok(())
}

pub(crate) async fn control(
    instance: flowmux_core::AgentInstanceId,
    mode: TerminateMode,
    json: bool,
) -> anyhow::Result<()> {
    let socket = paths::agent_host_socket();
    let binding = tokio::time::timeout(Duration::from_secs(2), async {
        let host = HostClient::connect(&socket, ClientRole::Observer).await?;
        let HostResponse::List {
            agents, terminals, ..
        } = host.call(HostRequest::List).await?
        else {
            anyhow::bail!("unexpected Agent Host list response");
        };
        let agent = agents
            .into_iter()
            .chain(terminals)
            .find(|agent| agent.instance == instance)
            .context("Execution is not in the current Agent Host")?;
        anyhow::ensure!(
            matches!(agent.state, ProcessState::Running | ProcessState::Stopping),
            "Agent is not running: {:?}",
            agent.state
        );
        Ok::<_, anyhow::Error>(flowmux_core::AgentViewBinding {
            instance,
            epoch: agent.epoch,
        })
    })
    .await
    .context("Agent Host lookup timed out; no control request sent")??;
    flowmux_cli::agent_host::control(&socket, binding, mode).await?;
    if json {
        println!(
            "{}",
            serde_json::json!({"instance": instance, "epoch": binding.epoch, "mode": mode, "accepted": true})
        );
    } else {
        println!(
            "{mode:?} accepted for {instance}. Use `flowmux agent ls` to check the observed state."
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_parser_requires_exact_instance_and_explicit_force() {
        let id = flowmux_core::AgentInstanceId::new();
        let instance = id.to_string();
        for force in [false, true] {
            let mut args = vec!["flowmuxctl", "agent", "terminate", &instance];
            if force {
                args.push("--force");
            }
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.cmd, Cmd::Agent {
                op: AgentOp::Terminate { instance, force: parsed_force }
            } if instance == id && parsed_force == force));
        }
        let cli = Cli::try_parse_from(["flowmuxctl", "agent", "interrupt", &instance]).unwrap();
        assert!(matches!(cli.cmd, Cmd::Agent {
            op: AgentOp::Interrupt { instance }
        } if instance == id));
        for args in [
            vec!["flowmuxctl", "agent", "terminate"],
            vec!["flowmuxctl", "agent", "terminate", "all", "--force"],
            vec!["flowmuxctl", "agent", "interrupt", "not-an-instance"],
            vec!["flowmuxctl", "agent", "interrupt", &instance, "--force"],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }

    #[test]
    fn launch_preserves_literal_arguments_and_validates_before_startup() {
        let root = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "flowmuxctl",
            "agent",
            "run",
            "--background",
            "--cwd",
            root.path().to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "literal '$HOME' ; 한글",
        ])
        .unwrap();
        let Cmd::Agent {
            op: AgentOp::Run(args),
        } = cli.cmd
        else {
            panic!("wrong command")
        };
        let launch = launch_request(&args).unwrap();
        assert_eq!(launch.argv, ["/bin/sh", "-c", "literal '$HOME' ; 한글"]);
        assert_eq!(launch.cwd, root.path());
        assert!(launch.argv[0].starts_with('/'));
        assert_eq!(launch.provider, "sh");
        let agy = root.path().join("agy");
        std::os::unix::fs::symlink("/bin/sh", &agy).unwrap();
        let launch = launch_request(&AgentRunArgs {
            pane: None,
            background: true,
            cwd: Some(root.path().into()),
            argv: vec![agy.to_str().unwrap().into()],
        })
        .unwrap();
        assert_eq!(launch.provider, "antigravity");
        assert!(Cli::try_parse_from([
            "flowmuxctl",
            "agent",
            "run",
            "--background",
            "--pane",
            &PaneId::new().to_string(),
            "--",
            "/bin/sh"
        ])
        .is_err());
        assert!(Cli::try_parse_from(["flowmuxctl", "agent", "run", "--background"]).is_err());
        let bad = AgentRunArgs {
            argv: vec!["/flowmux-no-such-executable".into()],
            ..args
        };
        assert!(launch_request(&bad).is_err());
    }
}
