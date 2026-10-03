// SPDX-License-Identifier: GPL-3.0-or-later
//! User scenarios: real Git changes, saved drafts, and a fresh store on reopen.
use flowmux_state::review_drafts::{Draft, DraftStore};
use flowmux_vcs::review::{self, notes::Note, Scope};
use std::{path::PathBuf, process::Command};

struct Scenario {
    _dir: tempfile::TempDir,
    root: PathBuf,
    database: PathBuf,
}

impl Scenario {
    fn new(base: Option<&str>, working: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        let scenario = Self {
            root,
            database: dir.path().join("reviews.sqlite3"),
            _dir: dir,
        };
        scenario.git(&["init", "-b", "main"]);
        if let Some(base) = base {
            scenario.write(base);
            scenario.commit();
        }
        scenario.write(working);
        scenario
    }
    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .env("GIT_AUTHOR_NAME", "JunsuChoi")
            .env("GIT_AUTHOR_EMAIL", "jsuya.choi@samsung.com")
            .env("GIT_COMMITTER_NAME", "JunsuChoi")
            .env("GIT_COMMITTER_EMAIL", "jsuya.choi@samsung.com")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn commit(&self) {
        self.git(&["add", "--all"]);
        self.git(&[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-m",
            "test: create comment tracking fixture",
        ]);
    }
    fn write(&self, content: &str) {
        std::fs::write(self.root.join("sample.txt"), content).unwrap();
    }
    fn store(&self) -> DraftStore {
        DraftStore::new(self.database.clone())
    }
    fn save(&self, first: &str, last: &str) -> Draft {
        let snapshot = review::load(&self.root, Scope::WorkingTree).unwrap();
        let file = snapshot
            .files
            .iter()
            .find(|f| f.label() == "sample.txt")
            .unwrap();
        let patch = snapshot.patch(file).unwrap();
        let start = patch.lines.iter().position(|l| l.text == first).unwrap();
        let end = patch.lines.iter().rposition(|l| l.text == last).unwrap() + 1;
        let note = Note::new(
            "scenario".into(),
            &snapshot,
            file,
            &patch,
            Some(start..end),
            "검토 의견: keep this feedback",
        )
        .unwrap();
        self.store()
            .save(
                &self.root,
                &Draft {
                    revision: 0,
                    notes: vec![note],
                },
            )
            .unwrap()
    }
    fn reopen(&self) -> Draft {
        let store = self.store();
        let current = store.load(&self.root).unwrap();
        store.refresh(&self.root, &current).unwrap()
    }
    fn assert_at(&self, lines: (u32, u32)) -> Note {
        let draft = self.reopen();
        assert_eq!(draft.notes.len(), 1, "review must survive the edit");
        let note = draft.notes[0].clone();
        assert!(!note.needs_reattach, "unique code should be tracked");
        assert_eq!(note.new_lines, Some(lines));
        assert_eq!(
            self.store().load(&self.root).unwrap(),
            draft,
            "relocation must be persisted"
        );
        assert_eq!(
            self.reopen(),
            draft,
            "unchanged reopen must not write again"
        );
        assert_eq!(note.text, "검토 의견: keep this feedback");
        note
    }
}

#[test]
fn saved_anchor_roundtrips_code_context_and_structured_line_bounds() {
    let s = Scenario::new(Some("before\nold\nafter\n"), "before\nnew\nafter\n");
    let saved = s.save("+new", "+new");
    let note = &saved.notes[0];
    assert_eq!(note.new_lines, Some((2, 2)));
    assert_eq!(note.old_lines, None);
    assert_eq!(note.excerpt, "+new\n");
    assert!(note.before.iter().any(|line| line == " before"));
    assert!(note.after.iter().any(|line| line == " after"));
    assert_eq!(s.store().load(&s.root).unwrap(), saved);
    s.assert_at((2, 2));
}

#[test]
fn inserted_and_deleted_lines_before_a_unicode_multiline_comment_follow_on_reopen() {
    let s = Scenario::new(None, "before\n검토 시작\n검토 끝\nafter\n");
    s.save("+검토 시작", "+검토 끝");
    s.write("inserted\nbefore\n검토 시작\n검토 끝\nafter\n");
    s.assert_at((3, 4));
    s.write("검토 시작\n검토 끝\nafter\n");
    s.assert_at((1, 2));
}

#[test]
fn multiline_context_survives_an_indentation_change_with_removed_diff_rows() {
    let s = Scenario::new(
        Some("heading\nstart\nend\n"),
        "changed heading\nstart\nend\n",
    );
    s.save(" start", " end");
    s.write("inserted\nchanged heading\nstart\n\tend\n");
    s.assert_at((3, 4));
    s.write("inserted again\ninserted\nchanged heading\nstart\n    end\n");
    s.assert_at((4, 5));
}

#[test]
fn additional_context_from_merged_hunks_does_not_delete_a_range_comment() {
    let original: String = (1..=30).map(|n| format!("line {n}\n")).collect();
    let changed = original
        .replace("line 3\n", "first change\n")
        .replace("line 27\n", "last change\n");
    let s = Scenario::new(Some(&original), &changed);
    s.save("+first change", "+last change");
    // New edits between the two hunks expose previously omitted context.
    let mut merged = changed;
    for n in [9, 15, 21] {
        merged = merged.replace(&format!("line {n}\n"), &format!("extra change {n}\n"));
    }
    s.write(&merged);
    let reopened = s.reopen();
    assert_eq!(
        reopened.notes.len(),
        1,
        "hunk presentation changes cannot invalidate feedback"
    );
    // If the whole selection cannot be traced, preserve it for reattachment.
    if !reopened.notes[0].needs_reattach {
        assert_eq!(reopened.notes[0].new_lines, Some((3, 27)));
    }
}

#[test]
fn unchanged_target_outside_the_new_diff_context_is_not_deleted() {
    let original: String = (1..=30).map(|n| format!("line {n}\n")).collect();
    let s = Scenario::new(
        Some(&original),
        &original.replace("line 3\n", "changed 3\n"),
    );
    s.save(" line 4", " line 4");
    s.write(&original.replace("line 27\n", "changed 27\n"));
    let reopened = s.reopen();
    assert_eq!(
        reopened.notes.len(),
        1,
        "code still exists outside the displayed hunk"
    );
    assert!(reopened.notes[0].needs_reattach);
}

#[test]
fn committed_added_code_can_become_context_while_other_changes_remain() {
    let s = Scenario::new(None, "before\ntarget\nafter\n");
    s.save("+target", "+target");
    s.commit();
    s.write("changed before\ntarget\nafter\n");
    let note = s.assert_at((2, 2));
    assert_eq!(note.excerpt, " target\n");
    assert_eq!(note.old_lines, Some((2, 2)));
}

#[test]
fn repeated_code_survives_reopen_blocks_feedback_and_recovers_when_unique() {
    let s = Scenario::new(None, "before\ntarget\nafter\n");
    let saved = s.save("+target", "+target");
    s.write("before\ntarget\nafter\nbefore\ntarget\nafter\n");
    let ambiguous = s.reopen();
    assert_eq!(ambiguous.notes.len(), 1);
    assert!(ambiguous.notes[0].needs_reattach);
    assert_eq!(ambiguous.notes[0].excerpt, saved.notes[0].excerpt);
    assert_eq!(ambiguous.notes[0].new_lines, saved.notes[0].new_lines);
    assert!(review::notes::prompt(&s.root, &ambiguous.notes)
        .unwrap_err()
        .contains("Reattach"));
    assert_eq!(s.reopen(), ambiguous);
    s.write("inserted\nbefore\ntarget\nafter\n");
    let note = s.assert_at((3, 3));
    assert!(review::notes::prompt(&s.root, &[note])
        .unwrap()
        .contains("new lines 3–3"));
}

#[test]
fn replaced_or_removed_target_is_pruned_but_failed_git_read_keeps_saved_feedback() {
    let s = Scenario::new(None, "target\n");
    let saved = s.save("+target", "+target");
    std::fs::rename(s.root.join(".git"), s.root.join("git-offline")).unwrap();
    assert!(s.store().refresh(&s.root, &saved).is_err());
    assert_eq!(s.store().load(&s.root).unwrap(), saved);
    std::fs::rename(s.root.join("git-offline"), s.root.join(".git")).unwrap();
    s.write("replacement\n");
    assert!(s.reopen().notes.is_empty());
}

#[test]
fn old_side_range_ignores_inserted_new_side_rows() {
    let s = Scenario::new(Some("heading\nstart\nend\n"), "changed heading\n");
    s.save("-start", "-end");
    s.write("prefix\nchanged heading\nstart\n");
    let current = s.reopen();
    assert_eq!(current.notes.len(), 1);
    assert!(!current.notes[0].needs_reattach);
    assert_eq!(current.notes[0].old_lines, Some((2, 3)));
    assert_eq!(current.notes[0].text, "검토 의견: keep this feedback");
}

#[test]
fn new_side_comment_never_moves_to_the_deleted_copy() {
    let s = Scenario::new(Some("first\ntarget\nlast\n"), "changed\ntarget\nlast\n");
    s.save(" target", " target");
    s.write("changed\nlast\ninserted\ntarget\n");
    s.assert_at((4, 4));
    s.write("changed\nlast\ninserted\n");
    assert!(
        s.reopen().notes.is_empty(),
        "old-side text is not a current target"
    );
}

#[test]
fn oversize_full_context_validation_fails_without_deleting_the_saved_comment() {
    let original = format!("target\n{}", "padding\n".repeat(1_100_000));
    let working = original.replacen("padding", "changed", 1);
    let s = Scenario::new(Some(&original), &working);
    let saved = s.save(" target", " target");
    s.write(&format!("{original}tail change\n"));
    let error = s.store().refresh(&s.root, &saved).unwrap_err();
    assert!(error.contains("8 MiB"), "{error}");
    assert_eq!(s.store().load(&s.root).unwrap(), saved);
}

#[test]
fn committed_clean_or_deleted_files_still_remove_obsolete_feedback() {
    for deleted in [false, true] {
        let s = Scenario::new(None, "target\n");
        s.save("+target", "+target");
        if deleted {
            std::fs::remove_file(s.root.join("sample.txt")).unwrap();
        } else {
            s.commit();
        }
        assert!(s.reopen().notes.is_empty());
    }
}
