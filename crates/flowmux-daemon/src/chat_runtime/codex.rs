// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

pub(super) async fn boot(
    wire: &mut Wire,
    session: &mut Session,
    cwd: &Path,
    native_id: Option<&str>,
) -> Result<()> {
    wire.rpc(
        "initialize",
        json!({
            "clientInfo":{"name":"flowmux","title":"Flowmux","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true}
        }),
    )
    .await?;
    wire.notify("initialized", json!({})).await?;
    session.catalog.commands = COMMANDS
        .iter()
        .map(|(name, description, hint)| SlashCommand::new(name, description, hint))
        .collect();

    let mut cursor: Option<String> = None;
    for _ in 0..8 {
        let page = wire
            .rpc(
                "model/list",
                json!({"cursor":cursor,"limit":100,"includeHidden":false}),
            )
            .await?;
        for model in values(&page["data"]).filter(|model| model["hidden"] != true) {
            let id = str_at(model, "model");
            if id.is_empty() {
                continue;
            }
            session.catalog.models.choices.push(Choice::new(
                id,
                str_at(model, "displayName"),
                str_at(model, "description"),
            ));
            if model["isDefault"] == true {
                session.catalog.models.current = Some(id.into());
            }
            session.catalog.model_efforts.insert(
                id.into(),
                Menu {
                    choices: values(&model["supportedReasoningEfforts"])
                        .map(|effort| {
                            let key = str_at(effort, "reasoningEffort");
                            Choice::new(key, key, str_at(effort, "description"))
                        })
                        .collect(),
                    current: model["defaultReasoningEffort"].as_str().map(str::to_owned),
                },
            );
        }
        let next = page["nextCursor"].as_str().map(str::to_owned);
        if next.is_none() || next == cursor {
            break;
        }
        cursor = next;
    }
    session.catalog.access = Menu {
        current: Some("workspace".into()),
        choices: vec![
            Choice::new(
                "read-only",
                "Read-only",
                "Read files. Write and elevation requests are disabled.",
            ),
            Choice::new(
                "workspace",
                "Workspace",
                "Write in the working directory. Other actions can request approval.",
            ),
            Choice::new(
                "full-access",
                "Full Access",
                "Run with full filesystem and network access for this chat.",
            ),
        ],
    };
    session.catalog.plan = true;
    let mut request = json!({
        "cwd":cwd, "sandbox":"workspace-write", "approvalPolicy":"on-request", "approvalsReviewer":"user",
    });
    let method = if let Some(id) = native_id {
        request["threadId"] = json!(id);
        request["excludeTurns"] = json!(true);
        "thread/resume"
    } else {
        "thread/start"
    };
    let started = wire.rpc(method, request).await?;
    session.key = started["thread"]["id"]
        .as_str()
        .context("Missing Codex thread ID")?
        .into();
    Ok(())
}

fn turn_settings(settings: &Settings, cwd: &Path) -> (Value, &'static str) {
    if settings.plan || settings.access.as_deref() == Some("read-only") {
        return (json!({"type":"readOnly","networkAccess":false}), "never");
    }
    if settings.access.as_deref() == Some("full-access") {
        return (json!({"type":"dangerFullAccess"}), "never");
    }
    (
        json!({"type":"workspaceWrite","writableRoots":[cwd],"networkAccess":false,
        "excludeTmpdirEnvVar":true,"excludeSlashTmp":true}),
        "on-request",
    )
}

/// Commands Codex runs through its own requests; typed as text, the model would only read them.
const COMMANDS: [(&str, &str, &str); 2] = [
    ("compact", "Summarize the conversation to free context", ""),
    (
        "review",
        "Review uncommitted changes, or follow your instructions",
        "[instructions]",
    ),
];

/// The arguments of `/name args`, when `text` is that command.
pub(super) fn command<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let rest = text.trim_start().strip_prefix('/')?.strip_prefix(name)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

pub(super) async fn send(
    wire: &mut Wire,
    session: &Session,
    cwd: &Path,
    text: &str,
    settings: &Settings,
) -> Result<u64> {
    if command(text, "compact").is_some() {
        return wire
            .request("thread/compact/start", json!({"threadId":session.key}))
            .await;
    }
    if let Some(instructions) = command(text, "review") {
        let target = if instructions.is_empty() {
            json!({"type":"uncommittedChanges"})
        } else {
            json!({"type":"custom","instructions":instructions})
        };
        return wire
            .request(
                "review/start",
                json!({"threadId":session.key,"target":target,"delivery":"inline"}),
            )
            .await;
    }
    let (sandbox, approval) = turn_settings(settings, cwd);
    let prompt = if settings.plan {
        format!("Plan only. Investigate the request and present a concrete plan. Do not change files.\n\n{text}")
    } else {
        text.to_owned()
    };
    wire.request(
        "turn/start",
        json!({
            "threadId":session.key, "input":[{"type":"text","text":prompt,"text_elements":[]}],
            "clientUserMessageId":uuid::Uuid::new_v4().to_string(),
            "model":settings.model, "effort":settings.effort, "summary":"auto",
            "sandboxPolicy":sandbox, "approvalPolicy":approval, "approvalsReviewer":"user",
        }),
    )
    .await
}

pub(super) async fn steer(
    wire: &mut Wire,
    thread: &str,
    turn: &mut Turn,
    text: String,
) -> Result<()> {
    let Some(turn_id) = turn.remote_id.as_deref() else {
        return Ok(());
    };
    let request = wire
        .request(
            "turn/steer",
            json!({
                "threadId":thread, "expectedTurnId":turn_id,
                "input":[{"type":"text","text":text,"text_elements":[]}],
                "clientUserMessageId":uuid::Uuid::new_v4().to_string(),
            }),
        )
        .await?;
    turn.steer = Some((request, text));
    Ok(())
}

pub(super) async fn interrupt(wire: &mut Wire, thread: &str, turn: &mut Turn) -> Result<()> {
    if let Some(turn_id) = turn
        .remote_id
        .as_ref()
        .filter(|_| turn.interrupt_request.is_none())
    {
        turn.interrupt_request = Some(
            wire.request(
                "turn/interrupt",
                json!({"threadId":thread,"turnId":turn_id}),
            )
            .await?,
        );
    }
    Ok(())
}

pub(super) async fn frame(
    wire: &mut Wire,
    session: &mut Session,
    events: &Sender<Event>,
    frame: &Value,
) -> Result<()> {
    let method = str_at(frame, "method");
    let params = &frame["params"];
    let relevant = params["threadId"].as_str().is_none_or(|thread| {
        thread == session.key
            || session
                .active
                .as_ref()
                .and_then(|turn| turn.review_thread.as_deref())
                == Some(thread)
    });
    let Some(turn) = session.active.as_mut().filter(|_| relevant) else {
        if !method.is_empty() && frame.get("id").is_some() {
            wire.unsupported(&frame["id"]).await?;
        }
        return Ok(());
    };
    if method.is_empty() {
        if frame["id"].as_u64() == turn.request {
            let result = match response(frame) {
                Ok(result) => result,
                Err(error) => {
                    session.active = None;
                    return emit(events, Event::Rejected(error.to_string())).await;
                }
            };
            turn.remote_id = result["turn"]["id"]
                .as_str()
                .map(str::to_owned)
                .or(turn.remote_id.take());
            turn.review_thread = result["reviewThreadId"]
                .as_str()
                .filter(|thread| *thread != session.key)
                .map(str::to_owned);
            emit(events, Event::SettingsApplied(turn.settings.clone())).await?;
            if turn.stop {
                interrupt(wire, &session.key, turn).await?;
            }
        } else if let Some((_, text)) = turn
            .steer
            .take_if(|(request, _)| frame["id"].as_u64() == Some(*request))
        {
            emit(
                events,
                match response(frame) {
                    Ok(_) => Event::Steered { text },
                    Err(error) => Event::SteerRejected(error.to_string()),
                },
            )
            .await?;
        } else if frame["id"].as_u64() == turn.interrupt_request && frame.get("error").is_some() {
            turn.stop = false;
            turn.stop_deadline = None;
            turn.interrupt_request = None;
            emit(
                events,
                Event::StopFailed("Could not stop the Codex turn.".into()),
            )
            .await?;
        }
        return Ok(());
    }
    let remote = params["turnId"]
        .as_str()
        .or_else(|| params["turn"]["id"].as_str());
    if turn
        .remote_id
        .as_deref()
        .zip(remote)
        .is_some_and(|(active, incoming)| active != incoming)
    {
        if frame.get("id").is_some() {
            wire.unsupported(&frame["id"]).await?;
        }
        return Ok(());
    }
    if turn.remote_id.is_none() {
        turn.remote_id = remote.map(str::to_owned);
        if turn.stop {
            interrupt(wire, &session.key, turn).await?;
        }
    }
    if frame.get("id").is_some() {
        return request(wire, turn, events, frame).await;
    }
    match method {
        "item/agentMessage/delta" | "item/reasoning/summaryTextDelta" | "item/plan/delta" => {
            let id = str_at(params, "itemId");
            let delta = str_at(params, "delta");
            if id.is_empty() || delta.is_empty() {
                return Ok(());
            }
            let (kind, id) = match method {
                "item/reasoning/summaryTextDelta" => (
                    TextKind::Reasoning,
                    format!("{id}:{}", params["summaryIndex"]),
                ),
                _ => (TextKind::Answer, id.into()),
            };
            emit(
                events,
                Event::Text {
                    id,
                    kind,
                    text: clip(delta, MAX_DETAIL),
                    append: true,
                },
            )
            .await?;
        }
        "item/started" | "item/completed" => {
            let item = &params["item"];
            let running = method == "item/started";
            let id = str_at(item, "id");
            match str_at(item, "type") {
                "agentMessage" | "plan" if !running => {
                    let text = str_at(item, "text");
                    if !text.is_empty() {
                        emit(
                            events,
                            Event::Text {
                                id: id.into(),
                                kind: TextKind::Answer,
                                text: clip(text, MAX_DETAIL),
                                append: false,
                            },
                        )
                        .await?;
                    }
                }
                "exitedReviewMode" if !running => {
                    emit(
                        events,
                        Event::Text {
                            id: id.into(),
                            kind: TextKind::Answer,
                            text: clip(str_at(item, "review"), MAX_DETAIL),
                            append: false,
                        },
                    )
                    .await?;
                }
                "reasoning" if !running => {
                    for (i, summary) in values(&item["summary"])
                        .filter_map(Value::as_str)
                        .enumerate()
                    {
                        emit(
                            events,
                            Event::Text {
                                id: format!("{id}:{i}"),
                                kind: TextKind::Reasoning,
                                text: clip(summary, MAX_DETAIL),
                                append: false,
                            },
                        )
                        .await?;
                    }
                }
                _ => {
                    if let Some(work) = item_work(item, running) {
                        turn.work.insert(work.id.clone(), work.clone());
                        emit(events, Event::Work(work)).await?;
                    }
                }
            }
        }
        "turn/plan/updated" => {
            let detail = values(&params["plan"])
                .map(|step| {
                    let mark = match str_at(step, "status") {
                        "completed" => "✓",
                        "inProgress" => "→",
                        _ => "○",
                    };
                    format!("{mark} {}", str_at(step, "step"))
                })
                .collect::<Vec<_>>()
                .join("\n");
            emit(
                events,
                Event::Work(Work {
                    id: "turn-plan".into(),
                    title: "Plan".into(),
                    detail: clip(&detail, MAX_DETAIL),
                    running: false,
                    failed: false,
                    read: false,
                    changes: Vec::new(),
                    check: None,
                }),
            )
            .await?;
        }
        "thread/compacted" | "turn/completed" if turn.compact => {
            emit(
                events,
                Event::Text {
                    id: "compact".into(),
                    kind: TextKind::Answer,
                    text: "Conversation compacted.".into(),
                    append: false,
                },
            )
            .await?;
            settle(session, events, "Completed", None).await?;
        }
        "turn/completed" => {
            let result = &params["turn"];
            let detail = result["error"]["message"].as_str().map(|s| clip(s, 2048));
            let status = match str_at(result, "status") {
                "completed" => "Completed",
                "interrupted" => "Stopped",
                _ => "Failed",
            };
            settle(session, events, status, detail).await?;
        }
        _ => {}
    }
    Ok(())
}

fn item_work(item: &Value, running: bool) -> Option<Work> {
    let name = str_at(item, "type");
    let failed = matches!(str_at(item, "status"), "failed" | "declined" | "cancelled")
        || item["exitCode"].as_i64().is_some_and(|code| code != 0);
    let mut work = Work {
        id: str_at(item, "id").into(),
        title: String::new(),
        detail: String::new(),
        running,
        failed,
        read: false,
        changes: Vec::new(),
        check: None,
    };
    match name {
        "commandExecution" => {
            let command = str_at(item, "command");
            work.title = format!("Command · {}", clip(command, 150));
            let actions = &item["commandActions"];
            work.read = actions.as_array().is_some_and(|actions| {
                !actions.is_empty()
                    && actions.iter().all(|action| {
                        matches!(str_at(action, "type"), "read" | "listFiles" | "search")
                    })
            });
            if work.read {
                work.title = format!("Read · {}", clip(command, 150));
            }
            work.detail = format!(
                "{}\n{}\n{}",
                str_at(item, "cwd"),
                command,
                str_at(item, "aggregatedOutput")
            );
            if let Some(code) = item["exitCode"].as_i64() {
                let label = format!("Exit code {code}");
                work.detail.push_str(&format!("\n{label}"));
                work.check = Some(format!("{} · {label}", clip(command, 240)));
            }
        }
        "fileChange" => {
            work.title = "File changes".into();
            work.changes = values(&item["changes"])
                .take(100)
                .filter_map(|change| {
                    let path = change["path"].as_str()?;
                    Some(Change {
                        path: path.into(),
                        diff: clip(str_at(change, "diff"), 8192),
                    })
                })
                .collect();
            work.detail = work
                .changes
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        }
        "webSearch" => {
            work.title = "Web search".into();
            work.detail = str_at(item, "query").into();
        }
        "mcpToolCall" | "dynamicToolCall" => {
            work.title = format!("Tool · {}", str_at(item, "tool"));
            work.detail = item
                .get("result")
                .filter(|value| !value.is_null())
                .map(Value::to_string)
                .unwrap_or_default();
        }
        _ => return None,
    }
    work.detail = clip(work.detail.trim(), MAX_DETAIL);
    Some(work)
}

async fn request(
    wire: &mut Wire,
    turn: &mut Turn,
    events: &Sender<Event>,
    frame: &Value,
) -> Result<()> {
    let params = &frame["params"];
    let method = str_at(frame, "method");
    let id = frame["id"].clone();
    if method == "item/tool/requestUserInput" {
        let questions: Vec<_> = values(&params["questions"])
            .take(12)
            .map(|question| Question {
                id: str_at(question, "id").into(),
                title: clip(str_at(question, "question"), 1024),
                options: values(&question["options"])
                    .map(|option| str_at(option, "label").to_owned())
                    .collect(),
            })
            .collect();
        let ids = questions
            .iter()
            .map(|question| question.id.clone())
            .collect();
        return register_prompt(
            turn,
            events,
            id,
            "Question from Codex",
            "",
            vec!["Answer".into()],
            questions,
            Reply::CodexQuestions(ids),
        )
        .await;
    }
    let read_only = turn.settings.plan || turn.settings.access.as_deref() == Some("read-only");
    let mut detail = str_at(params, "reason").to_owned();
    let show = |section: &str, detail: &mut String| {
        if !section.is_empty() {
            if !detail.is_empty() {
                detail.push('\n');
            }
            detail.push_str(section);
        }
    };
    let mut allow = true;
    let reply = match method {
        "item/commandExecution/requestApproval" => {
            let work = turn.work.get(str_at(params, "itemId"));
            let command = str_at(params, "command");
            show(str_at(params, "cwd"), &mut detail);
            show(command, &mut detail);
            if command.is_empty() {
                if let Some(work) = work {
                    show(&work.detail, &mut detail);
                } else {
                    allow = false;
                }
            }
            "Allow command"
        }
        "item/fileChange/requestApproval" => {
            let work = turn.work.get(str_at(params, "itemId"));
            if let Some(work) = work {
                show(&work.detail, &mut detail);
                for change in &work.changes {
                    show(&change.diff, &mut detail);
                }
            }
            show(str_at(params, "grantRoot"), &mut detail);
            allow = work.is_some_and(|work| !work.detail.is_empty())
                || !str_at(params, "grantRoot").is_empty();
            "Allow file changes"
        }
        "item/permissions/requestApproval" => {
            let requested = &params["permissions"];
            show(&requested.to_string(), &mut detail);
            let mut labels = vec!["Deny".into()];
            let mut responses = vec![json!({"permissions":{},"scope":"turn"})];
            if requested.is_object() && !read_only && detail.len() <= MAX_DETAIL {
                labels.insert(0, "Allow for this turn".into());
                responses.insert(0, json!({"permissions": requested, "scope":"turn"}));
            }
            return register_prompt(
                turn,
                events,
                id,
                "Permissions request",
                &detail,
                labels,
                Vec::new(),
                Reply::Rpc(responses),
            )
            .await;
        }
        _ => return wire.unsupported(&id).await,
    };
    let mut labels = vec!["Deny".into()];
    let mut responses = vec![json!({"decision":"decline"})];
    if allow
        && !read_only
        && detail.len() <= MAX_DETAIL
        && (params["availableDecisions"].is_null()
            || values(&params["availableDecisions"]).any(|choice| choice == "accept"))
    {
        labels.insert(0, "Allow once".into());
        responses.insert(0, json!({"decision":"accept"}));
    }
    register_prompt(
        turn,
        events,
        id,
        reply,
        &detail,
        labels,
        Vec::new(),
        Reply::Rpc(responses),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_need_their_exact_name() {
        assert_eq!(command("/compact", "compact"), Some(""));
        assert_eq!(
            command("/review focus on tests", "review"),
            Some("focus on tests")
        );
        assert_eq!(command("/compaction", "compact"), None);
        assert_eq!(command("please /compact", "compact"), None);
    }

    #[test]
    fn plan_and_read_only_do_not_inherit_full_access() {
        let mut settings = Settings {
            access: Some("full-access".into()),
            plan: true,
            ..Settings::default()
        };
        let (policy, approval) = turn_settings(&settings, Path::new("/project"));
        assert_eq!(policy["type"], "readOnly");
        assert_eq!(approval, "never");
        settings.plan = false;
        settings.access = Some("workspace".into());
        let (policy, approval) = turn_settings(&settings, Path::new("/project"));
        assert_eq!(policy["writableRoots"], json!(["/project"]));
        assert_eq!(approval, "on-request");
    }
}
