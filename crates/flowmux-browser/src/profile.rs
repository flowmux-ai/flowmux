// SPDX-License-Identifier: GPL-3.0-or-later
//! Persistent browser profile identifiers. Each slug has its own data directory.
//! Selecting a profile does not import host-browser cookies.

use serde::{Deserialize, Serialize};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
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
            BrowserProfile::Custom { name } => format!("custom-{}", encode_name(name)),
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
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)?;
        if !std::fs::symlink_metadata(&dir)?.is_dir() {
            return Err(ProfileError::InvalidName(
                "profile storage must be a real directory".into(),
            ));
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        Ok(dir)
    }

    pub fn parse_slug(s: &str) -> Option<Self> {
        match s {
            "default" => Some(Self::Default),
            "firefox-import" => Some(Self::FirefoxImport),
            "chrome-import" => Some(Self::ChromeImport),
            other => other
                .strip_prefix("custom-")
                .and_then(decode_name)
                .map(|name| Self::Custom { name }),
        }
    }
}

// Preserve canonical legacy keys; escape every other UTF-8 byte without collisions.
fn encode_name(name: &str) -> String {
    if name.is_empty() {
        return "~".into();
    }
    let mut out = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte) {
            out.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(out, "~{byte:02x}").unwrap();
        }
    }
    out
}

fn decode_name(encoded: &str) -> Option<String> {
    if encoded == "~" {
        return Some(String::new());
    }
    let mut bytes = Vec::new();
    let mut input = encoded.bytes();
    while let Some(byte) = input.next() {
        if byte == b'~' {
            let high = char::from(input.next()?).to_digit(16)?;
            let low = char::from(input.next()?).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else if byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"-_".contains(&byte) {
            bytes.push(byte);
        } else {
            return None;
        }
    }
    let name = String::from_utf8(bytes).ok()?;
    (encode_name(&name) == encoded).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_names_are_distinct_and_roundtrip() {
        let names = [
            "업무", "개인", "Work", "work", "a b", "a-b", "~", "", "unnamed", "../x",
        ];
        let mut slugs = std::collections::HashSet::new();
        for name in names {
            let profile = BrowserProfile::Custom { name: name.into() };
            let slug = profile.slug();
            assert!(slugs.insert(slug.clone()), "collision for {name}");
            assert_eq!(BrowserProfile::parse_slug(&slug), Some(profile));
            assert!(!slug.contains('/'));
        }
    }

    #[test]
    fn slug_is_stable_for_known_variants() {
        assert_eq!(BrowserProfile::Default.slug(), "default");
        assert_eq!(BrowserProfile::FirefoxImport.slug(), "firefox-import");
        assert_eq!(BrowserProfile::ChromeImport.slug(), "chrome-import");
    }

    #[test]
    fn canonical_legacy_keys_are_preserved() {
        assert_eq!(
            BrowserProfile::Custom {
                name: "work-123_test".into()
            }
            .slug(),
            "custom-work-123_test"
        );
        for malformed in ["custom-~gg", "custom-~ff", "custom-~61", "custom-../x"] {
            assert!(BrowserProfile::parse_slug(malformed).is_none());
        }
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
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir(&dir).unwrap();
        let other = tmp.path().join("other-account");
        std::fs::create_dir(&other).unwrap();
        std::os::unix::fs::symlink(&other, &dir).unwrap();
        assert!(BrowserProfile::Default.data_dir().is_err());

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
