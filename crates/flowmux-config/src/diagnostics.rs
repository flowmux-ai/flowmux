// SPDX-License-Identifier: GPL-3.0-or-later
//! Process diagnostics shared by the GUI and CLI entrypoints.

use crate::paths;
use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use std::backtrace::Backtrace;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;

pub const MAX_CRASH_REPORTS: usize = 20;
const LAST_SEEN_FILE: &str = ".last-seen";
const LOG_BUFFERED_LINES_LIMIT: usize = 1_024;

/// Initialize console and daily file logging. `FLOWMUX_LOG`, when valid, is
/// applied to both layers; otherwise the console keeps the binary's existing
/// default and the file stays at `warn`.
pub fn init_logging(
    filename_prefix: &str,
    console_default: &str,
) -> anyhow::Result<Option<tracing_appender::non_blocking::WorkerGuard>> {
    let override_filter = std::env::var("FLOWMUX_LOG").ok();
    let console_filter = override_filter
        .as_deref()
        .and_then(|value| EnvFilter::try_new(value).ok())
        .unwrap_or_else(|| EnvFilter::new(console_default));

    let console = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stdout)
        .with_filter(console_filter);

    let Some(log_dir) = paths::logs_dir() else {
        tracing_subscriber::registry().with(console).try_init()?;
        return Ok(None);
    };
    if let Err(error) = fs::create_dir_all(&log_dir) {
        tracing_subscriber::registry().with(console).try_init()?;
        eprintln!(
            "flowmux: could not create log directory {}: {error}",
            log_dir.display()
        );
        return Ok(None);
    }

    let appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(filename_prefix)
        .build(&log_dir)
        .with_context(|| format!("open daily log under {}", log_dir.display()))?;
    let (writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(LOG_BUFFERED_LINES_LIMIT)
        .finish(appender);
    let file_filter = override_filter
        .as_deref()
        .and_then(|value| EnvFilter::try_new(value).ok())
        .unwrap_or_else(|| EnvFilter::new("warn"));
    let file = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(writer)
        .with_filter(file_filter);

    tracing_subscriber::registry()
        .with(console)
        .with(file)
        .try_init()?;
    Ok(Some(guard))
}

/// Install a best-effort panic hook that writes a synchronous crash report,
/// emits the same evidence through tracing, then chains to the previous hook.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = panic_payload(info);
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "unknown".into());
        let backtrace = Backtrace::force_capture().to_string();
        let timestamp = Utc::now();
        let report = format_crash_report(timestamp, &payload, &location, &backtrace);
        let crash_result = paths::crash_dir()
            .ok_or_else(|| io::Error::other("XDG state directory unavailable"))
            .and_then(|dir| write_crash_report(&dir, timestamp, &report));

        match &crash_result {
            Ok(path) => tracing::error!(
                panic_payload = %payload,
                panic_location = %location,
                backtrace = %backtrace,
                crash_file = %path.display(),
                "process panicked"
            ),
            Err(error) => tracing::error!(
                panic_payload = %payload,
                panic_location = %location,
                backtrace = %backtrace,
                %error,
                "process panicked; crash file write failed"
            ),
        }
        previous(info);
    }));
}

fn panic_payload(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(message) = info.payload().downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = info.payload().downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".into()
    }
}

fn format_crash_report(
    timestamp: DateTime<Utc>,
    payload: &str,
    location: &str,
    backtrace: &str,
) -> String {
    format!(
        "flowmux crash report\n\
         timestamp: {}\n\
         process: {}\n\
         payload: {payload}\n\
         location: {location}\n\
         backtrace:\n{backtrace}\n",
        timestamp.to_rfc3339_opts(SecondsFormat::Nanos, true),
        std::process::id(),
    )
}

/// Write one report and retain only the newest [`MAX_CRASH_REPORTS`] files.
/// Kept separate from the hook so retention and formatting can be tested
/// without panicking the test process.
pub fn write_crash_report(
    crash_dir: &Path,
    timestamp: DateTime<Utc>,
    report: &str,
) -> io::Result<PathBuf> {
    fs::create_dir_all(crash_dir)?;
    let filename = format!(
        "crash-{}.txt",
        timestamp.to_rfc3339_opts(SecondsFormat::Nanos, true)
    );
    let path = crash_dir.join(filename);
    fs::write(&path, report)?;
    prune_crash_reports(crash_dir)?;
    Ok(path)
}

fn crash_reports(crash_dir: &Path) -> io::Result<Vec<PathBuf>> {
    if !crash_dir.exists() {
        return Ok(Vec::new());
    }
    let mut reports = fs::read_dir(crash_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("crash-") && name.ends_with(".txt"))
        })
        .collect::<Vec<_>>();
    reports.sort();
    Ok(reports)
}

fn prune_crash_reports(crash_dir: &Path) -> io::Result<()> {
    let reports = crash_reports(crash_dir)?;
    let excess = reports.len().saturating_sub(MAX_CRASH_REPORTS);
    for path in reports.into_iter().take(excess) {
        fs::remove_file(path)?;
    }
    Ok(())
}

/// Return the newest crash report once, recording it as announced before the
/// GUI displays its startup toast. Later launches stay quiet until a newer
/// crash report appears.
pub fn take_unreported_crash() -> io::Result<Option<PathBuf>> {
    let Some(crash_dir) = paths::crash_dir() else {
        return Ok(None);
    };
    take_unreported_crash_from(&crash_dir)
}

fn take_unreported_crash_from(crash_dir: &Path) -> io::Result<Option<PathBuf>> {
    let Some(latest) = crash_reports(crash_dir)?.pop() else {
        return Ok(None);
    };
    let filename = latest
        .file_name()
        .ok_or_else(|| io::Error::other("crash report has no file name"))?;
    let marker = crash_dir.join(LAST_SEEN_FILE);
    if fs::read(&marker).ok().as_deref() == Some(filename.as_encoded_bytes()) {
        return Ok(None);
    }
    fs::write(marker, filename.as_encoded_bytes())?;
    Ok(Some(latest))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    #[test]
    fn crash_report_format_contains_required_evidence() {
        let timestamp = Utc.with_ymd_and_hms(2026, 7, 17, 12, 34, 56).unwrap();
        let report = format_crash_report(timestamp, "boom", "main.rs:9:4", "frame one");
        assert!(report.contains("timestamp: 2026-07-17T12:34:56"));
        assert!(report.contains("payload: boom"));
        assert!(report.contains("location: main.rs:9:4"));
        assert!(report.contains("backtrace:\nframe one"));
    }

    #[test]
    fn crash_report_retention_keeps_newest_twenty() {
        let dir = tempfile::tempdir().unwrap();
        let start = Utc.with_ymd_and_hms(2026, 7, 17, 0, 0, 0).unwrap();
        for offset in 0..(MAX_CRASH_REPORTS + 3) {
            let timestamp = start + Duration::seconds(offset as i64);
            write_crash_report(dir.path(), timestamp, "backtrace: test").unwrap();
        }
        let reports = crash_reports(dir.path()).unwrap();
        assert_eq!(reports.len(), MAX_CRASH_REPORTS);
        assert!(reports[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("00:00:03"));
    }

    #[test]
    fn newest_crash_is_reported_only_once() {
        let dir = tempfile::tempdir().unwrap();
        let first = Utc.with_ymd_and_hms(2026, 7, 17, 1, 0, 0).unwrap();
        let first_path = write_crash_report(dir.path(), first, "first").unwrap();
        assert_eq!(
            take_unreported_crash_from(dir.path()).unwrap(),
            Some(first_path)
        );
        assert_eq!(take_unreported_crash_from(dir.path()).unwrap(), None);

        let second_path =
            write_crash_report(dir.path(), first + Duration::seconds(1), "second").unwrap();
        assert_eq!(
            take_unreported_crash_from(dir.path()).unwrap(),
            Some(second_path)
        );
    }
}
