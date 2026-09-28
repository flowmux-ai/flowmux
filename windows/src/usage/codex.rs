// SPDX-License-Identifier: GPL-3.0-or-later

use super::{duration_label, TokenTotals, UsageError, UsageErrorKind, UsageWindow};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use std::collections::HashSet;

pub fn codex_limits(value: &Value) -> Result<Vec<UsageWindow>, UsageError> {
    let result = response_result(value)?;
    let mut windows = Vec::new();
    let root_limit_id = result
        .get("rateLimits")
        .and_then(|snapshot| snapshot.get("limitId"))
        .and_then(Value::as_str);
    if let Some(snapshot) = result.get("rateLimits") {
        append_snapshot_windows(snapshot, None, &mut windows)?;
    }
    if let Some(by_id) = result.get("rateLimitsByLimitId").and_then(Value::as_object) {
        for (limit_id, snapshot) in by_id {
            let limit_id = snapshot
                .get("limitId")
                .and_then(Value::as_str)
                .unwrap_or(limit_id);
            if Some(limit_id) == root_limit_id {
                continue;
            }
            let scope = snapshot
                .get("limitName")
                .and_then(Value::as_str)
                .unwrap_or(limit_id)
                .to_owned();
            append_snapshot_windows(snapshot, Some(scope), &mut windows)?;
        }
    }
    let mut seen = HashSet::new();
    windows.retain(|window| {
        seen.insert((
            window.duration_minutes,
            window.resets_at,
            window.used_percent.to_bits(),
            window.scope.clone(),
        ))
    });
    Ok(windows)
}

fn append_snapshot_windows(
    snapshot: &Value,
    fallback_scope: Option<String>,
    windows: &mut Vec<UsageWindow>,
) -> Result<(), UsageError> {
    let object = snapshot.as_object().ok_or_else(invalid_response)?;
    let scope = object
        .get("limitName")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(fallback_scope);
    for key in ["primary", "secondary"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let window = value.as_object().ok_or_else(invalid_response)?;
        let Some(used_percent) = window.get("usedPercent").and_then(Value::as_f64) else {
            continue;
        };
        let duration_minutes = window.get("windowDurationMins").and_then(Value::as_u64);
        let resets_at = window
            .get("resetsAt")
            .and_then(Value::as_i64)
            .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0));
        windows.push(UsageWindow {
            label: duration_label(duration_minutes),
            scope: scope.clone(),
            used_percent,
            duration_minutes,
            resets_at,
        });
    }
    // Business/enterprise plans leave primary and secondary null and report the
    // seat's own allowance as `individualLimit` instead.
    if let Some(value) = object
        .get("individualLimit")
        .filter(|value| !value.is_null())
    {
        let window = value.as_object().ok_or_else(invalid_response)?;
        if let Some(remaining_percent) = window.get("remainingPercent").and_then(Value::as_f64) {
            windows.push(UsageWindow {
                label: duration_label(None),
                scope: scope.or_else(|| Some("Individual".to_owned())),
                used_percent: 100.0 - remaining_percent,
                duration_minutes: None,
                resets_at: window
                    .get("resetsAt")
                    .and_then(Value::as_i64)
                    .and_then(|timestamp| DateTime::<Utc>::from_timestamp(timestamp, 0)),
            });
        }
    }
    Ok(())
}

pub fn codex_tokens(value: &Value, day: NaiveDate) -> Result<TokenTotals, UsageError> {
    let result = response_result(value)?;
    let lifetime = match result.get("summary") {
        None | Some(Value::Null) => None,
        Some(Value::Object(summary)) => match summary.get("lifetimeTokens") {
            None | Some(Value::Null) => None,
            Some(value) => Some(value.as_u64().ok_or_else(invalid_response)?),
        },
        Some(_) => return Err(invalid_response()),
    };
    let today = match result.get("dailyUsageBuckets") {
        None | Some(Value::Null) => None,
        Some(Value::Array(buckets)) => {
            let target = day.format("%Y-%m-%d").to_string();
            let mut today = None;
            for bucket in buckets {
                let bucket = bucket.as_object().ok_or_else(invalid_response)?;
                let start_date = bucket
                    .get("startDate")
                    .and_then(Value::as_str)
                    .ok_or_else(invalid_response)?;
                let tokens = bucket
                    .get("tokens")
                    .and_then(Value::as_u64)
                    .ok_or_else(invalid_response)?;
                if start_date == target {
                    today = Some(tokens);
                    break;
                }
            }
            today
        }
        Some(_) => return Err(invalid_response()),
    };
    Ok(TokenTotals { today, lifetime })
}

fn response_result(value: &Value) -> Result<&Value, UsageError> {
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let error = error.as_object().ok_or_else(invalid_response)?;
        let code = error
            .get("code")
            .and_then(Value::as_i64)
            .ok_or_else(invalid_response)?;
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(invalid_response)?;
        if is_authentication_error(message) {
            return Err(UsageError::new(
                UsageErrorKind::NotLoggedIn,
                "Check the local Codex login.",
            ));
        }
        if code == -32601 {
            return Err(UsageError::new(
                UsageErrorKind::InvalidData,
                "The installed Codex CLI does not support usage reporting. Update Codex and retry.",
            ));
        }
        return Err(UsageError::new(
            UsageErrorKind::InvalidData,
            "The Codex usage request failed.",
        ));
    }
    value.get("result").ok_or_else(invalid_response)
}

fn is_authentication_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "not authenticated",
        "authentication required",
        "login required",
        "not logged in",
        "unauthorized",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn invalid_response() -> UsageError {
    UsageError::new(
        UsageErrorKind::InvalidData,
        "The Codex usage response was invalid.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rate_limit_parser_uses_duration_and_keeps_scoped_windows() {
        let value = serde_json::json!({"result": {
            "rateLimits": {
                "primary": {
                    "usedPercent": 8,
                    "windowDurationMins": 10080,
                    "resetsAt": 1784505600
                },
                "secondary": {
                    "usedPercent": 20,
                    "windowDurationMins": 300,
                    "resetsAt": 1784000000
                }
            },
            "rateLimitsByLimitId": {
                "fable": {
                    "limitName": "Fable",
                    "primary": {
                        "usedPercent": 55,
                        "windowDurationMins": 10080,
                        "resetsAt": 1784505600
                    }
                }
            }
        }});

        let windows = codex_limits(&value).unwrap();

        assert_eq!(windows[0].label, "Weekly");
        assert_eq!(windows[1].label, "5 hours");
        assert!(windows
            .iter()
            .any(|window| window.scope.as_deref() == Some("Fable")));
    }

    #[test]
    fn rate_limit_parser_deduplicates_mirrored_bucket() {
        let value = serde_json::json!({"result": {
            "rateLimits": {
                "limitName": "Codex",
                "primary": {"usedPercent": 8, "windowDurationMins": 300, "resetsAt": 1784000000}
            },
            "rateLimitsByLimitId": {
                "codex": {
                    "limitName": "Codex",
                    "primary": {"usedPercent": 8, "windowDurationMins": 300, "resetsAt": 1784000000}
                }
            }
        }});

        assert_eq!(codex_limits(&value).unwrap().len(), 1);
    }

    #[test]
    fn rate_limit_parser_deduplicates_null_named_root_by_limit_id() {
        let value = serde_json::json!({"result": {
            "rateLimits": {
                "limitId": "codex",
                "limitName": null,
                "primary": {"usedPercent": 33, "windowDurationMins": 10080, "resetsAt": 1784971874}
            },
            "rateLimitsByLimitId": {
                "codex": {
                    "limitId": "codex",
                    "limitName": null,
                    "primary": {"usedPercent": 33, "windowDurationMins": 10080, "resetsAt": 1784971874}
                }
            }
        }});

        let windows = codex_limits(&value).unwrap();

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 33.0);
        assert_eq!(windows[0].scope, None);
    }

    #[test]
    fn token_usage_parser_selects_local_date_bucket() {
        let value = serde_json::json!({"result": {
            "summary": {"lifetimeTokens": 123456},
            "dailyUsageBuckets": [
                {"startDate": "2026-07-13", "tokens": 10},
                {"startDate": "2026-07-14", "tokens": 789}
            ]
        }});

        assert_eq!(
            codex_tokens(&value, NaiveDate::from_ymd_opt(2026, 7, 14).unwrap()).unwrap(),
            TokenTotals {
                today: Some(789),
                lifetime: Some(123456)
            }
        );
    }

    #[test]
    fn token_usage_parser_rejects_malformed_today_bucket() {
        let value = serde_json::json!({"result": {
            "summary": {"lifetimeTokens": 123456},
            "dailyUsageBuckets": [
                {"startDate": "2026-07-14", "tokens": "unknown"}
            ]
        }});

        let error =
            codex_tokens(&value, NaiveDate::from_ymd_opt(2026, 7, 14).unwrap()).unwrap_err();

        assert_eq!(error.kind, UsageErrorKind::InvalidData);
    }

    #[test]
    fn token_usage_parser_does_not_invent_zero_for_missing_day() {
        let value = serde_json::json!({"result": {
            "summary": {"lifetimeTokens": 123456},
            "dailyUsageBuckets": [
                {"startDate": "2026-07-19", "tokens": 789}
            ]
        }});

        assert_eq!(
            codex_tokens(&value, NaiveDate::from_ymd_opt(2026, 7, 20).unwrap()).unwrap(),
            TokenTotals {
                today: None,
                lifetime: Some(123456)
            }
        );
    }

    #[test]
    fn unsupported_rpc_method_requests_a_codex_update() {
        let value = serde_json::json!({
            "id": 1,
            "error": {"code": -32601, "message": "Method not found"}
        });

        let error = response_result(&value).unwrap_err();

        assert_eq!(error.kind, UsageErrorKind::InvalidData);
        assert_eq!(
            error.message,
            "The installed Codex CLI does not support usage reporting. Update Codex and retry."
        );
    }

    #[test]
    fn server_limitation_does_not_request_a_codex_update() {
        let value = serde_json::json!({
            "id": 1,
            "error": {"code": -32000, "message": "Usage reporting is not implemented for this account"}
        });

        let error = response_result(&value).unwrap_err();

        assert_eq!(error.kind, UsageErrorKind::InvalidData);
        assert_eq!(error.message, "The Codex usage request failed.");
    }

    #[test]
    fn auth_rpc_error_is_reported_as_missing_local_login() {
        let value = serde_json::json!({
            "id": 1,
            "error": {"code": -32600, "message": "Not authenticated"}
        });

        let error = response_result(&value).unwrap_err();

        assert_eq!(error.kind, UsageErrorKind::NotLoggedIn);
        assert_eq!(error.message, "Check the local Codex login.");
    }

    #[test]
    fn business_plan_reports_the_individual_seat_limit() {
        let value = serde_json::json!({"result": {
            "rateLimits": {
                "limitId": "codex",
                "limitName": null,
                "primary": null,
                "secondary": null,
                "individualLimit": {
                    "limit": "300",
                    "used": "120.0",
                    "remainingPercent": 40,
                    "resetsAt": 1788220801
                },
                "planType": "business"
            },
            "rateLimitsByLimitId": {
                "codex": {
                    "limitId": "codex",
                    "primary": null,
                    "secondary": null,
                    "individualLimit": {"remainingPercent": 40, "resetsAt": 1788220801}
                }
            }
        }});

        let windows = codex_limits(&value).unwrap();

        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].used_percent, 60.0);
        assert_eq!(windows[0].scope.as_deref(), Some("Individual"));
        assert_eq!(windows[0].duration_minutes, None);
        assert_eq!(
            windows[0].resets_at,
            DateTime::<Utc>::from_timestamp(1788220801, 0)
        );
    }

    #[test]
    fn named_windows_still_win_over_the_individual_seat_limit_scope() {
        let value = serde_json::json!({"result": {"rateLimits": {
            "limitName": "Team pool",
            "primary": {"usedPercent": 10.0, "windowDurationMins": 300},
            "individualLimit": {"remainingPercent": 25}
        }}});

        let windows = codex_limits(&value).unwrap();

        assert_eq!(windows.len(), 2);
        assert_eq!(windows[1].used_percent, 75.0);
        assert_eq!(windows[1].scope.as_deref(), Some("Team pool"));
    }
}
