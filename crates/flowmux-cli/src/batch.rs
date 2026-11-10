// SPDX-License-Identifier: GPL-3.0-or-later
//! `browser run`: a bounded sequence of ordinary browser commands that stops
//! as soon as the page changes underneath it. Every step is sent once through
//! the normal request path; nothing is retried.

use super::*;

/// Steps that only observe the page; they need no settle check afterwards.
const QUERIES: &[&str] = &[
    "snapshot",
    "find",
    "read",
    "links",
    "grep",
    "query",
    "options",
    "text",
    "value",
    "attr",
    "url",
    "title",
    "is-visible",
    "is-enabled",
    "is-checked",
    "count",
    "wait",
    "screenshot",
    "diagnose",
];
/// Input steps run with a receipt, which reports document changes, dialogs,
/// popups and errors without failing the request.
const RECEIPT_ACTIONS: &[&str] = &[
    "click", "dblclick", "hover", "focus", "blur", "fill", "select", "check", "uncheck", "type",
    "press", "scroll", "pointer", "drag", "form",
];
const LOADS: &[&str] = &["navigate", "back", "forward", "reload"];
const REJECTED: &[&str] = &["run", "record", "open", "control", "task"];
const MAX_STEPS: usize = 20;

pub(crate) struct Step {
    argv: Vec<String>,
    verb: String,
    request: Request,
    compact: bool,
}

#[cfg(test)]
impl Step {
    pub(crate) fn request(self) -> Request {
        self.request
    }
}

/// Parse one step (`[verb, args...]`, without the pane) as a normal command.
pub(crate) fn parse_step(
    step: &[String],
    surface: Option<SurfaceId>,
    task: Option<uuid::Uuid>,
    pane: PaneId,
) -> anyhow::Result<Step> {
    let verb = step.first().context("a run step cannot be empty")?;
    anyhow::ensure!(
        !REJECTED.contains(&verb.as_str()),
        "`{verb}` cannot run inside a batch"
    );
    let parse = |inherited: &[String]| {
        let mut argv = vec!["flowmux".to_string(), "browser".to_string()];
        argv.extend_from_slice(inherited);
        argv.extend([verb.clone(), pane.to_string()]);
        argv.extend(step[1..].iter().cloned());
        Cli::try_parse_from(&argv).map_err(|error| {
            let message = error.to_string();
            anyhow::anyhow!(
                "run step {step:?}: {}",
                message.lines().next().unwrap_or_default()
            )
        })
    };
    // Parsed on its own first: whatever the step sets itself is then visible,
    // in any spelling, and text after `--` is not mistaken for an option.
    let Cmd::Browser {
        surface: own_surface,
        task: own_task,
        receipt,
        op,
    } = parse(&[])?.cmd
    else {
        anyhow::bail!("run step {step:?} is not a browser command");
    };
    anyhow::ensure!(
        own_surface.is_none() && own_task.is_none() && !receipt.enabled,
        "run steps inherit --surface and --task from run and cannot set receipts"
    );
    anyhow::ensure!(
        !matches!(&op, BrowserOp::Eval { source: script, .. } | BrowserOp::EvalAsync { body: script, .. } if script == "-"),
        "a run step cannot read its script from stdin; pass the script as the argument"
    );
    // Options go before the verb so they stay options when the step uses `--`.
    let mut inherited = Vec::new();
    if let Some(surface) = surface {
        inherited.extend(["--surface".to_string(), surface.to_string()]);
    }
    if let Some(task) = task {
        inherited.extend(["--task".to_string(), task.to_string()]);
    }
    if RECEIPT_ACTIONS.contains(&verb.as_str()) {
        inherited.push("--receipt".into());
    }
    let cli = parse(&inherited)?;
    let compact = compact_snapshot(&cli.cmd);
    Ok(Step {
        argv: step.to_vec(),
        verb: verb.clone(),
        request: build_request(cli.cmd)?,
        compact,
    })
}

fn wrap(request: Request, surface: Option<SurfaceId>, task: Option<uuid::Uuid>) -> Request {
    let request = match task {
        Some(task) => Request::BrowserWithTask {
            task,
            request: Box::new(request),
        },
        None => request,
    };
    match surface {
        Some(surface) => Request::BrowserOnSurface {
            surface,
            request: Box::new(request),
        },
        None => request,
    }
}

fn result_value(response: &Response) -> Result<serde_json::Value, serde_json::Value> {
    match response {
        Response::BrowserOk => Ok("ok".into()),
        Response::BrowserBoolResult { value } => Ok((*value).into()),
        Response::BrowserResult { value } => {
            Ok(serde_json::from_str(value).unwrap_or_else(|_| value.clone().into()))
        }
        Response::Error(error) => Err(serde_json::to_value(error).unwrap_or_default()),
        other => Err(format!("unexpected response: {other:?}").into()),
    }
}

/// Current document generation and whether a page dialog is pending. The
/// diagnose query reports both without adding a task result for each check.
async fn observe(
    client: &Client,
    pane: PaneId,
    surface: Option<SurfaceId>,
    task: Option<uuid::Uuid>,
) -> anyhow::Result<(serde_json::Value, bool)> {
    let report = result_value(
        &client
            .call(wrap(Request::BrowserDiagnose { pane }, surface, task))
            .await?,
    )
    .map_err(|error| anyhow::anyhow!("browser tab is not available: {error}"))?;
    Ok((
        report["observation"]["document_generation"].clone(),
        report["pending_dialog"] == true,
    ))
}

pub(crate) async fn run(
    client: &Client,
    surface: Option<SurfaceId>,
    task: Option<uuid::Uuid>,
    pane: PaneId,
    steps: &str,
) -> anyhow::Result<serde_json::Value> {
    let steps: Vec<Vec<String>> = serde_json::from_str(steps).context(
        "steps must be a JSON array of argument arrays without the pane, e.g. [[\"fill\",\"REF\",\"text\"],[\"press\",\"Enter\"]]",
    )?;
    anyhow::ensure!(
        (1..=MAX_STEPS).contains(&steps.len()),
        "run accepts 1 to {MAX_STEPS} steps"
    );
    // Parse every step first so a typo cannot leave a half-run batch.
    let steps = steps
        .iter()
        .map(|step| parse_step(step, surface, task, pane))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let total = steps.len();
    let (mut generation, _) = observe(client, pane, surface, task).await?;
    let mut rows = Vec::new();
    let mut stopped = serde_json::Value::Null;
    for (index, step) in steps.into_iter().enumerate() {
        if !stopped.is_null() {
            rows.push(serde_json::json!({"step": step.argv, "status": "not_run"}));
            continue;
        }
        let verb = step.verb.as_str();
        // Earlier steps already ran, so a lost connection is reported as a row
        // instead of discarding their results. This step may or may not have run.
        let mut response = match client.call(step.request).await {
            Ok(response) => response,
            Err(error) => {
                rows.push(serde_json::json!({"step": step.argv, "status": "unknown",
                    "error": format!("no response: {error:#}")}));
                stopped = serde_json::json!({"index": index, "reason": "no_response"});
                continue;
            }
        };
        if step.compact {
            output::compact_browser_snapshot(&mut response)?;
        }
        let (row, reason) = match result_value(&response) {
            Err(error) => (
                serde_json::json!({"status": "failed", "error": error}),
                Some("error"),
            ),
            Ok(receipt) if RECEIPT_ACTIONS.contains(&verb) => {
                let reason = if !receipt["error"].is_null() {
                    Some("error")
                } else if receipt["status"] == "dialog_pending" {
                    Some("dialog_pending")
                } else if receipt["status"] == "control_changed" {
                    Some("control_changed")
                } else if receipt["status"] == "popup_created" {
                    Some("popup_created")
                } else if receipt["document_after"] != generation {
                    Some("document_changed")
                } else {
                    None
                };
                generation = receipt["document_after"].clone();
                let status = if receipt["error"].is_null() {
                    "ok"
                } else {
                    "failed"
                };
                let mut row = serde_json::json!({"status": status,
                    "receipt": receipt["status"], "value": receipt["value"]});
                if !receipt["error"].is_null() {
                    row["error"] = receipt["error"].clone();
                }
                (row, reason)
            }
            Ok(load) if LOADS.contains(&verb) => {
                // Without --wait the new document has not loaded yet.
                let reason = if load.get("loaded").is_none() {
                    Some("navigation_started")
                } else if load["loaded"] != true {
                    Some("not_loaded")
                } else {
                    generation = load["document_generation"].clone();
                    None
                };
                (serde_json::json!({"status": "ok", "result": load}), reason)
            }
            Ok(serde_json::Value::Bool(false)) if verb == "wait" => (
                serde_json::json!({"status": "not_met", "result": false}),
                Some("condition_not_met"),
            ),
            Ok(result) => (serde_json::json!({"status": "ok", "result": result}), None),
        };
        let mut reason = reason.map(str::to_string);
        // Page effects such as navigation can follow an input slightly later.
        if reason.is_none() && !QUERIES.contains(&verb) && index + 1 < total {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            match observe(client, pane, surface, task).await {
                Ok((_, true)) => reason = Some("dialog_pending".into()),
                Ok((now, false)) => {
                    if now != generation {
                        reason = Some("document_changed".into());
                    }
                    generation = now;
                }
                // The step itself succeeded; the tab is gone or unreachable now.
                Err(_) => reason = Some("tab_unavailable".into()),
            }
        }
        let mut row = row;
        row["step"] = serde_json::json!(step.argv);
        rows.push(row);
        if let Some(reason) = reason {
            stopped = serde_json::json!({"index": index, "reason": reason});
        }
    }
    let completed = rows.iter().filter(|row| row["status"] == "ok").count();
    Ok(
        serde_json::json!({"steps": rows, "completed": completed, "stopped": stopped,
        "automatic_retry": false}),
    )
}
