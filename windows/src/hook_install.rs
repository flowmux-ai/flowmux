// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows native-hook configuration; unrelated provider settings stay intact.
use anyhow::Context;
use base64::Engine;
use serde_json::{json, Value};

const MARKER: &str = "true # flowmux-windows-session-hook-v1";
const EVENTS: [(&str, &str); 2] = [
    ("SessionStart", "session-start"),
    ("SessionEnd", "session-end"),
];
const CODEX_EVENTS: [(&str, &str); 10] = [
    ("SessionStart", "session-start"),
    ("SessionEnd", "session-end"),
    ("UserPromptSubmit", "turn-start"),
    ("PreToolUse", "running"),
    ("PostToolUse", "running"),
    ("PermissionRequest", "notification"),
    ("SubagentStart", "subagent-start"),
    ("SubagentStop", "subagent-stop"),
    ("Stop", "stop"),
    ("Interrupt", "interrupt"),
];
fn events(agent: &str) -> &[(&str, &str)] {
    if agent == "codex" {
        &CODEX_EVENTS
    } else {
        &EVENTS
    }
}

fn handler(agent: &str, event: &str, executable: &str) -> Value {
    if agent == "claude" {
        json!({"type":"command","command":executable,
            "args":["hooks",agent,event,"--flowmux-hook"],"timeout":10})
    } else {
        // No executable path crosses a shell parser before decoding. In the
        // PowerShell script a literal single-quoted path preserves Korean,
        // spaces, apostrophes, $, backticks, %, and & without expansion.
        let script = format!(
            "& '{}' hooks codex {event} --flowmux-hook",
            executable.replace('\'', "''")
        );
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            script
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        json!({"type":"command","command":MARKER,
            "commandWindows":format!("powershell.exe -NoProfile -NonInteractive -EncodedCommand {encoded}"),"timeout":10})
    }
}

fn owned(handler: &Value, agent: &str) -> bool {
    handler["type"] == "command"
        && if agent == "codex" {
            handler["command"] == MARKER
        } else {
            EVENTS.iter().any(|(_, event)| {
                handler["args"] == json!(["hooks", agent, event, "--flowmux-hook"])
            })
        }
}

fn changed(mut root: Value, agent: &str, executable: Option<&str>) -> anyhow::Result<Value> {
    anyhow::ensure!(
        matches!(agent, "claude" | "codex"),
        "unsupported hook setup provider"
    );
    let object = root
        .as_object_mut()
        .context("hook configuration must be a JSON object")?;
    if executable.is_some() {
        anyhow::ensure!(
            object.get("disableAllHooks").is_none_or(|v| v == false),
            "provider hooks are disabled or disableAllHooks is invalid"
        );
    }
    if !object.contains_key("hooks") && executable.is_none() {
        return Ok(root);
    }
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("hooks must be a JSON object")?;
    for &(event, operation) in events(agent) {
        if !hooks.contains_key(event) && executable.is_none() {
            continue;
        }
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .context("session hook event must contain an array of matcher groups")?;
        for group in groups.iter() {
            let group = group
                .as_object()
                .context("hook matcher group must be an object")?;
            anyhow::ensure!(
                group.get("matcher").is_none_or(Value::is_string),
                "hook matcher must be a string"
            );
            let handlers = group
                .get("hooks")
                .and_then(Value::as_array)
                .context("matcher hooks must be an array")?;
            anyhow::ensure!(
                handlers.iter().all(Value::is_object),
                "hook handlers must be objects"
            );
        }
        groups.retain_mut(|group| {
            let handlers = group["hooks"].as_array_mut().unwrap();
            let before = handlers.len();
            handlers.retain(|h| !owned(h, agent));
            before == handlers.len() || !handlers.is_empty()
        });
        if let Some(executable) = executable {
            groups.push(json!({"matcher":if event == "SessionStart" {"^(startup|resume|clear|fork)$"} else {""},
                "hooks":[handler(agent, operation, executable)]}));
        } else if groups.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        object.remove("hooks");
    }
    Ok(root)
}

#[cfg(windows)]
pub fn run(args: &crate::command::HookConfigArgs, uninstall: bool) -> anyhow::Result<String> {
    use crate::native::{checked, wide};
    use std::{
        fs::{File, OpenOptions},
        io::{Read, Write},
        os::windows::fs::OpenOptionsExt,
        path::PathBuf,
    };
    use windows_sys::Win32::Storage::FileSystem::*;
    const LIMIT: u64 = 1024 * 1024;
    fn read(path: &std::path::Path) -> anyhow::Result<Option<Vec<u8>>> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        anyhow::ensure!(
            file.metadata()?.is_file(),
            "hook configuration is not a regular file"
        );
        let mut bytes = Vec::new();
        file.take(LIMIT + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 <= LIMIT,
            "hook configuration exceeds 1 MiB"
        );
        Ok(Some(bytes))
    }
    let requested = if let Some(path) = &args.config {
        path.clone()
    } else {
        let (variable, directory, file) = match args.agent.as_str() {
            "claude" => ("CLAUDE_CONFIG_DIR", ".claude", "settings.json"),
            "codex" => ("CODEX_HOME", ".codex", "hooks.json"),
            _ => anyhow::bail!("unsupported hook setup provider"),
        };
        let home = match std::env::var_os(variable) {
            Some(value) => {
                anyhow::ensure!(!value.is_empty(), "agent home override is empty");
                PathBuf::from(value)
            }
            None => PathBuf::from(
                std::env::var_os("USERPROFILE").context("USERPROFILE is unavailable")?,
            )
            .join(directory),
        };
        home.join(file)
    };
    let requested = std::path::absolute(requested)?;
    let report = |path: &std::path::Path, changed: bool| {
        format!(
            "{}\n",
            json!({"agent":args.agent,
        "path":path,"changed":changed,"operation":if uninstall {"uninstall"} else {"setup"},
        "events":events(&args.agent).iter().map(|(event,_)| *event).collect::<Vec<_>>()})
        )
    };
    if uninstall && !requested.try_exists()? {
        return Ok(report(&requested, false));
    }
    // Resolve existing links so replacing a linked config preserves its link.
    let path = if requested.try_exists()? {
        requested.canonicalize()?
    } else {
        requested
            .parent()
            .context("configuration has no parent")?
            .canonicalize()
            .context("agent configuration directory must already exist")?
            .join(
                requested
                    .file_name()
                    .context("configuration has no filename")?,
            )
    };
    let _lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(0)
        .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
        .open(path.with_extension("flowmux-lock"))
        .context("could not acquire hook update lock")?;
    let original = read(&path)?;
    let root: Value = match &original {
        Some(bytes) => serde_json::from_slice(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes))
            .context("invalid provider JSON; configuration was not changed")?,
        None => json!({}),
    };
    let executable = if uninstall {
        None
    } else {
        let executable = args
            .flowmux_bin
            .clone()
            .unwrap_or(std::env::current_exe()?.with_file_name("flowmuxctl.exe"));
        let executable = std::path::absolute(executable)?;
        anyhow::ensure!(
            executable.is_file()
                && executable
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("exe")),
            "flowmux hook executable must be an existing .exe file"
        );
        Some(
            executable
                .to_str()
                .context("hook executable path is not Unicode")?
                .to_owned(),
        )
    };
    let next = changed(root.clone(), &args.agent, executable.as_deref())?;
    if next == root {
        return Ok(report(&path, false));
    }
    let bytes = serde_json::to_vec_pretty(&next)?;
    anyhow::ensure!(
        bytes.len() as u64 <= LIMIT,
        "updated hook configuration exceeds 1 MiB"
    );
    let temporary = path.with_extension(format!("flowmux-{}.tmp", uuid::Uuid::new_v4()));
    let backup = temporary.with_extension("bak");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| -> anyhow::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        anyhow::ensure!(
            read(&path)? == original,
            "provider configuration changed during setup; try again"
        );
        unsafe {
            if original.is_some() {
                // Preserve the original ACL and keep recovery data if Windows
                // cannot finish replacing the file. Never ignore ACL errors.
                checked(ReplaceFileW(
                    wide(&path).as_ptr(),
                    wide(&temporary).as_ptr(),
                    wide(&backup).as_ptr(),
                    0,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                ))?;
            } else {
                checked(MoveFileExW(
                    wide(&temporary).as_ptr(),
                    wide(&path).as_ptr(),
                    MOVEFILE_WRITE_THROUGH,
                ))?;
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        if backup.try_exists().unwrap_or(true)
            || (original.is_some() && !path.try_exists().unwrap_or(false))
        {
            anyhow::bail!(
                "{error:#}; recovery files retained: {} and {}",
                backup.display(),
                temporary.display()
            );
        }
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if backup.try_exists()? {
        std::fs::remove_file(&backup).context("hooks saved but temporary backup cleanup failed")?;
    }
    Ok(report(&path, true))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_hooks_merge_idempotently_and_remove_only_owned_handlers() {
        for agent in ["claude", "codex"] {
            let original = json!({"env":{"KOREAN":"한 한 é 😀"},"hooks":{
                "SessionStart":[{"matcher":"startup","extra":"keep","hooks":[{"type":"command","command":"user hook"}]}],
                "Stop":[{"hooks":[{"type":"command","command":"flowmux Linux hook"}]}]}});
            let setup = changed(
                original.clone(),
                agent,
                Some("C:\\한 글\\a'b$`%&\\flowmuxctl.exe"),
            )
            .unwrap();
            assert_eq!(
                changed(
                    setup.clone(),
                    agent,
                    Some("C:\\한 글\\a'b$`%&\\flowmuxctl.exe")
                )
                .unwrap(),
                setup
            );
            assert_eq!(changed(setup.clone(), agent, None).unwrap(), original);
            let mut shared = setup;
            shared["hooks"]["SessionStart"][1]["hooks"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":"command","command":"keep shared"}));
            let removed = changed(shared, agent, None).unwrap();
            assert_eq!(
                removed["hooks"]["SessionStart"][1]["hooks"],
                json!([{"type":"command","command":"keep shared"}])
            );
            for bad in [
                json!([]),
                json!({"hooks":null}),
                json!({"hooks":{"SessionEnd":{}}}),
                json!({"hooks":{"SessionStart":[{"hooks":null}]}}),
                json!({"disableAllHooks":true}),
            ] {
                assert!(changed(bad, agent, Some("C:\\flowmuxctl.exe")).is_err());
            }
        }
    }
}
