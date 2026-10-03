// SPDX-License-Identifier: GPL-3.0-or-later
use flowmux_vcs::review::{load, parse_patch, Scope};
use std::{path::Path, process::Command};

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_AUTHOR_NAME", "JunsuChoi")
        .env("GIT_COMMITTER_NAME", "JunsuChoi")
        .env("GIT_AUTHOR_EMAIL", "jsuya.choi@samsung.com")
        .env("GIT_COMMITTER_EMAIL", "jsuya.choi@samsung.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}
fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-b", "main"]);
    dir
}
fn commit(root: &Path) {
    git(root, &["add", "--all"]);
    git(
        root,
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "test: create review fixture",
        ],
    );
}

#[test]
fn empty_unborn_and_non_repository() {
    let dir = repo();
    assert!(load(dir.path(), Scope::WorkingTree)
        .unwrap()
        .files
        .is_empty());
    std::fs::write(dir.path().join("new.txt"), "first\n").unwrap();
    let snapshot = load(dir.path(), Scope::WorkingTree).unwrap();
    assert!(snapshot.files[0].untracked);
    assert!(snapshot
        .patch(&snapshot.files[0])
        .unwrap()
        .text
        .contains("+first"));
    git(dir.path(), &["add", "."]);
    let snapshot = load(dir.path(), Scope::Staged).unwrap();
    assert_eq!(snapshot.files.len(), 1);
    assert!(snapshot
        .patch(&snapshot.files[0])
        .unwrap()
        .text
        .contains("+first"));
    assert!(load(tempfile::tempdir().unwrap().path(), Scope::WorkingTree).is_err());
}

#[test]
fn scopes_rename_delete_binary_and_literal_paths() {
    let dir = repo();
    let root = dir.path();
    for name in [
        "a.txt",
        "deleted",
        "rename",
        "[literal]* 한글.txt",
        "--option",
    ] {
        std::fs::write(root.join(name), format!("old\n{name}\n")).unwrap();
    }
    commit(root);
    std::fs::write(root.join("a.txt"), "staged\nsecond\n").unwrap();
    git(root, &["add", "a.txt"]);
    std::fs::write(root.join("a.txt"), "unstaged\nsecond\n").unwrap();
    std::fs::write(root.join("[literal]* 한글.txt"), "literal\n").unwrap();
    std::fs::write(root.join("--option"), "option\n").unwrap();
    std::fs::remove_file(root.join("deleted")).unwrap();
    git(root, &["mv", "rename", "renamed"]);
    std::fs::write(root.join("image.bin"), [0, 1, 2, 3]).unwrap();
    let staged = load(root, Scope::Staged).unwrap();
    assert!(staged
        .patch(
            staged
                .files
                .iter()
                .find(|f| f.path == Path::new("a.txt"))
                .unwrap()
        )
        .unwrap()
        .text
        .contains("+staged"));
    let unstaged = load(root, Scope::Unstaged).unwrap();
    let patch = unstaged
        .patch(
            unstaged
                .files
                .iter()
                .find(|f| f.path == Path::new("a.txt"))
                .unwrap(),
        )
        .unwrap();
    assert!(patch.text.contains("-staged") && patch.text.contains("+unstaged"));
    let all = load(root, Scope::WorkingTree).unwrap();
    assert_eq!(all.files.len(), 6);
    for file in &all.files {
        let patch = all.patch(file).unwrap();
        match file.label().as_str() {
            "renamed" => assert!(patch.text.contains("rename from rename")),
            "image.bin" => assert!(patch.text.contains("Binary files")),
            "deleted" => assert!(patch.text.contains("deleted file")),
            name if name.starts_with("[literal]") => assert!(patch.text.contains("+literal")),
            "--option" => assert!(patch.text.contains("+option")),
            _ => assert!(
                patch.text.contains("+unstaged"),
                "{}: {}",
                file.label(),
                patch.text
            ),
        }
    }
}

#[test]
fn branch_comparison_is_pinned_and_does_not_include_dirty_changes() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("a"), "base\n").unwrap();
    commit(root);
    git(root, &["checkout", "-b", "feature"]);
    std::fs::write(root.join("a"), "feature\n").unwrap();
    commit(root);
    let snapshot = load(root, Scope::Branch("main".into())).unwrap();
    std::fs::write(root.join("a"), "later\n").unwrap();
    commit(root);
    let patch = snapshot.patch(&snapshot.files[0]).unwrap();
    assert!(patch.text.contains("+feature") && !patch.text.contains("+later"));
    assert!(load(root, Scope::Branch("--help".into())).is_err());
    assert!(load(root, Scope::Branch("missing".into())).is_err());
}

#[test]
fn symlinks_non_utf8_paths_and_ignored_files() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join(".gitignore"), "ignored\n").unwrap();
    commit(root);
    std::fs::write(root.join("ignored"), "secret").unwrap();
    symlink("/outside/do-not-read", root.join("link")).unwrap();
    let name = if cfg!(target_os = "linux") {
        std::ffi::OsString::from_vec(b"odd\n\t\xff".to_vec())
    } else {
        std::ffi::OsString::from("odd\n\t한글")
    };
    std::fs::write(root.join(&name), "unicode 내용\n").unwrap();
    let snapshot = load(root, Scope::WorkingTree).unwrap();
    assert_eq!(snapshot.files.len(), 2);
    let link = snapshot
        .files
        .iter()
        .find(|f| f.path == Path::new("link"))
        .unwrap();
    assert!(snapshot.patch(link).unwrap().text.contains("symbolic link"));
    let odd = snapshot
        .files
        .iter()
        .find(|f| f.path != Path::new("link"))
        .unwrap();
    assert!(!odd.label().contains('\n'));
    assert!(snapshot.patch(odd).unwrap().text.contains("+unicode"));
}

#[test]
fn line_numbers_multiple_hunks_deletion_and_no_final_newline() {
    let patch = parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n-old\n+new\n same\n@@ -50 +50,0 @@\n-delete\n\\ No newline at end of file\n");
    assert_eq!((patch.lines[4].old, patch.lines[4].new), (Some(1), None));
    assert_eq!((patch.lines[5].old, patch.lines[5].new), (None, Some(1)));
    assert_eq!((patch.lines[6].old, patch.lines[6].new), (Some(2), Some(2)));
    assert_eq!((patch.lines[8].old, patch.lines[8].new), (Some(50), None));
    assert_eq!((patch.lines[9].old, patch.lines[9].new), (None, None));
}

#[test]
fn many_files_large_patch_and_oversize_are_explicit() {
    let dir = repo();
    let root = dir.path();
    for i in 0..600 {
        std::fs::write(root.join(format!("file-{i:04}")), "new\n").unwrap();
    }
    let text = (0..20_000)
        .map(|i| format!("line {i} 한글\n"))
        .collect::<String>();
    std::fs::write(root.join("large"), &text).unwrap();
    let snapshot = load(root, Scope::WorkingTree).unwrap();
    assert_eq!(snapshot.files.len(), 601);
    let patch = snapshot
        .patch(
            snapshot
                .files
                .iter()
                .find(|f| f.path == Path::new("large"))
                .unwrap(),
        )
        .unwrap();
    assert!(patch.text.contains("+line 19999 한글"));
    let big = std::fs::File::create(root.join("oversize")).unwrap();
    big.set_len(9 * 1024 * 1024).unwrap();
    let snapshot = load(root, Scope::WorkingTree).unwrap();
    assert!(snapshot
        .patch(
            snapshot
                .files
                .iter()
                .find(|f| f.path == Path::new("oversize"))
                .unwrap()
        )
        .unwrap_err()
        .contains("limit"));
}

#[test]
fn comments_anchor_ranges_unicode_long_text_and_stale_files() {
    use flowmux_vcs::review::notes::{self, Note};
    let dir = repo();
    std::fs::write(dir.path().join("review.rs"), "before\ncontext\n").unwrap();
    std::fs::write(dir.path().join("binary"), [0, 1, 2]).unwrap();
    commit(dir.path());
    std::fs::write(dir.path().join("review.rs"), "after\ncontext\n").unwrap();
    std::fs::write(dir.path().join("binary"), [0, 1, 3]).unwrap();
    let snapshot = load(dir.path(), Scope::WorkingTree).unwrap();
    let file = snapshot
        .files
        .iter()
        .find(|f| f.path == Path::new("review.rs"))
        .unwrap();
    let patch = snapshot.patch(file).unwrap();
    assert!(Note::new("empty".into(), &snapshot, file, &patch, None, " \n").is_err());
    assert!(Note::new(
        "header".into(),
        &snapshot,
        file,
        &patch,
        Some(0..1),
        "comment"
    )
    .is_err());
    let first = patch
        .lines
        .iter()
        .position(|l| l.text == "-before")
        .unwrap();
    let note = Note::new(
        "range".into(),
        &snapshot,
        file,
        &patch,
        Some(first..first + 2),
        "짧은 리뷰\nSecond line",
    )
    .unwrap();
    assert_eq!(note.location, "old lines 1–1, new lines 1–1");
    assert_eq!(note.excerpt, "-before\n+after\n");
    let mut many = vec![note.clone(); 300];
    for (i, n) in many.iter_mut().enumerate() {
        n.id = i.to_string();
        n.text = "긴 리뷰 🧪\n".repeat(300);
    }
    assert!(notes::validate(dir.path(), &many).unwrap().is_empty());
    let prompt = notes::prompt(dir.path(), &many).unwrap();
    assert!(prompt.contains("300. review.rs"));
    assert!(prompt.contains("짧은 리뷰") == false);
    assert!(prompt.contains("> -before\n> +after"));
    let binary = snapshot
        .files
        .iter()
        .find(|f| f.path == Path::new("binary"))
        .unwrap();
    let binary_note = Note::new(
        "binary".into(),
        &snapshot,
        binary,
        &snapshot.patch(binary).unwrap(),
        None,
        "binary review",
    )
    .unwrap();
    std::fs::write(dir.path().join("binary"), [0, 1, 4]).unwrap();
    assert_eq!(
        notes::validate(dir.path(), &[binary_note]).unwrap(),
        vec!["binary"]
    );
    std::fs::write(dir.path().join("review.rs"), "changed again\ncontext\n").unwrap();
    assert_eq!(notes::validate(dir.path(), &many).unwrap().len(), 300);
    let mut resolved = note;
    resolved.resolved = true;
    assert!(notes::validate(dir.path(), &[resolved.clone()])
        .unwrap()
        .is_empty());
    assert!(notes::prompt(dir.path(), &[resolved]).is_err());
    std::fs::remove_file(dir.path().join("review.rs")).unwrap();
    assert_eq!(notes::validate(dir.path(), &many).unwrap().len(), 300);
}
