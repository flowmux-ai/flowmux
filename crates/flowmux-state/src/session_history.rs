// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only discovery of native agent session histories.

use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod sqlite;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAgent {
    Claude,
    Codex,
    OpenCode,
    Antigravity,
    Cline,
    Gemini,
}

impl SessionAgent {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "claude" | "claude code" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "opencode" => Some(Self::OpenCode),
            "agy" | "antigravity" => Some(Self::Antigravity),
            "cline" => Some(Self::Cline),
            "gemini" => Some(Self::Gemini),
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
            Self::Gemini => "Gemini",
        }
    }

    pub fn home_variables(self) -> &'static [&'static str] {
        match self {
            Self::Claude => &["HOME", "CLAUDE_CONFIG_DIR"],
            Self::Codex => &["HOME", "CODEX_HOME"],
            Self::OpenCode => &[
                "HOME",
                "XDG_DATA_HOME",
                "XDG_CONFIG_HOME",
                "XDG_STATE_HOME",
                "XDG_CACHE_HOME",
                "OPENCODE_CONFIG",
                "OPENCODE_CONFIG_DIR",
            ],
            Self::Antigravity => &["HOME"],
            Self::Cline => &[
                "HOME",
                "CLINE_DIR",
                "CLINE_DATA_DIR",
                "CLINE_DB_DATA_DIR",
                "CLINE_SESSION_DATA_DIR",
            ],
            Self::Gemini => &["HOME", "GEMINI_CLI_HOME"],
        }
    }

    /// [`Self::history_home`] for this process's environment.
    pub fn env_history_home(self) -> Option<PathBuf> {
        self.history_home(|key| std::env::var_os(key).map(PathBuf::from))
    }

    pub fn history_home(self, value: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
        let home = || value("HOME");
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
            Self::Gemini => value("GEMINI_CLI_HOME")
                .or_else(home)
                .map(|p| p.join(".gemini")),
        }
    }
}

#[derive(Clone, Debug)]
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
    pub fn resume_command(&self) -> io::Result<String> {
        if self.agent == SessionAgent::OpenCode {
            if !self.id.starts_with("ses_")
                || self.id.len() > 128
                || self.id.len() <= 4
                || !self
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            {
                return Err(invalid("Invalid OpenCode session ID"));
            }
            return Ok(format!("opencode --session {}", self.id));
        }
        let id = uuid::Uuid::parse_str(&self.id)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Invalid session ID"))?;
        Ok(match self.agent {
            SessionAgent::Claude => format!("claude --resume {id}"),
            SessionAgent::Codex => format!("codex resume {id}"),
            SessionAgent::Antigravity => format!("agy --conversation {id}"),
            SessionAgent::Cline => format!("cline --id {id} --tui"),
            SessionAgent::Gemini => format!("gemini --resume {id}"),
            SessionAgent::OpenCode => unreachable!(),
        })
    }

    pub fn preview(&self) -> io::Result<String> {
        if matches!(
            self.agent,
            SessionAgent::OpenCode | SessionAgent::Antigravity | SessionAgent::Cline
        ) {
            return sqlite::preview(self);
        }
        // ponytail: bound GTK preview memory to the latest 2 MiB; use paged
        // transcript loading if browsing older messages becomes necessary.
        let (bytes, partial) = read_window(&self.path, 2 * 1024 * 1024, true)?;
        let mut text = if partial {
            "Earlier content omitted; showing the latest part of this session.\n\n".to_string()
        } else {
            String::new()
        };
        let header = text.len();
        let gemini = self.agent == SessionAgent::Gemini;
        // Gemini re-appends its latest message under the same `id` as it gains tool calls.
        let mut latest = (String::new(), header);
        for record in records(&bytes) {
            let nested = gemini
                .then(|| record["$set"]["messages"].as_array())
                .flatten();
            if nested.is_some() {
                // Gemini replaces its whole history with `$set.messages` on resume.
                // ponytail: `$rewindTo` is ignored, so rewound messages stay visible;
                // track message IDs here if that becomes confusing.
                text.truncate(header);
            }
            for record in nested.into_iter().flatten().chain([&record]) {
                if let Some((role, message)) = message(record, self.agent) {
                    if let Some(id) = record["id"].as_str().filter(|_| gemini) {
                        if id == latest.0 {
                            text.truncate(latest.1);
                        }
                        latest = (id.to_owned(), text.len());
                    }
                    text.push_str(role);
                    text.push('\n');
                    text.push_str(&message);
                    text.push_str("\n\n");
                }
            }
        }
        if text.is_empty() {
            text.push_str("No readable conversation messages in this session.");
        }
        Ok(text)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

/// Read complete JSONL records without allocating in proportion to file size.
fn read_window(path: &Path, limit: usize, tail: bool) -> io::Result<(Vec<u8>, bool)> {
    let mut file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("Session transcript is not a regular file"));
    }
    let len = file.metadata()?.len();
    let partial = len > limit as u64;
    if tail && partial {
        file.seek(SeekFrom::Start(len - limit as u64))?;
    }
    let mut bytes = Vec::new();
    file.take(limit as u64).read_to_end(&mut bytes)?;
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
        SessionAgent::Gemini => {
            let role = match record["type"].as_str()? {
                "user" => "You",
                "gemini" => "Assistant",
                _ => return None,
            };
            // Parts carry `text` without a `type`; tool data is omitted.
            let text = match &record["content"] {
                Value::String(text) => clean(text),
                parts => parts
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|part| {
                        part["text"]
                            .as_str()
                            .map(clean)
                            .or_else(|| part.get("inlineData").map(|_| "[Image]".into()))
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            let start = text.trim_start();
            if start.is_empty()
                || start.starts_with("<session_context>")
                || start.starts_with("<hook_context>")
            {
                return None;
            }
            return Some((role, text));
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

fn session(path: PathBuf, agent: SessionAgent) -> io::Result<Option<HistorySession>> {
    let (head, partial) = read_window(&path, 256 * 1024, false)?;
    let mut id = None;
    let mut cwd = None;
    let mut title = String::new();
    let mut summary = String::new();
    let tail = if partial {
        read_window(&path, 128 * 1024, true)?.0
    } else {
        Vec::new()
    };
    for record in records(&head).chain(records(&tail)) {
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
            SessionAgent::Gemini => {
                if record["kind"].as_str().is_some_and(|kind| kind != "main") {
                    return Ok(None);
                }
                if id.is_none() {
                    id = record["sessionId"].as_str().map(str::to_owned);
                }
                // Appended as `$set.summary`; rewritten transcripts keep it in the header.
                if let Some(value) = record["$set"]["summary"]
                    .as_str()
                    .or_else(|| record["summary"].as_str())
                    .filter(|s| !s.trim().is_empty())
                {
                    title = short(value);
                }
            }
            _ => {}
        }
        // Gemini's `$set.messages` holds its history so far.
        let nested = (agent == SessionAgent::Gemini)
            .then(|| record["$set"]["messages"].as_array())
            .flatten();
        for record in nested.into_iter().flatten().chain([&record]) {
            if let Some((role, text)) = message(record, agent) {
                if title.is_empty() && role == "You" {
                    title = short(&text);
                }
                summary = short(&text);
            }
        }
    }
    if agent == SessionAgent::Gemini {
        // Every launch records a context-only transcript; Gemini cannot resume those.
        if summary.is_empty() {
            return Ok(None);
        }
        // Gemini keeps the project directory beside `chats/`, not in the transcript.
        cwd = path
            .parent()
            .and_then(Path::parent)
            .and_then(|project| read_window(&project.join(".project_root"), 4096, false).ok())
            .and_then(|(bytes, _)| String::from_utf8(bytes).ok())
            .map(|root| PathBuf::from(root.trim()))
            .filter(|root| root.is_absolute());
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
pub fn list_sessions(agent: SessionAgent, home: &Path) -> io::Result<Vec<HistorySession>> {
    if matches!(
        agent,
        SessionAgent::OpenCode | SessionAgent::Antigravity | SessionAgent::Cline
    ) {
        return sqlite::list_sessions(agent, home);
    }
    let root = home.join(match agent {
        SessionAgent::Claude => "projects",
        SessionAgent::Codex => "sessions",
        // `tmp/<project>/chats/session-*.jsonl`
        SessionAgent::Gemini => "tmp",
        _ => unreachable!(),
    });
    let mut paths = Vec::new();
    collect(
        &root,
        match agent {
            SessionAgent::Codex => 3,
            SessionAgent::Gemini => 2,
            _ => 1,
        },
        &mut paths,
    )?;
    if agent == SessionAgent::Gemini {
        paths.retain(|path| {
            path.parent().is_some_and(|dir| dir.ends_with("chats"))
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("session-"))
        });
    }
    let mut sessions = HashMap::new();
    for path in paths {
        if let Ok(Some(item)) = session(path, agent) {
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
        if let Ok((bytes, _)) =
            read_window(&home.join("session_index.jsonl"), 4 * 1024 * 1024, true)
        {
            for entry in records(&bytes) {
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
    Ok(sessions)
}

/// Thread name in a terminal tab title that Codex set. Once a thread has a
/// name the title reads `<thread name> | <project>`, behind a spinner while
/// working.
pub fn codex_title_thread(title: &str) -> Option<&str> {
    let name = title.rsplit_once(" | ")?.0;
    Some(name.trim_matches(|c: char| c.is_whitespace() || ('\u{2800}'..='\u{28ff}').contains(&c)))
}

/// Codex sessions with their current thread names, oldest first.
fn codex_thread_names(home: &Path) -> Vec<(String, String)> {
    let index = read_window(&home.join("session_index.jsonl"), 4 * 1024 * 1024, true);
    let mut names: Vec<(String, String)> =
        records(&index.map(|(bytes, _)| bytes).unwrap_or_default())
            .filter_map(|entry| {
                let id = entry["id"].as_str()?.to_owned();
                Some((id, entry["thread_name"].as_str()?.trim().to_owned()))
            })
            .collect();
    // Append-only: the last entry of a session holds its current name.
    let mut seen = std::collections::HashSet::new();
    names.reverse();
    names.retain(|(id, _)| seen.insert(id.clone()));
    names.reverse();
    names
}

/// Directory a Codex session was started in.
pub fn codex_session_cwd(home: &Path, id: &str) -> Option<PathBuf> {
    let mut paths = Vec::new();
    collect(&home.join("sessions"), 3, &mut paths).ok()?;
    let suffix = format!("{id}.jsonl");
    let path = paths
        .into_iter()
        .find(|path| path.to_string_lossy().ends_with(&suffix))?;
    Some(session(path, SessionAgent::Codex).ok()??.cwd)
}

/// Codex session behind a terminal tab title that Codex set: the newest
/// session with the title's thread name that was started in `cwd`.
pub fn codex_session_for_title(home: &Path, title: &str, cwd: &Path) -> Option<String> {
    let name = codex_title_thread(title)?;
    let mut names = codex_thread_names(home);
    names.retain(|(_, known)| known == name);
    names
        .into_iter()
        .rev()
        .find_map(|(id, _)| (codex_session_cwd(home, &id)? == cwd).then_some(id))
}

/// Current thread name of a Codex session, as its tab title shows it.
pub fn codex_thread_name(home: &Path, id: &str) -> Option<String> {
    let names = codex_thread_names(home);
    names
        .into_iter()
        .find_map(|(known, name)| (known == id).then_some(name))
}

fn collect(directory: &Path, depth: usize, paths: &mut Vec<PathBuf>) -> io::Result<()> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() && depth > 0 {
            collect(&path, depth - 1, paths)?;
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "jsonl") {
            paths.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
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
            (SessionAgent::Gemini, "/home/test/.gemini"),
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
    fn codex_session_for_title_matches_current_name_and_directory() {
        const OTHER: &str = "22222222-2222-4222-8222-222222222222";
        const RENAMED: &str = "33333333-3333-4333-8333-333333333333";
        let home = tempfile::tempdir().unwrap();
        for (id, cwd) in [
            (ID, "/work/app"),
            (OTHER, "/work/other"),
            (RENAMED, "/work/app"),
        ] {
            write(
                &home.path().join(format!(
                    "sessions/2026/10/02/rollout-2026-10-02T12-00-00-{id}.jsonl"
                )),
                &[json!({"type":"session_meta","payload":{"id":id,"cwd":cwd,"source":"cli"}})],
            );
        }
        write(
            &home.path().join("session_index.jsonl"),
            &[
                json!({"id":RENAMED,"thread_name":"한글 작업"}),
                json!({"id":ID,"thread_name":"한글 작업"}),
                json!({"id":OTHER,"thread_name":"한글 작업"}),
                json!({"id":RENAMED,"thread_name":"Renamed"}),
            ],
        );
        let find =
            |title: &str, cwd: &str| codex_session_for_title(home.path(), title, Path::new(cwd));

        assert_eq!(find("한글 작업 | app", "/work/app").as_deref(), Some(ID));
        assert_eq!(find("⠋ 한글 작업 | app", "/work/app").as_deref(), Some(ID));
        assert_eq!(
            find("한글 작업 | other", "/work/other").as_deref(),
            Some(OTHER)
        );
        assert_eq!(find("Renamed | app", "/work/app").as_deref(), Some(RENAMED));
        assert_eq!(find("한글 작업 | app", "/work/elsewhere"), None);
        assert_eq!(find("한글 작업", "/work/app"), None);
        assert_eq!(find("app", "/work/app"), None);

        let name = codex_thread_name(home.path(), RENAMED);
        assert_eq!(name.as_deref(), Some("Renamed"));
        assert_eq!(codex_thread_name(home.path(), "unknown"), None);
    }

    #[test]
    fn codex_names_unicode_preview_and_native_command() {
        let home = tempfile::tempdir().unwrap();
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
        assert_eq!(item.resume_command().unwrap(), format!("codex resume {ID}"));
        assert_eq!(
            item.preview().unwrap(),
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
            assert!(invalid.resume_command().is_err());
        }
    }

    #[test]
    fn gemini_reads_project_root_native_summary_and_resumed_history() {
        let home = tempfile::tempdir().unwrap();
        let project = home.path().join("tmp/proj");
        let context =
            json!({"id":"c","type":"user","content":[{"text":"<session_context>\nhidden"}]});
        let question = json!({"id":"q","type":"user","content":[{"text":"한글 질문 🦀"},{"inlineData":{"mimeType":"image/png"}}]});
        let answer =
            json!({"id":"a","type":"gemini","content":"답변","thoughts":[{"subject":"private"}]});
        write(
            &project.join("chats/session-2026-10-02T01-38-12345678.jsonl"),
            &[
                json!({"sessionId":ID,"startTime":"2026-10-02T01:38:53.141Z","kind":"main"}),
                json!({"$set":{"messages":[context.clone()]}}),
                question.clone(),
                json!({"$set":{"lastUpdated":"2026-10-02T01:38:53.649Z"}}),
                answer.clone(),
                // The latest message is appended again as it gains tool calls.
                json!({"id":"a","type":"gemini","content":"답변","toolCalls":[{"name":"edit"}]}),
                // A resume rewrites the history so far, then appends.
                json!({"$set":{"messages":[context.clone(), question.clone(), answer]}}),
                json!({"id":"i","type":"info","content":"hidden notice"}),
                json!({"id":"q2","type":"user","content":[{"text":"second"}]}),
                json!({"id":"a2","type":"gemini","content":"둘째 답"}),
                json!({"id":"a2","type":"gemini","content":"둘째 답변","toolCalls":[{"name":"edit"}]}),
                json!({"$set":{"summary":"Native summary"}}),
            ],
        );
        fs::write(project.join(".project_root"), "/한글 path\n").unwrap();
        write(
            &project.join("chats/session-2026-10-02T01-39-23456789.jsonl"),
            &[
                json!({"sessionId":"23456789-1234-4234-8234-123456789abc","kind":"subagent"}),
                json!({"id":"q","type":"user","content":[{"text":"child"}]}),
            ],
        );
        write(
            &project.join("chats/session-2026-10-02T01-44-56789abc.jsonl"),
            &[
                json!({"sessionId":"56789abc-1234-4234-8234-123456789abc","kind":"main"}),
                json!({"$set":{"messages":[context]}}),
            ],
        );
        // Not a session transcript, and a project without a recorded directory.
        write(
            &project.join("logs/session-other.jsonl"),
            &[json!({"sessionId":"3456789a-1234-4234-8234-123456789abc","kind":"main"})],
        );
        write(
            &home.path().join("tmp/rootless/chats/session-x.jsonl"),
            &[
                json!({"sessionId":"456789ab-1234-4234-8234-123456789abc","kind":"main"}),
                question,
            ],
        );
        let items = list_sessions(SessionAgent::Gemini, home.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Native summary");
        assert_eq!(items[0].summary, "둘째 답변");
        assert_eq!(items[0].cwd, Path::new("/한글 path"));
        assert_eq!(
            items[0].resume_command().unwrap(),
            format!("gemini --resume {ID}")
        );
        assert_eq!(
            items[0].preview().unwrap(),
            "You\n한글 질문 🦀\n[Image]\n\nAssistant\n답변\n\nYou\nsecond\n\nAssistant\n둘째 답변\n\n"
        );
        assert!(
            list_sessions(SessionAgent::Gemini, &home.path().join("missing"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn claude_filters_tools_children_and_accepts_string_and_block_content() {
        let home = tempfile::tempdir().unwrap();
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
            items[0].resume_command().unwrap(),
            format!("claude --resume {ID}")
        );
        assert_eq!(
            items[0].preview().unwrap(),
            "You\nquestion\n\nAssistant\nanswer[31m\n\n"
        );
    }

    #[test]
    fn malformed_missing_archived_subagent_and_symlink_inputs() {
        use std::os::unix::fs::symlink;
        let home = tempfile::tempdir().unwrap();
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
        let home = tempfile::tempdir().unwrap();
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
        let preview = items[0].preview().unwrap();
        assert!(preview.starts_with("Earlier content omitted"));
        assert!(preview.ends_with("Assistant\nrecent\n\n"));
        assert!(preview.len() < 200);
        fs::remove_file(path).unwrap();
        assert!(items[0].preview().is_err());
    }
}
