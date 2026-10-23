// SPDX-License-Identifier: GPL-3.0-or-later
//! One physical key with optional modifiers for native browser input.

#[derive(Clone, Copy)]
pub enum PointerAction {
    Click,
    DoubleClick,
    Hover,
    Drag { to_x: f64, to_y: f64 },
}

#[derive(serde::Deserialize)]
pub struct PointerTarget {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl PointerTarget {
    pub fn scale(&self, width: f64, height: f64) -> Result<(f64, f64), String> {
        if ![self.x, self.y, self.width, self.height, width, height]
            .iter()
            .all(|v| v.is_finite())
            || self.width <= 0.0
            || self.height <= 0.0
            || width <= 0.0
            || height <= 0.0
            || self.x < 0.0
            || self.y < 0.0
            || self.x >= self.width
            || self.y >= self.height
        {
            return Err("pointer is outside the browser viewport".into());
        }
        Ok((self.x / self.width * width, self.y / self.height * height))
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct KeyStroke {
    pub key: String,
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
}

impl KeyStroke {
    pub fn parse(source: &str) -> Result<Self, String> {
        let (modifiers, key) = if source == "+" {
            ("", source)
        } else {
            source.rsplit_once('+').unwrap_or(("", source))
        };
        if source.starts_with('+') && source != "+" {
            return Err("key modifier cannot be empty".into());
        }
        let mut stroke = Self::default();
        if !modifiers.is_empty() {
            for modifier in modifiers.split('+') {
                let flag = match modifier {
                    "Control" | "Ctrl" => &mut stroke.control,
                    "Alt" | "Option" => &mut stroke.alt,
                    "Shift" => &mut stroke.shift,
                    "Meta" | "Command" | "Cmd" | "Super" => &mut stroke.meta,
                    _ => return Err(format!("unknown key modifier: {modifier}")),
                };
                if std::mem::replace(flag, true) {
                    return Err(format!("duplicate key modifier: {modifier}"));
                }
            }
        }
        stroke.key = match key {
            "Return" => "Enter",
            "Esc" => "Escape",
            "Space" => " ",
            "Plus" => "+",
            key => key,
        }
        .into();
        let named = matches!(
            stroke.key.as_str(),
            "Enter"
                | "Tab"
                | "Escape"
                | "Backspace"
                | "Delete"
                | "Insert"
                | "Home"
                | "End"
                | "PageUp"
                | "PageDown"
                | "ArrowLeft"
                | "ArrowRight"
                | "ArrowUp"
                | "ArrowDown"
        ) || stroke
            .key
            .strip_prefix('F')
            .and_then(|n| n.parse::<u8>().ok())
            .is_some_and(|n| (1..=12).contains(&n) && stroke.key == format!("F{n}"));
        if !named
            && !(stroke.key.chars().count() == 1
                && !stroke.key.chars().next().unwrap().is_control())
        {
            return Err(format!("unsupported key: {key}"));
        }
        Ok(stroke)
    }

    pub fn event_key(&self) -> String {
        if self.shift && self.key.len() == 1 {
            let plain = "`1234567890-=[]\\;',./";
            let shifted = "~!@#$%^&*()_+{}|:\"<>?";
            plain
                .find(&self.key)
                .map(|i| shifted[i..i + 1].to_string())
                .unwrap_or_else(|| self.key.to_ascii_uppercase())
        } else {
            self.key.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointer_coordinates_scale_only_within_finite_viewports() {
        let mut target = PointerTarget {
            x: 30.0,
            y: 20.0,
            width: 100.0,
            height: 50.0,
        };
        assert_eq!(target.scale(200.0, 100.0).unwrap(), (60.0, 40.0));
        assert!(target.scale(0.0, 100.0).is_err());
        target.x = 100.0;
        assert!(target.scale(200.0, 100.0).is_err());
        target.x = f64::NAN;
        assert!(target.scale(200.0, 100.0).is_err());
    }

    #[test]
    fn parses_native_shortcuts_and_rejects_ambiguous_chords() {
        let key = KeyStroke::parse("Ctrl+Shift+Tab").unwrap();
        assert!(key.control && key.shift && !key.alt && !key.meta);
        assert_eq!(key.key, "Tab");
        assert_eq!(KeyStroke::parse("Cmd+a").unwrap().key, "a");
        assert_eq!(KeyStroke::parse("Control+Plus").unwrap().key, "+");
        assert_eq!(KeyStroke::parse("Shift+1").unwrap().event_key(), "!");
        assert_eq!(KeyStroke::parse("Shift+a").unwrap().event_key(), "A");
        for invalid in [
            "",
            "Hyper+a",
            "Control+",
            "Ctrl+Control+a",
            "Shift+Shift+Tab",
            "Unknown",
            "F99",
            "F01",
            "+a",
        ] {
            assert!(KeyStroke::parse(invalid).is_err(), "{invalid}");
        }
    }
}
