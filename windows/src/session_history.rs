// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only discovery of native agent session histories.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Instant, SystemTime};

mod sqlite;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionAgent {
    Claude,
    Codex,
    OpenCode,
    Antigravity,
    Cline,
}

impl SessionAgent {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "claude" | "claude code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            "agy" | "antigravity" => Some(Self::Antigravity),
            "cline" => Some(Self::Cline),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::OpenCode => "OpenCode",
            Self::Antigravity => "Antigravity",
            Self::Cline => "Cline",
        }
    }

    pub fn canonical_session_id(self, value: &str) -> io::Result<String> {
        if self == Self::OpenCode {
            if value.starts_with("ses_")
                && (5..=128).contains(&value.len())
                && value
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                return Ok(value.into());
            }
            return Err(invalid("Invalid OpenCode session ID"));
        }
        uuid::Uuid::parse_str(value)
            .map(|id| id.to_string())
            .map_err(|_| invalid("Invalid session ID"))
    }

    pub fn home_variables(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["USERPROFILE", "HOME", "CLAUDE_CONFIG_DIR"],
            Self::Codex => &["USERPROFILE", "HOME", "CODEX_HOME"],
            Self::OpenCode => &[
                "USERPROFILE",
                "HOME",
                "XDG_DATA_HOME",
                "XDG_CONFIG_HOME",
                "XDG_STATE_HOME",
                "XDG_CACHE_HOME",
                "OPENCODE_CONFIG",
                "OPENCODE_CONFIG_DIR",
            ],
            Self::Antigravity => &["USERPROFILE", "HOME"],
            Self::Cline => &[
                "USERPROFILE",
                "HOME",
                "CLINE_DIR",
                "CLINE_DATA_DIR",
                "CLINE_DB_DATA_DIR",
                "CLINE_SESSION_DATA_DIR",
            ],
        }
    }

    pub fn history_home(self, value: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
        let nonempty = |key: &str| value(key).filter(|p| !p.as_os_str().is_empty());
        let value = nonempty;
        let home = || value("USERPROFILE").or_else(|| value("HOME"));
        match self {
            Self::Claude => {
                value("CLAUDE_CONFIG_DIR").or_else(|| home().map(|p| p.join(".claude")))
            }
            Self::Codex => value("CODEX_HOME").or_else(|| home().map(|p| p.join(".codex"))),
            Self::OpenCode => value("XDG_DATA_HOME")
                .or_else(|| home().map(|p| p.join(".local/share")))
                .map(|p| p.join("opencode")),
            Self::Antigravity => home().map(|p| p.join(".gemini/antigravity-cli")),
            Self::Cline => value("CLINE_DB_DATA_DIR").or_else(|| {
                value("CLINE_DATA_DIR")
                    .or_else(|| {
                        value("CLINE_DIR")
                            .or_else(|| home().map(|p| p.join(".cline")))
                            .map(|p| p.join("data"))
                    })
                    .map(|p| p.join("db"))
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HistorySession {
    pub agent: SessionAgent,
    pub id: String,
    pub title: String,
    pub summary: String,
    pub cwd: PathBuf,
    pub modified: SystemTime,
    pub path: PathBuf,
}

impl HistorySession {
    /// Only validated native IDs can become terminal input; transcript text never can.
    pub fn resume_argv(&self) -> io::Result<Vec<String>> {
        let id = self.agent.canonical_session_id(&self.id)?;
        if self.agent == SessionAgent::OpenCode {
            return Ok(vec!["opencode".into(), "--session".into(), id]);
        }
        Ok(match self.agent {
            SessionAgent::Claude => vec!["claude".into(), "--resume".into(), id.to_string()],
            SessionAgent::Codex => vec!["codex".into(), "resume".into(), id.to_string()],
            SessionAgent::Antigravity => {
                vec!["agy".into(), "--conversation".into(), id.to_string()]
            }
            SessionAgent::Cline => vec![
                "cline".into(),
                "--id".into(),
                id.to_string(),
                "--tui".into(),
            ],
            SessionAgent::OpenCode => unreachable!(),
        })
    }

    pub fn preview(&self, cancel: &AtomicBool, deadline: Instant) -> io::Result<String> {
        let mut budget = Budget::new(cancel, deadline);
        budget.check()?;
        self.resume_argv()?;
        if matches!(
            self.agent,
            SessionAgent::OpenCode | SessionAgent::Antigravity | SessionAgent::Cline
        ) {
            return sqlite::preview(self, &mut budget);
        }
        // Match the native history preview: explicitly label the latest 2 MiB.
        let (bytes, partial) = read_window(&self.path, 2 * 1024 * 1024, true, &mut budget)?;
        let mut text = if partial {
            "Earlier content omitted; showing the latest part of this session.\n\n".to_string()
        } else {
            String::new()
        };
        for record in records(&bytes) {
            budget.row()?;
            if let Some((role, message)) = message(&record, self.agent) {
                text.push_str(role);
                text.push('\n');
                text.push_str(&message);
                text.push_str("\n\n");
            }
        }
        if text.is_empty() {
            text.push_str("No readable conversation messages in this session.");
        }
        budget.check()?;
        Ok(text)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Read complete JSONL records without allocating in proportion to file size.
fn read_window(
    path: &Path,
    limit: usize,
    tail: bool,
    budget: &mut Budget<'_>,
) -> io::Result<(Vec<u8>, bool)> {
    budget.file()?;
    let mut file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("Session transcript is not a regular file"));
    }
    let len = file.metadata()?.len();
    if len > MAX_FILE_BYTES {
        return Err(limit_error());
    }
    let partial = len > limit as u64;
    if tail && partial {
        file.seek(SeekFrom::Start(len - limit as u64))?;
    }
    let mut bytes = Vec::new();
    let mut remaining = limit;
    let mut chunk = [0u8; 16 * 1024];
    while remaining > 0 {
        budget.check()?;
        let count = remaining.min(chunk.len());
        let read = file.read(&mut chunk[..count])?;
        if read == 0 {
            break;
        }
        budget.bytes(read)?;
        bytes.extend_from_slice(&chunk[..read]);
        remaining -= read;
    }
    budget.check()?;
    if tail && partial {
        let end = bytes
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes.drain(..end);
    }
    Ok((bytes, partial))
}

fn records(bytes: &[u8]) -> impl Iterator<Item = Value> + '_ {
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice(line).ok())
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}

fn short(text: &str) -> String {
    clean(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(240)
        .collect()
}

fn content_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return clean(text);
    }
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| match block["type"].as_str()? {
            "text" | "input_text" | "output_text" => block["text"].as_str().map(clean),
            "image" | "input_image" => Some("[Image]".into()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn message(record: &Value, agent: SessionAgent) -> Option<(&'static str, String)> {
    let body = match agent {
        SessionAgent::Claude => {
            if record["isSidechain"] == true || record["isMeta"] == true {
                return None;
            }
            &record["message"]
        }
        SessionAgent::Codex => {
            if record["type"] != "response_item" || record["payload"]["type"] != "message" {
                return None;
            }
            &record["payload"]
        }
        _ => return None,
    };
    let role = match body["role"].as_str()? {
        "user" => "You",
        "assistant" => "Assistant",
        _ => return None,
    };
    let text = content_text(&body["content"]);
    // Runtime context records are not user conversation or useful summaries.
    if text.is_empty()
        || text.starts_with("# AGENTS.md instructions")
        || text.starts_with("<environment_context>")
        || text.starts_with("<system-reminder>")
    {
        return None;
    }
    Some((role, text))
}

fn session(
    path: PathBuf,
    agent: SessionAgent,
    budget: &mut Budget<'_>,
) -> io::Result<Option<HistorySession>> {
    let (head, partial) = read_window(&path, 256 * 1024, false, budget)?;
    let mut id = None;
    let mut cwd = None;
    let mut title = String::new();
    let mut summary = String::new();
    let tail = if partial {
        read_window(&path, 128 * 1024, true, budget)?.0
    } else {
        Vec::new()
    };
    for record in records(&head).chain(records(&tail)) {
        budget.row()?;
        match agent {
            SessionAgent::Codex if record["type"] == "session_meta" => {
                let meta = &record["payload"];
                if meta["source"].is_object()
                    || meta["thread_source"].is_object()
                    || meta["thread_source"] == "subagent"
                {
                    return Ok(None);
                }
                id = meta["id"]
                    .as_str()
                    .or_else(|| meta["session_id"].as_str())
                    .map(str::to_owned);
                cwd = meta["cwd"].as_str().map(PathBuf::from);
            }
            SessionAgent::Claude => {
                if record["isSidechain"] == true {
                    return Ok(None);
                }
                if id.is_none() {
                    id = record["sessionId"].as_str().map(str::to_owned);
                }
                if cwd.is_none() {
                    cwd = record["cwd"].as_str().map(PathBuf::from);
                }
                for key in ["summary", "customTitle", "aiTitle"] {
                    if let Some(value) = record[key].as_str().filter(|s| !s.trim().is_empty()) {
                        title = short(value);
                    }
                }
            }
            _ => {}
        }
        if let Some((role, text)) = message(&record, agent) {
            if title.is_empty() && role == "You" {
                title = short(&text);
            }
            summary = short(&text);
        }
    }
    let Some(id) = id
        .and_then(|id| uuid::Uuid::parse_str(&id).ok())
        .map(|id| id.to_string())
    else {
        return Ok(None);
    };
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    if title.is_empty() {
        title = id.clone();
    }
    Ok(Some(HistorySession {
        agent,
        id,
        title,
        summary,
        cwd,
        modified: fs::metadata(&path)?.modified()?,
        path,
    }))
}

/// Discover only native top-level sessions, never archives or subagent logs.
/// Call on a worker thread. A missing store is a valid empty history.
pub fn list_sessions(
    agent: SessionAgent,
    home: &Path,
    cancel: &AtomicBool,
    deadline: Instant,
) -> io::Result<Vec<HistorySession>> {
    let mut budget = Budget::new(cancel, deadline);
    budget.check()?;
    if matches!(
        agent,
        SessionAgent::OpenCode | SessionAgent::Antigravity | SessionAgent::Cline
    ) {
        return sqlite::list_sessions(agent, home, &mut budget);
    }
    let root = home.join(match agent {
        SessionAgent::Claude => "projects",
        SessionAgent::Codex => "sessions",
        _ => unreachable!(),
    });
    let mut paths = Vec::new();
    collect(
        &root,
        if agent == SessionAgent::Codex { 3 } else { 1 },
        &mut paths,
        &mut budget,
    )?;
    let mut sessions = HashMap::new();
    for path in paths {
        budget.check()?;
        if let Some(item) = session(path, agent, &mut budget)? {
            if sessions.len() >= MAX_SESSIONS && !sessions.contains_key(&item.id) {
                return Err(limit_error());
            }
            let existing = sessions
                .entry(item.id.clone())
                .or_insert_with(|| item.clone());
            if item.modified > existing.modified {
                *existing = item;
            }
        }
    }
    if agent == SessionAgent::Codex {
        // The append-only index holds user-assigned names; the last entry wins.
        let index = read_window(
            &home.join("session_index.jsonl"),
            4 * 1024 * 1024,
            true,
            &mut budget,
        );
        let bytes = match index {
            Ok((bytes, _)) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(error),
        };
        {
            for entry in records(&bytes) {
                budget.row()?;
                if let (Some(id), Some(title)) =
                    (entry["id"].as_str(), entry["thread_name"].as_str())
                {
                    if let Some(item) = sessions.get_mut(id) {
                        if !title.trim().is_empty() {
                            item.title = short(title);
                        }
                    }
                }
            }
        }
    }
    let mut sessions: Vec<_> = sessions.into_values().collect();
    sessions.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
    budget.check()?;
    Ok(sessions)
}

fn collect(
    directory: &Path,
    depth: usize,
    paths: &mut Vec<PathBuf>,
    budget: &mut Budget<'_>,
) -> io::Result<()> {
    budget.check()?;
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        budget.entry()?;
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() || reparse(&entry.metadata()?) {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() && depth > 0 {
            collect(&path, depth - 1, paths, budget)?;
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
            if paths.len() >= MAX_FILES {
                return Err(limit_error());
            }
            paths.push(path);
        }
    }
    Ok(())
}

// Whole-operation limits fail closed; a large/disconnected store never becomes
// a successful incomplete session list. OS filesystem calls are not preemptible.
const MAX_FILES: usize = 4096;
const MAX_ENTRIES: usize = 20_000;
const MAX_ROWS: usize = 200_000;
const MAX_SESSIONS: usize = 2048;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_COLUMN_BYTES: usize = 4 * 1024 * 1024;

fn limit_error() -> io::Error {
    invalid("Session history exceeds the bounded scan limit; no partial list is shown")
}
struct Budget<'a> {
    cancel: &'a AtomicBool,
    deadline: Instant,
    entries: usize,
    files: usize,
    rows: usize,
    bytes: usize,
}
impl<'a> Budget<'a> {
    fn new(cancel: &'a AtomicBool, deadline: Instant) -> Self {
        Self {
            cancel,
            deadline,
            entries: 0,
            files: 0,
            rows: 0,
            bytes: 0,
        }
    }
    fn check(&self) -> io::Result<()> {
        if self.cancel.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Session history read cancelled",
            ))
        } else if Instant::now() >= self.deadline {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Session history read timed out",
            ))
        } else {
            Ok(())
        }
    }
    fn entry(&mut self) -> io::Result<()> {
        self.check()?;
        self.entries += 1;
        if self.entries > MAX_ENTRIES {
            Err(limit_error())
        } else {
            Ok(())
        }
    }
    fn file(&mut self) -> io::Result<()> {
        self.check()?;
        self.files += 1;
        // Each transcript may have a head and tail window, plus the Codex index.
        if self.files > MAX_FILES * 2 + 1 {
            Err(limit_error())
        } else {
            Ok(())
        }
    }
    fn row(&mut self) -> io::Result<()> {
        self.check()?;
        self.rows += 1;
        if self.rows > MAX_ROWS {
            Err(limit_error())
        } else {
            Ok(())
        }
    }
    fn bytes(&mut self, count: usize) -> io::Result<()> {
        self.check()?;
        self.bytes = self.bytes.checked_add(count).ok_or_else(limit_error)?;
        if self.bytes > MAX_BYTES {
            Err(limit_error())
        } else {
            Ok(())
        }
    }
}
#[cfg(windows)]
fn reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn reparse(_: &fs::Metadata) -> bool {
    false
}
#[cfg(test)]
mod tests {
    pub(super) struct TempDir(PathBuf);
    impl TempDir {
        pub(super) fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("flowmux-sessions-한글-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        pub(super) fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn deadline() -> Instant {
        Instant::now() + std::time::Duration::from_secs(5)
    }
    fn list_sessions(agent: SessionAgent, home: &Path) -> io::Result<Vec<HistorySession>> {
        super::list_sessions(agent, home, &AtomicBool::new(false), deadline())
    }
    #[test]
    fn windows_home_and_provider_overrides_have_explicit_precedence() {
        let mut env = HashMap::from([
            ("USERPROFILE", PathBuf::from("C:/Users/한글")),
            ("HOME", PathBuf::from("D:/fallback")),
        ]);
        assert_eq!(
            SessionAgent::Claude.history_home(|key| env.get(key).cloned()),
            Some(PathBuf::from("C:/Users/한글/.claude"))
        );
        env.insert("CLAUDE_CONFIG_DIR", PathBuf::from("custom 한"));
        assert_eq!(
            SessionAgent::Claude.history_home(|key| env.get(key).cloned()),
            Some(PathBuf::from("custom 한"))
        );
        env.insert("CLAUDE_CONFIG_DIR", PathBuf::new());
        assert_eq!(
            SessionAgent::Claude.history_home(|key| env.get(key).cloned()),
            Some(PathBuf::from("C:/Users/한글/.claude"))
        );
        assert!(SessionAgent::Claude
            .home_variables()
            .contains(&"USERPROFILE"));
    }
    #[test]
    fn cancellation_deadline_and_oversized_history_return_errors_not_partial_lists() {
        let temp = TempDir::new();
        let cancel = AtomicBool::new(true);
        assert_eq!(
            super::list_sessions(SessionAgent::Claude, temp.path(), &cancel, deadline())
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        cancel.store(false, Ordering::Release);
        assert_eq!(
            super::list_sessions(SessionAgent::Claude, temp.path(), &cancel, Instant::now())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let directory = temp.path().join("sessions");
        fs::create_dir(&directory).unwrap();
        let file = File::create(directory.join("large.jsonl")).unwrap();
        file.set_len(MAX_FILE_BYTES + 1).unwrap();
        drop(file);
        assert_eq!(
            list_sessions(SessionAgent::Codex, temp.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        let mut budget = Budget::new(&cancel, deadline());
        budget.bytes = MAX_BYTES;
        assert!(budget.bytes(1).is_err());
        let mut budget = Budget::new(&cancel, deadline());
        budget.entries = MAX_ENTRIES;
        assert!(budget.entry().is_err());
        let mut budget = Budget::new(&cancel, deadline());
        budget.rows = MAX_ROWS;
        assert!(budget.row().is_err());
    }

    #[test]
    fn native_history_directories_follow_override_precedence() {
        let mut env = std::collections::HashMap::from([("HOME", PathBuf::from("/home/test"))]);
        for (agent, path) in [
            (SessionAgent::OpenCode, "/home/test/.local/share/opencode"),
            (
                SessionAgent::Antigravity,
                "/home/test/.gemini/antigravity-cli",
            ),
            (SessionAgent::Cline, "/home/test/.cline/data/db"),
        ] {
            assert_eq!(
                agent.history_home(|key| env.get(key).cloned()),
                Some(path.into())
            );
        }
        env.insert("XDG_DATA_HOME", "/custom/data".into());
        assert_eq!(
            SessionAgent::OpenCode.history_home(|key| env.get(key).cloned()),
            Some("/custom/data/opencode".into())
        );
        for (key, value, expected) in [
            ("CLINE_DIR", "/config", "/config/data/db"),
            ("CLINE_DATA_DIR", "/data", "/data/db"),
            ("CLINE_DB_DATA_DIR", "/database", "/database"),
        ] {
            env.insert(key, value.into());
            assert_eq!(
                SessionAgent::Cline.history_home(|key| env.get(key).cloned()),
                Some(expected.into())
            );
        }
    }

    use super::*;
    use serde_json::json;

    const ID: &str = "12345678-1234-4234-8234-123456789abc";
    fn write(path: &Path, values: &[Value]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(path, format!("{text}\n")).unwrap();
    }

    #[test]
    fn canonical_session_ids_reject_shell_and_path_input() {
        for agent in [
            SessionAgent::Claude,
            SessionAgent::Codex,
            SessionAgent::Antigravity,
            SessionAgent::Cline,
        ] {
            assert_eq!(agent.canonical_session_id(&ID.to_uppercase()).unwrap(), ID);
            for value in ["", "../session", "ses_other", "한글", "id;echo", "id\n"] {
                assert!(agent.canonical_session_id(value).is_err());
            }
        }
        assert_eq!(
            SessionAgent::OpenCode
                .canonical_session_id("ses_Ab12")
                .unwrap(),
            "ses_Ab12"
        );
        for value in ["ses_", "ses_../x", "ses_a-b", "ses_한글", ID] {
            assert!(SessionAgent::OpenCode.canonical_session_id(value).is_err());
        }
        assert!(SessionAgent::OpenCode
            .canonical_session_id(&format!("ses_{}", "a".repeat(125)))
            .is_err());
    }

    #[test]
    fn codex_names_unicode_preview_and_native_command() {
        let home = TempDir::new();
        let path = home.path().join("sessions/2026/09/16/rollout.jsonl");
        write(
            &path,
            &[
                json!({"type":"session_meta","payload":{"id":ID,"cwd":"/한글 path","source":"cli"}}),
                json!({"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"hidden instructions"}]}}),
                json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"한글 질문 🦀"},{"type":"input_image"}]}}),
                json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"답변\n두 번째 줄"}]}}),
                json!({"type":"event_msg","payload":{"type":"agent_message","message":"duplicate"}}),
            ],
        );
        write(
            &home.path().join("session_index.jsonl"),
            &[
                json!({"id":ID,"thread_name":"Old"}),
                json!({"id":ID,"thread_name":"새 이름"}),
            ],
        );
        let items = list_sessions(SessionAgent::Codex, home.path()).unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.title, "새 이름");
        assert_eq!(item.summary, "답변 두 번째 줄");
        assert_eq!(item.cwd, Path::new("/한글 path"));
        assert_eq!(
            item.resume_argv().unwrap().join(" "),
            format!("codex resume {ID}")
        );
        assert_eq!(
            item.preview(&AtomicBool::new(false), deadline()).unwrap(),
            "You\n한글 질문 🦀\n[Image]\n\nAssistant\n답변\n두 번째 줄\n\n"
        );
        let mut invalid = item.clone();
        for id in [
            "",
            "--last",
            "abc\r/quit",
            "$(touch /tmp/injected)",
            "../../../other",
        ] {
            invalid.id = id.into();
            assert!(invalid.resume_argv().is_err());
        }
    }

    #[test]
    fn claude_filters_tools_children_and_accepts_string_and_block_content() {
        let home = TempDir::new();
        let values = [
            json!({"type":"user","sessionId":ID,"cwd":"/repo","message":{"role":"user","content":"question"}}),
            json!({"type":"user","sessionId":ID,"message":{"role":"user","content":[{"type":"tool_result","content":"secret tool output"}]}}),
            json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"answer\u{1b}[31m"}]}}),
            json!({"type":"custom-title","customTitle":"Title","sessionId":ID}),
        ];
        write(&home.path().join("projects/-repo/session.jsonl"), &values);
        write(
            &home.path().join("projects/-repo/subagents/child.jsonl"),
            &values,
        );
        write(
            &home.path().join("projects/-repo/child.jsonl"),
            &[
                json!({"sessionId":"23456789-1234-4234-8234-123456789abc","cwd":"/repo","isSidechain":true}),
            ],
        );
        let items = list_sessions(SessionAgent::Claude, home.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Title");
        assert_eq!(
            items[0].resume_argv().unwrap().join(" "),
            format!("claude --resume {ID}")
        );
        assert_eq!(
            items[0]
                .preview(&AtomicBool::new(false), deadline())
                .unwrap(),
            "You\nquestion\n\nAssistant\nanswer[31m\n\n"
        );
    }

    #[test]
    fn malformed_missing_archived_subagent_and_symlink_inputs() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;
        let home = TempDir::new();
        assert!(list_sessions(SessionAgent::Codex, home.path())
            .unwrap()
            .is_empty());
        let values =
            [json!({"type":"session_meta","payload":{"id":ID,"cwd":"/repo","source":"cli"}})];
        write(&home.path().join("archived_sessions/old.jsonl"), &values);
        write(
            &home.path().join("sessions/child.jsonl"),
            &[
                json!({"type":"session_meta","payload":{"id":ID,"cwd":"/repo","source":{"subagent":{}}}}),
            ],
        );
        fs::write(
            home.path().join("sessions/broken.jsonl"),
            b"{bad}\n\xff\xfe\n{\"partial\"",
        )
        .unwrap();
        #[cfg(unix)]
        symlink(
            home.path().join("archived_sessions/old.jsonl"),
            home.path().join("sessions/link.jsonl"),
        )
        .unwrap();
        assert!(list_sessions(SessionAgent::Codex, home.path())
            .unwrap()
            .is_empty());
        write(&home.path().join("sessions/good.jsonl"), &values);
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open(home.path().join("sessions/good.jsonl"))
            .unwrap();
        f.write_all(b"{bad}\n{\"partial\"").unwrap();
        assert_eq!(
            list_sessions(SessionAgent::Codex, home.path())
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn large_records_are_bounded_and_recent_messages_survive() {
        let home = TempDir::new();
        let path = home.path().join("sessions/large.jsonl");
        write(
            &path,
            &[
                json!({"type":"session_meta","payload":{"id":ID,"cwd":"/repo","source":"cli"}}),
                json!({"type":"tool_output","text":"x".repeat(3 * 1024 * 1024)}),
                json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"recent"}]}}),
            ],
        );
        let items = list_sessions(SessionAgent::Codex, home.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].summary, "recent");
        let preview = items[0]
            .preview(&AtomicBool::new(false), deadline())
            .unwrap();
        assert!(preview.starts_with("Earlier content omitted"));
        assert!(preview.ends_with("Assistant\nrecent\n\n"));
        assert!(preview.len() < 200);
        fs::remove_file(path).unwrap();
        assert!(items[0]
            .preview(&AtomicBool::new(false), deadline())
            .is_err());
    }
}
