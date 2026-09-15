// SPDX-License-Identifier: GPL-3.0-or-later
//! User theme overrides at `<config dir>/flowmux/theme`, parsed using the
//! Ghostty key/value format. Built-in themes are provided by [`crate::presets`].

use crate::ghostty::GhosttyConfig;
use std::path::PathBuf;

/// `~/.config/flowmux/theme`
pub fn user_theme_path() -> Option<PathBuf> {
    crate::paths::config_dir().map(|d| d.join("theme"))
}

/// Load and parse the flowmux theme file if it exists. Returns `None`
/// if the file is absent so the caller can fall through to built-in
/// defaults.
pub fn load() -> Option<GhosttyConfig> {
    let path = user_theme_path()?;
    if !path.is_file() {
        return None;
    }
    crate::ghostty::load(&path).ok()
}

/// Copy the file at `src` into the flowmux theme location, creating
/// the parent directory if needed. Returns the destination path on
/// success. Used by `flowmux theme import <path>`.
pub fn import_from(src: &std::path::Path) -> std::io::Result<PathBuf> {
    let dest =
        user_theme_path().ok_or_else(|| std::io::Error::other("XDG config dir unavailable"))?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(src, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_theme_path_uses_flowmux_config_dir() {
        let _guard = crate::test_env::env_lock().lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", dir.path());

        assert_eq!(user_theme_path().unwrap(), dir.path().join("flowmux/theme"));
    }

    #[test]
    fn missing_theme_returns_none() {
        let _guard = crate::test_env::env_lock().lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("XDG_CONFIG_HOME", dir.path());

        assert!(load().is_none());
    }

    #[test]
    fn import_creates_parent_and_loads_theme() {
        let _guard = crate::test_env::env_lock().lock().unwrap();
        let config_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let src = source_dir.path().join("theme");
        std::fs::write(
            &src,
            "background = #10131a\nforeground = #f8f8f2\npalette = 2=#50fa7b\n",
        )
        .unwrap();
        std::env::set_var("XDG_CONFIG_HOME", config_dir.path());

        let dest = import_from(&src).unwrap();
        assert_eq!(dest, config_dir.path().join("flowmux/theme"));
        assert!(dest.is_file());

        let cfg = load().unwrap();
        assert_eq!(cfg.background.as_deref(), Some("#10131a"));
        assert_eq!(cfg.foreground.as_deref(), Some("#f8f8f2"));
        assert_eq!(cfg.palette[2].as_deref(), Some("#50fa7b"));
    }
}
