// SPDX-License-Identifier: GPL-3.0-or-later

use super::{cline_arguments, SessionAgent};
use std::collections::BTreeMap;

pub(super) fn read(
    pid: u32,
    agent: SessionAgent,
) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let keys = agent
        .home_variables()
        .iter()
        .copied()
        .chain(["PATH"])
        .collect::<Vec<_>>();
    let mut environment =
        flowmux_procmon::process_environment(pid, &keys).map_err(|error| error.to_string())?;
    let arguments = if agent == SessionAgent::Cline {
        let argv = flowmux_procmon::cmdline_of(pid);
        cline_arguments(
            &mut environment,
            &argv.iter().map(String::as_str).collect::<Vec<_>>(),
            || {
                flowmux_procmon::process_cwd(pid)
                    .ok_or_else(|| "Agent working directory is unavailable".into())
            },
        )?
    } else {
        Vec::new()
    };
    Ok((environment, arguments))
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    fn child_home(agent: SessionAgent, command: &mut Command) -> SessionHome {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        let result = session_home(&SessionTarget {
            pane: PaneId::new(),
            surface: SurfaceId::new(),
            agent,
            root_pid: child.id(),
            session_id: None,
        });
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        result.unwrap()
    }

    fn fixture(root: &std::path::Path, name: &str) -> PathBuf {
        let file = root.join(format!("{name}.js"));
        std::fs::write(&file, "console.log('ready');process.stdin.resume();process.stdin.on('end',()=>process.exit(0));").unwrap();
        file
    }

    #[test]
    fn session_home_reads_each_agents_own_environment() {
        let root = tempfile::tempdir().unwrap();
        let custom = root.path().join("custom ' 한글");
        let path = std::env::var("PATH").unwrap();
        for (agent, name, key) in [
            (SessionAgent::Claude, "claude", "CLAUDE_CONFIG_DIR"),
            (SessionAgent::Codex, "codex", "CODEX_HOME"),
            (SessionAgent::OpenCode, "opencode", "XDG_DATA_HOME"),
            (SessionAgent::Antigravity, "agy", "HOME"),
            (SessionAgent::Cline, "cline", "CLINE_DATA_DIR"),
            (SessionAgent::Gemini, "gemini", "GEMINI_CLI_HOME"),
        ] {
            let mut command = Command::new("node");
            command
                .arg(fixture(root.path(), name))
                .env_clear()
                .env("PATH", &path)
                .env("HOME", root.path())
                .env(key, &custom);
            let home = child_home(agent, &mut command);
            let expected = agent
                .history_home(|var| {
                    if var == key {
                        Some(custom.clone())
                    } else if var == "HOME" {
                        Some(root.path().into())
                    } else {
                        None
                    }
                })
                .unwrap();
            assert_eq!(home.path, expected, "{name}");
            assert_eq!(home.environment[key], custom.to_str().unwrap());
            assert_eq!(home.environment["PATH"], path);
        }
    }

    #[test]
    fn cline_session_home_retains_relative_cli_overrides() {
        let root = tempfile::tempdir().unwrap();
        let mut command = Command::new("node");
        command
            .arg(fixture(root.path(), "cline"))
            .args(["--config=config ' 한글", "--data-dir", "data path"])
            .env("CLINE_DATA_DIR", "/ignored/env")
            .current_dir(root.path());
        let home = child_home(SessionAgent::Cline, &mut command);
        let cwd = root.path().canonicalize().unwrap();
        assert_eq!(home.path, cwd.join("data path/db"));
        assert_eq!(
            home.arguments,
            [
                "--config".to_string(),
                cwd.join("config ' 한글").to_str().unwrap().into(),
                "--data-dir".into(),
                cwd.join("data path").to_str().unwrap().into()
            ]
        );
    }
}
