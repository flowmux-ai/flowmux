// SPDX-License-Identifier: GPL-3.0-or-later
//! Share credential-free usage observations with the Team helper.

use super::{FieldRefresh, Provider, ProviderRefresh};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub(crate) fn publish(home: &Path, refreshes: &[ProviderRefresh; 2]) -> io::Result<()> {
    let Some(state) = flowmux_config::paths::state_dir() else {
        return Ok(());
    };
    let directory = state.join("team-usage");
    for refresh in refreshes {
        let (agent, variable, default) = match refresh.provider {
            Provider::Claude => ("claude", "CLAUDE_CONFIG_DIR", ".claude"),
            Provider::Codex => ("codex", "CODEX_HOME", ".codex"),
        };
        let configured = std::env::var_os(variable)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(default));
        let profile = configured.canonicalize().unwrap_or(configured);
        let profile = if profile.is_absolute() {
            profile
        } else {
            std::env::current_dir()?.join(profile)
        };
        write_snapshot(&directory, agent, &profile, refresh)?;
    }
    Ok(())
}

fn write_snapshot(
    directory: &Path,
    agent: &str,
    profile: &Path,
    refresh: &ProviderRefresh,
) -> io::Result<PathBuf> {
    let profile = profile.to_string_lossy();
    let mut value = json!({
        "schema": 1, "agent": agent, "profile": profile,
        "collected_at": refresh.collected_at.timestamp(),
    });
    match &refresh.limits {
        FieldRefresh::Success(windows) => {
            value["windows"] = Value::Array(
                windows
                    .iter()
                    .map(|window| {
                        json!({"scope": window.scope, "used_percent": window.used_percent,
                            "resets_at": window.resets_at.map(|time| time.timestamp())})
                    })
                    .collect(),
            );
        }
        FieldRefresh::Failure(_) => value["error"] = json!("Usage refresh unavailable"),
    }
    fs::create_dir_all(directory)?;
    let key = format!("{agent}-{:x}", Sha256::digest(profile.as_bytes()));
    let target = directory.join(format!("{key}.json"));
    let temporary = directory.join(format!(".{key}-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, &value)?;
        file.write_all(b"\n")?;
        fs::rename(&temporary, &target)?;
        Ok(target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{TokenTotals, UsageError, UsageErrorKind, UsageWindow};
    use chrono::Utc;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn snapshots_preserve_scopes_and_reset_times_without_tokens_or_errors() {
        let root = tempfile::tempdir().unwrap();
        let now = Utc::now();
        let mut refresh = ProviderRefresh {
            provider: Provider::Claude,
            tokens: FieldRefresh::Success(TokenTotals {
                today: Some(123),
                lifetime: None,
            }),
            limits: FieldRefresh::Success(vec![UsageWindow {
                source_id: None,
                label: "Weekly".into(),
                scope: Some("Opus".into()),
                used_percent: 100.0,
                duration_minutes: None,
                resets_at: Some(now),
            }]),
            collected_at: now,
        };
        let path =
            write_snapshot(root.path(), "claude", Path::new("/profile-a"), &refresh).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["profile"], "/profile-a");
        assert_eq!(value["windows"][0]["scope"], "Opus");
        assert_eq!(value["windows"][0]["resets_at"], now.timestamp());
        assert!(value.get("tokens").is_none());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let other =
            write_snapshot(root.path(), "claude", Path::new("/profile-b"), &refresh).unwrap();
        assert_ne!(path, other);

        refresh.limits =
            FieldRefresh::Failure(UsageError::new(UsageErrorKind::Io, "private diagnostic"));
        write_snapshot(root.path(), "claude", Path::new("/profile-a"), &refresh).unwrap();
        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert!(value.get("windows").is_none());
        assert_eq!(value["error"], "Usage refresh unavailable");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }
}
