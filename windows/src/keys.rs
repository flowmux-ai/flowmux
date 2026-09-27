// SPDX-License-Identifier: GPL-3.0-or-later
//! Named terminal keys. Text/IME production continues to belong to xterm.
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct Key {
    base: String,
    shift: bool,
    alt: bool,
    ctrl: bool,
}
impl Key {
    pub fn parse(value: &str) -> anyhow::Result<Self> {
        anyhow::ensure!(
            value.len() <= 64 && value.is_ascii(),
            "unsupported named key"
        );
        let name = match value.to_ascii_lowercase().as_str() {
            "shiftenter" => "shift+enter".to_owned(),
            "shifttab" => "shift+tab".to_owned(),
            _ => value.to_ascii_lowercase(),
        };
        let mut parts = name.split('+').collect::<Vec<_>>();
        let base = parts.pop().unwrap_or_default();
        let mut key = Self {
            base: base.into(),
            shift: false,
            alt: false,
            ctrl: false,
        };
        for modifier in parts {
            let flag = match modifier {
                "shift" => &mut key.shift,
                "alt" => &mut key.alt,
                "ctrl" | "control" => &mut key.ctrl,
                _ => anyhow::bail!("unsupported key modifier"),
            };
            anyhow::ensure!(!*flag, "duplicate key modifier");
            *flag = true;
        }
        // Validate before any asynchronous request or native input submission.
        key.encode(false)?;
        Ok(key)
    }
    pub fn encode(&self, application_cursor: bool) -> anyhow::Result<Vec<u8>> {
        let modifiers =
            u8::from(self.shift) | (u8::from(self.alt) << 1) | (u8::from(self.ctrl) << 2);
        let cursor = |suffix: char| {
            if modifiers != 0 {
                format!("\x1b[1;{}{suffix}", modifiers + 1)
            } else if application_cursor {
                format!("\x1bO{suffix}")
            } else {
                format!("\x1b[{suffix}")
            }
        };
        let tilde = |code: u8| {
            if modifiers != 0 {
                format!("\x1b[{code};{}~", modifiers + 1)
            } else {
                format!("\x1b[{code}~")
            }
        };
        let alt = |text: &str| {
            if self.alt {
                format!("\x1b{text}")
            } else {
                text.to_owned()
            }
        };
        let encoded = match self.base.as_str() {
            "enter" | "return" => if self.alt || self.shift {
                "\x1b\r"
            } else {
                "\r"
            }
            .into(),
            "tab" => if self.shift { "\x1b[Z" } else { "\t" }.into(),
            "escape" | "esc" => alt("\x1b"),
            "backspace" | "bspace" => alt(if self.ctrl { "\x08" } else { "\x7f" }),
            "up" | "arrowup" => cursor('A'),
            "down" | "arrowdown" => cursor('B'),
            "right" | "arrowright" => cursor('C'),
            "left" | "arrowleft" => cursor('D'),
            "home" => cursor('H'),
            "end" => cursor('F'),
            "delete" | "del" => tilde(3),
            "insert" | "ins" => {
                anyhow::ensure!(
                    !self.ctrl && !self.shift,
                    "modified Insert belongs to clipboard actions"
                );
                "\x1b[2~".into()
            }
            "pageup" | "pgup" | "pagedown" | "pgdn" => {
                anyhow::ensure!(
                    !self.shift,
                    "Shift+PageUp/PageDown belongs to viewport scrolling"
                );
                let code = if matches!(self.base.as_str(), "pageup" | "pgup") {
                    5
                } else {
                    6
                };
                if self.ctrl {
                    tilde(code)
                } else {
                    format!("\x1b[{code}~")
                }
            }
            name if name.starts_with('f') && (2..=3).contains(&name.len()) => {
                let index = name[1..]
                    .parse::<u8>()
                    .ok()
                    .filter(|n| (1..=12).contains(n))
                    .ok_or_else(|| anyhow::anyhow!("supported function keys are F1 through F12"))?;
                if index <= 4 {
                    let suffix = char::from(b'P' + index - 1);
                    if modifiers == 0 {
                        format!("\x1bO{suffix}")
                    } else {
                        format!("\x1b[1;{}{suffix}", modifiers + 1)
                    }
                } else {
                    tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(index - 5)])
                }
            }
            "space" if self.ctrl || self.alt => {
                anyhow::ensure!(!self.shift, "unsupported modified Space");
                alt(if self.ctrl { "\0" } else { " " })
            }
            name if name.len() == 1 && (self.ctrl || self.alt) => {
                let ch = name.as_bytes()[0];
                if ch.is_ascii_lowercase() {
                    anyhow::ensure!(
                        !self.ctrl || !self.shift,
                        "Ctrl+Shift+letter is not a named terminal key"
                    );
                    let code = if self.ctrl {
                        ch - b'a' + 1
                    } else if self.shift {
                        ch.to_ascii_uppercase()
                    } else {
                        ch
                    };
                    alt(&char::from(code).to_string())
                } else {
                    anyhow::ensure!(
                        self.ctrl && !self.alt && !self.shift,
                        "unsupported named key"
                    );
                    let code = match ch {
                        b'3'..=b'7' => ch - b'3' + 27,
                        b'8' => 127,
                        b'[' => 27,
                        b'\\' => 28,
                        b']' => 29,
                        b'_' => 31,
                        b'@' => 0,
                        _ => anyhow::bail!("unsupported named key"),
                    };
                    char::from(code).to_string()
                }
            }
            _ => anyhow::bail!("unsupported named key"),
        };
        Ok(encoded.into_bytes())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModeOutcome {
    Ok { application_cursor: bool },
    Error { message: String },
}
impl ModeOutcome {
    pub fn application_cursor(self) -> anyhow::Result<bool> {
        match self {
            Self::Ok { application_cursor } => Ok(application_cursor),
            Self::Error { message } => {
                anyhow::ensure!(message.len() <= 512, "invalid named-key error");
                anyhow::bail!(message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_keys_match_xterm_reference_vectors_in_both_cursor_modes() {
        let data: serde_json::Value =
            serde_json::from_str(include_str!("../terminal/src/named-key.cases.json")).unwrap();
        for case in data["cases"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let key = Key::parse(name).unwrap();
            for (mode, field) in [(false, "normal"), (true, "application")] {
                assert_eq!(
                    key.encode(mode).unwrap(),
                    case[field].as_str().unwrap().as_bytes(),
                    "{name} {field}"
                );
            }
        }
    }
    #[test]
    fn invalid_or_ui_owned_keys_fail_before_input_and_legacy_aliases_remain() {
        for value in [
            "",
            "한",
            "a",
            "Ctrl++",
            "Ctrl+Ctrl+C",
            "Meta+C",
            "Ctrl+Shift+C",
            "F0",
            "F13",
            "Shift+Insert",
            "Ctrl+Insert",
            "Shift+PageUp",
            "Ctrl+2",
        ] {
            assert!(Key::parse(value).is_err(), "{value}");
        }
        assert!(Key::parse(&"x".repeat(65)).is_err());
        for (alias, bytes) in [
            ("ShiftEnter", b"\x1b\r".as_slice()),
            ("ShiftTab", b"\x1b[Z"),
            ("Control+C", b"\x03"),
            ("ArRoWuP", b"\x1b[A"),
        ] {
            assert_eq!(Key::parse(alias).unwrap().encode(false).unwrap(), bytes);
        }
        assert!(serde_json::from_str::<ModeOutcome>(
            r#"{"status":"ok","application_cursor":false,"data":"injected"}"#
        )
        .is_err());
        assert!(ModeOutcome::Error {
            message: "x".repeat(513)
        }
        .application_cursor()
        .is_err());
    }
}
