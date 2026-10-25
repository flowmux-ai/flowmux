// SPDX-License-Identifier: GPL-3.0-or-later

use crate::BrowserProfile;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub title: String,
    pub url: String,
}

#[derive(Clone, Debug)]
pub struct BookmarkRepository {
    path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum BookmarkError {
    #[error("browser data directory unavailable")]
    NoDataDir,
    #[error("bookmark I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("bookmark data is invalid: {0}")]
    Invalid(#[from] serde_json::Error),
}

impl BookmarkRepository {
    pub fn for_profile(profile: &BrowserProfile) -> Result<Self, BookmarkError> {
        let base = dirs::data_dir().ok_or(BookmarkError::NoDataDir)?;
        Ok(Self {
            path: base
                .join("flowmux")
                .join("browser")
                .join(profile.slug())
                .join("bookmarks.json"),
        })
    }

    #[cfg(test)]
    fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn load(&self) -> Result<Vec<Bookmark>, BookmarkError> {
        match std::fs::read(&self.path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
    }

    pub fn add(&self, bookmark: Bookmark) -> Result<Vec<Bookmark>, BookmarkError> {
        let _write = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let mut bookmarks = self.load()?;
        bookmarks.retain(|existing| existing.url != bookmark.url);
        bookmarks.insert(0, bookmark);
        self.save(&bookmarks)?;
        Ok(bookmarks)
    }

    pub fn remove(&self, url: &str) -> Result<Vec<Bookmark>, BookmarkError> {
        let _write = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let mut bookmarks = self.load()?;
        bookmarks.retain(|bookmark| bookmark.url != url);
        self.save(&bookmarks)?;
        Ok(bookmarks)
    }

    /// Preserve existing titles/order and add each new URL only once.
    pub fn merge(&self, imported: Vec<Bookmark>) -> Result<usize, BookmarkError> {
        let _write = WRITES.lock().unwrap_or_else(|e| e.into_inner());
        let mut bookmarks = self.load()?;
        let mut urls: std::collections::HashSet<_> =
            bookmarks.iter().map(|b| b.url.clone()).collect();
        let before = bookmarks.len();
        for bookmark in imported {
            if urls.insert(bookmark.url.clone()) {
                bookmarks.push(bookmark);
            }
        }
        let added = bookmarks.len() - before;
        if added > 0 {
            self.save(&bookmarks)?;
        }
        Ok(added)
    }

    fn save(&self, bookmarks: &[Bookmark]) -> Result<(), BookmarkError> {
        let parent = self.path.parent().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "bookmark path has no parent",
            )
        })?;
        std::fs::create_dir_all(parent)?;
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = parent.join(format!(".bookmarks-{}-{suffix}.tmp", std::process::id()));
        std::fs::write(&temporary, serde_json::to_vec_pretty(bookmarks)?)?;
        if let Err(error) = std::fs::rename(&temporary, &self.path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_preserves_existing_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let repository = BookmarkRepository::from_path(dir.path().join("bookmarks.json"));
        let existing = Bookmark {
            title: "Keep title".into(),
            url: "https://a.test".into(),
        };
        repository.add(existing.clone()).unwrap();
        let imported = vec![
            Bookmark {
                title: "New title".into(),
                url: existing.url.clone(),
            },
            Bookmark {
                title: "New".into(),
                url: "https://b.test".into(),
            },
        ];
        assert_eq!(repository.merge(imported.clone()).unwrap(), 1);
        assert_eq!(repository.merge(imported).unwrap(), 0);
        assert_eq!(repository.load().unwrap()[0], existing);
        std::fs::write(&repository.path, "broken").unwrap();
        assert!(repository.merge(vec![]).is_err());
        assert_eq!(std::fs::read_to_string(&repository.path).unwrap(), "broken");
    }

    #[test]
    fn add_updates_duplicate_and_remove_persists() {
        let directory = tempfile::tempdir().unwrap();
        let repository = BookmarkRepository::from_path(directory.path().join("bookmarks.json"));

        repository
            .add(Bookmark {
                title: "First".into(),
                url: "https://example.com".into(),
            })
            .unwrap();
        repository
            .add(Bookmark {
                title: "Updated".into(),
                url: "https://example.com".into(),
            })
            .unwrap();
        repository
            .add(Bookmark {
                title: "Other".into(),
                url: "https://example.org".into(),
            })
            .unwrap();

        assert_eq!(
            repository.load().unwrap(),
            vec![
                Bookmark {
                    title: "Other".into(),
                    url: "https://example.org".into(),
                },
                Bookmark {
                    title: "Updated".into(),
                    url: "https://example.com".into(),
                }
            ]
        );
        repository.remove("https://example.org").unwrap();
        assert_eq!(repository.load().unwrap().len(), 1);
    }

    #[test]
    fn malformed_bookmark_file_is_reported_without_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bookmarks.json");
        std::fs::write(&path, "not-json").unwrap();
        let repository = BookmarkRepository::from_path(path.clone());

        assert!(matches!(repository.load(), Err(BookmarkError::Invalid(_))));
        assert!(repository
            .add(Bookmark {
                title: "New".into(),
                url: "https://example.com".into(),
            })
            .is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "not-json");
    }
}
