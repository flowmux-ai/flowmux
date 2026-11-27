// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

pub(super) const ARGS: &[&str] = &[
    "--print",
    "--input-format",
    "stream-json",
    "--output-format",
    "stream-json",
    "--verbose",
    "--include-partial-messages",
    "--permission-prompt-tool",
    "stdio",
    "--permission-mode",
    "plan",
    "--allow-dangerously-skip-permissions",
];

async fn control(wire: &mut Wire, request: Value) -> Result<Value> {
    let id = wire.id().to_string();
    wire.write(json!({"type":"control_request","request_id":id,"request":request}))
        .await?;
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(frame) = wire.read().await? {
            let response = &frame["response"];
            if frame["type"] == "control_response" && response["request_id"] == id {
                if response["subtype"] == "success" {
                    return Ok(response["response"].clone());
                }
                bail!(
                    "{}",
                    response["error"]
                        .as_str()
                        .unwrap_or("Claude rejected the change.")
                );
            }
            if frame["type"] == "control_request" {
                reject(wire, &frame["request_id"]).await?;
            }
        }
        bail!("Claude ended before applying the request.")
    })
    .await
    .context("Claude did not respond")?
}

pub(super) async fn boot(wire: &mut Wire, session: &mut Session) -> Result<()> {
    let result = control(wire, json!({"subtype":"initialize","hooks":{}})).await?;
    session.catalog.models.choices = values(&result["models"])
        .filter_map(|model| {
            let id = model["value"].as_str()?;
            Some(Choice::new(
                id,
                model["displayName"].as_str().unwrap_or(id),
                str_at(model, "description"),
            ))
        })
        .collect();
    session.catalog.models.current = session
        .catalog
        .models
        .choices
        .iter()
        .find(|choice| choice.id == "default")
        .map(|choice| choice.id.clone());
    session.catalog.access = Menu {
        current: Some("default".into()),
        choices: vec![
            Choice::new(
                "default",
                "Ask",
                "Use Claude's permission rules and ask for tools that require approval.",
            ),
            Choice::new("plan", "Plan", "Explore files and prepare a plan."),
            Choice::new(
                "acceptEdits",
                "Accept edits",
                "Allow file edits under Claude's rules. Other tools may ask.",
            ),
            Choice::new(
                "bypassPermissions",
                "Full Access",
                "Bypass Claude permission prompts for this session.",
            ),
        ],
    };
    Ok(())
}

pub(super) async fn send(
    wire: &mut Wire,
    session: &mut Session,
    text: &str,
    settings: &Settings,
) -> Result<()> {
    if let Some(model) = settings
        .model
        .as_ref()
        .filter(|model| Some(*model) != session.catalog.models.current.as_ref())
    {
        control(wire, json!({"subtype":"set_model","model":model})).await?;
        session.catalog.models.current = Some(model.clone());
    }
    let mode = settings.access.as_deref().unwrap_or("default");
    let applied = control(wire, json!({"subtype":"set_permission_mode","mode":mode})).await?;
    let reported = str_at(&applied, "mode");
    if !reported.is_empty() && reported != mode && !(mode == "default" && reported == "manual") {
        bail!("Claude did not apply the selected permission mode.");
    }
    session.catalog.access.current = Some(mode.into());
    wire.write(
        json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":text}]},
        "parent_tool_use_id":null,"session_id":session.key}),
    )
    .await
}

pub(super) async fn interrupt(wire: &mut Wire) -> Result<()> {
    let id = wire.id().to_string();
    wire.write(json!({"type":"control_request","request_id":id,"request":{"subtype":"interrupt"}}))
        .await
}

async fn reject(wire: &mut Wire, id: &Value) -> Result<()> {
    wire.write(json!({"type":"control_response","response":{"subtype":"error","request_id":id,"error":"Request unavailable in Flowmux Chat"}})).await
}

pub(super) async fn frame(
    wire: &mut Wire,
    session: &mut Session,
    events: &Sender<Event>,
    frame: &Value,
) -> Result<()> {
    let kind = str_at(frame, "type");
    if let Some(id) = frame["session_id"].as_str() {
        session.key = id.into();
    }
    let Some(turn) = session.active.as_mut() else {
        if kind == "control_request" {
            reject(wire, &frame["request_id"]).await?;
        }
        return Ok(());
    };
    if kind == "control_request" {
        let request = &frame["request"];
        if request["subtype"] != "can_use_tool" {
            return reject(wire, &frame["request_id"]).await;
        }
        let name = str_at(request, "tool_name");
        let input = request["input"].clone();
        let description = input.to_string();
        if name.is_empty() || description.len() > MAX_DETAIL {
            return reject(wire, &frame["request_id"]).await;
        }
        let title = format!("Allow {name}");
        return register_prompt(
            turn,
            events,
            frame["request_id"].clone(),
            &title,
            &description,
            vec!["Allow once".into(), "Deny".into()],
            Vec::new(),
            Reply::Claude(input),
        )
        .await;
    }
    if !frame["parent_tool_use_id"].is_null() && kind != "result" {
        return Ok(());
    }
    match kind {
        "stream_event" => {
            let event = &frame["event"];
            match str_at(event, "type") {
                "message_start" => turn.message_id = str_at(&event["message"], "id").into(),
                "content_block_delta" if event["delta"]["type"] == "text_delta" => {
                    emit(
                        events,
                        Event::Text {
                            id: format!("{}:{}", turn.message_id, event["index"]),
                            kind: TextKind::Answer,
                            text: clip(str_at(&event["delta"], "text"), MAX_DETAIL),
                            append: true,
                        },
                    )
                    .await?;
                }
                _ => {}
            }
        }
        "assistant" => {
            let message = &frame["message"];
            for (index, block) in values(&message["content"]).enumerate() {
                match str_at(block, "type") {
                    "text" => {
                        emit(
                            events,
                            Event::Text {
                                id: format!("{}:{index}", str_at(message, "id")),
                                kind: TextKind::Answer,
                                text: clip(str_at(block, "text"), MAX_DETAIL),
                                append: false,
                            },
                        )
                        .await?
                    }
                    "tool_use" => {
                        let id = str_at(block, "id");
                        let input = &block["input"];
                        let tool = str_at(block, "name");
                        let work = Work {
                            id: id.into(),
                            title: format!("Tool · {tool}"),
                            detail: clip(&input.to_string(), MAX_DETAIL),
                            running: true,
                            failed: false,
                            read: matches!(tool, "Read" | "Grep" | "Glob"),
                            changes: input["file_path"]
                                .as_str()
                                .filter(|_| matches!(tool, "Edit" | "Write" | "MultiEdit"))
                                .map(|path| {
                                    vec![Change {
                                        path: path.into(),
                                        diff: String::new(),
                                    }]
                                })
                                .unwrap_or_default(),
                            check: None,
                        };
                        turn.work.insert(id.into(), work.clone());
                        emit(events, Event::Work(work)).await?;
                    }
                    _ => {}
                }
            }
        }
        "user" => {
            for block in
                values(&frame["message"]["content"]).filter(|block| block["type"] == "tool_result")
            {
                let id = str_at(block, "tool_use_id");
                let Some(work) = turn.work.get_mut(id) else {
                    continue;
                };
                work.running = false;
                work.failed = block["is_error"] == true;
                let content = &block["content"];
                let detail = content.as_str().map(str::to_owned).unwrap_or_else(|| {
                    values(content)
                        .filter_map(|item| item["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                });
                work.detail = clip(&format!("{}\n{detail}", work.detail), MAX_DETAIL);
                emit(events, Event::Work(work.clone())).await?;
            }
        }
        "result" => {
            let status = if turn.stop {
                "Stopped"
            } else if frame["is_error"] == true {
                "Failed"
            } else {
                "Completed"
            };
            let detail = (status == "Failed").then(|| {
                let errors = values(&frame["errors"])
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n");
                clip(
                    if errors.is_empty() {
                        str_at(frame, "result")
                    } else {
                        &errors
                    },
                    2048,
                )
            });
            settle(session, events, status, detail).await?;
        }
        "control_response" if turn.stop && frame["response"]["subtype"] == "error" => {
            turn.stop = false;
            turn.stop_deadline = None;
            emit(
                events,
                Event::StopFailed("Could not stop the Claude turn.".into()),
            )
            .await?;
        }
        _ => {}
    }
    Ok(())
}
