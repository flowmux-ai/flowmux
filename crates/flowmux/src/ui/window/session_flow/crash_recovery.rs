// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in real Codex crash checkpoint. Only the external test driver kills its own process.

use super::*;

pub(super) fn prevent_publication(report: &std::path::Path) {
    assert!(report.starts_with("/tmp") && !report.exists());
    let database = FlowStore::default_path().unwrap();
    assert!(database.starts_with("/tmp"));
    rusqlite::Connection::open(database)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER session_flow_test_publication_failure BEFORE INSERT ON forks
             BEGIN SELECT RAISE(ABORT, 'injected Session Flow publication failure'); END;",
        )
        .unwrap();
}

pub(super) async fn await_crash(
    controller: &WindowController,
    source: &SessionKey,
    child_surface: SurfaceId,
    report: &std::path::Path,
) {
    let saved = flowmux_state::agent_sessions::default_agent_session_store().unwrap();
    let child = glib::future_with_timeout(Duration::from_secs(35), async {
        loop {
            controller.poll_agent_processes().await;
            controller.refresh_session_flow(false).await;
            if let Some(id) = saved.lookup("codex", child_surface) {
                let home = source.home.clone();
                let records = gtk::gio::spawn_blocking(move || {
                    flowmux_state::session_history::list_sessions(SessionAgent::Codex, &home)
                })
                .await
                .unwrap()
                .unwrap();
                if records.iter().any(|session| {
                    session.id == id
                        && session.fork_parent().unwrap().as_deref() == Some(&source.id)
                }) {
                    break SessionKey {
                        id,
                        ..source.clone()
                    };
                }
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("native child and saved surface identity before publication");
    // Let the normal observer encounter the injected persistence failure.
    glib::timeout_future(Duration::from_secs(2)).await;
    let store = FlowStore::open(&FlowStore::default_path().unwrap()).unwrap();
    let operations = store.pending().unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].source, *source);
    assert_eq!(operations[0].target_surface, child_surface.0);
    assert!(!store
        .snapshot()
        .unwrap()
        .lineage
        .edges()
        .any(|(_, key)| key == &child));
    controller.store.save_now().await.unwrap();
    let pid = controller.pane_registry.borrow().terminals[&child_surface]
        .pid
        .get()
        .unwrap();
    let evidence = serde_json::json!({
        "checkpoint":"native child and saved surface exist; operation pending; fork unpublished",
        "pid":std::process::id(),"child_pty_pid":pid,"source":source,"child":child,
        "operation":operations[0],"database":FlowStore::default_path().unwrap(),
        "config_home":std::env::var("XDG_CONFIG_HOME").unwrap(),
        "data_home":std::env::var("XDG_DATA_HOME").unwrap(),
        "state_home":std::env::var("XDG_STATE_HOME").unwrap(),
        "runtime_dir":std::env::var("FLOWMUX_RUNTIME_DIR").unwrap(),
        "trigger":"session_flow_test_publication_failure"
    });
    std::fs::write(report, serde_json::to_vec_pretty(&evidence).unwrap()).unwrap();
    println!("MACOS_SESSION_FLOW_PRE_PUBLICATION_CHECKPOINT");
    glib::timeout_future(Duration::from_secs(60)).await;
    panic!("external crash driver did not terminate its isolated native process");
}
