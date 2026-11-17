// SPDX-License-Identifier: GPL-3.0-or-later
//! Window ownership and the tab-move route matrix. Native DnD is covered by the GUI harness.

use super::*;

async fn close_fixture(c: &WindowController) {
    for id in c.store.list_workspaces().await {
        c.store.remove_workspace(id).await;
        c.drop_workspace(id);
    }
    c.window.destroy();
}

fn assert_projection(c: &WindowController) {
    let main = c.sidebar.tab_drag_drop_state();
    for host in c.detached_workspaces.borrow().values() {
        let flags = host.sidebar.tab_drag_drop_state();
        assert!(Rc::ptr_eq(&main.0, &flags.0));
        assert!(Rc::ptr_eq(&main.1, &flags.1));
    }
}

async fn assert_surface(
    c: &WindowController,
    surface: SurfaceId,
    workspace: WorkspaceId,
    live: &gtk::Widget,
) {
    let pane = c.pane_registry.borrow().pane_for_surface(surface).unwrap();
    assert_eq!(c.store.workspace_of_pane(pane).await, Some(workspace));
    let mut occurrences = 0;
    for id in c.store.list_workspaces().await {
        let ws = c.store.get_workspace(id).await.unwrap();
        for tree in &ws.surfaces {
            let mut leaves = Vec::new();
            tree.root_pane.for_each_leaf(|id| leaves.push(id));
            for leaf in leaves {
                let flowmux_core::PaneContent::Tabs { surfaces, .. } =
                    tree.root_pane.find_leaf_content(leaf).unwrap()
                else {
                    panic!("non-normalized pane");
                };
                let model = surfaces.iter().map(|tab| tab.id).collect::<Vec<_>>();
                occurrences += model.iter().filter(|id| **id == surface).count();
                let registry = c.pane_registry.borrow();
                assert_eq!(
                    registry.surface_tabs[&leaf]
                        .iter()
                        .map(|(id, _)| *id)
                        .collect::<Vec<_>>(),
                    model
                );
                assert_eq!(
                    registry.stack_for_pane(leaf).unwrap().pages().n_items() as usize,
                    model.len()
                );
            }
        }
    }
    assert_eq!(occurrences, 1, "a move must have exactly one model owner");
    let registry = c.pane_registry.borrow();
    assert_eq!(registry.surface_workspace[&surface], workspace);
    assert_eq!(
        registry
            .stack_for_pane(pane)
            .unwrap()
            .child_by_name(&surface.to_string()),
        Some(live.clone())
    );
    assert_eq!(
        live.root().and_downcast::<adw::ApplicationWindow>(),
        Some(c.workspace_window(workspace))
    );
}

#[gtk::test]
async fn tab_move_matrix_preserves_all_backends_across_three_windows() {
    use webkit6::prelude::*;
    let _direct_pty = crate::ui::ghostty_pane::bypass_pty_tee_for_test();
    let root = tempfile::tempdir().unwrap();
    let (c, main_ws, main_pane) =
        surface_ops::tests::setup("com.flowmux.App.UiTest.TabMoveMatrix", root.path()).await;
    let mut hosts = vec![(main_ws, main_pane)];
    for _ in 0..2 {
        let (_, anchor) = c
            .store
            .add_terminal_surface_to_pane(main_pane, None)
            .await
            .unwrap();
        c.attach_or_rerender_surface(main_ws, main_pane, anchor, None)
            .await
            .unwrap();
        c.tear_off_surface(main_pane, anchor).await;
        let pane = c.pane_registry.borrow().pane_for_surface(anchor).unwrap();
        hosts.push((c.store.workspace_of_pane(pane).await.unwrap(), pane));
    }
    let sibling = c
        .store
        .create_workspace(Some("Main sibling".into()), root.path().into())
        .await;
    let ws = c.store.get_workspace(sibling).await.unwrap();
    let sibling_pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
    c.render_workspace_with_activation(&ws, false);
    hosts.push((sibling, sibling_pane));
    assert_projection(&c);
    for (ws, _) in &hosts {
        let sidebar = c
            .detached_workspaces
            .borrow()
            .get(ws)
            .map(|host| host.sidebar.clone())
            .unwrap_or_else(|| c.sidebar.clone());
        let mut child = sidebar.list.first_child();
        let mut visible = 0;
        while let Some(row) = child {
            visible += usize::from(row.is_visible());
            child = row.next_sibling();
        }
        assert_eq!(
            visible,
            if c.detached_workspaces.borrow().contains_key(ws) {
                1
            } else {
                2
            },
            "each side panel must show only its own workspaces"
        );
    }
    let mut count = 0;
    for kind in 0..3 {
        let (_, surface) = match kind {
            0 => c
                .store
                .add_terminal_surface_to_pane(main_pane, None)
                .await
                .unwrap(),
            1 => c
                .store
                .add_browser_surface_to_pane(main_pane, "about:blank".into())
                .await
                .unwrap(),
            _ => c
                .store
                .add_editor_surface_to_pane(main_pane, root.path().into())
                .await
                .unwrap(),
        };
        c.attach_or_rerender_surface(main_ws, main_pane, surface, None)
            .await
            .unwrap();
        let live = c
            .pane_registry
            .borrow()
            .stack_for_pane(main_pane)
            .unwrap()
            .child_by_name(&surface.to_string())
            .unwrap();
        let terminal = c.pane_registry.borrow().terminals.get(&surface).cloned();
        let pid = terminal.as_ref().and_then(|term| term.pid.get());
        let browser = c.pane_registry.borrow().browsers.get(&surface).cloned();
        async fn eval(browser: &crate::ui::browser_pane::BrowserPane, source: &str) -> String {
            let (tx, rx) = oneshot::channel();
            browser.evaluate_js(source, move |value| {
                let _ = tx.send(value);
            });
            rx.await.unwrap().unwrap()
        }
        if let Some(browser) = &browser {
            browser.load_uri("data:text/html,<title>Move audit</title><body>loaded</body>");
            wait_for("browser document loaded", || {
                browser.current_url().starts_with("data:") && !browser.web_view.is_loading()
            })
            .await;
            eval(browser, "window.moveAudit = 'alive'").await;
        }
        let editor = c.pane_registry.borrow().editors.get(&surface).cloned();
        let file = root.path().join("dirty.txt");
        if let Some(editor) = &editor {
            std::fs::write(&file, "original").unwrap();
            editor.open_file(&file).await.unwrap();
            editor.flush_pending_changes().await.unwrap();
            c.activate_workspace(main_ws).await;
            editor.grab_focus();
            editor
                .focus_widget()
                .downcast::<webkit6::WebView>()
                .unwrap()
                .execute_editing_command_with_argument("InsertText", "unsaved");
            wait_for("editor dirty", || !editor.dirty_document_paths().is_empty()).await;
        }
        for &(src_ws, src) in &hosts {
            for &(dst_ws, dst) in &hosts {
                for route in 0..5 {
                    let current = c.pane_registry.borrow().pane_for_surface(surface).unwrap();
                    c.move_surface(current, surface, None, src, usize::MAX)
                        .await
                        .unwrap();
                    assert_surface(&c, surface, src_ws, &live).await;
                    let (ack, reply) = oneshot::channel();
                    let command = match route {
                        0 | 1 if src == dst => GtkCommand::ReorderSurface {
                            pane: src,
                            surface,
                            target_index: if route == 0 { 0 } else { usize::MAX },
                            ack,
                        },
                        0 | 1 => GtkCommand::MoveSurfaceToPane {
                            src_pane: src,
                            surface,
                            surface_model: None,
                            dst_pane: dst,
                            target_index: if route == 0 { 0 } else { usize::MAX },
                            ack,
                        },
                        2 => GtkCommand::MoveSurfaceToWorkspace {
                            src_pane: src,
                            surface,
                            dst_workspace: dst_ws,
                            ack,
                        },
                        _ => GtkCommand::SplitSurfaceIntoPane {
                            src_pane: src,
                            surface,
                            surface_model: None,
                            dst_pane: dst,
                            direction: if route == 3 {
                                SplitDirection::Vertical
                            } else {
                                SplitDirection::Horizontal
                            },
                            ack,
                        },
                    };
                    c.dispatch(command).await;
                    reply.await.unwrap().unwrap_or_else(|error| {
                        panic!("kind={kind} source={src_ws} target={dst_ws} route={route}: {error}")
                    });
                    assert_surface(&c, surface, dst_ws, &live).await;
                    if let Some(terminal) = &terminal {
                        assert_eq!(terminal.pid.get(), pid);
                    }
                    if let Some(browser) = &browser {
                        assert_eq!(
                            eval(browser, "window.moveAudit").await,
                            "alive",
                            "source={src_ws} destination={dst_ws} route={route}"
                        );
                    }
                    if let Some(editor) = &editor {
                        assert_eq!(editor.dirty_document_paths(), vec![file.clone()]);
                        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
                    }
                    count += 1;
                }
            }
        }
        if let Some(editor) = &editor {
            editor.flush_pending_changes().await.unwrap();
            editor.save_all_dirty().await.unwrap();
            assert!(std::fs::read_to_string(&file).unwrap().contains("unsaved"));
        }
    }
    assert_eq!(count, 240);
    println!("TAB_MOVE_MATRIX_OK cases={count}");
    close_fixture(&c).await;
}

async fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
    glib::future_with_timeout(Duration::from_secs(10), async {
        while !ready() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out: {description}"));
}

fn button_named(root: &impl IsA<gtk::Widget>, name: &str) -> gtk::Button {
    let mut pending = vec![root.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = pending.pop() {
        if widget.widget_name() == name
            || widget
                .downcast_ref::<gtk::Button>()
                .is_some_and(|button| button.label().as_deref() == Some(name))
        {
            return widget.downcast().unwrap();
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            pending.push(widget);
        }
    }
    panic!("button missing: {name}");
}

#[gtk::test]
async fn tab_move_rejections_keep_the_source_and_never_import_a_foreign_tab() {
    let _direct_pty = crate::ui::ghostty_pane::bypass_pty_tee_for_test();
    let root = tempfile::tempdir().unwrap();
    let (c, workspace, pane) =
        surface_ops::tests::setup("com.flowmux.App.UiTest.TabMoveRejections", root.path()).await;
    let surface = c.pane_registry.borrow().active_surface(pane).unwrap();
    let live = c
        .pane_registry
        .borrow()
        .stack_for_pane(pane)
        .unwrap()
        .child_by_name(&surface.to_string())
        .unwrap();
    let pid = c.pane_registry.borrow().terminals[&surface].pid.get();
    let models = [
        flowmux_core::PaneSurface::terminal("foreign", None),
        flowmux_core::PaneSurface::browser("foreign", "about:blank".into()),
        flowmux_core::PaneSurface::editor("foreign", root.path().into()),
    ];
    for model in models {
        for split in [false, true] {
            let source = PaneId::new();
            let result = if split {
                c.split_surface_into_pane(
                    source,
                    model.id,
                    Some(model.clone()),
                    pane,
                    SplitDirection::Vertical,
                )
                .await
            } else {
                c.move_surface(source, model.id, Some(model.clone()), pane, 0)
                    .await
            };
            assert!(
                result.is_err(),
                "a foreign snapshot must not be copied/restarted as a live move"
            );
            assert_surface(&c, surface, workspace, &live).await;
            assert_eq!(c.pane_registry.borrow().terminals.len(), 1);
            assert!(c.pane_registry.borrow().browsers.is_empty());
            assert!(c.pane_registry.borrow().editors.is_empty());
        }
    }
    assert!(c
        .move_surface(pane, surface, None, PaneId::new(), 0)
        .await
        .is_err());
    assert!(c
        .split_surface_into_pane(
            pane,
            surface,
            None,
            PaneId::new(),
            SplitDirection::Horizontal
        )
        .await
        .is_err());
    assert!(c
        .move_surface_to_workspace(pane, surface, WorkspaceId::new())
        .await
        .is_err());
    assert_surface(&c, surface, workspace, &live).await;
    assert_eq!(c.pane_registry.borrow().terminals[&surface].pid.get(), pid);
    assert!(c.detached_workspaces.borrow().is_empty());
    close_fixture(&c).await;
}

#[gtk::test]
async fn workspace_creation_uses_requesting_window_despite_later_focus_change() {
    let _direct_pty = crate::ui::ghostty_pane::bypass_pty_tee_for_test();
    let root = tempfile::tempdir().unwrap();
    let (c, workspace, pane) = surface_ops::tests::setup(
        "com.flowmux.App.UiTest.WorkspaceWindowOwnership",
        root.path(),
    )
    .await;
    let (_, surface) = c
        .store
        .add_terminal_surface_to_pane(pane, None)
        .await
        .unwrap();
    c.attach_or_rerender_surface(workspace, pane, surface, None)
        .await
        .unwrap();
    c.tear_off_surface(pane, surface).await;
    let detached = c
        .detached_workspaces
        .borrow()
        .values()
        .next()
        .unwrap()
        .window
        .clone();
    for from_detached in [false, true] {
        for route in 0..4 {
            let (requester, other) = if from_detached {
                (&detached, &c.window)
            } else {
                (&c.window, &detached)
            };
            let previous = c
                .store
                .list_workspaces()
                .await
                .into_iter()
                .collect::<HashSet<_>>();
            requester.present();
            if route >= 2 {
                button_named(requester, "flowmux-workspace-creation-menu-button").emit_clicked();
                button_named(
                    requester,
                    if route == 2 {
                        "New workspace"
                    } else {
                        "New Team Workspace"
                    },
                )
                .emit_clicked();
            } else if route == 1 {
                button_named(requester, "flowmux-new-workspace-button").emit_clicked();
            } else {
                requester
                    .lookup_action("new-workspace")
                    .unwrap()
                    .activate(None);
            }
            // A queued creation belongs to the sender, even if focus changes before dispatch.
            other.present();
            let id = glib::future_with_timeout(Duration::from_secs(10), async {
                loop {
                    if let Some(ws) = c
                        .store
                        .list_workspaces()
                        .await
                        .into_iter()
                        .find(|ws| !previous.contains(ws))
                    {
                        break ws;
                    }
                    glib::timeout_future(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap();
            wait_for("workspace rendered", || {
                c.surfaces.borrow().contains_key(&id)
            })
            .await;
            assert_eq!(
                c.surfaces.borrow()[&id]
                    .root()
                    .and_downcast::<adw::ApplicationWindow>(),
                Some(requester.clone()),
                "new workspace went to another window: detached={from_detached}, route={route}"
            );
        }
    }
    close_fixture(&c).await;
}

#[gtk::test]
async fn queued_workspace_creation_does_not_fall_back_after_its_window_closes() {
    let _direct_pty = crate::ui::ghostty_pane::bypass_pty_tee_for_test();
    let root = tempfile::tempdir().unwrap();
    let (c, workspace, pane) =
        surface_ops::tests::setup("com.flowmux.App.UiTest.ClosedCreationWindow", root.path()).await;
    let (_, surface) = c
        .store
        .add_terminal_surface_to_pane(pane, None)
        .await
        .unwrap();
    c.attach_or_rerender_surface(workspace, pane, surface, None)
        .await
        .unwrap();
    c.tear_off_surface(pane, surface).await;
    let detached = c.pane_registry.borrow().surface_workspace[&surface];
    let window = c.workspace_window(detached);
    let window_id = Some(window.id());
    c.store.remove_workspace(detached).await;
    c.drop_workspace(detached);
    assert!(!window.is_visible());
    c.dispatch(GtkCommand::NewWorkspace {
        window_id,
        root: root.path().into(),
        team: false,
    })
    .await;
    assert_eq!(c.store.list_workspaces().await, vec![workspace]);
    assert!(c.detached_workspaces.borrow().is_empty());
    close_fixture(&c).await;
}
