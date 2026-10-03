// SPDX-License-Identifier: GPL-3.0-or-later
//! Review drafts are independent of terminal/agent sessions and repository files.
use flowmux_vcs::review::notes::Note;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Draft {
    pub revision: u64,
    pub notes: Vec<Note>,
}

#[derive(Clone)]
pub struct DraftStore {
    path: PathBuf,
}

impl DraftStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
    pub fn default_store() -> Result<Self, String> {
        flowmux_config::paths::state_dir()
            .map(|p| Self::new(p.join("reviews.sqlite3")))
            .ok_or_else(|| "Review storage directory is unavailable.".into())
    }
    fn connection(&self) -> Result<Connection, String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let connection = Connection::open(&self.path).map_err(|e| e.to_string())?;
        connection
            .busy_timeout(Duration::from_secs(3))
            .map_err(|e| e.to_string())?;
        connection.execute_batch("CREATE TABLE IF NOT EXISTS review_drafts (root BLOB PRIMARY KEY, revision INTEGER NOT NULL, data TEXT NOT NULL);").map_err(|e| e.to_string())?;
        Ok(connection)
    }
    pub fn load(&self, root: &Path) -> Result<Draft, String> {
        let connection = self.connection()?;
        let row: Option<(u64, String)> = connection
            .query_row(
                "SELECT revision, data FROM review_drafts WHERE root = ?1",
                [root.as_os_str().as_bytes()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match row {
            Some((revision, data)) => Ok(Draft {
                revision,
                notes: serde_json::from_str(&data)
                    .map_err(|e| format!("Cannot read saved review: {e}"))?,
            }),
            None => Ok(Draft::default()),
        }
    }
    /// Optimistic revision checking preserves reviews edited in another window.
    pub fn save(&self, root: &Path, draft: &Draft) -> Result<Draft, String> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|e| e.to_string())?;
        let revision: Option<u64> = transaction
            .query_row(
                "SELECT revision FROM review_drafts WHERE root = ?1",
                [root.as_os_str().as_bytes()],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if revision.unwrap_or(0) != draft.revision {
            return Err(
                "This review changed in another window. Reload saved comments before editing."
                    .into(),
            );
        }
        let mut saved = draft.clone();
        saved.revision += 1;
        let data = serde_json::to_string(&saved.notes).map_err(|e| e.to_string())?;
        transaction.execute("INSERT INTO review_drafts(root, revision, data) VALUES (?1, ?2, ?3) ON CONFLICT(root) DO UPDATE SET revision=excluded.revision, data=excluded.data", params![root.as_os_str().as_bytes(), saved.revision, data]).map_err(|e| e.to_string())?;
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_drafts_roundtrip_conflict_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let store = DraftStore::new(dir.path().join("reviews.sqlite3"));
        let root = dir.path().join("project 한글");
        let draft = store.load(&root).unwrap();
        let saved = store.save(&root, &draft).unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(store.load(&root).unwrap(), saved);
        assert!(store
            .save(&root, &draft)
            .unwrap_err()
            .contains("another window"));
        assert_eq!(
            store.load(&dir.path().join("other")).unwrap(),
            Draft::default()
        );
        store
            .connection()
            .unwrap()
            .execute("UPDATE review_drafts SET data='invalid'", [])
            .unwrap();
        assert!(store.load(&root).is_err());
    }
}
