// SPDX-License-Identifier: GPL-3.0-or-later
//! Persistent browser profile identifiers. Each slug has its own data directory.
//! Selecting a profile does not import host-browser cookies.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BrowserProfile {
    Default,
    FirefoxImport,
    ChromeImport,
    Custom { name: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("XDG data dir unavailable")]
    NoDataDir,
    #[error("invalid profile name: {0}")]
    InvalidName(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl BrowserProfile {
    /// Stable filesystem-safe identifier for this profile. Used as
    /// the directory name under `$XDG_DATA_HOME/flowmux/browser/`.
    pub fn slug(&self) -> String {
        match self {
            BrowserProfile::Default => "default".into(),
            BrowserProfile::FirefoxImport => "firefox-import".into(),
            BrowserProfile::ChromeImport => "chrome-import".into(),
            BrowserProfile::Custom { name } => format!("custom-{}", sanitize(name)),
        }
    }

    /// Human-readable label for the right-click menu.
    pub fn display_name(&self) -> String {
        match self {
            BrowserProfile::Default => "Default".into(),
            BrowserProfile::FirefoxImport => "Firefox import".into(),
            BrowserProfile::ChromeImport => "Chrome import".into(),
            BrowserProfile::Custom { name } => name.clone(),
        }
    }

    /// Create `<platform data dir>/flowmux/browser/<slug>/` if absent.
    pub fn data_dir(&self) -> Result<PathBuf, ProfileError> {
        let base = dirs::data_dir().ok_or(ProfileError::NoDataDir)?;
        let dir = base.join("flowmux").join("browser").join(self.slug());
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    pub fn parse_slug(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Self::Default),
            "firefox-import" => Some(Self::FirefoxImport),
            "chrome-import" => Some(Self::ChromeImport),
            other => other
                .strip_prefix("custom-")
                .map(|n| Self::Custom { name: n.into() }),
        }
    }
}

/// Keep a-z, 0-9, '-', '_' only; collapse the rest to '-'.
fn sanitize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "unnamed".into()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_is_stable_for_known_variants() {
        assert_eq!(BrowserProfile::Default.slug(), "default");
        assert_eq!(BrowserProfile::FirefoxImport.slug(), "firefox-import");
        assert_eq!(BrowserProfile::ChromeImport.slug(), "chrome-import");
    }

    #[test]
    fn custom_slug_sanitizes_unsafe_characters() {
        let p = BrowserProfile::Custom {
            name: "Work GitHub!".into(),
        };
        assert_eq!(p.slug(), "custom-work-github");
    }

    #[test]
    fn custom_slug_collapses_runs_of_specials() {
        let p = BrowserProfile::Custom {
            name: "a / b // c".into(),
        };
        // runs of "non-allowed" chars collapse to a single '-'
        assert_eq!(p.slug(), "custom-a-b-c");
    }

    #[test]
    fn custom_slug_falls_back_to_unnamed_when_empty() {
        let p = BrowserProfile::Custom {
            name: "  / / /  ".into(),
        };
        assert_eq!(p.slug(), "custom-unnamed");
    }

    #[test]
    fn parse_slug_recovers_known_variants() {
        assert_eq!(
            BrowserProfile::parse_slug("default"),
            Some(BrowserProfile::Default)
        );
        assert_eq!(
            BrowserProfile::parse_slug("firefox-import"),
            Some(BrowserProfile::FirefoxImport)
        );
        assert_eq!(
            BrowserProfile::parse_slug("chrome-import"),
            Some(BrowserProfile::ChromeImport)
        );
        assert_eq!(
            BrowserProfile::parse_slug("custom-work"),
            Some(BrowserProfile::Custom {
                name: "work".into()
            })
        );
    }

    #[test]
    fn parse_slug_rejects_unknown() {
        assert_eq!(BrowserProfile::parse_slug("nope"), None);
    }

    #[test]
    fn data_dir_under_flowmux_browser_subtree() {
        // Use a temp data root so we don't pollute the real profile dir. Linux
        // follows XDG_DATA_HOME, while macOS derives data_dir from HOME.
        let tmp = tempfile::tempdir().unwrap();
        let original = std::env::var_os("XDG_DATA_HOME");
        let original_home = std::env::var_os("HOME");
        std::env::set_var("XDG_DATA_HOME", tmp.path());
        std::env::set_var("HOME", tmp.path());

        let dir = BrowserProfile::Default.data_dir().unwrap();
        assert!(dir.starts_with(tmp.path()));
        assert!(dir.ends_with("flowmux/browser/default"));
        assert!(dir.is_dir());

        match original {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        match original_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn display_name_is_user_facing() {
        assert_eq!(BrowserProfile::Default.display_name(), "Default");
        assert_eq!(
            BrowserProfile::FirefoxImport.display_name(),
            "Firefox import"
        );
        assert_eq!(
            BrowserProfile::Custom {
                name: "Work".into()
            }
            .display_name(),
            "Work"
        );
    }

    #[test]
    fn profile_serde_roundtrips() {
        for p in [
            BrowserProfile::Default,
            BrowserProfile::FirefoxImport,
            BrowserProfile::ChromeImport,
            BrowserProfile::Custom {
                name: "work".into(),
            },
        ] {
            let s = serde_json::to_string(&p).unwrap();
            let back: BrowserProfile = serde_json::from_str(&s).unwrap();
            assert_eq!(p, back);
        }
    }
}
