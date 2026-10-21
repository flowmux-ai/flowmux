// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use vte::prelude::*;

pub(super) async fn check(app: &adw::Application, root: &std::path::Path) {
    // Exercise the fallback with a real direct shell, without pty-tee's OSC 7.
    let original_path = std::env::var_os("PATH");
    std::env::set_var("PATH", "/usr/bin:/bin");
    assert!(flowmux_terminal::find_flowmuxctl().is_none());
    let store = StateStore::new_lazy_ephemeral(State::default());
    let workspace = store.create_workspace(None, root.into()).await;
    let ws = store.get_workspace(workspace).await.unwrap();
    let pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
    let (bridge, rx) = Bridge::new();
    let controller = WindowController::new(
        app,
        store.clone(),
        Arc::new(ResolvedTheme::load()),
        bridge,
        gtk::CssProvider::new(),
        None,
    );
    controller.render_workspace(&ws);
    match original_path {
        Some(path) => std::env::set_var("PATH", path),
        None => std::env::remove_var("PATH"),
    }
    spawn_dispatch_loop(rx, controller.clone());
    controller.window.present();
    wait_until("cwd test window mapped", || controller.window.is_mapped()).await;
    controller.focus_pane(pane);
    controller.show_file_browser_for_pane(pane).await;
    let surface = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let terminal = controller.pane_registry.borrow().terminals[&surface].clone();
    for name in ["cwd first", "한글 second"] {
        let directory = root.join(name);
        std::fs::create_dir(&directory).unwrap();
        let expected = directory.canonicalize().unwrap();
        let marker = directory.join("ready");
        terminal.widget.feed_child(
            format!(
                "cd {}; printf ready > ready\n",
                shell_quote(directory.to_str().unwrap())
            )
            .as_bytes(),
        );
        wait_until("shell changed directory", || marker.exists()).await;
        assert_eq!(terminal.announced_current_dir(), None);
        assert_eq!(terminal.current_dir(), Some(expected.clone()));
        assert!(controller
            .pane_registry
            .borrow()
            .terminal_cwd_poll_inputs()
            .iter()
            .any(|(_, id, _, _)| *id == surface));
        controller.poll_terminal_cwds().await;
        assert!(controller.file_browser.panel.is_showing_root(&expected));
        assert_eq!(
            store.surface_title(pane, surface).await.as_deref(),
            Some(name)
        );
        assert_eq!(
            controller
                .pane_registry
                .borrow()
                .surface_title_text(surface)
                .as_deref(),
            Some(name)
        );
    }
    controller.window.destroy();
    println!("MACOS_CWD_POLL_OK");
}
