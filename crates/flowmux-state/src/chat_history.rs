// SPDX-License-Identifier: GPL-3.0-or-later

use flowmux_core::SurfaceId;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextKind {
    Answer,
    Reasoning,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    pub diff: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Work {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub running: bool,
    pub failed: bool,
    pub read: bool,
    pub changes: Vec<Change>,
    pub check: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub title: String,
    pub options: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub choices: Vec<String>,
    pub questions: Vec<Question>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Begin {
        prompt: String,
        /// IDs of pasted PNGs in the chat image store.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<String>,
        /// Added to the running turn instead of starting one. A field rather than a variant so
        /// older readers of the shared history still load the record.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        steer: bool,
    },
    Text {
        id: String,
        kind: TextKind,
        text: String,
        append: bool,
    },
    Work(Work),
    Prompt(Prompt),
    Resolved {
        id: String,
        label: String,
        #[serde(default)]
        answers: Vec<String>,
    },
    Finished {
        status: String,
        detail: Option<String>,
        elapsed: u64,
    },
    Session {
        native_id: String,
    },
}

#[derive(Clone, Debug)]
pub struct Summary {
    pub id: SurfaceId,
    pub agent: String,
    pub cwd: PathBuf,
    pub title: String,
    pub modified: SystemTime,
    pub native_id: Option<String>,
}

#[derive(Default)]
pub struct History {
    pub native_id: Option<String>,
    pub records: Vec<Record>,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("state directory is unavailable")]
    NoStateDir,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("saved chat metadata is invalid")]
    InvalidChat,
}

pub struct Store(Connection);

pub fn lock(id: SurfaceId) -> Result<crate::InstanceLock, crate::StateError> {
    let path = flowmux_config::paths::state_dir()
        .ok_or(crate::StateError::NoStateDir)?
        .join("chat-locks")
        .join(id.to_string())
        .join("chat");
    crate::instance_lock::try_acquire_for_state(&path)
}

impl Store {
    pub fn open() -> Result<Self, Error> {
        Self::open_at(&Self::path()?)
    }

    fn path() -> Result<PathBuf, Error> {
        Ok(flowmux_config::paths::state_dir()
            .ok_or(Error::NoStateDir)?
            .join("chat-history.sqlite3"))
    }

    fn open_at(path: &Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        let db = Connection::open(path)?;
        db.busy_timeout(Duration::from_millis(150))?;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS chats (
                id TEXT PRIMARY KEY, agent TEXT NOT NULL, cwd BLOB NOT NULL,
                title TEXT NOT NULL, modified INTEGER NOT NULL, native_id TEXT
            );
            CREATE TABLE IF NOT EXISTS chat_records (
                chat_id TEXT NOT NULL REFERENCES chats(id), payload TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS chat_records_by_chat ON chat_records(chat_id);",
        )?;
        if db.prepare("SELECT settings FROM chats LIMIT 0").is_err() {
            db.execute_batch("ALTER TABLE chats ADD COLUMN settings TEXT")?;
        }
        Ok(Self(db))
    }

    /// Remembers the chat's last turn settings for the next launch.
    pub fn save_settings(&self, id: SurfaceId, settings: &impl Serialize) -> Result<(), Error> {
        self.0.execute(
            "UPDATE chats SET settings = ?2 WHERE id = ?1",
            params![id.to_string(), serde_json::to_string(settings)?],
        )?;
        Ok(())
    }

    pub fn settings<T: serde::de::DeserializeOwned>(
        &self,
        id: SurfaceId,
    ) -> Result<Option<T>, Error> {
        let saved: Option<Option<String>> = self
            .0
            .query_row(
                "SELECT settings FROM chats WHERE id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(saved
            .flatten()
            .map(|json| serde_json::from_str(&json))
            .transpose()?)
    }

    pub fn existing() -> Result<Option<Self>, Error> {
        let path = Self::path()?;
        if !path.try_exists()? {
            return Ok(None);
        }
        let db = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        db.busy_timeout(Duration::from_millis(150))?;
        Ok(Some(Self(db)))
    }

    pub fn append(
        &mut self,
        id: SurfaceId,
        agent: &str,
        cwd: &Path,
        history: &[Record],
    ) -> Result<(), Error> {
        let id = id.to_string();
        let at = chrono::Utc::now().timestamp_millis();
        let first_prompt = history.iter().find_map(|r| match r {
            Record::Begin {
                prompt,
                images,
                steer: false,
            } => Some(if prompt.trim().is_empty() && !images.is_empty() {
                "Image"
            } else {
                prompt.as_str()
            }),
            _ => None,
        });
        let tx = self.0.transaction()?;
        if let Some(prompt) = first_prompt {
            let title: String = prompt
                .trim()
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(100)
                .collect();
            tx.execute(
                "INSERT OR IGNORE INTO chats (id, agent, cwd, title, modified) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, agent, cwd.as_os_str().as_encoded_bytes(), title, at],
            )?;
        }
        if tx.execute(
            "UPDATE chats SET modified = ?2 WHERE id = ?1",
            params![id, at],
        )? == 0
        {
            return Err(Error::InvalidChat);
        }
        {
            let mut insert =
                tx.prepare_cached("INSERT INTO chat_records(chat_id, payload) VALUES (?1, ?2)")?;
            for record in history {
                if let Record::Session { native_id } = record {
                    tx.execute(
                        "UPDATE chats SET native_id = ?2 WHERE id = ?1",
                        params![id, native_id],
                    )?;
                } else {
                    insert.execute(params![id, serde_json::to_string(record)?])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn load(&self, id: SurfaceId) -> Result<History, Error> {
        self.load_prefix(id, None)
    }

    pub fn load_prefix(&self, id: SurfaceId, through: Option<i64>) -> Result<History, Error> {
        let mut statement = self.0.prepare(
            "SELECT chats.native_id, chat_records.payload
             FROM chats JOIN chat_records ON chat_records.chat_id = chats.id
             WHERE chats.id = ?1 AND (?2 IS NULL OR chat_records.rowid <= ?2)
             ORDER BY chat_records.rowid",
        )?;
        let mut rows = statement.query(params![id.to_string(), through])?;
        let mut history = History::default();
        while let Some(row) = rows.next()? {
            if history.records.is_empty() {
                history.native_id = row.get(0)?;
            }
            let payload: String = row.get(1)?;
            history.records.push(serde_json::from_str(&payload)?);
        }
        Ok(history)
    }

    pub fn last_record(&self, id: SurfaceId) -> Result<i64, Error> {
        Ok(self.0.query_row(
            "SELECT COALESCE(MAX(rowid), 0) FROM chat_records WHERE chat_id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )?)
    }

    pub fn list(&self, agent: Option<&str>) -> Result<Vec<Summary>, Error> {
        use std::os::unix::ffi::OsStringExt;
        let mut stmt = self.0.prepare(
            "SELECT id, agent, cwd, title, modified, native_id FROM chats
             WHERE ?1 IS NULL OR agent = ?1 ORDER BY modified DESC, id",
        )?;
        let mut rows = stmt.query([agent])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let at: i64 = row.get(4)?;
            result.push(Summary {
                id: row
                    .get::<_, String>(0)?
                    .parse()
                    .map_err(|_| Error::InvalidChat)?,
                agent: row.get(1)?,
                cwd: std::ffi::OsString::from_vec(row.get(2)?).into(),
                title: row.get(3)?,
                modified: chrono::DateTime::from_timestamp_millis(at)
                    .ok_or(Error::InvalidChat)?
                    .into(),
                native_id: row.get(5)?,
            });
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_prompts_keep_the_format_older_readers_parse() {
        let begin = |steer| Record::Begin {
            prompt: "x".into(),
            images: Vec::new(),
            steer,
        };
        assert_eq!(
            serde_json::to_string(&begin(false)).unwrap(),
            r#"{"type":"begin","prompt":"x"}"#
        );
        let steered: Record =
            serde_json::from_str(&serde_json::to_string(&begin(true)).unwrap()).unwrap();
        assert!(matches!(steered, Record::Begin { steer: true, .. }));
    }

    #[test]
    fn restores_record_order_and_native_id_after_reopening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let id = SurfaceId::new();
        let mut store = Store::open_at(&path).unwrap();
        store
            .append(
                id,
                "Codex",
                directory.path(),
                &[
                    Record::Begin {
                        prompt: "한글 prompt\nsecond line".into(),
                        images: vec!["0123456789abcdef0123456789abcdef".into()],
                        steer: false,
                    },
                    Record::Session {
                        native_id: "native-thread".into(),
                    },
                    Record::Work(Work {
                        id: "work".into(),
                        title: "Tests".into(),
                        detail: String::new(),
                        running: false,
                        failed: false,
                        read: false,
                        changes: vec![Change {
                            path: "a.txt".into(),
                            diff: "+saved".into(),
                        }],
                        check: None,
                    }),
                ],
            )
            .unwrap();
        store
            .append(
                id,
                "Codex",
                directory.path(),
                &[
                    Record::Resolved {
                        id: "question".into(),
                        label: "Answer".into(),
                        answers: vec!["제출한 답변".into()],
                    },
                    Record::Finished {
                        status: "Completed".into(),
                        detail: None,
                        elapsed: 3,
                    },
                ],
            )
            .unwrap();
        drop(store);

        let store = Store::open_at(&path).unwrap();
        let history = store.load(id).unwrap();
        assert_eq!(history.native_id.as_deref(), Some("native-thread"));
        assert!(matches!(history.records.as_slice(), [
            Record::Begin { prompt, images, steer: false }, Record::Work(work),
            Record::Resolved { answers, .. }, Record::Finished { elapsed: 3, .. },
        ] if prompt == "한글 prompt\nsecond line"
            && images == &["0123456789abcdef0123456789abcdef"]
            && work.changes[0].diff == "+saved" && answers == &["제출한 답변"]));
        let chats = store.list(Some("Codex")).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].id, id);
        assert_eq!(chats[0].cwd, directory.path());
        assert_eq!(chats[0].title, "한글 prompt");
        assert!(store.list(Some("Claude")).unwrap().is_empty());
    }

    #[test]
    fn adds_settings_to_an_existing_history_and_restores_them() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let id = SurfaceId::new();
        Connection::open(&path)
            .unwrap()
            .execute_batch(&format!(
                "CREATE TABLE chats (
                    id TEXT PRIMARY KEY, agent TEXT NOT NULL, cwd BLOB NOT NULL,
                    title TEXT NOT NULL, modified INTEGER NOT NULL, native_id TEXT
                );
                INSERT INTO chats VALUES ('{id}', 'Claude', x'2f', 'old', 0, NULL);"
            ))
            .unwrap();

        let store = Store::open_at(&path).unwrap();
        assert_eq!(store.settings::<serde_json::Value>(id).unwrap(), None);
        let saved = serde_json::json!({"model": "opus", "access": "bypassPermissions"});
        store.save_settings(id, &saved).unwrap();
        drop(store);

        let store = Store::open_at(&path).unwrap();
        assert_eq!(store.settings(id).unwrap(), Some(saved));
        assert_eq!(store.load(id).unwrap().records.len(), 0);
    }
}
