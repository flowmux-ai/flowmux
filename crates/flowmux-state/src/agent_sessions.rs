// SPDX-License-Identifier: GPL-3.0-or-later
//! Persistent surface ↔ agent-session mapping.
//!
//! Mirrors cmux's `~/.cmuxterm/<agent>-hook-sessions.json` — each
//! supported agent (claude, codex, opencode, …) reports its session id
//! to flowmux through an IPC verb and we persist
//! `(agent, surface_id) → session_id`. On the next launch the GUI can
//! re-spawn the same agent in the same surface with `<agent> --resume
//! <session-id>` and continue the conversation where it left off.
//!
//! Storage layout:
//!
//! ```text
//! $XDG_DATA_HOME/flowmux/agent-sessions/<agent>.json
//! ```
//!
//! A single file per agent makes it easy to inspect, back up, or
//! delete one agent's history without touching the others. The JSON
//! payload is `{"<surface_uuid>": "<session_id>", ...}` — flat, so
//! human-readable and trivially mergeable.
//!
//! Writes are atomic (tmp file + rename), matching the policy in
//! `state_store.rs`, so a crash mid-write never leaves a partially
//! serialized file behind.

use flowmux_core::SurfaceId;
use fs2::FileExt;
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};

const RESUMABLE_AGENTS: [&str; 4] = ["claude", "codex", "opencode", "antigravity"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAgentSession {
    pub agent: String,
    pub session_id: String,
}

impl SavedAgentSession {
    pub fn resume_argv(&self) -> Vec<String> {
        match self.agent.as_str() {
            "claude" => vec!["claude".into(), "--resume".into(), self.session_id.clone()],
            "codex" => vec!["codex".into(), "resume".into(), self.session_id.clone()],
            "opencode" => vec![
                "opencode".into(),
                "--session".into(),
                self.session_id.clone(),
            ],
            "antigravity" => vec![
                "agy".into(),
                "--conversation".into(),
                self.session_id.clone(),
            ],
            _ => Vec::new(),
        }
    }

    /// Command used as a shell startup argument while restoring a session. The
    /// agent and flags are fixed by [`resume_argv`]; the opaque session id is
    /// single-quote escaped. When the agent exits or fails to start, erase both
    /// the visible screen and scrollback so the replacement shell starts clean.
    pub fn shell_command(&self) -> Option<String> {
        let argv = self.resume_argv();
        let executable = argv.first()?;
        let command = argv
            .iter()
            .map(|arg| shell_quote(arg))
            .collect::<Vec<_>>()
            .join(" ");
        let cleanup = match self.agent.as_str() {
            "claude" => "if command -v flowmuxctl >/dev/null 2>&1; then printf '%s' '{\"reason\":\"prompt_input_exit\"}' | flowmuxctl hooks claude session-end >/dev/null 2>&1; fi; ".to_string(),
            "codex" | "opencode" => format!(
                "if command -v flowmuxctl >/dev/null 2>&1; then flowmuxctl hooks {agent} stop '{{\"reason\":\"flowmux_resume_returned\"}}' >/dev/null 2>&1; fi; ",
                agent = self.agent,
            ),
            "antigravity" => "if command -v flowmuxctl >/dev/null 2>&1; then printf '%s' '{\"reason\":\"flowmux_resume_returned\"}' | flowmuxctl hooks antigravity stop >/dev/null 2>&1; fi; ".to_string(),
            _ => String::new(),
        };
        Some(format!(
            "if command -v {exe} >/dev/null 2>&1; then {command}; fi; printf '\\033[3J\\033[2J\\033[H'; {cleanup}printf '\\033c'",
            exe = shell_quote(executable),
        ))
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// File-backed store. Constructed once on daemon boot from
/// `$XDG_DATA_HOME/flowmux/agent-sessions/`.
#[derive(Debug, Clone)]
pub struct AgentSessionStore {
    dir: PathBuf,
}

impl AgentSessionStore {
    /// `dir` is the directory that holds `<agent>.json` files. The
    /// store creates it on the first write — callers don't need to
    /// pre-create it.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn agent_path(&self, agent: &str) -> PathBuf {
        self.dir.join(format!("{agent}.json"))
    }

    fn with_exclusive_lock<T>(&self, f: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        std::fs::create_dir_all(&self.dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.dir.join("sessions.lock"))?;
        lock.lock_exclusive()?;
        let result = f();
        let unlock_result = FileExt::unlock(&lock);
        match (result, unlock_result) {
            (Err(error), _) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn normalize_agent(agent: &str) -> Option<&'static str> {
        RESUMABLE_AGENTS
            .into_iter()
            .find(|candidate| candidate.eq_ignore_ascii_case(agent.trim()))
    }

    fn load(&self, agent: &str) -> io::Result<BTreeMap<String, String>> {
        let path = self.agent_path(agent);
        if !path.exists() {
            return Ok(BTreeMap::new());
        }
        let bytes = std::fs::read(&path)?;
        if bytes.is_empty() {
            return Ok(BTreeMap::new());
        }
        serde_json::from_slice(&bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    fn write_atomic(path: &Path, payload: &[u8]) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(payload)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, path)
    }

    /// Record (or overwrite) the session id for `(agent, surface)`.
    pub fn record(&self, agent: &str, surface: SurfaceId, session_id: &str) -> io::Result<()> {
        let agent = Self::normalize_agent(agent).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "unsupported resumable agent")
        })?;
        if session_id.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty agent session id",
            ));
        }
        self.with_exclusive_lock(|| {
            // A tab can run different agents over its lifetime. Keep exactly
            // one binding so restore never starts two agents in the same shell.
            for other in RESUMABLE_AGENTS {
                if other != agent {
                    self.forget_unlocked(other, surface)?;
                }
            }
            let mut map = self.load(agent)?;
            map.insert(surface.to_string(), session_id.to_string());
            let bytes = serde_json::to_vec_pretty(&map)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Self::write_atomic(&self.agent_path(agent), &bytes)
        })
    }

    /// Return the session id previously recorded for `(agent,
    /// surface)`, or `None` if the agent has never reported one for
    /// that surface (or the file is missing).
    pub fn lookup(&self, agent: &str, surface: SurfaceId) -> Option<String> {
        let agent = Self::normalize_agent(agent)?;
        self.load(agent).ok()?.get(&surface.to_string()).cloned()
    }

    pub fn lookup_surface(&self, surface: SurfaceId) -> Option<SavedAgentSession> {
        RESUMABLE_AGENTS.into_iter().find_map(|agent| {
            self.lookup(agent, surface)
                .map(|session_id| SavedAgentSession {
                    agent: agent.to_string(),
                    session_id,
                })
        })
    }

    /// Atomically remove and return the binding for `surface`. Restore is a
    /// consume operation: agents that report the resumed session record it
    /// again, while a stale/empty session cannot loop on every app launch.
    pub fn take_surface(&self, surface: SurfaceId) -> io::Result<Option<SavedAgentSession>> {
        self.with_exclusive_lock(|| {
            for agent in RESUMABLE_AGENTS {
                let mut map = self.load(agent)?;
                let Some(session_id) = map.remove(&surface.to_string()) else {
                    continue;
                };
                let bytes = serde_json::to_vec_pretty(&map)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Self::write_atomic(&self.agent_path(agent), &bytes)?;
                return Ok(Some(SavedAgentSession {
                    agent: agent.to_string(),
                    session_id,
                }));
            }
            Ok(None)
        })
    }

    /// Forget any session previously recorded for `(agent, surface)`.
    /// Used when the surface is closed permanently.
    pub fn forget(&self, agent: &str, surface: SurfaceId) -> io::Result<()> {
        let Some(agent) = Self::normalize_agent(agent) else {
            return Ok(());
        };
        self.with_exclusive_lock(|| self.forget_unlocked(agent, surface))
    }

    fn forget_unlocked(&self, agent: &str, surface: SurfaceId) -> io::Result<()> {
        let mut map = self.load(agent)?;
        if map.remove(&surface.to_string()).is_some() {
            let bytes = serde_json::to_vec_pretty(&map)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Self::write_atomic(&self.agent_path(agent), &bytes)?;
        }
        Ok(())
    }

    pub fn forget_surface(&self, surface: SurfaceId) -> io::Result<()> {
        self.with_exclusive_lock(|| {
            for agent in RESUMABLE_AGENTS {
                self.forget_unlocked(agent, surface)?;
            }
            Ok(())
        })
    }

    /// Total entries stored for `agent`. Useful for tests / diagnostics.
    pub fn len(&self, agent: &str) -> usize {
        Self::normalize_agent(agent)
            .and_then(|agent| self.load(agent).ok())
            .map(|map| map.len())
            .unwrap_or(0)
    }

    pub fn is_empty(&self, agent: &str) -> bool {
        self.len(agent) == 0
    }
}

pub fn default_agent_session_store() -> Option<AgentSessionStore> {
    flowmux_config::paths::data_dir().map(|dir| AgentSessionStore::new(dir.join("agent-sessions")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn store() -> (AgentSessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let s = AgentSessionStore::new(dir.path().to_path_buf());
        (s, dir)
    }

    #[test]
    fn lookup_returns_none_when_no_file_exists() {
        let (s, _td) = store();
        assert_eq!(s.lookup("claude", SurfaceId::new()), None);
        assert_eq!(s.len("claude"), 0);
    }

    #[test]
    fn record_then_lookup_returns_session_id() {
        let (s, _td) = store();
        let surface = SurfaceId::new();
        s.record("claude", surface, "sess-abc").unwrap();
        assert_eq!(s.lookup("claude", surface), Some("sess-abc".into()));
        assert_eq!(s.len("claude"), 1);
    }

    #[test]
    fn antigravity_session_round_trips_with_canonical_name() {
        let (s, _td) = store();
        let surface = SurfaceId::new();
        s.record("Antigravity", surface, "conversation-1").unwrap();
        let saved = s.lookup_surface(surface).unwrap();
        assert_eq!(saved.agent, "antigravity");
        assert_eq!(
            saved.resume_argv(),
            ["agy", "--conversation", "conversation-1"]
        );
    }

    #[test]
    fn record_overwrites_existing_session_id() {
        let (s, _td) = store();
        let surface = SurfaceId::new();
        s.record("claude", surface, "sess-old").unwrap();
        s.record("claude", surface, "sess-new").unwrap();
        assert_eq!(s.lookup("claude", surface), Some("sess-new".into()));
        assert_eq!(s.len("claude"), 1);
    }

    #[test]
    fn forget_removes_only_the_targeted_surface() {
        let (s, _td) = store();
        let a = SurfaceId::new();
        let b = SurfaceId::new();
        s.record("claude", a, "sess-a").unwrap();
        s.record("claude", b, "sess-b").unwrap();
        s.forget("claude", a).unwrap();
        assert_eq!(s.lookup("claude", a), None);
        assert_eq!(s.lookup("claude", b), Some("sess-b".into()));
    }

    #[test]
    fn forget_surface_clears_binding_without_touching_other_tabs() {
        let (s, _td) = store();
        let closed = SurfaceId::new();
        let kept = SurfaceId::new();
        s.record("opencode", closed, "closed-session").unwrap();
        s.record("opencode", kept, "kept-session").unwrap();

        s.forget_surface(closed).unwrap();
        assert_eq!(s.lookup_surface(closed), None);
        assert_eq!(s.lookup("opencode", kept), Some("kept-session".into()));
    }

    #[test]
    fn agents_are_isolated_in_separate_files() {
        let (s, td) = store();
        let claude_surface = SurfaceId::new();
        let codex_surface = SurfaceId::new();
        s.record("claude", claude_surface, "claude-sess").unwrap();
        s.record("codex", codex_surface, "codex-sess").unwrap();
        assert_eq!(
            s.lookup("claude", claude_surface),
            Some("claude-sess".into())
        );
        assert_eq!(s.lookup("codex", codex_surface), Some("codex-sess".into()));
        // Two distinct files on disk.
        assert!(td.path().join("claude.json").exists());
        assert!(td.path().join("codex.json").exists());
    }

    #[test]
    fn recording_new_agent_replaces_prior_surface_binding() {
        let (s, _td) = store();
        let surface = SurfaceId::new();
        s.record("claude", surface, "claude-session").unwrap();
        s.record("codex", surface, "codex-session").unwrap();

        assert_eq!(s.lookup("claude", surface), None);
        assert_eq!(
            s.lookup_surface(surface),
            Some(SavedAgentSession {
                agent: "codex".into(),
                session_id: "codex-session".into(),
            })
        );
    }

    #[test]
    fn take_surface_consumes_only_the_resumed_binding() {
        let (s, _td) = store();
        let resumed = SurfaceId::new();
        let kept = SurfaceId::new();
        s.record("claude", resumed, "resume-once").unwrap();
        s.record("codex", kept, "keep-me").unwrap();

        assert_eq!(
            s.take_surface(resumed).unwrap(),
            Some(SavedAgentSession {
                agent: "claude".into(),
                session_id: "resume-once".into(),
            })
        );
        assert_eq!(s.lookup_surface(resumed), None);
        assert_eq!(s.lookup("codex", kept), Some("keep-me".into()));
        assert_eq!(s.take_surface(resumed).unwrap(), None);
    }

    #[test]
    fn concurrent_hook_writes_do_not_drop_other_surface_bindings() {
        let (s, _td) = store();
        let surfaces: Vec<_> = (0..16).map(|_| SurfaceId::new()).collect();
        let threads: Vec<_> = surfaces
            .iter()
            .copied()
            .enumerate()
            .map(|(index, surface)| {
                let store = s.clone();
                std::thread::spawn(move || {
                    store
                        .record("claude", surface, &format!("session-{index}"))
                        .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        for (index, surface) in surfaces.into_iter().enumerate() {
            assert_eq!(
                s.lookup("claude", surface),
                Some(format!("session-{index}"))
            );
        }
    }

    #[test]
    fn resume_argv_matches_supported_agent_native_commands() {
        let cases = [
            ("claude", vec!["claude", "--resume", "session-1"]),
            ("codex", vec!["codex", "resume", "session-1"]),
            ("opencode", vec!["opencode", "--session", "session-1"]),
            ("antigravity", vec!["agy", "--conversation", "session-1"]),
        ];
        for (agent, expected) in cases {
            let saved = SavedAgentSession {
                agent: agent.into(),
                session_id: "session-1".into(),
            };
            assert_eq!(saved.resume_argv(), expected);
        }
    }

    #[test]
    fn resume_shell_command_quotes_opaque_session_id() {
        let td = tempfile::tempdir().unwrap();
        let agent = td.path().join("claude");
        let args_file = td.path().join("args");
        let marker = td.path().join("injected");
        std::fs::write(
            &agent,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n",
                args_file.display()
            ),
        )
        .unwrap();
        let mut perms = std::fs::metadata(&agent).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&agent, perms).unwrap();

        let session_id = format!("abc'; touch '{}'; printf '", marker.display());
        let saved = SavedAgentSession {
            agent: "claude".into(),
            session_id: session_id.clone(),
        };
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(saved.shell_command().unwrap())
            .env("PATH", td.path())
            .output()
            .unwrap();

        assert!(output.status.success());
        assert!(
            !marker.exists(),
            "session id must never execute as shell code"
        );
        assert_eq!(
            std::fs::read_to_string(args_file).unwrap(),
            format!("--resume\n{session_id}\n")
        );
    }

    #[test]
    fn resume_failure_clears_terminal_without_a_message() {
        let td = tempfile::tempdir().unwrap();
        let agent = td.path().join("codex");
        std::fs::write(&agent, "#!/bin/sh\nexit 7\n").unwrap();
        let mut perms = std::fs::metadata(&agent).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&agent, perms).unwrap();
        let saved = SavedAgentSession {
            agent: "codex".into(),
            session_id: "session-1".into(),
        };

        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(saved.shell_command().unwrap())
            .env("PATH", td.path())
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "wrapper shell must stay successful"
        );
        assert_eq!(output.stdout, b"\x1b[3J\x1b[2J\x1b[H\x1bc");
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn claude_resume_return_forgets_racing_session_start_binding() {
        let td = tempfile::tempdir().unwrap();
        let claude = td.path().join("claude");
        let flowmuxctl = td.path().join("flowmuxctl");
        let args_file = td.path().join("cleanup-args");
        let input_file = td.path().join("cleanup-input");
        std::fs::write(&claude, "#!/bin/sh\nexit 7\n").unwrap();
        std::fs::write(
            &flowmuxctl,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nIFS= read -r payload\nprintf '%s' \"$payload\" > '{}'\n",
                args_file.display(),
                input_file.display()
            ),
        )
        .unwrap();
        for executable in [&claude, &flowmuxctl] {
            let mut perms = std::fs::metadata(executable).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(executable, perms).unwrap();
        }
        let saved = SavedAgentSession {
            agent: "claude".into(),
            session_id: "stale-session".into(),
        };

        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(saved.shell_command().unwrap())
            .env("PATH", td.path())
            .output()
            .unwrap();

        assert!(output.status.success());
        assert_eq!(output.stdout, b"\x1b[3J\x1b[2J\x1b[H\x1bc");
        assert_eq!(
            std::fs::read_to_string(args_file).unwrap(),
            "hooks\nclaude\nsession-end\n"
        );
        assert_eq!(
            std::fs::read_to_string(input_file).unwrap(),
            r#"{"reason":"prompt_input_exit"}"#
        );
    }

    #[test]
    fn resumable_agents_emit_silent_cleanup_hooks() {
        for (agent, expected) in [
            (
                "codex",
                "flowmuxctl hooks codex stop '{\"reason\":\"flowmux_resume_returned\"}'",
            ),
            (
                "opencode",
                "flowmuxctl hooks opencode stop '{\"reason\":\"flowmux_resume_returned\"}'",
            ),
            (
                "antigravity",
                "printf '%s' '{\"reason\":\"flowmux_resume_returned\"}' | flowmuxctl hooks antigravity stop",
            ),
        ] {
            let saved = SavedAgentSession {
                agent: agent.into(),
                session_id: "stale-session".into(),
            };
            assert!(saved.shell_command().unwrap().contains(expected));
        }
    }

    #[test]
    fn missing_agent_binary_clears_terminal_without_a_message() {
        let td = tempfile::tempdir().unwrap();
        let saved = SavedAgentSession {
            agent: "opencode".into(),
            session_id: "session-1".into(),
        };
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(saved.shell_command().unwrap())
            .env("PATH", td.path())
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"\x1b[3J\x1b[2J\x1b[H\x1bc");
        assert!(output.stderr.is_empty());
    }

    #[test]
    fn write_is_atomic_no_tmp_left_behind_on_success() {
        let (s, td) = store();
        s.record("claude", SurfaceId::new(), "sess").unwrap();
        // Atomic-replace policy: only the final `claude.json` exists,
        // no `claude.tmp.<pid>` lingering.
        for entry in std::fs::read_dir(td.path()).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(
                !name.contains("tmp."),
                "stale tmp file from atomic write: {name}"
            );
        }
    }

    /// Scenario: cmux-equivalent restart flow. Daemon boot calls
    /// `lookup` for every recorded (agent, surface) and rehydrates
    /// the session. Verify the round-trip: write, drop the in-memory
    /// store, recreate it pointing at the same dir, look up.
    #[test]
    fn scenario_session_survives_daemon_restart_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let claude_surface = SurfaceId::new();
        let codex_surface = SurfaceId::new();

        // First "boot": record a couple of sessions.
        {
            let s = AgentSessionStore::new(dir.path().to_path_buf());
            s.record("claude", claude_surface, "sess-1").unwrap();
            s.record("codex", codex_surface, "sess-2").unwrap();
        }

        // Second "boot": fresh store reads the same files.
        let s = AgentSessionStore::new(dir.path().to_path_buf());
        assert_eq!(s.lookup("claude", claude_surface), Some("sess-1".into()));
        assert_eq!(s.lookup("codex", codex_surface), Some("sess-2".into()));
    }

    #[test]
    fn malformed_file_yields_invalid_data_error_on_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude.json"), "not json {{").unwrap();
        let s = AgentSessionStore::new(dir.path().to_path_buf());
        // load surfaces InvalidData; lookup just returns None
        // (defensive — a bad file shouldn't crash the daemon).
        assert_eq!(s.lookup("claude", SurfaceId::new()), None);
    }
}
