// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded Windows browser wait grammar and predicate construction.
use anyhow::ensure;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
pub struct Options {
    /// CSS selector; existence is sufficient (visibility is not required).
    #[arg(long)]
    pub selector: Option<String>,
    /// Exact Unicode substring of document.body.innerText.
    #[arg(long)]
    pub text: Option<String>,
    /// Substring of location.href, which may contain percent encoding.
    #[arg(long)]
    pub url: Option<String>,
    #[arg(long)]
    pub ready_state: Option<String>,
    /// Synchronous expression, function, or function body; evaluated repeatedly.
    #[arg(long)]
    pub js: Option<String>,
    #[arg(long, default_value_t = default_timeout())]
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[arg(long, default_value_t = default_poll())]
    #[serde(default = "default_poll")]
    pub poll_ms: u64,
}
fn default_timeout() -> u64 {
    5000
}
fn default_poll() -> u64 {
    100
}
impl Options {
    fn condition(&self) -> anyhow::Result<(&'static str, &str)> {
        ensure!(
            (1..=120_000).contains(&self.timeout_ms),
            "--timeout-ms must be 1..120000"
        );
        ensure!(
            (1..=10_000).contains(&self.poll_ms),
            "--poll-ms must be 1..10000"
        );
        let mut choices = [
            ("selector", self.selector.as_deref()),
            ("text", self.text.as_deref()),
            ("url", self.url.as_deref()),
            ("ready_state", self.ready_state.as_deref()),
            ("js", self.js.as_deref()),
        ]
        .into_iter()
        .filter_map(|(kind, value)| value.map(|value| (kind, value)));
        let chosen = choices
            .next()
            .ok_or_else(|| anyhow::anyhow!("exactly one wait condition is required"))?;
        ensure!(
            choices.next().is_none(),
            "exactly one wait condition is required"
        );
        let (kind, value) = chosen;
        ensure!(
            !value.is_empty() && value.len() <= 64 * 1024,
            "empty or oversized wait condition"
        );
        if kind == "selector" {
            crate::browser_dom::selector(value)?;
        }
        if kind == "ready_state" {
            ensure!(
                matches!(value, "loading" | "interactive" | "complete"),
                "invalid ready-state"
            );
        }
        Ok(chosen)
    }
    pub fn script(&self) -> anyhow::Result<String> {
        let (kind, value) = self.condition()?;
        let args = serde_json::json!({"kind":kind,"value":value});
        let script = format!("({})({args})", include_str!("../browser/wait.js"));
        ensure!(
            script.len() <= 128 * 1024,
            "encoded wait condition exceeds 128 KiB"
        );
        Ok(script)
    }
    pub fn during_load(&self) -> bool {
        matches!(self.ready_state.as_deref(), Some("loading" | "interactive"))
    }
    pub fn ipc_budget(&self, default: Duration, padding: u64) -> anyhow::Result<Duration> {
        self.script()?; // Raw IPC requests receive the same validation as CLI calls.
        Ok(default.max(Duration::from_millis(self.timeout_ms) + Duration::from_secs(padding)))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn options(value: serde_json::Value) -> Options {
        serde_json::from_value(value).unwrap()
    }
    #[test]
    fn wait_validation_applies_to_raw_requests_and_bounds_transport_budget() {
        for value in [
            serde_json::json!({}),
            serde_json::json!({"text":"a","url":"b"}),
            serde_json::json!({"text":""}),
            serde_json::json!({"ready_state":"done"}),
            serde_json::json!({"js":"true","timeout_ms":0}),
            serde_json::json!({"js":"true","timeout_ms":120001}),
            serde_json::json!({"js":"true","poll_ms":0}),
            serde_json::json!({"js":"true","poll_ms":10001}),
        ] {
            assert!(options(value)
                .ipc_budget(Duration::from_secs(15), 5)
                .is_err());
        }
        let value = options(serde_json::json!({"text":"한글 한 é 😀","timeout_ms":120000}));
        assert_eq!(
            value.ipc_budget(Duration::from_secs(15), 5).unwrap(),
            Duration::from_secs(125)
        );
        assert!(!value.during_load());
        assert!(options(serde_json::json!({"ready_state":"interactive"})).during_load());
    }
    #[test]
    fn wait_parameters_remain_data_and_encoded_script_has_a_bound() {
        let source = "한글 한 é 😀 \\\";window.injected=true; //";
        let value = options(serde_json::json!({"text":source}));
        let script = value.script().unwrap();
        assert!(script.contains(&serde_json::to_string(source).unwrap()));
        assert!(options(serde_json::json!({"text":"\0".repeat(64000)}))
            .script()
            .is_err());
        assert_eq!(value.timeout_ms, 5000);
        assert_eq!(value.poll_ms, 100);
    }
}
