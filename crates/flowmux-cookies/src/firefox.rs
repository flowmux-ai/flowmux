// SPDX-License-Identifier: GPL-3.0-or-later
//! Firefox stores cookies at:
//!
//!   `~/.mozilla/firefox/<profile>/cookies.sqlite`
//!
//! Schema (firefox 100+):
//!
//!   CREATE TABLE moz_cookies (
//!     id INTEGER PRIMARY KEY,
//!     originAttributes TEXT NOT NULL,
//!     name TEXT, value TEXT, host TEXT, path TEXT,
//!     expiry INTEGER, lastAccessed INTEGER, creationTime INTEGER,
//!     isSecure INTEGER, isHttpOnly INTEGER,
//!     inBrowserElement INTEGER, sameSite INTEGER, ...
//!   );
//!
//! Values are plaintext.

use crate::cookie::{Cookie, SameSite};
use crate::source::{BrowserId, Error, Source};
use std::path::{Path, PathBuf};

pub struct Firefox;

impl Firefox {
    pub fn new() -> Self {
        Self
    }
}

impl Default for Firefox {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for Firefox {
    fn id(&self) -> BrowserId {
        BrowserId::Firefox
    }

    fn detect(&self) -> Option<PathBuf> {
        crate::profiles::discover()
            .ok()?
            .iter()
            .filter(|p| p.browser == BrowserId::Firefox)
            .map(crate::profiles::cookie_path)
            .find(|p| p.is_file())
    }

    fn list_cookies(&self, domain_filter: Option<&str>) -> Result<Vec<Cookie>, Error> {
        let profile = crate::profiles::resolve(BrowserId::Firefox, None)?;
        list_cookies_from_path(&crate::profiles::cookie_path(&profile), domain_filter)
    }
}

pub(crate) fn list_cookies_from_path(
    path: &Path,
    domain_filter: Option<&str>,
) -> Result<Vec<Cookie>, Error> {
    Ok(read_batch(path, domain_filter)?.cookies)
}

pub(crate) fn read_batch(
    path: &Path,
    domain_filter: Option<&str>,
) -> Result<crate::profiles::CookieBatch, Error> {
    let conn = crate::profiles::database(path)?;
    let mut columns = conn.prepare("PRAGMA table_info(moz_cookies)")?;
    let columns = columns
        .query_map([], |r| r.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    let attributes = if columns.iter().any(|c| c == "originAttributes") {
        "originAttributes"
    } else {
        "''"
    };
    let sql = format!("SELECT host, name, value, path, expiry, isSecure, isHttpOnly, sameSite, {attributes} FROM moz_cookies");
    let mut stmt = conn.prepare(&sql)?;
    let map = |row: &rusqlite::Row<'_>| -> rusqlite::Result<(Cookie, String)> {
        let expiry: i64 = row.get(4)?;
        let same_site: i64 = row.get(7)?;
        Ok((
            Cookie {
                host: row.get(0)?,
                name: row.get(1)?,
                value: row.get(2)?,
                path: row.get(3)?,
                expires_at: if expiry > 0 {
                    Some(chrono::DateTime::from_timestamp(expiry, 0).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Integer,
                            "cookie expiration is out of range".into(),
                        )
                    })?)
                } else {
                    None
                },
                secure: row.get::<_, i64>(5)? != 0,
                http_only: row.get::<_, i64>(6)? != 0,
                same_site: match same_site {
                    1 => SameSite::Lax,
                    2 => SameSite::Strict,
                    0 => SameSite::None,
                    _ => SameSite::NoRestriction,
                },
            },
            row.get(8)?,
        ))
    };
    let rows = stmt.query_map([], map)?.collect::<Result<Vec<_>, _>>()?;
    let mut batch = crate::profiles::CookieBatch::default();
    for (cookie, attributes) in rows {
        if !crate::profiles::domain_matches(&cookie.host, domain_filter) {
            continue;
        }
        if !attributes.is_empty() {
            batch.skipped_isolated += 1;
            continue;
        }
        batch.cookies.push(cookie);
    }
    Ok(batch)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn fixture_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_cookies (
                host TEXT,
                name TEXT,
                value TEXT,
                path TEXT,
                expiry INTEGER,
                isSecure INTEGER,
                isHttpOnly INTEGER,
                sameSite INTEGER
            );",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_cookies VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                ".example.com",
                "sid",
                "abc",
                "/",
                1_700_000_000_i64,
                1_i64,
                0_i64,
                1_i64,
            ),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO moz_cookies VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                ".other.test",
                "pref",
                "dark",
                "/ui",
                0_i64,
                0_i64,
                1_i64,
                2_i64,
            ),
        )
        .unwrap();
    }

    #[test]
    fn reads_firefox_cookies_from_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cookies.sqlite");
        fixture_db(&db);

        let cookies = list_cookies_from_path(&db, None).unwrap();
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].host, ".example.com");
        assert_eq!(cookies[0].name, "sid");
        assert_eq!(cookies[0].value, "abc");
        assert_eq!(cookies[0].path, "/");
        assert!(cookies[0].expires_at.is_some());
        assert!(cookies[0].secure);
        assert!(!cookies[0].http_only);
        assert_eq!(cookies[0].same_site, SameSite::Lax);
    }

    #[test]
    fn filters_firefox_cookies_by_domain() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("cookies.sqlite");
        fixture_db(&db);

        let cookies = list_cookies_from_path(&db, Some("other.test")).unwrap();
        assert_eq!(cookies.len(), 1);
        assert_eq!(cookies[0].host, ".other.test");
        assert_eq!(cookies[0].same_site, SameSite::Strict);
        assert_eq!(cookies[0].expires_at, None);
        assert!(!cookies[0].secure);
        assert!(cookies[0].http_only);
    }
}
