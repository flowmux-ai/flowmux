// SPDX-License-Identifier: GPL-3.0-or-later

use super::{
    native_program, FieldRefresh, Provider, ProviderRefresh, TokenTotals, UsageError,
    UsageErrorKind, UsageWindow,
};
use chrono::{DateTime, Duration, Local, NaiveTime, Utc};
use flowmux_state::session_history::{list_sessions, SessionAgent};
use flowmux_terminal::pty::Pty;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(crate) async fn collect(home: PathBuf, check_quota: bool) -> ProviderRefresh {
    let profile = SessionAgent::Gemini
        .env_history_home()
        .unwrap_or_else(|| home.join(".gemini"));
    let tokens = tokio::task::spawn_blocking(move || sum_today(&profile));
    let limits = tokio::task::spawn_blocking(move || {
        if check_quota {
            match read_limits(&home) {
                Ok(windows) => FieldRefresh::Success(windows),
                Err(error) => FieldRefresh::Failure(error),
            }
        } else {
            FieldRefresh::Skipped
        }
    });
    let (tokens, limits) = tokio::join!(tokens, limits);
    let tokens = match tokens {
        Ok(Ok(totals)) => FieldRefresh::Success(totals),
        _ => FieldRefresh::Failure(UsageError::new(
            UsageErrorKind::Io,
            "Could not read Gemini CLI token history.",
        )),
    };
    let limits = match limits {
        Ok(limits) => limits,
        Err(_) => FieldRefresh::Failure(unavailable("Could not read Gemini CLI quotas.")),
    };
    ProviderRefresh {
        provider: Provider::Gemini,
        tokens,
        limits,
        collected_at: Utc::now(),
    }
}

fn sum_today(profile: &Path) -> io::Result<TokenTotals> {
    let today = Local::now().date_naive();
    let mut messages = HashMap::new();
    for session in list_sessions(SessionAgent::Gemini, profile)? {
        if DateTime::<Local>::from(session.modified).date_naive() < today {
            continue;
        }
        let file = match File::open(&session.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        while super::read_transcript_line(&mut reader, &mut line)? {
            let Ok(item) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            for message in item["$set"]["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .chain([&item])
            {
                if let Some((id, model, total)) = message_tokens(message, today) {
                    messages.insert(id.to_owned(), (model.to_owned(), total));
                }
            }
        }
    }
    let mut totals = BTreeMap::<_, u64>::new();
    for (model, amount) in messages.into_values() {
        let value = totals.entry(model).or_default();
        *value = value.saturating_add(amount);
    }
    Ok(TokenTotals {
        today: (!totals.is_empty()).then(|| totals.values().copied().fold(0, u64::saturating_add)),
        by_model: totals.into_iter().collect(),
        ..TokenTotals::default()
    })
}

fn message_tokens(message: &Value, today: chrono::NaiveDate) -> Option<(&str, &str, u64)> {
    if message["type"] != "gemini"
        || DateTime::parse_from_rfc3339(message["timestamp"].as_str()?)
            .ok()?
            .with_timezone(&Local)
            .date_naive()
            != today
    {
        return None;
    }
    let model = message["model"].as_str().filter(|model| {
        !model.is_empty() && model.len() <= 120 && !model.chars().any(char::is_control)
    })?;
    let total = message["tokens"]["total"].as_u64()?;
    Some((message["id"].as_str()?, model, total))
}

fn unavailable(message: &str) -> UsageError {
    UsageError::new(UsageErrorKind::Unavailable, message)
}

fn read_limits(home: &Path) -> Result<Vec<UsageWindow>, UsageError> {
    let program = native_program(home, "gemini").ok_or_else(|| {
        UsageError::new(UsageErrorKind::NotInstalled, "Gemini CLI was not found.")
    })?;
    let program = program
        .to_str()
        .ok_or_else(|| unavailable("Could not start Gemini CLI."))?;
    let temporary = tempfile::tempdir()
        .map_err(|_| unavailable("Could not start the Gemini CLI quota check."))?;
    // The model picker renders account quotas. Trust only this empty directory for the probe.
    let options = [
        program,
        "--skip-trust",
        "--screen-reader",
        "--extensions",
        "none",
        "--allowed-mcp-server-names",
        "flowmux-usage-probe",
        "--prompt-interactive",
        "/model",
    ];
    let env = [
        ("NO_BROWSER".into(), "true".into()),
        ("NO_COLOR".into(), "1".into()),
        ("TERM".into(), "xterm-256color".into()),
        ("TZ".into(), "UTC".into()),
    ];
    let mut pty = Pty::spawn(&options, Some(temporary.path()), &env, 150, 60)
        .map_err(|_| unavailable("Could not start Gemini CLI."))?;
    let deadline = Instant::now() + std::time::Duration::from_secs(15);
    let mut output = Vec::new();
    let mut chunk = [0; 8192];
    while Instant::now() < deadline && output.len() < 256 * 1024 {
        let mut fd = libc::pollfd {
            fd: pty.master_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd is one valid pollfd backed by the live PTY master.
        let ready = unsafe { libc::poll(&mut fd, 1, 200) };
        if ready <= 0 {
            continue;
        }
        let Ok(count) = pty.read(&mut chunk) else {
            break;
        };
        if count == 0 {
            break;
        }
        output.extend_from_slice(&chunk[..count]);
        let text = anstream::adapter::strip_str(&String::from_utf8_lossy(&output)).to_string();
        if text.contains("no longer supported for Gemini Code Assist for individuals") {
            return Err(unavailable(
                "Gemini Code Assist for individuals no longer supplies quotas to Gemini CLI.",
            ));
        }
        if text.contains("How would you like to authenticate")
            || text.contains("Paste the authorization code")
        {
            return Err(UsageError::new(
                UsageErrorKind::NotLoggedIn,
                "Sign in to an eligible account in Gemini CLI to view quotas.",
            ));
        }
        if text.contains("(Press Esc to close)") {
            if let Some(windows) = parse_quota(&text, Utc::now()) {
                return Ok(windows);
            }
            break;
        }
    }
    Err(unavailable(
        "Gemini CLI did not report account quotas. Use /model in Gemini CLI to check availability.",
    ))
}

fn parse_quota(text: &str, now: DateTime<Utc>) -> Option<Vec<UsageWindow>> {
    let (_, section) = text.rsplit_once("Model usage")?;
    let (section, _) = section.split_once("(Press Esc to close)")?;
    let rows = section
        .lines()
        .skip(1)
        .map(str::trim)
        .skip_while(|line| line.is_empty());
    let mut windows = Vec::new();
    for line in rows {
        let Some(window) = parse_quota_row(line, now) else {
            break;
        };
        windows.push(window);
    }
    (!windows.is_empty()).then_some(windows)
}

fn parse_quota_row(line: &str, now: DateTime<Utc>) -> Option<UsageWindow> {
    let (before_percent, after_percent) = line.split_once('%')?;
    let (label, percent) = before_percent.trim_end().rsplit_once(char::is_whitespace)?;
    let percent: u8 = percent.parse().ok().filter(|value| *value <= 100)?;
    let model = label.trim_end_matches(|c: char| {
        c.is_whitespace() || matches!(c, '▬' | '━' | '─' | '█' | '░' | '▒' | '▓')
    });
    if model.is_empty()
        || model.len() > 64
        || !model
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'))
    {
        return None;
    }
    let resets_at = after_percent
        .trim()
        .strip_prefix("Resets: ")
        .and_then(|text| parse_reset(text, now));
    Some(UsageWindow {
        source_id: Some(model.to_owned()),
        label: "Model quota".into(),
        scope: Some(model.into()),
        used_percent: f64::from(percent),
        duration_minutes: None,
        resets_at,
    })
}

fn parse_reset(text: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let (time, duration) = text.split_once(" (")?;
    let normalized = time.split_whitespace().collect::<Vec<_>>().join(" ");
    let time = NaiveTime::parse_from_str(&normalized, "%I:%M %p").ok()?;
    let mut minutes = 0_i64;
    for part in duration.strip_suffix(')')?.split_ascii_whitespace() {
        let (amount, unit) = part.split_at(part.len().checked_sub(1)?);
        let amount = amount.parse::<u16>().ok()?;
        minutes += match unit {
            "h" => i64::from(amount) * 60,
            "m" => i64::from(amount),
            _ => return None,
        };
    }
    let estimated = now + Duration::minutes(minutes);
    let rounded = estimated.date_naive().and_time(time).and_utc();
    [rounded, rounded - Duration::days(1)]
        .into_iter()
        .find(|time| (*time - estimated).num_minutes().abs() <= 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_gemini_model_groups_and_utc_reset_across_midnight() {
        let now = "2030-01-01T23:45:45Z".parse().unwrap();
        let reading = "Select Model\n─────\nModel usage\n\nPro         ▬▬▬▬▬▬    75%  Resets: 12:15 AM (30m)\nFlash Lite   ▬▬▬▬▬▬    12%  Resets: 1:45 AM (2h)\n\n(Press Esc to close)\n";
        let windows = parse_quota(reading, now).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].scope.as_deref(), Some("Pro"));
        assert_eq!(windows[0].used_percent, 75.0);
        assert_eq!(
            windows[0].resets_at,
            Some("2030-01-02T00:15:00Z".parse().unwrap())
        );
        assert_eq!(windows[1].scope.as_deref(), Some("Flash Lite"));
        assert!(parse_quota("Gemini response\n 75%", now).is_none());
    }

    #[test]
    fn token_history_counts_only_current_dated_model_records() {
        let timestamp = Utc::now();
        let today = timestamp.with_timezone(&Local).date_naive();
        let mut value = serde_json::json!({"id":"m", "type":"gemini", "timestamp":timestamp, "model":"gemini-pro", "tokens":{"total":42}});
        assert_eq!(message_tokens(&value, today), Some(("m", "gemini-pro", 42)));
        value["timestamp"] = serde_json::json!(timestamp - Duration::days(2));
        assert!(message_tokens(&value, today).is_none());
    }
}
