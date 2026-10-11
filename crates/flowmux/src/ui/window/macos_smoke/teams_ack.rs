// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_ipc::{server::Handler, Request, Response, RpcError};

pub(super) async fn check(controller: &WindowController, root: &std::path::Path) {
    for fail_second in [true, false] {
        let (bridge, rx) = Bridge::new();
        let handler = crate::ipc_handler::GuiHandler::new(
            flowmux_daemon::DaemonHandler::new(controller.store.clone()),
            bridge,
        );
        let request = handler.handle(Request::ClaudeTeams {
            count: 2,
            args: vec!["task with spaces".into(), "--continue".into()],
            root: root.into(),
        });
        tokio::pin!(request);
        let mut created = None;
        let mut inputs = 0;
        let mut markers = Vec::new();
        let response = loop {
            tokio::select! {
                response = &mut request => break response,
                command = rx.recv() => {
                    let command = command.unwrap();
                    if let GtkCommand::WorkspaceCreated { id, .. } = &command {
                        created = Some(*id);
                    }
                    if let GtkCommand::PaneSendKeys { pane, .. } = &command {
                        inputs += 1;
                        if fail_second && inputs == 2 {
                            let (ack, reply) = tokio::sync::oneshot::channel();
                            controller.dispatch(GtkCommand::CloseFocused { pane: *pane, ack }).await;
                            reply.await.unwrap().unwrap();
                        } else {
                            let terminal = controller.pane_registry.borrow().active_terminal(*pane).unwrap().clone();
                            let marker = root.join(format!("team-{fail_second}-{inputs}"));
                            let ready = marker.with_extension("ready");
                            // Exercise delivery without starting an authenticated agent.
                            terminal.write_input(format!(
                                "claude() {{ printf '%s\\n' \"$@\" > '{}'; }}; printf ready > '{}'\n",
                                marker.display(), ready.display(),
                            ).as_bytes()).unwrap();
                            wait_until("fixture shell function ready", || ready.exists()).await;
                            markers.push(marker);
                        }
                    }
                    controller.dispatch(command).await;
                }
            }
        };
        let workspace = created.unwrap();
        if fail_second {
            let Response::Error(RpcError::Internal(error)) = response else {
                panic!("closed-pane input reported as success: {response:?}")
            };
            assert!(
                error.contains(&workspace.to_string())
                    && error.contains("partial")
                    && error.contains("1"),
                "{error}"
            );
        } else {
            assert!(matches!(response, Response::WorkspaceCreated { id } if id == workspace));
        }
        assert_eq!(inputs, 2);
        for marker in markers {
            wait_until("shell received team arguments", || marker.exists()).await;
            assert_eq!(
                std::fs::read_to_string(marker).unwrap(),
                "task with spaces\n--continue\n"
            );
        }
        assert!(controller.store.get_workspace(workspace).await.is_some());
    }
    println!("MACOS_TEAMS_ACK_OK");
}
