// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

pub(super) async fn check(controller: &WindowController, workspace: WorkspaceId, pane: PaneId) {
    let first = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let (_, second) = controller
        .store
        .add_terminal_surface_to_pane(pane, None)
        .await
        .unwrap();
    controller
        .attach_or_rerender_surface(workspace, pane, second, None)
        .await
        .unwrap();
    controller
        .split_pane(pane, SplitDirection::Vertical)
        .await
        .unwrap();
    glib::timeout_future(Duration::from_millis(300)).await;
    controller
        .dispatch(GtkCommand::TogglePaneZoom { pane })
        .await;
    wait_until("zoom transition finished", || {
        controller.pane_zoom.transition.borrow().is_none()
    })
    .await;
    assert!(controller.pane_zoom.active.borrow().is_some());
    let (tabs, container) = {
        let registry = controller.pane_registry.borrow();
        let tabs = registry.surface_tabs[&pane].clone();
        let container = tabs[0].1.parent().unwrap();
        (tabs, container)
    };
    let badge = {
        let mut child = container.first_child();
        loop {
            let widget = child.expect("zoom badge");
            if widget.has_css_class("flowmux-pane-zoom-badge") {
                break widget;
            }
            child = widget.next_sibling();
        }
    };
    let terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(pane)
        .unwrap()
        .clone();
    let pid = terminal.pid.get();
    for (index, expected) in [
        (1, vec![second, first]),
        (1, vec![second, first]),
        (0, vec![first, second]),
    ] {
        let (ack, reply) = oneshot::channel();
        controller
            .dispatch(GtkCommand::ReorderSurface {
                pane,
                surface: first,
                target_index: index,
                ack,
            })
            .await;
        reply.await.unwrap().unwrap();
        assert_eq!(
            badge.parent(),
            Some(container.clone()),
            "reorder detached zoom badge"
        );
        let registry = controller.pane_registry.borrow();
        assert_eq!(
            registry.surface_tabs[&pane]
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
            expected
        );
        let mut child = container.first_child();
        for id in expected {
            let widget = &tabs.iter().find(|(tab, _)| *tab == id).unwrap().1;
            assert_eq!(child.as_ref(), Some(widget));
            child = widget.next_sibling();
        }
        assert_eq!(child.as_ref(), Some(&badge));
        assert_eq!(registry.active_surface(pane), Some(second));
        assert_eq!(registry.active_terminal(pane).unwrap().pid.get(), pid);
    }
    controller
        .dispatch(GtkCommand::TogglePaneZoom { pane })
        .await;
    wait_until("restore transition finished", || {
        controller.pane_zoom.transition.borrow().is_none()
    })
    .await;
    assert!(badge.parent().is_none());
    assert!(controller.pane_zoom.active.borrow().is_none());
    println!("MACOS_ZOOM_REORDER_OK");
}
