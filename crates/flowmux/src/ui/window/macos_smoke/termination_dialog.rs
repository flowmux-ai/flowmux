// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use vte::prelude::*;

pub(super) async fn check(controller: &WindowController, workspace: WorkspaceId, pane: PaneId) {
    println!("MACOS_TERMINATION_DIALOG_START");
    let original = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let original_pid = controller.pane_registry.borrow().terminals[&original]
        .pid
        .get();
    let (_, surface) = controller
        .store
        .add_terminal_surface_to_pane(pane, None)
        .await
        .unwrap();
    controller
        .attach_or_rerender_surface(workspace, pane, surface, None)
        .await
        .unwrap();
    controller.pane_registry.borrow().terminals[&surface]
        .widget
        .feed_child(b"kill -KILL $$\n");
    wait_until("termination explanation", || {
        controller.current_termination_dialog().is_some()
    })
    .await;
    let dialog = controller.current_termination_dialog().unwrap();
    println!("MACOS_TERMINATION_DEAD_TAB_DIALOG");
    let heading = dialog.heading().unwrap();
    assert!(
        heading.contains("SIGKILL") || heading.contains("low on memory"),
        "{heading}"
    );
    assert_eq!(dialog.response_label("close"), "Close tab");
    glib::timeout_future(Duration::from_millis(300)).await;
    let (ack, rx) = oneshot::channel();
    controller
        .dispatch(GtkCommand::CloseSurface { pane, surface, ack })
        .await;
    rx.await.unwrap().unwrap();
    println!("MACOS_TERMINATION_DEAD_TAB_CLOSED");
    wait_until("closed tab's termination explanation dismissed", || {
        controller.current_termination_dialog().is_none() && !dialog.is_mapped()
    })
    .await;
    wait_until("dead tab removed", || {
        !controller
            .pane_registry
            .borrow()
            .terminals
            .contains_key(&surface)
    })
    .await;
    assert_eq!(
        controller.pane_registry.borrow().terminals[&original]
            .pid
            .get(),
        original_pid
    );
    assert!(controller
        .store
        .surface_title(pane, original)
        .await
        .is_some());
    // Close a new live sibling so later smoke scenarios keep their original terminal.
    let (_, surface) = controller
        .store
        .add_terminal_surface_to_pane(pane, None)
        .await
        .unwrap();
    controller
        .attach_or_rerender_surface(workspace, pane, surface, None)
        .await
        .unwrap();
    let (ack, rx) = oneshot::channel();
    controller
        .dispatch(GtkCommand::CloseSurface { pane, surface, ack })
        .await;
    rx.await.unwrap().unwrap();
    glib::timeout_future(Duration::from_millis(500)).await;
    assert!(controller.current_termination_dialog().is_none());
    println!("MACOS_TERMINATION_LIVE_TAB_CLOSED");

    let (_, surface) = controller
        .store
        .add_terminal_surface_to_pane(pane, None)
        .await
        .unwrap();
    controller
        .attach_or_rerender_surface(workspace, pane, surface, None)
        .await
        .unwrap();
    let terminal = controller.pane_registry.borrow().terminals[&surface].clone();
    controller.tear_off_surface(pane, surface).await;
    println!("MACOS_TERMINATION_TORN_OFF");
    let torn = controller.torn_off_terminals.borrow()[&surface]
        .window
        .upgrade()
        .unwrap();
    terminal.widget.feed_child(b"kill -KILL $$\n");
    wait_until("torn-off termination explanation", || {
        controller.current_termination_dialog().is_some()
    })
    .await;
    let dialog = controller.current_termination_dialog().unwrap();
    println!("MACOS_TERMINATION_TORN_OFF_DIALOG");
    assert_eq!(
        dialog.root().and_downcast::<adw::ApplicationWindow>(),
        Some(torn.clone())
    );
    assert!(terminal.pid.get().is_none());
    glib::timeout_future(Duration::from_millis(300)).await;
    let settings = dialog.settings();
    let animations = settings.is_gtk_enable_animations();
    settings.set_gtk_enable_animations(false);
    dialog.close();
    wait_until("terminated torn-off window closed", || !torn.is_visible()).await;
    settings.set_gtk_enable_animations(animations);
    assert!(!controller
        .torn_off_terminals
        .borrow()
        .contains_key(&surface));
    assert_eq!(
        controller.pane_registry.borrow().terminals[&original]
            .pid
            .get(),
        original_pid
    );
    println!("MACOS_TERMINATION_DIALOG_OK");
}
