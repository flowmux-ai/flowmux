// SPDX-License-Identifier: GPL-3.0-or-later
use anyhow::Context;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_SETTINGS_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SettingKey {
    FontFamily,
    FontSize,
    Theme,
    Scrollback,
    CursorBlink,
    CursorStyle,
    MinimapEnabled,
    MinimapWidth,
    MinimapOpacity,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    Dark,
    Light,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    Block,
    Underline,
    Bar,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
    pub font_family: String,
    pub font_size: u16,
    pub theme: Theme,
    pub scrollback: u32,
    pub cursor_blink: bool,
    pub cursor_style: CursorStyle,
    pub minimap_enabled: bool,
    pub minimap_width: u16,
    pub minimap_opacity: u8,
}
impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: "Cascadia Mono, Consolas, \"Malgun Gothic\", monospace".into(),
            font_size: 14,
            theme: Theme::Dark,
            scrollback: 10000,
            cursor_blink: true,
            cursor_style: CursorStyle::Block,
            minimap_enabled: true,
            minimap_width: 40,
            minimap_opacity: 50,
        }
    }
}
impl TerminalSettings {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.font_family.trim().is_empty()
                && self.font_family.encode_utf16().count() <= 256
                && !self.font_family.chars().any(char::is_control),
            "font family must be a nonempty single line of at most 256 UTF-16 units"
        );
        anyhow::ensure!(
            (6..=72).contains(&self.font_size),
            "font size must be between 6 and 72"
        );
        anyhow::ensure!(
            self.scrollback <= 100_000,
            "scrollback must be between 0 and 100000"
        );
        anyhow::ensure!(
            (12..=96).contains(&self.minimap_width),
            "minimap width must be between 12 and 96"
        );
        anyhow::ensure!(
            self.minimap_opacity <= 100,
            "minimap opacity must be between 0 and 100"
        );
        Ok(())
    }
    pub fn value(&self, key: SettingKey) -> String {
        match key {
            SettingKey::FontFamily => self.font_family.clone(),
            SettingKey::FontSize => self.font_size.to_string(),
            SettingKey::Scrollback => self.scrollback.to_string(),
            SettingKey::CursorBlink => self.cursor_blink.to_string(),
            SettingKey::MinimapEnabled => self.minimap_enabled.to_string(),
            SettingKey::MinimapWidth => self.minimap_width.to_string(),
            SettingKey::MinimapOpacity => self.minimap_opacity.to_string(),
            SettingKey::Theme => match self.theme {
                Theme::Dark => "dark",
                Theme::Light => "light",
            }
            .into(),
            SettingKey::CursorStyle => match self.cursor_style {
                CursorStyle::Block => "block",
                CursorStyle::Underline => "underline",
                CursorStyle::Bar => "bar",
            }
            .into(),
        }
    }
    pub fn changed(
        &self,
        key: SettingKey,
        value: &str,
        expected: Option<&str>,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            expected.is_none_or(|value| value == self.value(key)),
            "this setting changed elsewhere; reopen the editor before applying"
        );
        let mut next = self.clone();
        match key {
            SettingKey::FontFamily => next.font_family = value.into(),
            SettingKey::FontSize => {
                next.font_size = value
                    .trim()
                    .parse()
                    .context("font size must be an integer")?
            }
            SettingKey::Scrollback => {
                next.scrollback = value
                    .trim()
                    .parse()
                    .context("scrollback must be an integer")?
            }
            SettingKey::CursorBlink => {
                next.cursor_blink = value
                    .parse()
                    .context("cursor blink must be true or false")?
            }
            SettingKey::MinimapEnabled => {
                next.minimap_enabled = value
                    .parse()
                    .context("minimap enabled must be true or false")?
            }
            SettingKey::MinimapWidth => {
                next.minimap_width = value
                    .trim()
                    .parse()
                    .context("minimap width must be an integer")?
            }
            SettingKey::MinimapOpacity => {
                next.minimap_opacity = value
                    .trim()
                    .parse()
                    .context("minimap opacity must be an integer")?
            }
            SettingKey::Theme => {
                next.theme = match value {
                    "dark" => Theme::Dark,
                    "light" => Theme::Light,
                    _ => anyhow::bail!("theme must be dark or light"),
                }
            }
            SettingKey::CursorStyle => {
                next.cursor_style = match value {
                    "block" => CursorStyle::Block,
                    "underline" => CursorStyle::Underline,
                    "bar" => CursorStyle::Bar,
                    _ => anyhow::bail!("cursor style must be block, underline or bar"),
                }
            }
        }
        next.validate()?;
        Ok(next)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub version: u8,
    pub revision: Uuid,
    pub terminal: TerminalSettings,
    #[serde(default)]
    pub default_shell: crate::shell::Shell,
}
impl Default for Document {
    fn default() -> Self {
        Self {
            version: 1,
            revision: Uuid::nil(),
            terminal: TerminalSettings::default(),
            default_shell: Default::default(),
        }
    }
}
impl Document {
    pub fn decode(bytes: &[u8]) -> anyhow::Result<Self> {
        anyhow::ensure!(
            bytes.len() <= MAX_SETTINGS_BYTES,
            "settings file exceeds 64 KiB"
        );
        let value: Self = serde_json::from_slice(bytes)?;
        anyhow::ensure!(value.version == 1, "unsupported settings version");
        value.terminal.validate()?;
        value.default_shell.validate()?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn settings_preserve_unicode_and_validate_before_changing() {
        let old = TerminalSettings::default();
        let name = "\"한글 한 e\u{301} 😀\", Consolas, monospace";
        let next = old.changed(SettingKey::FontFamily, name, None).unwrap();
        assert_eq!(next.font_family, name);
        for (key, value) in [
            (SettingKey::FontSize, "5"),
            (SettingKey::FontSize, "73"),
            (SettingKey::FontSize, "14.5"),
            (SettingKey::Scrollback, "100001"),
            (SettingKey::FontFamily, "bad\nfont"),
            (SettingKey::Theme, "unknown"),
            (SettingKey::CursorBlink, "yes"),
            (SettingKey::CursorStyle, "none"),
            (SettingKey::MinimapEnabled, "yes"),
            (SettingKey::MinimapWidth, "11"),
            (SettingKey::MinimapWidth, "97"),
            (SettingKey::MinimapOpacity, "101"),
        ] {
            assert!(old.changed(key, value, None).is_err());
        }
        assert_eq!(old, TerminalSettings::default());
        assert!(old.changed(SettingKey::FontSize, "20", Some("16")).is_err());
        assert_eq!(
            old.changed(SettingKey::FontSize, "20", Some("14"))
                .unwrap()
                .font_size,
            20
        );
    }
    #[test]
    fn settings_schema_rejects_future_unknown_and_oversized_documents() {
        let doc = Document::default();
        assert_eq!(
            Document::decode(&serde_json::to_vec(&doc).unwrap()).unwrap(),
            doc
        );
        let mut value = serde_json::to_value(&doc).unwrap();
        value["version"] = 2.into();
        assert!(Document::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        value["version"] = 1.into();
        value["terminal"]["unknown"] = true.into();
        assert!(Document::decode(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(Document::decode(&vec![b' '; MAX_SETTINGS_BYTES + 1]).is_err());
    }
    #[test]
    fn old_settings_gain_minimap_defaults_and_boundary_values_persist() {
        let old = Document::decode(
            br#"{"version":1,"revision":"00000000-0000-0000-0000-000000000000","terminal":{}}"#,
        )
        .unwrap();
        assert!(old.terminal.minimap_enabled);
        assert_eq!(
            (old.terminal.minimap_width, old.terminal.minimap_opacity),
            (40, 50)
        );
        for (key, text) in [
            (SettingKey::MinimapWidth, "12"),
            (SettingKey::MinimapWidth, "96"),
            (SettingKey::MinimapOpacity, "0"),
            (SettingKey::MinimapOpacity, "100"),
            (SettingKey::MinimapEnabled, "false"),
        ] {
            let value = old.terminal.changed(key, text, None).unwrap();
            assert_eq!(value.value(key), text);
            assert_eq!(
                serde_json::from_slice::<TerminalSettings>(&serde_json::to_vec(&value).unwrap())
                    .unwrap(),
                value
            );
        }
    }
}
