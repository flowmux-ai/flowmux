// SPDX-License-Identifier: GPL-3.0-or-later
use flowmux_core::{Pane, PaneContent, SshTarget, SshWorkspaceConfig, SurfaceKind};
use flowmux_daemon::StateStore;
use flowmux_ipc::{client::Client, Request, Response};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Daemon(Child);
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn empty_ssh_tabs_remain_remote_and_persist_after_daemon_start() {
    for tmux in [false, true] {
        let dir = tempfile::Builder::new()
            .prefix("fm-a07-")
            .tempdir_in("/tmp")
            .unwrap();
        for key in [
            "XDG_STATE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "FLOWMUX_RUNTIME_DIR",
        ] {
            std::env::set_var(key, dir.path());
        }
        let seed = StateStore::new_lazy_ephemeral(flowmux_state::State::default());
        let id = seed
            .create_ssh_workspace(
                None,
                SshWorkspaceConfig {
                    target: SshTarget::parse("audit.invalid").unwrap(),
                    cwd: Some("/srv/project".into()),
                    tmux,
                    forwards: vec![],
                },
            )
            .await
            .unwrap();
        let mut state = seed.snapshot().await;
        let Pane::Leaf {
            id: pane,
            content: PaneContent::Tabs { surfaces, .. },
        } = &mut state.workspaces[0].surfaces[0].root_pane
        else {
            unreachable!()
        };
        surfaces.clear();
        let pane = *pane;
        flowmux_state::save(&state).unwrap();
        let socket = dir.path().join("daemon.sock");
        let daemon = Daemon(
            Command::new(env!("CARGO_BIN_EXE_flowmux-daemon"))
                .arg("--socket")
                .arg(&socket)
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let client = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(client) = Client::connect(&socket).await {
                    break client;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let Response::Tree { workspaces } = client.call(Request::WorkspaceTree).await.unwrap()
        else {
            panic!("tree response missing")
        };
        assert_eq!(workspaces[0].id, id);
        assert_eq!(workspaces[0].panes[0].id, pane);
        assert_eq!(workspaces[0].panes[0].tabs.len(), 1);
        assert_eq!(workspaces[0].panes[0].tabs[0].kind, "ssh_terminal");
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let disk = flowmux_state::load().unwrap();
                if let Some(tab) = disk.workspaces[0].surfaces[0].root_pane.find_leaf_content(pane).unwrap().active_surface() {
                    assert!(matches!(&tab.kind, SurfaceKind::SshTerminal { cwd, tmux_session } if cwd.as_deref() == Some("/srv/project") && tmux_session.is_some() == tmux));
                    assert_eq!(tab.id, workspaces[0].panes[0].tabs[0].id);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await.expect("normalized SSH state did not persist");
        drop(client);
        drop(daemon);
        flowmux_state::load().unwrap();
        println!("A07_DAEMON_SSH_RESTORE_OK tmux={tmux}");
    }
}
