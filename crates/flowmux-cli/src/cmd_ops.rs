// SPDX-License-Identifier: GPL-3.0-or-later
//! Local command handlers: identify, capabilities, agent, doctor, fix, theme.

use super::*;

pub(crate) async fn run_identify(
    json: bool,
    session: Option<&str>,
    socket: Option<PathBuf>,
) -> anyhow::Result<()> {
    let id = if let Some(session) = session {
        let (client, pane, surface, _) = hooks::resolve_codex_source(session, socket)
            .await?
            .ok_or_else(|| anyhow::anyhow!("No unique live Flowmux pane for Codex session {session}; do not select another window or create a replacement workspace"))?;
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            client.call(Request::WorkspaceTree),
        )
        .await??;
        let Response::Tree { workspaces } = response else {
            anyhow::bail!("Could not verify the session's workspace");
        };
        let workspace = workspaces
            .iter()
            .find(|ws| {
                ws.panes
                    .iter()
                    .any(|p| p.id == pane && p.tabs.iter().any(|tab| tab.id == surface))
            })
            .ok_or_else(|| {
                anyhow::anyhow!("Session pane disappeared; retry from the originating pane")
            })?;
        Identity {
            pane: Some(pane.to_string()),
            surface: Some(surface.to_string()),
            workspace: Some(workspace.id.to_string()),
            socket: Some(client.socket_path().to_string_lossy().into_owned()),
        }
    } else {
        Identity::from_env()
    };
    if json {
        let v = serde_json::json!({
            "pane": id.pane,
            "surface": id.surface,
            "workspace": id.workspace,
            "socket": id.socket,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".to_string());
        println!("pane:      {}", show(&id.pane));
        println!("surface:   {}", show(&id.surface));
        println!("workspace: {}", show(&id.workspace));
        println!("socket:    {}", show(&id.socket));
    }
    Ok(())
}
pub(crate) fn run_capabilities(json: bool) -> anyhow::Result<()> {
    let caps = flowmux_ipc::protocol::capabilities();
    if json {
        println!("{}", serde_json::to_string_pretty(&caps)?);
    } else {
        println!("browser verbs:");
        for v in &caps.browser_verbs {
            println!("  {v}");
        }
        println!("cookie import browsers:");
        for browser in &caps.cookie_import_browsers {
            println!("  {browser}");
        }
        println!("unsupported (CDP-only, return not_supported):");
        for u in &caps.unsupported {
            println!("  {u}");
        }
    }
    Ok(())
}

pub(crate) async fn run_session_name(client: &Client) -> anyhow::Result<()> {
    let workspace =
        workspace_from_env().ok_or_else(|| anyhow::anyhow!("FLOWMUX_WORKSPACE_ID is not set"))?;
    let surface = hooks::surface_from_env()
        .ok_or_else(|| anyhow::anyhow!("FLOWMUX_SURFACE_ID is not set"))?;
    let Response::Tree { workspaces } = client.call(Request::WorkspaceTree).await? else {
        anyhow::bail!("unexpected response to WorkspaceTree");
    };
    let workspace = workspaces
        .iter()
        .find(|candidate| candidate.id == workspace)
        .ok_or_else(|| anyhow::anyhow!("calling workspace is not in the live tree"))?;
    println!("{}", claude_session_name(workspace, surface));
    Ok(())
}

pub(crate) async fn run_agents(client: &Client, json: bool) -> anyhow::Result<()> {
    let Response::Tree { workspaces } = client.call(Request::WorkspaceTree).await? else {
        anyhow::bail!("unexpected response to WorkspaceTree");
    };
    println!("{}", render_agents(&workspaces, json)?);
    Ok(())
}

pub(crate) fn claude_session_name(
    workspace: &flowmux_ipc::protocol::TreeWorkspace,
    surface: SurfaceId,
) -> String {
    let base = match &workspace.location {
        flowmux_core::WorkspaceLocation::Local { root_dir }
        | flowmux_core::WorkspaceLocation::Team { root_dir } => root_dir
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("workspace"),
        flowmux_core::WorkspaceLocation::Ssh { config } => &config.target.host,
    };
    let mut slug = String::new();
    for ch in base.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "workspace" } else { slug };
    format!(
        "{}-{}-{}",
        slug.chars().take(40).collect::<String>(),
        &workspace.id.to_string()[..4],
        &surface.to_string()[..4],
    )
}

pub(crate) fn run_agent_op(op: &AgentOp, json: bool) -> anyhow::Result<()> {
    let home = agent::resolved_home()?;
    let codex_home = agent::resolved_codex_home();
    let overrides = agent::SkillOverrides::from_env();

    let parse_targets = |slugs: &[String]| -> anyhow::Result<Vec<agent::Target>> {
        if slugs.is_empty() {
            Ok(agent::Target::ALL.to_vec())
        } else {
            slugs
                .iter()
                .map(|s| {
                    agent::Target::from_slug(s).ok_or_else(|| anyhow::anyhow!("unknown agent: {s}"))
                })
                .collect()
        }
    };

    match op {
        AgentOp::Install {
            agent: slugs,
            force,
        } => {
            let targets = parse_targets(slugs)?;
            let outcomes =
                agent::install_all(&targets, &home, codex_home.as_deref(), *force, &overrides)?;
            if json {
                let body = outcomes
                    .iter()
                    .map(|(t, p, o)| {
                        serde_json::json!({
                            "agent": t.slug(),
                            "path": p.display().to_string(),
                            "outcome": match o {
                                agent::InstallOutcome::Written => "written",
                                agent::InstallOutcome::Updated { .. } => "written",
                                agent::InstallOutcome::AlreadyUpToDate => "already_up_to_date",
                            },
                            "backup": match o {
                                agent::InstallOutcome::Updated { backup } => Some(backup),
                                _ => None,
                            },
                        })
                    })
                    .collect::<Vec<_>>();
                println!("{}", serde_json::to_string(&body)?);
            } else {
                for (t, p, o) in &outcomes {
                    let label = match o {
                        agent::InstallOutcome::Written => "wrote   ",
                        agent::InstallOutcome::Updated { .. } => "updated ",
                        agent::InstallOutcome::AlreadyUpToDate => "up-to-date",
                    };
                    println!("{label}  {:12}  {}", t.slug(), p.display());
                    if let agent::InstallOutcome::Updated { backup } = o {
                        println!("backup   {:12}  {}", t.slug(), backup.display());
                    }
                }
            }
            Ok(())
        }
        AgentOp::Doctor { agent: slugs } => {
            let targets = parse_targets(slugs)?;
            let report = agent::doctor_all(&targets, &home, codex_home.as_deref(), &overrides);
            let codex_duplicates = if targets.contains(&agent::Target::Codex) {
                agent::codex_unmanaged_skill_paths(&home, codex_home.as_deref())
            } else {
                Vec::new()
            };
            let any_bad = report
                .iter()
                .any(|e| !matches!(e.status, agent::DoctorStatus::Ok));
            if json {
                let body = report
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "agent": e.target.slug(),
                            "path": e.path.display().to_string(),
                            "status": e.status.label(),
                            "detail": match &e.status {
                                agent::DoctorStatus::Error(message) => Some(message),
                                _ => None,
                            },
                            "unmanaged_duplicates": if e.target == agent::Target::Codex {
                                codex_duplicates
                                    .iter()
                                    .map(|p| p.display().to_string())
                                    .collect::<Vec<_>>()
                            } else {
                                Vec::new()
                            },
                        })
                    })
                    .collect::<Vec<_>>();
                println!("{}", serde_json::to_string(&body)?);
            } else {
                for entry in &report {
                    println!(
                        "{:9}  {:12}  {}",
                        entry.status.label(),
                        entry.target.slug(),
                        entry.path.display()
                    );
                    if let agent::DoctorStatus::Error(message) = &entry.status {
                        println!("           {message}");
                    }
                }
                for path in &codex_duplicates {
                    println!(
                        "warn       codex         {} (unmanaged duplicate)",
                        path.display()
                    );
                }
            }
            if any_bad {
                std::process::exit(1);
            }
            Ok(())
        }
        AgentOp::Uninstall {
            agent: slugs,
            skills_only,
        } => {
            let targets = parse_targets(slugs)?;
            let remove_tmux = !skills_only && targets.contains(&agent::Target::ClaudeCode);
            let mut removed = Vec::new();
            for t in targets {
                let path = overrides.path(t, &home, codex_home.as_deref());
                let outcome = agent::uninstall_one(&path)?;
                let label = match outcome {
                    agent::UninstallOutcome::Removed => "removed",
                    agent::UninstallOutcome::Preserved { .. } => "removed",
                    agent::UninstallOutcome::AlreadyAbsent => "absent ",
                };
                let backup = match &outcome {
                    agent::UninstallOutcome::Preserved { backup } => Some(backup),
                    _ => None,
                };
                removed.push(serde_json::json!({
                    "agent": t.slug(), "path": path, "outcome": label.trim(), "backup": backup,
                }));
                if !json {
                    println!("{label}  {:12}  {}", t.slug(), path.display());
                    if let Some(backup) = backup {
                        println!("backup   {:12}  {}", t.slug(), backup.display());
                    }
                }
                if *skills_only {
                    continue;
                }
                let shim = match t {
                    agent::Target::ClaudeCode => "claude",
                    agent::Target::OpenCode => "opencode",
                    agent::Target::Codex => "codex",
                    agent::Target::Antigravity => "agy",
                    agent::Target::Cline => "cline",
                };
                for path in hook_install::uninstall_agent_shim(shim)? {
                    removed.push(serde_json::json!({"agent": t.slug(), "path": path, "outcome": "removed", "kind": "shim"}));
                    if !json {
                        println!("removed  {shim:12}  {}", path.display());
                    }
                }
            }
            if remove_tmux {
                if let Some(path) = hook_install::uninstall_tmux_shim()? {
                    removed.push(serde_json::json!({"agent": "claude-code", "path": path, "outcome": "removed", "kind": "shim"}));
                    if !json {
                        println!("removed  {:12}  {}", "tmux", path.display());
                    }
                }
            }
            if json {
                println!("{}", serde_json::to_string(&removed)?);
            }
            Ok(())
        }
    }
}
/// `flowmux doctor` — render the unified report and exit non-zero
/// if any row needs the user to do something.
pub(crate) async fn run_doctor(socket: Option<PathBuf>, json: bool) -> anyhow::Result<()> {
    let home = agent::resolved_home()?;
    let codex_home = agent::resolved_codex_home();
    let report = doctor::collect(&home, codex_home.as_deref(), socket).await;
    if json {
        println!("{}", doctor::render_json(&report)?);
    } else {
        print!("{}", doctor::render_text(&report));
    }
    if report.has_problems() {
        std::process::exit(1);
    }
    Ok(())
}
/// `flowmux fix` — re-install everything the doctor would flag.
pub(crate) fn run_fix(json: bool) -> anyhow::Result<()> {
    let home = agent::resolved_home()?;
    let codex_home = agent::resolved_codex_home();
    let bin = resolve_self_bin().unwrap_or_else(|| "flowmux".to_string());
    let report = doctor::run_fix(&home, codex_home.as_deref(), &bin);
    if json {
        println!("{}", doctor::render_fix_json(&report)?);
    } else {
        print!("{}", doctor::render_fix_text(&report));
    }
    if report.has_problems() {
        std::process::exit(1);
    }
    Ok(())
}
pub(crate) fn run_theme_op(op: &ThemeOp) -> anyhow::Result<()> {
    match op {
        ThemeOp::Path => {
            match flowmux_config::theme::user_theme_path() {
                Some(p) => {
                    let exists = p.is_file();
                    println!("{}  exists={exists}", p.display());
                }
                None => println!("(XDG config dir unavailable)"),
            }
            Ok(())
        }
        ThemeOp::Import { src } => {
            let dest = flowmux_config::theme::import_from(src)
                .with_context(|| format!("importing {}", src.display()))?;
            println!("imported  {} → {}", src.display(), dest.display());
            println!("Select User theme file in Options > Theme, then use Reload config to apply.");
            Ok(())
        }
    }
}
