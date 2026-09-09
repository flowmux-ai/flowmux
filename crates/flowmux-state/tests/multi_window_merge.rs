// SPDX-License-Identifier: GPL-3.0-or-later
//! Cross-thread read/merge/write coverage for per-window workspace ownership.

use flowmux_core::{Workspace, WorkspaceId};
use flowmux_state::{claim_window_from, load_from, save_window_to, State, WindowOwner};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use uuid::Uuid;

fn snapshot(name: &str) -> (State, WorkspaceId) {
    let workspace = Workspace {
        id: WorkspaceId::new(),
        name: name.into(),
        custom_title: None,
        location: flowmux_core::WorkspaceLocation::Local {
            root_dir: PathBuf::from(format!("/tmp/{name}")),
        },
        git: None,
        listening_ports: vec![],
        surfaces: vec![],
        color: None,
    };
    let id = workspace.id;
    (
        State {
            workspaces: vec![workspace],
            workspace_order: vec![id],
            active_workspace: Some(id),
            ..State::default()
        },
        id,
    )
}

#[test]
fn interleaved_window_saves_preserve_both_workspace_sets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let (state_a, workspace_a) = snapshot("window-a");
    let (state_b, workspace_b) = snapshot("window-b");
    let owner_a = WindowOwner {
        instance_id: Uuid::new_v4(),
        pid: std::process::id(),
    };
    let owner_b = WindowOwner {
        instance_id: Uuid::new_v4(),
        pid: std::process::id(),
    };
    let barrier = Arc::new(Barrier::new(2));

    let handles = [(owner_a, state_a), (owner_b, state_b)]
        .into_iter()
        .map(|(owner, state)| {
            let path = path.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..50 {
                    save_window_to(&path, owner, &state).unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }

    let merged = load_from(&path).unwrap();
    let ids = merged
        .workspaces
        .iter()
        .map(|workspace| workspace.id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids, [workspace_a, workspace_b].into_iter().collect());
    assert_eq!(merged.windows.len(), 2);
    assert_eq!(merged.workspace_owners[&workspace_a], owner_a.instance_id);
    assert_eq!(merged.workspace_owners[&workspace_b], owner_b.instance_id);
}

#[test]
fn next_window_reclaims_workspaces_from_dead_owners() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let (state_a, workspace_a) = snapshot("dead-a");
    let (state_b, workspace_b) = snapshot("dead-b");
    let dead_a = WindowOwner {
        instance_id: Uuid::new_v4(),
        pid: 999_999,
    };
    let dead_b = WindowOwner {
        instance_id: Uuid::new_v4(),
        pid: 999_998,
    };
    save_window_to(&path, dead_a, &state_a).unwrap();
    save_window_to(&path, dead_b, &state_b).unwrap();

    let current = WindowOwner::current();
    let claimed = claim_window_from(&path, current).unwrap();
    let ids = claimed
        .workspaces
        .iter()
        .map(|workspace| workspace.id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids, [workspace_a, workspace_b].into_iter().collect());

    let disk = load_from(&path).unwrap();
    assert_eq!(disk.windows.len(), 1);
    assert!(disk
        .workspace_owners
        .values()
        .all(|owner| *owner == current.instance_id));
}
