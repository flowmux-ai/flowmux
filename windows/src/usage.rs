// SPDX-License-Identifier: GPL-3.0-or-later

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

mod claude;
mod codex;
pub use claude::{claude_limits, claude_tokens};
pub use codex::{codex_limits, codex_tokens};

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Claude,
    Codex,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenTotals {
    pub today: Option<u64>,
    pub lifetime: Option<u64>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct UsageWindow {
    pub label: String,
    pub scope: Option<String>,
    pub used_percent: f64,
    pub duration_minutes: Option<u64>,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageErrorKind {
    NotInstalled,
    NotLoggedIn,
    Unauthorized,
    Timeout,
    Network,
    InvalidData,
    Io,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct UsageError {
    pub kind: UsageErrorKind,
    pub message: String,
}

impl UsageError {
    pub fn new(kind: UsageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn network() -> Self {
        Self::new(
            UsageErrorKind::Network,
            "Could not connect to the usage service.",
        )
    }

    pub fn unauthorized() -> Self {
        Self::new(
            UsageErrorKind::Unauthorized,
            "Run Claude once to refresh the local login.",
        )
    }
}

#[derive(Serialize, Clone, Debug)]
pub enum FieldRefresh<T> {
    Success(T),
    Failure(UsageError),
}

#[derive(Serialize, Clone, Debug)]
pub struct ProviderRefresh {
    pub provider: Provider,
    pub tokens: FieldRefresh<TokenTotals>,
    pub limits: FieldRefresh<Vec<UsageWindow>>,
    pub collected_at: DateTime<Utc>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Timestamped<T> {
    pub value: T,
    pub updated_at: DateTime<Utc>,
}

#[derive(Serialize, Clone, Debug)]
pub struct ProviderState {
    pub provider: Provider,
    pub tokens: Option<Timestamped<TokenTotals>>,
    pub limits: Option<Timestamped<Vec<UsageWindow>>>,
    pub token_error: Option<UsageError>,
    pub limits_error: Option<UsageError>,
}

impl ProviderState {
    pub fn new(provider: Provider) -> Self {
        Self {
            provider,
            tokens: None,
            limits: None,
            token_error: None,
            limits_error: None,
        }
    }

    pub fn apply(&mut self, update: ProviderRefresh) {
        match update.tokens {
            FieldRefresh::Success(value) => {
                self.tokens = Some(Timestamped {
                    value,
                    updated_at: update.collected_at,
                });
                self.token_error = None;
            }
            FieldRefresh::Failure(error) => self.token_error = Some(error),
        }
        match update.limits {
            FieldRefresh::Success(value) => {
                self.limits = Some(Timestamped {
                    value,
                    updated_at: update.collected_at,
                });
                self.limits_error = None;
            }
            FieldRefresh::Failure(error) => {
                self.limits_error = Some(error);
                if let Some(limits) = &mut self.limits {
                    for window in &mut limits.value {
                        if window
                            .resets_at
                            .is_some_and(|reset| reset <= update.collected_at)
                        {
                            window.used_percent = 0.0;
                            window.resets_at = None;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct UsagePanelState {
    pub claude: ProviderState,
    pub codex: ProviderState,
    pub refreshing: bool,
    pub last_finished_at: Option<DateTime<Utc>>,
}

impl Default for UsagePanelState {
    fn default() -> Self {
        Self {
            claude: ProviderState::new(Provider::Claude),
            codex: ProviderState::new(Provider::Codex),
            refreshing: false,
            last_finished_at: None,
        }
    }
}

impl UsagePanelState {
    pub fn apply(&mut self, update: ProviderRefresh) {
        match update.provider {
            Provider::Claude => self.claude.apply(update),
            Provider::Codex => self.codex.apply(update),
        }
    }

    pub fn begin_refresh(&mut self, now: DateTime<Utc>) -> bool {
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

    pub fn finish_refresh(&mut self, now: DateTime<Utc>) {
        self.refreshing = false;
        self.last_finished_at = Some(now);
    }

    pub fn begin_forced_refresh(&mut self) -> bool {
        if self.refreshing {
            return false;
        }
        self.refreshing = true;
        true
    }
}

pub fn duration_label(minutes: Option<u64>) -> String {
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

pub fn format_token_count(value: u64) -> String {
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
    fn failed_refresh_expires_usage_after_its_reset() {
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
        assert_eq!(limits[0].used_percent, 0.0);
        assert_eq!(limits[0].resets_at, None);
        assert_eq!(limits[1].used_percent, 42.0);
        assert!(limits[1].resets_at.is_some());
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
}
