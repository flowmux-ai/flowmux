// SPDX-License-Identifier: GPL-3.0-or-later
//! The real daemon must coexist with the GUI's claim/merge/final-save path.
use flowmux_core::WorkspaceId;
use flowmux_daemon::StateStore;
use flowmux_ipc::{client::Client, Request, Response};
use flowmux_state::{State, WindowOwner};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn start_daemon(socket: &Path) -> (Daemon, Client) {
    let daemon = Daemon(
        Command::new(env!("CARGO_BIN_EXE_flowmux-daemon"))
            .arg("--socket")
            .arg(socket)
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(client) = Client::connect(socket).await {
                if matches!(client.call(Request::Ping).await, Ok(Response::Pong)) {
                    break client;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("daemon did not become ready");
    (daemon, client)
}

async fn saved_workspace(id: WorkspaceId) -> State {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let disk = flowmux_state::load().unwrap();
            if disk.workspaces.iter().any(|workspace| workspace.id == id) {
                break disk;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("workspace was not persisted")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_and_gui_preserve_each_others_workspaces() {
    // This test binary owns its environment; never inherit a user's paths.
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("FLOWMUX_") {
            std::env::remove_var(key);
        }
    }
    for gui_first in [false, true] {
        let dir = tempfile::Builder::new()
            .prefix("fm-a01-")
            .tempdir_in("/tmp")
            .unwrap();
        for key in [
            "XDG_STATE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "FLOWMUX_RUNTIME_DIR",
        ] {
            std::env::set_var(key, dir.path());
        }
        let socket = dir.path().join("daemon.sock");
        let early_daemon = if gui_first {
            None
        } else {
            Some(start_daemon(&socket).await)
        };
        let owner = WindowOwner::current();
        let gui = StateStore::new_lazy_window(flowmux_state::claim_window(owner).unwrap(), owner);
        let gui_id = gui
            .create_workspace(Some("GUI bytes 한글 🫒".into()), dir.path().into())
            .await;
        gui.save_now().await.unwrap();
        let gui_before = format!("{:?}", gui.get_workspace(gui_id).await.unwrap());
        let (daemon, client) = match early_daemon {
            Some(pair) => pair,
            None => start_daemon(&socket).await,
        };
        let Response::WorkspaceCreated { id: daemon_id } = client
            .call(Request::WorkspaceCreate {
                team: false,
                name: Some("daemon bytes 별도".into()),
                root: dir.path().into(),
            })
            .await
            .unwrap()
        else {
            panic!("workspace creation failed")
        };
        let disk = saved_workspace(daemon_id).await;
        let saved_gui = disk
            .workspaces
            .iter()
            .find(|workspace| workspace.id == gui_id)
            .expect("daemon save removed the GUI workspace");
        assert_eq!(format!("{saved_gui:?}"), gui_before);
        assert_eq!(disk.workspace_owners[&gui_id], owner.instance_id);
        assert_ne!(disk.workspace_owners[&daemon_id], owner.instance_id);
        assert!(
            matches!(client.call(Request::WorkspaceList).await.unwrap(), Response::WorkspaceList { ids } if ids == vec![daemon_id])
        );
        let daemon_before = format!(
            "{:?}",
            disk.workspaces
                .iter()
                .find(|workspace| workspace.id == daemon_id)
                .unwrap()
        );

        gui.rename_workspace(gui_id, "GUI changed 최종".into())
            .await;
        let final_gui = gui.clone();
        tokio::task::spawn_blocking(move || final_gui.save_now_blocking())
            .await
            .unwrap()
            .unwrap();
        let disk = flowmux_state::load().unwrap();
        assert_eq!(
            format!(
                "{:?}",
                disk.workspaces
                    .iter()
                    .find(|workspace| workspace.id == daemon_id)
                    .unwrap()
            ),
            daemon_before
        );
        let bytes = std::fs::read(flowmux_state::default_path().unwrap()).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(text.contains("GUI changed 최종") && text.contains("daemon bytes 별도"));
        println!("A01 gui_first={gui_first} daemon_pid={} gui_pid={} gui={gui_id} daemon={daemon_id} persisted_bytes={}", daemon.0.id(), std::process::id(), bytes.len());

        // A restart reclaims only the dead daemon's workspaces, while GUI stays live.
        drop(client);
        drop(daemon);
        let (restarted, client) = start_daemon(&socket).await;
        assert!(
            matches!(client.call(Request::WorkspaceList).await.unwrap(), Response::WorkspaceList { ids } if ids == vec![daemon_id])
        );
        gui.remove_workspace(gui_id).await;
        gui.save_now().await.unwrap();
        let disk = flowmux_state::load().unwrap();
        assert_eq!(disk.workspaces.len(), 1);
        assert_eq!(format!("{:?}", disk.workspaces[0]), daemon_before);
        drop(client);
        drop(restarted);
        let restored = flowmux_state::claim_window(WindowOwner::current()).unwrap();
        assert_eq!(restored.workspaces.len(), 1);
        assert_eq!(format!("{:?}", restored.workspaces[0]), daemon_before);
    }
}
