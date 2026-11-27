// SPDX-License-Identifier: GPL-3.0-or-later

pub(super) use flowmux_daemon::chat_runtime::*;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

pub(super) async fn available() -> Vec<Provider> {
    let mut probes = tokio::task::JoinSet::new();
    for provider in Provider::ALL {
        if let Some(program) = executable(provider) {
            probes.spawn(async move { (provider, supports_chat(provider, program).await) });
        }
    }
    let mut available = Vec::new();
    while let Some(result) = probes.join_next().await {
        if let Ok((provider, true)) = result {
            available.push(provider);
        }
    }
    Provider::ALL
        .into_iter()
        .filter(|provider| available.contains(provider))
        .collect()
}

async fn supports_chat(provider: Provider, executable: PathBuf) -> bool {
    let (args, marker): (&[&str], &str) = match provider {
        Provider::Codex => (&["app-server", "--help"], "app-server"),
        Provider::Claude => (&["--help"], "stream-json"),
        Provider::OpenCode => (&["acp", "--help"], "opencode acp"),
        Provider::Gemini | Provider::Cline => (&["--help"], "--acp"),
        Provider::Antigravity => (&["help", "acp"], "acp"),
    };
    let mut command = Command::new(executable);
    command
        .args(args)
        .current_dir("/")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if let Some(path) = chat_path() {
        command.env("PATH", path);
    }
    matches!(tokio::time::timeout(Duration::from_secs(5), command.output()).await,
        Ok(Ok(output)) if output.status.success()
            && (String::from_utf8_lossy(&output.stdout).contains(marker)
                || String::from_utf8_lossy(&output.stderr).contains(marker)))
}

fn executable(provider: Provider) -> Option<PathBuf> {
    flowmux_cli::hook_install::resolve_agent_binary_on_path(
        match provider {
            Provider::Codex => "codex",
            Provider::Claude => "claude",
            Provider::OpenCode => "opencode",
            Provider::Gemini => "gemini",
            Provider::Cline => "cline",
            Provider::Antigravity => "agy",
        },
        &chat_path()?,
    )
}

fn chat_path() -> Option<std::ffi::OsString> {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let extra = std::env::var_os("HOME").into_iter().flat_map(|home| {
        let home = PathBuf::from(home);
        [
            home.join(".local/bin"),
            home.join(".cargo/bin"),
            home.join(".opencode/bin"),
        ]
    });
    std::env::join_paths(
        std::env::split_paths(&existing)
            .filter(|path| path.is_absolute())
            .chain(extra)
            .chain([
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
            ]),
    )
    .ok()
}

pub(super) fn start(
    runtime: &tokio::runtime::Handle,
    provider: Provider,
    cwd: PathBuf,
    native_id: Option<String>,
) -> (async_channel::Sender<Input>, async_channel::Receiver<Event>) {
    let program = executable(provider).unwrap_or_default();
    let (input, events, _) = flowmux_daemon::chat_runtime::start(
        runtime,
        provider,
        program,
        cwd,
        environment(),
        native_id,
    );
    (input, events)
}

fn environment() -> Vec<(String, String)> {
    let mut environment: Vec<_> = std::env::vars_os()
        .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
        .filter(|(key, _)| !key.starts_with("FLOWMUX_"))
        .collect();
    if let Some(path) = chat_path().and_then(|path| path.into_string().ok()) {
        environment.retain(|(key, _)| key != "PATH");
        environment.push(("PATH".into(), path));
    }
    environment
}

pub(super) fn hosted(
    runtime: &tokio::runtime::Handle,
    id: flowmux_core::SurfaceId,
    provider: Provider,
    cwd: PathBuf,
    native_id: Option<String>,
    start: bool,
) -> (async_channel::Sender<Input>, async_channel::Receiver<Event>) {
    use flowmux_daemon::agent_host::chat::{Launch, Request, Response};
    use flowmux_ipc::agent_host::{ClientRole, HostClient, HostRequest, HostResponse};
    let (tx, input) = async_channel::bounded(16);
    let (output, rx) = async_channel::bounded(128);
    async fn request(client: &HostClient, request: Request) -> anyhow::Result<Response> {
        let response = tokio::time::timeout(
            Duration::from_secs(3),
            client.call(HostRequest::Chat {
                request: serde_json::to_value(request)?,
            }),
        )
        .await??;
        let HostResponse::Chat { response } = response else {
            anyhow::bail!("Unexpected Agent Host chat response");
        };
        Ok(serde_json::from_value(response)?)
    }
    let run = {
        let output = output.clone();
        async move {
            let socket = flowmux_config::paths::agent_host_socket();
            let client = if start {
                flowmux_cli::agent_host::connect_for_launch(&socket).await?
            } else {
                if !socket.exists() {
                    output.send(Event::Detached).await?;
                    return Ok(());
                }
                HostClient::connect(&socket, ClientRole::Relay).await?
            };
            let launch = start.then(|| Launch {
                provider,
                program: executable(provider).unwrap_or_default(),
                cwd,
                native_id,
                environment: environment(),
            });
            let Response::Attached {
                mut sequence,
                through,
                busy,
                catalog,
                settings,
            } = request(&client, Request::Attach { id, launch }).await?
            else {
                output.send(Event::Detached).await?;
                return Ok(());
            };
            let history = tokio::task::spawn_blocking(move || {
                flowmux_state::chat_history::Store::existing()?
                    .map_or(Ok(Default::default()), |store| {
                        store.load_prefix(id, Some(through))
                    })
            })
            .await??;
            output
                .send(Event::Attached {
                    records: history.records,
                    busy,
                    native_id: history.native_id,
                    settings,
                })
                .await?;
            if let Some(catalog) = catalog {
                output.send(Event::Ready(*catalog)).await?;
            }
            let mut tick = tokio::time::interval(Duration::from_millis(80));
            loop {
                tokio::select! {
                    next = input.recv() => {
                        let Ok(next) = next else { return Ok(()) };
                        let send = matches!(next, Input::Send { .. });
                        let stop = matches!(next, Input::Stop);
                        if let Err(error) = request(&client, Request::Input { id, input: next }).await {
                            if error.downcast_ref::<flowmux_ipc::agent_host::HostRpcError>().is_none() {
                                return Err(error.context("Could not confirm the chat command; check the saved conversation"));
                            }
                            let detail = format!("{error:#}");
                            output.send(if send { Event::Rejected(detail) }
                                else if stop { Event::StopFailed(detail) } else { Event::Notice(detail) }).await?;
                        }
                    }
                    _ = tick.tick() => {
                        let Response::Events { sequence: next, events, ended } =
                            request(&client, Request::Read { id, after: sequence }).await?
                        else { anyhow::bail!("Chat session has ended") };
                        sequence = next;
                        for event in events { output.send(event).await?; }
                        if ended {
                            output.send(Event::Detached).await?;
                            return Ok(())
                        }
                    }
                }
            }
        }
    };
    runtime.spawn(async move {
        let result: anyhow::Result<()> = tokio::select! {
            result = run => result,
            () = output.closed() => Ok(()),
        };
        if let Err(error) = result {
            let _ = output.send(Event::Offline(format!("{error:#}"))).await;
        }
    });
    (tx, rx)
}
