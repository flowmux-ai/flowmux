// SPDX-License-Identifier: GPL-3.0-or-later
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::process::Command;
use std::time::Duration;

#[test]
fn local_paths_use_caller_directory_and_remote_paths_stay_remote() {
    let dir = tempfile::Builder::new()
        .prefix("fm-path-")
        .tempdir_in("/tmp")
        .unwrap();
    let caller = dir.path().join("caller");
    std::fs::create_dir(&caller).unwrap();
    let caller = caller.canonicalize().unwrap();
    let id = "11111111-1111-1111-1111-111111111111";
    let local = json!({"type":"local", "root_dir":"/server"});
    let ssh = json!({"type":"ssh", "config":{"target":{"host":"example.test"}, "cwd":"/remote", "tmux":false, "forwards":[]}});
    let cases = vec![
        (
            vec!["workspace", "new", "--root", "."],
            "root",
            json!(caller),
            local.clone(),
        ),
        (
            vec!["workspace", "new"],
            "root",
            json!(caller),
            local.clone(),
        ),
        (
            vec!["claude-teams", "--root", "missing"],
            "root",
            json!(caller.join("missing")),
            local.clone(),
        ),
        (
            vec![
                "team-spawn",
                id,
                "--cwd",
                ".",
                "--shell",
                "/bin/sh",
                "--role",
                "worker",
            ],
            "cwd",
            json!(caller),
            local.clone(),
        ),
        (
            vec!["new-tab", "--workspace", id, "--cwd", "."],
            "cwd",
            json!(caller),
            local.clone(),
        ),
        (
            vec!["new-tab", "--workspace", id, "--cwd", "../missing"],
            "cwd",
            json!(caller.join("../missing")),
            json!({"type":"team", "root_dir":"/server"}),
        ),
        (
            vec!["new-tab", "--workspace", id, "--cwd", "."],
            "cwd",
            json!("."),
            ssh.clone(),
        ),
        (
            vec!["new-tab", "--workspace", id, "--cwd", "/remote"],
            "cwd",
            json!("/remote"),
            ssh,
        ),
        (
            vec!["new-tab", "--workspace", id],
            "cwd",
            Value::Null,
            local.clone(),
        ),
        (
            vec![
                "ssh",
                "connect",
                "example.test",
                "--cwd",
                "/remote",
                "--identity-file",
                "key",
                "--config-file",
                "config",
            ],
            "request",
            json!({}),
            local,
        ),
    ];
    for (args, field, expected, location) in cases {
        let socket = dir.path().join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                let mut response = json!({"id":request["id"], "kind":"response"});
                if request["verb"] == "workspace_tree" {
                    response["tree"] = json!({"workspaces":[{"id":id,"name":"test","root":null,"location":location,"panes":[]}]});
                } else {
                    response["ok"] = Value::Null;
                }
                writeln!(stream, "{response}").unwrap();
                if request["verb"] != "workspace_tree" {
                    return request;
                }
            }
        });
        let output = Command::new(env!("CARGO_BIN_EXE_flowmuxctl"))
            .args(["--socket", socket.to_str().unwrap()])
            .args(&args)
            .current_dir(&caller)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        let request = server.join().unwrap();
        std::fs::remove_file(socket).unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        if field == "request" {
            let config = &request["request"]["config"];
            assert_eq!(config["cwd"], "/remote");
            assert_eq!(config["target"]["identity_file"], json!(caller.join("key")));
            assert_eq!(
                config["target"]["config_file"],
                json!(caller.join("config"))
            );
        } else {
            assert_eq!(request[field], expected, "{args:?}: {request}");
        }
    }
}
