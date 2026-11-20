// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::ui::session_panel::SessionPanelAction;
use flowmux_state::session_history::{list_sessions, SessionAgent};
use std::os::unix::fs::PermissionsExt;
use vte::prelude::*;

pub(super) async fn check(controller: &WindowController, pane: PaneId, root: &std::path::Path) {
    let socket = flowmux_config::paths::runtime_socket_for_pid(std::process::id());
    let handler = Arc::new(crate::ipc_handler::GuiHandler::new(
        flowmux_daemon::DaemonHandler::new(controller.store.clone()),
        controller.bridge.clone(),
    ));
    let server_socket = socket.clone();
    let server = tokio::spawn(async move {
        flowmux_ipc::server::run(&server_socket, handler)
            .await
            .unwrap();
    });
    wait_until("session paths IPC listening", || socket.exists()).await;
    let custom = root.join("custom codex 한글");
    let project = root.join("project ' quoted");
    let bin = root.join("fixture-bin");
    for dir in [custom.join("sessions"), project.clone(), bin.clone()] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let id = "12345678-1234-1234-1234-123456789abc";
    let history = custom.join("sessions/rollout-fixture.jsonl");
    let meta =
        serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":project,"source":"cli"}});
    let message = serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"custom history fixture"}]}});
    std::fs::write(history, format!("{meta}\n{message}\n")).unwrap();
    let script = root.join("codex.js");
    std::fs::write(
        &script,
        "console.log('SESSION_PATH_READY');setInterval(()=>{},1000)",
    )
    .unwrap();
    let marker = root.join("resumed-session");
    let node = std::process::Command::new("node")
        .args(["-p", "process.execPath"])
        .output()
        .unwrap();
    assert!(node.status.success(), "resolve fixture Node executable");
    let node = String::from_utf8(node.stdout).unwrap();
    let managed_script = bin.join("codex.js");
    std::fs::write(
        &managed_script,
        format!(
            "const fs=require('fs');const marker={};\n\
             fs.writeFileSync(marker+'.tmp',[process.env.CODEX_HOME,process.cwd(),process.env.NO_COLOR ?? '<unset>',...process.argv.slice(2)].join('\\n')+'\\n');\n\
             fs.renameSync(marker+'.tmp',marker);\n\
             process.stdin.once('data',()=>process.exit(0));process.stdin.resume();\n",
            serde_json::to_string(&marker).unwrap()
        ),
    )
    .unwrap();
    let fake = bin.join("codex");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n[ \"$1\" = --help ] && exit 0\nexec {} {} \"$@\"\n",
            shell_quote(node.trim()),
            shell_quote(managed_script.to_str().unwrap())
        ),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Longer than macOS MAX_CANON even on a CI host with a short PATH.
    let path = format!(
        "{}:{}{}",
        bin.display(),
        std::env::var("PATH").unwrap(),
        ":/unused-fixture-path".repeat(80)
    );
    let terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(pane)
        .unwrap()
        .clone();
    let original_surface = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let start = root.join("start-agent.sh");
    std::fs::write(
        &start,
        format!(
            "exec env CODEX_HOME={} PATH={} node {}\n",
            shell_quote(custom.to_str().unwrap()),
            shell_quote(&path),
            shell_quote(script.to_str().unwrap())
        ),
    )
    .unwrap();
    terminal
        .widget
        .feed_child(format!("/bin/sh {}\n", shell_quote(start.to_str().unwrap())).as_bytes());
    wait_until("custom agent started", || {
        terminal
            .widget
            .text_format(vte::Format::Text)
            .is_some_and(|text| text.contains("SESSION_PATH_READY"))
    })
    .await;
    controller.poll_agent_processes().await;
    controller.focus_pane(pane);
    controller
        .dispatch_session_panel(SessionPanelAction::Toggle)
        .await;
    wait_until("custom session listed", || {
        controller
            .sessions
            .panel
            .status
            .text()
            .contains("Codex · 1 sessions")
    })
    .await;
    let session = list_sessions(SessionAgent::Codex, &custom)
        .unwrap()
        .remove(0);
    let generation = controller.sessions.panel.generation.get();
    controller
        .dispatch_session_panel(SessionPanelAction::Select {
            session: session.clone(),
            generation,
        })
        .await;
    wait_until("custom session preview", || {
        controller.sessions.panel.resume.is_sensitive()
    })
    .await;
    let buffer = controller.sessions.panel.preview.buffer();
    assert!(buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .contains("custom history fixture"));
    std::env::set_var("NO_COLOR", "1");
    controller
        .dispatch_session_panel(SessionPanelAction::Resume {
            session,
            generation,
            yolo: true,
        })
        .await;
    wait_for_resume(controller, pane, &marker, "custom session").await;
    let output = std::fs::read_to_string(&marker).unwrap();
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 6);
    assert_eq!(lines[0], custom.to_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(lines[1]).unwrap(),
        project.canonicalize().unwrap()
    );
    assert_eq!(
        &lines[2..],
        &[
            "<unset>",
            "resume",
            id,
            "--dangerously-bypass-approvals-and-sandbox"
        ]
    );
    close_prompt(controller, pane, "cancel").await;
    finish_managed_session(controller, pane).await;
    terminal.widget.feed_child(b"\x03");
    // Reuse the original ordinary tab as the context for the selected-provider check.
    let workspace = controller.visible_workspace().unwrap();
    controller
        .activate_workspace_at(workspace, Some((pane, Some(original_surface))))
        .await;

    // Finder's PATH lacks tools installed by the user's login startup files.
    let startup = root.join("zsh-startup");
    std::fs::create_dir(&startup).unwrap();
    std::fs::write(
        startup.join(".zprofile"),
        format!("export PATH={}:$PATH\n", shell_quote(bin.to_str().unwrap())),
    )
    .unwrap();
    std::env::set_var("SHELL", "/bin/zsh");
    std::env::set_var("ZDOTDIR", &startup);
    std::env::set_var("FLOWMUX_REAL_ZDOTDIR", &startup);
    std::env::set_var("CODEX_HOME", &custom);
    // Cargo's test executable lives in deps/, so its Host is found through PATH.
    let daemon = flowmux_terminal::find_flowmux_daemon()
        .expect("native fixture needs the built Agent Host")
        .canonicalize()
        .unwrap();
    let path = std::env::join_paths(
        [daemon.parent().unwrap().to_path_buf()]
            .into_iter()
            .chain(std::env::split_paths("/usr/bin:/bin:/usr/sbin:/sbin")),
    )
    .unwrap();
    assert!(
        !std::env::split_paths(&path).any(|dir| dir.join("codex").is_file()),
        "selected provider must only be available through login startup"
    );
    std::env::set_var("PATH", path);
    assert_eq!(
        flowmux_terminal::find_flowmux_daemon()
            .unwrap()
            .canonicalize()
            .unwrap(),
        daemon,
        "Finder-like PATH must retain the fixture's Host"
    );
    std::fs::remove_file(&marker).unwrap();
    controller
        .dispatch_session_panel(SessionPanelAction::ToggleAgent(SessionAgent::Codex))
        .await;
    wait_until("selected agent sessions listed", || {
        controller
            .sessions
            .panel
            .status
            .text()
            .contains("Codex · 1 sessions")
    })
    .await;
    let session = list_sessions(SessionAgent::Codex, &custom)
        .unwrap()
        .remove(0);
    let generation = controller.sessions.panel.generation.get();
    controller
        .dispatch_session_panel(SessionPanelAction::Resume {
            session,
            generation,
            yolo: false,
        })
        .await;
    wait_for_resume(
        controller,
        pane,
        &marker,
        "selected session login shell PATH",
    )
    .await;
    let output = std::fs::read_to_string(&marker).unwrap();
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], custom.to_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(lines[1]).unwrap(),
        project.canonicalize().unwrap()
    );
    assert_eq!(&lines[2..], &["<unset>", "resume", id]);
    close_prompt(controller, pane, "stop").await;
    server.abort();
    let _ = server.await;
    println!("MACOS_SELECTED_SESSION_PATH_OK");
    println!("MACOS_SESSION_PATHS_OK");
}

async fn wait_for_resume(
    controller: &WindowController,
    pane: PaneId,
    marker: &std::path::Path,
    description: &str,
) {
    let resumed = glib::future_with_timeout(Duration::from_secs(30), async {
        while !marker.exists() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await;
    if resumed.is_err() {
        let active = controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .clone();
        panic!(
            "{description} resume failed: {}; screen: {:?}",
            controller.sessions.panel.status.text(),
            active.widget.text_format(vte::Format::Text)
        );
    }
}

/// Press the managed tab's close button and answer its prompt. `cancel` keeps
/// the tab; `stop` closes it and terminates the execution in the real Host.
async fn close_prompt(controller: &WindowController, pane: PaneId, response: &str) {
    // The provider can write its marker before the window has opened the tab for it.
    let bound = glib::future_with_timeout(Duration::from_secs(30), async {
        loop {
            let surface = controller
                .pane_registry
                .borrow()
                .active_surface(pane)
                .unwrap();
            if let Some(binding) = controller.store.surface_agent_binding(pane, surface).await {
                break (surface, binding);
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await;
    let (surface, binding) = bound.unwrap_or_else(|_| {
        panic!(
            "resume must create a managed tab: {}",
            controller.sessions.panel.status.text()
        )
    });
    wait_until("managed execution observed running", || {
        controller.managed_session_agent(binding).is_some()
    })
    .await;
    let (ack, closed) = oneshot::channel();
    let pressed = controller.clone();
    glib::MainContext::default().spawn_local(async move {
        pressed
            .dispatch(GtkCommand::CloseSurfaceButton { pane, surface, ack })
            .await;
    });
    wait_until("agent tab close prompt", || {
        controller.window.visible_dialog().is_some()
    })
    .await;
    let dialog = controller
        .window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .expect("close prompt is an alert");
    assert_eq!(dialog.heading().as_deref(), Some("Close agent tab?"));
    assert_eq!(dialog.default_response().as_deref(), Some("keep"));
    for id in ["cancel", "stop", "keep"] {
        assert!(dialog.has_response(id), "missing response {id}");
    }
    dialog.emit_by_name::<()>("response", &[&response]);
    let result = glib::future_with_timeout(Duration::from_secs(30), closed)
        .await
        .expect("close prompt answered")
        .unwrap();
    dialog.force_close();
    wait_until("close prompt dismissed", || {
        controller.window.visible_dialog().is_none()
    })
    .await;
    if response == "cancel" {
        assert_eq!(result, Err("Tab close cancelled".into()));
        assert_eq!(
            controller.store.surface_agent_binding(pane, surface).await,
            Some(binding),
            "cancel keeps the managed tab"
        );
        assert!(controller.managed_session_agent(binding).is_some());
        println!("MACOS_AGENT_TAB_CLOSE_CANCEL_OK");
        return;
    }
    assert_eq!(result, Ok(()));
    assert_eq!(
        controller.store.surface_agent_binding(pane, surface).await,
        None,
        "the tab closes"
    );
    wait_until("terminated execution ended", || {
        controller
            .bridge
            .agent_registry
            .borrow()
            .agents
            .iter()
            .any(|agent| {
                agent.instance == binding.instance
                    && agent.state == flowmux_ipc::agent_host::ProcessState::Exited
            })
    })
    .await;
    let host_socket = flowmux_config::paths::agent_host_socket();
    wait_until("empty Host retired", || !host_socket.exists()).await;
    println!("MACOS_AGENT_TAB_CLOSE_TERMINATE_OK");
}

async fn finish_managed_session(controller: &WindowController, pane: PaneId) {
    let surface = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let binding = controller
        .store
        .surface_agent_binding(pane, surface)
        .await
        .expect("resume must create a managed tab");
    // Observe the new managed target after exec, rather than a transient launch status.
    controller.refresh_session_panel(true).await;
    wait_until("managed session history remains available", || {
        controller
            .sessions
            .panel
            .status
            .text()
            .contains("Codex · 1 sessions")
    })
    .await;
    let terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(pane)
        .unwrap()
        .clone();
    terminal.widget.feed_child(b"\n");
    wait_until("fixture session naturally exited", || {
        controller
            .bridge
            .agent_registry
            .borrow()
            .agents
            .iter()
            .any(|agent| {
                agent.instance == binding.instance
                    && agent.epoch == binding.epoch
                    && agent.state == flowmux_ipc::agent_host::ProcessState::Exited
            })
    })
    .await;
    let host_socket = flowmux_config::paths::agent_host_socket();
    wait_until("empty Host retired", || !host_socket.exists()).await;
    let stopped = Instant::now();
    while stopped.elapsed() < Duration::from_millis(500) {
        assert!(
            !host_socket.exists(),
            "launchd must not restart an empty Host"
        );
        glib::timeout_future(Duration::from_millis(20)).await;
    }
}
