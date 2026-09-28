// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared Linux presets resolved into platform-independent Windows colors.
//! No user theme file is imported or read by this adapter.
use crate::settings::{TerminalSettings, Theme};
use flowmux_config::ghostty::GhosttyConfig;
use serde::{Deserialize, Serialize};

const DEFAULT_BG: &str = "#282c34";
const DEFAULT_FG: &str = "#ffffff";
// Same stock fallbacks as crates/flowmux/src/theme.rs; its resolver requires GTK.
const DEFAULT_PALETTE: [&str; 16] = [
    "#5c6370", "#cc6666", "#b5bd68", "#f0c674", "#81a2be", "#b294bb", "#8abeb7", "#c5c8c6",
    "#7f848e", "#d54e53", "#b9ca4a", "#e7c547", "#7aa6da", "#c397d8", "#70c0b1", "#eaeaea",
];
const LEGACY_LIGHT_PALETTE: [&str; 16] = [
    "#202124", "#b42318", "#18733b", "#805500", "#185abc", "#8f2db3", "#007580", "#c5c7cb",
    "#686b70", "#c5221f", "#188038", "#956500", "#1967d2", "#a142b8", "#00838f", "#f1f3f4",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedTheme {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection_background: Option<String>,
    pub selection_foreground: Option<String>,
    pub palette: [String; 16],
    pub dark: bool,
}

pub fn presets() -> &'static [flowmux_config::presets::ThemePreset] {
    flowmux_config::presets::PRESETS
}

pub fn valid_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
}

/// An empty edit removes the override. Otherwise accept only an opaque hex RGB.
pub fn color_override(value: &str) -> anyhow::Result<Option<String>> {
    if value.is_empty() {
        return Ok(None);
    }
    anyhow::ensure!(valid_color(value), "theme color must be #RRGGBB");
    Ok(Some(value.to_ascii_lowercase()))
}

fn color(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| valid_color(value))
        .map(str::to_ascii_lowercase)
}

fn from_config(cfg: &GhosttyConfig) -> ResolvedTheme {
    let background = color(cfg.background.as_deref()).unwrap_or_else(|| DEFAULT_BG.into());
    let foreground = color(cfg.foreground.as_deref()).unwrap_or_else(|| DEFAULT_FG.into());
    // Match the Linux resolver's weighted encoded-RGB threshold, not a new
    // gamma-correct luminance algorithm that would classify themes differently.
    let component = |offset| {
        u8::from_str_radix(&background[offset..offset + 2], 16).unwrap_or(0) as f32 / 255.0
    };
    let dark = 0.2126 * component(1) + 0.7152 * component(3) + 0.0722 * component(5) < 0.5;
    ResolvedTheme {
        cursor: color(cfg.cursor_color.as_deref()).unwrap_or_else(|| foreground.clone()),
        background,
        foreground,
        selection_background: color(cfg.selection_background.as_deref()),
        selection_foreground: color(cfg.selection_foreground.as_deref()),
        palette: std::array::from_fn(|i| {
            color(cfg.palette[i].as_deref()).unwrap_or_else(|| DEFAULT_PALETTE[i].into())
        }),
        dark,
    }
}

pub fn resolve_preset(id: &str) -> anyhow::Result<ResolvedTheme> {
    let cfg = flowmux_config::presets::config(id)
        .ok_or_else(|| anyhow::anyhow!("unknown theme preset: {id}"))?;
    Ok(from_config(&cfg))
}

/// Nonempty preset selection takes precedence over the retained legacy mode.
/// Clearing the preset restores that mode. Overrides apply to either base.
pub fn resolve(settings: &TerminalSettings) -> ResolvedTheme {
    let mut cfg = if let Some(id) = settings.theme_preset.as_deref() {
        // Settings validation rejects unknown ids. Keep the shared Linux default
        // fallback for callers resolving an as-yet-unvalidated in-memory value.
        flowmux_config::presets::config(id).unwrap_or_default()
    } else {
        let (background, foreground, cursor, selection, palette) = match settings.theme {
            Theme::Dark => (
                DEFAULT_BG,
                DEFAULT_FG,
                "#b9c6ff",
                "#455483",
                DEFAULT_PALETTE,
            ),
            Theme::Light => (
                "#ffffff",
                "#202124",
                "#202124",
                "#b5cff7",
                LEGACY_LIGHT_PALETTE,
            ),
        };
        GhosttyConfig {
            background: Some(background.into()),
            foreground: Some(foreground.into()),
            cursor_color: Some(cursor.into()),
            selection_background: Some(selection.into()),
            palette: palette.map(|color| Some(color.into())),
            ..Default::default()
        }
    };
    cfg.merge(settings.theme_overrides.shared().to_ghostty());
    from_config(&cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Document, SettingKey};

    #[test]
    fn theme_presets_overrides_legacy_and_cas_contract() {
        let old = Document::decode(br#"{"version":1,"revision":"00000000-0000-0000-0000-000000000000","terminal":{"theme":"light"}}"#).unwrap();
        assert!(old.terminal.theme_preset.is_none());
        assert!(old.terminal.theme_overrides.is_empty());
        let legacy = resolve(&old.terminal);
        assert_eq!(
            (
                legacy.background.as_str(),
                legacy.cursor.as_str(),
                legacy.palette[1].as_str()
            ),
            ("#ffffff", "#202124", "#b42318")
        );
        assert_eq!(legacy.selection_background.as_deref(), Some("#b5cff7"));
        assert_eq!(legacy.selection_foreground, None);
        assert!(!legacy.dark);
        let dark = resolve(&TerminalSettings::default());
        assert_eq!(dark.cursor, "#b9c6ff");
        assert_eq!(dark.selection_background.as_deref(), Some("#455483"));
        assert!(dark.dark);
        let serialized = serde_json::to_value(&old.terminal).unwrap();
        assert!(
            serialized.get("theme_preset").is_none() && serialized.get("theme_overrides").is_none()
        );
        assert_eq!(presets().len(), 11);
        for preset in presets() {
            let cfg = flowmux_config::presets::config(preset.id).unwrap();
            let resolved = resolve_preset(preset.id).unwrap();
            assert!(
                valid_color(&resolved.background)
                    && valid_color(&resolved.foreground)
                    && valid_color(&resolved.cursor)
            );
            for (i, actual) in resolved.palette.iter().enumerate() {
                assert_eq!(
                    actual,
                    cfg.palette[i].as_deref().unwrap_or(DEFAULT_PALETTE[i])
                );
            }
            if preset.id == "default" {
                assert_eq!(resolved.cursor, DEFAULT_FG);
                assert_eq!(resolved.selection_background, None);
                assert_eq!(resolved.selection_foreground, None);
            } else {
                assert_eq!(resolved.background, cfg.background.unwrap());
                assert_eq!(resolved.cursor, cfg.cursor_color.unwrap());
                assert_eq!(resolved.selection_background, cfg.selection_background);
                assert_eq!(resolved.selection_foreground, cfg.selection_foreground);
            }
        }
        let overridden = old
            .terminal
            .changed(SettingKey::ThemeBackground, "#AABBCC", Some(""))
            .unwrap();
        assert_eq!(overridden.value(SettingKey::ThemeBackground), "#aabbcc");
        let preset = overridden
            .changed(SettingKey::ThemePreset, "dracula", Some(""))
            .unwrap();
        assert_eq!(preset.theme, Theme::Light);
        assert_eq!(preset.theme_overrides, overridden.theme_overrides);
        assert_eq!(resolve(&preset).background, "#aabbcc");
        assert_eq!(
            resolve(&preset).palette,
            resolve_preset("dracula").unwrap().palette
        );
        let changed = preset
            .changed(SettingKey::ThemeCursor, "#123456", Some(""))
            .unwrap();
        let stale = preset.value(SettingKey::ThemeOverrides);
        assert!(changed
            .changed(SettingKey::ThemeOverrides, "{}", Some(&stale))
            .is_err());
        let baseline = changed.value(SettingKey::ThemeOverrides);
        let reset = changed
            .changed(SettingKey::ThemeOverrides, "{}", Some(&baseline))
            .unwrap();
        assert!(reset.theme_overrides.is_empty());
        assert_eq!(reset.theme_preset.as_deref(), Some("dracula"));
        assert_eq!(resolve(&reset), resolve_preset("dracula").unwrap());
        assert_eq!(
            resolve(
                &reset
                    .changed(SettingKey::ThemePreset, "", Some("dracula"))
                    .unwrap()
            ),
            legacy
        );
        let inherited = changed
            .changed(SettingKey::ThemeBackground, "", Some("#aabbcc"))
            .unwrap();
        assert_eq!(
            resolve(&inherited).background,
            resolve_preset("dracula").unwrap().background
        );
        for (key, value) in [
            (SettingKey::ThemePreset, "missing"),
            (SettingKey::ThemeBackground, "#fff"),
            (SettingKey::ThemeBackground, "#aabbccdd"),
            (SettingKey::ThemeCursor, " #123456"),
            (SettingKey::ThemeCursor, "#GG0000"),
            (SettingKey::ThemeOverrides, r#"{"background":""}"#),
            (SettingKey::ThemeOverrides, r##"{"other":"#112233"}"##),
        ] {
            assert!(changed.changed(key, value, None).is_err());
        }
        assert_eq!(changed.theme_overrides.cursor.as_deref(), Some("#123456"));
        let all = reset.changed(SettingKey::ThemeOverrides, r##"{"selection_foreground":"#123450","selection_background":"#234560","cursor":"#345670","foreground":"#456780","background":"#ABCDEF"}"##, Some(" {} ")).unwrap();
        let colors = resolve(&all);
        assert_eq!(
            (
                colors.background.as_str(),
                colors.foreground.as_str(),
                colors.cursor.as_str()
            ),
            ("#abcdef", "#456780", "#345670")
        );
        assert_eq!(colors.selection_background.as_deref(), Some("#234560"));
        assert_eq!(colors.selection_foreground.as_deref(), Some("#123450"));
        assert!(reset
            .changed(
                SettingKey::ThemeOverrides,
                "[null,null,null,null,null]",
                None
            )
            .is_err());
    }
}
