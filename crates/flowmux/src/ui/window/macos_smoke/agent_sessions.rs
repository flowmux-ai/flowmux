// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_ipc::{client::Client, Request, Response};

pub(super) async fn check(store: &StateStore, bridge: &Bridge, surface: SurfaceId) {
    let socket = flowmux_config::paths::runtime_socket_for_pid(std::process::id());
    let handler = Arc::new(crate::ipc_handler::GuiHandler::new(
        flowmux_daemon::DaemonHandler::new(store.clone()),
        bridge.clone(),
    ));
    let server_socket = socket.clone();
    let server = tokio::spawn(async move {
        flowmux_ipc::server::run(&server_socket, handler)
            .await
            .unwrap();
    });
    wait_until("session IPC listening", || socket.exists()).await;
    let client = Client::connect(&socket).await.unwrap();
    let update = |agent: &str, session: &str| Request::AgentSessionUpdate {
        agent: agent.into(),
        surface,
        session_id: session.into(),
    };
    let get = |agent: &str| Request::AgentSessionGet {
        agent: agent.into(),
        surface,
    };
    assert!(matches!(
        client.call(update("claude", "old-session")).await.unwrap(),
        Response::Ok
    ));
    let directory = flowmux_config::paths::data_dir()
        .unwrap()
        .join("agent-sessions");
    let original = std::fs::read(directory.join("claude.json")).unwrap();
    let obstruction = directory.join(format!("codex.tmp.{}", std::process::id()));
    std::fs::create_dir(&obstruction).unwrap();
    assert!(matches!(
        client.call(update("codex", "new-session")).await.unwrap(),
        Response::Error(_)
    ));
    assert!(
        matches!(client.call(get("claude")).await.unwrap(), Response::AgentSession { session_id: Some(id) } if id == "old-session")
    );
    assert_eq!(
        std::fs::read(directory.join("claude.json")).unwrap(),
        original
    );
    std::fs::remove_dir(obstruction).unwrap();
    assert!(matches!(
        client.call(update("codex", "new-session")).await.unwrap(),
        Response::Ok
    ));
    assert!(matches!(
        client.call(get("claude")).await.unwrap(),
        Response::AgentSession { session_id: None }
    ));
    assert!(
        matches!(client.call(get("codex")).await.unwrap(), Response::AgentSession { session_id: Some(id) } if id == "new-session")
    );
    let sessions = flowmux_state::default_agent_session_store().unwrap();
    assert_eq!(
        sessions.take_surface(surface).unwrap().unwrap().session_id,
        "new-session"
    );
    assert!(sessions.take_surface(surface).unwrap().is_none());
    server.abort();
    let _ = server.await;
    println!("MACOS_AGENT_SESSION_IO_OK");
}
