// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_ipc::Response;

async fn cli(socket: &std::path::Path, cwd: &std::path::Path, args: &[&str]) -> Response {
    let mut command = std::process::Command::new(
        std::env::var_os("FLOWMUX_BUNDLED_CLI_PATH").expect("rebuilt CLI path"),
    );
    command
        .args(["--json", "--socket", socket.to_str().unwrap()])
        .args(args)
        .current_dir(cwd);
    let output = tokio::task::spawn_blocking(move || command.output().unwrap())
        .await
        .unwrap();
    assert!(output.status.success(), "{args:?}: {output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

pub(super) async fn check(controller: &WindowController, bridge: &Bridge, root: &std::path::Path) {
    let caller = root.join("caller");
    let child = caller.join("child");
    std::fs::create_dir_all(&child).unwrap();
    let caller = caller.canonicalize().unwrap();
    let socket = root.join("run/paths.sock");
    let server_socket = socket.clone();
    let handler = Arc::new(crate::ipc_handler::GuiHandler::new(
        flowmux_daemon::DaemonHandler::new(controller.store.clone()),
        bridge.clone(),
    ));
    let server = tokio::spawn(async move {
        flowmux_ipc::server::run(&server_socket, handler)
            .await
            .unwrap()
    });
    wait_until("CLI path IPC listening", || socket.exists()).await;
    assert_ne!(std::env::current_dir().unwrap(), caller);
    for team in [false, true] {
        let mut args = vec!["workspace", "new", "--root", "."];
        if team {
            args.push("--team");
        }
        let Response::WorkspaceCreated { id: workspace } = cli(&socket, &caller, &args).await
        else {
            panic!("workspace reply")
        };
        let ws = controller.store.get_workspace(workspace).await.unwrap();
        assert_eq!(ws.location.local_root().unwrap(), caller);
        let pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
        let marker = root.join(format!("cwd-{team}"));
        let tab = if team {
            cli(
                &socket,
                &caller,
                &[
                    "team-spawn",
                    &pane.to_string(),
                    "--cwd",
                    "child",
                    "--shell",
                    "/bin/sh",
                    "--role",
                    "worker",
                ],
            )
            .await
        } else {
            cli(
                &socket,
                &caller,
                &[
                    "new-tab",
                    "--workspace",
                    &workspace.to_string(),
                    "--cwd",
                    "child",
                ],
            )
            .await
        };
        let terminal = if team {
            let Response::SurfaceCreated { pane, .. } = tab else {
                panic!("{tab:?}")
            };
            controller
                .pane_registry
                .borrow()
                .active_terminal(pane)
                .unwrap()
                .clone()
        } else {
            assert!(matches!(tab, Response::SurfaceCreated { .. }));
            controller
                .pane_registry
                .borrow()
                .active_terminal(pane)
                .unwrap()
                .clone()
        };
        terminal
            .write_input(format!("pwd -P > '{}'\n", marker.display()).as_bytes())
            .unwrap();
        wait_until("CLI terminal cwd output", || {
            std::fs::read_to_string(&marker).is_ok_and(|text| !text.is_empty())
        })
        .await;
        assert_eq!(
            std::fs::read_to_string(marker).unwrap().trim(),
            child.canonicalize().unwrap().to_str().unwrap()
        );
    }
    server.abort();
    println!("MACOS_CLI_CALLER_PATHS_OK");
}
