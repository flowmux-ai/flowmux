// SPDX-License-Identifier: GPL-3.0-or-later
//! Worktree data and NUL-delimited Git parsers; no process or UI dependencies.
use serde::Serialize;
use std::path::PathBuf;

pub const MAX_ROWS: usize = 256;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Changes {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
}
impl Changes {
    pub fn is_clean(&self) -> bool {
        self.staged == 0 && self.unstaged == 0 && self.untracked == 0
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Info {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub commit_subject: Option<String>,
    pub commit_time: Option<i64>,
    pub changes: Option<Changes>,
    pub is_main: bool,
    pub is_current: bool,
    pub is_bare: bool,
    pub lock_reason: Option<String>,
    pub prunable_reason: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct List {
    pub repository_root: PathBuf,
    pub current_worktree: PathBuf,
    pub items: Vec<Info>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub enum RemoveError {
    RequiresForce(String),
    Locked(String),
    Failed(String),
}
impl std::fmt::Display for RemoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RequiresForce(message) | Self::Locked(message) | Self::Failed(message) => {
                f.write_str(message)
            }
        }
    }
}
impl std::error::Error for RemoveError {}

fn text(bytes: &[u8]) -> Result<String, String> {
    String::from_utf8(bytes.to_vec()).map_err(|_| "Git returned invalid UTF-8".into())
}
fn path(bytes: &[u8]) -> Result<PathBuf, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into())
    }
    #[cfg(not(unix))]
    {
        text(bytes).map(PathBuf::from)
    }
}

pub fn parse_porcelain(bytes: &[u8]) -> Result<Vec<Info>, String> {
    if !bytes.is_empty() && !bytes.ends_with(&[0, 0]) {
        return Err("Incomplete Git worktree record".into());
    }
    let mut rows = Vec::new();
    let mut current: Option<Info> = None;
    for field in bytes.split(|byte| *byte == 0) {
        if field.is_empty() {
            if let Some(mut row) = current.take() {
                if row.path.as_os_str().is_empty() || rows.len() >= MAX_ROWS {
                    return Err("Empty worktree path or more than 256 worktrees".into());
                }
                if rows.iter().any(|other: &Info| other.path == row.path) {
                    return Err("Duplicate worktree path".into());
                }
                row.is_main = rows.is_empty();
                rows.push(row);
            }
            continue;
        }
        let (key, value) = field
            .iter()
            .position(|byte| *byte == b' ')
            .map_or((field, &[][..]), |at| (&field[..at], &field[at + 1..]));
        if key == b"worktree" {
            if current.is_some() {
                return Err("Missing Git worktree separator".into());
            }
            current = Some(Info {
                path: path(value)?,
                ..Info::default()
            });
            continue;
        }
        let row = current.as_mut().ok_or("Git field precedes worktree path")?;
        match key {
            b"HEAD" => {
                if !matches!(value.len(), 40 | 64) || !value.iter().all(u8::is_ascii_hexdigit) {
                    return Err("Invalid Git worktree HEAD".into());
                }
                row.head = text(value)?;
            }
            b"branch" => {
                row.branch = Some(text(value.strip_prefix(b"refs/heads/").unwrap_or(value))?)
            }
            b"detached" => row.branch = None,
            b"bare" => row.is_bare = true,
            b"locked" => row.lock_reason = Some(text(value)?),
            b"prunable" => row.prunable_reason = Some(text(value)?),
            _ => {} // Git can extend porcelain with additional attributes.
        }
    }
    Ok(rows)
}

pub fn parse_status(bytes: &[u8]) -> Result<Changes, String> {
    if !bytes.is_empty() && !bytes.ends_with(&[0]) {
        return Err("Incomplete Git status record".into());
    }
    let mut fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let mut changes = Changes::default();
    while let Some(field) = fields.next() {
        match field.first() {
            Some(b'?') if field.starts_with(b"? ") => changes.untracked += 1,
            Some(kind @ (b'1' | b'2' | b'u'))
                if field.len() >= 5 && field[1] == b' ' && field[4] == b' ' =>
            {
                changes.staged += usize::from(field[2] != b'.');
                changes.unstaged += usize::from(field[3] != b'.');
                if *kind == b'2' && fields.next().is_none() {
                    return Err("Missing renamed Git status path".into());
                }
            }
            Some(b'!' | b'#') => {}
            _ => return Err("Invalid Git status record".into()),
        }
    }
    Ok(changes)
}

pub fn parse_commit(bytes: &[u8]) -> (Option<String>, Option<i64>) {
    let mut fields = bytes.split(|byte| *byte == 0);
    let subject = fields
        .next()
        .and_then(|field| text(field).ok())
        .filter(|s| !s.is_empty());
    let time = fields
        .next()
        .and_then(|field| std::str::from_utf8(field).ok())
        .and_then(|field| field.trim().parse().ok());
    (subject, time)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn porcelain_preserves_unicode_and_separates_flags_from_path_bytes() {
        let name = "/repo/한글 한 😀 \"branch\"\nline";
        let bytes = format!("worktree {name}\0HEAD {}\0branch refs/heads/한글\0locked reason\0\0worktree /other\0bare\0prunable gone\0\0", "a".repeat(40));
        let rows = parse_porcelain(bytes.as_bytes()).unwrap();
        assert_eq!(rows[0].path, PathBuf::from(name));
        assert_eq!(rows[0].branch.as_deref(), Some("한글"));
        assert_eq!(rows[0].lock_reason.as_deref(), Some("reason"));
        assert!(rows[0].is_main && !rows[1].is_main && rows[1].is_bare);
        assert!(parse_porcelain(b"worktree /x\0HEAD bad\0\0").is_err());
        assert!(parse_porcelain(b"worktree /x\0").is_err());
        let too_many: String = (0..=MAX_ROWS)
            .map(|index| format!("worktree /tree-{index}\0\0"))
            .collect();
        assert!(parse_porcelain(too_many.as_bytes()).is_err());
    }
    #[test]
    fn status_counts_rename_once_and_never_interprets_original_filename_as_status() {
        let changes =
            parse_status(b"2 R. metadata new\0? original name\0u UU metadata path\0? loose\0")
                .unwrap();
        assert_eq!(
            changes,
            Changes {
                staged: 2,
                unstaged: 1,
                untracked: 1
            }
        );
        assert!(parse_status(b"2 R. metadata new\0").is_err());
        assert!(parse_status(b"? partial").is_err());
        assert!(parse_status(b"").unwrap().is_clean());
    }
}
