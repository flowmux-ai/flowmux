// SPDX-License-Identifier: GPL-3.0-or-later

use super::{
    cli_version, duration_label, native_program, reset_timestamp, FieldRefresh, Provider,
    ProviderRefresh, TokenTotals, UsageError, UsageErrorKind, UsageWindow,
};
use chrono::Utc;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::{io::AsyncReadExt, process::Command};

pub(crate) async fn collect(home: PathBuf) -> ProviderRefresh {
    ProviderRefresh {
        provider: Provider::Antigravity,
        tokens: FieldRefresh::Success(TokenTotals::default()),
        limits: match read_limits(&home).await {
            Ok(windows) => FieldRefresh::Success(windows),
            Err(error) => FieldRefresh::Failure(error),
        },
        collected_at: Utc::now(),
    }
}

async fn read_limits(home: &Path) -> Result<Vec<UsageWindow>, UsageError> {
    let executable = native_program(home, "agy").ok_or_else(|| {
        UsageError::new(
            UsageErrorKind::NotInstalled,
            "Antigravity CLI was not found.",
        )
    })?;
    let work = tempfile::tempdir().map_err(|_| {
        UsageError::new(
            UsageErrorKind::Io,
            "Could not start the Antigravity quota check.",
        )
    })?;
    let mut command = Command::new(&executable);
    command
        .current_dir(work.path())
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .env("BROWSER", "/usr/bin/false")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY");

    let mut version = Command::new(executable);
    version
        .current_dir(work.path())
        .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
        .arg("--version");
    let version = capture(&mut version, Duration::from_secs(5)).await?;
    if !cli_version(&version).is_some_and(|version| version >= (1, 1, 11)) {
        return Err(UsageError::new(
            UsageErrorKind::Unavailable,
            "Antigravity CLI 1.1.11 or newer is required for quota checks.",
        ));
    }

    // Older versions can submit an unknown slash command as a model prompt.
    command.args([
        "-p",
        "/usage",
        "--output-format",
        "json",
        "--print-timeout",
        "20s",
    ]);
    let output = capture(&mut command, Duration::from_secs(25)).await?;
    parse_limits(&output)
}

async fn capture(command: &mut Command, timeout: Duration) -> Result<String, UsageError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| UsageError::new(UsageErrorKind::Io, "Could not start Antigravity CLI."))?;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let result = tokio::time::timeout(timeout, async {
        const MAX_BYTES: u64 = 512 * 1024;
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .ok()?;
        if bytes.len() as u64 > MAX_BYTES {
            return None;
        }
        child.wait().await.ok()?;
        String::from_utf8(bytes).ok()
    })
    .await;
    match result {
        Ok(Some(output)) => Ok(output),
        Ok(None) => Err(UsageError::new(
            UsageErrorKind::InvalidData,
            "Could not read the Antigravity quota response.",
        )),
        Err(_) => Err(UsageError::new(
            UsageErrorKind::Timeout,
            "Antigravity quota check timed out.",
        )),
    }
}

fn parse_limits(stdout: &str) -> Result<Vec<UsageWindow>, UsageError> {
    let mut signed_out = false;
    for response in stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        if response["num_turns"]
            .as_u64()
            .is_some_and(|turns| turns > 0)
            || response["conversation_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        {
            return Err(UsageError::new(
                UsageErrorKind::Unavailable,
                "This Antigravity CLI did not run /usage as a quota command.",
            ));
        }
        let command = &response["command"];
        if response["status"] == "SUCCESS" && command["name"] == "usage" {
            if let Some(groups) = command["data"]["groups"].as_array() {
                let mut windows = groups.iter().flat_map(group_windows).peekable();
                if windows.peek().is_some() {
                    return Ok(windows.collect());
                }
            }
            return Err(UsageError::new(
                UsageErrorKind::Unavailable,
                "Antigravity reported no account quota.",
            ));
        }
        if response["status"] == "ERROR" {
            let error = response["error"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            signed_out |= ["sign in", "sign-in", "not logged", "login required"]
                .iter()
                .any(|word| error.contains(word));
        }
    }
    if signed_out {
        Err(UsageError::new(
            UsageErrorKind::NotLoggedIn,
            "Sign in with agy to view Antigravity quotas.",
        ))
    } else {
        Err(UsageError::new(
            UsageErrorKind::Unavailable,
            "Antigravity CLI did not report account quotas.",
        ))
    }
}

fn group_windows(group: &Value) -> impl Iterator<Item = UsageWindow> + '_ {
    let name = group["name"]
        .as_str()
        .filter(|name| !name.is_empty() && !name.chars().any(char::is_control));
    group["buckets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(move |bucket| {
            let name = name?;
            let id = bucket["id"].as_str().filter(|id| !id.is_empty())?;
            let remaining = bucket["remaining_fraction"]
                .as_f64()
                .filter(|value| (0.0..=1.0).contains(value))?;
            if bucket["disabled"] == true {
                return None;
            }
            let duration_minutes = match bucket["window"].as_str() {
                Some("5h") => Some(300),
                Some("daily" | "24h") => Some(1_440),
                Some("weekly" | "1w") => Some(10_080),
                _ => None,
            };
            let label = match duration_minutes {
                Some(_) => duration_label(duration_minutes),
                None => bucket["name"]
                    .as_str()
                    .filter(|name| !name.chars().any(char::is_control))
                    .unwrap_or("Usage limit")
                    .to_owned(),
            };
            Some(UsageWindow {
                source_id: Some(id.to_owned()),
                label,
                scope: Some(name.to_owned()),
                used_percent: (1.0 - remaining) * 100.0,
                duration_minutes,
                resets_at: reset_timestamp(&bucket["reset_time"])
                    .filter(|date| date.timestamp() > 0),
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_command_keeps_group_and_window_identity_and_ignores_disabled_buckets() {
        let output = concat!(
            "startup log\n",
            r#"{"status":"SUCCESS","command":{"name":"usage","data":{"groups":[
            {"name":"Gemini Models","buckets":[
                {"id":"gemini-weekly","window":"weekly","remaining_fraction":0.25,"reset_time":"2030-01-02T03:00:00Z"},
                {"id":"gemini-5h","window":"5h","remaining_fraction":1,"disabled":true}]},
            {"name":"Claude and GPT models","buckets":[
                {"id":"3p-weekly","window":"weekly","remaining_fraction":0,"reset_time":"1970-01-01T00:00:00Z"}]}
        ]}}}"#
        );
        let output = output
            .replace('\n', " ")
            .replacen("startup log ", "startup log\n", 1);
        let windows = parse_limits(&output).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].scope.as_deref(), Some("Gemini Models"));
        assert_eq!(windows[0].used_percent, 75.0);
        assert!(windows[0].resets_at.is_some());
        assert_eq!(windows[1].source_id.as_deref(), Some("3p-weekly"));
        assert_eq!(windows[1].duration_minutes, Some(10_080));
        assert_eq!(windows[1].used_percent, 100.0);
        assert_eq!(windows[1].resets_at, None);
        assert!(parse_limits(r#"{"status":"SUCCESS","response":"usage","num_turns":1}"#).is_err());
    }
}
