// SPDX-License-Identifier: GPL-3.0-or-later
//! Response rendering: print_response and the tree view.

use super::*;

pub(crate) fn render_agent_explanations(
    agents: &[flowmux_ipc::protocol::AgentExplanation],
) -> String {
    use std::fmt::Write;
    if agents.is_empty() {
        return "No current agent evidence for the requested surface(s).\n".into();
    }
    let mut text = String::new();
    for agent in agents {
        let _ = writeln!(
            text,
            "{} · {:?} · {:?}\n  workspace={} pane={} surface={}",
            agent.agent, agent.status, agent.attention, agent.workspace, agent.pane, agent.surface
        );
        let _ = writeln!(
            text,
            "  identity={} evidence={:?} sequence={:?} session_key={:?}\n  seen={} outcome={:?}",
            agent.identity_source.as_deref().unwrap_or("unconfirmed"),
            agent.status_evidence,
            agent.sequence,
            agent.session_key,
            agent.seen,
            agent.turn_outcome
        );
        for wait in &agent.pending_waits {
            let _ = writeln!(
                text,
                "  wait {:?}: {:?}, actor={:?}, retry={:?}, count={}, since={}, context={:?}",
                wait.scope,
                wait.reason,
                wait.actor,
                wait.retry,
                wait.count,
                wait.since,
                wait.context_evidence
            );
        }
        if let Some(report) = &agent.terminal_report {
            let _ = writeln!(
                text,
                "  terminal reports: {} ({}) since={}",
                report.records.len(),
                if agent.terminal_report_primary {
                    "displayed"
                } else {
                    "supplemental"
                },
                report.since
            );
            for record in &report.records {
                let _ = writeln!(
                    text,
                    "    id={:?} state={} app={:?} progress={:?} reason={:?}",
                    record.id, record.state, record.app, record.progress, record.reason
                );
            }
        }
        match &agent.children {
            Some(children) => {
                let _ = writeln!(
                    text,
                    "  observed active children: {}; parent completion pending={}",
                    children.active.len(),
                    children.parent_completion_pending
                );
                for child in &children.active {
                    let _ = writeln!(text, "    child={} turn={}", child.id, child.turn_id);
                    for wait in &child.pending_waits {
                        let _ = writeln!(
                            text,
                            "      wait {:?}, actor={:?}, retry={:?}, since={}, context={:?}",
                            wait.wait.reason,
                            wait.wait.actor,
                            wait.wait.retry,
                            wait.wait.since,
                            wait.wait.context_evidence
                        );
                    }
                }
                for wait in &children.unattributed_waits {
                    let _ = writeln!(
                        text,
                        "    unattributed child wait={} reason={:?} actor={:?} retry={:?} since={}",
                        wait.scope_id,
                        wait.wait.reason,
                        wait.wait.actor,
                        wait.wait.retry,
                        wait.wait.since
                    );
                }
                let _ = writeln!(text, "  Child observations are incomplete; the surface above opens the parent terminal, not a child approval.");
            }
            None => {
                let _ = writeln!(
                    text,
                    "  child observations unavailable (does not mean zero children)"
                );
            }
        }
    }
    text
}

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

pub(crate) fn compact_browser_snapshot(response: &mut Response) -> anyhow::Result<()> {
    if let Response::BrowserResult { value } = response {
        let mut snapshot: serde_json::Value = serde_json::from_str(value)?;
        let object = snapshot
            .as_object_mut()
            .context("expected a snapshot object")?;
        let refs = object
            .get_mut("refs")
            .and_then(serde_json::Value::as_object_mut)
            .context("expected snapshot refs")?;
        for meta in refs.values_mut() {
            meta.as_object_mut()
                .context("expected element metadata")?
                .remove("selector");
        }
        object.remove("markdown");
        *value = serde_json::to_string(&snapshot)?;
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
            ws.location.display()
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
                        "identity_source": agent.source,
                        "status_evidence": agent.status_evidence,
                        "turn_outcome": agent.turn_outcome,
                        "pending_waits": agent.pending_waits,
                        "attention": flowmux_core::agent_attention(agent.status, agent.status != flowmux_core::AgentStatus::Done, agent.turn_outcome, &agent.pending_waits),
                        "status_text": agent.status_text,
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
    fn compact_snapshot_keeps_identity_states_and_document_context() {
        let original = serde_json::json!({
            "truncated": true,
            "markdown": "- button Save [ref=e1-opaque]",
            "page": {"surface_id":"tab-1", "url":"https://example.test", "text":"Article prose"},
            "refs": {"e1-opaque": {
                "role":"button", "name":"Save", "selector":"button:nth-of-type(2)",
                "states":{"enabled":false,"checked":false,"invalid":true},
                "description":"Requires a valid email",
                "frame_path":[{"selector":"#frame", "shadow_path":["#host"]}],
                "shadow_path":["#inner"], "frame_accessible":false
            }}
        });
        let mut response = Response::BrowserResult {
            value: original.to_string(),
        };
        compact_browser_snapshot(&mut response).unwrap();
        let Response::BrowserResult { value } = response else {
            unreachable!()
        };
        let compact: serde_json::Value = serde_json::from_str(&value).unwrap();
        assert_eq!(compact["page"], original["page"]);
        assert_eq!(compact["truncated"], true);
        assert!(compact.get("markdown").is_none());
        let mut expected = original["refs"].clone();
        expected["e1-opaque"]
            .as_object_mut()
            .unwrap()
            .remove("selector");
        assert_eq!(compact["refs"], expected);
        assert!(value.len() < original.to_string().len());

        let mut error = Response::Error(flowmux_ipc::protocol::RpcError::NotFound(
            "closed tab".into(),
        ));
        compact_browser_snapshot(&mut error).unwrap();
        assert!(matches!(error, Response::Error(_)));
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
