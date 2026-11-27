// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

pub(super) async fn boot(
    wire: &mut Wire,
    session: &mut Session,
    cwd: &Path,
    native_id: Option<&str>,
) -> Result<()> {
    let initialize = wire.rpc("initialize", json!({
        "protocolVersion":1,"clientCapabilities":{},
        "clientInfo":{"name":"flowmux","title":"Flowmux","version":env!("CARGO_PKG_VERSION")}
    })).await?;
    if initialize["protocolVersion"].as_u64() != Some(1) {
        bail!("This agent requires an unsupported ACP protocol version.");
    }
    let result = if let Some(id) = native_id {
        if initialize["agentCapabilities"]["loadSession"] != true {
            bail!(
                "This agent cannot resume a saved chat. The conversation remains available here."
            );
        }
        let result = wire
            .rpc(
                "session/load",
                json!({"sessionId":id,"cwd":cwd,"mcpServers":[]}),
            )
            .await?;
        session.key = id.into();
        result
    } else {
        let result = wire
            .rpc("session/new", json!({"cwd":cwd,"mcpServers":[]}))
            .await?;
        session.key = result["sessionId"]
            .as_str()
            .context("Missing ACP session ID")?
            .into();
        result
    };
    update_options(session, &result["configOptions"]);
    if session.model_option.is_none() {
        session.catalog.models = legacy_menu(
            &result["models"],
            "availableModels",
            "currentModelId",
            "modelId",
        );
    }
    if session.access_option.is_none() {
        session.catalog.access =
            legacy_menu(&result["modes"], "availableModes", "currentModeId", "id");
    }
    Ok(())
}

fn legacy_menu(data: &Value, choices: &str, current: &str, id: &str) -> Menu {
    Menu {
        current: data[current].as_str().map(str::to_owned),
        choices: values(&data[choices])
            .filter_map(|value| {
                let id = value[id].as_str()?;
                Some(Choice::new(
                    id,
                    value["name"].as_str().unwrap_or(id),
                    str_at(value, "description"),
                ))
            })
            .collect(),
    }
}

fn update_options(session: &mut Session, options: &Value) {
    for config in values(options) {
        let Some(id) = config["id"].as_str() else {
            continue;
        };
        let choices = values(&config["options"])
            .flat_map(|choice| {
                if choice["options"].is_array() {
                    values(&choice["options"]).collect::<Vec<_>>()
                } else {
                    vec![choice]
                }
            })
            .filter_map(|choice| {
                let value = choice["value"].as_str()?;
                Some(Choice::new(
                    value,
                    choice["name"].as_str().unwrap_or(value),
                    str_at(choice, "description"),
                ))
            })
            .collect::<Vec<_>>();
        if choices.is_empty() {
            continue;
        }
        let menu = Menu {
            choices,
            current: config["currentValue"].as_str().map(str::to_owned),
        };
        match str_at(config, "category") {
            "model" => {
                session.model_option = Some(id.into());
                session.catalog.models = menu;
            }
            "mode" => {
                session.access_option = Some(id.into());
                session.catalog.access = menu;
            }
            "thought_level" | "model_config"
                if session.effort_option.as_deref().is_none_or(|old| old == id) =>
            {
                session.effort_option = Some(id.into());
                session.catalog.efforts = menu;
            }
            _ => {}
        }
    }
}

pub(super) async fn send(
    wire: &mut Wire,
    session: &mut Session,
    text: &str,
    settings: &Settings,
) -> Result<u64> {
    for (value, option, current, method, field) in [
        (
            &settings.model,
            session.model_option.clone(),
            session.catalog.models.current.clone(),
            "session/set_model",
            "modelId",
        ),
        (
            &settings.access,
            session.access_option.clone(),
            session.catalog.access.current.clone(),
            "session/set_mode",
            "modeId",
        ),
        (
            &settings.effort,
            session.effort_option.clone(),
            session.catalog.efforts.current.clone(),
            "",
            "",
        ),
    ] {
        let Some(value) = value
            .as_ref()
            .filter(|value| Some(*value) != current.as_ref())
        else {
            continue;
        };
        if let Some(option) = option {
            let response = wire
                .rpc(
                    "session/set_config_option",
                    json!({
                        "sessionId":session.key,"configId":option,"value":value,
                    }),
                )
                .await?;
            update_options(session, &response["configOptions"]);
            let effective = if session.model_option.as_ref() == Some(&option) {
                &session.catalog.models.current
            } else if session.access_option.as_ref() == Some(&option) {
                &session.catalog.access.current
            } else {
                &session.catalog.efforts.current
            };
            if effective.as_ref() != Some(value) {
                bail!("The agent did not apply {}", option);
            }
        } else {
            let mut params = json!({"sessionId":session.key});
            params[field] = json!(value);
            wire.rpc(method, params).await?;
            match field {
                "modelId" => session.catalog.models.current = Some(value.clone()),
                "modeId" => session.catalog.access.current = Some(value.clone()),
                _ => {}
            }
        }
    }
    if let Some(current) = &session.catalog.access.current {
        let elevated = session
            .catalog
            .access
            .choices
            .iter()
            .find(|choice| &choice.id == current)
            .is_some_and(Choice::elevated);
        if elevated && !settings.confirmed_access {
            bail!("Confirm the agent's current access mode or choose a less permissive mode.");
        }
    }
    wire.request(
        "session/prompt",
        json!({"sessionId":session.key,"prompt":[{"type":"text","text":text}]}),
    )
    .await
}

pub(super) async fn frame(
    wire: &mut Wire,
    session: &mut Session,
    events: &Sender<Event>,
    frame: &Value,
) -> Result<()> {
    let params = &frame["params"];
    let matches = params["sessionId"]
        .as_str()
        .is_none_or(|id| id == session.key);
    let method = str_at(frame, "method");
    if !matches {
        if frame.get("id").is_some() && !method.is_empty() {
            wire.unsupported(&frame["id"]).await?;
        }
        return Ok(());
    }
    let update = &params["update"];
    match str_at(update, "sessionUpdate") {
        "config_option_update" => {
            update_options(session, &update["configOptions"]);
            emit(events, Event::Ready(session.catalog.clone())).await?;
        }
        "current_mode_update" => {
            session.catalog.access.current = update["currentModeId"].as_str().map(str::to_owned);
            emit(events, Event::Ready(session.catalog.clone())).await?;
        }
        _ => {}
    }
    let Some(turn) = session.active.as_mut() else {
        if !method.is_empty() && frame.get("id").is_some() {
            wire.unsupported(&frame["id"]).await?;
        }
        return Ok(());
    };
    if method.is_empty() {
        if frame["id"].as_u64() == turn.request {
            match response(frame) {
                Ok(result) => {
                    let reason = str_at(&result, "stopReason");
                    let status = if turn.stop || reason == "cancelled" {
                        "Stopped"
                    } else if reason == "refusal" {
                        "Failed"
                    } else {
                        "Completed"
                    };
                    let detail = match reason {
                        "max_tokens" => Some("The agent reached its output limit.".into()),
                        "max_turn_requests" => Some("The agent reached its request limit.".into()),
                        "refusal" => Some("The agent declined to continue.".into()),
                        _ => None,
                    };
                    settle(session, events, status, detail).await?;
                }
                Err(error) => settle(session, events, "Failed", Some(error.to_string())).await?,
            }
        }
    } else if method == "session/request_permission" {
        turn.acp_text = None;
        let call = &params["toolCall"];
        let detail = if call["rawInput"].is_null() {
            turn.work
                .get(str_at(call, "toolCallId"))
                .map(|work| work.detail.clone())
                .unwrap_or_default()
        } else {
            format!("{}\n{}", str_at(call, "title"), call["rawInput"])
        };
        let options: Vec<_> = values(&params["options"])
            .filter(|choice| {
                let kind = str_at(choice, "kind");
                (matches!(kind, "reject_once" | "reject_always")
                    || (kind == "allow_once" && !detail.is_empty() && detail.len() <= MAX_DETAIL))
                    && choice["optionId"].is_string()
            })
            .collect();
        if options.is_empty() {
            return wire.write(json!({"jsonrpc":"2.0","id":frame["id"],"result":{"outcome":{"outcome":"cancelled"}}})).await;
        }
        let labels = options
            .iter()
            .map(|option| str_at(option, "name").to_owned())
            .collect();
        let ids = options
            .iter()
            .map(|option| str_at(option, "optionId").to_owned())
            .collect();
        register_prompt(
            turn,
            events,
            frame["id"].clone(),
            "Agent permission request",
            &detail,
            labels,
            Vec::new(),
            Reply::Acp(ids),
        )
        .await?;
    } else if method == "session/update" {
        match str_at(update, "sessionUpdate") {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let text = str_at(&update["content"], "text");
                if !text.is_empty() {
                    let kind = if update["sessionUpdate"] == "agent_thought_chunk" {
                        TextKind::Reasoning
                    } else {
                        TextKind::Answer
                    };
                    let index = match turn.acp_text {
                        Some((previous, index)) if previous == kind => index,
                        _ => {
                            turn.acp_next_text += 1;
                            turn.acp_text = Some((kind, turn.acp_next_text));
                            turn.acp_next_text
                        }
                    };
                    let id = format!("acp:{index}");
                    emit(
                        events,
                        Event::Text {
                            id,
                            kind,
                            text: clip(text, MAX_DETAIL),
                            append: true,
                        },
                    )
                    .await?;
                }
            }
            "tool_call" | "tool_call_update" => {
                let id = str_at(update, "toolCallId");
                if id.is_empty() {
                    return Ok(());
                }
                if !turn.work.contains_key(id) {
                    turn.acp_text = None;
                }
                let work = turn.work.entry(id.into()).or_insert_with(|| Work {
                    id: id.into(),
                    title: "Tool".into(),
                    detail: String::new(),
                    running: true,
                    failed: false,
                    read: false,
                    changes: Vec::new(),
                    check: None,
                });
                work.read |= matches!(str_at(update, "kind"), "read" | "search" | "list");
                if let Some(title) = update["title"].as_str() {
                    work.title = clip(title, 200);
                }
                match str_at(update, "status") {
                    "failed" => {
                        work.running = false;
                        work.failed = true;
                    }
                    "completed" => work.running = false,
                    _ => {}
                }
                let mut detail = Vec::new();
                if !update["rawInput"].is_null() {
                    detail.push(update["rawInput"].to_string());
                }
                for content in values(&update["content"]).take(100) {
                    match str_at(content, "type") {
                        "content" => detail.push(str_at(&content["content"], "text").into()),
                        "diff" => {
                            let path = str_at(content, "path");
                            if !path.is_empty() {
                                let before = clip(str_at(content, "oldText"), 4096);
                                let after = clip(str_at(content, "newText"), 4096);
                                let diff = before
                                    .lines()
                                    .map(|line| format!("-{line}"))
                                    .chain(after.lines().map(|line| format!("+{line}")))
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                let change = Change {
                                    path: path.into(),
                                    diff,
                                };
                                if let Some(index) =
                                    work.changes.iter().position(|value| value.path == path)
                                {
                                    work.changes[index] = change;
                                } else {
                                    work.changes.push(change);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !detail.is_empty() {
                    let detail = detail.join("\n");
                    if work.detail.is_empty() || detail.starts_with(&work.detail) {
                        work.detail = clip(&detail, MAX_DETAIL);
                    } else if !work.detail.ends_with(&detail) {
                        work.detail = clip(&format!("{}\n{detail}", work.detail), MAX_DETAIL);
                    }
                }
                emit(events, Event::Work(work.clone())).await?;
            }
            "plan" => {
                turn.acp_text = None;
                let detail = values(&update["entries"])
                    .map(|step| {
                        let mark = match str_at(step, "status") {
                            "completed" => "✓",
                            "in_progress" => "→",
                            _ => "○",
                        };
                        format!("{mark} {}", str_at(step, "content"))
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
            _ => {}
        }
    } else if frame.get("id").is_some() {
        wire.unsupported(&frame["id"]).await?;
    }
    Ok(())
}
