// SPDX-License-Identifier: GPL-3.0-or-later

use super::{duration_label, TokenTotals, UsageError, UsageErrorKind, UsageWindow};
use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashSet;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub fn claude_limits(value: &Value) -> Result<Vec<UsageWindow>, UsageError> {
    let object = value.as_object().ok_or_else(|| {
        UsageError::new(
            UsageErrorKind::InvalidData,
            "The Claude usage response was invalid.",
        )
    })?;
    let mut windows = Vec::new();
    if let Some(window) = parse_named_window(object.get("five_hour"), Some(300), None)? {
        windows.push(window);
    }
    if let Some(window) = parse_named_window(object.get("seven_day"), Some(10_080), None)? {
        windows.push(window);
    }
    // Model- or feature-scoped windows (e.g. "seven_day_opus", "fable") arrive
    // as extra top-level objects; keep them all instead of the fixed two.
    // Enterprise plans report no five_hour/seven_day window at all — their seat
    // limits arrive only through these extra objects.
    // "spend" restates "extra_usage" in money with a rounded percent, so it is
    // dropped whenever the finer-grained section is present.
    let has_extra_usage = object.get("extra_usage").is_some_and(Value::is_object);
    for (key, entry) in object {
        if matches!(key.as_str(), "five_hour" | "seven_day" | "limits")
            || (key == "spend" && has_extra_usage)
            || !entry.is_object()
        {
            continue;
        }
        let (duration_minutes, scope) = window_metadata(key);
        if let Some(window) = parse_named_window(Some(entry), duration_minutes, scope)? {
            windows.push(window);
        }
    }
    // Same for the optional "limits" array, which carries kind/group metadata.
    if let Some(limits) = object.get("limits").and_then(Value::as_array) {
        for limit in limits {
            let duration_minutes = limit
                .get("kind")
                .and_then(Value::as_str)
                .and_then(kind_duration_minutes);
            let scope = limit.get("group").and_then(Value::as_str).map(scope_label);
            if let Some(window) = parse_named_window(Some(limit), duration_minutes, scope)? {
                windows.push(window);
            }
        }
    }
    deduplicate_windows(&mut windows);
    Ok(windows)
}

fn window_metadata(key: &str) -> (Option<u64>, Option<String>) {
    if let Some(rest) = key.strip_prefix("five_hour_") {
        (Some(300), Some(scope_label(rest)))
    } else if let Some(rest) = key.strip_prefix("seven_day_") {
        (Some(10_080), Some(scope_label(rest)))
    } else {
        (None, Some(scope_label(key)))
    }
}

fn kind_duration_minutes(kind: &str) -> Option<u64> {
    match kind {
        "five_hour" => Some(300),
        "daily" => Some(1_440),
        "weekly" | "seven_day" => Some(10_080),
        _ => None,
    }
}

fn scope_label(raw: &str) -> String {
    let label = raw.replace('_', " ");
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => label,
    }
}

fn parse_named_window(
    value: Option<&Value>,
    duration_minutes: Option<u64>,
    scope: Option<String>,
) -> Result<Option<UsageWindow>, UsageError> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(UsageError::new(
            UsageErrorKind::InvalidData,
            "The Claude rate limit response was invalid.",
        ));
    };
    let Some(used_percent) = object
        .get("utilization")
        .or_else(|| object.get("percent"))
        .and_then(Value::as_f64)
    else {
        return Ok(None);
    };
    let resets_at = object
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    Ok(Some(UsageWindow {
        label: duration_label(duration_minutes),
        scope,
        used_percent,
        duration_minutes,
        resets_at,
    }))
}

fn deduplicate_windows(windows: &mut Vec<UsageWindow>) {
    let mut seen = HashSet::new();
    windows.retain(|window| {
        seen.insert((
            window.duration_minutes,
            window.resets_at,
            window.used_percent.to_bits(),
            window.scope.clone(),
        ))
    });
}

#[derive(Deserialize)]
struct TranscriptEntry {
    #[serde(rename = "type")]
    kind: Option<String>,
    timestamp: Option<String>,
    uuid: Option<String>,
    message: Option<TranscriptMessage>,
}

#[derive(Deserialize)]
struct TranscriptMessage {
    id: Option<String>,
    usage: Option<TranscriptUsage>,
}

#[derive(Default, Deserialize)]
struct TranscriptUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

impl TranscriptUsage {
    fn total(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.cache_read_input_tokens)
    }
}

// Bound discovery, allocation and reads independently: exceeding any bound fails
// the whole token field, so a truncated scan is never reported as today's total.
const MAX_ENTRIES: usize = 20_000;
const MAX_FILES: usize = 4_096;
const MAX_FILE_BYTES: usize = 32 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

fn check(cancel: &AtomicBool, deadline: Instant) -> Result<(), UsageError> {
    if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
        return Err(UsageError::new(
            UsageErrorKind::Timeout,
            "The Claude token history scan was cancelled or timed out.",
        ));
    }
    Ok(())
}

fn limit_error() -> UsageError {
    UsageError::new(
        UsageErrorKind::InvalidData,
        "The Claude token history exceeds the scan limit; no partial total is shown.",
    )
}

fn transcript_io_error() -> UsageError {
    UsageError::new(
        UsageErrorKind::Io,
        "Could not read the local Claude usage history.",
    )
}

/// `root` is Claude's projects directory. Dates use the caller's fixed local
/// offset, just like the Linux collector; original strings are never normalized.
/// Filesystem syscalls themselves cannot be interrupted while inside the OS.
pub fn claude_tokens(
    root: &Path,
    day: NaiveDate,
    offset: FixedOffset,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<TokenTotals, UsageError> {
    check(cancel, deadline)?;
    match std::fs::metadata(root) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Err(missing_history()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(missing_history()),
        Err(_) => return Err(transcript_io_error()),
    }
    let mut directories = vec![root.to_path_buf()];
    let mut entries_seen = 0_usize;
    let mut files_seen = 0_usize;
    let mut bytes_seen = 0_usize;
    let mut seen = HashSet::new();
    let mut total = 0_u64;
    while let Some(directory) = directories.pop() {
        check(cancel, deadline)?;
        for entry in std::fs::read_dir(directory).map_err(|_| transcript_io_error())? {
            check(cancel, deadline)?;
            entries_seen += 1;
            if entries_seen > MAX_ENTRIES {
                return Err(limit_error());
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(transcript_io_error()),
            };
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(transcript_io_error()),
            };
            // Linux skips symlinks. Also skip Windows junction/reparse entries
            // instead of recursively revisiting an external or cyclic tree.
            if metadata.file_type().is_symlink() || is_reparse(&metadata) {
                continue;
            }
            let path = entry.path();
            if metadata.is_dir() {
                directories.push(path);
            } else if metadata.is_file()
                && path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            {
                files_seen += 1;
                if files_seen > MAX_FILES || metadata.len() > MAX_FILE_BYTES as u64 {
                    return Err(limit_error());
                }
                let file = match File::open(path) {
                    Ok(file) => file,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(_) => return Err(transcript_io_error()),
                };
                let mut reader = BufReader::new(file);
                let mut file_bytes = 0_usize;
                let mut line = Vec::new();
                loop {
                    check(cancel, deadline)?;
                    let available = reader.fill_buf().map_err(|_| transcript_io_error())?;
                    if available.is_empty() {
                        if !line.is_empty() {
                            total = total
                                .saturating_add(transcript_line(&line, day, offset, &mut seen)?);
                        }
                        break;
                    }
                    let end = available.iter().position(|&byte| byte == b'\n');
                    let count = end.map_or(available.len(), |index| index + 1);
                    file_bytes += count;
                    bytes_seen += count;
                    if file_bytes > MAX_FILE_BYTES
                        || bytes_seen > MAX_TOTAL_BYTES
                        || line.len() + count > MAX_LINE_BYTES
                    {
                        return Err(limit_error());
                    }
                    line.extend_from_slice(&available[..count]);
                    reader.consume(count);
                    if end.is_some() {
                        total =
                            total.saturating_add(transcript_line(&line, day, offset, &mut seen)?);
                        line.clear();
                    }
                }
            }
        }
    }
    check(cancel, deadline)?;
    Ok(TokenTotals {
        today: Some(total),
        lifetime: None,
    })
}

fn missing_history() -> UsageError {
    UsageError::new(
        UsageErrorKind::NotLoggedIn,
        "The local Claude usage history was not found.",
    )
}

#[cfg(windows)]
fn is_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse(_: &std::fs::Metadata) -> bool {
    false
}

fn transcript_line(
    line: &[u8],
    day: NaiveDate,
    offset: FixedOffset,
    seen: &mut HashSet<String>,
) -> Result<u64, UsageError> {
    // Match Linux: malformed/incomplete JSON records are skipped, while an
    // unreadable UTF-8 stream fails the field rather than silently losing data.
    let line = std::str::from_utf8(line).map_err(|_| transcript_io_error())?;
    let Ok(entry) = serde_json::from_str::<TranscriptEntry>(line) else {
        return Ok(0);
    };
    if entry.kind.as_deref() != Some("assistant") {
        return Ok(0);
    }
    let Some(timestamp) = entry
        .timestamp
        .as_deref()
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
    else {
        return Ok(0);
    };
    if timestamp.with_timezone(&offset).date_naive() != day {
        return Ok(0);
    }
    let Some(message) = entry.message else {
        return Ok(0);
    };
    let Some(id) = message.id.or(entry.uuid) else {
        return Ok(0);
    };
    let Some(usage) = message.usage else {
        return Ok(0);
    };
    Ok(if seen.insert(id) { usage.total() } else { 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Temp(std::path::PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("flowmux-usage-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn transcript_files_preserve_unicode_deduplicate_and_fail_without_partial_totals() {
        let temp = Temp::new();
        let day = NaiveDate::from_ymd_opt(2026, 7, 14).unwrap();
        let offset = FixedOffset::east_opt(9 * 3600).unwrap();
        let cancelled = AtomicBool::new(false);
        let record = serde_json::json!({"type":"assistant", "timestamp":"2026-07-13T15:30:00Z",
            "uuid":"한글한😀", "message":{"id":"중복한", "usage":{"input_tokens":2,
            "output_tokens":3,"cache_creation_input_tokens":5,"cache_read_input_tokens":7}}})
        .to_string();
        let nested = temp.0.join("프로젝트한");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(temp.0.join("한글.jsonl"), format!("{record}\nnot-json\n")).unwrap();
        std::fs::write(nested.join("복사.jsonl"), &record).unwrap();
        let scan = || {
            claude_tokens(
                &temp.0,
                day,
                offset,
                &cancelled,
                Instant::now() + Duration::from_secs(2),
            )
        };
        assert_eq!(scan().unwrap().today, Some(17));
        assert_eq!(
            claude_tokens(
                &temp.0,
                day.pred_opt().unwrap(),
                offset,
                &cancelled,
                Instant::now() + Duration::from_secs(2)
            )
            .unwrap()
            .today,
            Some(0)
        );
        cancelled.store(true, Ordering::Relaxed);
        assert_eq!(scan().unwrap_err().kind, UsageErrorKind::Timeout);
        cancelled.store(false, Ordering::Relaxed);
        assert_eq!(
            claude_tokens(&temp.0, day, offset, &cancelled, Instant::now())
                .unwrap_err()
                .kind,
            UsageErrorKind::Timeout
        );
        std::fs::write(temp.0.join("invalid.jsonl"), [0xff]).unwrap();
        assert_eq!(scan().unwrap_err().kind, UsageErrorKind::Io);
        std::fs::remove_file(temp.0.join("invalid.jsonl")).unwrap();
        let large = File::create(temp.0.join("large.jsonl")).unwrap();
        large.set_len(MAX_FILE_BYTES as u64 + 1).unwrap();
        drop(large);
        assert_eq!(scan().unwrap_err().kind, UsageErrorKind::InvalidData);
    }
    #[test]
    fn usage_response_keeps_every_reported_window() {
        let value = serde_json::json!({
            "five_hour": {
                "utilization": 7.0,
                "resets_at": "2026-07-14T10:00:00Z"
            },
            "seven_day": {
                "utilization": 42.0,
                "resets_at": "2026-07-20T00:00:00Z"
            },
            "seven_day_opus": {
                "utilization": 12.0,
                "resets_at": "2026-07-20T00:00:00Z"
            },
            "extra_window": {
                "utilization": 3.0
            },
            "not_a_window": "ignored",
            "limits": [{
                "kind": "weekly",
                "group": "fable",
                "percent": 78.0,
                "resets_at": "2026-07-19T00:00:00Z"
            }]
        });

        let windows = claude_limits(&value).unwrap();

        assert_eq!(windows[0].duration_minutes, Some(300));
        assert_eq!(windows[0].scope, None);
        assert_eq!(windows[1].duration_minutes, Some(10_080));
        assert_eq!(windows[1].scope, None);
        assert!(windows.iter().any(|window| {
            window.scope.as_deref() == Some("Opus")
                && window.duration_minutes == Some(10_080)
                && window.used_percent == 12.0
        }));
        assert!(windows.iter().any(|window| {
            window.scope.as_deref() == Some("Extra window") && window.duration_minutes.is_none()
        }));
        assert!(windows.iter().any(|window| {
            window.scope.as_deref() == Some("Fable")
                && window.duration_minutes == Some(10_080)
                && window.used_percent == 78.0
        }));
    }

    #[test]
    fn usage_response_allows_missing_optional_sections() {
        assert!(claude_limits(&serde_json::json!({})).unwrap().is_empty());
    }

    #[test]
    fn enterprise_response_keeps_seat_limits_and_drops_the_spend_restatement() {
        let value = serde_json::json!({
            "five_hour": null,
            "seven_day": null,
            "seven_day_opus": null,
            "cinder_cove": {
                "utilization": 100.0,
                "resets_at": "2026-09-10T01:35:21.514375+00:00",
                "limit_dollars": 1000,
                "used_dollars": 1000.0
            },
            "extra_usage": {"monthly_limit": 46600, "used_credits": 25217.0, "utilization": 54.5},
            "spend": {"percent": 54, "used": {"amount_minor": 25217}},
            "limits": [],
            "member_dashboard_available": true
        });

        let windows = claude_limits(&value).unwrap();

        assert_eq!(windows.len(), 2);
        assert!(windows
            .iter()
            .any(|window| window.scope.as_deref() == Some("Cinder cove")
                && window.used_percent == 100.0));
        assert!(windows
            .iter()
            .any(|window| window.scope.as_deref() == Some("Extra usage")
                && window.used_percent == 54.5));
        assert!(!windows
            .iter()
            .any(|window| window.scope.as_deref() == Some("Spend")));
    }

    #[test]
    fn spend_survives_when_it_is_the_only_credit_section() {
        let value = serde_json::json!({"spend": {"percent": 54}});

        let windows = claude_limits(&value).unwrap();

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].scope.as_deref(), Some("Spend"));
        assert_eq!(windows[0].used_percent, 54.0);
    }
}
