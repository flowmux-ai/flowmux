// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use vte::prelude::*;

pub(super) async fn check(
    controller: &WindowController,
    workspace: WorkspaceId,
    pane: PaneId,
    root: &std::path::Path,
) {
    let surface = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let terminal = controller.pane_registry.borrow().terminals[&surface].clone();
    let pid = terminal.pid.get().unwrap();
    terminal
        .widget
        .feed_child(b"printf '\\033]2;A05 running task\\007'\n");
    wait_until("OSC title displayed", || {
        controller
            .pane_registry
            .borrow()
            .surface_title_text(surface)
            .as_deref()
            == Some("A05 running task")
    })
    .await;
    for kind in 0..3 {
        let (_, added) = match kind {
            0 => {
                controller
                    .store
                    .add_terminal_surface_to_pane(pane, Some(root.into()))
                    .await
            }
            1 => {
                controller
                    .store
                    .add_browser_surface_to_pane(pane, "about:blank".into())
                    .await
            }
            _ => {
                controller
                    .store
                    .add_editor_surface_to_pane(pane, root.into())
                    .await
            }
        }
        .unwrap();
        controller
            .attach_or_rerender_surface(workspace, pane, added, None)
            .await
            .unwrap();
        assert_eq!(
            controller
                .store
                .surface_title(pane, surface)
                .await
                .as_deref(),
            Some("A05 running task")
        );
        let ws = controller.store.get_workspace(workspace).await.unwrap();
        controller.rerender_workspace(&ws);
        assert_eq!(
            controller
                .pane_registry
                .borrow()
                .surface_title_text(surface)
                .as_deref(),
            Some("A05 running task")
        );
        assert_eq!(
            controller.pane_registry.borrow().terminals[&surface]
                .pid
                .get(),
            Some(pid)
        );
    }
    println!("MACOS_LIVE_TAB_TITLE_OK");
}
