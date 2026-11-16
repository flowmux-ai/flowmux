// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit managed launches, separate from ordinary shell/shim execution.

use super::*;
use flowmux_ipc::agent_host::{
    ClientRole, HostClient, HostRequest, HostResponse, LaunchAgent, ProcessState,
};
use std::os::unix::fs::PermissionsExt;
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
    let mut argv = args.argv.clone();
    let first = argv.first().context("agent executable is required")?;
    let program = Path::new(first);
    let resolved = if program.components().count() > 1 || program.is_absolute() {
        std::path::absolute(program)?
    } else {
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|directory| directory.join(program))
            .find(|path| executable(path))
            .with_context(|| format!("agent executable not found: {first}"))?
    };
    anyhow::ensure!(executable(&resolved), "agent executable is not executable");
    let provider = program
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("agent")
        .to_string();
    // Keep a shim's path: canonicalizing it may bypass provider argument setup.
    argv[0] = std::path::absolute(resolved)?
        .to_str()
        .context("agent path must be UTF-8")?
        .into();
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
    let cwd = std::path::absolute(args.cwd.as_deref().unwrap_or(Path::new(".")))?;
    anyhow::ensure!(cwd.is_dir(), "agent working directory does not exist");
    Ok(LaunchAgent {
        request_id: uuid::Uuid::new_v4(),
        label: provider.clone(),
        provider,
        argv,
        cwd,
        env: environment,
        size: Default::default(),
    })
}

fn executable(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
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
        let pane = resolve_pane(args.pane)?;
        let socket = socket
            .or_else(|| std::env::var_os("FLOWMUX_SOCKET").map(PathBuf::from))
            .unwrap_or_else(paths::runtime_socket);
        let gui = Client::connect(&socket).await?;
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
    let request_id = launch.request_id;
    let host = flowmux_cli::agent_host::connect_for_launch(&paths::agent_host_socket()).await?;
    let response = tokio::time::timeout(
        Duration::from_secs(15),
        host.call(HostRequest::Launch { launch }),
    )
    .await
    .context("Agent Host launch request timed out")
    .and_then(|response| response)
    .with_context(|| format!("Agent launch result unknown (request {request_id}); inspect `flowmux agent ls` before retrying"))?;
    let HostResponse::Launched { agent } = response else {
        anyhow::bail!(
            "unexpected Agent Host launch response; inspect `flowmux agent ls` before retrying"
        );
    };
    anyhow::ensure!(
        agent.state == ProcessState::Running,
        "Agent {} failed to start: {}",
        agent.instance,
        agent.error.as_deref().unwrap_or("unknown failure")
    );
    let view = if let Some((gui, pane)) = destination {
        match gui.call(Request::AgentViewOpen { pane, instance: agent.instance }).await {
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
        let HostResponse::List { agents, .. } =
            tokio::time::timeout(Duration::from_secs(2), host.call(HostRequest::List)).await??
        else {
            anyhow::bail!("unexpected Agent Host list response");
        };
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

#[cfg(test)]
mod tests {
    use super::*;

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
