// SPDX-License-Identifier: GPL-3.0-or-later
use crate::source::{BrowserId, Error};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize)]
pub struct Profile {
    pub browser: BrowserId,
    pub name: String,
    pub path: PathBuf,
}

fn base(browser: BrowserId, home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    let path = match browser {
        BrowserId::Chrome => "Library/Application Support/Google/Chrome",
        BrowserId::Chromium => "Library/Application Support/Chromium",
        BrowserId::Brave => "Library/Application Support/BraveSoftware/Brave-Browser",
        BrowserId::Edge => "Library/Application Support/Microsoft Edge",
        BrowserId::Arc => "Library/Application Support/Arc/User Data",
        BrowserId::Firefox => "Library/Application Support/Firefox/Profiles",
    };
    #[cfg(not(target_os = "macos"))]
    let path = match browser {
        BrowserId::Chrome => ".config/google-chrome",
        BrowserId::Chromium => ".config/chromium",
        BrowserId::Brave => ".config/BraveSoftware/Brave-Browser",
        BrowserId::Edge => ".config/microsoft-edge",
        BrowserId::Arc => ".config/arc/User Data",
        BrowserId::Firefox => ".mozilla/firefox",
    };
    home.join(path)
}

pub fn discover() -> Result<Vec<Profile>, Error> {
    discover_in(&dirs::home_dir().ok_or(Error::NoHome)?)
}

fn discover_in(home: &Path) -> Result<Vec<Profile>, Error> {
    let mut profiles = Vec::new();
    for source in crate::discover_sources() {
        let browser = source.id();
        let base = base(browser, home);
        let entries = match std::fs::read_dir(&base) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let path = entry?.path();
            let files: &[&str] = if browser == BrowserId::Firefox {
                &["cookies.sqlite", "places.sqlite"]
            } else {
                &["Cookies", "Network/Cookies", "Bookmarks"]
            };
            if files.iter().any(|file| path.join(file).is_file()) {
                profiles.push(Profile {
                    browser,
                    name: path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                    path,
                });
            }
        }
    }
    profiles.sort_by(|a, b| (a.browser.slug(), &a.name).cmp(&(b.browser.slug(), &b.name)));
    Ok(profiles)
}

pub fn resolve(browser: BrowserId, path: Option<&Path>) -> Result<Profile, Error> {
    if let Some(path) = path {
        let path = path.canonicalize()?;
        if !path.is_dir() {
            return Err(Error::Import("source profile must be a directory".into()));
        }
        return Ok(Profile {
            browser,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path,
        });
    }
    let mut profiles: Vec<_> = discover()?
        .into_iter()
        .filter(|p| p.browser == browser)
        .collect();
    if profiles.len() != 1 {
        return Err(Error::Import(format!(
            "found {} {} profiles; select a source profile explicitly",
            profiles.len(),
            browser.slug()
        )));
    }
    Ok(profiles.remove(0))
}

pub fn cookie_path(profile: &Profile) -> PathBuf {
    if profile.browser == BrowserId::Firefox {
        profile.path.join("cookies.sqlite")
    } else if profile.path.join("Network/Cookies").is_file() {
        profile.path.join("Network/Cookies")
    } else {
        profile.path.join("Cookies")
    }
}

pub(crate) fn database(path: &Path) -> Result<rusqlite::Connection, Error> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    Ok(db)
}

#[derive(Default)]
pub struct CookieBatch {
    pub cookies: Vec<crate::Cookie>,
    pub skipped_isolated: usize,
    pub skipped_expired: usize,
}

pub fn cookies(profile: &Profile, domain: Option<&str>) -> Result<CookieBatch, Error> {
    let mut batch = if profile.browser == BrowserId::Firefox {
        crate::firefox::read_batch(&cookie_path(profile), domain)?
    } else {
        crate::chromium::read_cookies(profile, domain)?
    };
    batch.cookies.retain(|cookie| {
        if cookie
            .expires_at
            .is_some_and(|date| date <= chrono::Utc::now())
        {
            batch.skipped_expired += 1;
            false
        } else {
            true
        }
    });
    for cookie in &batch.cookies {
        cookie.validate().map_err(Error::Import)?;
    }
    Ok(batch)
}

pub(crate) fn domain_matches(host: &str, domain: Option<&str>) -> bool {
    let Some(domain) = domain else { return true };
    let domain = domain.trim_start_matches('.').to_ascii_lowercase();
    let host = host.trim_start_matches('.').to_ascii_lowercase();
    !domain.is_empty()
        && (host == domain
            || host
                .strip_suffix(&domain)
                .is_some_and(|prefix| prefix.ends_with('.')))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_lists_multiple_profiles_without_reading_secrets() {
        let home = tempfile::tempdir().unwrap();
        for name in ["Default", "Profile 1"] {
            let path = base(BrowserId::Chrome, home.path()).join(name);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("Bookmarks"), "not parsed during discovery").unwrap();
        }
        let profiles = discover_in(home.path()).unwrap();
        assert_eq!(profiles.len(), 2);
        assert_eq!(profiles[0].name, "Default");
        assert_eq!(profiles[1].name, "Profile 1");
    }
    #[test]
    fn domain_filter_has_label_boundaries() {
        assert!(domain_matches(".sub.Example.com", Some("example.com")));
        assert!(!domain_matches("notexample.com", Some("example.com")));
        assert!(!domain_matches(
            "example.com.evil.test",
            Some("example.com")
        ));
        assert!(!domain_matches("example.com", Some("%")));
    }
}
