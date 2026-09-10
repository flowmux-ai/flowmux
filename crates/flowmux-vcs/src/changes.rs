// SPDX-License-Identifier: GPL-3.0-or-later
//! File-level Git review. Call on a blocking worker, never the GTK thread.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

// ponytail: load only the selected file; previews above 2 MiB need a streaming viewer.
pub const MAX_PREVIEW_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Staged,
    Unstaged,
    Untracked,
    Conflict,
}

impl Area {
    pub fn label(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Unstaged",
            Self::Untracked => "Untracked",
            Self::Conflict => "Conflicts",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub original_path: Option<PathBuf>,
    pub area: Area,
    pub status: char,
    pub submodule: bool,
}

#[derive(Clone, Debug)]
pub struct Changes {
    pub root: PathBuf,
    pub files: Vec<Change>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preview {
    pub original: String,
    pub modified: String,
    pub note: String,
}

fn git(root: &Path, args: &[&OsStr]) -> Result<Output, String> {
    // std::process, like worktree::git_output: GLib owns SIGCHLD in the GUI.
    Command::new("git")
        .args(["--literal-pathspecs", "--no-optional-locks"])
        .args(args)
        .current_dir(root)
        .env("LC_ALL", "C")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .map_err(|e| format!("Unable to run git: {e}"))
}

fn checked(root: &Path, args: &[&OsStr]) -> Result<Vec<u8>, String> {
    let output = git(root, args)?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(2000)
            .collect());
    }
    Ok(output.stdout)
}

fn path(bytes: &[u8]) -> Result<PathBuf, String> {
    #[cfg(unix)]
    let value: PathBuf = {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(bytes.to_vec()).into()
    };
    #[cfg(not(unix))]
    let value: PathBuf = String::from_utf8(bytes.to_vec())
        .map_err(|e| e.to_string())?
        .into();
    validate_path(&value)?;
    Ok(value)
}

fn validate_path(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("Invalid repository-relative path".into());
    }
    Ok(())
}

pub fn list(start: &Path) -> Result<Changes, String> {
    let repo = gix::discover(start).map_err(|_| "Not a Git repository".to_string())?;
    let root = repo
        .work_dir()
        .ok_or("Bare repositories have no working files")?;
    let root = std::fs::canonicalize(root).map_err(|e| e.to_string())?;
    let bytes = checked(
        &root,
        &[
            "status".as_ref(),
            "--porcelain=v2".as_ref(),
            "-z".as_ref(),
            "--untracked-files=all".as_ref(),
            "--ignore-submodules=none".as_ref(),
        ],
    )?;
    Ok(Changes {
        root,
        files: parse_status(&bytes)?,
    })
}

fn parse_status(bytes: &[u8]) -> Result<Vec<Change>, String> {
    let mut records = bytes.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut files = Vec::new();
    while let Some(record) = records.next() {
        let kind = record[0];
        if kind == b'?' {
            files.push(Change {
                path: path(record.get(2..).ok_or("Invalid status")?)?,
                original_path: None,
                area: Area::Untracked,
                status: '?',
                submodule: false,
            });
            continue;
        }
        let fields: Vec<_> = record
            .splitn(
                match kind {
                    b'1' => 9,
                    b'2' => 10,
                    b'u' => 11,
                    _ => return Err("Unknown Git status record".into()),
                },
                |b| *b == b' ',
            )
            .collect();
        let expected = match kind {
            b'1' => 9,
            b'2' => 10,
            _ => 11,
        };
        if fields.len() != expected || fields[1].len() != 2 {
            return Err("Invalid Git status record".into());
        }
        let file = path(fields[expected - 1])?;
        let old = if kind == b'2' {
            Some(path(records.next().ok_or("Missing rename source")?)?)
        } else {
            None
        };
        let submodule = fields[2].first() == Some(&b'S');
        if kind == b'u' {
            files.push(Change {
                path: file,
                original_path: None,
                area: Area::Conflict,
                status: 'U',
                submodule,
            });
            continue;
        }
        for (index, area) in [Area::Staged, Area::Unstaged].into_iter().enumerate() {
            let status = fields[1][index];
            if status != b'.' {
                files.push(Change {
                    path: file.clone(),
                    original_path: if matches!(status, b'R' | b'C') {
                        old.clone()
                    } else {
                        None
                    },
                    area,
                    status: status as char,
                    submodule,
                });
            }
        }
    }
    files.sort_by(|a, b| (a.area as u8, &a.path).cmp(&(b.area as u8, &b.path)));
    Ok(files)
}

fn blob(root: &Path, revision: &str, path: &Path) -> Result<Vec<u8>, String> {
    let mut spec = OsString::from(revision);
    spec.push(":");
    spec.push(path);
    // Resolve once: an external index update between size and content reads
    // must not substitute a different, potentially huge blob.
    let oid = checked(
        root,
        &[
            "rev-parse".as_ref(),
            "--verify".as_ref(),
            "--end-of-options".as_ref(),
            &spec,
        ],
    )?;
    let oid = String::from_utf8(oid).map_err(|_| "Invalid Git object ID")?;
    let object = OsStr::new(oid.trim());
    let size = checked(root, &["cat-file".as_ref(), "-s".as_ref(), object])?;
    let size: usize = String::from_utf8_lossy(&size)
        .trim()
        .parse()
        .map_err(|_| "Invalid Git blob size")?;
    if size > MAX_PREVIEW_BYTES {
        return Err("File exceeds the 2 MiB diff preview limit".into());
    }
    checked(root, &["cat-file".as_ref(), "blob".as_ref(), object])
}

fn working_file(root: &Path, path: &Path) -> Result<Vec<u8>, String> {
    // Do not follow a symlinked parent out of the repository. A tracked symlink
    // itself is compared as its link text, matching Git's blob representation.
    let full = root.join(path);
    let parent =
        std::fs::canonicalize(full.parent().ok_or("Missing parent")?).map_err(|e| e.to_string())?;
    if !parent.starts_with(root) {
        return Err("File parent is outside this worktree".into());
    }
    let metadata = std::fs::symlink_metadata(&full).map_err(|e| e.to_string())?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(full).map_err(|e| e.to_string())?;
        return Ok(target.as_os_str().as_encoded_bytes().to_vec());
    }
    if !metadata.is_file() {
        return Err("This entry is not a regular file".into());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(full)
        .map_err(|e| e.to_string())?
        .take(MAX_PREVIEW_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_PREVIEW_BYTES {
        return Err("File exceeds the 2 MiB diff preview limit".into());
    }
    Ok(bytes)
}

fn text(bytes: Vec<u8>) -> Result<String, String> {
    if bytes.contains(&0) {
        return Err("Binary file — text diff is unavailable".into());
    }
    String::from_utf8(bytes).map_err(|_| "Non-UTF-8 file — text diff is unavailable".into())
}

pub fn preview(root: &Path, change: &Change) -> Result<Preview, String> {
    validate_path(&change.path)?;
    if let Some(old) = &change.original_path {
        validate_path(old)?;
    }
    if change.area == Area::Conflict {
        return Err("Merge conflict — resolve it in the terminal before staging".into());
    }
    if change.submodule {
        return Err("Submodule — review its changes in the terminal".into());
    }
    let original_path = change.original_path.as_deref().unwrap_or(&change.path);
    let original = match change.area {
        Area::Untracked => Vec::new(),
        Area::Staged if change.status == 'A' => Vec::new(),
        Area::Staged => blob(root, "HEAD", original_path)?,
        _ => blob(root, "", original_path)?,
    };
    let modified = if change.status == 'D' {
        Vec::new()
    } else if change.area == Area::Staged {
        blob(root, "", &change.path)?
    } else {
        working_file(root, &change.path)?
    };
    let original = text(original)?;
    let modified = text(modified)?;
    let note = if original == modified {
        "Text is unchanged; Git reports a path or file mode change.".to_string()
    } else {
        format!(
            "Read-only · Original: {} · Modified: {}. Working files include saved content only.",
            text_format(&original),
            text_format(&modified)
        )
    };
    Ok(Preview {
        original,
        modified,
        note,
    })
}

fn text_format(text: &str) -> String {
    let crlf = text.matches("\r\n").count();
    let lf = text.matches('\n').count();
    let ending = if lf == 0 {
        "no line ending"
    } else if crlf == 0 {
        "LF"
    } else if crlf == lf {
        "CRLF"
    } else {
        "mixed line endings"
    };
    format!(
        "{}{}",
        ending,
        if text.starts_with('\u{feff}') {
            ", UTF-8 BOM"
        } else {
            ""
        }
    )
}

/// Change only the selected entry in the index; never write working files.
pub fn set_staged(root: &Path, change: &Change) -> Result<(), String> {
    validate_path(&change.path)?;
    if let Some(old) = &change.original_path {
        validate_path(old)?;
    }
    let current = list(root)?;
    if current.root != root || !current.files.contains(change) {
        return Err("Git status changed. Refresh and select the file again.".into());
    }
    if change.area == Area::Conflict || change.submodule {
        return Err("Resolve conflicts and submodule changes in the terminal".into());
    }
    let mut args: Vec<&OsStr> = if change.area == Area::Staged {
        let head = git(
            root,
            &["rev-parse".as_ref(), "--verify".as_ref(), "HEAD".as_ref()],
        )?;
        if head.status.success() {
            vec![
                "restore".as_ref(),
                "--staged".as_ref(),
                "--source=HEAD".as_ref(),
                "--".as_ref(),
            ]
        } else {
            let reference = checked(
                root,
                &["symbolic-ref".as_ref(), "-q".as_ref(), "HEAD".as_ref()],
            )?;
            let reference = String::from_utf8(reference).map_err(|_| "Invalid HEAD reference")?;
            let exists = git(
                root,
                &[
                    "show-ref".as_ref(),
                    "--verify".as_ref(),
                    "--quiet".as_ref(),
                    reference.trim().as_ref(),
                ],
            )?;
            if exists.status.code() != Some(1) {
                return Err("HEAD could not be read; the index was left unchanged".into());
            }
            // Unborn HEAD: remove only the index entry, preserving the new file.
            vec![
                "rm".as_ref(),
                "--cached".as_ref(),
                "-f".as_ref(),
                "--".as_ref(),
            ]
        }
    } else {
        vec!["add".as_ref(), "-A".as_ref(), "--".as_ref()]
    };
    args.push(change.path.as_os_str());
    if change.status == 'R' {
        if let Some(old) = &change.original_path {
            args.push(old.as_os_str());
        }
    }
    checked(root, &args)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn run(root: &Path, args: &[&str]) {
        checked(root, &args.iter().map(OsStr::new).collect::<Vec<_>>()).unwrap();
    }
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-b", "main"]);
        run(dir.path(), &["config", "user.name", "Test"]);
        run(dir.path(), &["config", "user.email", "test@example.test"]);
        dir
    }
    fn commit(root: &Path) {
        run(root, &["add", "-A"]);
        run(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--no-verify",
                "-m",
                "base",
            ],
        );
    }
    fn write(root: &Path, file: &str, text: &str) {
        std::fs::write(root.join(file), text).unwrap();
    }
    fn find(root: &Path, file: &str, area: Area) -> Change {
        list(root)
            .unwrap()
            .files
            .into_iter()
            .find(|c| c.path == Path::new(file) && c.area == area)
            .unwrap()
    }

    #[test]
    fn staged_and_unstaged_compare_different_versions_and_preserve_working_file() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "HEAD\n");
        commit(root);
        write(root, "file", "INDEX\n");
        run(root, &["add", "file"]);
        write(root, "file", "DISK\n");
        let staged = find(root, "file", Area::Staged);
        let unstaged = find(root, "file", Area::Unstaged);
        let p = preview(root, &staged).unwrap();
        assert_eq!((&*p.original, &*p.modified), ("HEAD\n", "INDEX\n"));
        let p = preview(root, &unstaged).unwrap();
        assert_eq!((&*p.original, &*p.modified), ("INDEX\n", "DISK\n"));
        set_staged(root, &staged).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file")).unwrap(),
            "DISK\n"
        );
        assert_eq!(list(root).unwrap().files.len(), 1);
        set_staged(root, &find(root, "file", Area::Unstaged)).unwrap();
        assert_eq!(
            preview(root, &find(root, "file", Area::Staged))
                .unwrap()
                .modified,
            "DISK\n"
        );
    }

    #[test]
    fn unborn_head_and_literal_untracked_paths() {
        let dir = repo();
        let root = dir.path();
        for name in ["*.txt", "other.txt", "-flag", ":(glob)*", "한 글\n🙂"] {
            write(root, name, "new\n");
        }
        let change = find(root, "*.txt", Area::Untracked);
        assert_eq!(preview(root, &change).unwrap().original, "");
        set_staged(root, &change).unwrap();
        let changes = list(root).unwrap();
        assert_eq!(
            changes
                .files
                .iter()
                .filter(|c| c.area == Area::Staged)
                .count(),
            1
        );
        let change = find(root, "*.txt", Area::Staged);
        assert_eq!(preview(root, &change).unwrap().modified, "new\n");
        // Further edits must survive unstage even without HEAD.
        write(root, "*.txt", "edited\n");
        set_staged(root, &change).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("*.txt")).unwrap(),
            "edited\n"
        );
        for name in ["-flag", ":(glob)*", "한 글\n🙂"] {
            set_staged(root, &find(root, name, Area::Untracked)).unwrap();
            set_staged(root, &find(root, name, Area::Staged)).unwrap();
        }
        assert!(list(root)
            .unwrap()
            .files
            .iter()
            .all(|c| c.area == Area::Untracked));
    }

    #[test]
    fn rename_delete_and_addition_round_trip() {
        let dir = repo();
        let root = dir.path();
        write(root, "old", "base\n");
        write(root, "delete", "gone\n");
        commit(root);
        run(root, &["mv", "old", "new"]);
        write(root, "new", "changed after rename\n");
        let renamed = find(root, "new", Area::Staged);
        assert_eq!(renamed.original_path.as_deref(), Some(Path::new("old")));
        assert_eq!(preview(root, &renamed).unwrap().original, "base\n");
        assert_eq!(
            preview(root, &find(root, "new", Area::Unstaged))
                .unwrap()
                .original,
            "base\n"
        );
        set_staged(root, &renamed).unwrap();
        assert!(!root.join("old").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("new")).unwrap(),
            "changed after rename\n"
        );
        std::fs::remove_file(root.join("delete")).unwrap();
        let deleted = find(root, "delete", Area::Unstaged);
        assert_eq!(preview(root, &deleted).unwrap().modified, "");
        set_staged(root, &deleted).unwrap();
        let deleted = find(root, "delete", Area::Staged);
        assert_eq!(preview(root, &deleted).unwrap().original, "gone\n");
        set_staged(root, &deleted).unwrap();
        assert!(!root.join("delete").exists());
    }

    #[test]
    fn binary_large_and_non_utf8_content_are_explicit_but_can_be_staged() {
        let dir = repo();
        let root = dir.path();
        for (name, bytes) in [
            ("binary", vec![0, 1]),
            ("encoding", vec![255]),
            ("large", vec![b'x'; MAX_PREVIEW_BYTES + 1]),
        ] {
            std::fs::write(root.join(name), &bytes).unwrap();
            let c = find(root, name, Area::Untracked);
            assert!(preview(root, &c).is_err());
            set_staged(root, &c).unwrap();
            let c = find(root, name, Area::Staged);
            assert!(preview(root, &c).is_err());
            set_staged(root, &c).unwrap();
            assert_eq!(std::fs::read(root.join(name)).unwrap(), bytes);
        }
    }

    #[test]
    fn index_lock_and_stale_entries_return_errors_without_changing_files() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "new\n");
        let c = find(root, "file", Area::Untracked);
        write(root, ".git/index.lock", "owned by another process");
        assert!(set_staged(root, &c).unwrap_err().contains("index.lock"));
        std::fs::remove_file(root.join(".git/index.lock")).unwrap();
        set_staged(root, &c).unwrap();
        assert!(set_staged(root, &c).unwrap_err().contains("status changed"));
        assert_eq!(std::fs::read_to_string(root.join("file")).unwrap(), "new\n");
    }

    #[test]
    fn worktree_subdirectory_and_detached_head_use_their_own_index() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "main\n");
        commit(root);
        let linked = root.join("linked");
        run(
            root,
            &["worktree", "add", "--detach", linked.to_str().unwrap()],
        );
        std::fs::create_dir(linked.join("nested")).unwrap();
        write(&linked, "file", "linked\n");
        let changes = list(&linked.join("nested")).unwrap();
        assert_eq!(changes.root, linked);
        set_staged(&changes.root, &changes.files[0]).unwrap();
        assert_eq!(blob(root, "", Path::new("file")).unwrap(), b"main\n");
        assert_eq!(blob(&linked, "", Path::new("file")).unwrap(), b"linked\n");
        set_staged(&linked, &find(&linked, "file", Area::Staged)).unwrap();
    }

    #[test]
    fn merge_conflicts_are_separate_and_cannot_be_staged_accidentally() {
        let dir = repo();
        let root = dir.path();
        write(root, "file", "base\n");
        commit(root);
        run(root, &["checkout", "-b", "other"]);
        write(root, "file", "other\n");
        commit(root);
        run(root, &["checkout", "main"]);
        write(root, "file", "main\n");
        commit(root);
        assert!(!git(root, &["merge".as_ref(), "other".as_ref()])
            .unwrap()
            .status
            .success());
        let c = find(root, "file", Area::Conflict);
        assert!(preview(root, &c).unwrap_err().contains("conflict"));
        assert!(set_staged(root, &c).is_err());
        assert_eq!(list(root).unwrap().files.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_file_modes_and_non_utf8_filenames() {
        use std::os::unix::{
            ffi::OsStringExt,
            fs::{symlink, PermissionsExt},
        };
        let dir = repo();
        let root = dir.path();
        symlink("outside-before", root.join("link")).unwrap();
        write(root, "mode", "same\n");
        commit(root);
        std::fs::remove_file(root.join("link")).unwrap();
        symlink("/outside-after", root.join("link")).unwrap();
        let p = preview(root, &find(root, "link", Area::Unstaged)).unwrap();
        assert_eq!(
            (&*p.original, &*p.modified),
            ("outside-before", "/outside-after")
        );
        std::fs::set_permissions(root.join("mode"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(preview(root, &find(root, "mode", Area::Unstaged))
            .unwrap()
            .note
            .contains("file mode"));
        let name = OsString::from_vec(vec![b'f', 255]);
        std::fs::write(root.join(&name), "raw name").unwrap();
        let c = list(root)
            .unwrap()
            .files
            .into_iter()
            .find(|c| c.path == Path::new(&name))
            .unwrap();
        set_staged(root, &c).unwrap();
        let c = list(root)
            .unwrap()
            .files
            .into_iter()
            .find(|c| c.path == Path::new(&name))
            .unwrap();
        assert_eq!(preview(root, &c).unwrap().modified, "raw name");
        set_staged(root, &c).unwrap();
    }

    #[test]
    fn malformed_status_and_unsafe_paths_are_rejected() {
        assert!(parse_status(b"1 M. broken\0").is_err());
        assert!(parse_status(b"? ../outside\0").is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(list(dir.path())
            .unwrap_err()
            .contains("Not a Git repository"));
        let c = Change {
            path: "../outside".into(),
            original_path: None,
            area: Area::Untracked,
            status: '?',
            submodule: false,
        };
        assert!(set_staged(dir.path(), &c).is_err());
    }

    #[test]
    fn submodule_changes_are_explicit_and_not_mutated() {
        let source = repo();
        write(source.path(), "file", "submodule\n");
        commit(source.path());
        let dir = repo();
        let root = dir.path();
        run(
            root,
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                source.path().to_str().unwrap(),
                "sub",
            ],
        );
        commit(root);
        write(root, "sub/file", "modified\n");
        let c = find(root, "sub", Area::Unstaged);
        assert!(c.submodule);
        assert!(preview(root, &c).unwrap_err().contains("Submodule"));
        assert!(set_staged(root, &c).is_err());
        assert_eq!(
            std::fs::read_to_string(root.join("sub/file")).unwrap(),
            "modified\n"
        );
    }

    #[test]
    fn line_endings_and_bom_differences_remain_visible_in_metadata() {
        let dir = repo();
        let root = dir.path();
        run(root, &["config", "core.autocrlf", "false"]);
        write(root, "file", "same\r\n");
        commit(root);
        write(root, "file", "\u{feff}same\n");
        let p = preview(root, &find(root, "file", Area::Unstaged)).unwrap();
        assert_eq!(p.original, "same\r\n");
        assert_eq!(p.modified, "\u{feff}same\n");
        assert!(p.note.contains("Original: CRLF"));
        assert!(p.note.contains("Modified: LF, UTF-8 BOM"));
    }
}
