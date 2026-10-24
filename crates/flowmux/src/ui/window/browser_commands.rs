// SPDX-License-Identifier: GPL-3.0-or-later
//! Browser command dispatch on the GTK main thread.

use super::*;

impl WindowController {
    fn browser_target(
        &self,
        pane: PaneId,
        surface: Option<SurfaceId>,
    ) -> Result<crate::ui::browser_pane::BrowserPane, String> {
        let surface = {
            let registry = self.pane_registry.borrow();
            match surface {
                Some(surface) if registry.pane_for_surface(surface) == Some(pane) => surface,
                Some(surface) => {
                    return Err(format!("browser surface not found: {surface} in {pane}"))
                }
                None => registry
                    .active_surface(pane)
                    .ok_or_else(|| format!("browser pane not found: {pane}"))?,
            }
        };
        crate::ui::workspace_view::materialize_surface(&self.pane_registry, surface)?;
        self.pane_registry
            .borrow()
            .browsers
            .get(&surface)
            .cloned()
            .ok_or_else(|| format!("browser surface not found: {surface} in {pane}"))
    }

    fn pointer_action(
        &self,
        browser: &crate::ui::browser_pane::BrowserPane,
        pane: PaneId,
        surface: Option<SurfaceId>,
        geometry: Result<String, String>,
        action: flowmux_browser::input::PointerAction,
        ack: tokio::sync::oneshot::Sender<Result<BrowserActionResult, String>>,
    ) {
        use flowmux_browser::{input::PointerTarget, scripts};
        let revision = browser.control.revision();
        let registry = self.pane_registry.clone();
        let expected = surface.or_else(|| registry.borrow().active_surface(pane));
        if registry.borrow().active_surface(pane) != expected {
            let _ = ack.send(Err(
                "native pointer input requires the active browser tab".into()
            ));
            return;
        }
        let geometry = match geometry {
            Ok(geometry) => geometry,
            Err(error) => {
                let _ = ack.send(Err(error));
                return;
            }
        };
        let input = browser.clone();
        browser.evaluate_automation_async_js(
            &scripts::prepare_pointer_input(&geometry, action),
            move |prepared| {
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    Err(error) => {
                        let _ = ack.send(Err(error));
                        return;
                    }
                };
                let sent = (|| {
                    input.control.check_revision(revision)?;
                    if registry.borrow().active_surface(pane) != expected {
                        return Err("browser tab changed before pointer input".into());
                    }
                    let point: PointerTarget =
                        serde_json::from_str(&prepared).map_err(|error| error.to_string())?;
                    input.pointer(action, &point)
                })();
                if let Err(error) = sent {
                    input.evaluate_automation_js(scripts::CANCEL_POINTER_INPUT_JS, move |_| {
                        let _ = ack.send(Err(error));
                    });
                    return;
                }
                let reply = native_input_reply(&input, ack);
                input.evaluate_automation_async_js(
                    scripts::FINISH_POINTER_INPUT_JS,
                    move |result| {
                        reply(result.and_then(|value| {
                            if value == "ok" {
                                Ok(BrowserActionResult::Ok)
                            } else {
                                Err(value)
                            }
                        }));
                    },
                );
            },
        );
    }

    pub(super) async fn dispatch_browser_command(&self, cmd: GtkCommand) {
        match cmd {
            GtkCommand::BrowserAction {
                pane,
                surface,
                op,
                ack,
            } => {
                let browser = match self.browser_target(pane, surface) {
                    Ok(browser) => browser,
                    Err(error) => {
                        let _ = ack.send(Err(error));
                        return;
                    }
                };
                let registry = Rc::downgrade(&self.pane_registry);
                let bridge = self.bridge.clone();
                browser.control.set_tab_opener(Rc::new(move |id| {
                    let surface = SurfaceId(id);
                    if let Some(registry) = registry.upgrade() {
                        if let Some(pane) = registry.borrow().pane_for_surface(surface) {
                            return bridge
                                .tx
                                .try_send(GtkCommand::ActivateSurface {
                                    pane,
                                    surface,
                                    ack: None,
                                })
                                .is_ok();
                        }
                    }
                    false
                }));
                let (task, op) = match op {
                    BrowserOp::WithTask { task, op } => (Some(task), *op),
                    op => (None, op),
                };
                let (mut receipt, op) = match op {
                    BrowserOp::Receipt {
                        op,
                        condition,
                        timeout_ms,
                    } => (Some((condition, timeout_ms)), *op),
                    op => (None, op),
                };
                if let BrowserOp::Control { action } = &op {
                    let result = browser.control.control(action, task).map(|mut value| {
                        value["surface_id"] =
                            serde_json::json!(surface
                                .or_else(|| self.pane_registry.borrow().active_surface(pane)));
                        BrowserActionResult::String(value.to_string())
                    });
                    let _ = ack.send(result);
                    return;
                }
                if let BrowserOp::Task(details) = &op {
                    let result = browser
                        .control
                        .update_task(task, details.clone())
                        .map(|value| BrowserActionResult::String(value.to_string()));
                    let _ = ack.send(result);
                    return;
                }
                // Snapshot changes the ref registry; JS predicates can mutate the page.
                let read_only = (op.is_query()
                    && !matches!(
                        op,
                        BrowserOp::Snapshot
                            | BrowserOp::Inspect(_)
                            | BrowserOp::Wait {
                                condition: BrowserWaitCondition::Js(_),
                                ..
                            }
                    ))
                    || matches!(op, BrowserOp::Screenshot { .. });
                let (operation, revision) = if read_only {
                    (None, browser.control.revision())
                } else {
                    match browser.control.admit(
                        task,
                        !matches!(op, BrowserOp::Snapshot | BrowserOp::Inspect(_)),
                    ) {
                        Ok((operation, revision)) => (Some(operation), revision),
                        Err(error) => {
                            let _ = ack.send(Err(error));
                            return;
                        }
                    }
                };
                if let Some((condition, _)) = &mut receipt {
                    if matches!(condition, Some(BrowserWaitCondition::Js(_))) {
                        let _ = ack.send(Err("receipts require a read-only typed outcome; use wait --js explicitly for scripts".into()));
                        return;
                    }
                    if let Some(condition) = condition {
                        if let Err(error) = capture_change_baseline(&browser, condition).await {
                            let _ = ack.send(Err(error));
                            return;
                        }
                        if let Err(error) = browser.control.check_revision(revision) {
                            let _ = ack.send(Err(error));
                            return;
                        }
                    }
                }
                let publish_action = (!read_only
                    && !matches!(op, BrowserOp::Snapshot | BrowserOp::Inspect(_)))
                    || receipt.is_some();
                let action_name = op.capability_name();
                let capture_path = if let BrowserOp::Screenshot { path, .. } = &op {
                    Some(path.clone())
                } else {
                    None
                };
                let upload = match &op {
                    BrowserOp::FileChooser {
                        action: flowmux_ipc::protocol::BrowserFileChooserAction::Select { id, .. },
                    } => Some(id.clone()),
                    _ => None,
                };
                let is_form = matches!(op, BrowserOp::Form(_));
                let publish = browser.control.status()["task_id"].clone();
                let ack = if operation.is_some() || receipt.is_some() || capture_path.is_some() {
                    let (tx, rx) = oneshot::channel();
                    let observed = browser.clone();
                    let receipt = receipt.map(|(condition, timeout_ms)| ActionReceipt {
                        registry: self.pane_registry.clone(),
                        before: browser.control.observation(),
                        started: Instant::now(),
                        id: uuid::Uuid::new_v4(),
                        surface: surface
                            .or_else(|| self.pane_registry.borrow().active_surface(pane)),
                        condition,
                        timeout_ms,
                        revision,
                        mutation: !read_only,
                    });
                    glib::spawn_future_local(async move {
                        let result = rx
                            .await
                            .unwrap_or_else(|_| Err("browser action callback closed".into()));
                        let action_succeeded = result.is_ok();
                        let result = if let Some(receipt) = receipt {
                            Ok(receipt.finish(observed.clone(), result, operation).await)
                        } else {
                            drop(operation);
                            result
                        };
                        if observed.control.status()["task_id"] == publish && !publish.is_null() {
                            let (status, detail) = match &result {
                                _ if !action_succeeded => (
                                    "Action failed",
                                    "Inspect the command error before continuing".to_string(),
                                ),
                                Ok(BrowserActionResult::String(value)) if is_form => {
                                    let data: serde_json::Value =
                                        serde_json::from_str(value).unwrap_or_default();
                                    let data = if data["value"].is_string() {
                                        serde_json::from_str(data["value"].as_str().unwrap())
                                            .unwrap_or_default()
                                    } else {
                                        data
                                    };
                                    let rows = data["fields"].as_array();
                                    let count = rows.map_or(0, |rows| {
                                        rows.iter()
                                            .filter(|row| row["status"] == "verified")
                                            .count()
                                    });
                                    ("Form checked", format!("{count}/{} fields verified. No submit command was sent.", rows.map_or(0, Vec::len)))
                                }
                                Ok(BrowserActionResult::String(value)) => {
                                    let data: serde_json::Value =
                                        serde_json::from_str(value).unwrap_or_default();
                                    match data["status"].as_str() {
                                        Some("input_delivered") => ("Input delivered", "No business outcome was verified.".into()),
                                        Some("outcome_confirmed") => ("Result confirmed", "The requested page condition was observed; server persistence is not independently verified.".into()),
                                        Some("outcome_unknown") => ("Result unconfirmed", "Input may have completed. Inspect the page before retrying.".into()),
                                        Some("control_changed") => ("Control changed", "No further input will be sent by this action.".into()),
                                        Some("dialog_pending") => ("Waiting for dialog", "Inspect the page dialog or take control.".into()),
                                        _ => ("Action returned", "No business outcome was verified.".into()),
                                    }
                                }
                                Err(_) => unreachable!("failed action handled above"),
                                _ => (
                                    "Input delivered",
                                    "No business outcome was verified.".into(),
                                ),
                            };
                            if publish_action {
                                observed.control.result(serde_json::json!({"key":format!("action:{}",uuid::Uuid::new_v4()),"title":format!("{action_name}: {status}"),"detail":detail}));
                            }
                            if let Some(id) = upload {
                                if status == "Result confirmed" {
                                    observed.control.upload_observed(&id);
                                }
                            }
                            if action_succeeded {
                                if let Some(path) = capture_path {
                                    observed.control.result(serde_json::json!({"key":format!("capture:{}",path.display()),"title":"Screenshot saved","detail":path.display().to_string(),"path":path}));
                                }
                            }
                        }
                        let _ = ack.send(result);
                    });
                    tx
                } else {
                    ack
                };
                match op {
                    BrowserOp::WithTask { .. }
                    | BrowserOp::Receipt { .. }
                    | BrowserOp::Control { .. }
                    | BrowserOp::Task(_) => unreachable!("unwrapped browser control"),
                    BrowserOp::Form(fields) => {
                        glib::spawn_future_local(async move {
                            let result = run_browser_form(&browser, fields, revision).await;
                            let _ = ack.send(result);
                        });
                    }
                    BrowserOp::Eval { source } => {
                        browser.evaluate_js(&source, move |result| {
                            let _ = ack.send(result.map(BrowserActionResult::String));
                        });
                    }
                    BrowserOp::Diagnose => {
                        let active = self.pane_registry.borrow().active_surface(pane);
                        let target = surface.or(active);
                        let mut report = browser.diagnostics();
                        let supported = report["native_input_supported"] == true;
                        let visible = browser.focus_widget().is_mapped();
                        let enabled = report["navigation_enabled"] == true;
                        let pending_dialog = browser.has_pending_dialog();
                        let reason = if !supported {
                            report["native_input_reason"].as_str()
                        } else if !enabled {
                            Some("navigation_disabled")
                        } else if target != active {
                            Some("inactive_tab")
                        } else if !visible {
                            Some("tab_not_visible")
                        } else if pending_dialog {
                            Some("dialog_pending")
                        } else {
                            None
                        };
                        let next_action = match reason {
                            Some("requires_x11") => Some("Native keys and pointer input require X11; this session cannot send them"),
                            Some("native_libraries_missing") => Some("Install the X11, XInput and XTest runtime libraries"),
                            Some("navigation_disabled") => Some("Open a new browser or reconnect the expired preview"),
                            Some("inactive_tab" | "tab_not_visible") => Some("Show the target workspace and browser tab before native input"),
                            Some("dialog_pending") => Some("Inspect and answer the pending browser dialog"),
                            Some(_) => Some("Use a platform with a supported browser backend"),
                            None => None,
                        };
                        report["native_input"] = serde_json::json!({
                            "supported": supported, "environment_ready": reason.is_none(),
                            "reason": reason, "next_action": next_action,
                            "requires_visible_active_tab": true,
                            "commands": ["press", "click", "dblclick", "hover", "pointer", "drag"],
                        });
                        report["native_text"] = serde_json::json!({
                            "supported": report["engine"] != "unavailable",
                            "environment_ready": report["engine"] != "unavailable" && enabled && !pending_dialog &&
                                (report["native_text_requires_visible_tab"] != true || (visible && target == active)),
                            "requires_focused_editable_element": true,
                            "requires_visible_active_tab": report["native_text_requires_visible_tab"],
                        });
                        report["control"] = browser.control.status();
                        report["observation"] = browser.control.observation();
                        report["surface_id"] = serde_json::json!(target);
                        report["active"] = serde_json::json!(target == active);
                        report["visible"] = serde_json::json!(visible);
                        report["pending_dialog"] = serde_json::json!(pending_dialog);
                        report["document_capture"] = serde_json::json!({
                            "supported": report["document_capture_supported"],
                            "max_bitmap_bytes": 64 * 1024 * 1024,
                            "may_omit_accelerated_layers": true,
                            "next_action": if report["document_capture_supported"] == true { None }
                                else { Some("Use viewport capture; full-document capture needs macOS 15 or newer WebKit") },
                        });
                        report["cross_origin_frames"] = serde_json::json!(false);
                        report["cookie_import"] = serde_json::json!({"supported":false,
                            "next_action":"Sign in inside the selected Flowmux profile; host cookie insertion is not implemented"});
                        let _ = ack.send(Ok(BrowserActionResult::String(report.to_string())));
                    }
                    BrowserOp::FileChooser { action } => {
                        glib::spawn_future_local(async move {
                            let _ = ack.send(
                                browser
                                    .file_chooser(&action)
                                    .await
                                    .map(BrowserActionResult::String),
                            );
                        });
                    }
                    BrowserOp::Dialog { action } => {
                        glib::spawn_future_local(async move {
                            let _ = ack.send(
                                browser
                                    .dialog(&action)
                                    .await
                                    .map(BrowserActionResult::String),
                            );
                        });
                    }
                    BrowserOp::Download { action } => {
                        let _ =
                            ack.send(browser.download(&action).map(BrowserActionResult::String));
                    }
                    BrowserOp::Media { target, action } => {
                        let action =
                            serde_json::to_string(&action).expect("validated media action");
                        match ref_script(&browser, &target, |selector| {
                            flowmux_browser::scripts::media(selector, &action)
                        }) {
                            Ok(script) => browser.evaluate_automation_async_js(
                                &format!("return await {script};"),
                                move |result| {
                                    let _ = ack.send(result.map(BrowserActionResult::String));
                                },
                            ),
                            Err(error) => {
                                let _ = ack.send(Err(error));
                            }
                        }
                    }
                    BrowserOp::EvalAsync { body, timeout_ms } => {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        browser.evaluate_async_js(&body, move |result| {
                            let _ = tx.send(result);
                        });
                        glib::spawn_future_local(async move {
                            let result = match glib::future_with_timeout(
                                std::time::Duration::from_millis(timeout_ms.into()),
                                rx,
                            )
                            .await
                            {
                                Ok(Ok(result)) => result.map(BrowserActionResult::String),
                                Ok(Err(_)) => Err("browser closed before async completion".into()),
                                Err(_) => Err(
                                    "async script timed out; its page effects may still complete"
                                        .into(),
                                ),
                            };
                            let _ = ack.send(result);
                        });
                    }
                    BrowserOp::Snapshot | BrowserOp::Inspect(_) => {
                        let options = match op {
                            BrowserOp::Inspect(options) => serde_json::to_value(options).unwrap(),
                            _ => serde_json::json!({}),
                        };
                        let script = flowmux_browser::scripts::snapshot_with_options(&options);
                        // Run the non-mutating snapshot script, then mirror
                        // its `refs` map into the pane's server-side
                        // `RefStore` so subsequent `click e3` / `text e3`
                        // calls resolve. The page DOM is never stamped.
                        let browser2 = browser.clone();
                        let cell = std::cell::Cell::new(Some(ack));
                        browser.evaluate_automation_js(&script, move |result| {
                            if let Some(ack) = cell.take() {
                                // An expired query must not replace a newer
                                // snapshot's references when its callback arrives.
                                if ack.is_closed() {
                                    return;
                                }
                                let mapped = match result {
                                    Ok(json) => {
                                        if let Ok(mut snap) =
                                            serde_json::from_str::<flowmux_browser::DomSnapshot>(
                                                &json,
                                            )
                                        {
                                            snap.page.surface_id = Some(
                                                uuid::Uuid::from_u128(browser2.ref_scope.0)
                                                    .to_string(),
                                            );
                                            browser2
                                                .refs
                                                .borrow_mut()
                                                .populate_from_snapshot(browser2.ref_scope, &snap);
                                            serde_json::to_string(&snap)
                                                .map(BrowserActionResult::String)
                                                .map_err(|error| error.to_string())
                                        } else {
                                            Err("browser snapshot could not be decoded".into())
                                        }
                                    }
                                    Err(e) => Err(e),
                                };
                                let _ = ack.send(mapped);
                            }
                        });
                    }
                    BrowserOp::Navigate { url } => {
                        // Any navigation invalidates the current snapshot's
                        // refs — drop them so a stale `eN` can't resolve to
                        // an element on the old page.
                        browser.refs.borrow_mut().clear(browser.ref_scope);
                        browser.load_uri(&url);
                        let _ = ack.send(Ok(BrowserActionResult::Ok));
                    }
                    BrowserOp::Back => {
                        let moved = browser.go_back();
                        if moved {
                            browser.refs.borrow_mut().clear(browser.ref_scope);
                        }
                        let _ = ack.send(Ok(BrowserActionResult::Bool(moved)));
                    }
                    BrowserOp::Forward => {
                        let moved = browser.go_forward();
                        if moved {
                            browser.refs.borrow_mut().clear(browser.ref_scope);
                        }
                        let _ = ack.send(Ok(BrowserActionResult::Bool(moved)));
                    }
                    BrowserOp::Reload => {
                        browser.refs.borrow_mut().clear(browser.ref_scope);
                        browser.reload();
                        let _ = ack.send(Ok(BrowserActionResult::Ok));
                    }
                    BrowserOp::Url => {
                        let value = browser.current_url();
                        let _ = ack.send(Ok(BrowserActionResult::String(value)));
                    }
                    BrowserOp::Title => {
                        let value = browser.current_title();
                        let _ = ack.send(Ok(BrowserActionResult::String(value)));
                    }
                    BrowserOp::Drag { from, to } => self.pointer_action(
                        &browser,
                        pane,
                        surface,
                        Ok(flowmux_browser::scripts::drag_target(from, to)),
                        flowmux_browser::input::PointerAction::Drag {
                            to_x: to[0],
                            to_y: to[1],
                        },
                        ack,
                    ),
                    BrowserOp::Pointer { action, x, y } => {
                        use flowmux_browser::input::PointerAction;
                        use flowmux_ipc::protocol::BrowserPointerAction;
                        let action = match action {
                            BrowserPointerAction::Click => PointerAction::Click,
                            BrowserPointerAction::Dblclick => PointerAction::DoubleClick,
                            BrowserPointerAction::Hover => PointerAction::Hover,
                        };
                        self.pointer_action(
                            &browser,
                            pane,
                            surface,
                            Ok(flowmux_browser::scripts::coordinate_target(
                                x,
                                y,
                                matches!(action, PointerAction::Hover),
                            )),
                            action,
                            ack,
                        );
                    }
                    BrowserOp::Click { target } => self.pointer_action(
                        &browser,
                        pane,
                        surface,
                        ref_script(&browser, &target, |selector| {
                            flowmux_browser::scripts::pointer_target(selector, false)
                        }),
                        flowmux_browser::input::PointerAction::Click,
                        ack,
                    ),
                    BrowserOp::Fill { target, value } => {
                        match ref_script(&browser, &target, |sel| {
                            flowmux_browser::scripts::fill_by_selector(sel, &value)
                        }) {
                            Ok(js) => run_prepared_element_action(&browser, &target, true, js, ack),
                            Err(e) => {
                                let _ = ack.send(Err(e));
                            }
                        }
                    }
                    BrowserOp::Select { target, value } => {
                        match ref_script(&browser, &target, |sel| {
                            flowmux_browser::scripts::select_option_by_selector(sel, &value)
                        }) {
                            Ok(js) => {
                                run_prepared_element_action(&browser, &target, false, js, ack)
                            }
                            Err(e) => {
                                let _ = ack.send(Err(e));
                            }
                        }
                    }
                    BrowserOp::Scroll { target, x, y } => {
                        match ref_script(&browser, &target, |sel| {
                            flowmux_browser::scripts::scroll_by_selector(sel, x, y)
                        }) {
                            Ok(js) => run_browser_js(&browser, &js, ack, true),
                            Err(e) => {
                                let _ = ack.send(Err(e));
                            }
                        }
                    }
                    BrowserOp::Type { text } => {
                        if text.is_empty() {
                            let _ = ack.send(Ok(BrowserActionResult::Ok));
                            return;
                        }
                        let target = browser.clone();
                        browser.evaluate_automation_js(
                            &flowmux_browser::scripts::prepare_text_input(&text),
                            move |prepared| {
                                match prepared {
                                    Ok(value) if value == "ok" => {}
                                    result => {
                                        let _ = ack.send(Err(result.unwrap_or_else(|error| error)));
                                        return;
                                    }
                                }
                                let reply = native_input_reply(&target, ack);
                                let inserted = target
                                    .control
                                    .check_revision(revision)
                                    .and_then(|_| target.insert_text(&text));
                                target.evaluate_automation_js(
                                    &flowmux_browser::scripts::finish_text_input(),
                                    move |result| {
                                        let result = inserted.and(result).and_then(|value| {
                                            if value == "ok" {
                                                Ok(BrowserActionResult::Ok)
                                            } else {
                                                Err(value)
                                            }
                                        });
                                        reply(result);
                                    },
                                );
                            },
                        );
                    }
                    BrowserOp::Press { key } => {
                        let registry = self.pane_registry.clone();
                        let expected_surface =
                            surface.or_else(|| registry.borrow().active_surface(pane));
                        let stroke = match flowmux_browser::input::KeyStroke::parse(&key) {
                            Ok(stroke) => stroke,
                            Err(error) => {
                                let _ = ack.send(Err(error));
                                return;
                            }
                        };
                        let target = browser.clone();
                        browser.evaluate_automation_js(&flowmux_browser::scripts::prepare_key_input(&stroke), move |prepared| {
                            match prepared {
                                Ok(value) if value == "ok" => {}
                                result => { let _ = ack.send(Err(result.unwrap_or_else(|error| error))); return; }
                            }
                            let sent = if let Err(error) = target.control.check_revision(revision) {
                                Err(error)
                            } else if registry.borrow().active_surface(pane) != expected_surface {
                                Err("native key input requires the active browser tab".into())
                            } else {
                                target.press_key(&stroke)
                            };
                            if let Err(error) = sent {
                                target.evaluate_automation_js("(function(){const state=globalThis.__flowmuxInput; if(state){window.removeEventListener('keyup', state.listener, true); window.removeEventListener('keydown', state.keydown, true); delete globalThis.__flowmuxInput;} return 'ok';})()", move |_| { let _ = ack.send(Err(error)); });
                                return;
                            }
                            #[cfg(target_os = "macos")]
                            let finished = target.clone();
                            let reply = native_input_reply(&target, ack);
                            target.evaluate_automation_async_js(flowmux_browser::scripts::FINISH_KEY_INPUT_JS, move |result| {
                                let result = result.and_then(|value| {
                                    if value != "ok" && value != "prevented" { return Err(value); }
                                    #[cfg(target_os = "macos")]
                                    if value == "ok" { finished.finish_key_input(&stroke)?; }
                                    Ok(BrowserActionResult::Ok)
                                });
                                reply(result);
                            });
                        });
                    }
                    BrowserOp::Text { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::text_of_selector,
                    ) {
                        Ok(js) => run_browser_js(&browser, &js, ack, false),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Value { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::value_of_selector,
                    ) {
                        Ok(js) => run_browser_js(&browser, &js, ack, false),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Attr { target, name } => match ref_script(&browser, &target, |sel| {
                        flowmux_browser::scripts::attr_of_selector(sel, &name)
                    }) {
                        Ok(js) => run_browser_js(&browser, &js, ack, false),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },

                    BrowserOp::Wait {
                        mut condition,
                        timeout_ms,
                        poll_ms,
                    } => {
                        if let Err(error) = capture_change_baseline(&browser, &mut condition).await
                        {
                            let _ = ack.send(Err(error));
                            return;
                        }
                        let cancellation = matches!(
                            condition,
                            BrowserWaitCondition::Js(_) | BrowserWaitCondition::Element { .. }
                        )
                        .then_some(revision);
                        run_browser_wait(
                            browser.clone(),
                            self.pane_registry.clone(),
                            condition,
                            timeout_ms,
                            poll_ms,
                            cancellation,
                            ack,
                        );
                    }
                    BrowserOp::Screenshot {
                        path,
                        full_page,
                        element,
                    } => run_browser_screenshot(&browser, path, full_page, element, ack),
                    BrowserOp::DblClick { target } => self.pointer_action(
                        &browser,
                        pane,
                        surface,
                        ref_script(&browser, &target, |selector| {
                            flowmux_browser::scripts::pointer_target(selector, false)
                        }),
                        flowmux_browser::input::PointerAction::DoubleClick,
                        ack,
                    ),
                    BrowserOp::Hover { target } => self.pointer_action(
                        &browser,
                        pane,
                        surface,
                        ref_script(&browser, &target, |selector| {
                            flowmux_browser::scripts::pointer_target(selector, true)
                        }),
                        flowmux_browser::input::PointerAction::Hover,
                        ack,
                    ),
                    BrowserOp::Focus { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::focus_by_selector,
                    ) {
                        Ok(js) => run_browser_js(&browser, &js, ack, true),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Blur { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::blur_by_selector,
                    ) {
                        Ok(js) => run_browser_js(&browser, &js, ack, true),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Check { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::check_by_selector,
                    ) {
                        Ok(js) => run_prepared_element_action(&browser, &target, false, js, ack),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Uncheck { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::uncheck_by_selector,
                    ) {
                        Ok(js) => run_prepared_element_action(&browser, &target, false, js, ack),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::IsVisible { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::is_visible_selector,
                    ) {
                        Ok(js) => run_browser_js_bool(&browser, &js, ack),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::IsEnabled { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::is_enabled_selector,
                    ) {
                        Ok(js) => run_browser_js_bool(&browser, &js, ack),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::IsChecked { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::is_checked_selector,
                    ) {
                        Ok(js) => run_browser_js_bool(&browser, &js, ack),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Count { selector } => {
                        // Count takes a raw selector (not a ref) — the
                        // agent might want to know how many `.row`
                        // elements exist before navigating into them.
                        run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::count_selector(&selector),
                            ack,
                            false,
                        );
                    }
                }
            }
            GtkCommand::BrowserOpenSplit {
                target_pane,
                url,
                direction,
                ack,
            } => {
                let Some(target) = target_pane.or_else(|| self.focused_pane.get()) else {
                    let _ = ack.send(Err("no target pane focused".into()));
                    return;
                };

                // cmux preferredBrowserTargetPane policy: if the source
                // pane already has a browser leaf on its right side,
                // append a new tab there instead of creating a new
                // split. Falls back to a fresh vertical split when no
                // such right sibling exists.
                if let Some(reuse_target) = self.store.find_right_sibling_browser_leaf(target).await
                {
                    match self
                        .store
                        .add_browser_surface_to_pane(reuse_target, url.clone())
                        .await
                    {
                        Some((workspace, surface_id)) => {
                            // Incremental attach: only the right-sibling
                            // browser pane gets a new tab. Other panes —
                            // including the terminal that called us — keep
                            // their PTY child and browser navigation
                            // state. Falling back to rerender_workspace
                            // here would kill claude/codex running in the
                            // caller's terminal (regression #pane-reset).
                            if let Err(error) = self
                                .attach_or_rerender_surface(
                                    workspace,
                                    reuse_target,
                                    surface_id,
                                    None,
                                )
                                .await
                            {
                                self.store.close_surface(reuse_target, surface_id).await;
                                let _ = ack.send(Err(error));
                                return;
                            }
                            let _ = ack.send(Ok(BrowserOpenOutcome {
                                pane: reuse_target,
                                placement_strategy: PlacementStrategy::ReuseRightSibling,
                            }));
                            return;
                        }
                        None => {
                            // The right-sibling pane disappeared between
                            // discovery and update — fall through to the
                            // split path so the agent still gets a pane.
                            tracing::debug!(
                                %reuse_target,
                                "right-sibling browser leaf disappeared; falling back to split"
                            );
                        }
                    }
                }

                match self
                    .store
                    .split_pane_with_browser(target, direction, url)
                    .await
                {
                    None => {
                        let _ = ack.send(Err(format!("pane not found: {target}")));
                    }
                    Some((workspace, new_pane)) => {
                        // Incremental split: reparent the source pane's
                        // existing frame into a fresh Paned and put a new
                        // BrowserPane in the sibling slot. Other panes
                        // (including the terminal we are called from)
                        // keep their state. Same regression as above
                        // applied to the split path.
                        let result = self
                            .apply_split_incremental_or_rerender(
                                workspace, target, new_pane, direction,
                            )
                            .await
                            .map(|()| BrowserOpenOutcome {
                                pane: new_pane,
                                placement_strategy: PlacementStrategy::SplitRight,
                            });
                        let _ = ack.send(result);
                    }
                }
            }
            GtkCommand::OpenUrlInBrowserTab { pane, url } => {
                // Open a Ctrl-clicked terminal URL in a new browser tab in the
                // same pane. BrowserPane::build receives the URL as initial_url
                // and immediately load_uri's it, so no extra navigate command is
                // needed. If surface creation fails, for example because the pane
                // disappeared right after the click, ignore it quietly.
                if let Some((ws_id, surface_id)) =
                    self.store.add_browser_surface_to_pane(pane, url).await
                {
                    if let Err(error) = self
                        .attach_or_rerender_surface(ws_id, pane, surface_id, None)
                        .await
                    {
                        self.store.close_surface(pane, surface_id).await;
                        self.clipboard_toast.show_with_message(&error);
                    }
                }
            }
            GtkCommand::OpenBrowserPopup { pane, surface } => {
                let browser = self
                    .pane_registry
                    .borrow_mut()
                    .pending_browser_popups
                    .remove(&surface);
                if let Some(browser) = browser {
                    let url = browser.current_url();
                    if let Some((workspace, _)) = self
                        .store
                        .add_browser_surface_to_pane_with_id(pane, url, surface)
                        .await
                    {
                        self.pane_registry
                            .borrow_mut()
                            .pending_browser_reuse
                            .insert(surface, browser);
                        if let Err(error) = self
                            .attach_or_rerender_surface(workspace, pane, surface, None)
                            .await
                        {
                            if let Some(browser) = self
                                .pane_registry
                                .borrow_mut()
                                .pending_browser_reuse
                                .remove(&surface)
                            {
                                browser.prepare_for_close();
                            }
                            self.store.close_surface(pane, surface).await;
                            self.clipboard_toast.show_with_message(&error);
                        }
                    } else {
                        browser.prepare_for_close();
                    }
                }
            }

            other => unreachable!("browser router got a non-browser command: {other:?}"),
        }
    }
}

fn native_input_reply(
    browser: &crate::ui::browser_pane::BrowserPane,
    ack: oneshot::Sender<Result<BrowserActionResult, String>>,
) -> impl FnOnce(Result<BrowserActionResult, String>) {
    let reply = Rc::new(RefCell::new(Some(ack)));
    let dialog_reply = reply.clone();
    let pending_input = browser.clone();
    // A page dialog pauses JavaScript; its native presentation also completes the action.
    glib::timeout_add_local(Duration::from_millis(10), move || {
        let mut reply = dialog_reply.borrow_mut();
        if pending_input.has_pending_dialog() {
            if let Some(ack) = reply.take() {
                let _ = ack.send(Ok(BrowserActionResult::Ok));
            }
        }
        if reply.is_some() {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
    move |result| {
        if let Some(ack) = reply.borrow_mut().take() {
            let _ = ack.send(result);
        }
    }
}

fn run_prepared_element_action(
    browser: &crate::ui::browser_pane::BrowserPane,
    target: &str,
    editable: bool,
    action: String,
    ack: oneshot::Sender<Result<BrowserActionResult, String>>,
) {
    let readiness = match ref_script(browser, target, |selector| {
        flowmux_browser::scripts::input_readiness(selector, editable)
    }) {
        Ok(script) => script,
        Err(error) => {
            let _ = ack.send(Err(error));
            return;
        }
    };
    let browser = browser.clone();
    let revision = browser.control.revision();
    glib::spawn_future_local(async move {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if ack.is_closed() {
                return;
            }
            if let Err(error) = browser.control.check_revision(revision) {
                let _ = ack.send(Err(error));
                return;
            }
            let (tx, rx) = oneshot::channel();
            browser.evaluate_automation_js(&readiness, move |result| {
                let _ = tx.send(result);
            });
            let result =
                glib::future_with_timeout(deadline.saturating_duration_since(Instant::now()), rx)
                    .await;
            match result {
                Ok(Ok(Ok(value))) if value == "ok" => break,
                Ok(Ok(Err(error))) => {
                    let _ = ack.send(Err(error));
                    return;
                }
                Ok(Ok(Ok(value))) if Instant::now() >= deadline => {
                    let _ = ack.send(Err(value));
                    return;
                }
                Ok(Ok(Ok(_))) => glib::timeout_future(Duration::from_millis(50)).await,
                _ => {
                    let _ = ack.send(Err("element readiness timed out before input".into()));
                    return;
                }
            }
        }
        if let Err(error) = browser.control.check_revision(revision) {
            let _ = ack.send(Err(error));
            return;
        }
        if ack.is_closed() {
            return;
        }
        // The action still validates the original ref and executes exactly once.
        run_browser_js(&browser, &action, ack, true);
    });
}

async fn capture_change_baseline(
    browser: &crate::ui::browser_pane::BrowserPane,
    condition: &mut BrowserWaitCondition,
) -> Result<(), String> {
    if let BrowserWaitCondition::Element { target, expect } = condition {
        use flowmux_ipc::protocol::BrowserElementExpectation as Expect;
        let (property, before) = match expect {
            Expect::TextChanged { before } => ("innerText", before),
            Expect::ValueChanged { before } => ("value", before),
            _ => return element_expectation_js(browser, target, expect).map(|_| ()),
        };
        let js = ref_script(browser, target, |_| {
            format!("(() => {{ if (el.matches('input[type=password]')) throw new Error('password values are redacted'); if (typeof el.{property} !== 'string') throw new Error('change target has no text value'); return el.{property}; }})()")
        })?;
        let (tx, rx) = oneshot::channel();
        browser.evaluate_automation_js(&js, move |result| {
            let _ = tx.send(result);
        });
        *before = Some(
            glib::future_with_timeout(Duration::from_secs(5), rx)
                .await
                .map_err(|_| "change baseline timed out")?
                .map_err(|_| "change baseline closed")??,
        );
        return Ok(());
    }
    if let BrowserWaitCondition::PopupReady { after_sequence, .. } = condition {
        *after_sequence = browser.control.observation()["event_sequence"].as_u64();
    }
    let property = if matches!(condition, BrowserWaitCondition::ValueChanged { .. }) {
        "value"
    } else {
        "innerText"
    };
    if let BrowserWaitCondition::TextChanged { selector, before }
    | BrowserWaitCondition::ValueChanged { selector, before } = condition
    {
        let (tx, rx) = oneshot::channel();
        let js = format!("(() => {{ const el = document.querySelector({}); if (!el) throw new Error('change target not found'); if ('{property}' === 'value' && el.matches('input[type=password]')) throw new Error('password values are redacted'); if (typeof el.{property} !== 'string') throw new Error('change target has no text value'); return el.{property}; }})()", serde_json::to_string(selector).unwrap());
        browser.evaluate_automation_js(&js, move |result| {
            let _ = tx.send(result);
        });
        *before = Some(
            glib::future_with_timeout(Duration::from_secs(5), rx)
                .await
                .map_err(|_| "change baseline timed out")?
                .map_err(|_| "change baseline closed")??,
        );
    }
    Ok(())
}

struct ActionReceipt {
    registry: Rc<RefCell<PaneRegistry>>,
    before: serde_json::Value,
    started: Instant,
    id: uuid::Uuid,
    surface: Option<SurfaceId>,
    condition: Option<BrowserWaitCondition>,
    timeout_ms: u64,
    revision: u64,
    mutation: bool,
}

impl ActionReceipt {
    async fn finish(
        self,
        browser: crate::ui::browser_pane::BrowserPane,
        result: Result<BrowserActionResult, String>,
        operation: Option<crate::ui::browser_control::Operation>,
    ) -> BrowserActionResult {
        let mut outcome_met = None;
        let mut outcome_error = None;
        if result.is_ok() && !browser.has_pending_dialog() {
            if let Some(condition) = self.condition {
                let (tx, mut rx) = oneshot::channel();
                run_browser_wait(
                    browser.clone(),
                    self.registry.clone(),
                    condition,
                    self.timeout_ms,
                    50,
                    Some(self.revision),
                    tx,
                );
                let deadline = Instant::now() + Duration::from_millis(self.timeout_ms);
                loop {
                    if browser.has_pending_dialog() {
                        break;
                    }
                    if let Err(error) = browser.control.check_revision(self.revision) {
                        outcome_error = Some(error);
                        break;
                    }
                    if Instant::now() >= deadline {
                        outcome_met = Some(false);
                        break;
                    }
                    match glib::future_with_timeout(Duration::from_millis(50), &mut rx).await {
                        Ok(Ok(Ok(BrowserActionResult::Bool(value)))) => {
                            outcome_met = Some(value);
                            break;
                        }
                        Ok(Ok(Err(error))) => {
                            outcome_error = Some(error);
                            break;
                        }
                        Ok(_) => {
                            outcome_error = Some("outcome observation closed".into());
                            break;
                        }
                        Err(_) => {}
                    }
                }
            }
        }
        let after = browser.control.observation();
        let sequence = self.before["event_sequence"].as_u64().unwrap_or(0);
        let events: Vec<_> = after["events"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|event| event["sequence"].as_u64().unwrap_or(0) > sequence)
            .cloned()
            .collect();
        let pending_dialog = browser.has_pending_dialog();
        let control_changed = browser.control.check_revision(self.revision).is_err();
        let status = if pending_dialog {
            "dialog_pending"
        } else if control_changed {
            "control_changed"
        } else if result.is_err() || outcome_error.is_some() || outcome_met == Some(false) {
            "outcome_unknown"
        } else if outcome_met == Some(true) {
            "outcome_confirmed"
        } else if events.iter().any(|event| event["kind"] == "download") {
            "download_started"
        } else if events.iter().any(|event| event["kind"] == "popup") {
            "popup_created"
        } else if self.mutation {
            "input_delivered"
        } else {
            "action_returned"
        };
        let (value, error) = match result {
            Ok(BrowserActionResult::Ok) => (serde_json::Value::Null, None),
            Ok(BrowserActionResult::Bool(value)) => (serde_json::json!(value), None),
            Ok(BrowserActionResult::String(value)) => (serde_json::json!(value), None),
            Err(error) => (serde_json::Value::Null, Some(error)),
        };
        drop(operation);
        BrowserActionResult::String(serde_json::json!({
            "action_id":self.id,"surface_id":self.surface,"status":status,
            "document_before":self.before["document_generation"],"document_after":after["document_generation"],
            "events":events,"events_truncated":after["event_sequence"].as_u64().unwrap_or(0).saturating_sub(sequence)>64,
            "elapsed_ms":self.started.elapsed().as_millis(),"outcome_met":outcome_met,
            "value":value,"error":error,"outcome_error":outcome_error,
            "side_effects_possible":self.mutation,"automatic_retry":false,
            "control":browser.control.status(),
        }).to_string())
    }
}

async fn run_browser_form(
    browser: &crate::ui::browser_pane::BrowserPane,
    fields: Vec<flowmux_ipc::protocol::BrowserFormField>,
    revision: u64,
) -> Result<BrowserActionResult, String> {
    use flowmux_ipc::protocol::BrowserFormAction as Action;
    if fields.is_empty() || fields.len() > 20 {
        return Err("form requires between 1 and 20 fields".into());
    }
    let mut targets = std::collections::HashSet::new();
    for field in &fields {
        if !targets.insert(field.target.trim_start_matches('@')) {
            return Err("form targets must be unique".into());
        }
        ref_script(browser, &field.target, |_| "true".into())?;
    }
    let generation = browser.control.observation()["document_generation"].clone();
    let mut results = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut stopped = false;
    for field in fields {
        if stopped
            || Instant::now() >= deadline
            || browser.control.check_revision(revision).is_err()
            || browser.control.observation()["document_generation"] != generation
            || browser.has_pending_dialog()
        {
            stopped = true;
            results.push(serde_json::json!({"target":field.target,"status":"not_run"}));
            continue;
        }
        let script = ref_script(browser, &field.target, |selector| {
            let action = match &field.action {
                Action::Fill(value) => flowmux_browser::scripts::fill_by_selector(selector, value),
                Action::Select(value) => {
                    flowmux_browser::scripts::select_option_by_selector(selector, value)
                }
                Action::Check(checked) => {
                    if *checked {
                        flowmux_browser::scripts::check_by_selector(selector)
                    } else {
                        flowmux_browser::scripts::uncheck_by_selector(selector)
                    }
                }
            };
            format!("(() => {{ if (el.matches('input[type=password]')) throw new Error('use manual password entry'); return {action}; }})()")
        });
        let script = match script {
            Ok(script) => script,
            Err(error) => {
                results.push(
                    serde_json::json!({"target":field.target,"status":"failed","error":error}),
                );
                continue;
            }
        };
        let (tx, rx) = oneshot::channel();
        run_prepared_element_action(
            browser,
            &field.target,
            matches!(field.action, Action::Fill(_)),
            script,
            tx,
        );
        let result = match glib::future_with_timeout(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(8)),
            rx,
        )
        .await
        {
            Ok(Ok(result)) => result,
            _ => {
                stopped = true;
                Err(
                    "form input timed out; input may still complete, remaining fields were not run"
                        .into(),
                )
            }
        };
        let validation = match result {
            Err(error) => Err(error),
            Ok(_) => async {
                let script = ref_script(browser, &field.target, |_| {
                    let expected = match &field.action {
                        Action::Fill(value) => format!("(el.isContentEditable ? el.textContent : el.value) === {}", serde_json::to_string(value).unwrap()),
                        Action::Select(value) => format!("el.value === {0} || Array.from(el.selectedOptions).some(option => option.textContent.trim() === {0})", serde_json::to_string(value).unwrap()),
                        Action::Check(value) => format!("el.checked === {value}"),
                    };
                    format!("JSON.stringify({{matched: {expected}, valid: !el.matches(':invalid') && el.getAttribute('aria-invalid') !== 'true', message: el.validationMessage || ''}})")
                })?;
                let (tx, rx) = oneshot::channel();
                browser.evaluate_automation_js(&script, move |result| {
                    let _ = tx.send(result);
                });
                match glib::future_with_timeout(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .min(Duration::from_secs(5)),
                    rx,
                )
                .await
                {
                    Ok(Ok(Ok(value))) => serde_json::from_str::<serde_json::Value>(&value)
                        .map_err(|error| error.to_string()),
                    Ok(Ok(Err(error))) => Err(error),
                    _ => Err("form validation timed out; input may have completed".into()),
                }
            }.await
        };
        let row = match validation {
            Ok(value) => {
                serde_json::json!({"target":field.target,"status":if value["matched"] == true && value["valid"] == true {"verified"}else{"invalid"},"validation":value})
            }
            Err(error) => {
                serde_json::json!({"target":field.target,"status":"failed","error":error})
            }
        };
        results.push(row);
    }
    Ok(BrowserActionResult::String(
        serde_json::json!({"fields":results,"submit_command_sent":false,"automatic_retry":false})
            .to_string(),
    ))
}
