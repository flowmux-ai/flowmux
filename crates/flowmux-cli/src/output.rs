// SPDX-License-Identifier: GPL-3.0-or-later
//! Response rendering: print_response and the tree view.
//!
//! Split out of `main.rs` (pure move; behavior unchanged).

use super::*;

pub(crate) fn print_response(r: &Response, json_mode: bool) -> anyhow::Result<()> {
    // `flowmux tree` gets a human-readable indented view in text mode;
    // --json still emits the structured payload for scripts.
    if !json_mode {
        if let Response::Tree { workspaces } = r {
            print!("{}", render_tree(workspaces));
            return Ok(());
        }
        if let Response::WorkspaceCurrent { id } = r {
            match id {
                Some(id) => println!("{id}"),
                None => println!("(none)"),
            }
            return Ok(());
        }
        if let Response::ScreenContents { text } = r {
            // Raw terminal text — print as-is (already newline-terminated
            // per row), no extra framing.
            print!("{text}");
            return Ok(());
        }
        if let Some(value) = plain_browser_response(r) {
            println!("{value}");
            return Ok(());
        }
        if let Response::Notifications {
            entries,
            unread_count,
        } = r
        {
            println!("{} notification(s), {unread_count} unread", entries.len());
            for entry in entries {
                let state = if entry.read { "read" } else { "unread" };
                let pane = entry
                    .pane
                    .map(|pane| pane.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{} [{}] {:?} pane={} {} - {}",
                    entry.id, state, entry.level, pane, entry.title, entry.body
                );
            }
            return Ok(());
        }
        if let Response::NotificationState { changed } = r {
            println!("{}", if *changed { "ok" } else { "not-found" });
            return Ok(());
        }
    }
    let s = if json_mode {
        // Single-line JSON — easier to parse from agent scripts
        // (`jq -r .browser_pane_opened.pane` etc.).
        serde_json::to_string(r)?
    } else {
        serde_json::to_string_pretty(r)?
    };
    println!("{s}");
    if let Response::Error(error) = r {
        anyhow::bail!("{error:?}");
    }
    Ok(())
}

fn plain_browser_response(response: &Response) -> Option<String> {
    match response {
        Response::BrowserResult { value } => Some(value.clone()),
        Response::BrowserOk => Some("ok".into()),
        Response::BrowserBoolResult { value } => Some(value.to_string()),
        _ => None,
    }
}

/// Render `flowmux tree` as an indented workspace → leaf-pane → tab
/// view. The active tab in each pane is marked with `*`.
pub(crate) fn render_tree(workspaces: &[flowmux_ipc::protocol::TreeWorkspace]) -> String {
    use std::fmt::Write as _;
    if workspaces.is_empty() {
        return "(no workspaces)\n".to_string();
    }
    let mut out = String::new();
    for ws in workspaces {
        let _ = writeln!(
            out,
            "workspace {} \"{}\" ({})",
            ws.id,
            ws.name,
            ws.root.display()
        );
        for pane in &ws.panes {
            let _ = writeln!(out, "  pane {}", pane.id);
            for tab in &pane.tabs {
                let marker = if tab.active { '*' } else { ' ' };
                let agent = tab
                    .agent
                    .as_ref()
                    .map(|agent| format!(" agent={} status={}", agent.name, agent.status.as_str()))
                    .unwrap_or_default();
                let _ = writeln!(
                    out,
                    "    {marker} [{}] {} \"{}\"{}",
                    tab.kind, tab.id, tab.title, agent
                );
            }
        }
    }
    out
}

pub(crate) fn render_agents(
    workspaces: &[flowmux_ipc::protocol::TreeWorkspace],
    json: bool,
) -> anyhow::Result<String> {
    if json {
        let mut rows = Vec::new();
        for workspace in workspaces {
            for pane in &workspace.panes {
                for tab in &pane.tabs {
                    let Some(agent) = &tab.agent else { continue };
                    rows.push(serde_json::json!({
                        "workspace": workspace.name,
                        "workspace_id": workspace.id,
                        "root": workspace.root,
                        "pane": pane.id,
                        "tab": tab.id,
                        "agent": agent.name,
                        "status": agent.status.as_str(),
                        "session_name": agent.session_name,
                        "messaging": agent.messaging_socket.is_some(),
                    }));
                }
            }
        }
        return Ok(serde_json::to_string(&rows)?);
    }

    use std::fmt::Write as _;
    let mut out = String::from("WORKSPACE\tPANE\tTAB\tAGENT\tSTATUS\tSESSION NAME\tMESSAGING\n");
    for workspace in workspaces {
        for pane in &workspace.panes {
            for tab in &pane.tabs {
                let Some(agent) = &tab.agent else { continue };
                let pane = pane.id.to_string();
                let tab = tab.id.to_string();
                let messaging = if agent.name.eq_ignore_ascii_case("claude") {
                    if agent.messaging_socket.is_some() {
                        "yes"
                    } else {
                        "no"
                    }
                } else {
                    "-"
                };
                let _ = writeln!(
                    out,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    workspace.name,
                    &pane[..8],
                    &tab[..8],
                    agent.name,
                    agent.status.as_str(),
                    agent.session_name.as_deref().unwrap_or("-"),
                    messaging,
                );
            }
        }
    }
    Ok(out.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_responses_render_as_agent_friendly_values() {
        assert_eq!(
            plain_browser_response(&Response::BrowserResult {
                value: "page title".into(),
            }),
            Some("page title".into())
        );
        assert_eq!(
            plain_browser_response(&Response::BrowserBoolResult { value: true }),
            Some("true".into())
        );
        assert_eq!(
            plain_browser_response(&Response::BrowserOk),
            Some("ok".into())
        );
    }

    #[test]
    fn rpc_errors_return_failure() {
        assert!(print_response(
            &Response::Error(flowmux_ipc::protocol::RpcError::NotFound("missing".into())),
            true,
        )
        .is_err());
    }
}
