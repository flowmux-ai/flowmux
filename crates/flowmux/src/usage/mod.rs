// SPDX-License-Identifier: GPL-3.0-or-later

use chrono::{DateTime, Duration, Utc};
use std::future::Future;
use std::io::{self, BufRead, Read};
use std::path::PathBuf;

mod antigravity;
mod claude;
mod codex;
mod gemini;
pub(crate) mod team;

pub(crate) use claude::canonical_source_id as canonical_claude_source_id;
pub(crate) use {antigravity::collect as collect_antigravity, gemini::collect as collect_gemini};

pub(crate) async fn collect_all(home: PathBuf) -> [ProviderRefresh; 2] {
    join_provider_refreshes(
        claude::collect(home.clone(), reqwest::Client::new()),
        codex::collect(home),
    )
    .await
}

async fn join_provider_refreshes<C, D>(claude: C, codex: D) -> [ProviderRefresh; 2]
where
    C: Future<Output = ProviderRefresh>,
    D: Future<Output = ProviderRefresh>,
{
    let (claude, codex) = tokio::join!(claude, codex);
    [claude, codex]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Provider {
    Claude,
    Codex,
    Antigravity,
    Gemini,
}

impl Provider {
    pub(crate) const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Antigravity, Self::Gemini];

    pub(crate) fn index(self) -> usize {
        self as usize
    }

    pub(crate) fn icon(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
            Self::Gemini => "gemini",
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Antigravity => "Antigravity",
            Self::Gemini => "Gemini CLI",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct TokenTotals {
    pub(crate) today: Option<u64>,
    pub(crate) lifetime: Option<u64>,
    pub(crate) by_model: Vec<(String, u64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UsageWindow {
    pub(crate) source_id: Option<String>,
    pub(crate) label: String,
    pub(crate) scope: Option<String>,
    pub(crate) used_percent: f64,
    pub(crate) duration_minutes: Option<u64>,
    pub(crate) resets_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum UsageErrorKind {
    NotInstalled,
    NotLoggedIn,
    Unauthorized,
    Timeout,
    Network,
    InvalidData,
    Io,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageError {
    pub(crate) kind: UsageErrorKind,
    pub(crate) message: String,
}

impl UsageError {
    pub(crate) fn new(kind: UsageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn network() -> Self {
        Self::new(
            UsageErrorKind::Network,
            "Could not connect to the usage service.",
        )
    }

    pub(crate) fn unauthorized() -> Self {
        Self::new(
            UsageErrorKind::Unauthorized,
            "Run Claude once to refresh the local login.",
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) enum FieldRefresh<T> {
    Success(T),
    Failure(UsageError),
    Skipped,
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderRefresh {
    pub(crate) provider: Provider,
    pub(crate) tokens: FieldRefresh<TokenTotals>,
    pub(crate) limits: FieldRefresh<Vec<UsageWindow>>,
    pub(crate) collected_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub(crate) struct Timestamped<T> {
    pub(crate) value: T,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug)]
pub(crate) struct ProviderState {
    pub(crate) provider: Provider,
    pub(crate) tokens: Option<Timestamped<TokenTotals>>,
    pub(crate) limits: Option<Timestamped<Vec<UsageWindow>>>,
    pub(crate) token_error: Option<UsageError>,
    pub(crate) limits_error: Option<UsageError>,
}

impl ProviderState {
    fn new(provider: Provider) -> Self {
        Self {
            provider,
            tokens: None,
            limits: None,
            token_error: None,
            limits_error: None,
        }
    }

    fn apply(&mut self, update: ProviderRefresh) {
        match update.tokens {
            FieldRefresh::Success(value) => {
                self.tokens = Some(Timestamped {
                    value,
                    updated_at: update.collected_at,
                });
                self.token_error = None;
            }
            FieldRefresh::Failure(error) => self.token_error = Some(error),
            FieldRefresh::Skipped => {}
        }
        match update.limits {
            FieldRefresh::Success(value) => {
                self.limits = Some(Timestamped {
                    value,
                    updated_at: update.collected_at,
                });
                self.limits_error = None;
            }
            FieldRefresh::Failure(error) => self.limits_error = Some(error),
            FieldRefresh::Skipped => {}
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct UsagePanelState {
    pub(crate) claude: ProviderState,
    pub(crate) codex: ProviderState,
    antigravity: ProviderState,
    gemini: ProviderState,
    pub(crate) refreshing: bool,
    last_finished_at: Option<DateTime<Utc>>,
}

impl Default for UsagePanelState {
    fn default() -> Self {
        Self {
            claude: ProviderState::new(Provider::Claude),
            codex: ProviderState::new(Provider::Codex),
            antigravity: ProviderState::new(Provider::Antigravity),
            gemini: ProviderState::new(Provider::Gemini),
            refreshing: false,
            last_finished_at: None,
        }
    }
}

impl UsagePanelState {
    pub(crate) fn provider(&self, provider: Provider) -> &ProviderState {
        match provider {
            Provider::Claude => &self.claude,
            Provider::Codex => &self.codex,
            Provider::Antigravity => &self.antigravity,
            Provider::Gemini => &self.gemini,
        }
    }

    pub(crate) fn apply(&mut self, update: ProviderRefresh) {
        match update.provider {
            Provider::Claude => self.claude.apply(update),
            Provider::Codex => self.codex.apply(update),
            Provider::Antigravity => self.antigravity.apply(update),
            Provider::Gemini => self.gemini.apply(update),
        }
    }

    pub(crate) fn begin_refresh(&mut self, now: DateTime<Utc>) -> bool {
        if self.refreshing
            || self
                .last_finished_at
                .is_some_and(|last| now - last < Duration::seconds(60))
        {
            return false;
        }
        self.refreshing = true;
        true
    }

    pub(crate) fn finish_refresh(&mut self, now: DateTime<Utc>) {
        self.refreshing = false;
        self.last_finished_at = Some(now);
    }

    pub(crate) fn begin_forced_refresh(&mut self) -> bool {
        if self.refreshing {
            return false;
        }
        self.refreshing = true;
        true
    }
}

pub(crate) fn duration_label(minutes: Option<u64>) -> String {
    match minutes {
        Some(10_080) => "Weekly".into(),
        Some(1_440) => "1 day".into(),
        Some(60) => "1 hour".into(),
        Some(1) => "1 minute".into(),
        Some(0) => "0 minutes".into(),
        Some(value) if value % 1_440 == 0 => format!("{} days", value / 1_440),
        Some(value) if value % 60 == 0 => format!("{} hours", value / 60),
        Some(value) => format!("{value} minutes"),
        None => "Usage limit".into(),
    }
}

pub(crate) fn reset_countdown(resets_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (resets_at - now).num_seconds();
    match seconds {
        ..=0 => "Reset due".into(),
        1..60 => "<1m".into(),
        60..3600 => format!("{}m", seconds / 60),
        3600..86400 => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
        _ => format!("{}d {}h", seconds / 86400, seconds % 86400 / 3600),
    }
}

/// Usage services send some numbers as JSON strings (`"120.0"`); accept both.
pub(crate) fn json_number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
        .filter(|value| value.is_finite())
}

pub(crate) fn reset_timestamp(value: &serde_json::Value) -> Option<DateTime<Utc>> {
    if let Some(epoch) = value.as_i64().or_else(|| value.as_str()?.parse().ok()) {
        return if epoch >= 10_000_000_000 {
            DateTime::from_timestamp_millis(epoch)
        } else {
            DateTime::from_timestamp(epoch, 0)
        };
    }
    DateTime::parse_from_rfc3339(value.as_str()?)
        .ok()
        .map(Into::into)
}

fn read_transcript_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> io::Result<bool> {
    const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
    line.clear();
    let read = reader
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', line)?;
    if read > MAX_LINE_BYTES {
        return Err(io::ErrorKind::FileTooLarge.into());
    }
    Ok(read != 0)
}

fn native_program(home: &std::path::Path, name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .filter(|path| path.is_absolute())
        .chain([
            home.join(".local/bin"),
            "/opt/homebrew/bin".into(),
            "/usr/local/bin".into(),
        ])
        .map(|dir| dir.join(name))
        .find(|path| codex::is_executable(path) && !codex::is_flowmux_agent_wrapper(path))
}

fn cli_version(output: &str) -> Option<(u64, u64, u64)> {
    let version = output.trim().split_ascii_whitespace().last()?;
    let mut parts = version.trim_start_matches('v').split(['.', '-', '+']);
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

pub(crate) fn format_token_count(value: u64) -> String {
    if value >= 1_000_000 {
        format_compact(value, 1_000_000, "M")
    } else if value >= 1_000 {
        format_compact(value, 1_000, "K")
    } else {
        value.to_string()
    }
}

fn format_compact(value: u64, scale: u64, suffix: &str) -> String {
    let tenths = value.saturating_mul(10).saturating_add(scale / 2) / scale;
    format!("{}.{:01}{suffix}", tenths / 10, tenths % 10)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn window(duration_minutes: u64, used_percent: f64) -> UsageWindow {
        UsageWindow {
            source_id: None,
            label: duration_label(Some(duration_minutes)),
            scope: None,
            used_percent,
            duration_minutes: Some(duration_minutes),
            resets_at: None,
        }
    }

    #[test]
    fn failed_field_refresh_preserves_last_success() {
        let now = Utc::now();
        let mut state = UsagePanelState::default();
        state.apply(ProviderRefresh {
            provider: Provider::Claude,
            tokens: FieldRefresh::Success(TokenTotals {
                today: Some(123),
                lifetime: None,
                by_model: Vec::new(),
            }),
            limits: FieldRefresh::Success(vec![window(300, 25.0)]),
            collected_at: now,
        });
        state.apply(ProviderRefresh {
            provider: Provider::Claude,
            tokens: FieldRefresh::Failure(UsageError::network()),
            limits: FieldRefresh::Failure(UsageError::unauthorized()),
            collected_at: now + Duration::minutes(1),
        });

        assert_eq!(state.claude.tokens.as_ref().unwrap().value.today, Some(123));
        assert_eq!(
            state.claude.limits.as_ref().unwrap().value[0].used_percent,
            25.0
        );
        assert!(state.claude.token_error.is_some());
        assert!(state.claude.limits_error.is_some());
    }

    #[test]
    fn failed_refresh_preserves_the_last_observed_reset_and_usage() {
        let now = Utc::now();
        let mut expired = window(300, 100.0);
        expired.resets_at = Some(now + Duration::minutes(1));
        let mut current = window(10_080, 42.0);
        current.resets_at = Some(now + Duration::days(1));
        let mut state = UsagePanelState::default();
        state.apply(ProviderRefresh {
            provider: Provider::Claude,
            tokens: FieldRefresh::Success(TokenTotals::default()),
            limits: FieldRefresh::Success(vec![expired, current]),
            collected_at: now,
        });

        state.apply(ProviderRefresh {
            provider: Provider::Claude,
            tokens: FieldRefresh::Success(TokenTotals::default()),
            limits: FieldRefresh::Failure(UsageError::network()),
            collected_at: now + Duration::minutes(2),
        });

        let limits = &state.claude.limits.as_ref().unwrap().value;
        assert_eq!(limits[0].used_percent, 100.0);
        assert_eq!(limits[0].resets_at, Some(now + Duration::minutes(1)));
        assert_eq!(limits[1].used_percent, 42.0);
        assert!(limits[1].resets_at.is_some());
    }

    #[test]
    fn reset_countdown_covers_minute_hour_day_and_elapsed_boundaries() {
        let now = Utc::now();
        for (minutes, expected) in [
            (0, "Reset due"),
            (24, "24m"),
            (564, "9h 24m"),
            (1500, "1d 1h"),
        ] {
            assert_eq!(
                reset_countdown(now + Duration::minutes(minutes), now),
                expected
            );
        }
    }

    #[test]
    fn duration_labels_are_metadata_driven() {
        assert_eq!(duration_label(Some(300)), "5 hours");
        assert_eq!(duration_label(Some(10_080)), "Weekly");
        assert_eq!(duration_label(Some(4_320)), "3 days");
        assert_eq!(duration_label(Some(90)), "90 minutes");
        assert_eq!(duration_label(Some(1_440)), "1 day");
        assert_eq!(duration_label(Some(60)), "1 hour");
        assert_eq!(duration_label(Some(1)), "1 minute");
        assert_eq!(duration_label(Some(0)), "0 minutes");
        assert_eq!(duration_label(None), "Usage limit");
    }

    #[test]
    fn token_counts_use_compact_readable_units() {
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(1_250), "1.3K");
        assert_eq!(format_token_count(2_500_000), "2.5M");
    }

    #[test]
    fn refreshes_are_coalesced_and_cached_for_sixty_seconds() {
        let now = Utc::now();
        let mut state = UsagePanelState::default();
        assert!(state.begin_refresh(now));
        assert!(!state.begin_refresh(now + Duration::seconds(1)));
        state.finish_refresh(now + Duration::seconds(2));
        assert!(!state.begin_refresh(now + Duration::seconds(59)));
        assert!(state.begin_refresh(now + Duration::seconds(63)));
    }

    #[test]
    fn forced_refresh_bypasses_cache_but_not_an_active_refresh() {
        let now = Utc::now();
        let mut state = UsagePanelState::default();
        assert!(state.begin_refresh(now));
        state.finish_refresh(now + Duration::seconds(1));
        assert!(!state.begin_refresh(now + Duration::seconds(2)));
        assert!(state.begin_forced_refresh());
        assert!(!state.begin_forced_refresh());
    }

    #[tokio::test]
    async fn concurrent_refresh_results_keep_provider_order() {
        fn refresh(provider: Provider) -> ProviderRefresh {
            ProviderRefresh {
                provider,
                tokens: FieldRefresh::Success(TokenTotals::default()),
                limits: FieldRefresh::Success(Vec::new()),
                collected_at: Utc::now(),
            }
        }

        let results = join_provider_refreshes(async { refresh(Provider::Claude) }, async {
            refresh(Provider::Codex)
        })
        .await;

        assert_eq!(results[0].provider, Provider::Claude);
        assert_eq!(results[1].provider, Provider::Codex);
    }
}
