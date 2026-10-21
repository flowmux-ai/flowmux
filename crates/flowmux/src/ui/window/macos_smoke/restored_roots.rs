// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use vte::prelude::*;

pub(super) async fn check(app: &adw::Application, root: &std::path::Path) {
    let seed = StateStore::new_lazy_ephemeral(State::default());
    let workspace = seed
        .create_workspace(Some("Restored roots".into()), root.into())
        .await;
    let mut initial = seed.snapshot().await;
    let terminal = PaneSurface::terminal("task", Some(root.into()));
    let terminal_id = terminal.id;
    let browser = PaneSurface::browser("browser", "about:blank".into());
    let browser_id = browser.id;
    let editor = PaneSurface::editor("editor", root.into());
    let editor_id = editor.id;
    let left = PaneId::new();
    let right = PaneId::new();
    initial.workspaces[0].surfaces.push(Surface {
        id: SurfaceId::new(),
        title: "extra root".into(),
        kind: SurfaceKind::Terminal {
            shell: None,
            cwd: Some(root.into()),
        },
        root_pane: Pane::Split {
            id: PaneId::new(),
            direction: SplitDirection::Vertical,
            ratio: 0.3,
            first: Box::new(Pane::Leaf {
                id: left,
                content: PaneContent::Tabs {
                    active: terminal_id,
                    surfaces: vec![terminal, browser],
                },
            }),
            second: Box::new(Pane::Leaf {
                id: right,
                content: PaneContent::Tabs {
                    active: editor_id,
                    surfaces: vec![editor],
                },
            }),
        },
    });
    let previous_owner = WindowOwner {
        instance_id: uuid::Uuid::new_v4(),
        pid: i32::MAX as u32,
    };
    flowmux_state::save_window_owned(previous_owner, initial).unwrap();
    assert_eq!(
        flowmux_state::load().unwrap().windows[0].boot_id,
        flowmux_procmon::boot_session_id()
    );
    let owner = WindowOwner::current();
    let store = StateStore::new_lazy_window(flowmux_state::claim_window(owner).unwrap(), owner);
    let (bridge, rx) = Bridge::new();
    let controller = WindowController::new(
        app,
        store.clone(),
        Arc::new(ResolvedTheme::load()),
        bridge,
        gtk::CssProvider::new(),
        None,
    );
    let ws = store.get_workspace(workspace).await.unwrap();
    controller.render_workspace(&ws);
    spawn_dispatch_loop(rx, controller.clone());
    controller.window.present();
    wait_until("restored root window mapped", || {
        controller.window.is_mapped()
    })
    .await;
    {
        let registry = controller.pane_registry.borrow();
        assert!(registry.terminals.contains_key(&terminal_id));
        assert!(registry.surface_tabs[&left]
            .iter()
            .any(|(id, _)| *id == browser_id));
        assert!(registry.surface_tabs[&right]
            .iter()
            .any(|(id, _)| *id == editor_id));
        assert_eq!(registry.surface_tabs[&left].len(), 2);
        assert_eq!(registry.surface_tabs[&right].len(), 1);
    }
    let terminal = controller.pane_registry.borrow().terminals[&terminal_id].clone();
    terminal.widget.feed_child(b"printf 'A06_%s\\n' RESTORED\n");
    wait_until("restored terminal executes", || {
        terminal
            .widget
            .text_format(vte::Format::Text)
            .is_some_and(|text| text.contains("A06_RESTORED"))
    })
    .await;
    controller
        .activate_surface_now(left, browser_id)
        .await
        .unwrap();
    let browser = controller.pane_registry.borrow().browsers[&browser_id].clone();
    assert_eq!(javascript(&browser, "6 * 7").await, "42");
    let file = root.join("restored.txt");
    std::fs::write(&file, "restored editor").unwrap();
    controller
        .activate_surface_now(right, editor_id)
        .await
        .unwrap();
    let editor = controller.pane_registry.borrow().editors[&editor_id].clone();
    editor.open_file(&file).await.unwrap();
    editor.flush_pending_changes().await.unwrap();
    assert_eq!(editor.session_state().active_file.as_ref(), Some(&file));
    store.save_now().await.unwrap();
    let disk = StateStore::new_lazy_ephemeral(flowmux_state::load().unwrap());
    assert_eq!(disk.tab_count_in_pane(left).await, Some(2));
    assert_eq!(disk.tab_count_in_pane(right).await, Some(1));
    controller.window.destroy();
    // Simulate a saved previous boot without rebooting the user's machine.
    let mut previous_boot = flowmux_state::load().unwrap();
    for window in &mut previous_boot.windows {
        window.boot_id = Some("previous-boot".into());
    }
    flowmux_state::save(&previous_boot).unwrap();
    let claimed = flowmux_state::claim_window(WindowOwner::current()).unwrap();
    assert!(claimed.workspaces.is_empty());
    assert!(flowmux_state::load().unwrap().workspaces.is_empty());
    println!("MACOS_BOOT_RESTORE_POLICY_OK");
    println!("MACOS_RESTORED_ROOTS_OK");
}
