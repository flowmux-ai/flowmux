// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in two-GUI graph activation against a shared disposable Codex store.

use super::*;

fn publish(directory: &std::path::Path, name: &str, value: serde_json::Value) {
    let temporary = directory.join(format!("{name}.tmp"));
    std::fs::write(&temporary, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    std::fs::rename(temporary, directory.join(format!("{name}.json"))).unwrap();
}

async fn wait_for_file(controller: &WindowController, path: &std::path::Path) {
    glib::future_with_timeout(Duration::from_secs(90), async {
        while !path.is_file() {
            controller.poll_agent_processes().await;
            controller.refresh_session_flow(true).await;
            glib::timeout_future(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("two-GUI barrier timed out: {}", path.display()));
}

pub(super) async fn check(
    controller: &WindowController,
    workspace: WorkspaceId,
    pane: PaneId,
    source: &SessionKey,
    directory: &std::path::Path,
) {
    assert!(directory.starts_with("/tmp") && directory.join("test-owned").is_file());
    let role = std::env::var("FLOWMUX_SESSION_FLOW_MULTI_GUI_ROLE").unwrap();
    assert!(matches!(role.as_str(), "a" | "b"));
    let target = SessionKey {
        id: std::env::var("FLOWMUX_SESSION_FLOW_MULTI_GUI_TARGET").unwrap(),
        ..source.clone()
    };
    assert_ne!(source, &target);
    let terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(pane)
        .unwrap()
        .clone();
    let original_pid = terminal.pid.get();
    let original_widget = terminal.widget.clone();
    let original_surface = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let database = FlowStore::default_path().unwrap();
    glib::future_with_timeout(Duration::from_secs(20), async {
        loop {
            controller.poll_agent_processes().await;
            controller.refresh_session_flow(true).await;
            if controller
                .flow
                .catalog
                .borrow()
                .as_ref()
                .is_some_and(|catalog| {
                    catalog.resumable.contains(&target)
                        && catalog
                            .snapshot
                            .lineage
                            .project([source])
                            .nodes()
                            .contains(&target)
                })
            {
                break;
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("shared native target was not available in the graph");
    controller.flow.panel.toggle(workspace);
    glib::timeout_future(Duration::from_millis(200)).await;
    publish(
        directory,
        &format!("{role}-ready"),
        serde_json::json!({
            "pid":std::process::id(),"owner":controller.flow.owner,"source":source,"target":target,
            "workspace":workspace,"source_pane":pane,"source_surface":original_surface,
            "database":database,"socket":flowmux_config::paths::runtime_socket_for_pid(std::process::id())
        }),
    );
    wait_for_file(controller, &directory.join("go")).await;
    let clicked_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_micros();
    assert!(controller.flow.panel.try_click_native_node(&target));
    let live = glib::future_with_timeout(Duration::from_secs(40), async {
        loop {
            controller.poll_agent_processes().await;
            controller.refresh_session_flow(true).await;
            let db = FlowStore::open(&database).unwrap();
            let owners = db
                .live_sessions()
                .unwrap()
                .remove(&target)
                .unwrap_or_default();
            assert!(owners.len() <= 1, "multiple live owners for one target");
            if owners.len() == 1 && db.pending().unwrap().iter().all(|op| op.source != target) {
                break owners[0].clone();
            }
            glib::timeout_future(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("two-GUI resume did not settle to one owner");
    let owns_target = live.owner == controller.flow.owner;
    let expected_count = if owns_target { 2 } else { 1 };
    assert_eq!(
        controller.pane_registry.borrow().terminals.len(),
        expected_count
    );
    assert_eq!(terminal.pid.get(), original_pid);
    assert_eq!(
        controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .widget,
        original_widget
    );
    publish(
        directory,
        &format!("{role}-opened"),
        serde_json::json!({
            "pid":std::process::id(),"clicked_at_unix_micros":clicked_at,"owns_target":owns_target,
            "terminals":expected_count,"live":live,"status":controller.flow.panel.status.text().to_string(),
            "original_pid":original_pid,"original_surface":original_surface
        }),
    );
    wait_for_file(controller, &directory.join("focus-away")).await;
    controller.focus_pane(pane);
    assert_eq!(controller.focused_pane.get(), Some(pane));
    publish(
        directory,
        &format!("{role}-away"),
        serde_json::json!({"pane":pane}),
    );
    wait_for_file(controller, &directory.join("refocus")).await;
    if !owns_target {
        assert!(controller.flow.panel.try_click_native_node(&target));
    } else {
        glib::future_with_timeout(Duration::from_secs(10), async {
            while controller.focused_pane.get() != Some(PaneId(live.pane))
                || controller
                    .pane_registry
                    .borrow()
                    .active_surface(PaneId(live.pane))
                    != Some(SurfaceId(live.surface))
            {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("foreign graph click did not focus exact owner pane/surface");
        publish(
            directory,
            "foreign-focus-verified",
            serde_json::json!({
                "owner_pid":std::process::id(),"pane":live.pane,"surface":live.surface
            }),
        );
    }
    wait_for_file(controller, &directory.join("release")).await;
    assert_eq!(
        controller.pane_registry.borrow().terminals.len(),
        expected_count
    );
    assert_eq!(terminal.pid.get(), original_pid);
    assert_eq!(
        controller.pane_registry.borrow().terminals[&original_surface].widget,
        original_widget
    );
    println!("MACOS_SESSION_FLOW_MULTI_GUI_OK role={role} owns_target={owns_target}");
}
