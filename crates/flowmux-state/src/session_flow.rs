// SPDX-License-Identifier: GPL-3.0-or-later
//! Flowmux-owned lineage database. Native histories and workspace state are never written.
//! All methods perform blocking I/O and belong on a worker thread.

use crate::session_history::{HistorySession, SessionAgent};
use flowmux_core::session_flow::{Camera, Lineage, Point, SessionKey};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

const SCHEMA: i64 = 2;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRecord {
    pub title: String,
    pub cwd: PathBuf,
    pub path: PathBuf,
}

impl From<&HistorySession> for SessionRecord {
    fn from(session: &HistorySession) -> Self {
        Self {
            title: session.title.clone(),
            cwd: session.cwd.clone(),
            path: session.path.clone(),
        }
    }
}

pub fn session_key(agent: SessionAgent, home: &Path, id: &str) -> io::Result<SessionKey> {
    Ok(SessionKey {
        provider: agent.name().to_ascii_lowercase(),
        home: home.canonicalize()?,
        id: id.to_owned(),
    })
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub lineage: Lineage,
    pub records: BTreeMap<SessionKey, SessionRecord>,
    pub positions: BTreeMap<SessionKey, Point>,
}

/// Refresh one native namespace without changing the existing history panel's
/// discovery/filtering behavior. Unknown provider lineage stays unknown.
pub fn import_native(
    store: &mut FlowStore,
    agent: SessionAgent,
    home: &Path,
) -> io::Result<Vec<(SessionKey, HistorySession)>> {
    let sessions = crate::session_history::list_sessions(agent, home)?;
    let mut available = Vec::new();
    let mut records = Vec::new();
    let mut edges = Vec::new();
    for session in sessions {
        let key = session_key(agent, home, &session.id)?;
        if let Some(parent) = session.fork_parent()? {
            edges.push((
                SessionKey {
                    id: parent,
                    ..key.clone()
                },
                key.clone(),
            ));
        }
        records.push((key.clone(), SessionRecord::from(&session)));
        available.push((key, session));
    }
    store.import(&records, &edges)?;
    Ok(available)
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum OperationKind {
    Fork,
    Resume,
}

/// Persisted intent. A crash leaves a pending intent for reconciliation, never an automatic retry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Operation {
    pub request: Uuid,
    pub owner: Uuid,
    pub owner_pid: u32,
    pub source: SessionKey,
    pub kind: OperationKind,
    pub target_surface: Uuid,
}

/// Persisted presentation only. These rows never contribute graph membership or edges.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SavedView {
    pub camera: Camera,
    pub selected: Option<SessionKey>,
    pub initialized: bool,
    pub open: bool,
    pub height: i32,
}

impl SavedView {
    fn valid(&self) -> bool {
        self.camera.origin.finite()
            && (0.1..=4.0).contains(&self.camera.zoom)
            && (0..=16384).contains(&self.height)
    }
}

pub struct FlowStore {
    connection: Connection,
}

fn encode(value: &impl Serialize) -> io::Result<String> {
    serde_json::to_string(value).map_err(io::Error::other)
}
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> io::Result<T> {
    serde_json::from_str(value).map_err(io::Error::other)
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

impl FlowStore {
    pub fn default_path() -> io::Result<PathBuf> {
        flowmux_config::paths::data_dir()
            .map(|p| p.join("session-flow.sqlite"))
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "Flowmux data directory is unavailable",
                )
            })
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path).map_err(io::Error::other)?;
        connection
            .busy_timeout(Duration::from_millis(250))
            .map_err(io::Error::other)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io::Error::other)?;
        let version: i64 = transaction
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(io::Error::other)?;
        if version > SCHEMA {
            return Err(invalid(
                "Session Flow database is from a newer Flowmux version",
            ));
        }
        if version == 0 {
            transaction.execute_batch(
                "CREATE TABLE sessions (key TEXT PRIMARY KEY, metadata TEXT, x REAL, y REAL);
                 CREATE TABLE forks (child TEXT PRIMARY KEY, parent TEXT NOT NULL);
                 CREATE INDEX forks_parent ON forks(parent);
                 CREATE TABLE operations (request TEXT PRIMARY KEY, source TEXT NOT NULL, intent TEXT NOT NULL,
                     state TEXT NOT NULL CHECK(state IN ('pending','complete','failed')), result TEXT);
                 CREATE UNIQUE INDEX active_operation ON operations(source) WHERE state = 'pending';
                 PRAGMA user_version = 1;"
            ).map_err(io::Error::other)?;
        }
        if version < 2 {
            transaction.execute_batch("CREATE TABLE views (workspace TEXT PRIMARY KEY, state TEXT NOT NULL); PRAGMA user_version = 2;").map_err(io::Error::other)?;
        }
        transaction.commit().map_err(io::Error::other)?;
        Ok(Self { connection })
    }

    pub fn views(&self) -> io::Result<BTreeMap<Uuid, SavedView>> {
        let mut query = self
            .connection
            .prepare("SELECT workspace,state FROM views")
            .map_err(io::Error::other)?;
        let rows = query
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(io::Error::other)?;
        let mut result = BTreeMap::new();
        for row in rows {
            let (workspace, state) = row.map_err(io::Error::other)?;
            if let (Ok(id), Ok(view)) = (Uuid::parse_str(&workspace), decode::<SavedView>(&state)) {
                if view.valid() {
                    result.insert(id, view);
                }
            }
        }
        Ok(result)
    }

    pub fn save_views(&mut self, views: &BTreeMap<Uuid, SavedView>) -> io::Result<()> {
        if views.values().any(|v| !v.valid()) {
            return Err(invalid("Invalid Session Flow view"));
        }
        let tx = self.connection.transaction().map_err(io::Error::other)?;
        for (workspace, view) in views {
            tx.execute("INSERT INTO views(workspace,state) VALUES(?1,?2) ON CONFLICT(workspace) DO UPDATE SET state=excluded.state", params![workspace.to_string(), encode(view)?]).map_err(io::Error::other)?;
        }
        tx.commit().map_err(io::Error::other)
    }

    pub fn snapshot(&self) -> io::Result<Snapshot> {
        read_snapshot(&self.connection)
    }

    /// Import a batch transactionally; inconsistent evidence rejects the whole batch.
    pub fn import(
        &mut self,
        records: &[(SessionKey, SessionRecord)],
        edges: &[(SessionKey, SessionKey)],
    ) -> io::Result<()> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io::Error::other)?;
        let mut graph = read_lineage(&tx)?;
        for (parent, child) in edges {
            graph
                .link(parent.clone(), child.clone())
                .map_err(|e| invalid(format!("Invalid fork: {e:?}")))?;
        }
        for (key, record) in records {
            tx.execute("INSERT INTO sessions(key,metadata) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET metadata=excluded.metadata",
                params![encode(key)?, encode(record)?]).map_err(io::Error::other)?;
        }
        for (parent, child) in edges {
            insert_edge(&tx, parent, child)?;
        }
        tx.commit().map_err(io::Error::other)
    }

    /// Missing native records remain visible placeholders; this only updates display geometry.
    pub fn save_positions(&mut self, positions: &BTreeMap<SessionKey, Point>) -> io::Result<()> {
        if positions.values().any(|p| !p.finite()) {
            return Err(invalid("Non-finite Session Flow position"));
        }
        let tx = self.connection.transaction().map_err(io::Error::other)?;
        for (key, p) in positions {
            tx.execute(
                "UPDATE sessions SET x=?2,y=?3 WHERE key=?1",
                params![encode(key)?, p.x, p.y],
            )
            .map_err(io::Error::other)?;
        }
        tx.commit().map_err(io::Error::other)
    }

    /// Reserve before creating a pane. A repeated request is safe only with exactly the same intent.
    pub fn begin(&mut self, operation: &Operation) -> io::Result<bool> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io::Error::other)?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT intent FROM operations WHERE request=?1",
                [operation.request.to_string()],
                |r| r.get(0),
            )
            .optional()
            .map_err(io::Error::other)?;
        if let Some(existing) = existing {
            if decode::<Operation>(&existing)? != *operation {
                return Err(invalid(
                    "Request ID belongs to a different Session Flow operation",
                ));
            }
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO operations(request,source,intent,state) VALUES(?1,?2,?3,'pending')",
            params![
                operation.request.to_string(),
                encode(&operation.source)?,
                encode(operation)?
            ],
        )
        .map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)?;
        Ok(true)
    }

    pub fn pending(&self) -> io::Result<Vec<Operation>> {
        let mut query = self
            .connection
            .prepare("SELECT intent FROM operations WHERE state='pending' ORDER BY request")
            .map_err(io::Error::other)?;
        let rows = query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(io::Error::other)?;
        rows.map(|row| decode(&row.map_err(io::Error::other)?))
            .collect()
    }

    /// The caller must observe this exact native identity on the reserved surface.
    /// Publishing the fork and finishing the operation happen in one commit.
    pub fn complete(&mut self, operation: &Operation, observed: &SessionKey) -> io::Result<()> {
        match operation.kind {
            OperationKind::Resume if observed != &operation.source => {
                return Err(invalid("Resumed session identity does not match"))
            }
            OperationKind::Fork if observed == &operation.source => {
                return Err(invalid("Fork did not create a new session"))
            }
            _ => {}
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(io::Error::other)?;
        let (intent, state, result): (String, String, Option<String>) = tx
            .query_row(
                "SELECT intent,state,result FROM operations WHERE request=?1",
                [operation.request.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(io::Error::other)?;
        if decode::<Operation>(&intent)? != *operation {
            return Err(invalid("Session Flow operation owner or target changed"));
        }
        if state == "complete" && result.as_deref() == Some(&encode(observed)?) {
            return Ok(());
        }
        if state != "pending" {
            return Err(invalid("Session Flow operation is no longer pending"));
        }
        if operation.kind == OperationKind::Fork {
            let mut graph = read_lineage(&tx)?;
            graph
                .link(operation.source.clone(), observed.clone())
                .map_err(|e| invalid(format!("Invalid fork: {e:?}")))?;
            insert_edge(&tx, &operation.source, observed)?;
        }
        tx.execute(
            "UPDATE operations SET state='complete',result=?2 WHERE request=?1",
            params![operation.request.to_string(), encode(observed)?],
        )
        .map_err(io::Error::other)?;
        tx.commit().map_err(io::Error::other)
    }

    /// Release a failed operation only after its process/target has been reconciled.
    pub fn fail(&mut self, operation: &Operation) -> io::Result<()> {
        let changed = self.connection.execute("UPDATE operations SET state='failed' WHERE request=?1 AND intent=?2 AND state='pending'",
            params![operation.request.to_string(), encode(operation)?]).map_err(io::Error::other)?;
        if changed == 0 {
            return Err(invalid(
                "Session Flow operation is not pending or its owner changed",
            ));
        }
        Ok(())
    }
}

fn insert_edge(db: &Connection, parent: &SessionKey, child: &SessionKey) -> io::Result<()> {
    let (parent, child) = (encode(parent)?, encode(child)?);
    for key in [&parent, &child] {
        db.execute("INSERT OR IGNORE INTO sessions(key) VALUES(?1)", [key])
            .map_err(io::Error::other)?;
    }
    db.execute(
        "INSERT OR IGNORE INTO forks(child,parent) VALUES(?1,?2)",
        params![child, parent],
    )
    .map_err(io::Error::other)?;
    Ok(())
}

fn read_lineage(db: &Connection) -> io::Result<Lineage> {
    let mut graph = Lineage::default();
    let mut query = db
        .prepare("SELECT parent,child FROM forks ORDER BY child")
        .map_err(io::Error::other)?;
    let rows = query
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(io::Error::other)?;
    for row in rows {
        let (parent, child) = row.map_err(io::Error::other)?;
        graph
            .link(decode(&parent)?, decode(&child)?)
            .map_err(|e| invalid(format!("Invalid stored fork: {e:?}")))?;
    }
    Ok(graph)
}

fn read_snapshot(db: &Connection) -> io::Result<Snapshot> {
    // One read transaction prevents combining metadata/geometry from different commits.
    let tx = db.unchecked_transaction().map_err(io::Error::other)?;
    let mut snapshot = Snapshot {
        lineage: read_lineage(&tx)?,
        ..Snapshot::default()
    };
    {
        let mut query = tx
            .prepare("SELECT key,metadata,x,y FROM sessions")
            .map_err(io::Error::other)?;
        let rows = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<f64>>(2)?,
                    r.get::<_, Option<f64>>(3)?,
                ))
            })
            .map_err(io::Error::other)?;
        for row in rows {
            let (key, metadata, x, y) = row.map_err(io::Error::other)?;
            let key: SessionKey = decode(&key)?;
            if let Some(metadata) = metadata {
                snapshot.records.insert(key.clone(), decode(&metadata)?);
            }
            if let (Some(x), Some(y)) = (x, y) {
                let p = Point { x, y };
                if p.finite() {
                    snapshot.positions.insert(key, p);
                }
            }
        }
    }
    tx.commit().map_err(io::Error::other)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(id: &str) -> SessionKey {
        SessionKey {
            provider: "codex".into(),
            home: "/history".into(),
            id: id.into(),
        }
    }
    fn operation(kind: OperationKind) -> Operation {
        Operation {
            request: Uuid::new_v4(),
            owner: Uuid::new_v4(),
            owner_pid: std::process::id(),
            source: key("parent"),
            kind,
            target_surface: Uuid::new_v4(),
        }
    }
    fn store() -> (tempfile::TempDir, FlowStore) {
        let dir = tempfile::tempdir().unwrap();
        let db = FlowStore::open(&dir.path().join("flow.sqlite")).unwrap();
        (dir, db)
    }
    #[test]
    fn import_roundtrips_metadata_placeholders_and_shared_geometry_without_workspace_ids() {
        let (dir, mut db) = store();
        let record = SessionRecord {
            title: "Branch".into(),
            cwd: "/repo".into(),
            path: "/history/child.jsonl".into(),
        };
        db.import(
            &[(key("child"), record.clone())],
            &[(key("parent"), key("child"))],
        )
        .unwrap();
        let positions = [(key("child"), Point { x: 41.0, y: -22.0 })].into();
        db.save_positions(&positions).unwrap();
        drop(db);
        let snapshot = FlowStore::open(&dir.path().join("flow.sqlite"))
            .unwrap()
            .snapshot()
            .unwrap();
        assert_eq!(snapshot.records[&key("child")], record);
        assert!(!snapshot.records.contains_key(&key("parent")));
        assert_eq!(snapshot.lineage.nodes().len(), 2);
        assert_eq!(snapshot.positions, positions);
        assert_eq!(
            snapshot.lineage.project([&key("child")]),
            snapshot.lineage.project([&key("parent")])
        );
    }
    #[test]
    fn invalid_batch_is_atomic_and_repeated_import_is_idempotent() {
        let (_dir, mut db) = store();
        let edges = [(key("a"), key("b"))];
        db.import(&[], &edges).unwrap();
        db.import(&[], &edges).unwrap();
        assert!(db
            .import(&[], &[(key("x"), key("y")), (key("b"), key("a"))])
            .is_err());
        let snapshot = db.snapshot().unwrap();
        assert_eq!(snapshot.lineage.edges().count(), 1);
        assert!(db
            .save_positions(
                &[(
                    key("a"),
                    Point {
                        x: f64::NAN,
                        y: 0.0
                    }
                )]
                .into()
            )
            .is_err());
        assert!(db.snapshot().unwrap().positions.is_empty());
    }
    #[test]
    fn concurrent_instances_reserve_once_and_completed_fork_is_atomic_idempotent() {
        let (dir, mut a) = store();
        let mut b = FlowStore::open(&dir.path().join("flow.sqlite")).unwrap();
        let op = operation(OperationKind::Fork);
        assert!(a.begin(&op).unwrap());
        assert!(!b.begin(&op).unwrap());
        assert!(b.begin(&operation(OperationKind::Resume)).is_err());
        assert!(a.complete(&op, &op.source).is_err());
        assert_eq!(b.pending().unwrap(), std::slice::from_ref(&op));
        let mut stolen = op.clone();
        stolen.target_surface = Uuid::new_v4();
        assert!(b.complete(&stolen, &key("child")).is_err());
        assert!(b.begin(&stolen).is_err());
        a.complete(&op, &key("child")).unwrap();
        b.complete(&op, &key("child")).unwrap();
        assert!(b.pending().unwrap().is_empty());
        assert_eq!(b.snapshot().unwrap().lineage.edges().count(), 1);
        assert!(b.complete(&op, &key("different")).is_err());
        assert!(!b.begin(&op).unwrap());
    }
    #[test]
    fn crash_keeps_intent_and_only_matching_owner_can_fail_or_finish() {
        let (dir, mut db) = store();
        let op = operation(OperationKind::Resume);
        db.begin(&op).unwrap();
        drop(db);
        let mut db = FlowStore::open(&dir.path().join("flow.sqlite")).unwrap();
        assert_eq!(db.pending().unwrap(), std::slice::from_ref(&op));
        assert!(db.complete(&op, &key("wrong")).is_err());
        let mut foreign = op.clone();
        foreign.owner = Uuid::new_v4();
        assert!(db.fail(&foreign).is_err());
        db.complete(&op, &op.source).unwrap();
        assert!(db.snapshot().unwrap().lineage.is_empty());
        let retry = operation(OperationKind::Resume);
        assert!(db.begin(&retry).unwrap());
        db.fail(&retry).unwrap();
        assert!(db.complete(&retry, &retry.source).is_err());
        assert!(db.begin(&operation(OperationKind::Resume)).unwrap());
    }
    #[test]
    fn failed_completion_never_publishes_partial_edge_or_loses_reservation() {
        let (_dir, mut db) = store();
        db.import(&[], &[(key("child"), key("parent"))]).unwrap();
        let op = operation(OperationKind::Fork);
        db.begin(&op).unwrap();
        assert!(db.complete(&op, &key("child")).is_err());
        assert_eq!(db.pending().unwrap(), std::slice::from_ref(&op));
        assert_eq!(db.snapshot().unwrap().lineage.edges().count(), 1);
        let mut different_store = key("child");
        different_store.home = "/foreign".into();
        assert!(db.complete(&op, &different_store).is_err());
    }
    #[test]
    fn newer_or_corrupt_database_is_not_recreated() {
        let (dir, db) = store();
        db.connection
            .pragma_update(None, "user_version", 99)
            .unwrap();
        drop(db);
        let path = dir.path().join("flow.sqlite");
        let before = std::fs::read(&path).unwrap();
        assert!(FlowStore::open(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let corrupt = dir.path().join("broken.sqlite");
        std::fs::write(&corrupt, "not sqlite").unwrap();
        assert!(FlowStore::open(&corrupt).is_err());
        assert_eq!(std::fs::read(&corrupt).unwrap(), b"not sqlite");
    }
    #[test]
    fn symlink_namespaces_resolve_to_one_store() {
        let dir = tempfile::tempdir().unwrap();
        let native = dir.path().join("native");
        std::fs::create_dir(&native).unwrap();
        let alias = dir.path().join("alias");
        std::os::unix::fs::symlink(&native, &alias).unwrap();
        assert_eq!(
            session_key(SessionAgent::Codex, &native, "id").unwrap(),
            session_key(SessionAgent::Codex, &alias, "id").unwrap()
        );
        assert!(session_key(SessionAgent::Codex, &dir.path().join("missing"), "id").is_err());
    }
    #[test]
    fn native_codex_fork_uses_thread_identity_and_does_not_change_history() {
        let (dir, mut db) = store();
        let home = dir.path().join("codex");
        let sessions = home.join("sessions/2026/10/31");
        std::fs::create_dir_all(&sessions).unwrap();
        let parent = Uuid::new_v4().to_string();
        let child = Uuid::new_v4().to_string();
        let grandchild = Uuid::new_v4().to_string();
        let unrelated = Uuid::new_v4().to_string();
        for (id, source, fork, ordinary_parent) in [
            (&child, serde_json::json!("cli"), Some(&parent), None),
            (&grandchild, serde_json::json!("cli"), Some(&child), None),
            (&unrelated, serde_json::json!("cli"), None, Some(&parent)),
        ] {
            let content = serde_json::json!({"type":"session_meta","payload":{
                "id":id,"session_id":parent,"source":source,"forked_from_id":fork,
                "parent_thread_id":ordinary_parent,"cwd":dir.path()
            }})
            .to_string();
            std::fs::write(sessions.join(format!("{id}.jsonl")), content).unwrap();
        }
        let before = crate::session_history::list_sessions(SessionAgent::Codex, &home).unwrap();
        let available = import_native(&mut db, SessionAgent::Codex, &home).unwrap();
        assert_eq!(available.len(), before.len());
        let graph = db.snapshot().unwrap().lineage;
        assert_eq!(graph.edges().count(), 2);
        assert_eq!(
            graph
                .project([&session_key(SessionAgent::Codex, &home, &grandchild).unwrap()])
                .nodes()
                .len(),
            3
        );
        assert!(graph
            .project([&session_key(SessionAgent::Codex, &home, &unrelated).unwrap()])
            .is_empty());
        import_native(&mut db, SessionAgent::Codex, &home).unwrap();
        assert_eq!(db.snapshot().unwrap().lineage, graph);
        assert_eq!(
            crate::session_history::list_sessions(SessionAgent::Codex, &home)
                .unwrap()
                .len(),
            before.len()
        );
    }

    #[test]
    fn fork_commands_validate_ids_and_never_relax_permissions() {
        let mut session = HistorySession {
            agent: SessionAgent::Claude,
            id: Uuid::new_v4().to_string(),
            title: String::new(),
            summary: String::new(),
            cwd: "/repo".into(),
            path: "/transcript".into(),
            modified: std::time::UNIX_EPOCH,
        };
        assert_eq!(
            session.fork_command().unwrap(),
            format!("claude --resume {} --fork-session", session.id)
        );
        session.agent = SessionAgent::Codex;
        assert_eq!(
            session.fork_command().unwrap(),
            format!("codex fork {}", session.id)
        );
        for agent in [
            SessionAgent::Gemini,
            SessionAgent::Cline,
            SessionAgent::Antigravity,
        ] {
            session.agent = agent;
            assert_eq!(
                session.fork_command().unwrap_err().kind(),
                io::ErrorKind::Unsupported
            );
        }
        session.agent = SessionAgent::OpenCode;
        session.id = "ses_valid123".into();
        assert_eq!(
            session.fork_command().unwrap(),
            "opencode --session ses_valid123 --fork"
        );
        for id in ["ses_x;touch /tmp/pwn", "ses_", "$(echo injected)"] {
            session.id = id.into();
            assert!(session.fork_command().is_err());
        }
        session.agent = SessionAgent::Codex;
        assert!(session.fork_command().is_err());
    }

    #[test]
    fn malformed_native_parent_rejects_import_without_publishing_guesswork() {
        let (dir, mut db) = store();
        let path = dir.path().join("transcript.jsonl");
        let id = Uuid::new_v4().to_string();
        let session = HistorySession {
            agent: SessionAgent::Codex,
            id: id.clone(),
            title: String::new(),
            summary: String::new(),
            cwd: dir.path().into(),
            path: path.clone(),
            modified: std::time::UNIX_EPOCH,
        };
        for (meta, expected) in [
            (serde_json::json!({"id":id,"forked_from_id":"bad"}), "error"),
            (
                serde_json::json!({"id":id,"source":{"subagent":"review"},"forked_from_id":Uuid::new_v4()}),
                "none",
            ),
            (
                serde_json::json!({"id":Uuid::new_v4(),"forked_from_id":Uuid::new_v4()}),
                "none",
            ),
            (
                serde_json::json!({"id":id,"parent_thread_id":Uuid::new_v4()}),
                "none",
            ),
        ] {
            std::fs::write(
                &path,
                serde_json::json!({"type":"session_meta","payload":meta}).to_string(),
            )
            .unwrap();
            match expected {
                "error" => assert!(session.fork_parent().is_err()),
                _ => assert_eq!(session.fork_parent().unwrap(), None),
            }
        }
        assert!(db.snapshot().unwrap().lineage.is_empty());
        db.import(&[], &[]).unwrap();
    }
    #[test]
    fn view_preferences_roundtrip_without_changing_shared_lineage() {
        let (dir, mut db) = store();
        let id = Uuid::new_v4();
        db.import(&[], &[(key("a"), key("b"))]).unwrap();
        let graph = db.snapshot().unwrap().lineage;
        let view = SavedView {
            camera: Camera {
                origin: Point { x: 42.0, y: -99.0 },
                zoom: 1.7,
            },
            selected: Some(key("b")),
            initialized: true,
            open: true,
            height: 240,
        };
        db.save_views(&[(id, view.clone())].into()).unwrap();
        drop(db);
        let mut db = FlowStore::open(&dir.path().join("flow.sqlite")).unwrap();
        assert_eq!(db.views().unwrap()[&id], view);
        assert_eq!(db.snapshot().unwrap().lineage, graph);
        let mut bad = view;
        bad.camera.zoom = 0.0;
        assert!(db.save_views(&[(id, bad)].into()).is_err());
        assert_eq!(db.views().unwrap()[&id].camera.zoom, 1.7);
    }

    #[test]
    fn v1_migration_preserves_relations_and_unknown_view_content_is_ignored() {
        let (dir, db) = store();
        db.connection
            .execute_batch("DROP TABLE views; PRAGMA user_version = 1;")
            .unwrap();
        drop(db);
        let db = FlowStore::open(&dir.path().join("flow.sqlite")).unwrap();
        db.connection
            .execute(
                "INSERT INTO views VALUES (?1,'not json')",
                [Uuid::new_v4().to_string()],
            )
            .unwrap();
        assert!(db.views().unwrap().is_empty());
        assert!(db.snapshot().unwrap().lineage.is_empty());
    }
}
