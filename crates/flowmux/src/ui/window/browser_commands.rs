// SPDX-License-Identifier: GPL-3.0-or-later
//! Browser command dispatch on the GTK main thread.

use super::*;

impl WindowController {
    pub(super) async fn dispatch_browser_command(&self, cmd: GtkCommand) {
        match cmd {
            GtkCommand::BrowserEval { pane, source, ack } => {
                let registry = self.pane_registry.borrow();
                match registry.active_browser(pane) {
                    None => {
                        let _ = ack.send(Err(format!("browser pane not found: {pane}")));
                    }
                    Some(browser) => {
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
            GtkCommand::BrowserAction { pane, op, ack } => {
                let browser = self.pane_registry.borrow().active_browser(pane).cloned();
                let Some(browser) = browser else {
                    let _ = ack.send(Err(format!("browser pane not found: {pane}")));
                    return;
                };
                match op {
                    BrowserOp::Snapshot => {
                        // Run the non-mutating snapshot script, then mirror
                        // its `refs` map into the pane's server-side
                        // `RefStore` so subsequent `click e3` / `text e3`
                        // calls resolve. The page DOM is never stamped.
                        let browser2 = browser.clone();
                        let cell = std::cell::Cell::new(Some(ack));
                        browser.evaluate_js(flowmux_browser::scripts::SNAPSHOT_JS, move |result| {
                            if let Some(ack) = cell.take() {
                                let mapped = match result {
                                    Ok(json) => {
                                        if let Ok(snap) =
                                            serde_json::from_str::<flowmux_browser::DomSnapshot>(
                                                &json,
                                            )
                                        {
                                            browser2
                                                .refs
                                                .borrow_mut()
                                                .populate_from_snapshot(browser2.ref_scope, &snap);
                                        }
                                        Ok(BrowserActionResult::String(json))
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
                    BrowserOp::Click { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::click_by_selector(&sel),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Fill { target, value } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::fill_by_selector(&sel, &value),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Select { target, value } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::select_option_by_selector(&sel, &value),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Scroll { target, x, y } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::scroll_by_selector(&sel, x, y),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Type { text } => {
                        let js = flowmux_browser::scripts::type_keys(&text);
                        run_browser_js(&browser, &js, ack, true);
                    }
                    BrowserOp::Press { key } => {
                        let js = flowmux_browser::scripts::press_key(&key);
                        run_browser_js(&browser, &js, ack, true);
                    }
                    BrowserOp::Text { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::text_of_selector(&sel),
                            ack,
                            false,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Value { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::value_of_selector(&sel),
                            ack,
                            false,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Attr { target, name } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::attr_of_selector(&sel, &name),
                            ack,
                            false,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },

                    // ---- Phase 5 P0 action gap ------------------------
                    BrowserOp::Wait {
                        condition,
                        timeout_ms,
                        poll_ms,
                    } => run_browser_wait(browser.clone(), condition, timeout_ms, poll_ms, ack),
                    BrowserOp::Screenshot { path } => run_browser_screenshot(&browser, path, ack),
                    BrowserOp::DblClick { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::dblclick_by_selector(&sel),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Hover { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::hover_by_selector(&sel),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Focus { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::focus_by_selector(&sel),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Blur { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::blur_by_selector(&sel),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Check { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::check_by_selector(&sel),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::Uncheck { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js(
                            &browser,
                            &flowmux_browser::scripts::uncheck_by_selector(&sel),
                            ack,
                            true,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::IsVisible { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js_bool(
                            &browser,
                            &flowmux_browser::scripts::is_visible_selector(&sel),
                            ack,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::IsEnabled { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js_bool(
                            &browser,
                            &flowmux_browser::scripts::is_enabled_selector(&sel),
                            ack,
                        ),
                        Err(e) => {
                            let _ = ack.send(Err(e));
                        }
                    },
                    BrowserOp::IsChecked { target } => match resolve_ref(&browser, &target) {
                        Ok(sel) => run_browser_js_bool(
                            &browser,
                            &flowmux_browser::scripts::is_checked_selector(&sel),
                            ack,
                        ),
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
                            self.attach_or_rerender_surface(workspace, reuse_target, surface_id)
                                .await;
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
                    self.attach_or_rerender_surface(ws_id, pane, surface_id)
                        .await;
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
