// SPDX-License-Identifier: GPL-3.0-or-later
//! Exercise the shipped CLI against private user/config/data directories.
use serde_json::Value;
use std::{fs, path::Path, process::Command};

const PAYLOAD: &str = include_str!("../../../.agents/skills/flowmux-browser/SKILL.md");

fn run(home: &Path, args: &[&str], success: bool) -> Value {
    run_with_env(home, args, success, &[])
}

fn run_with_env(home: &Path, args: &[&str], success: bool, overrides: &[(&str, &Path)]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_flowmuxctl"))
        .env_clear()
        .env("HOME", home)
        .env("CODEX_HOME", home.join(".codex"))
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("FLOWMUX_RUNTIME_DIR", home.join("run"))
        .env("PATH", "/usr/bin:/bin")
        .envs(overrides.iter().copied())
        .current_dir(home)
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    assert_eq!(
        output.status.success(),
        success,
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    if output.stdout.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "{args:?} emitted invalid JSON: {error}: {}",
                String::from_utf8_lossy(&output.stdout)
            )
        })
    }
}

#[test]
fn install_update_uninstall_reinstall_all_targets() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let missing = run(home, &["agent", "doctor"], false);
    assert_eq!(missing.as_array().unwrap().len(), 5);
    assert!(missing
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["status"] == "missing"));

    let installed = run(home, &["agent", "install"], true);
    for row in installed.as_array().unwrap() {
        assert_eq!(
            fs::read_to_string(row["path"].as_str().unwrap()).unwrap(),
            PAYLOAD
        );
    }
    let unchanged = run(home, &["agent", "install"], true);
    assert!(unchanged
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["outcome"] == "already_up_to_date"));
    run(home, &["agent", "doctor"], true);

    for row in installed.as_array().unwrap() {
        fs::write(row["path"].as_str().unwrap(), "custom instructions").unwrap();
    }
    let drift = run(home, &["agent", "doctor"], false);
    assert!(drift
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["status"] == "drift"));
    run(home, &["agent", "install"], false);
    for row in installed.as_array().unwrap() {
        assert_eq!(
            fs::read_to_string(row["path"].as_str().unwrap()).unwrap(),
            "custom instructions"
        );
    }
    let updated = run(home, &["agent", "install", "--force"], true);
    for row in updated.as_array().unwrap() {
        assert_eq!(
            fs::read_to_string(row["backup"].as_str().unwrap()).unwrap(),
            "custom instructions"
        );
        fs::write(row["path"].as_str().unwrap(), "second customization").unwrap();
    }
    let removed = run(home, &["agent", "uninstall", "--skills-only"], true);
    for row in removed.as_array().unwrap() {
        assert!(!Path::new(row["path"].as_str().unwrap()).exists());
        assert_eq!(
            fs::read_to_string(row["backup"].as_str().unwrap()).unwrap(),
            "second customization"
        );
    }
    for row in updated.as_array().unwrap() {
        assert_eq!(
            fs::read_to_string(row["backup"].as_str().unwrap()).unwrap(),
            "custom instructions"
        );
    }
    let absent = run(home, &["agent", "uninstall", "--skills-only"], true);
    assert!(absent
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["outcome"] == "absent"));
    run(home, &["agent", "install"], true);
    run(home, &["agent", "doctor"], true);
}

#[test]
fn selective_uninstall_preserves_other_skills_hooks_and_shims() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    run(
        home,
        &[
            "agent",
            "install",
            "--agent",
            "claude-code",
            "--agent",
            "codex",
        ],
        true,
    );
    let settings = home.join(".claude/settings.json");
    fs::write(&settings, "{\"custom\":true}").unwrap();
    let shims = home.join("data/flowmux/shims");
    fs::create_dir_all(&shims).unwrap();
    for name in ["claude", "tmux"] {
        fs::write(
            shims.join(name),
            if name == "claude" {
                "flowmux agent wrapper shim"
            } else {
                "flowmux tmux compat shim"
            },
        )
        .unwrap();
    }
    let result = run(
        home,
        &[
            "agent",
            "uninstall",
            "--agent",
            "claude-code",
            "--skills-only",
        ],
        true,
    );
    assert_eq!(result.as_array().unwrap().len(), 1);
    for name in ["claude", "tmux"] {
        assert_eq!(
            fs::read_to_string(shims.join(name)).unwrap(),
            if name == "claude" {
                "flowmux agent wrapper shim"
            } else {
                "flowmux tmux compat shim"
            }
        );
    }
    assert_eq!(fs::read_to_string(settings).unwrap(), "{\"custom\":true}");
    run(home, &["agent", "doctor", "--agent", "codex"], true);
    // Default uninstall must also produce JSON and remove the managed wrappers.
    run(
        home,
        &["agent", "uninstall", "--agent", "claude-code"],
        true,
    );
    for name in ["claude", "tmux"] {
        assert!(!shims.join(name).exists());
    }
}

#[cfg(unix)]
#[test]
fn linked_skill_reports_an_actionable_error_without_touching_source() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let skill = home.join(".agents/skills/flowmux-browser/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    let source = home.join("dotfiles.md");
    fs::write(&source, "custom instructions").unwrap();
    std::os::unix::fs::symlink(&source, &skill).unwrap();
    let report = run(home, &["agent", "doctor", "--agent", "codex"], false);
    assert_eq!(report[0]["status"], "error");
    assert!(report[0]["detail"].as_str().unwrap().contains("symlink"));
    run(
        home,
        &["agent", "install", "--agent", "codex", "--force"],
        false,
    );
    assert_eq!(fs::read_to_string(&source).unwrap(), "custom instructions");
    run(
        home,
        &["agent", "uninstall", "--agent", "codex", "--skills-only"],
        true,
    );
    assert!(!skill.is_symlink());
    assert_eq!(fs::read_to_string(source).unwrap(), "custom instructions");
}

#[test]
fn repair_keeps_a_backup_and_leaves_unmanaged_codex_copy_alone() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let unmanaged = home.join(".codex/skills/flowmux-browser/SKILL.md");
    fs::create_dir_all(unmanaged.parent().unwrap()).unwrap();
    fs::write(&unmanaged, "user legacy skill").unwrap();
    let installed = run(home, &["agent", "install", "--agent", "codex"], true);
    let managed = Path::new(installed[0]["path"].as_str().unwrap());
    fs::write(managed, "old managed skill").unwrap();
    let output = run(home, &["fix"], true);
    let row = output["outcomes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["area"] == "codex skill")
        .unwrap();
    assert!(row["detail"].as_str().unwrap().contains("backup:"));
    assert_eq!(fs::read_to_string(managed).unwrap(), PAYLOAD);
    assert_eq!(fs::read_to_string(&unmanaged).unwrap(), "user legacy skill");
    let report = run(home, &["agent", "doctor", "--agent", "codex"], true);
    assert_eq!(
        report[0]["unmanaged_duplicates"][0],
        unmanaged.to_str().unwrap()
    );
}

#[test]
fn custom_config_roots_are_shared_by_install_doctor_fix_and_uninstall() {
    for (target, variable, relative, hooks_relative) in [
        (
            "claude-code",
            "CLAUDE_CONFIG_DIR",
            "skills/flowmux-browser/SKILL.md",
            "settings.json",
        ),
        (
            "opencode",
            "XDG_CONFIG_HOME",
            "opencode/skills/flowmux-browser/SKILL.md",
            "opencode/plugins/flowmux-session.js",
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        let config = home.join("custom config");
        let overrides = [(variable, config.as_path())];
        let installed = run_with_env(
            home,
            &["agent", "install", "--agent", target],
            true,
            &overrides,
        );
        let path = config.join(relative);
        assert_eq!(installed[0]["path"], path.to_str().unwrap());
        run_with_env(
            home,
            &["agent", "doctor", "--agent", target],
            true,
            &overrides,
        );
        fs::write(&path, "customized skill").unwrap();
        let repaired = run_with_env(home, &["fix"], true, &overrides);
        let rows = repaired["outcomes"].as_array().unwrap();
        let row = rows
            .iter()
            .find(|row| row["area"] == format!("{target} skill"))
            .unwrap();
        assert!(row["detail"].as_str().unwrap().contains("backup:"));
        assert_eq!(fs::read_to_string(&path).unwrap(), PAYLOAD);
        assert!(
            config.join(hooks_relative).is_file(),
            "hooks must use the same config root: {repaired}"
        );
        run_with_env(
            home,
            &["agent", "uninstall", "--skills-only", "--agent", target],
            true,
            &overrides,
        );
        assert!(!path.exists());
        assert!(!home
            .join(".claude/skills/flowmux-browser/SKILL.md")
            .exists());
        assert!(!home
            .join(".config/opencode/skills/flowmux-browser/SKILL.md")
            .exists());
    }
}

#[test]
fn flatpak_skills_use_the_host_opencode_root() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let sandbox_config = home.join(".var/app/com.flowmux.App/config");
    let overrides = [
        ("FLATPAK_ID", Path::new("com.flowmux.App")),
        ("XDG_CONFIG_HOME", sandbox_config.as_path()),
    ];
    let installed = run_with_env(
        home,
        &["agent", "install", "--agent", "opencode"],
        true,
        &overrides,
    );
    assert_eq!(
        installed[0]["path"],
        home.join(".config/opencode/skills/flowmux-browser/SKILL.md")
            .to_str()
            .unwrap()
    );
    assert!(!sandbox_config.join("opencode").exists());
    run_with_env(
        home,
        &["agent", "doctor", "--agent", "opencode"],
        true,
        &overrides,
    );
    run_with_env(
        home,
        &["agent", "uninstall", "--skills-only", "--agent", "opencode"],
        true,
        &overrides,
    );
}

#[test]
fn opencode_default_hook_and_skill_roots_match_on_every_os() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path();
    let overrides = [("XDG_CONFIG_HOME", Path::new(""))];
    let installed = run_with_env(
        home,
        &["agent", "install", "--agent", "opencode"],
        true,
        &overrides,
    );
    assert_eq!(
        installed[0]["path"],
        home.join(".config/opencode/skills/flowmux-browser/SKILL.md")
            .to_str()
            .unwrap()
    );
    run_with_env(home, &["fix"], true, &overrides);
    assert!(home
        .join(".config/opencode/plugins/flowmux-session.js")
        .exists());
    assert!(!home.join("Library/Application Support/opencode").exists());
}
