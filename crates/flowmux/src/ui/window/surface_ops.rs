// SPDX-License-Identifier: GPL-3.0-or-later
//! Surface lifecycle: split, move, tear-off, reattach, import.

use super::*;

impl WindowController {
    pub fn cancel_tab_drag(&self) -> bool {
        let sources = self
            .pane_registry
            .borrow()
            .surface_tabs
            .values()
            .flatten()
            .filter_map(|(_, tab)| tab.first_child())
            .flat_map(|button| {
                let controllers = button.observe_controllers();
                (0..controllers.n_items())
                    .filter_map(move |i| controllers.item(i).and_downcast::<gtk::DragSource>())
            })
            .filter(|source| source.drag().is_some())
            .collect::<Vec<_>>();
        if sources.is_empty() {
            return false;
        }
        // GTK can still deliver the pending target drop after cancelling its source.
        self.callbacks.tab_drag_drop_committed.set(true);
        self.callbacks.tab_drag_split_candidate.borrow_mut().take();
        for source in sources {
            source.drag_cancel();
        }
        true
    }

    /// Remove a pane from the live widget tree by re-parenting its
    /// surviving sibling into the slot the parent `gtk::Paned`
    /// occupied. Every other pane's widget instance — and therefore
    /// every running PTY shell + browser navigation state — is
    /// preserved.
    ///
    /// Falls back to [`Self::rerender_workspace`] when the GTK tree
    /// shape is not the simple "frame inside Paned" case (e.g. the
    /// removed pane was the workspace root). The fallback reuses surviving
    /// sessions and closes only surfaces absent from the rebuilt tree.
    pub async fn apply_close_pane_incremental_or_rerender(
        &self,
        ws_id: WorkspaceId,
        removed: PaneId,
    ) {
        self.forget_closed_pane_ui_state(&[removed]);
        let frame = {
            let r = self.pane_registry.borrow();
            r.pane_frame(removed)
        };
        let Some(frame) = frame else {
            if let Some(ws) = self.store.get_workspace(ws_id).await {
                self.rerender_workspace(&ws);
            }
            return;
        };

        let Some(parent) = frame.parent() else {
            if let Some(ws) = self.store.get_workspace(ws_id).await {
                self.rerender_workspace(&ws);
            }
            return;
        };

        let Some(paned) = parent.downcast::<gtk::Paned>().ok() else {
            // Removed pane wasn't inside a `gtk::Paned` — nothing to
            // collapse incrementally. Defensive fallback.
            if let Some(ws) = self.store.get_workspace(ws_id).await {
                self.rerender_workspace(&ws);
            }
            return;
        };

        // Pick the sibling: the child of `paned` that isn't `frame`.
        let sibling = if paned.start_child().map(|w| w == frame).unwrap_or(false) {
            paned.end_child()
        } else {
            paned.start_child()
        };
        let Some(sibling) = sibling else {
            if let Some(ws) = self.store.get_workspace(ws_id).await {
                self.rerender_workspace(&ws);
            }
            return;
        };

        let Some(grand) = paned.parent() else {
            if let Some(ws) = self.store.get_workspace(ws_id).await {
                self.rerender_workspace(&ws);
            }
            return;
        };
        let paned_widget: gtk::Widget = paned.clone().upcast();

        // A nested focused paned cannot briefly have no focus child on GTK
        // 4.14. Hand its outer paned a valid sibling while it is reparented.
        let restore_nested_focus = grand
            .downcast_ref::<gtk::Paned>()
            .is_some_and(|grand_paned| {
                handoff_paned_focus_before_detach(grand_paned, &paned_widget)
            });

        // Detach both children through `GtkPaned` so it can update its
        // internal focus child before the widgets are reparented.
        if paned.start_child().as_ref() == Some(&frame) {
            paned.set_start_child(None::<&gtk::Widget>);
            paned.set_end_child(None::<&gtk::Widget>);
        } else {
            paned.set_end_child(None::<&gtk::Widget>);
            paned.set_start_child(None::<&gtk::Widget>);
        }

        // Re-parent `sibling` into the slot `paned` occupied. Each
        // grand-parent kind needs its own removal API: a plain
        // `paned.unparent()` works for `gtk::Paned`, but a
        // `gtk::Stack` keeps a `GtkStackPage` registered to the
        // child name. If we unparent the paned without going through
        // `Stack::remove`, the page entry survives, the subsequent
        // `add_named` with the same name silently no-ops, and the
        // workspace renders blank — which the user reported as "the
        // right X-close drops every pane".
        if let Some(grand_paned) = grand.downcast_ref::<gtk::Paned>() {
            // Nested split: identify which slot of the outer paned holds
            // `paned` and replace it with `sibling`. Calling `set_*_child`
            // directly auto-unparents the previous occupant in one step,
            // so we do NOT pre-call `paned.unparent()` here. With manual
            // unparent followed by `is_none()` slot-pick, GTK4 can leave
            // the parent slot still pointing at `paned` until the next
            // event-loop flush, which triggered the rerender fallback —
            // and that fallback rebuilt every other pane's terminal, killing
            // any agent (claude/codex/shell) running in those panes.
            if grand_paned.start_child().as_ref() == Some(&paned_widget) {
                grand_paned.set_start_child(Some(&sibling));
            } else if grand_paned.end_child().as_ref() == Some(&paned_widget) {
                grand_paned.set_end_child(Some(&sibling));
            } else {
                tracing::warn!(
                    "apply_close_pane_incremental: paned not found in grand_paned slots; falling back"
                );
                paned.unparent();
                if let Some(ws) = self.store.get_workspace(ws_id).await {
                    self.rerender_workspace(&ws);
                }
                return;
            }
            if restore_nested_focus {
                grand_paned.set_focus_child(Some(&sibling));
            }
        } else if let Some(stack) = grand.downcast_ref::<gtk::Stack>() {
            // Top-level workspace child of the GtkStack. Use
            // `Stack::remove` so the GtkStackPage for the old
            // paned-as-name is freed before we register `sibling`
            // under the same name.
            let name = ws_id.to_string();
            let was_visible = stack.visible_child().as_ref() == Some(&paned_widget);
            stack.remove(&paned);
            stack.add_named(&sibling, Some(&name));
            self.surfaces.borrow_mut().insert(ws_id, sibling.clone());
            if was_visible {
                stack.set_visible_child_name(&name);
            }
        } else if let Some(b) = grand.downcast_ref::<gtk::Box>().filter(|container| {
            container.has_css_class("flowmux-ssh-workspace")
                && container.last_child().as_ref() == Some(&paned_widget)
        }) {
            // Keep the SSH toolbar and the outer widget tracked by surfaces.
            b.remove(&paned);
            b.append(&sibling);
        } else {
            tracing::warn!(
                kind = ?grand.type_(),
                "apply_close_pane_incremental: unexpected grand parent kind; falling back"
            );
            paned.unparent();
            if let Some(ws) = self.store.get_workspace(ws_id).await {
                self.rerender_workspace(&ws);
            }
            return;
        }

        // Drop registry entries for the removed pane only — every
        // other pane's TerminalPane / BrowserPane stays alive in the
        // registry and continues to render. The collapsed `paned` is also
        // gone now, so drop its split-index entries to avoid a slow leak
        // across repeated split/close cycles.
        {
            let mut reg = self.pane_registry.borrow_mut();
            reg.forget_pane(removed);
            reg.forget_split_paned(&paned);
        }
        if let Some(ws) = self.store.get_workspace(ws_id).await {
            self.refresh_workspace_solo(&ws);
        }
    }
    /// After a pane has been removed, hand keyboard focus to a sibling so the
    /// user can keep typing without clicking back into a terminal. Only acts
    /// when the removed pane *was* the focused one — closing an unfocused pane
    /// (e.g. clicking another pane's X-button while typing in this one) must
    /// leave focus alone.
    ///
    /// Successor selection, in order:
    /// 1. The most recently focused pane that still exists in this workspace
    ///    (the MRU head after removing `removed`). This matches what the user
    ///    most likely thinks of as "the previous pane".
    /// 2. The workspace's first leaf as a defensive fallback when MRU is empty
    ///    (e.g. the user closed a pane before any focus event was recorded).
    ///
    /// `grab_focus` is deferred to the next idle so the surviving sibling has
    /// a chance to be reparented and realized first; the existing
    /// `EventControllerFocus` then fires `on_focus`, which updates
    /// `focused_pane` and re-pushes the new front of MRU.
    pub(super) async fn focus_after_close(&self, ws_id: WorkspaceId, removed: PaneId) {
        let was_focused = self.focused_pane.get() == Some(removed);

        // Always evict the closed pane from MRU so a later focus_after_close
        // can't pick a dead PaneId.
        {
            let mut mru = self.focus_mru.borrow_mut();
            if let Some(q) = mru.get_mut(&ws_id) {
                q.retain(|p| *p != removed);
            }
        }

        if !was_focused {
            return;
        }

        // 1. MRU head, only if its frame is still registered.
        let mru_head = self
            .focus_mru
            .borrow()
            .get(&ws_id)
            .and_then(|q| q.front().copied());
        let target = match mru_head {
            Some(p) if self.pane_registry.borrow().pane_frame(p).is_some() => Some(p),
            _ => None,
        };

        // 2. Fall back to the workspace's first leaf in the daemon-side tree.
        let target = match target {
            Some(t) => Some(t),
            None => self.store.get_workspace(ws_id).await.and_then(|ws| {
                ws.surfaces
                    .first()
                    .and_then(|s| s.root_pane.first_leaf_id())
            }),
        };
        let Some(target) = target else { return };

        let registry = self.pane_registry.clone();
        glib::idle_add_local_once(move || {
            let r = registry.borrow();
            if let Some(term) = r.active_terminal(target) {
                term.grab_focus();
            } else if let Some(browser) = r.active_browser(target) {
                browser.grab_focus();
            } else if let Some(editor) = r.active_editor(target) {
                editor.grab_focus();
            }
        });
    }
    /// One owner for the state transition, widget update and rollback.
    pub(super) async fn split_pane(
        &self,
        pane: PaneId,
        direction: SplitDirection,
    ) -> Result<PaneId, String> {
        let (workspace, new_pane) = self
            .store
            .split_pane(pane, direction)
            .await
            .ok_or_else(|| format!("pane not found: {pane}"))?;
        self.apply_split_incremental_or_rerender(workspace, pane, new_pane, direction)
            .await?;
        Ok(new_pane)
    }

    /// Update the GTK widget tree after the daemon-side split has completed.
    /// When possible, reuse `target_pane`'s existing `gtk::Frame` inside the new
    /// `gtk::Paned` so other panes in the same workspace, including shell
    /// sessions and browser navigation state, are not reset. If this fails,
    /// roll the store split back instead of rebuilding live panes.
    pub(super) async fn apply_split_incremental_or_rerender(
        &self,
        ws_id: WorkspaceId,
        target_pane: PaneId,
        new_pane: PaneId,
        direction: SplitDirection,
    ) -> Result<(), String> {
        let Some(ws) = self.store.get_workspace(ws_id).await else {
            let _ = self.store.close_pane(new_pane).await;
            return Err("split workspace vanished".to_string());
        };

        let split = ws.surfaces.iter().find_map(|surface| {
            Some((
                surface.root_pane.find_leaf_content(new_pane)?,
                surface.root_pane.parent_split_id(new_pane)?,
            ))
        });
        let Some((new_content, new_split_id)) = split else {
            let _ = self.store.close_pane(new_pane).await;
            return Err("could not locate the new split".to_string());
        };

        // Fallback cwd for the new terminal. Surface content cwd wins; this
        // value is only for legacy or empty-state fallback, so workspace root_dir
        // is enough.
        let new_cwd = ws.local_root().map(PathBuf::from);

        let stack_name = ws.id.to_string();
        let outcome = split_pane_incremental(
            ws.id,
            target_pane,
            new_pane,
            new_split_id,
            direction,
            0.5,
            new_content,
            new_cwd,
            &stack_name,
            &self.callbacks,
            self.pane_registry.clone(),
            self.current_theme(),
            false,
        );

        match outcome {
            IncrementalSplitOutcome::SucceededRoot { new_root } => {
                // If target was the stack root, update the surfaces tracking map
                // to the new widget so later drop_workspace / rerender paths do
                // not look for the old widget in the stack.
                self.surfaces.borrow_mut().insert(ws.id, new_root);
                self.refresh_workspace_solo(&ws);
                self.refresh_window_title().await;
                Ok(())
            }
            IncrementalSplitOutcome::SucceededNested => {
                self.refresh_workspace_solo(&ws);
                self.refresh_window_title().await;
                Ok(())
            }
            IncrementalSplitOutcome::Failed(error) => {
                let _ = self.store.close_pane(new_pane).await;
                self.refresh_window_title().await;
                Err(error)
            }
        }
    }
    /// Attach a newly created surface incrementally whenever possible.
    ///
    /// Old behavior: call rerender_workspace, rebuild the entire workspace
    /// widget, and lose browser navigation state plus terminal shell sessions in
    /// other panes.
    ///
    /// New behavior: append only to the target pane's tab bar / stack. If the
    /// pane is not rendered yet, for example because another workspace is
    /// visible, or the registry cannot find handles, safely fall back to a full
    /// rerender.
    pub(super) async fn attach_or_rerender_surface(
        &self,
        ws_id: WorkspaceId,
        pane: PaneId,
        surface_id: SurfaceId,
        previous: Option<SurfaceId>,
    ) -> Result<(), String> {
        let ws = self
            .store
            .get_workspace(ws_id)
            .await
            .ok_or_else(|| format!("workspace no longer exists: {ws_id}"))?;
        let workspace_visible = self.workspace_stack(ws_id).visible_child_name().as_deref()
            == Some(ws.id.to_string().as_str());
        let surface = ws
            .surfaces
            .iter()
            .find_map(|s| s.root_pane.find_surface(pane, surface_id))
            .ok_or_else(|| format!("surface not found in pane {pane}: {surface_id}"))?;
        // Unrendered panes need the caller's model snapshot from before insertion.
        let previous = previous.or_else(|| self.pane_registry.borrow().active_surface(pane));
        {
            let attached = match attach_surface_to_pane(
                pane,
                ws.id,
                &surface,
                &self.callbacks,
                self.pane_registry.clone(),
                self.current_theme(),
            ) {
                Ok(attached) => attached,
                Err(error) => {
                    self.pane_registry
                        .borrow_mut()
                        .detach_surface_widget(pane, surface_id);
                    if let Some(previous) = previous {
                        self.store.set_active_surface(pane, previous).await;
                    }
                    return Err(error);
                }
            };
            if attached {
                self.refresh_workspace_solo(&ws);
                self.refresh_window_title().await;
                // Move keyboard focus to the newly added terminal/browser tab.
                // attach_surface_to_pane adds the widget and switches the visible
                // child but does not grab focus, which previously left focus on
                // the now-hidden old widget after Ctrl+Shift+T / Ctrl+Shift+B.
                // Defer to idle like ActivateSurface so focus happens after realize.
                if workspace_visible {
                    let registry = self.pane_registry.clone();
                    glib::idle_add_local_once(move || {
                        let r = registry.borrow();
                        if let Some(term) = r.terminals.get(&surface_id) {
                            term.grab_focus();
                        } else if let Some(browser) = r.browsers.get(&surface_id) {
                            browser.grab_focus();
                        } else if let Some(editor) = r.editors.get(&surface_id) {
                            editor.grab_focus();
                        }
                    });
                }
                return Ok(());
            }
        }
        self.rerender_workspace(&ws);
        self.refresh_window_title().await;
        // A full render keeps a failed surface as a placeholder and records
        // why in the registry. For a surface the user just asked for, that
        // placeholder is the failure: report it and roll the tab back.
        let build_error =
            crate::ui::workspace_view::materialize_surface(&self.pane_registry, surface_id)
                .err()
                .or_else(|| {
                    self.pane_registry
                        .borrow()
                        .surface_errors
                        .get(&surface_id)
                        .cloned()
                });
        if let Some(error) = build_error {
            self.pane_registry
                .borrow_mut()
                .detach_surface_widget(pane, surface_id);
            if let Some(previous) = previous {
                self.store.set_active_surface(pane, previous).await;
                self.pane_registry
                    .borrow_mut()
                    .activate_surface(pane, previous);
            }
            return Err(error);
        }
        if self.pane_registry.borrow().has_surface(pane, surface_id) {
            Ok(())
        } else {
            Err(format!(
                "surface could not be mounted in pane {pane}: {surface_id}"
            ))
        }
    }
    pub(super) fn workspace_stack(&self, workspace: WorkspaceId) -> gtk::Stack {
        self.detached_workspaces
            .borrow()
            .get(&workspace)
            .map(|host| host.stack.clone())
            .unwrap_or_else(|| self.stack.clone())
    }

    pub(super) fn workspace_window(&self, workspace: WorkspaceId) -> adw::ApplicationWindow {
        self.detached_workspaces
            .borrow()
            .get(&workspace)
            .map(|host| host.window.clone())
            .unwrap_or_else(|| self.window.clone())
    }

    pub(super) fn workspace_sidebar(&self, workspace: WorkspaceId) -> Sidebar {
        self.detached_workspaces
            .borrow()
            .get(&workspace)
            .map(|host| host.sidebar.clone())
            .unwrap_or_else(|| self.sidebar.clone())
    }

    pub(super) fn upsert_workspace_sidebar(&self, workspace: &Workspace) {
        self.sidebar.upsert(workspace);
        self.sidebar.set_workspace_visible(
            workspace.id,
            !self
                .detached_workspaces
                .borrow()
                .contains_key(&workspace.id),
        );
        if let Some(host) = self.detached_workspaces.borrow().get(&workspace.id) {
            host.sidebar.upsert(workspace);
        }
    }

    pub(super) fn refresh_detached_terminals(&self) {
        let registry = self.pane_registry.borrow();
        let windows = self.detached_workspaces.borrow();
        let mut terminals = self.torn_off_terminals.borrow_mut();
        terminals.clear();
        if windows.is_empty() {
            return;
        }
        for (surface, terminal) in &registry.terminals {
            let Some(workspace) = registry.surface_workspace.get(surface) else {
                continue;
            };
            let Some(host) = windows.get(workspace) else {
                continue;
            };
            terminals.insert(
                *surface,
                termination_dialog::TornOffTerminal {
                    window: host.window.downgrade(),
                    title: registry.surface_title_text(*surface).unwrap_or_default(),
                    pid: terminal.pid.clone(),
                },
            );
        }
    }

    /// Run on the native GTK main thread with an isolated controller and state.
    #[cfg(all(test, feature = "native-smoke"))]
    pub(crate) async fn check_terminal_tearoff(&self, root: &std::path::Path) {
        use vte::prelude::*;

        async fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
            glib::future_with_timeout(Duration::from_secs(10), async {
                while !ready() {
                    glib::timeout_future(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("terminal tear-off timed out: {description}"));
        }

        let app = self.window.application().unwrap();
        for (tear_off, destroy) in [(true, false), (true, true), (false, false)] {
            println!("A19 scenario tear_off={tear_off} destroy={destroy}");
            let workspace = self
                .store
                .create_workspace(Some("Terminal tear-off smoke".into()), root.into())
                .await;
            let ws = self.store.get_workspace(workspace).await.unwrap();
            let pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
            let surface = ws.surfaces[0].root_pane.active_surface_id(pane).unwrap();
            self.render_workspace(&ws);
            self.window.present();
            // Retain only the VTE widget and PID cell, never a backend clone.
            let (widget, pid_cell) = {
                let registry = self.pane_registry.borrow();
                let terminal = registry.terminals.get(&surface).unwrap();
                (terminal.widget.clone(), terminal.pid.clone())
            };
            let pid = pid_cell.get().unwrap();
            {
                let mut registry = self.pane_registry.borrow_mut();
                let moving = registry.detach_surface_for_move(pane, surface).unwrap();
                assert!(moving.content.parent().is_none());
                assert!(registry
                    .attach_moved_surface(
                        pane,
                        workspace,
                        moving,
                        gtk::Button::new().upcast(),
                        gtk::Label::new(Some("Terminal")),
                    )
                    .is_ok());
                assert_eq!(
                    registry.terminals.get(&surface).unwrap().pid.get(),
                    Some(pid)
                );
            }
            wait_for("source terminal mapped", || widget.is_mapped()).await;
            println!("A19 source mapped pid={pid}");
            widget.feed_child(b"printf 'A19_%s\\n' BEFORE\n");
            wait_for("initial output", || {
                widget
                    .text_format(vte::Format::Text)
                    .is_some_and(|text| text.contains("A19_BEFORE"))
            })
            .await;
            println!("A19 initial output received");
            if !tear_off {
                let (_, extra) = self
                    .store
                    .add_terminal_surface_to_pane(pane, Some(root.into()))
                    .await
                    .unwrap();
                self.attach_or_rerender_surface(workspace, pane, extra, None)
                    .await
                    .unwrap();
                let (ack, result) = oneshot::channel();
                self.dispatch(GtkCommand::CloseSurface { pane, surface, ack })
                    .await;
                result.await.unwrap().unwrap();
                wait_for("ordinary tab close terminates child", || {
                    !flowmux_procmon::pid_alive(pid as u32)
                })
                .await;
                continue;
            }
            let previous_windows = app.windows();
            self.tear_off_surface(pane, surface).await;
            println!("A19 tear-off returned");
            let window = app
                .windows()
                .into_iter()
                .find(|window| !previous_windows.contains(window))
                .expect("tear-off must present a new window");
            wait_for("torn terminal mapped", || widget.is_mapped()).await;
            assert!(self.pane_registry.borrow().terminals.contains_key(&surface));
            // Exceed the PTY reaper grace period; stale screen text is not liveness.
            glib::timeout_future(Duration::from_secs(6)).await;
            assert_eq!(pid_cell.get(), Some(pid), "tear-off killed the terminal");
            assert!(flowmux_procmon::pid_alive(pid as u32));
            widget.feed_child(b"printf 'A19_%s\\n' AFTER\n");
            wait_for("new output after tear-off", || {
                widget
                    .text_format(vte::Format::Text)
                    .is_some_and(|text| text.contains("A19_AFTER"))
            })
            .await;
            if destroy {
                window.destroy();
            } else {
                window.close();
            }
            drop(window);
            wait_for("child terminated after window close", || {
                !flowmux_procmon::pid_alive(pid as u32)
            })
            .await;
            assert!(self.store.get_workspace(workspace).await.is_none());
        }
        println!("MACOS_NATIVE_TERMINAL_TEAROFF_OK");
    }

    async fn move_surface_to_new_workspace(
        &self,
        pane: PaneId,
        surface: SurfaceId,
    ) -> Result<(WorkspaceId, PaneId), String> {
        let source = self
            .store
            .workspace_of_pane(pane)
            .await
            .ok_or("source pane no longer exists")?;
        let ws = self
            .store
            .get_workspace(source)
            .await
            .ok_or("source workspace no longer exists")?;
        if ws.location.ssh().is_some() {
            return Err("Moving SSH tabs to another window is not supported".into());
        }
        let title = self
            .store
            .surface_title(pane, surface)
            .await
            .ok_or("source tab no longer exists")?;
        // Render an empty destination so the move adopts the live backend without spawning a shell.
        let workspace = self
            .store
            .create_workspace(
                Some(title),
                ws.local_root()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("/")),
            )
            .await;
        let mut destination = self.store.get_workspace(workspace).await.unwrap();
        let dst_pane = destination.surfaces[0].root_pane.first_leaf_id().unwrap();
        let placeholder = destination.surfaces[0]
            .root_pane
            .active_surface_id(dst_pane)
            .unwrap();
        destination.surfaces[0].root_pane = Pane::Leaf {
            id: dst_pane,
            content: PaneContent::Tabs {
                active: surface,
                surfaces: Vec::new(),
            },
        };
        self.render_workspace_with_activation(&destination, false);
        if let Err(error) = self.move_surface(pane, surface, None, dst_pane, 0).await {
            if self.store.surface_title(dst_pane, surface).await.is_none() {
                self.store.remove_workspace(workspace).await;
                self.drop_workspace(workspace);
                return Err(error);
            }
            // A committed move must survive a rendering failure, including its live backend.
            tracing::warn!(%error, "tab moved but destination rendering needs recovery");
        }
        self.store.close_surface(dst_pane, placeholder).await;
        Ok((workspace, dst_pane))
    }

    pub(super) async fn return_surface_to_main_window(
        &self,
        pane: PaneId,
        surface: SurfaceId,
    ) -> Result<(), String> {
        if self.window_close.approved.get() {
            return Err("The main window has closed".into());
        }
        let visible = self
            .stack
            .visible_child_name()
            .and_then(|name| name.parse::<WorkspaceId>().ok());
        let target = visible
            .filter(|id| self.surfaces.borrow().contains_key(id))
            .or_else(|| {
                self.surfaces
                    .borrow()
                    .keys()
                    .find(|id| !self.detached_workspaces.borrow().contains_key(id))
                    .copied()
            });
        let workspace = if let Some(workspace) = target {
            self.move_surface_to_workspace(pane, surface, workspace)
                .await?;
            workspace
        } else {
            self.move_surface_to_new_workspace(pane, surface).await?.0
        };
        self.activate_workspace(workspace).await;
        self.window.present();
        Ok(())
    }

    pub(super) async fn tear_off_surface(&self, pane: PaneId, surface: SurfaceId) {
        let Some(app) = self.window.application() else {
            return;
        };
        let Some(title) = self.store.surface_title(pane, surface).await else {
            return;
        };
        let (workspace, dst_pane) = match self.move_surface_to_new_workspace(pane, surface).await {
            Ok(destination) => destination,
            Err(error) => {
                tracing::warn!(%error, "tab tear-off rejected");
                return;
            }
        };
        let widget = self.surfaces.borrow()[&workspace].clone();
        self.stack.remove(&widget);
        let stack = gtk::Stack::new();
        stack.set_hexpand(true);
        stack.set_vexpand(true);
        stack.add_named(&widget, Some(&workspace.to_string()));
        let window = adw::ApplicationWindow::builder()
            .application(&app)
            .default_width(1280)
            .default_height(800)
            .icon_name(crate::APP_ID)
            .title(format!("Flowmux - {title}"))
            .build();
        window.set_widget_name("flowmux-detached-window");
        let select_bridge = self.bridge.clone();
        let close_bridge = self.bridge.clone();
        let ssh_bridge = self.bridge.clone();
        let mut sidebar = Sidebar::new(
            move |id| select_bridge.send_from_ui(GtkCommand::ActivateWorkspace { id, ack: None }),
            move |id| {
                let (ack, _) = oneshot::channel();
                close_bridge.send_from_ui(GtkCommand::RemoveWorkspace {
                    id,
                    confirm: true,
                    ack,
                });
            },
            move || ssh_bridge.send_from_ui(GtkCommand::ShowSshDialog),
            self.bridge.clone(),
            std::ops::Deref::deref(&self.notifications).clone(),
            self.activities.clone(),
            self.notifications.tokio_handle(),
            self.options.borrow().agent_bar_mode,
        );
        sidebar.share_tab_drag_drop_state(&self.sidebar);
        sidebar.upsert(&self.store.get_workspace(workspace).await.unwrap());
        sidebar.select_workspace(workspace);
        let split =
            build_split_window_shell(&sidebar, &stack, self.sidebar_split.position().max(260));
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&split));
        set_window_content(&window, &overlay);
        self.detached_workspaces.borrow_mut().insert(
            workspace,
            DetachedWorkspace {
                window: window.clone(),
                stack,
                sidebar,
            },
        );
        self.sidebar.set_workspace_visible(workspace, false);
        self.refresh_detached_terminals();
        crate::keybindings::install_actions_for_window(self, self.bridge.clone(), &window);
        let windows = Rc::downgrade(&self.detached_workspaces);
        let bridge = self.bridge.clone();
        window.connect_close_request(move |window| {
            if let Some(windows) = windows.upgrade() {
                let workspaces = windows
                    .borrow()
                    .iter()
                    .filter(|(_, host)| host.window == *window)
                    .map(|(id, _)| *id)
                    .collect();
                bridge.send_from_ui(GtkCommand::CloseDetachedWindow { workspaces });
            }
            glib::Propagation::Stop
        });
        let windows = Rc::downgrade(&self.detached_workspaces);
        let bridge = self.bridge.clone();
        window.connect_destroy(move |window| {
            if let Some(windows) = windows.upgrade() {
                let workspaces = windows
                    .borrow()
                    .iter()
                    .filter(|(_, host)| host.window == *window)
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                if !workspaces.is_empty() {
                    bridge.send_from_ui(GtkCommand::CloseDetachedWindow { workspaces });
                }
            }
        });
        self.show_status_when_empty();
        window.present();
        self.focus_pane(dst_pane);
    }
    /// Move a live surface tab into another pane (`dst_pane`), preserving its
    /// terminal/browser state by re-parenting the existing widget rather than
    /// rebuilding it. `target_index` is the final position in the destination
    /// pane (clamped to the end). Backs cross-pane and cross-workspace
    /// drag-and-drop.
    pub(super) async fn move_surface(
        &self,
        src_pane: PaneId,
        surface: SurfaceId,
        surface_model: Option<flowmux_core::PaneSurface>,
        dst_pane: PaneId,
        target_index: usize,
    ) -> Result<(), String> {
        if self.store.workspace_of_pane(src_pane).await.is_some()
            && !self.store.can_move_between_panes(src_pane, dst_pane).await
        {
            return Err("Moving tabs across SSH workspaces is not supported".into());
        }
        if src_pane == dst_pane {
            // A drop onto the tab's own pane (e.g. pane-body drop) is just a
            // reorder; do that instead of a no-op move.
            if !self.pane_registry.borrow().has_surface(src_pane, surface) {
                return Err("source surface is not rendered".into());
            }
            if self
                .store
                .reorder_surface_in_pane(src_pane, surface, target_index)
                .await
                .is_some()
                || self.store.surface_title(src_pane, surface).await.is_some()
            {
                self.pane_registry.borrow_mut().reorder_surface_widget(
                    src_pane,
                    surface,
                    target_index,
                );
                return Ok(());
            }
            return Err("source surface no longer exists".into());
        }

        if !self.pane_registry.borrow().has_pane(dst_pane) {
            return Err("destination pane is not rendered".to_string());
        }

        if self.store.workspace_of_pane(dst_pane).await.is_none() {
            return Err("destination pane no longer exists".to_string());
        }
        let Some(src_workspace) = self.store.workspace_of_pane(src_pane).await else {
            return match surface_model {
                Some(surface_model)
                    if src_pane.0.is_nil()
                        && matches!(surface_model.kind, SurfaceKind::Editor { .. }) =>
                {
                    self.import_surface_to_pane(surface_model, dst_pane, target_index)
                        .await
                }
                _ => Err("source pane is not owned by this Flowmux instance".to_string()),
            };
        };

        crate::ui::workspace_view::materialize_surface(&self.pane_registry, surface)?;
        let moving = self
            .pane_registry
            .borrow_mut()
            .detach_surface_for_move(src_pane, surface);
        let Some(moving) = moving else {
            return Err("source surface is not rendered".to_string());
        };

        let outcome = match self
            .store
            .move_surface_to_pane(src_pane, surface, dst_pane, target_index)
            .await
        {
            Some(outcome) => outcome,
            None => {
                // Model rejected the move; restore the widget to its source.
                self.reattach_surface(src_workspace, src_pane, surface, moving)
                    .await;
                return Err("destination pane no longer exists".to_string());
            }
        };

        let mounted = self
            .mount_moved_surface(
                outcome.dst_pane,
                outcome.dst_workspace,
                outcome.surface,
                moving,
                target_index,
            )
            .await;
        if let Err(moving) = mounted {
            self.rerender_with_moving_surface(outcome.dst_workspace, *moving)
                .await;
        }

        self.refresh_detached_terminals();
        if outcome.src_workspace_removed {
            self.drop_workspace(outcome.src_workspace);
            self.activate_active_or_show_empty().await;
        } else if outcome.src_pane_removed {
            self.apply_close_pane_incremental_or_rerender(outcome.src_workspace, outcome.src_pane)
                .await;
        } else {
            self.sync_pane_active_from_store(outcome.src_workspace, outcome.src_pane)
                .await;
        }

        if let Some(ws) = self.store.get_workspace(outcome.dst_workspace).await {
            self.refresh_workspace_solo(&ws);
        }
        self.sync_workspace_agent_status_from_store(outcome.dst_workspace)
            .await;
        if outcome.src_workspace != outcome.dst_workspace && !outcome.src_workspace_removed {
            self.sync_workspace_agent_status_from_store(outcome.src_workspace)
                .await;
        }
        self.refresh_window_title().await;
        if !self
            .pane_registry
            .borrow()
            .has_surface(outcome.dst_pane, outcome.surface)
        {
            return Err("moved surface could not be mounted".into());
        }
        self.focus_pane(outcome.dst_pane);
        Ok(())
    }
    /// Move a live surface tab to the last position of the first pane of
    /// `dst_workspace`, then bring that workspace to the front. Backs the
    /// right-click "Move" submenu and a drop directly onto a side-panel row.
    pub(super) async fn move_surface_to_workspace(
        &self,
        src_pane: PaneId,
        surface: SurfaceId,
        dst_workspace: WorkspaceId,
    ) -> Result<(), String> {
        let dst_pane = self
            .store
            .get_workspace(dst_workspace)
            .await
            .and_then(|ws| {
                ws.surfaces
                    .first()
                    .and_then(|s| s.root_pane.first_leaf_id())
            })
            .ok_or_else(|| "destination workspace has no pane".to_string())?;
        let res = self
            .move_surface(src_pane, surface, None, dst_pane, usize::MAX)
            .await;
        if res.is_ok() {
            self.activate_workspace(dst_workspace).await;
        }
        res
    }
    /// Build a tab widget for `surface` in `dst_pane` and mount the detached
    /// live widget there, aligning the tab order with the model index.
    pub(super) async fn mount_moved_surface(
        &self,
        dst_pane: PaneId,
        dst_workspace: WorkspaceId,
        surface: SurfaceId,
        moving: MovingSurface,
        target_index: usize,
    ) -> Result<(), Box<MovingSurface>> {
        let surface_model = self
            .store
            .get_workspace(dst_workspace)
            .await
            .and_then(|ws| {
                ws.surfaces
                    .iter()
                    .find_map(|s| s.root_pane.find_surface(dst_pane, surface))
            });
        let Some(surface_model) = surface_model else {
            return Err(Box::new(moving));
        };
        let (tab, label) =
            build_surface_tab_widget(dst_pane, &surface_model, true, &self.callbacks);
        let mounted = self.pane_registry.borrow_mut().attach_moved_surface(
            dst_pane,
            dst_workspace,
            moving,
            tab.upcast(),
            label,
        );
        if mounted.is_ok() {
            self.pane_registry
                .borrow_mut()
                .reorder_surface_widget(dst_pane, surface, target_index);
        }
        mounted
    }

    /// Rebuild a workspace after an incremental DnD mount failed. A detached
    /// surface is stashed first so its PTY, WebView and unsaved state survive.
    pub(super) async fn rerender_with_moving_surface(
        &self,
        workspace: WorkspaceId,
        moving: MovingSurface,
    ) {
        self.pane_registry
            .borrow_mut()
            .stash_moving_surface_for_rerender(moving);
        if let Some(ws) = self.store.get_workspace(workspace).await {
            self.rerender_workspace(&ws);
        } else {
            self.pane_registry
                .borrow_mut()
                .discard_unused_detached_surfaces();
        }
    }
    /// Best-effort restore of a detached surface back into its source pane when
    /// a move is rejected by the store.
    pub(super) async fn reattach_surface(
        &self,
        src_workspace: WorkspaceId,
        src_pane: PaneId,
        surface: SurfaceId,
        moving: MovingSurface,
    ) {
        let mounted = self
            .mount_moved_surface(src_pane, src_workspace, surface, moving, usize::MAX)
            .await;
        if let Err(moving) = mounted {
            self.rerender_with_moving_surface(src_workspace, *moving)
                .await;
        }
    }
    pub(super) async fn sync_pane_active_from_store(&self, workspace: WorkspaceId, pane: PaneId) {
        let Some(ws) = self.store.get_workspace(workspace).await else {
            return;
        };
        let Some(active) = active_surface_from_workspace(&ws, pane) else {
            return;
        };
        self.pane_registry
            .borrow_mut()
            .activate_surface(pane, active);
    }
    pub(super) async fn import_surface_to_pane(
        &self,
        surface_model: flowmux_core::PaneSurface,
        dst_pane: PaneId,
        target_index: usize,
    ) -> Result<(), String> {
        if !self.pane_registry.borrow().has_pane(dst_pane) {
            return Err("destination pane is not rendered".to_string());
        }

        let Some((ws_id, surface_id)) = self
            .store
            .import_surface_to_pane(dst_pane, surface_model, target_index)
            .await
        else {
            return Err("destination pane no longer exists".to_string());
        };

        if let Err(error) = self
            .attach_or_rerender_surface(ws_id, dst_pane, surface_id, None)
            .await
        {
            self.store.close_surface(dst_pane, surface_id).await;
            return Err(error);
        }
        if let Some(ws) = self.store.get_workspace(ws_id).await {
            self.refresh_workspace_solo(&ws);
        }
        self.sync_workspace_agent_status_from_store(ws_id).await;
        self.refresh_window_title().await;
        self.focus_pane(dst_pane);
        Ok(())
    }
    pub(super) async fn split_imported_surface_into_pane(
        &self,
        surface_model: flowmux_core::PaneSurface,
        dst_pane: PaneId,
        direction: SplitDirection,
    ) -> Result<(), String> {
        if !self.pane_registry.borrow().has_pane(dst_pane) {
            return Err("destination pane is not rendered".to_string());
        }
        if !can_split_pane_incrementally(&self.pane_registry.borrow(), dst_pane) {
            return Err("destination pane cannot be split incrementally".to_string());
        }

        let Some((ws_id, new_pane, _surface_id)) = self
            .store
            .split_imported_surface_into_pane(dst_pane, surface_model, direction)
            .await
        else {
            return Err("destination pane no longer exists".to_string());
        };
        let Some(ws) = self.store.get_workspace(ws_id).await else {
            let _ = self.store.close_pane(new_pane).await;
            return Err("destination workspace vanished".to_string());
        };
        let Some(new_split_id) = ws
            .surfaces
            .iter()
            .find_map(|s| s.root_pane.parent_split_id(new_pane))
        else {
            let _ = self.store.close_pane(new_pane).await;
            return Err("could not locate the new split node".to_string());
        };
        let Some(content) = ws
            .surfaces
            .iter()
            .find_map(|s| s.root_pane.find_leaf_content(new_pane))
        else {
            let _ = self.store.close_pane(new_pane).await;
            return Err("could not locate imported surface content".to_string());
        };

        let stack_name = ws.id.to_string();
        match split_pane_incremental(
            ws.id,
            dst_pane,
            new_pane,
            new_split_id,
            direction,
            0.5,
            content,
            ws.local_root().map(PathBuf::from),
            &stack_name,
            &self.callbacks,
            self.pane_registry.clone(),
            self.current_theme(),
            false,
        ) {
            IncrementalSplitOutcome::SucceededRoot { new_root } => {
                self.surfaces.borrow_mut().insert(ws.id, new_root);
            }
            IncrementalSplitOutcome::SucceededNested => {}
            IncrementalSplitOutcome::Failed(error) => {
                let _ = self.store.close_pane(new_pane).await;
                return Err(error);
            }
        }

        self.refresh_workspace_solo(&ws);
        self.sync_workspace_agent_status_from_store(ws.id).await;
        self.refresh_window_title().await;
        self.focus_pane(new_pane);
        Ok(())
    }
    /// Split `dst_pane` and move the dragged tab (with its live state) into the
    /// new sibling. Backs dropping a tab on the right / bottom region of a pane.
    pub(super) async fn split_surface_into_pane(
        &self,
        src_pane: PaneId,
        surface: SurfaceId,
        surface_model: Option<flowmux_core::PaneSurface>,
        dst_pane: PaneId,
        direction: SplitDirection,
    ) -> Result<(), String> {
        if self.store.workspace_of_pane(src_pane).await.is_some()
            && !self.store.can_move_between_panes(src_pane, dst_pane).await
        {
            return Err("Moving tabs across SSH workspaces is not supported".into());
        }

        if !self.pane_registry.borrow().has_pane(dst_pane) {
            return Err("destination pane is not rendered".to_string());
        }
        if !can_split_pane_incrementally(&self.pane_registry.borrow(), dst_pane) {
            return Err("destination pane cannot be split incrementally".to_string());
        }

        if self.store.workspace_of_pane(dst_pane).await.is_none() {
            return Err("destination pane no longer exists".to_string());
        }
        let Some(src_workspace) = self.store.workspace_of_pane(src_pane).await else {
            return match surface_model {
                Some(surface_model)
                    if src_pane.0.is_nil()
                        && matches!(surface_model.kind, SurfaceKind::Editor { .. }) =>
                {
                    self.split_imported_surface_into_pane(surface_model, dst_pane, direction)
                        .await
                }
                _ => Err("source pane is not owned by this Flowmux instance".to_string()),
            };
        };

        crate::ui::workspace_view::materialize_surface(&self.pane_registry, surface)?;
        let moving = self
            .pane_registry
            .borrow_mut()
            .detach_surface_for_move(src_pane, surface);
        let Some(moving) = moving else {
            return Err("source surface is not rendered".to_string());
        };

        let outcome = match self
            .store
            .split_surface_into_pane(src_pane, surface, dst_pane, direction)
            .await
        {
            Some(outcome) => outcome,
            None => {
                self.reattach_surface(src_workspace, src_pane, surface, moving)
                    .await;
                return Err("destination pane no longer exists".to_string());
            }
        };
        let dst_ws = outcome.dst_workspace;
        let new_pane = outcome.new_pane;
        let dst_pane = outcome.dst_pane;
        let surface = outcome.surface;

        // Build the new sibling pane empty, then mount the live tab into it.
        let Some(ws) = self.store.get_workspace(dst_ws).await else {
            return Err("destination workspace vanished".to_string());
        };
        let new_split_id = ws
            .surfaces
            .iter()
            .find_map(|s| s.root_pane.parent_split_id(new_pane));
        let Some(new_split_id) = new_split_id else {
            // Could not locate the split node; fall back to a plain tab move so
            // the live widget is not lost.
            self.rerender_with_moving_surface(dst_ws, moving).await;
            return Err("could not locate the new split node".to_string());
        };

        let empty = flowmux_core::PaneContent::Tabs {
            active: surface,
            surfaces: Vec::new(),
        };
        let stack_name = ws.id.to_string();
        let split_outcome = split_pane_incremental(
            ws.id,
            dst_pane,
            new_pane,
            new_split_id,
            direction,
            0.5,
            empty,
            ws.local_root().map(PathBuf::from),
            &stack_name,
            &self.callbacks,
            self.pane_registry.clone(),
            self.current_theme(),
            true,
        );
        let moving = match split_outcome {
            IncrementalSplitOutcome::SucceededRoot { new_root } => {
                self.surfaces.borrow_mut().insert(ws.id, new_root);
                Some(moving)
            }
            IncrementalSplitOutcome::SucceededNested => Some(moving),
            IncrementalSplitOutcome::Failed(_) => {
                // The model already owns the split. Recover its live sessions
                // and finish source cleanup before reporting the actual result.
                self.rerender_with_moving_surface(dst_ws, moving).await;
                None
            }
        };

        if let Some(moving) = moving {
            if let Err(moving) = self
                .mount_moved_surface(new_pane, dst_ws, surface, moving, 0)
                .await
            {
                self.rerender_with_moving_surface(dst_ws, *moving).await;
            }
        }

        self.refresh_detached_terminals();
        if outcome.src_workspace_removed {
            self.drop_workspace(outcome.src_workspace);
            self.activate_active_or_show_empty().await;
        } else if outcome.src_pane_removed {
            self.apply_close_pane_incremental_or_rerender(outcome.src_workspace, outcome.src_pane)
                .await;
        } else {
            self.sync_pane_active_from_store(outcome.src_workspace, outcome.src_pane)
                .await;
        }

        if let Some(ws) = self.store.get_workspace(dst_ws).await {
            self.refresh_workspace_solo(&ws);
        }
        self.sync_workspace_agent_status_from_store(dst_ws).await;
        if outcome.src_workspace != dst_ws && !outcome.src_workspace_removed {
            self.sync_workspace_agent_status_from_store(outcome.src_workspace)
                .await;
        }
        self.refresh_window_title().await;
        if !self.pane_registry.borrow().has_surface(new_pane, surface) {
            return Err("moved surface could not be mounted in the new pane".into());
        }
        self.focus_pane(new_pane);
        Ok(())
    }
}

#[cfg(all(test, not(target_os = "macos")))]
pub(super) mod tests {
    use super::*;

    async fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
        glib::future_with_timeout(Duration::from_secs(10), async {
            while !ready() {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out: {description}"));
    }

    pub(crate) async fn setup(
        name: &str,
        root: &std::path::Path,
    ) -> (WindowController, WorkspaceId, PaneId) {
        adw::init().unwrap();
        let store = StateStore::new_lazy(flowmux_state::State::default());
        let workspace = store
            .create_workspace(Some("Main".into()), root.into())
            .await;
        let ws = store.get_workspace(workspace).await.unwrap();
        let pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
        let (bridge, rx) = Bridge::new();
        let app = adw::Application::builder().application_id(name).build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        let controller = WindowController::new(
            &app,
            store,
            Arc::new(ResolvedTheme::load()),
            bridge,
            gtk::CssProvider::new(),
            None,
        );
        crate::keybindings::install_actions(&controller, controller.bridge.clone());
        controller.render_workspace(&ws);
        controller.window.present();
        spawn_dispatch_loop(rx, controller.clone());
        (controller, workspace, pane)
    }

    #[gtk::test]
    async fn detached_window_moves_live_tabs_splits_and_creates_workspaces() {
        use vte::prelude::*;
        let _direct_pty = crate::ui::ghostty_pane::bypass_pty_tee_for_test();
        let root = tempfile::tempdir().unwrap();
        let (c, workspace, pane) =
            setup("com.flowmux.App.UiTest.DetachedWindows", root.path()).await;
        let (_, surface) = c
            .store
            .add_terminal_surface_to_pane(pane, None)
            .await
            .unwrap();
        c.attach_or_rerender_surface(workspace, pane, surface, None)
            .await
            .unwrap();
        let terminal = c.pane_registry.borrow().terminals[&surface].clone();
        let pid = terminal.pid.get().unwrap();
        c.tear_off_surface(pane, surface).await;
        let dst = c.pane_registry.borrow().pane_for_surface(surface).unwrap();
        let detached_ws = c.pane_registry.borrow().workspace_of_pane(dst).unwrap();
        let window = c.detached_workspaces.borrow()[&detached_ws].window.clone();
        assert_eq!(
            c.pane_registry.borrow().surface_tabs[&dst]
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            vec![surface],
            "tear-off must not create a tab outside the model"
        );
        assert_eq!(
            c.pane_registry
                .borrow()
                .stack_for_pane(dst)
                .unwrap()
                .pages()
                .n_items(),
            1
        );
        assert_eq!(c.pane_registry.borrow().terminals.len(), 2);
        wait_for("detached terminal mapped", || terminal.widget.is_mapped()).await;
        assert_eq!(terminal.pid.get(), Some(pid));
        assert_eq!(
            terminal
                .widget
                .root()
                .and_downcast::<adw::ApplicationWindow>(),
            Some(window.clone())
        );
        terminal.widget.feed_child(b"printf 'WINDOW_%s\\n' LIVE\n");
        wait_for("live output after tear-off", || {
            terminal
                .widget
                .text_format(vte::Format::Text)
                .is_some_and(|text| text.contains("WINDOW_LIVE"))
        })
        .await;
        assert!(c.pane_in_visible_workspace(dst));
        assert_eq!(
            c.focused_pane.get(),
            Some(dst),
            "detached tab must own focus before its split action"
        );
        window.lookup_action("split-right").unwrap().activate(None);
        wait_for("split action in detached window", || {
            c.pane_registry
                .borrow()
                .pane_ids_in_workspace(detached_ws)
                .count()
                == 2
        })
        .await;
        assert_eq!(
            c.pane_registry.borrow().terminals[&surface].pid.get(),
            Some(pid)
        );
        window
            .lookup_action("new-workspace")
            .unwrap()
            .activate(None);
        wait_for("new workspace in detached window", || {
            c.detached_workspaces.borrow().len() == 2
        })
        .await;
        assert!(c
            .detached_workspaces
            .borrow()
            .values()
            .all(|host| host.window == window));
        c.return_surface_to_main_window(dst, surface).await.unwrap();
        assert_eq!(
            c.pane_registry.borrow().pane_for_surface(surface),
            Some(pane)
        );
        assert_eq!(terminal.pid.get(), Some(pid));
        assert!(
            window.is_visible(),
            "other workspaces must keep the detached window open"
        );
        let original = c.pane_registry.borrow().surface_tabs[&pane]
            .iter()
            .find(|(id, _)| *id != surface)
            .unwrap()
            .0;
        let (ack, rx) = oneshot::channel();
        c.dispatch(GtkCommand::CloseSurface {
            pane,
            surface: original,
            ack,
        })
        .await;
        rx.await.unwrap().unwrap();
        let target = c.store.get_workspace(detached_ws).await.unwrap().surfaces[0]
            .root_pane
            .first_leaf_id()
            .unwrap();
        c.split_surface_into_pane(pane, surface, None, target, SplitDirection::Horizontal)
            .await
            .unwrap();
        assert_eq!(c.stack.visible_child_name().as_deref(), Some("__empty"));
        let moved_pane = c.pane_registry.borrow().pane_for_surface(surface).unwrap();
        assert_eq!(
            c.pane_registry.borrow().workspace_of_pane(moved_pane),
            Some(detached_ws)
        );
        c.return_surface_to_main_window(moved_pane, surface)
            .await
            .unwrap();
        // Rebuild the detached layout through its own stack, retaining its terminal.
        let ws = c.store.get_workspace(detached_ws).await.unwrap();
        c.rerender_workspace(&ws);
        assert_eq!(
            c.surfaces.borrow()[&detached_ws]
                .root()
                .and_downcast::<adw::ApplicationWindow>(),
            Some(window.clone())
        );
        window.close();
        wait_for("detached window closed", || {
            c.detached_workspaces.borrow().is_empty()
        })
        .await;
        assert!(flowmux_procmon::pid_alive(pid as u32));
        assert_eq!(terminal.pid.get(), Some(pid));
        c.window.destroy();
    }

    #[gtk::test]
    async fn detached_tab_rename_dialog_belongs_to_its_window() {
        let _direct_pty = crate::ui::ghostty_pane::bypass_pty_tee_for_test();
        let root = tempfile::tempdir().unwrap();
        let (c, workspace, pane) =
            setup("com.flowmux.App.UiTest.DetachedRename", root.path()).await;
        let (_, surface) = c
            .store
            .add_terminal_surface_to_pane(pane, None)
            .await
            .unwrap();
        c.attach_or_rerender_surface(workspace, pane, surface, None)
            .await
            .unwrap();
        c.tear_off_surface(pane, surface).await;
        let dst = c.pane_registry.borrow().pane_for_surface(surface).unwrap();
        let detached_ws = c.pane_registry.borrow().workspace_of_pane(dst).unwrap();
        let window = c.detached_workspaces.borrow()[&detached_ws].window.clone();
        let button = c.pane_registry.borrow().surface_tabs[&dst]
            .iter()
            .find(|(id, _)| *id == surface)
            .unwrap()
            .1
            .first_child()
            .and_downcast::<gtk::Button>()
            .unwrap();
        button.emit_clicked();
        button.emit_clicked();
        wait_for("tab rename dialog", || {
            window.visible_dialog().is_some() || c.window.visible_dialog().is_some()
        })
        .await;
        assert!(
            c.window.visible_dialog().is_none(),
            "double-clicking a detached tab must not open a dialog in the source window"
        );
        let dialog = window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        assert_eq!(dialog.heading().as_deref(), Some("Rename Pane Tab"));
        dialog
            .extra_child()
            .unwrap()
            .downcast::<gtk::Entry>()
            .unwrap()
            .set_text("Detached tab");
        glib::timeout_future(Duration::from_millis(300)).await;
        let mut pending = vec![dialog.clone().upcast::<gtk::Widget>()];
        let mut accepted = false;
        while let Some(widget) = pending.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some("OK") {
                    button.emit_clicked();
                    accepted = true;
                    break;
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                pending.push(widget);
            }
        }
        assert!(accepted, "rename confirmation button must exist");
        wait_for("tab rename dialog closed", || {
            window.visible_dialog().is_none()
        })
        .await;
        glib::future_with_timeout(Duration::from_secs(5), async {
            while c.store.surface_title(dst, surface).await.as_deref() != Some("Detached tab") {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        // The side-panel workspace's double-click uses a separate rename command.
        c.store.set_active_workspace(Some(workspace)).await;
        let expected_name = c
            .store
            .get_workspace(detached_ws)
            .await
            .unwrap()
            .display_title()
            .to_string();
        window
            .lookup_action("rename-active-workspace")
            .unwrap()
            .activate(None);
        wait_for("workspace rename shortcut", || {
            window.visible_dialog().is_some() || c.window.visible_dialog().is_some()
        })
        .await;
        assert!(c.window.visible_dialog().is_none(), "the shortcut must use its own window, even when another workspace is active in the store");
        let dialog = window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        assert_eq!(dialog.heading().as_deref(), Some("Rename Tab"));
        assert_eq!(
            dialog
                .extra_child()
                .unwrap()
                .downcast::<gtk::Entry>()
                .unwrap()
                .text(),
            expected_name
        );
        glib::timeout_future(Duration::from_millis(300)).await;
        dialog.close();
        wait_for("workspace rename cancelled", || {
            window.visible_dialog().is_none()
        })
        .await;
        c.return_surface_to_main_window(dst, surface).await.unwrap();
        assert!(c.detached_workspaces.borrow().is_empty());
        let pane = c.pane_registry.borrow().pane_for_surface(surface).unwrap();
        c.dispatch(GtkCommand::ShowRenameSurfaceDialog { pane, surface })
            .await;
        let dialog = c
            .window
            .visible_dialog()
            .unwrap()
            .downcast::<adw::AlertDialog>()
            .unwrap();
        assert_eq!(
            dialog
                .extra_child()
                .unwrap()
                .downcast::<gtk::Entry>()
                .unwrap()
                .text(),
            "Detached tab"
        );
        glib::timeout_future(Duration::from_millis(300)).await;
        dialog.close();
        wait_for("main window rename cancelled", || {
            c.window.visible_dialog().is_none()
        })
        .await;
        c.window.destroy();
    }

    #[gtk::test]
    async fn detached_browser_and_dirty_editor_keep_their_live_state() {
        use webkit6::prelude::*;
        let _direct_pty = crate::ui::ghostty_pane::bypass_pty_tee_for_test();
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("unsaved.txt");
        std::fs::write(&file, "original").unwrap();
        let (c, workspace, pane) =
            setup("com.flowmux.App.UiTest.DetachedWebViews", root.path()).await;
        let (_, browser_id) = c
            .store
            .add_browser_surface_to_pane(pane, "about:blank".into())
            .await
            .unwrap();
        c.attach_or_rerender_surface(workspace, pane, browser_id, None)
            .await
            .unwrap();
        let browser = c.pane_registry.borrow().browsers[&browser_id].clone();
        async fn eval(browser: &crate::ui::browser_pane::BrowserPane, source: &str) -> String {
            let (tx, rx) = oneshot::channel();
            browser.evaluate_js(source, move |value| {
                let _ = tx.send(value);
            });
            rx.await.unwrap().unwrap()
        }
        eval(&browser, "window.unsavedTabState = 'still here'").await;
        c.tear_off_surface(pane, browser_id).await;
        let source = c
            .pane_registry
            .borrow()
            .pane_for_surface(browser_id)
            .unwrap();
        c.return_surface_to_main_window(source, browser_id)
            .await
            .unwrap();
        assert_eq!(
            c.pane_registry.borrow().browsers[&browser_id].root,
            browser.root
        );
        assert_eq!(eval(&browser, "window.unsavedTabState").await, "still here");
        let (_, editor_id) = c
            .store
            .add_editor_surface_to_pane(pane, root.path().into())
            .await
            .unwrap();
        c.attach_or_rerender_surface(workspace, pane, editor_id, None)
            .await
            .unwrap();
        let editor = c.pane_registry.borrow().editors[&editor_id].clone();
        editor.open_file(&file).await.unwrap();
        editor.flush_pending_changes().await.unwrap();
        editor.grab_focus();
        let view = editor
            .focus_widget()
            .downcast::<webkit6::WebView>()
            .unwrap();
        view.execute_editing_command_with_argument("InsertText", "unsaved");
        wait_for("dirty editor", || !editor.dirty_document_paths().is_empty()).await;
        c.tear_off_surface(pane, editor_id).await;
        let source = c
            .pane_registry
            .borrow()
            .pane_for_surface(editor_id)
            .unwrap();
        let window = c
            .detached_workspaces
            .borrow()
            .values()
            .next()
            .unwrap()
            .window
            .clone();
        window.close();
        wait_for("dirty close prompt in the detached window", || {
            window.visible_dialog().is_some()
        })
        .await;
        glib::timeout_future(Duration::from_millis(300)).await;
        window.visible_dialog().unwrap().close();
        wait_for("dirty close cancelled", || {
            window.visible_dialog().is_none()
        })
        .await;
        assert!(window.is_visible());
        c.return_surface_to_main_window(source, editor_id)
            .await
            .unwrap();
        assert_eq!(
            c.pane_registry.borrow().editors[&editor_id].root,
            editor.root
        );
        assert_eq!(editor.dirty_document_paths(), vec![file.clone()]);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
        editor.flush_pending_changes().await.unwrap();
        editor.save_all_dirty().await.unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("unsaved"));
        c.window.destroy();
    }

    #[gtk::test]
    async fn last_detached_tab_returns_to_an_empty_main_window() {
        let _direct_pty = crate::ui::ghostty_pane::bypass_pty_tee_for_test();
        let root = tempfile::tempdir().unwrap();
        let (c, workspace, pane) = setup("com.flowmux.App.UiTest.DetachedEmpty", root.path()).await;
        let surface = c.pane_registry.borrow().active_surface(pane).unwrap();
        let terminal = c.pane_registry.borrow().terminals[&surface].clone();
        let pid = terminal.pid.get().unwrap();
        c.tear_off_surface(pane, surface).await;
        assert!(c.store.get_workspace(workspace).await.is_none());
        assert_eq!(c.stack.visible_child_name().as_deref(), Some("__empty"));
        let dst = c.pane_registry.borrow().pane_for_surface(surface).unwrap();
        let detached = c
            .detached_workspaces
            .borrow()
            .values()
            .next()
            .unwrap()
            .window
            .clone();
        c.return_surface_to_main_window(dst, surface).await.unwrap();
        wait_for("restored tab mapped", || terminal.widget.is_mapped()).await;
        assert!(c.detached_workspaces.borrow().is_empty());
        assert!(!detached.is_visible());
        assert_eq!(terminal.pid.get(), Some(pid));
        assert_eq!(
            terminal
                .widget
                .root()
                .and_downcast::<adw::ApplicationWindow>(),
            Some(c.window.clone())
        );
        c.window.destroy();
    }

    #[gtk::test]
    async fn ssh_wrapper_split_and_close_preserve_toolbar_and_terminal_pid() {
        let (controller, workspace, pane) = super::super::tests::build_single_workspace_controller(
            "com.flowmux.App.UiTest.SshWrapperSplitClose",
        )
        .await;
        let original = controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .clone();
        let original_pid = original.pid.get().expect("terminal child must be running");
        let frame = controller.pane_registry.borrow().pane_frame(pane).unwrap();
        let wrapper = gtk::Box::new(gtk::Orientation::Vertical, 4);
        wrapper.add_css_class("flowmux-ssh-workspace");
        let toolbar = gtk::Label::new(Some("SSH toolbar"));
        wrapper.append(&toolbar);
        controller.stack.remove(&frame);
        wrapper.append(&frame);
        controller
            .stack
            .add_named(&wrapper, Some(&workspace.to_string()));
        controller
            .surfaces
            .borrow_mut()
            .insert(workspace, wrapper.clone().upcast());
        let wrapper_widget: gtk::Widget = wrapper.clone().upcast();

        for direction in [SplitDirection::Vertical, SplitDirection::Horizontal] {
            assert!(can_split_pane_incrementally(
                &controller.pane_registry.borrow(),
                pane
            ));
            let (_, sibling) = controller.store.split_pane(pane, direction).await.unwrap();
            controller
                .apply_split_incremental_or_rerender(workspace, pane, sibling, direction)
                .await
                .unwrap();
            assert!(wrapper.last_child().unwrap().is::<gtk::Paned>());
            assert_eq!(wrapper.first_child().as_ref(), Some(toolbar.upcast_ref()));
            assert_eq!(
                controller.surfaces.borrow().get(&workspace),
                Some(&wrapper_widget)
            );
            let after_split = controller
                .pane_registry
                .borrow()
                .active_terminal(pane)
                .unwrap()
                .clone();
            assert_eq!(after_split.widget, original.widget);
            assert_eq!(after_split.pid.get(), Some(original_pid));

            controller.store.close_pane(sibling).await.unwrap();
            controller
                .apply_close_pane_incremental_or_rerender(workspace, sibling)
                .await;
            assert_eq!(wrapper.last_child().as_ref(), Some(&frame));
            assert_eq!(wrapper.first_child().as_ref(), Some(toolbar.upcast_ref()));
            assert_eq!(toolbar.next_sibling().as_ref(), Some(&frame));
            assert_eq!(
                controller.surfaces.borrow().get(&workspace),
                Some(&wrapper_widget)
            );
            assert_eq!(
                controller.stack.child_by_name(&workspace.to_string()),
                Some(wrapper_widget.clone())
            );
            let survivor = controller
                .pane_registry
                .borrow()
                .active_terminal(pane)
                .unwrap()
                .clone();
            assert_eq!(survivor.widget, original.widget);
            assert_eq!(survivor.pid.get(), Some(original_pid));
            assert_eq!(
                unsafe { libc::kill(original_pid, 0) },
                0,
                "surviving PTY process must remain alive"
            );
            assert!(controller
                .pane_registry
                .borrow()
                .pane_frame(sibling)
                .is_none());
        }
        original.close_pty();
    }
}
