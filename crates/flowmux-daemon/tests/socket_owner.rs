// SPDX-License-Identifier: GPL-3.0-or-later
use flowmux_ipc::{client::Client, Request, Response};
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

fn start(socket: &Path, state: &Path) -> Daemon {
    Daemon(
        Command::new(env!("CARGO_BIN_EXE_flowmux-daemon"))
            .arg("--socket")
            .arg(socket)
            .env("XDG_STATE_HOME", state)
            .env("XDG_CONFIG_HOME", state)
            .env("XDG_DATA_HOME", state)
            .env("FLOWMUX_RUNTIME_DIR", state)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

async fn connect(socket: &Path) -> Client {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(client) = Client::connect(socket).await {
                return client;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn live_daemon_keeps_its_address_and_crashed_daemon_can_restart() {
    let root = tempfile::Builder::new()
        .prefix("fm-a04-")
        .tempdir_in("/tmp")
        .unwrap();
    let socket = root.path().join("daemon.sock");
    let control = flowmux_ipc::control_socket_path(&socket);
    let first = start(&socket, &root.path().join("first"));
    let existing = connect(&socket).await;
    assert!(matches!(
        existing.call(Request::Ping).await.unwrap(),
        Response::Pong
    ));
    let mut second = start(&socket, &root.path().join("second"));
    let status = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = second.0.try_wait().unwrap() {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("duplicate daemon did not reject the occupied address");
    assert!(!status.success());
    for endpoint in [&socket, &control] {
        let client = connect(endpoint).await;
        assert!(matches!(
            client.call(Request::Ping).await.unwrap(),
            Response::Pong
        ));
    }
    assert!(matches!(
        existing.call(Request::Ping).await.unwrap(),
        Response::Pong
    ));
    drop(existing);
    drop(first);
    // SIGKILL leaves socket files behind but releases the lifetime lock.
    let restarted = start(&socket, &root.path().join("restarted"));
    let client = connect(&socket).await;
    assert!(matches!(
        client.call(Request::Ping).await.unwrap(),
        Response::Pong
    ));
    drop(client);
    drop(restarted);
    println!("A04_DAEMON_SOCKET_OWNERSHIP_OK");
}
