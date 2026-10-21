// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

impl WindowController {
    pub(crate) async fn check_native_ssh_preview_route(app: &adw::Application) {
        let store = StateStore::new_lazy_ephemeral(flowmux_state::State::default());
        let id = uuid::Uuid::new_v4();
        let workspace = store
            .create_ssh_workspace(
                None,
                SshWorkspaceConfig {
                    target: SshTarget::parse("127.0.0.1").unwrap(),
                    cwd: None,
                    tmux: false,
                    forwards: vec![SshForwardSpec {
                        id,
                        remote_port: 8000,
                        local_port: None,
                        https: false,
                    }],
                },
            )
            .await
            .unwrap();
        let (bridge, rx) = Bridge::new();
        let controller = Self::new(
            app,
            store.clone(),
            Arc::new(ResolvedTheme::load()),
            bridge,
            gtk::CssProvider::new(),
            None,
        );
        let ws = store.get_workspace(workspace).await.unwrap();
        controller.ensure_ssh_runtime(&ws);
        let runtime = controller.pane_registry.borrow().ssh[&workspace].clone();
        // OpenSSH adds a dot and a 16-character temporary suffix to ControlPath.
        assert!(
            runtime.borrow().socket.as_os_str().as_encoded_bytes().len() + 19 < 104,
            "macOS SSH control path must leave room for the generation and OpenSSH suffix"
        );
        let binding = format!("flowmux-ssh-preview://{id}");
        assert!(runtime.borrow().preview_url(&binding).is_none());
        assert_eq!(
            controller
                .open_ssh_preview(workspace, id)
                .await
                .unwrap_err(),
            "SSH is not connected"
        );
        runtime.borrow_mut().state = "connected";
        assert_eq!(
            controller
                .open_ssh_preview(workspace, id)
                .await
                .unwrap_err(),
            "Forward is not active"
        );
        runtime.borrow_mut().forwards.insert(id, 9);
        assert_eq!(
            runtime.borrow().preview_url(&binding).as_deref(),
            Some("http://127.0.0.1:9")
        );
        controller.render_workspace(&ws);
        spawn_dispatch_loop(rx, controller.clone());
        controller.window.present();
        let opened = controller.open_ssh_preview(workspace, id).await.unwrap();
        let surface: SurfaceId = serde_json::from_value(opened["surface"].clone()).unwrap();
        let pane = controller.pane_registry.borrow().browsers[&surface].clone();
        assert!(pane.root.is_sensitive());
        assert_eq!(runtime.borrow().previews.len(), 1);
        let snapshot = store.snapshot().await;
        let saved = serde_json::to_string(&snapshot).unwrap();
        assert!(saved.contains(&binding), "persist the forward binding");
        assert!(
            !saved.contains("http://127.0.0.1:9"),
            "never persist a leased local port"
        );
        runtime.borrow_mut().expire_preview(Some(id));
        assert!(!pane.root.is_sensitive());
        assert!(runtime.borrow().previews.is_empty());
        runtime.borrow_mut().state = "disconnected";
        assert!(runtime.borrow().preview_url(&binding).is_none());
        runtime.borrow_mut().state = "connected";
        runtime.borrow_mut().forwards.insert(id, 10);
        runtime.borrow_mut().config.forwards[0].https = true;
        assert_eq!(
            runtime.borrow().preview_url(&binding).as_deref(),
            Some("https://127.0.0.1:10")
        );
        controller.window.destroy();
        println!("MACOS_SSH_PREVIEW_ROUTE_OK");
    }
}
