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
        target: &str,
        action: flowmux_browser::input::PointerAction,
        ack: tokio::sync::oneshot::Sender<Result<BrowserActionResult, String>>,
    ) {
        use flowmux_browser::{
            input::{PointerAction, PointerTarget},
            scripts,
        };
        let registry = self.pane_registry.clone();
        let expected = surface.or_else(|| registry.borrow().active_surface(pane));
        if registry.borrow().active_surface(pane) != expected {
            let _ = ack.send(Err(
                "native pointer input requires the active browser tab".into()
            ));
            return;
        }
        let geometry = match ref_script(browser, target, |selector| {
            scripts::pointer_target(selector, matches!(action, PointerAction::Hover))
        }) {
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
                input.evaluate_automation_async_js(
                    scripts::FINISH_POINTER_INPUT_JS,
                    move |result| {
                        let _ = ack.send(result.and_then(|value| {
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
            GtkCommand::BrowserEval {
                pane,
                surface,
                source,
                ack,
            } => {
                match self.browser_target(pane, surface) {
                    Err(error) => {
                        let _ = ack.send(Err(error));
                    }
                    Ok(browser) => {
                        // evaluate_js is callback-style; bridge it to the ack.
                        let cell = std::cell::Cell::new(Some(ack));
                        browser.evaluate_js(&source, move |result| {
                            if let Some(ack) = cell.take() {
                                let _ = ack.send(result);
                            }
                        });
                    }
                }
            }
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
                match op {
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
                    BrowserOp::Snapshot => {
                        // Run the non-mutating snapshot script, then mirror
                        // its `refs` map into the pane's server-side
                        // `RefStore` so subsequent `click e3` / `text e3`
                        // calls resolve. The page DOM is never stamped.
                        let browser2 = browser.clone();
                        let cell = std::cell::Cell::new(Some(ack));
                        browser.evaluate_automation_js(
                            flowmux_browser::scripts::SNAPSHOT_JS,
                            move |result| {
                                if let Some(ack) = cell.take() {
                                    // An expired query must not replace a newer
                                    // snapshot's references when its callback arrives.
                                    if ack.is_closed() {
                                        return;
                                    }
                                    let mapped = match result {
                                        Ok(json) => {
                                            if let Ok(mut snap) = serde_json::from_str::<
                                                flowmux_browser::DomSnapshot,
                                            >(
                                                &json
                                            ) {
                                                snap.page.surface_id = Some(
                                                    uuid::Uuid::from_u128(browser2.ref_scope.0)
                                                        .to_string(),
                                                );
                                                browser2.refs.borrow_mut().populate_from_snapshot(
                                                    browser2.ref_scope,
                                                    &snap,
                                                );
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
                            },
                        );
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
                    BrowserOp::Click { target } => self.pointer_action(
                        &browser,
                        pane,
                        surface,
                        &target,
                        flowmux_browser::input::PointerAction::Click,
                        ack,
                    ),
                    BrowserOp::Fill { target, value } => {
                        match ref_script(&browser, &target, |sel| {
                            flowmux_browser::scripts::fill_by_selector(sel, &value)
                        }) {
                            Ok(js) => run_browser_js(&browser, &js, ack, true),
                            Err(e) => {
                                let _ = ack.send(Err(e));
                            }
                        }
                    }
                    BrowserOp::Select { target, value } => {
                        match ref_script(&browser, &target, |sel| {
                            flowmux_browser::scripts::select_option_by_selector(sel, &value)
                        }) {
                            Ok(js) => run_browser_js(&browser, &js, ack, true),
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
                                let inserted = target.insert_text(&text);
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
                                        let _ = ack.send(result);
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
                            let sent = if registry.borrow().active_surface(pane) != expected_surface {
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
                            target.evaluate_automation_async_js(flowmux_browser::scripts::FINISH_KEY_INPUT_JS, move |result| {
                                let result = result.and_then(|value| {
                                    if value != "ok" && value != "prevented" { return Err(value); }
                                    #[cfg(target_os = "macos")]
                                    if value == "ok" { finished.finish_key_input(&stroke)?; }
                                    Ok(BrowserActionResult::Ok)
                                });
                                let _ = ack.send(result);
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
                        condition,
                        timeout_ms,
                        poll_ms,
                    } => run_browser_wait(browser.clone(), condition, timeout_ms, poll_ms, ack),
                    BrowserOp::Screenshot {
                        path,
                        full_page,
                        element,
                    } => run_browser_screenshot(&browser, path, full_page, element, ack),
                    BrowserOp::DblClick { target } => self.pointer_action(
                        &browser,
                        pane,
                        surface,
                        &target,
                        flowmux_browser::input::PointerAction::DoubleClick,
                        ack,
                    ),
                    BrowserOp::Hover { target } => self.pointer_action(
                        &browser,
                        pane,
                        surface,
                        &target,
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
                        Ok(js) => run_browser_js(&browser, &js, ack, true),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Uncheck { target } => match ref_script(
                        &browser,
                        &target,
                        flowmux_browser::scripts::uncheck_by_selector,
                    ) {
                        Ok(js) => run_browser_js(&browser, &js, ack, true),
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
            GtkCommand::InjectCookies { cookies, ack } => {
                let result = inject_cookies_into_webkit(&cookies);
                let _ = ack.send(result);
            }
            other => unreachable!("browser router got a non-browser command: {other:?}"),
        }
    }
}
