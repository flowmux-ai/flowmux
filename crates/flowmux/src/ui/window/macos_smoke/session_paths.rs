// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::ui::session_panel::SessionPanelAction;
use flowmux_state::session_history::{list_sessions, SessionAgent};
use std::os::unix::fs::PermissionsExt;
use vte::prelude::*;

pub(super) async fn check(controller: &WindowController, pane: PaneId, root: &std::path::Path) {
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
    let fake = bin.join("codex");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\n[ \"$1\" = --help ] && exit 0\nprintf '%s\\n' \"$CODEX_HOME\" \"$PWD\" \"$@\" > {0}.tmp && mv {0}.tmp {0}\n",
            shell_quote(marker.to_str().unwrap())
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
    controller
        .dispatch_session_panel(SessionPanelAction::Resume {
            session,
            generation,
            yolo: true,
        })
        .await;
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
            "custom session resume failed: {}; screen: {:?}",
            controller.sessions.panel.status.text(),
            active.widget.text_format(vte::Format::Text)
        );
    }
    let output = std::fs::read_to_string(&marker).unwrap();
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[0], custom.to_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(lines[1]).unwrap(),
        project.canonicalize().unwrap()
    );
    assert_eq!(
        &lines[2..],
        &["resume", id, "--dangerously-bypass-approvals-and-sandbox"]
    );
    terminal.widget.feed_child(b"\x03");

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
    std::env::set_var("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
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
    wait_until("selected session uses login shell PATH", || marker.exists()).await;
    let output = std::fs::read_to_string(&marker).unwrap();
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(lines[0], custom.to_str().unwrap());
    assert_eq!(
        std::fs::canonicalize(lines[1]).unwrap(),
        project.canonicalize().unwrap()
    );
    assert_eq!(&lines[2..], &["resume", id]);
    println!("MACOS_SELECTED_SESSION_PATH_OK");
    println!("MACOS_SESSION_PATHS_OK");
}
