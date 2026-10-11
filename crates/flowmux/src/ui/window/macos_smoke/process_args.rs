// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use vte::prelude::*;

pub(super) async fn check(controller: &WindowController, pane: PaneId, root: &std::path::Path) {
    let surface = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let terminal = controller.pane_registry.borrow().terminals[&surface].clone();
    let shell = terminal.pid.get().unwrap();
    for file in ["app.js", "codex.js"] {
        std::fs::write(
            root.join(file),
            "console.log('A09_READY');setInterval(()=>{},1000)",
        )
        .unwrap();
    }
    for (arguments, expected) in [
        (
            "-e 'console.log(123);setInterval(()=>{},1000)' -- codex".to_string(),
            None,
        ),
        (format!("'{}' codex", root.join("app.js").display()), None),
        (
            format!("--no-warnings '{}' claude", root.join("codex.js").display()),
            Some("codex"),
        ),
    ] {
        terminal
            .widget
            .feed_child(format!("node {arguments}\n").as_bytes());
        let pid = Rc::new(Cell::new(None));
        wait_until("Node process started", || {
            let found = flowmux_procmon::descendants(u32::try_from(shell).unwrap())
                .unwrap()
                .into_iter()
                .find(|pid| {
                    flowmux_procmon::comm_of(*pid)
                        .is_some_and(|name| name == "node" || name == "MainThread")
                });
            pid.set(found);
            found.is_some()
        })
        .await;
        let pid = pid.get().unwrap();
        assert_eq!(
            flowmux_procmon::agent_name_for_pid(pid),
            expected,
            "{arguments}"
        );
        controller.poll_agent_processes().await;
        let presence = controller.store.located_agent_presence(surface).await;
        assert_eq!(
            presence.as_ref().map(|entry| entry.presence.name.as_str()),
            expected,
            "Agent display: {arguments}"
        );
        terminal.widget.feed_child(b"\x03");
        wait_until("Node process exited", || !flowmux_procmon::pid_alive(pid)).await;
        controller.poll_agent_processes().await;
        assert!(controller
            .store
            .located_agent_presence(surface)
            .await
            .is_none());
    }
    assert_eq!(terminal.pid.get(), Some(shell));
    println!("MACOS_AGENT_ENTRYPOINT_OK");
}
