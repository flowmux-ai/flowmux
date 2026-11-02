// SPDX-License-Identifier: GPL-3.0-or-later
//! User-initiated, site-scoped imports. No source database is ever modified.
use crate::{cookie::SameSite, crypto::Decryptor, source::BrowserId, Cookie};
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug)]
pub struct Profile {
    pub browser: BrowserId,
    pub name: String,
    pub database: PathBuf,
}

pub struct Import {
    pub cookies: Vec<Cookie>,
    pub skipped: usize,
}

pub fn site_host(url: &str) -> Result<String, String> {
    let url =
        url::Url::parse(url).map_err(|_| "Open an HTTP or HTTPS site before importing cookies")?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Open an HTTP or HTTPS site before importing cookies".into());
    }
    url.host_str()
        .map(str::to_owned)
        .ok_or_else(|| "This page has no site hostname".into())
}

pub fn discover_profiles() -> Vec<Profile> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    #[cfg(target_os = "macos")]
    let roots = {
        let base = home.join("Library/Application Support");
        vec![
            (BrowserId::Chrome, base.join("Google/Chrome")),
            (BrowserId::Chromium, base.join("Chromium")),
            (BrowserId::Brave, base.join("BraveSoftware/Brave-Browser")),
            (BrowserId::Edge, base.join("Microsoft Edge")),
            (BrowserId::Arc, base.join("Arc/User Data")),
            (BrowserId::Firefox, base.join("Firefox/Profiles")),
        ]
    };
    #[cfg(not(target_os = "macos"))]
    let roots = {
        let base = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
        vec![
            (BrowserId::Chrome, base.join("google-chrome")),
            (BrowserId::Chromium, base.join("chromium")),
            (BrowserId::Brave, base.join("BraveSoftware/Brave-Browser")),
            (BrowserId::Edge, base.join("microsoft-edge")),
            (BrowserId::Firefox, home.join(".mozilla/firefox")),
            (
                BrowserId::Firefox,
                home.join("snap/firefox/common/.mozilla/firefox"),
            ),
            (
                BrowserId::Chromium,
                home.join("snap/chromium/common/chromium"),
            ),
            (
                BrowserId::Chrome,
                home.join(".var/app/com.google.Chrome/config/google-chrome"),
            ),
            (
                BrowserId::Firefox,
                home.join(".var/app/org.mozilla.firefox/.mozilla/firefox"),
            ),
        ]
    };
    let mut profiles = Vec::new();
    for (browser, root) in roots {
        profiles.extend(profiles_in(browser, &root));
    }
    let mut seen = HashSet::new();
    profiles.retain(|profile| {
        seen.insert(
            profile
                .database
                .canonicalize()
                .unwrap_or_else(|_| profile.database.clone()),
        )
    });
    profiles
}

fn profiles_in(browser: BrowserId, root: &Path) -> Vec<Profile> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let metadata: serde_json::Value = std::fs::read(root.join("Local State"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut profiles = Vec::new();
    for entry in entries.flatten() {
        let directory = entry.path();
        let candidates = if browser == BrowserId::Firefox {
            vec![directory.join("cookies.sqlite")]
        } else {
            vec![directory.join("Network/Cookies"), directory.join("Cookies")]
        };
        let Some(database) = candidates.into_iter().find(|path| path.is_file()) else {
            continue;
        };
        let folder = entry.file_name().to_string_lossy().into_owned();
        let name = metadata["profile"]["info_cache"][&folder]["name"]
            .as_str()
            .map(|name| format!("{name} ({folder})"))
            .unwrap_or(folder);
        profiles.push(Profile {
            browser,
            name: format!("{} — {name}", browser.slug()),
            database,
        });
    }
    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    profiles
}

fn matching_domains(host: &str) -> Vec<String> {
    let mut domains = vec![host.to_string(), format!(".{host}")];
    if host.parse::<std::net::IpAddr>().is_err() && !host.starts_with('[') {
        let mut suffix = host;
        while let Some((_, parent)) = suffix.split_once('.') {
            if !parent.contains('.') {
                break;
            }
            domains.push(format!(".{parent}"));
            suffix = parent;
        }
    }
    domains
}

fn columns(connection: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    connection
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get(1))?
        .collect()
}

/// Call only after the user selected a source and approved the site/profile scope.
/// Keyring implementations can display an OS approval/unlock prompt on this worker.
pub fn read(profile: &Profile, host: &str) -> Result<Import, String> {
    // Use the URL parser again at this public boundary, not a SQL substring filter.
    if site_host(&format!("https://{host}/"))? != host || host.contains(['/', '@', '?', '#']) {
        return Err("Invalid site hostname".into());
    }
    let result = read_database(profile, host);
    result.map_err(|error| format!("Could not import from {}: {error}", profile.browser.slug()))
}

fn read_database(profile: &Profile, host: &str) -> Result<Import, String> {
    let mut connection =
        Connection::open_with_flags(&profile.database, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(
            |_| "Cannot read the selected browser profile. Check its file permissions.".to_string(),
        )?;
    connection
        .busy_timeout(Duration::from_secs(3))
        .map_err(|e| e.to_string())?;
    let transaction = connection.transaction().map_err(|e| e.to_string())?;
    let firefox = profile.browser == BrowserId::Firefox;
    let fields = columns(
        &transaction,
        if firefox { "moz_cookies" } else { "cookies" },
    )
    .map_err(|e| e.to_string())?;
    let version: i64 = if firefox {
        0
    } else {
        transaction
            .query_row("SELECT value FROM meta WHERE key='version'", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|_| "Cannot determine the Chromium cookie database version")?
            .parse()
            .map_err(|_| "Invalid Chromium cookie database version")?
    };
    let partition = if firefox && fields.contains("originAttributes") {
        "originAttributes"
    } else if !firefox && fields.contains("top_frame_site_key") {
        "top_frame_site_key"
    } else {
        "''"
    };
    let select = if firefox {
        format!("SELECT host,name,value,path,expiry,isSecure,isHttpOnly,sameSite,X'',{partition} FROM moz_cookies WHERE lower(host)")
    } else {
        format!("SELECT host_key,name,value,path,expires_utc,is_secure,is_httponly,samesite,encrypted_value,{partition} FROM cookies WHERE lower(host_key)")
    };
    let domains = matching_domains(host);
    let placeholders = vec!["?"; domains.len()].join(",");
    let mut statement = transaction
        .prepare(&format!("{select} IN ({placeholders}) LIMIT 2001"))
        .map_err(|e| e.to_string())?;
    let mut rows = statement
        .query(rusqlite::params_from_iter(&domains))
        .map_err(|e| e.to_string())?;
    let mut decryptor = Decryptor::new(profile.browser);
    let mut imported = Import {
        cookies: Vec::new(),
        skipped: 0,
    };
    let mut count = 0;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        count += 1;
        if count > 2000 {
            return Err("Too many cookies for this site; nothing was imported".into());
        }
        let partition: String = row.get(9).map_err(|e| e.to_string())?;
        let expires: i64 = row.get(4).map_err(|e| e.to_string())?;
        let expires_at = if expires == 0 {
            None
        } else {
            let seconds = if firefox {
                expires
            } else {
                expires / 1_000_000 - 11_644_473_600
            };
            Some(chrono::DateTime::from_timestamp(seconds, 0).ok_or("Invalid cookie expiry")?)
        };
        // WebKit cannot preserve Chromium partition keys or Firefox containers.
        if !partition.is_empty() || expires_at.is_some_and(|time| time <= chrono::Utc::now()) {
            imported.skipped += 1;
            continue;
        }
        let host: String = row.get(0).map_err(|e| e.to_string())?;
        let encrypted: Vec<u8> = row.get(8).map_err(|e| e.to_string())?;
        let value = if encrypted.is_empty() {
            row.get(2).map_err(|e| e.to_string())?
        } else {
            decryptor.decrypt(&encrypted, &host, version)?
        };
        let same_site: i64 = row.get(7).map_err(|e| e.to_string())?;
        let same_site = match same_site {
            0 => SameSite::None,
            1 => SameSite::Lax,
            2 => SameSite::Strict,
            -1 => SameSite::Lax,
            _ => {
                imported.skipped += 1;
                continue;
            }
        };
        let cookie = Cookie {
            host,
            name: row.get(1).map_err(|e| e.to_string())?,
            value,
            path: row.get(3).map_err(|e| e.to_string())?,
            expires_at,
            secure: row.get(5).map_err(|e| e.to_string())?,
            http_only: row.get(6).map_err(|e| e.to_string())?,
            same_site,
        };
        cookie_header(&cookie)?;
        imported.cookies.push(cookie);
    }
    Ok(imported)
}

/// Shared native cookie parsing input; never use document.cookie for HttpOnly data.
pub fn cookie_header(cookie: &Cookie) -> Result<String, String> {
    if cookie.name.is_empty()
        || cookie
            .name
            .bytes()
            .any(|c| !c.is_ascii_graphic() || b"()<>@,;:\\\"/[]?={}".contains(&c))
        || [&cookie.value, &cookie.path, &cookie.host]
            .iter()
            .any(|value| value.len() > 65536 || value.chars().any(|c| c.is_control() || c == ';'))
        || !cookie.path.starts_with('/')
    {
        return Err("Invalid cookie data; nothing was imported".into());
    }
    let mut header = format!("{}={}; Path={}", cookie.name, cookie.value, cookie.path);
    if cookie.host.starts_with('.') {
        header.push_str(&format!("; Domain={}", cookie.host));
    }
    if cookie.secure {
        header.push_str("; Secure");
    }
    if cookie.http_only {
        header.push_str("; HttpOnly");
    }
    header.push_str(match cookie.same_site {
        SameSite::Strict => "; SameSite=Strict",
        SameSite::Lax => "; SameSite=Lax",
        SameSite::None | SameSite::NoRestriction => "; SameSite=None",
    });
    if let Some(expires) = cookie.expires_at {
        header.push_str(&format!(
            "; Expires={}",
            expires.format("%a, %d %b %Y %H:%M:%S GMT")
        ));
    }
    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_rejects_non_web_urls_and_matches_only_host_and_parent_domains() {
        assert_eq!(
            site_host("https://APP.example.com/path").unwrap(),
            "app.example.com"
        );
        assert!(site_host("file:///tmp/example.com").is_err());
        assert!(site_host("about:blank").is_err());
        assert_eq!(
            matching_domains("app.example.com"),
            ["app.example.com", ".app.example.com", ".example.com"]
        );
        assert_eq!(matching_domains("127.0.0.1"), ["127.0.0.1", ".127.0.0.1"]);
    }

    #[test]
    fn imports_only_the_selected_site_without_containers_or_expired_cookies() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("cookies.sqlite");
        let writer = Connection::open(&database).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE moz_cookies(host TEXT,name TEXT,value TEXT,path TEXT,expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,originAttributes TEXT);
            INSERT INTO moz_cookies VALUES('.example.test','sid','fixture','/',0,1,1,1,''),
            ('app.example.test','host','fixture','/',0,1,0,2,''),
            ('example.test','parent_host_only','private','/',0,1,0,1,''),
            ('.notexample.test','other','private','/',0,1,0,1,''),
            ('.example.test','container','private','/',0,1,0,1,'^userContextId=1'),
            ('.example.test','expired','private','/',1,1,0,1,'');").unwrap();
        let profile = Profile {
            browser: BrowserId::Firefox,
            name: "fixture".into(),
            database,
        };
        let imported = read(&profile, "app.example.test").unwrap();
        assert_eq!(imported.cookies.len(), 2);
        assert_eq!(imported.skipped, 2);
        assert!(imported.cookies[0].http_only);
        assert!(cookie_header(&imported.cookies[0])
            .unwrap()
            .contains("Domain=.example.test"));
        assert!(!cookie_header(&imported.cookies[1])
            .unwrap()
            .contains("Domain="));
        assert_eq!(
            writer
                .query_row("SELECT count(*) FROM moz_cookies", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            6
        );
    }

    #[test]
    fn discovers_multiple_chromium_profiles_and_modern_cookie_path() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("Default/Network")).unwrap();
        std::fs::create_dir_all(temp.path().join("Profile 1")).unwrap();
        std::fs::write(temp.path().join("Default/Network/Cookies"), []).unwrap();
        std::fs::write(temp.path().join("Profile 1/Cookies"), []).unwrap();
        std::fs::write(
            temp.path().join("Local State"),
            r#"{"profile":{"info_cache":{"Default":{"name":"Work"}}}}"#,
        )
        .unwrap();
        let profiles = profiles_in(BrowserId::Chrome, temp.path());
        assert_eq!(profiles.len(), 2);
        assert!(profiles
            .iter()
            .any(|p| p.name.contains("Work") && p.database.ends_with("Network/Cookies")));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn chromium_reads_encrypted_wal_rows_and_keeps_partitioned_cookies_isolated() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
        use sha2::{Digest, Sha256};
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("Cookies");
        let writer = Connection::open(&database).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE meta(key TEXT,value TEXT); INSERT INTO meta VALUES('version','24');
            CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,encrypted_value BLOB,top_frame_site_key TEXT);").unwrap();
        let mut key = [0; 16];
        pbkdf2::pbkdf2_hmac::<sha1::Sha1>(b"peanuts", b"saltysalt", 1, &mut key);
        let mut plaintext = Sha256::digest(b".example.test").to_vec();
        plaintext.extend_from_slice(b"fixture-session");
        let mut encrypted = b"v10".to_vec();
        encrypted.extend(
            cbc::Encryptor::<aes::Aes128>::new((&key).into(), (&[b' '; 16]).into())
                .encrypt_padded_vec_mut::<Pkcs7>(&plaintext),
        );
        for (name, partition) in [("sid", ""), ("partitioned", "https://other.test")] {
            writer
                .execute(
                    "INSERT INTO cookies VALUES('.example.test',?1,'','/',0,1,1,2,?2,?3)",
                    rusqlite::params![name, encrypted, partition],
                )
                .unwrap();
        }
        let profile = Profile {
            browser: BrowserId::Chrome,
            name: "fixture".into(),
            database,
        };
        let imported = read(&profile, "app.example.test").unwrap();
        assert_eq!(imported.cookies.len(), 1);
        assert_eq!(imported.skipped, 1);
        assert_eq!(imported.cookies[0].value, "fixture-session");
        assert_eq!(imported.cookies[0].same_site, SameSite::Strict);
        writer
            .execute("UPDATE cookies SET host_key='.other.test'", [])
            .unwrap();
        assert!(
            matches!(read(&profile, "other.test"), Err(error) if error.contains("domain verification failed"))
        );
    }

    #[test]
    fn rejects_header_injection_before_any_native_insertion() {
        let mut cookie = Cookie {
            host: "example.test".into(),
            name: "sid".into(),
            value: "safe".into(),
            path: "/".into(),
            expires_at: None,
            secure: true,
            http_only: true,
            same_site: SameSite::Strict,
        };
        assert!(cookie_header(&cookie).is_ok());
        cookie.value = "value; Domain=other.test".into();
        assert!(cookie_header(&cookie).is_err());
        cookie.value = "value\r\nSet-Cookie: other=secret".into();
        assert!(cookie_header(&cookie).is_err());
    }
}
