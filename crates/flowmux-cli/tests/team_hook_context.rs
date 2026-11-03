// SPDX-License-Identifier: GPL-3.0-or-later
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

fn hook(home: &Path, agent: &str, event: &str, worker: bool, input: Value) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_flowmuxctl"))
        .args(["hooks", agent, event])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .current_dir(home)
        .env("HOME", home)
        .env("CODEX_HOME", home.join("codex-profile"))
        .env("CLAUDE_CONFIG_DIR", home.join("claude-profile"))
        .env("GEMINI_CLI_HOME", home.join("gemini-profile"))
        .env("FLOWMUX_RUNTIME_DIR", home.join("runtime"))
        .env("FLOWMUX_SOCKET_PATH", home.join("missing.sock"))
        .env("FLOWMUX_PANE_ID", "4f2c5936-4a7f-44bb-a5d1-a578585ebcd4")
        .env("FLOWMUX_TEAM_WORKER", if worker { "1" } else { "" })
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn native_prompt_hooks_activate_only_installed_lead_skills() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    for (agent, root, event, native_event) in [
        (
            "claude",
            "claude-profile/skills",
            "prompt-submit",
            "UserPromptSubmit",
        ),
        ("codex", ".agents/skills", "turn-start", "UserPromptSubmit"),
        (
            "gemini",
            "gemini-profile/.gemini/skills",
            "running",
            "BeforeAgent",
        ),
        (
            "antigravity",
            ".gemini/config/skills",
            "turn-start",
            "PreInvocation",
        ),
    ] {
        let empty = if agent == "antigravity" { "{}\n" } else { "" };
        assert_eq!(hook(home, agent, event, false, json!({})), empty);
        let skill = home.join(root).join("flowmux-team");
        fs::create_dir_all(skill.join("scripts")).unwrap();
        fs::write(skill.join("SKILL.md"), "installed skill").unwrap();
        fs::write(skill.join("scripts/team.py"), "# helper").unwrap();
        if agent == "antigravity" {
            assert_eq!(hook(home, agent, "running", false, json!({})), empty);
        }
        for (event, native_event) in [(event, native_event), ("session-start", "SessionStart")] {
            let output: Value =
                serde_json::from_str(&hook(home, agent, event, false, json!({}))).unwrap();
            let context = if agent == "antigravity" {
                output["injectSteps"][0]["ephemeralMessage"]
                    .as_str()
                    .unwrap()
            } else {
                assert_eq!(output["hookSpecificOutput"]["hookEventName"], native_event);
                output["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap()
            };
            assert!(context.contains(&skill.join("scripts/team.py").display().to_string()));
            assert!(context.contains("mode inline"));
            assert!(context.contains("Small, sequential tasks"));
            assert_eq!(hook(home, agent, event, true, json!({})), empty);
            assert_eq!(
                hook(home, agent, event, false, json!({"agent_id":"child"})),
                empty
            );
            assert_eq!(
                hook(
                    home,
                    agent,
                    event,
                    false,
                    json!({"parent_agent_id":"parent"})
                ),
                empty
            );
        }
        fs::remove_file(skill.join("scripts/team.py")).unwrap();
        assert!(hook(home, agent, event, false, json!({})).contains("incomplete"));
        fs::remove_file(skill.join("SKILL.md")).unwrap();
        assert_eq!(hook(home, agent, event, false, json!({})), empty);
    }
    assert_eq!(
        hook(home, "antigravity", "stop", false, json!({})),
        "{\"decision\":\"\"}\n"
    );
    assert_eq!(
        hook(home, "antigravity", "running", false, json!({})),
        "{}\n"
    );
}
