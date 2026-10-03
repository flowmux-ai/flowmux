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
    /// Refresh anchors and remove obsolete comments only after a successful
    /// Git read. Revision checking prevents overwriting another window's edits.
    pub fn refresh(&self, root: &Path, draft: &Draft) -> Result<Draft, String> {
        let mut current = draft.clone();
        for note in &mut current.notes {
            note.scope = flowmux_vcs::review::Scope::WorkingTree;
            note.resolved = false;
        }
        let (notes, stale) = flowmux_vcs::review::notes::refresh(root, &current.notes)?;
        current.notes = notes;
        current.notes.retain(|note| !stale.contains(&note.id));
        if current != *draft {
            self.save(root, &current)
        } else {
            if self.load(root)?.revision != draft.revision {
                return Err(
                    "This review changed in another window. Reload saved comments before editing."
                        .into(),
                );
            }
            Ok(current)
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
    fn refresh_removes_obsolete_notes_but_preserves_moved_code_and_failed_reads() {
        use flowmux_vcs::review::{self, Scope};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        assert!(std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&root)
            .output()
            .unwrap()
            .status
            .success());
        let store = DraftStore::new(dir.path().join("reviews.sqlite3"));
        for (file, value) in [
            ("changed", "old code\n"),
            ("removed", "gone\n"),
            ("moved", "keep code\n"),
        ] {
            std::fs::write(root.join(file), value).unwrap();
        }
        let snapshot = review::load(&root, Scope::WorkingTree).unwrap();
        let mut draft = Draft::default();
        for file in &snapshot.files {
            let patch = snapshot.patch(file).unwrap();
            let row = patch
                .lines
                .iter()
                .position(|line| line.new == Some(1))
                .unwrap();
            draft.notes.push(
                Note::new(
                    file.label(),
                    &snapshot,
                    file,
                    &patch,
                    Some(row..row + 1),
                    "Keep this feedback",
                )
                .unwrap(),
            );
        }
        // Older saved comments lack structured bounds and the reattach flag.
        let mut legacy = serde_json::to_value(&draft.notes).unwrap();
        for note in legacy.as_array_mut().unwrap() {
            let note = note.as_object_mut().unwrap();
            note.remove("old_lines");
            note.remove("new_lines");
            note.remove("needs_reattach");
        }
        draft.notes = serde_json::from_value(legacy).unwrap();
        assert!(draft.notes.iter().all(|n| n.new_lines.is_none()));
        let saved = store.save(&root, &draft).unwrap();
        std::fs::write(root.join("changed"), "different code\n").unwrap();
        std::fs::remove_file(root.join("removed")).unwrap();
        std::fs::write(root.join("moved"), "inserted\n    keep code\n").unwrap();
        let current = store.refresh(&root, &saved).unwrap();
        assert_eq!(current.notes.len(), 1);
        assert_eq!(current.notes[0].id, "moved");
        assert_eq!(current.notes[0].location, "new lines 2–2");
        assert_eq!(current.notes[0].new_lines, Some((2, 2)));
        assert_eq!(current.notes[0].excerpt, "+    keep code\n");
        assert_eq!(store.load(&root).unwrap(), current);
        assert_eq!(
            store.refresh(&root, &current).unwrap(),
            current,
            "unchanged refresh must not write"
        );
        assert!(
            store.refresh(&root, &saved).is_err(),
            "stale revision must not overwrite saved cleanup"
        );
        let newer = store.save(&root, &current).unwrap();
        assert!(
            store.refresh(&root, &current).is_err(),
            "unchanged anchors must still check the saved revision"
        );
        std::fs::write(
            root.join("moved"),
            "inserted\n    keep code\ninserted\n    keep code\n",
        )
        .unwrap();
        let ambiguous = store.refresh(&root, &newer).unwrap();
        assert_eq!(ambiguous.notes.len(), 1);
        assert!(ambiguous.notes[0].needs_reattach);
        assert_eq!(store.load(&root).unwrap(), ambiguous);
        std::fs::write(root.join("moved"), "inserted\n    keep code\n").unwrap();
        let current = store.refresh(&root, &ambiguous).unwrap();
        assert!(!current.notes[0].needs_reattach);
        std::fs::rename(root.join(".git"), root.join("git-unavailable")).unwrap();
        assert!(store.refresh(&root, &current).is_err());
        assert_eq!(
            store.load(&root).unwrap(),
            current,
            "failed Git read must not delete notes"
        );
        std::fs::rename(root.join("git-unavailable"), root.join(".git")).unwrap();
        std::fs::remove_file(root.join("moved")).unwrap();
        assert!(store.refresh(&root, &current).unwrap().notes.is_empty());
        assert!(store.load(&root).unwrap().notes.is_empty());
    }
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
