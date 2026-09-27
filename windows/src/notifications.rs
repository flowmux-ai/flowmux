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
