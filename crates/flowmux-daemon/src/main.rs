// SPDX-License-Identifier: GPL-3.0-or-later
//! `flowmux-daemon` — IPC-only entrypoint.
//!
//! Boots the same handler that the GTK app uses, but without any
//! windowing. Useful in container/CI contexts and as a smoke test that
//! the IPC layer works end-to-end.

use clap::Parser;
use flowmux_config::paths;
use flowmux_daemon::{DaemonHandler, StateStore};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

#[derive(Parser)]
#[command(name = "flowmux-daemon", version)]
struct Cli {
    /// Override the unix socket path.
    #[arg(long, env = "FLOWMUX_SOCKET")]
    socket: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Release builds stay quiet — only ERROR events surface so a
    // packaged binary doesn't spam journald with info/debug noise.
    // `FLOWMUX_LOG` still overrides for users debugging in prod.
    let default_filter = if cfg!(debug_assertions) {
        "info,flowmux=debug"
    } else {
        "error"
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("FLOWMUX_LOG")
                .unwrap_or_else(|_| default_filter.into()),
        )
        .init();

    let cli = Cli::parse();
    let socket = cli.socket.unwrap_or_else(paths::runtime_socket);
    info!(?socket, "flowmux-daemon starting");

    let store = load_store();
    let handler = Arc::new(DaemonHandler::new(store));

    flowmux_ipc::server::run(&socket, handler).await
}

fn load_store() -> StateStore {
    // Share the GUI's claim/merge policy: live processes keep their workspaces;
    // only unowned or stale-owner workspaces are restored by this daemon.
    let owner = flowmux_state::WindowOwner::current();
    let initial = match flowmux_state::claim_window(owner) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to claim state, starting empty");
            flowmux_state::State::default()
        }
    };
    let store = StateStore::new_lazy_window(initial, owner);
    store.spawn_persist(&tokio::runtime::Handle::current());
    store
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn daemon_startup_and_saves_respect_live_gui_ownership() {
        // This binary has one test and owns its environment.
        let dir = tempfile::tempdir().unwrap();
        for key in ["XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
            std::env::set_var(key, dir.path());
        }
        let legacy = StateStore::new_lazy_ephemeral(flowmux_state::State::default());
        let legacy_id = legacy
            .create_workspace(Some("legacy".into()), dir.path().into())
            .await;
        flowmux_state::save(&legacy.snapshot().await).unwrap();
        let daemon = load_store();
        assert_eq!(daemon.list_workspaces().await, vec![legacy_id]);
        let gui_owner = flowmux_state::WindowOwner::current();
        let gui =
            StateStore::new_lazy_window(flowmux_state::claim_window(gui_owner).unwrap(), gui_owner);
        let gui_id = gui
            .create_workspace(Some("GUI 한글".into()), dir.path().into())
            .await;
        gui.save_now().await.unwrap();
        let daemon_id = daemon
            .create_workspace(Some("daemon".into()), dir.path().into())
            .await;
        daemon.save_now().await.unwrap();
        let disk = flowmux_state::load().unwrap();
        assert!(
            disk.workspaces
                .iter()
                .any(|workspace| workspace.id == gui_id),
            "daemon save removed the GUI workspace"
        );
        assert_eq!(disk.workspace_owners[&gui_id], gui_owner.instance_id);
        assert_ne!(disk.workspace_owners[&daemon_id], gui_owner.instance_id);
        // A later daemon must not load either live owner's workspace.
        let later = load_store();
        assert!(later.list_workspaces().await.is_empty());
        later.save_now().await.unwrap();
        gui.remove_workspace(gui_id).await;
        gui.save_now().await.unwrap();
        let disk = flowmux_state::load().unwrap();
        assert_eq!(disk.workspaces.len(), 2);
        assert!(disk
            .workspaces
            .iter()
            .any(|workspace| workspace.id == legacy_id));
        assert!(disk
            .workspaces
            .iter()
            .any(|workspace| workspace.id == daemon_id));
    }
}
