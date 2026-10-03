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
fn all_changes_includes_committed_dirty_and_new_files_without_duplicates() {
    let dir = repo();
    let root = dir.path();
    std::fs::write(root.join("shared"), "base\n").unwrap();
    commit(root);
    git(root, &["checkout", "-b", "feature"]);
    std::fs::write(root.join("shared"), "committed\n").unwrap();
    std::fs::write(root.join("committed-only"), "committed\n").unwrap();
    commit(root);
    std::fs::write(root.join("shared"), "working\n").unwrap();
    std::fs::write(root.join("new"), "new\n").unwrap();
    let snapshot = load(root, Scope::AllChanges(String::new())).unwrap();
    assert_eq!(snapshot.scope, Scope::AllChanges("main".into()));
    assert_eq!(snapshot.files.len(), 3);
    let file = snapshot
        .files
        .iter()
        .find(|f| f.label() == "shared")
        .unwrap();
    let patch = snapshot.patch(file).unwrap();
    assert!(patch.text.contains("-base") && patch.text.contains("+working"));
    assert!(!patch.text.contains("+committed"));
    assert!(snapshot
        .files
        .iter()
        .any(|f| f.label() == "new" && f.untracked));
}

#[test]
fn all_changes_before_first_commit_and_without_default_branch() {
    let dir = repo();
    std::fs::write(dir.path().join("new"), "new\n").unwrap();
    let snapshot = load(dir.path(), Scope::AllChanges(String::new())).unwrap();
    assert_eq!(snapshot.scope, Scope::AllChanges("HEAD".into()));
    assert_eq!(snapshot.files.len(), 1);
    commit(dir.path());
    git(dir.path(), &["branch", "-m", "custom"]);
    std::fs::write(dir.path().join("new"), "dirty\n").unwrap();
    assert_eq!(
        load(dir.path(), Scope::AllChanges(String::new()))
            .unwrap()
            .files
            .len(),
        1
    );
}

#[test]
fn comment_follows_code_when_unrelated_lines_change() {
    use flowmux_vcs::review::notes::{self, Note};
    let dir = repo();
    std::fs::write(dir.path().join("a"), "alpha\ntarget\nomega\n").unwrap();
    let snapshot = load(dir.path(), Scope::AllChanges(String::new())).unwrap();
    let file = &snapshot.files[0];
    let patch = snapshot.patch(file).unwrap();
    let row = patch
        .lines
        .iter()
        .position(|l| l.text == "+target")
        .unwrap();
    let note = Note::new(
        "note".into(),
        &snapshot,
        file,
        &patch,
        Some(row..row + 1),
        "Keep this readable",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("a"),
        "inserted\nalpha\ntarget\nchanged elsewhere\n",
    )
    .unwrap();
    let (updated, stale) = notes::refresh(dir.path(), &[note]).unwrap();
    assert!(stale.is_empty());
    assert_eq!(updated[0].location, "new lines 3–3");
    assert!(notes::prompt(dir.path(), &updated)
        .unwrap()
        .contains("new lines 3–3"));
    std::fs::write(
        dir.path().join("a"),
        "inserted\nalpha\nreplaced target\nchanged elsewhere\n",
    )
    .unwrap();
    assert_eq!(notes::validate(dir.path(), &updated).unwrap(), ["note"]);
}

#[test]
fn repeated_comment_context_is_not_guessed() {
    use flowmux_vcs::review::notes::{self, Note};
    let dir = repo();
    std::fs::write(dir.path().join("a"), "same\n").unwrap();
    let snapshot = load(dir.path(), Scope::WorkingTree).unwrap();
    let patch = snapshot.patch(&snapshot.files[0]).unwrap();
    let row = patch.lines.iter().position(|l| l.text == "+same").unwrap();
    let note = Note::new(
        "ambiguous".into(),
        &snapshot,
        &snapshot.files[0],
        &patch,
        Some(row..row + 1),
        "Which one?",
    )
    .unwrap();
    std::fs::write(dir.path().join("a"), "same\nsame\n").unwrap();
    assert_eq!(notes::validate(dir.path(), &[note]).unwrap(), ["ambiguous"]);
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

#[test]
fn reviews_across_many_files_and_scopes_validate_independently() {
    use flowmux_vcs::review::notes::{self, Note};
    let dir = repo();
    for i in 0..50 {
        std::fs::write(
            dir.path().join(format!("file-{i:02}.txt")),
            format!("original {i}\n"),
        )
        .unwrap();
    }
    commit(dir.path());
    for i in 0..50 {
        std::fs::write(
            dir.path().join(format!("file-{i:02}.txt")),
            format!("staged {i}\n"),
        )
        .unwrap();
    }
    git(dir.path(), &["add", "."]);
    std::fs::write(dir.path().join("file-00.txt"), "unstaged\n").unwrap();
    let staged = load(dir.path(), Scope::Staged).unwrap();
    let mut notes: Vec<_> = staged
        .files
        .iter()
        .enumerate()
        .map(|(i, file)| {
            Note::new(
                format!("staged-{i}"),
                &staged,
                file,
                &staged.patch(file).unwrap(),
                None,
                &if i % 2 == 0 {
                    "short".into()
                } else {
                    "다중 파일 리뷰\n".repeat(500)
                },
            )
            .unwrap()
        })
        .collect();
    let unstaged = load(dir.path(), Scope::Unstaged).unwrap();
    let file = &unstaged.files[0];
    notes.push(
        Note::new(
            "unstaged".into(),
            &unstaged,
            file,
            &unstaged.patch(file).unwrap(),
            None,
            "different scope",
        )
        .unwrap(),
    );
    assert!(notes::validate(dir.path(), &notes).unwrap().is_empty());
    std::fs::write(dir.path().join("file-00.txt"), "changed externally\n").unwrap();
    // Whole-file feedback remains attached when only the file contents change.
    assert!(notes::validate(dir.path(), &notes).unwrap().is_empty());
    let current = load(dir.path(), Scope::Unstaged).unwrap();
    let file = &current.files[0];
    notes[50] = Note::new(
        "unstaged".into(),
        &current,
        file,
        &current.patch(file).unwrap(),
        None,
        "reattached review",
    )
    .unwrap();
    assert!(notes::validate(dir.path(), &notes).unwrap().is_empty());
    assert!(notes::prompt(dir.path(), &notes)
        .unwrap()
        .contains("51. file-00.txt"));
}

#[test]
fn current_checkout_combines_git_states_and_refreshes_after_commit() {
    let dir = repo();
    let root = dir.path();
    for name in ["staged.txt", "unstaged.txt", "both.txt"] {
        std::fs::write(root.join(name), "original\n").unwrap();
    }
    commit(root);
    git(root, &["switch", "-c", "feature"]);
    std::fs::write(root.join("already-committed.txt"), "committed\n").unwrap();
    commit(root);
    std::fs::write(root.join("staged.txt"), "staged only\n").unwrap();
    std::fs::write(root.join("both.txt"), "intermediate\n").unwrap();
    git(root, &["add", "staged.txt", "both.txt"]);
    std::fs::write(root.join("both.txt"), "final contents\n").unwrap();
    std::fs::write(root.join("unstaged.txt"), "unstaged only\n").unwrap();
    std::fs::write(root.join("new.txt"), "new file\n").unwrap();
    let current = load(root, Scope::WorkingTree).unwrap();
    assert_eq!(
        current.files.iter().map(|f| f.label()).collect::<Vec<_>>(),
        ["both.txt", "new.txt", "staged.txt", "unstaged.txt"]
    );
    let patch = current.patch(&current.files[0]).unwrap();
    assert!(patch.text.contains("-original") && patch.text.contains("+final contents"));
    assert!(!patch.text.contains("intermediate"));
    let unstaged = load(root, Scope::Unstaged).unwrap();
    assert!(!unstaged.files.iter().any(|f| f.label() == "staged.txt"));
    commit(root);
    assert!(load(root, Scope::WorkingTree).unwrap().files.is_empty());
}
