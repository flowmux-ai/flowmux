// SPDX-License-Identifier: GPL-3.0-or-later
//! Reuse pure Rust notification contracts without linking the GTK/D-Bus crates.
#[path = "../../crates/flowmux-notify/src/osc.rs"]
pub mod osc;
#[path = "../../crates/flowmux/src/notifications.rs"]
pub mod store;
#[path = "../../crates/flowmux-notify/src/stream.rs"]
pub mod stream;

use flowmux_core::NotificationLevel;
use std::{cell::RefCell, rc::Rc};

/// Shell balloon fields are fixed UTF-16 arrays. Keep the full text in the bell
/// store and truncate only this display copy, never splitting a surrogate pair.
pub fn desktop_text<const N: usize>(text: &str) -> [u16; N] {
    let mut result = [0; N];
    let mut length = 0;
    for ch in text.chars() {
        let mut units = [0; 2];
        let units = ch.encode_utf16(&mut units);
        if length + units.len() >= N {
            break;
        }
        result[length..length + units.len()].copy_from_slice(units);
        length += units.len();
    }
    result
}

pub fn validate(title: &str, body: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        title.len() <= 1024 && body.len() <= 8192,
        "notification title/body exceed 1/8 KiB UTF-8 limits"
    );
    anyhow::ensure!(
        !title.contains('\0') && !body.contains('\0'),
        "notification fields cannot contain NUL"
    );
    anyhow::ensure!(
        !title.trim().is_empty() || !body.trim().is_empty(),
        "notification cannot be empty"
    );
    Ok(())
}
pub fn level(value: &str) -> anyhow::Result<NotificationLevel> {
    Ok(match value {
        "info" => NotificationLevel::Info,
        "attention" => NotificationLevel::NeedsInput,
        "error" => NotificationLevel::Error,
        "completed" => NotificationLevel::TurnCompleted,
        _ => anyhow::bail!("level must be info, attention, error or completed"),
    })
}
pub fn suppress(level: NotificationLevel, source_focused: bool) -> bool {
    source_focused
        && matches!(
            level,
            NotificationLevel::Info | NotificationLevel::TurnCompleted
        )
}

pub fn agent_notice(
    agent: &str,
    suffix: &str,
    body: &str,
    level: NotificationLevel,
) -> osc::OscNotification {
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = body.chars();
    let mut shortened: String = chars.by_ref().take(160).collect();
    if chars.next().is_some() {
        shortened.push('…');
    }
    osc::OscNotification {
        title: format!("{agent} {suffix}"),
        body: shortened,
        level,
    }
}

pub fn hook_notice(
    agent: &str,
    event: crate::command::SessionHookEvent,
    input: &crate::agent_activity::Input,
) -> Option<osc::OscNotification> {
    use crate::command::SessionHookEvent as Event;
    if event == Event::Notification
        && agent == "Claude"
        && !matches!(
            input.notification_type.as_deref(),
            None | Some(
                "permission_prompt"
                    | "elicitation_dialog"
                    | "elicitation_url_dialog"
                    | "agent_needs_input"
                    | "quota_auto_resume_stale"
                    | "quota_auto_resume_fired"
                    | "quota_auto_resume_disabled"
                    | "agent_completed"
            )
        )
    {
        return None;
    }
    let (suffix, body, level) = match event {
        Event::StopFailure => (
            "stopped",
            input
                .last_assistant_message
                .clone()
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    input
                        .error
                        .as_ref()
                        .map(|error| format!("API error: {error}"))
                })
                .unwrap_or_else(|| "API request failed".into()),
            NotificationLevel::Error,
        ),
        Event::Notification
            if agent == "Claude"
                && matches!(
                    input.notification_type.as_deref(),
                    Some(
                        "quota_auto_resume_fired"
                            | "quota_auto_resume_disabled"
                            | "agent_completed"
                    )
                ) =>
        {
            let suffix = match input.notification_type.as_deref() {
                Some("quota_auto_resume_fired") => "resumed",
                Some("quota_auto_resume_disabled") => "auto-resume stopped",
                _ => "background agent finished",
            };
            (
                suffix,
                input
                    .message
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "status changed".into()),
                NotificationLevel::Info,
            )
        }
        Event::PermissionRequest | Event::Notification => (
            "needs your input",
            input
                .message
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "needs your attention".into()),
            NotificationLevel::NeedsInput,
        ),
        Event::ToolStart
            if input.permission_mode.as_deref() == Some("bypassPermissions")
                && input.tool_name.as_deref().is_some_and(|name| {
                    name.eq_ignore_ascii_case("AskUserQuestion")
                        || name.eq_ignore_ascii_case("ExitPlanMode")
                }) =>
        {
            (
                "needs your input",
                "needs your attention".into(),
                NotificationLevel::NeedsInput,
            )
        }
        _ => return None,
    };
    Some(agent_notice(agent, suffix, &body, level))
}

fn priority(level: NotificationLevel) -> u8 {
    match level {
        NotificationLevel::Info | NotificationLevel::TurnCompleted => 0,
        NotificationLevel::NeedsInput => 1,
        NotificationLevel::Error => 2,
    }
}

/// Per-surface streaming parser. The callback cannot enqueue unbounded UI work.
/// Only received ConPTY bytes enter this path, never restored terminal history.
type OscCallback = Box<dyn FnMut(&str)>;

pub struct Sniffer {
    extractor: stream::OscExtractor<OscCallback>,
    pending: Rc<RefCell<Vec<osc::OscNotification>>>,
}
impl Default for Sniffer {
    fn default() -> Self {
        let pending = Rc::new(RefCell::new(Vec::new()));
        let queue = pending.clone();
        let extractor = stream::OscExtractor::new(Box::new(move |payload: &str| {
            // OSC 9;9 (cwd), 9;4 (progress), and other numeric ConEmu subcommands
            // are metadata, not iTerm-style plain notification text.
            if payload
                .strip_prefix("9;")
                .and_then(|s| s.split_once(';'))
                .is_some_and(|(code, _)| {
                    !code.is_empty() && code.bytes().all(|b| b.is_ascii_digit())
                })
            {
                return;
            }
            let Some(note) = osc::parse_osc(payload) else {
                return;
            };
            if validate(&note.title, &note.body).is_err()
                || (note.title == "Terminal" && note.body.is_empty())
            {
                return;
            }
            let mut queue = queue.borrow_mut();
            if queue.len() == 16 {
                // A burst of low-priority output must not hide a later error.
                // Remove only an earlier lower-priority item, preserving order
                // among the retained notifications from this surface.
                if let Some(index) = queue.iter().position(|old: &osc::OscNotification| {
                    priority(old.level) < priority(note.level)
                }) {
                    queue.remove(index);
                }
            }
            if queue.len() < 16 {
                queue.push(note);
            }
        }) as OscCallback);
        Self { extractor, pending }
    }
}
impl Sniffer {
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<osc::OscNotification> {
        self.extractor.feed(bytes);
        self.pending.borrow_mut().drain(..).collect()
    }
}

#[derive(Debug, Clone, clap::Subcommand, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Inspect retained entries without marking them read.
    List {
        #[arg(long)]
        #[serde(default)]
        unread: bool,
    },
    /// Show the native notification list and mark its existing entries read.
    Show {},
    Open {
        id: uuid::Uuid,
    },
    JumpToUnread {},
    MarkRead {
        id: uuid::Uuid,
    },
    Delete {
        id: uuid::Uuid,
    },
    Clear {},
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_hook_notices_keep_linux_titles_levels_unicode_and_informational_scope() {
        use crate::{agent_activity::Input, command::SessionHookEvent as Event};
        let mut input = Input {
            message: Some("한글 한 é 😀 입력".into()),
            ..Default::default()
        };
        let note = hook_notice("Codex", Event::Notification, &input).unwrap();
        assert_eq!(note.title, "Codex needs your input");
        assert_eq!(note.body, input.message.as_deref().unwrap());
        assert_eq!(note.level, NotificationLevel::NeedsInput);
        input.notification_type = Some("agent_completed".into());
        let note = hook_notice("Claude", Event::Notification, &input).unwrap();
        assert_eq!(note.title, "Claude background agent finished");
        assert_eq!(note.level, NotificationLevel::Info);
        for category in ["idle_prompt", "auth_success", "elicitation_response"] {
            input.notification_type = Some(category.into());
            assert!(hook_notice("Claude", Event::Notification, &input).is_none());
        }
        input.error = Some("rate_limit".into());
        let note = hook_notice("Claude", Event::StopFailure, &input).unwrap();
        assert_eq!(
            (note.title.as_str(), note.body.as_str(), note.level),
            (
                "Claude stopped",
                "API error: rate_limit",
                NotificationLevel::Error
            )
        );
        input.tool_name = Some("AskUserQuestion".into());
        assert!(hook_notice("Claude", Event::ToolStart, &input).is_none());
        input.permission_mode = Some("bypassPermissions".into());
        assert_eq!(
            hook_notice("Claude", Event::ToolStart, &input)
                .unwrap()
                .level,
            NotificationLevel::NeedsInput
        );
        let note = agent_notice(
            "Claude",
            "ready",
            &"한😀".repeat(100),
            NotificationLevel::TurnCompleted,
        );
        assert_eq!(note.body.chars().count(), 161);
        assert!(note.body.ends_with('…'));
    }
    #[test]
    fn desktop_fields_preserve_raw_unicode_and_complete_surrogates() {
        let text = "한글 한 é 😀";
        let field = desktop_text::<64>(text);
        assert_eq!(
            String::from_utf16(&field[..text.encode_utf16().count()]).unwrap(),
            text
        );
        for length in 0..300 {
            let text = format!("{}😀한", "x".repeat(length));
            for field in [
                desktop_text::<64>(&text).to_vec(),
                desktop_text::<256>(&text).to_vec(),
            ] {
                assert_eq!(field.last(), Some(&0));
                let end = field.iter().position(|unit| *unit == 0).unwrap();
                let shown = String::from_utf16(&field[..end]).unwrap();
                assert!(text.starts_with(&shown));
                assert!(text.encode_utf16().count() >= field.len() || shown == text);
            }
        }
    }
    #[test]
    fn split_unicode_and_metadata_filter_keep_osc_contracts_separate() {
        let bytes = "\x1b]9;한글 한 😀\x07\x1b]9;9;C:\\한글\x07\x1b]9;4;1;75\x07\x1b]99;;needs approval\x1b\\\x1b]777;notify;완료;done\x07".as_bytes();
        let mut sniffer = Sniffer::default();
        let mut found = vec![];
        for byte in bytes {
            found.extend(sniffer.feed(&[*byte]));
        }
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].body, "한글 한 😀");
        assert_eq!(found[1].level, NotificationLevel::NeedsInput);
        assert_eq!(found[2].title, "완료");
    }
    #[test]
    fn malformed_oversize_and_bursts_are_bounded_and_recover() {
        let mut sniffer = Sniffer::default();
        assert!(sniffer
            .feed(format!("\x1b]9;{}\x07", "x".repeat(stream::MAX_OSC_PAYLOAD + 1)).as_bytes())
            .is_empty());
        assert!(sniffer.feed(b"\x1b]9;\xff\x07").is_empty());
        assert_eq!(
            sniffer
                .feed("\x1b]9;done\x07".repeat(1000).as_bytes())
                .len(),
            16
        );
        let burst = format!(
            "{}\x1b]9;error after burst\x07",
            "\x1b]9;done\x07".repeat(1000)
        );
        let notices = sniffer.feed(burst.as_bytes());
        assert_eq!(notices.len(), 16);
        assert_eq!(notices.last().unwrap().level, NotificationLevel::Error);
        assert_eq!(sniffer.feed(b"\x1b]9;next\x07").len(), 1);
        assert!(validate("ok", &"한".repeat(3000)).is_err());
        assert!(suppress(NotificationLevel::Info, true));
        assert!(!suppress(NotificationLevel::NeedsInput, true));
        assert!(!suppress(NotificationLevel::Error, true));
        assert!(!suppress(NotificationLevel::TurnCompleted, false));
    }
}
