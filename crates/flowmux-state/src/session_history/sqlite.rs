// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only adapters for native agent databases. Never migrate agent schemas.

use super::*;
use rusqlite::{Connection, OpenFlags};
use std::time::{Duration, UNIX_EPOCH};

fn database(path: &Path) -> io::Result<Connection> {
    let db = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(io::Error::other)?;
    db.busy_timeout(Duration::from_millis(250))
        .map_err(io::Error::other)?;
    Ok(db)
}

fn timestamp(value: &str) -> Option<SystemTime> {
    if let Ok(millis) = value.parse::<u64>() {
        return UNIX_EPOCH.checked_add(Duration::from_millis(millis));
    }
    chrono::DateTime::parse_from_rfc3339(&value.replacen(' ', "T", 1))
        .ok()
        .map(Into::into)
}

pub(super) fn list_sessions(agent: SessionAgent, home: &Path) -> io::Result<Vec<HistorySession>> {
    let (filename, sql) = match agent {
        SessionAgent::OpenCode => ("opencode.db", "SELECT id, title, directory, CAST(time_updated AS TEXT), '' FROM session WHERE parent_id IS NULL AND time_archived IS NULL"),
        SessionAgent::Antigravity => ("conversation_summaries.db", "SELECT conversation_id, title, workspace_uris, last_modified_time, preview FROM conversation_summaries WHERE COALESCE(parent_conversation_id, '') = '' AND nesting_depth = 0"),
        SessionAgent::Cline => ("sessions.db", "SELECT session_id, COALESCE(metadata_json, '{}'), cwd, updated_at, COALESCE(prompt, '') FROM sessions WHERE COALESCE(parent_session_id, '') = '' AND COALESCE(is_subagent, 0) = 0"),
        _ => unreachable!(),
    };
    let path = home.join(filename);
    if !path.try_exists()? {
        return Ok(Vec::new());
    }
    let db = database(&path)?;
    let mut query = db.prepare(sql).map_err(io::Error::other)?;
    let rows = query
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })
        .map_err(io::Error::other)?;
    let mut result = Vec::new();
    for row in rows {
        let (id, title, directory, modified, summary) = row.map_err(io::Error::other)?;
        let Some(modified) = timestamp(&modified) else {
            continue;
        };
        let cwd = if agent == SessionAgent::Antigravity {
            let uris: Vec<String> = serde_json::from_str(&directory).unwrap_or_default();
            let Some(path) = uris
                .iter()
                .find_map(|uri| url::Url::parse(uri).ok()?.to_file_path().ok())
            else {
                continue;
            };
            path
        } else {
            PathBuf::from(directory)
        };
        if !cwd.is_absolute() {
            continue;
        }
        let title = if agent == SessionAgent::Cline {
            serde_json::from_str::<Value>(&title)
                .ok()
                .and_then(|v| v["title"].as_str().map(short))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| short(&summary))
        } else {
            short(&title)
        };
        let mut item = HistorySession {
            agent,
            id,
            title,
            cwd,
            modified,
            summary: short(&summary),
            path: path.clone(),
        };
        if item.resume_command().is_err() {
            continue;
        }
        if item.title.is_empty() {
            item.title.clone_from(&item.id);
        }
        if agent == SessionAgent::OpenCode {
            // Only sample recent text parts; do not read full conversations for list rows.
            item.summary = opencode_text(&db, &item.id, 8)?
                .lines()
                .last()
                .map(short)
                .unwrap_or_default();
        }
        result.push(item);
    }
    result.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    Ok(result)
}

fn opencode_text(db: &Connection, id: &str, limit: usize) -> io::Result<String> {
    let mut query = db.prepare("SELECT substr(m.data, 1, 262144), substr(p.data, 1, 262144) FROM part p JOIN message m ON p.message_id = m.id WHERE p.session_id = ?1 AND m.session_id = ?1 ORDER BY p.time_created DESC, p.id DESC LIMIT ?2")
        .map_err(io::Error::other)?;
    let rows = query
        .query_map(rusqlite::params![id, limit], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(io::Error::other)?;
    let mut messages = Vec::new();
    let mut size = 0;
    for row in rows {
        let (message, part) = row.map_err(io::Error::other)?;
        let (Ok(message), Ok(part)) = (
            serde_json::from_str::<Value>(&message),
            serde_json::from_str::<Value>(&part),
        ) else {
            continue;
        };
        let role = match message["role"].as_str() {
            Some("user") => "You",
            Some("assistant") => "Assistant",
            _ => continue,
        };
        if part["type"] != "text" || part["synthetic"] == true || part["ignored"] == true {
            continue;
        }
        if let Some(text) = part["text"].as_str() {
            size += text.len();
            if size > 2 * 1024 * 1024 {
                break;
            }
            messages.push(format!("{role}\n{}", clean(text)));
        }
    }
    messages.reverse();
    Ok(messages.join("\n\n"))
}

pub(super) fn preview(session: &HistorySession) -> io::Result<String> {
    let db = database(&session.path)?;
    match session.agent {
        SessionAgent::OpenCode => {
            let text = opencode_text(&db, &session.id, 1000)?;
            Ok(format!(
                "Recent conversation (up to 1,000 parts / 2 MiB).\n\n{text}"
            ))
        }
        SessionAgent::Antigravity => {
            let summary: String = db
                .query_row(
                    "SELECT preview FROM conversation_summaries WHERE conversation_id = ?1",
                    [&session.id],
                    |r| r.get(0),
                )
                .map_err(io::Error::other)?;
            Ok(format!("Conversation summary\n\n{}\n\nFull conversation preview is unavailable for Antigravity's binary transcript format. Resume in a new tab to read the conversation.", clean(&summary)))
        }
        SessionAgent::Cline => {
            let path: Option<String> = db
                .query_row(
                    "SELECT messages_path FROM sessions WHERE session_id = ?1",
                    [&session.id],
                    |r| r.get(0),
                )
                .map_err(io::Error::other)?;
            let Some(path) = path.filter(|p| !p.is_empty()) else {
                return Ok(format!(
                    "{}\n\nNo saved conversation messages are available yet.",
                    session.summary
                ));
            };
            let (bytes, partial) = read_window(Path::new(&path), 2 * 1024 * 1024, false)?;
            if partial {
                return Ok(
                    "Conversation exceeds the 2 MiB preview limit. Resume in a new tab to read it."
                        .into(),
                );
            }
            let data: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            let mut text = String::new();
            for message in data["messages"].as_array().into_iter().flatten() {
                let role = match message["role"].as_str() {
                    Some("user") => "You",
                    Some("assistant") => "Assistant",
                    _ => continue,
                };
                let content = content_text(&message["content"]);
                if !content.is_empty() {
                    text.push_str(&format!("{role}\n{content}\n\n"));
                }
            }
            if text.is_empty() {
                text.push_str("No readable conversation messages in this session.");
            }
            Ok(text)
        }
        _ => unreachable!(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ID: &str = "12345678-1234-4234-8234-123456789abc";

    #[test]
    fn opencode_database_filters_children_archives_and_private_parts() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("opencode.db");
        let db = Connection::open(&path).unwrap();
        db.execute_batch(r#"CREATE TABLE session(id TEXT, title TEXT, directory TEXT, time_updated INTEGER, parent_id TEXT, time_archived INTEGER);
            CREATE TABLE message(id TEXT, session_id TEXT, data TEXT);
            CREATE TABLE part(id TEXT, message_id TEXT, session_id TEXT, time_created INTEGER, data TEXT);
            INSERT INTO session VALUES ('ses_abc123', '한글 title', '/repo', 1786001628258, NULL, NULL), ('ses_child', 'child', '/repo', 1, 'ses_abc123', NULL), ('ses_archived', 'archived', '/repo', 1, NULL, 1);
            INSERT INTO message VALUES ('msg_a', 'ses_abc123', '{"role":"assistant"}');"#).unwrap();
        for (i, part) in [
            json!({"type":"reasoning","text":"private"}),
            json!({"type":"text","text":"hidden","synthetic":true}),
            json!({"type":"text","text":"답변 🦀"}),
            json!({"type":"tool","state":{"output":"private tool result"}}),
        ]
        .iter()
        .enumerate()
        {
            db.execute(
                "INSERT INTO part VALUES (?1, 'msg_a', 'ses_abc123', ?2, ?3)",
                rusqlite::params![format!("part{i}"), i, part.to_string()],
            )
            .unwrap();
        }
        drop(db);
        let before = fs::read(&path).unwrap();
        let items = list_sessions(SessionAgent::OpenCode, home.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].resume_command().unwrap(),
            "opencode --session ses_abc123"
        );
        assert_eq!(items[0].summary, "답변 🦀");
        let preview = items[0].preview().unwrap();
        assert!(preview.contains("Assistant\n답변 🦀"));
        assert!(!preview.contains("private") && !preview.contains("hidden"));
        assert_eq!(before, fs::read(path).unwrap());
        let mut invalid = items[0].clone();
        for id in ["ses_", "ses_a;id", "ses_../../a", "--session", "ses_a\n"] {
            invalid.id = id.into();
            assert!(invalid.resume_command().is_err());
        }
    }

    #[test]
    fn antigravity_decodes_local_workspace_uri_and_labels_summary_only_preview() {
        let home = tempfile::tempdir().unwrap();
        let db = Connection::open(home.path().join("conversation_summaries.db")).unwrap();
        db.execute_batch("CREATE TABLE conversation_summaries(conversation_id TEXT, title TEXT, workspace_uris TEXT, last_modified_time TEXT, preview TEXT, parent_conversation_id TEXT, nesting_depth INTEGER);").unwrap();
        db.execute("INSERT INTO conversation_summaries VALUES (?1, 'Title', ?2, '2026-09-03 02:05:48.241518043+00:00', 'Summary', '', 0)", rusqlite::params![ID, "[\"file:///tmp/project%20%ED%95%9C%EA%B8%80\"]"]).unwrap();
        let items = list_sessions(SessionAgent::Antigravity, home.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].cwd, Path::new("/tmp/project 한글"));
        assert_eq!(
            items[0].resume_command().unwrap(),
            format!("agy --conversation {ID}")
        );
        assert!(items[0]
            .preview()
            .unwrap()
            .contains("binary transcript format"));
    }

    #[test]
    fn cline_reads_native_messages_and_handles_missing_or_large_artifacts() {
        let home = tempfile::tempdir().unwrap();
        let db = Connection::open(home.path().join("sessions.db")).unwrap();
        db.execute_batch("CREATE TABLE sessions(session_id TEXT, metadata_json TEXT, cwd TEXT, updated_at TEXT, prompt TEXT, parent_session_id TEXT, is_subagent INTEGER, messages_path TEXT);").unwrap();
        let path = home.path().join("messages.json");
        fs::write(&path, json!({"messages":[{"role":"system","content":"private"},{"role":"user","content":"질문"},{"role":"assistant","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"answer"}]}]}).to_string()).unwrap();
        db.execute(r#"INSERT INTO sessions VALUES (?1, '{"title":"Cline title"}', '/repo', '2026-09-16T01:00:00Z', 'question', NULL, 0, ?2)"#, rusqlite::params![ID,path.to_str().unwrap()]).unwrap();
        let items = list_sessions(SessionAgent::Cline, home.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Cline title");
        assert_eq!(
            items[0].resume_command().unwrap(),
            format!("cline --id {ID} --tui")
        );
        assert_eq!(
            items[0].preview().unwrap(),
            "You\n질문\n\nAssistant\nanswer\n\n"
        );
        fs::write(&path, vec![b' '; 2 * 1024 * 1024 + 1]).unwrap();
        assert!(items[0].preview().unwrap().contains("exceeds"));
        fs::remove_file(path).unwrap();
        assert!(items[0].preview().is_err());
    }

    #[test]
    fn missing_databases_stay_missing_and_bad_schemas_report_errors() {
        let home = tempfile::tempdir().unwrap();
        for agent in [
            SessionAgent::OpenCode,
            SessionAgent::Antigravity,
            SessionAgent::Cline,
        ] {
            assert!(list_sessions(agent, home.path()).unwrap().is_empty());
        }
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
        fs::write(home.path().join("opencode.db"), "broken").unwrap();
        assert!(list_sessions(SessionAgent::OpenCode, home.path()).is_err());
    }
}
