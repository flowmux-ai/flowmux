// SPDX-License-Identifier: GPL-3.0-or-later
//! Agent-hook install/doctor ops and hook-event handlers.
//!
//! Split out of `main.rs` (pure move; behavior unchanged).

use super::*;

const RESUME_RETURNED_REASON: &str = "flowmux_resume_returned";
const FLOWMUX_AGENT_NAME_ENV: &str = "FLOWMUX_AGENT_NAME";

pub(crate) fn select_hook_agent_name(
    reported: &str,
    env_name: Option<&str>,
    process_name: Option<&str>,
) -> String {
    env_name
        .and_then(known_agent_name)
        .or_else(|| process_name.and_then(known_agent_name))
        .or_else(|| known_agent_name(reported))
        .unwrap_or(reported)
        .to_ascii_lowercase()
}

fn known_agent_name(name: &str) -> Option<&'static str> {
    let name = name.trim();
    flowmux_procmon::KNOWN_AGENT_COMMS
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(name))
}

fn resolve_hook_agent_name(reported: &str, pid: Option<u32>) -> String {
    let env_name = std::env::var(FLOWMUX_AGENT_NAME_ENV).ok();
    let process_name = pid.and_then(flowmux_procmon::agent_name_for_pid);
    select_hook_agent_name(reported, env_name.as_deref(), process_name)
}

fn hook_agent_display_name(agent: &str) -> &str {
    match agent {
        "codex" => "Codex",
        "claude" => "Claude",
        "opencode" => "OpenCode",
        "cline" => "Cline",
        "gemini" => "Gemini",
        "aider" => "Aider",
        "goose" => "Goose",
        _ => agent,
    }
}

/// Claude reports deliberate session replacement/termination with a specific
/// reason. `other` is non-specific, so retaining that binding preserves
/// recovery across an ambiguous app/terminal teardown. Unknown future reasons
/// also stay resumable rather than risking data loss.
pub(crate) fn claude_session_end_forgets_resume_binding(reason: Option<&str>) -> bool {
    matches!(
        reason,
        Some("clear" | "resume" | "logout" | "prompt_input_exit" | "bypass_permissions_disabled")
    )
}

pub(crate) fn claude_session_end_forget_request(
    agent: &str,
    reason: Option<&str>,
    surface: Option<SurfaceId>,
) -> Option<Request> {
    if !claude_session_end_forgets_resume_binding(reason) {
        return None;
    }
    Some(Request::AgentSessionForget {
        agent: agent.to_ascii_lowercase(),
        surface: surface?,
    })
}

pub(crate) fn generic_resume_return_forget_request(
    agent: &str,
    reason: Option<&str>,
    surface: Option<SurfaceId>,
) -> Option<Request> {
    if reason != Some(RESUME_RETURNED_REASON) {
        return None;
    }
    Some(Request::AgentSessionForget {
        agent: agent.to_ascii_lowercase(),
        surface: surface?,
    })
}

/// Dispatch every `flowmux hooks <op>` invocation. Setup/Doctor/Uninstall
/// only touch user config files and never need the daemon. The runtime
/// hook events (Claude/Codex/OpenCode/Cline) talk to the daemon themselves.
pub(crate) async fn run_hooks_op(op: &HooksOp, socket: Option<PathBuf>) -> anyhow::Result<()> {
    use hook_install::HookInstallStatus;
    match op {
        HooksOp::Setup { agent, flowmux_bin } => {
            let bin = flowmux_bin
                .clone()
                .or_else(resolve_self_bin)
                .unwrap_or_else(|| "flowmux".to_string());
            let targets = parse_hook_targets(agent)?;
            for t in targets {
                match hook_install::install(t, &bin) {
                    Ok(report) => print_hook_report(&report),
                    Err(e) => println!("{:8}  error: {e:#}", t.slug()),
                }
            }
            Ok(())
        }
        HooksOp::Uninstall { agent } => {
            let targets = parse_hook_targets(agent)?;
            for t in targets {
                match hook_install::uninstall(t) {
                    Ok(report) => print_hook_report(&report),
                    Err(e) => println!("{:8}  error: {e:#}", t.slug()),
                }
            }
            Ok(())
        }
        HooksOp::Doctor => {
            run_hooks_doctor(socket.clone()).await;
            // The `let _` pin is intentional: it forces the compiler
            // to keep the `HookInstallStatus` variants reachable so a
            // future refactor cannot silently drop them.
            let _ = HookInstallStatus::Installed;
            Ok(())
        }
        HooksOp::Claude { event } => run_claude_hook_event(event, socket).await,
        HooksOp::Codex { event } => run_generic_agent_hook_event("Codex", event, socket).await,
        HooksOp::Opencode { event } => {
            run_generic_agent_hook_event("OpenCode", event, socket).await
        }
        HooksOp::Cline { event } => run_generic_agent_hook_event("Cline", event, socket).await,
    }
}
/// Full diagnostic dump that one command captures: sandbox state,
/// resolved socket + connect outcome, per-agent install status, hook
/// plugin checksums. The single goal is "run this once on the failing host
/// and paste the output."
pub(crate) async fn run_hooks_doctor(socket: Option<PathBuf>) {
    use hook_install::HookTarget;

    println!("=== flowmux hooks doctor ===");

    // 1. Sandbox + env
    let sandbox = flowmux_config::paths::is_flatpak_sandbox();
    println!(
        "sandbox          : {} (FLATPAK_ID={:?})",
        sandbox,
        std::env::var_os("FLATPAK_ID")
    );
    println!("HOME             : {:?}", std::env::var_os("HOME"));
    println!(
        "XDG_RUNTIME_DIR  : {:?}",
        std::env::var_os("XDG_RUNTIME_DIR")
    );
    println!(
        "XDG_CONFIG_HOME  : {:?}",
        std::env::var_os("XDG_CONFIG_HOME")
    );

    // 2. Socket resolution + reachability
    let env_socket = socket
        .clone()
        .or_else(|| std::env::var_os("FLOWMUX_SOCKET_PATH").map(PathBuf::from))
        .or_else(|| std::env::var_os("FLOWMUX_SOCKET").map(PathBuf::from));
    let resolved = env_socket
        .clone()
        .unwrap_or_else(flowmux_config::paths::runtime_socket);
    println!(
        "socket primary   : {resolved:?} (source={})",
        if env_socket.is_some() {
            "env"
        } else {
            "fallback"
        }
    );
    println!(
        "  exists?        : {} symlink_target?={:?}",
        resolved.exists(),
        std::fs::read_link(&resolved).ok()
    );

    if let Some(cache) = flowmux_config::paths::host_visible_cache_dir() {
        println!("cache dir        : {cache:?} exists={}", cache.exists());
        if let Ok(entries) = std::fs::read_dir(&cache) {
            for e in entries.flatten() {
                let name = e.file_name();
                let name_s = name.to_string_lossy();
                if name_s.starts_with("flowmux-") && name_s.ends_with(".sock") {
                    println!("  per-pid sock   : {:?}", e.path());
                }
            }
        }
    }

    // Live connect probe through the same path the OpenCode plugin
    // would take (envless, fallback resolver, scan included).
    println!("daemon ping      : ...");
    match hooks::connect_daemon(socket).await {
        Some(client) => match client.call(flowmux_ipc::protocol::Request::Ping).await {
            Ok(resp) => println!("  -> ok ({resp:?})"),
            Err(e) => println!("  -> connected but rpc failed: {e}"),
        },
        None => println!("  -> UNREACHABLE"),
    }

    // 3. Per-agent install state
    println!();
    println!("--- agents ---");
    for t in HookTarget::ALL {
        let label = match t {
            HookTarget::Claude => "claude",
            HookTarget::Codex => "codex",
            HookTarget::OpenCode => "opencode",
            HookTarget::Cline => "cline",
        };
        let entry = hook_install::check(*t);
        println!("{label:8}  status={:?}", entry.status);
        for p in &entry.paths {
            let info = if p.exists() {
                let len = std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
                format!("exists len={len}B")
            } else {
                "missing".into()
            };
            println!("           {p:?} ({info})");
        }
    }
}
pub(crate) fn parse_hook_targets(
    agents: &[String],
) -> anyhow::Result<Vec<hook_install::HookTarget>> {
    if agents.is_empty() {
        return Ok(hook_install::HookTarget::ALL.to_vec());
    }
    agents
        .iter()
        .map(|s| {
            hook_install::HookTarget::from_slug(s)
                .ok_or_else(|| anyhow::anyhow!("unknown hook target: {s}"))
        })
        .collect()
}
pub(crate) fn print_hook_report(report: &hook_install::HookInstallReport) {
    let label = report.target.slug();
    match &report.status {
        hook_install::HookInstallStatus::Installed if report.touched_paths.is_empty() => {
            println!("{label:8}  ok");
        }
        hook_install::HookInstallStatus::Installed => {
            for p in &report.touched_paths {
                println!("{label:8}  wrote  {}", p.display());
            }
        }
        hook_install::HookInstallStatus::Skipped => {
            println!("{label:8}  skipped (agent not installed)");
        }
    }
}
/// Best-effort discovery of the running `flowmux` binary path so the
/// command lines we drop into `~/.claude/settings.json` etc. survive
/// when the user has multiple `flowmux` builds on PATH.
pub(crate) fn resolve_self_bin() -> Option<String> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .and_then(|p| p.to_str().map(|s| s.to_string()))
}
pub(crate) async fn run_claude_hook_event(
    event: &ClaudeHookEvent,
    socket: Option<PathBuf>,
) -> anyhow::Result<()> {
    use flowmux_core::AgentActivity::{Idle, NeedsInput, Running};
    use hooks::*;
    let input = read_claude_hook_input();
    let pane = pane_from_env();
    let surface = surface_from_env();
    let pid = match event {
        ClaudeHookEvent::SessionStart => pid_from_env_or_parent(),
        _ => pid_from_env(),
    };
    let agent = resolve_hook_agent_name("claude", pid);
    let agent_display_name = hook_agent_display_name(&agent);
    // Most events carry exactly one request; Stop/Notification carry two
    // (the user-facing toast *and* the activity flip) so the existing
    // "ready" notification keeps firing alongside the new live-status
    // tracking.
    let mut reqs: Vec<_> = Vec::new();
    match event {
        ClaudeHookEvent::Stop => {
            let body = normalized_activity_text(input.last_assistant_message.as_deref());
            let status_text = completed_activity_text(body.as_deref());
            reqs.push(build_activity_update_with_metadata(
                &agent,
                Some(Idle),
                pid,
                pane,
                surface,
                body.as_deref(),
                Some(&status_text),
                input.session_id.as_deref(),
            ));
            reqs.push(build_stop_notify(
                agent_display_name,
                body.as_deref(),
                pane,
                surface,
            ));
        }
        ClaudeHookEvent::StopFailure => {
            let message = normalized_activity_text(input.last_assistant_message.as_deref())
                .or_else(|| {
                    input
                        .error
                        .as_deref()
                        .map(|error| format!("API error: {error}"))
                });
            let status_text = message.as_deref().unwrap_or("API request failed");
            reqs.push(build_activity_update_with_metadata(
                &agent,
                Some(NeedsInput),
                pid,
                pane,
                surface,
                message.as_deref(),
                Some(status_text),
                input.session_id.as_deref(),
            ));
            reqs.push(build_failure_notify(
                agent_display_name,
                message.as_deref(),
                pane,
                surface,
            ));
        }
        ClaudeHookEvent::Notification => {
            if !claude_notification_needs_input(input.notification_type.as_deref()) {
                return Ok(());
            }
            let msg = normalized_activity_text(input.message.as_deref());
            let status_text = msg.as_deref().unwrap_or("Waiting for input");
            reqs.push(build_activity_update_with_metadata(
                &agent,
                Some(NeedsInput),
                pid,
                pane,
                surface,
                msg.as_deref(),
                Some(status_text),
                input.session_id.as_deref(),
            ));
            reqs.push(build_notification_notify(
                agent_display_name,
                msg.as_deref(),
                pane,
                surface,
            ));
        }
        // SessionStart registers the agent's presence (and PID, for the
        // liveness sweep) without claiming it is working yet.
        ClaudeHookEvent::SessionStart => {
            reqs.push(build_activity_update_with_metadata(
                &agent,
                Some(Idle),
                pid,
                pane,
                surface,
                None,
                Some("Ready"),
                input.session_id.as_deref(),
            ));
        }
        // A new prompt or an imminent tool call means the agent is
        // actively working this turn — and clears any "needs input".
        ClaudeHookEvent::PromptSubmit => {
            reqs.push(build_activity_update_with_metadata(
                &agent,
                Some(Running),
                pid,
                pane,
                surface,
                None,
                Some("Starting turn"),
                input.session_id.as_deref(),
            ));
        }
        ClaudeHookEvent::PreToolUse => {
            let status_text = tool_activity_text(input.tool_name.as_deref());
            reqs.push(build_activity_update_with_metadata(
                &agent,
                Some(Running),
                pid,
                pane,
                surface,
                None,
                Some(&status_text),
                input.session_id.as_deref(),
            ));
        }
        // Real teardown (covers Ctrl+C, where Stop never fires). The
        // daemon PID sweep is the backstop for a hard kill that skips
        // SessionEnd too.
        ClaudeHookEvent::SessionEnd => {
            reqs.push(build_activity_update_with_metadata(
                &agent,
                None,
                pid,
                pane,
                surface,
                None,
                Some("Session ended"),
                input.session_id.as_deref(),
            ));
            if let Some(request) =
                claude_session_end_forget_request(&agent, input.reason.as_deref(), surface)
            {
                reqs.push(request);
            }
        }
    };
    if let Some(client) = hooks::connect_daemon(socket).await {
        for req in reqs {
            hooks::send_best_effort(&client, req).await;
        }
    }
    Ok(())
}
pub(crate) async fn run_generic_agent_hook_event(
    reported_agent: &str,
    event: &AgentHookEvent,
    socket: Option<PathBuf>,
) -> anyhow::Result<()> {
    use hooks::*;
    let env_pane = pane_from_env();
    let env_surface = surface_from_env();
    let (cli_pane, cli_surface) = match event {
        AgentHookEvent::Stop { pane, surface, .. } => (*pane, *surface),
        AgentHookEvent::Notification { pane, surface, .. } => (*pane, *surface),
        AgentHookEvent::Running { pane, surface, .. } => (*pane, *surface),
        AgentHookEvent::SessionStart { pane, surface, .. } => (*pane, *surface),
    };
    // CLI flags win over env so the OpenCode Flatpak plugin (which
    // passes them explicitly across the sandbox boundary) is the
    // single source of truth for pane/surface attribution. Non-flatpak
    // callers leave the flags unset and we recover the values from
    // env, preserving the legacy code path.
    let pane = cli_pane.or(env_pane);
    let surface = cli_surface.or(env_surface);
    use flowmux_core::AgentActivity::{Idle, NeedsInput, Running};
    let pid = match event {
        AgentHookEvent::SessionStart { .. } => hooks::pid_from_env_or_parent(),
        _ => hooks::pid_from_env(),
    };
    let agent = resolve_hook_agent_name(reported_agent, pid);
    let agent_display_name = hook_agent_display_name(&agent);
    let mut reqs: Vec<_> = Vec::new();
    match event {
        AgentHookEvent::Stop { args, .. } => {
            let input = read_codex_hook_input(args);
            if let Some(request) =
                generic_resume_return_forget_request(&agent, input.reason.as_deref(), surface)
            {
                reqs.push(request);
            } else {
                let body = normalized_activity_text(input.last_assistant_message.as_deref());
                let status_text = completed_activity_text(body.as_deref());
                reqs.push(build_activity_update_with_metadata(
                    &agent,
                    Some(Idle),
                    pid,
                    pane,
                    surface,
                    body.as_deref(),
                    Some(&status_text),
                    input.session_id.as_deref(),
                ));
                reqs.push(build_stop_notify(
                    agent_display_name,
                    body.as_deref(),
                    pane,
                    surface,
                ));
            }
        }
        AgentHookEvent::Notification { args, .. } => {
            let input = read_codex_hook_input(args);
            let msg = normalized_activity_text(input.message.as_deref());
            let status_text = msg.as_deref().unwrap_or("Waiting for input");
            reqs.push(build_activity_update_with_metadata(
                &agent,
                Some(NeedsInput),
                pid,
                pane,
                surface,
                msg.as_deref(),
                Some(status_text),
                input.session_id.as_deref(),
            ));
            reqs.push(build_notification_notify(
                agent_display_name,
                msg.as_deref(),
                pane,
                surface,
            ));
        }
        AgentHookEvent::Running { args, .. } => {
            let input = read_codex_hook_input(args);
            reqs.push(build_activity_update_with_metadata(
                &agent,
                Some(Running),
                pid,
                pane,
                surface,
                None,
                Some("Working"),
                input.session_id.as_deref(),
            ));
        }
        // Codex / OpenCode register presence on session start without claiming
        // a turn is idle. The wrapper PID, when available, lets the liveness
        // sweep clear sessions that have no SessionEnd hook.
        AgentHookEvent::SessionStart { args, .. } => {
            let input = read_codex_hook_input(args);
            reqs.push(build_unknown_activity_update_with_session(
                &agent,
                pid,
                pane,
                surface,
                input.session_id.as_deref(),
            ));
        }
    };
    if let Some(client) = hooks::connect_daemon(socket).await {
        for req in reqs {
            hooks::send_best_effort(&client, req).await;
        }
    }
    Ok(())
}
