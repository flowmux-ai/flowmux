// SPDX-License-Identifier: GPL-3.0-or-later
use crate::{
    profiles::{database, Profile},
    source::{BrowserId, Error},
};
use flowmux_browser::Bookmark;
use std::io::Read;

pub fn read(profile: &Profile) -> Result<Vec<Bookmark>, Error> {
    let mut bookmarks = Vec::new();
    if profile.browser == BrowserId::Firefox {
        let db = database(&profile.path.join("places.sqlite"))?;
        let mut statement = db.prepare("SELECT COALESCE(b.title, p.title, p.url), p.url FROM moz_bookmarks b JOIN moz_places p ON p.id = b.fk WHERE b.type = 1 ORDER BY b.id")?;
        bookmarks = statement
            .query_map([], |row| {
                Ok(Bookmark {
                    title: row.get(0)?,
                    url: row.get(1)?,
                })
            })?
            .collect::<Result<_, _>>()?;
    } else {
        let mut data = Vec::new();
        std::fs::File::open(profile.path.join("Bookmarks"))?
            .take(32 * 1024 * 1024 + 1)
            .read_to_end(&mut data)?;
        if data.len() > 32 * 1024 * 1024 {
            return Err(Error::Import("bookmark file exceeds 32 MiB".into()));
        }
        let value: serde_json::Value = serde_json::from_slice(&data)
            .map_err(|_| Error::Import("invalid Chromium bookmarks".into()))?;
        let roots = value
            .get("roots")
            .and_then(|v| v.as_object())
            .ok_or_else(|| Error::Import("missing bookmark roots".into()))?;
        for root in roots.values() {
            visit(root, &mut bookmarks)?;
        }
    }
    // Browser-internal and executable URLs cannot be used safely in another engine.
    bookmarks.retain(|b| b.url.starts_with("https://") || b.url.starts_with("http://"));
    Ok(bookmarks)
}

fn visit(node: &serde_json::Value, out: &mut Vec<Bookmark>) -> Result<(), Error> {
    if node["type"] == "url" {
        let url = node["url"]
            .as_str()
            .ok_or_else(|| Error::Import("bookmark URL is missing".into()))?;
        let title = node["name"].as_str().unwrap_or(url);
        out.push(Bookmark {
            title: title.into(),
            url: url.into(),
        });
    }
    if let Some(children) = node.get("children") {
        let children = children
            .as_array()
            .ok_or_else(|| Error::Import("invalid bookmark folder".into()))?;
        for child in children {
            visit(child, out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chromium_folders_unicode_and_safe_urls() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Bookmarks"), r#"{"roots":{"bookmark_bar":{"type":"folder","children":[{"type":"url","name":"한글","url":"https://example.com/"},{"type":"folder","children":[{"type":"url","url":"http://localhost/"},{"type":"url","url":"javascript:alert(1)"}]}]}}}"#).unwrap();
        let profile = Profile {
            browser: BrowserId::Chrome,
            name: "test".into(),
            path: dir.path().into(),
        };
        let bookmarks = read(&profile).unwrap();
        assert_eq!(bookmarks.len(), 2);
        assert_eq!(bookmarks[0].title, "한글");
        std::fs::write(dir.path().join("Bookmarks"), "invalid").unwrap();
        assert!(read(&profile).is_err());
    }
    #[test]
    fn firefox_reads_only_bookmarks_and_keeps_database_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("places.sqlite");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE moz_places(id INTEGER, title TEXT, url TEXT); CREATE TABLE moz_bookmarks(id INTEGER, fk INTEGER, title TEXT, type INTEGER); INSERT INTO moz_places VALUES(1,'Page','https://example.com'),(2,'History only','https://other.test'); INSERT INTO moz_bookmarks VALUES(1,1,'Saved',1);").unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        let rows = read(&Profile {
            browser: BrowserId::Firefox,
            name: "test".into(),
            path: dir.path().into(),
        })
        .unwrap();
        assert_eq!(
            rows,
            vec![Bookmark {
                title: "Saved".into(),
                url: "https://example.com".into()
            }]
        );
        assert_eq!(before, std::fs::read(path).unwrap());
    }
}
