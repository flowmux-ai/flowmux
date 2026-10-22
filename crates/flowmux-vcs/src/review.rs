// SPDX-License-Identifier: GPL-3.0-or-later
//! Read-only Git review snapshots. Call from a blocking worker, never GTK.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;
const DISPLAY_LINE_LIMIT: usize = 100_000;
pub mod history;
pub mod notes;
mod repository;
pub use repository::Repository;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Scope {
    /// Net change from the branch base to the working tree, including new files.
    /// An empty reference selects the repository's default branch, or HEAD.
    AllChanges(String),
    WorkingTree,
    Unstaged,
    Staged,
    Branch(String),
    /// One immutable commit, compared with its first parent (or the empty tree).
    Commit(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: PathBuf,
    pub previous_path: Option<PathBuf>,
    pub status: String,
    pub untracked: bool,
}

impl File {
    pub fn label(&self) -> String {
        display_path(&self.path)
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub root: PathBuf,
    repository: Repository,
    pub scope: Scope,
    /// Resolved OID, so a moving branch cannot silently change the comparison.
    pub base: Option<String>,
    pub tip: Option<String>,
    pub commit_message: Option<String>,
    pub files: Vec<File>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Patch {
    pub text: String,
    pub lines: Vec<Line>,
}

pub fn display_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .flat_map(|c| {
            if c.is_control() {
                c.escape_default().collect::<Vec<_>>()
            } else {
                vec![c]
            }
        })
        .collect()
}

fn git_args() -> Vec<OsString> {
    args(&[
        "git",
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "color.ui=false",
        // The line parser needs the context prefix, including on blank lines.
        "-c",
        "diff.suppressBlankEmpty=false",
    ])
}

fn git(root: &Repository, args: &[OsString], allow_difference: bool) -> Result<Vec<u8>, String> {
    let mut command = git_args();
    command.extend_from_slice(args);
    run(&mut root.command(&command)?, allow_difference)
}

fn run(command: &mut Command, allow_difference: bool) -> Result<Vec<u8>, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start Git: {e}"))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_exceeded = exceeded.clone();
    let stderr_exceeded = exceeded.clone();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(OUTPUT_LIMIT as u64 + 1).read_to_end(&mut bytes);
        if bytes.len() > OUTPUT_LIMIT {
            stdout_exceeded.store(true, Ordering::Relaxed);
        }
        result.map(|_| bytes)
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stderr.take(64 * 1024 + 1).read_to_end(&mut bytes);
        if bytes.len() > 64 * 1024 {
            stderr_exceeded.store(true, Ordering::Relaxed);
        }
        result.map(|_| bytes)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None)
                if started.elapsed() < Duration::from_secs(30)
                    && !exceeded.load(Ordering::Relaxed) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) if started.elapsed() < Duration::from_secs(30) => {
                // A capped reader can finish while Git is blocked writing.
                let _ = child.kill();
                break child.wait().map_err(|e| e.to_string());
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err("Git timed out after 30 seconds. Refresh to retry.".into());
            }
            Err(e) => break Err(e.to_string()),
        }
    };
    let output = out
        .join()
        .map_err(|_| "Git reader failed")?
        .map_err(|e| e.to_string())?;
    let error = err
        .join()
        .map_err(|_| "Git error reader failed")?
        .map_err(|e| e.to_string())?;
    if output.len() > OUTPUT_LIMIT {
        return Err("This diff exceeds the 8 MiB display limit. Review a smaller change with Git or your editor.".into());
    }
    let status = status?;
    if !(status.success() || allow_difference && status.code() == Some(1)) {
        return Err(format!("Git: {}", String::from_utf8_lossy(&error).trim()));
    }
    Ok(output)
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn revision(root: &Repository, name: &str) -> Result<String, String> {
    let bytes = git(
        root,
        &args(&["rev-parse", "--verify", "--end-of-options", name]),
        false,
    )?;
    parse_oid(&bytes)
}

fn parse_oid(bytes: &[u8]) -> Result<String, String> {
    let oid = bytes.trim_ascii();
    if !matches!(oid.len(), 40 | 64) || !oid.iter().all(u8::is_ascii_hexdigit) {
        return Err("Invalid Git object ID. Check the remote shell for unexpected output.".into());
    }
    Ok(String::from_utf8(oid.to_vec()).expect("hexadecimal is UTF-8"))
}

fn empty_tree(root: &Repository) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&git(
        root,
        &args(&["hash-object", "-t", "tree", "--stdin"]),
        false,
    )?)
    .trim()
    .into())
}

pub fn repository_root(start: &Path) -> Result<PathBuf, String> {
    Ok(Repository::local(start).discover()?.root)
}

pub fn load(start: &Path, scope: Scope) -> Result<Snapshot, String> {
    Repository::local(start).load(scope)
}

fn load_repository(start: &Repository, scope: Scope) -> Result<Snapshot, String> {
    let root = start.discover()?;
    let scope = match scope {
        Scope::AllChanges(reference) if reference.trim().is_empty() => {
            Scope::AllChanges(default_base(&root))
        }
        Scope::Commit(reference) => {
            Scope::Commit(revision(&root, &format!("{reference}^{{commit}}"))?)
        }
        scope => scope,
    };
    let head_or_empty = || -> Result<String, String> {
        match revision(&root, "HEAD^{commit}") {
            Ok(oid) => Ok(oid),
            Err(_) => empty_tree(&root),
        }
    };
    let mut commit_message = None;
    let base = match &scope {
        Scope::Unstaged => None,
        Scope::Commit(oid) => {
            // Traversal commands hide parents at shallow boundaries. Read the
            // object header so missing history cannot look like a root commit.
            let object = git(&root, &args(&["cat-file", "-p", oid]), false)?;
            let object = String::from_utf8_lossy(&object);
            commit_message = object
                .split_once("\n\n")
                .map(|(_, message)| message.trim_end().to_string());
            let parent = object
                .lines()
                .take_while(|line| !line.is_empty())
                .find_map(|line| line.strip_prefix("parent "));
            Some(match parent {
                Some(parent) => revision(&root, &format!("{parent}^{{commit}}"))
                    .map_err(|_| "The parent commit is unavailable. Fetch the missing history before reviewing this commit.".to_string())?,
                None => empty_tree(&root)?,
            })
        }
        Scope::WorkingTree | Scope::Staged => Some(head_or_empty()?),
        Scope::AllChanges(reference) if reference == "HEAD" => Some(head_or_empty()?),
        Scope::Branch(reference) | Scope::AllChanges(reference) => {
            if reference.trim().is_empty() {
                return Err("Enter a base branch or commit.".into());
            }
            let oid = revision(&root, &format!("{}^{{commit}}", reference.trim()))?;
            Some(
                String::from_utf8_lossy(&git(&root, &args(&["merge-base", "HEAD", &oid]), false)?)
                    .trim()
                    .into(),
            )
        }
    };
    let tip = match &scope {
        Scope::Commit(oid) => Some(oid.clone()),
        Scope::Branch(_) => Some(revision(&root, "HEAD^{commit}")?),
        _ => None,
    };
    let mut snapshot = Snapshot {
        root: root.root.clone(),
        repository: root,
        scope,
        base,
        tip,
        commit_message,
        files: Vec::new(),
    };
    let mut command = snapshot.diff_args();
    command.extend(args(&["--name-status", "-z", "--"]));
    snapshot.files = parse_files(&git(&snapshot.repository, &command, false)?)?;
    if matches!(
        snapshot.scope,
        Scope::AllChanges(_) | Scope::WorkingTree | Scope::Unstaged
    ) {
        let untracked = git(
            &snapshot.repository,
            &args(&["ls-files", "--others", "--exclude-standard", "-z"]),
            false,
        )?;
        let tracked_paths: std::collections::HashMap<_, _> = snapshot
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| (file.path.clone(), index))
            .collect();
        for path in untracked.split(|b| *b == 0).filter(|s| !s.is_empty()) {
            let path = PathBuf::from(OsString::from_vec(path.to_vec()));
            if let Some(&index) = tracked_paths.get(&path) {
                let file = &mut snapshot.files[index];
                // A staged deletion may have a new untracked file at the same
                // path. Keep both patches under one unambiguous comment target.
                file.untracked = true;
                file.status.push_str("/?");
            } else {
                snapshot.files.push(File {
                    path,
                    previous_path: None,
                    status: "?".into(),
                    untracked: true,
                });
            }
        }
    }
    snapshot.files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(snapshot)
}

fn default_base(root: &Repository) -> String {
    if let Ok(bytes) = git(
        root,
        &args(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]),
        false,
    ) {
        let reference = String::from_utf8_lossy(&bytes).trim().to_string();
        if revision(root, &format!("{reference}^{{commit}}")).is_ok() {
            return reference
                .strip_prefix("refs/remotes/")
                .unwrap_or(&reference)
                .to_string();
        }
    }
    for reference in ["main", "master"] {
        if revision(root, &format!("refs/heads/{reference}^{{commit}}")).is_ok() {
            return reference.into();
        }
    }
    "HEAD".into()
}

impl Snapshot {
    fn diff_args(&self) -> Vec<OsString> {
        let mut command = args(&[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--full-index",
            "--find-renames",
            "--submodule=short",
        ]);
        if self.scope == Scope::Staged {
            command.push("--cached".into());
        }
        if let Some(base) = &self.base {
            command.push(base.into());
        }
        if let Some(tip) = &self.tip {
            command.push(tip.into());
        }
        command
    }

    pub fn patch(&self, file: &File) -> Result<Patch, String> {
        self.patch_with_context(file, 3)
    }

    fn patch_with_context(&self, file: &File, context: u32) -> Result<Patch, String> {
        let mut bytes = if file.untracked {
            self.repository.untracked_patch(file)?
        } else {
            Vec::new()
        };
        if !file.untracked || file.status != "?" {
            let mut command = self.diff_args();
            command.extend(args(&[&format!("--unified={context}"), "--"]));
            if let Some(old) = &file.previous_path {
                command.push(old.as_os_str().into());
            }
            command.push(file.path.as_os_str().into());
            let mut tracked = git(&self.repository, &command, false)?;
            tracked.append(&mut bytes);
            bytes = tracked;
        }
        if bytes.len() > OUTPUT_LIMIT {
            return Err("This diff exceeds the 8 MiB display limit. Review a smaller change with Git or your editor.".into());
        }
        let text = String::from_utf8(bytes).map_err(|_| {
            "This diff is not UTF-8 text. Review it with a binary-aware editor.".to_string()
        })?;
        // Short lines can exhaust GTK's per-row storage while still below the byte limit.
        if text.lines().nth(DISPLAY_LINE_LIMIT).is_some() {
            return Err("This diff exceeds the 100,000-line display limit. Review a smaller change with Git or your editor.".into());
        }
        Ok(parse_patch(&text))
    }
}

fn parse_files(bytes: &[u8]) -> Result<Vec<File>, String> {
    let mut fields = bytes.split(|b| *b == 0).filter(|s| !s.is_empty());
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        let first = fields.next().ok_or("Incomplete Git file list")?;
        let renamed = matches!(status.first(), Some(b'R' | b'C'));
        let path = if renamed {
            fields.next().ok_or("Incomplete Git rename")?
        } else {
            first
        };
        files.push(File {
            path: OsString::from_vec(path.to_vec()).into(),
            previous_path: renamed.then(|| OsString::from_vec(first.to_vec()).into()),
            status: String::from_utf8_lossy(status).into(),
            untracked: false,
        });
    }
    Ok(files)
}

pub fn parse_patch(text: &str) -> Patch {
    let mut old = None;
    let mut new = None;
    let lines = text
        .lines()
        .map(|text| {
            if text.starts_with("diff --git ") {
                old = None;
                new = None;
            }
            if text.starts_with("@@ ") {
                let mut ranges = text.split_whitespace().skip(1);
                old = ranges
                    .next()
                    .and_then(|s| s.strip_prefix('-'))
                    .and_then(|s| s.split(',').next())
                    .and_then(|s| s.parse::<u32>().ok());
                new = ranges
                    .next()
                    .and_then(|s| s.strip_prefix('+'))
                    .and_then(|s| s.split(',').next())
                    .and_then(|s| s.parse::<u32>().ok());
                return Line {
                    old: None,
                    new: None,
                    text: text.into(),
                };
            }
            let (a, b) = match text.as_bytes().first() {
                Some(b'+') if new.is_some() => (None, new),
                Some(b'-') if old.is_some() => (old, None),
                Some(b' ') => (old, new),
                _ => (None, None),
            };
            if a.is_some() {
                old = old.and_then(|v| v.checked_add(1));
            }
            if b.is_some() {
                new = new.and_then(|v| v.checked_add(1));
            }
            Line {
                old: a,
                new: b,
                text: text.into(),
            }
        })
        .collect();
    Patch {
        text: text.into(),
        lines,
    }
}
