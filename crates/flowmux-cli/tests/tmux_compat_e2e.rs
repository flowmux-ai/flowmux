// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end test for the Claude Code agent-teams tmux bridge.
//!
//! Reproduces the exact call chain a Claude Code lead uses when it
//! spawns teammates with `teammateMode: "tmux"` inside a flowmux pane:
//!
//! ```text
//! tmux shim (bash, installed by `flowmuxctl fix`)
//!   → flowmuxctl tmux-compat …        (real binary, real argv)
//!     → Request::TmuxCompat            (Unix socket, NDJSON)
//!       → DaemonHandler → StateStore   (in-process daemon)
//! ```
//!
//! The invocation sequence below mirrors what Claude Code v2.1.207's
//! external-swarm backend actually issues (observed argv), so this is
//! the regression guard for "agent teams keep working in flowmux".

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Arc;
use std::time::Duration;

use flowmux_daemon::{DaemonHandler, StateStore};
use flowmux_state::State;

fn flowmuxctl_path() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_flowmuxctl"))
}

struct Harness {
    _dir: tempfile::TempDir,
    shim: PathBuf,
    path_env: String,
    socket: PathBuf,
    cwd: PathBuf,
}

impl Harness {
    /// Run one tmux invocation through the shim, exactly as Claude
    /// Code would exec `tmux` from PATH inside a flowmux pane.
    fn tmux(&self, args: &[&str]) -> Output {
        Command::new(&self.shim)
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            // Keep instrumentation output while isolating the user's app env.
            .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|path| ("LLVM_PROFILE_FILE", path)))
            .env("PATH", &self.path_env)
            .env("FLOWMUX_SOCKET_PATH", &self.socket)
            .output()
            .expect("run tmux shim")
    }
}

/// Install the shim via the real `flowmuxctl fix` into an isolated
/// $XDG_DATA_HOME, start an in-process daemon on a temp socket, and
/// return everything needed to drive the shim.
async fn harness() -> (Harness, StateStore) {
    let dir = tempfile::TempDir::new().expect("tempdir");
    let home = dir.path().join("home");
    let data_home = home.join(".local/share");
    std::fs::create_dir_all(&data_home).unwrap();

    // `flowmuxctl fix` writes the tmux shim (among the other on-host
    // pieces) into $XDG_DATA_HOME/flowmux/shims.
    let fix = Command::new(flowmuxctl_path())
        .arg("fix")
        .env_clear()
        .envs(std::env::var_os("LLVM_PROFILE_FILE").map(|path| ("LLVM_PROFILE_FILE", path)))
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &home)
        .env("XDG_DATA_HOME", &data_home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .output()
        .expect("run flowmuxctl fix");
    assert!(fix.status.success(), "fix failed: {fix:?}");
    let shim = data_home.join("flowmux/shims/tmux");
    assert!(shim.exists(), "fix must install the tmux shim");

    // `tmux-compat` resolves `flowmuxctl` from PATH.
    let bin_dir = dir.path().join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::os::unix::fs::symlink(flowmuxctl_path(), bin_dir.join("flowmuxctl")).unwrap();

    // In-process daemon on a temp socket (same handler the headless
    // binary serves).
    let socket = dir.path().join("flowmux.sock");
    let store = StateStore::new_lazy(State::default());
    let handler = Arc::new(DaemonHandler::new(store.clone()));
    let server_socket = socket.clone();
    tokio::spawn(async move {
        let _ = flowmux_ipc::server::run(&server_socket, handler).await;
    });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(socket.exists(), "daemon socket never appeared");

    let cwd = dir.path().join("project");
    std::fs::create_dir_all(&cwd).unwrap();
    let shim_dir = shim.parent().unwrap().to_path_buf();
    let path_env = format!("{}:{}:/usr/bin:/bin", shim_dir.display(), bin_dir.display());
    (
        Harness {
            _dir: dir,
            shim,
            path_env,
            socket,
            cwd,
        },
        store,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_code_external_swarm_sequence_end_to_end() {
    let (h, store) = harness().await;
    let sock = "claude-swarm-31415";

    // Availability probe.
    let out = h.tmux(&["-V"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("tmux 3.4"));

    // has-session before any team exists.
    let out = h.tmux(&["-L", sock, "has-session", "-t", "claude-swarm"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");

    // createExternalSwarmSession.
    let out = h.tmux(&[
        "-L",
        sock,
        "new-session",
        "-d",
        "-s",
        "claude-swarm",
        "-n",
        "swarm-view",
        "-P",
        "-F",
        "#{pane_id}",
        "--",
        "cat",
    ]);
    assert!(out.status.success(), "{out:?}");
    let first_pane = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert!(!first_pane.is_empty());

    // The workspace exists, named after the swarm socket, rooted at the
    // lead's cwd.
    let ws = store
        .ordered_workspaces()
        .await
        .into_iter()
        .find(|w| w.name == sock)
        .expect("swarm workspace");
    assert_eq!(ws.root_dir, h.cwd.canonicalize().unwrap_or(h.cwd.clone()));

    // Window probe + first-teammate setup on the initial pane.
    let out = h.tmux(&[
        "-L",
        sock,
        "list-windows",
        "-t",
        "claude-swarm",
        "-F",
        "#{window_name}",
    ]);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "swarm-view");

    for argv in [
        vec![
            "-L", sock, "set-option", "-p", "-t", &first_pane, "remain-on-exit", "failed",
        ],
        vec![
            "-L",
            sock,
            "select-pane",
            "-t",
            &first_pane,
            "-T",
            "researcher",
        ],
        vec![
            "-L",
            sock,
            "respawn-pane",
            "-k",
            "-t",
            &first_pane,
            "--",
            "cd /tmp && env CLAUDECODE=1 CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1 claude --agent-id 'r-1' --agent-name 'researcher' --team-name 's-1'",
        ],
    ] {
        let out = h.tmux(&argv);
        assert!(out.status.success(), "argv {argv:?}: {out:?}");
    }

    // Second teammate: split, then rebalance.
    let out = h.tmux(&[
        "-L",
        sock,
        "split-window",
        "-d",
        "-t",
        &first_pane,
        "-v",
        "-P",
        "-F",
        "#{pane_id}",
        "--",
        "cat",
    ]);
    assert!(out.status.success(), "{out:?}");
    let second_pane = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_ne!(second_pane, first_pane);
    let out = h.tmux(&[
        "-L",
        sock,
        "select-layout",
        "-t",
        "claude-swarm:swarm-view",
        "tiled",
    ]);
    assert!(out.status.success());

    let out = h.tmux(&[
        "-L",
        sock,
        "list-panes",
        "-t",
        "claude-swarm:swarm-view",
        "-F",
        "#{pane_id}",
    ]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let panes: Vec<&str> = stdout.lines().collect();
    assert_eq!(panes.len(), 2, "{panes:?}");
    assert!(panes.contains(&first_pane.as_str()));
    assert!(panes.contains(&second_pane.as_str()));

    // The teammate title landed in the state (tab title, locked).
    let ws = store
        .ordered_workspaces()
        .await
        .into_iter()
        .find(|w| w.name == sock)
        .unwrap();
    fn tab_titles(pane: &flowmux_core::Pane, out: &mut Vec<String>) {
        match pane {
            flowmux_core::Pane::Leaf { content, .. } => {
                if let flowmux_core::PaneContent::Tabs { surfaces, .. } = content {
                    out.extend(surfaces.iter().map(|s| s.title.clone()));
                }
            }
            flowmux_core::Pane::Split { first, second, .. } => {
                tab_titles(first, out);
                tab_titles(second, out);
            }
        }
    }
    let mut titles = Vec::new();
    for surface in &ws.surfaces {
        tab_titles(&surface.root_pane, &mut titles);
    }
    assert!(
        titles.iter().any(|t| t == "researcher"),
        "tab titles: {titles:?}"
    );

    // Teardown: teammate pane, then the last pane removes the
    // workspace (tmux kill-pane semantics for the last pane).
    let out = h.tmux(&["-L", sock, "kill-pane", "-t", &second_pane]);
    assert!(out.status.success(), "{out:?}");
    let out = h.tmux(&["-L", sock, "kill-pane", "-t", &first_pane]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        !store
            .ordered_workspaces()
            .await
            .iter()
            .any(|w| w.name == sock),
        "workspace should be gone after killing the last pane"
    );
}
