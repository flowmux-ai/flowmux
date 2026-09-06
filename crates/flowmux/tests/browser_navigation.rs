// SPDX-License-Identifier: GPL-3.0-or-later
//! Run the actual GUI, IPC server and WebKit together. Use Xvfb + D-Bus as in
//! CI. The child's home, config, state and sockets are isolated from the user.

#![cfg(target_os = "linux")]

use flowmux_browser::DomSnapshot;
use flowmux_core::{PaneId, SplitDirection};
use flowmux_ipc::{
    client::Client,
    protocol::{Request, Response},
};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

struct App(Child);

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn call(client: &Client, request: Request) -> Response {
    let description = format!("{request:?}");
    tokio::time::timeout(Duration::from_secs(45), client.call(request))
        .await
        .unwrap_or_else(|_| panic!("GUI IPC response timed out: {description}"))
        .expect("GUI IPC failed")
}

async fn wait_page(client: &Client, pane: PaneId, title: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let result = call(
            client,
            Request::BrowserEval {
                pane,
                source: "document.title + ':' + document.readyState".into(),
            },
        )
        .await;
        if matches!(result, Response::BrowserResult { ref value } if value == &format!("{title}:complete"))
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "page {title} did not load: {result:?}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}

async fn snapshot(client: &Client, pane: PaneId) -> DomSnapshot {
    match call(client, Request::BrowserSnapshot { pane }).await {
        Response::BrowserResult { value } => serde_json::from_str(&value).unwrap(),
        other => panic!("snapshot failed: {other:?}"),
    }
}

#[tokio::test]
async fn native_link_navigation_rejects_old_refs_and_fresh_snapshot_restores_actions() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = dir.path().join("runtime");
    std::fs::create_dir(&runtime).unwrap();
    let first = dir.path().join("first.html");
    std::fs::write(
        &first,
        "<!doctype html><title>first</title><a id='next' href='second.html'>Next</a>",
    )
    .unwrap();
    std::fs::write(dir.path().join("second.html"), "<!doctype html><title>second</title><button id='next' onclick=\"this.textContent='Clicked'\">Second</button>").unwrap();
    let log = std::fs::File::create(dir.path().join("gui.log")).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_flowmux"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("FLOWMUX_") {
            command.env_remove(key);
        }
    }
    command
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("XDG_CACHE_HOME", dir.path().join("cache"))
        .env("FLOWMUX_RUNTIME_DIR", &runtime)
        .env("SHELL", "/bin/sh")
        .env_remove("FLATPAK_ID")
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log);
    let mut app = App(command.spawn().unwrap());
    let socket = runtime.join(format!("flowmux-{}.sock", app.0.id()));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let client = loop {
        if let Ok(client) = Client::connect(&socket).await {
            break client;
        }
        assert!(
            app.0.try_wait().unwrap().is_none() && tokio::time::Instant::now() < deadline,
            "GUI did not start:\n{}",
            std::fs::read_to_string(dir.path().join("gui.log")).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    let workspace = match call(
        &client,
        Request::WorkspaceCreate {
            name: Some("browser regression".into()),
            root: dir.path().into(),
        },
    )
    .await
    {
        Response::WorkspaceCreated { id } => id,
        other => panic!("workspace create: {other:?}"),
    };
    let terminal = match call(&client, Request::WorkspaceTree).await {
        Response::Tree { workspaces } => {
            workspaces.iter().find(|w| w.id == workspace).unwrap().panes[0].id
        }
        other => panic!("tree: {other:?}"),
    };
    let pane = match call(
        &client,
        Request::BrowserOpen {
            url: format!("file://{}", first.display()),
            target_pane: Some(terminal),
            direction: SplitDirection::Vertical,
        },
    )
    .await
    {
        Response::BrowserPaneOpened { pane, .. } => pane,
        other => panic!("browser open: {other:?}"),
    };
    wait_page(&client, pane, "first").await;
    let first_snapshot = snapshot(&client, pane).await;
    let old_ref = first_snapshot
        .refs
        .iter()
        .find(|(_, meta)| meta.name == "Next")
        .unwrap()
        .0
        .clone();
    assert!(matches!(
        call(
            &client,
            Request::BrowserClick {
                pane,
                target: old_ref.clone()
            }
        )
        .await,
        Response::BrowserOk
    ));
    wait_page(&client, pane, "second").await;
    // Both pages deliberately contain #next. Retaining the old selector would
    // silently click a different element instead of reporting an expired ref.
    assert!(
        matches!(
            call(
                &client,
                Request::BrowserClick {
                    pane,
                    target: old_ref
                }
            )
            .await,
            Response::Error(_)
        ),
        "native navigation must invalidate refs even without a BrowserNavigate request"
    );
    let fresh = snapshot(&client, pane).await;
    let new_ref = fresh
        .refs
        .iter()
        .find(|(_, meta)| meta.name == "Second")
        .unwrap()
        .0
        .clone();
    assert!(matches!(
        call(
            &client,
            Request::BrowserClick {
                pane,
                target: new_ref.clone()
            }
        )
        .await,
        Response::BrowserOk
    ));
    match call(
        &client,
        Request::BrowserText {
            pane,
            target: new_ref,
        },
    )
    .await
    {
        Response::BrowserResult { value } => assert_eq!(value, "Clicked"),
        other => panic!("fresh ref action: {other:?}"),
    }
}
