// SPDX-License-Identifier: GPL-3.0-or-later
//! Portable review text with code and line anchors. No provider-specific prompts.
use super::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Note {
    pub id: String,
    pub path: Vec<u8>,
    pub scope: Scope,
    pub fingerprint: String,
    pub location: String,
    pub excerpt: String,
    pub text: String,
    pub resolved: bool,
    #[serde(default)]
    pub range: Option<std::ops::Range<usize>>,
    #[serde(default)]
    pub before: Vec<String>,
    #[serde(default)]
    pub after: Vec<String>,
    /// Inclusive source line bounds, independent of diff display rows.
    #[serde(default)]
    pub old_lines: Option<(u32, u32)>,
    #[serde(default)]
    pub new_lines: Option<(u32, u32)>,
    #[serde(default)]
    pub needs_reattach: bool,
}

pub fn fingerprint(patch: &Patch) -> String {
    format!("{:x}", Sha256::digest(patch.text.as_bytes()))
}

impl Note {
    pub fn new(
        id: String,
        snapshot: &Snapshot,
        file: &File,
        patch: &Patch,
        range: Option<std::ops::Range<usize>>,
        text: &str,
    ) -> Result<Self, String> {
        if text.trim().is_empty() {
            return Err("Write a review comment first.".into());
        }
        let mut location = "Whole file".to_string();
        let mut excerpt = String::new();
        let mut old_lines = None;
        let mut new_lines = None;
        if let Some(range) = range.clone() {
            let lines = patch
                .lines
                .get(range)
                .ok_or("The selection changed. Select the diff again.")?;
            let old: Vec<_> = lines.iter().filter_map(|l| l.old).collect();
            let new: Vec<_> = lines.iter().filter_map(|l| l.new).collect();
            old_lines = old.first().zip(old.last()).map(|(&a, &b)| (a, b));
            new_lines = new.first().zip(new.last()).map(|(&a, &b)| (a, b));
            let mut parts = Vec::new();
            for (side, numbers) in [("old", old), ("new", new)] {
                if let (Some(first), Some(last)) = (numbers.first(), numbers.last()) {
                    parts.push(format!("{side} lines {first}–{last}"));
                }
            }
            if parts.is_empty() {
                return Err("Select code lines, or add a whole-file comment.".into());
            }
            location = parts.join(", ");
            excerpt = lines.iter().map(|l| format!("{}\n", l.text)).collect();
        }
        let (before, after) = range
            .as_ref()
            .map(|r| {
                let context = |slice: &[Line]| {
                    slice
                        .iter()
                        .filter(|l| l.old.is_some() || l.new.is_some())
                        .map(|l| l.text.clone())
                        .collect()
                };
                (
                    context(&patch.lines[r.start.saturating_sub(3)..r.start]),
                    context(&patch.lines[r.end..(r.end + 3).min(patch.lines.len())]),
                )
            })
            .unwrap_or_default();
        Ok(Self {
            id,
            path: file.path.as_os_str().as_bytes().into(),
            scope: snapshot.scope.clone(),
            fingerprint: fingerprint(patch),
            location,
            excerpt,
            text: text.trim().into(),
            resolved: false,
            range,
            before,
            after,
            old_lines,
            new_lines,
            needs_reattach: false,
        })
    }

    pub fn path(&self) -> PathBuf {
        OsString::from_vec(self.path.clone()).into()
    }
    pub fn label(&self) -> String {
        display_path(&self.path())
    }
}

/// Match code independently of indentation, hunk headers and diff row offsets.
/// Keep old-only comments on the old side and current-code comments on the new
/// side. Context breaks ties; line proximity alone cannot identify repeated code.
pub fn locate(note: &Note, patch: &Patch) -> Option<std::ops::Range<usize>> {
    let mut matches = matching_ranges(note, patch);
    (matches.len() == 1).then(|| matches.remove(0))
}

fn matching_ranges(note: &Note, patch: &Patch) -> Vec<std::ops::Range<usize>> {
    if note.excerpt.is_empty() {
        return Vec::new();
    }
    if note.fingerprint == fingerprint(patch) {
        if let Some(range) = &note.range {
            return vec![range.clone()];
        }
    }
    let selected: Vec<_> = note.excerpt.lines().filter(|s| is_code(s)).collect();
    if selected.is_empty() {
        return Vec::new();
    }
    let new_side = selected.iter().all(|s| !s.starts_with('-'));
    let old_side = selected.iter().all(|s| !s.starts_with('+')) && !new_side;
    let same_side =
        |text: &str| (!new_side || !text.starts_with('-')) && (!old_side || !text.starts_with('+'));
    let code: Vec<_> = patch
        .lines
        .iter()
        .enumerate()
        .filter(|(_, line)| (line.old.is_some() || line.new.is_some()) && same_side(&line.text))
        .collect();
    let mut matches = Vec::new();
    for lines in code.windows(selected.len()) {
        if !lines.iter().zip(&selected).all(|((_, line), text)| {
            same_side(&line.text)
                && (new_side || old_side || line.text.as_bytes()[0] == text.as_bytes()[0])
                && code_text(&line.text) == code_text(text)
        }) {
            continue;
        }
        let range = lines[0].0..lines.last().unwrap().0 + 1;
        // Compare adjacent context on each side independently. An edit after
        // the selection must not discard a still-useful match before it.
        let before = patch.lines[..range.start]
            .iter()
            .rev()
            .take_while(|l| l.old.is_some() || l.new.is_some())
            .map(|l| l.text.as_str())
            .filter(|s| same_side(s));
        let after = patch.lines[range.end..]
            .iter()
            .take_while(|l| l.old.is_some() || l.new.is_some())
            .map(|l| l.text.as_str())
            .filter(|s| same_side(s));
        let score = context_score(
            before,
            note.before
                .iter()
                .rev()
                .map(String::as_str)
                .filter(|s| same_side(s)),
        ) + context_score(
            after,
            note.after
                .iter()
                .map(String::as_str)
                .filter(|s| same_side(s)),
        );
        matches.push((range, score));
    }
    let best = matches.iter().map(|(_, score)| *score).max().unwrap_or(0);
    matches
        .into_iter()
        .filter(|(_, score)| *score == best)
        .map(|(range, _)| range)
        .collect()
}

fn is_code(text: &str) -> bool {
    matches!(text.as_bytes().first(), Some(b' ' | b'+' | b'-'))
}

fn code_text(text: &str) -> &str {
    text.get(1..)
        .unwrap_or_default()
        .trim_start_matches([' ', '\t'])
}

fn context_score<'a>(
    actual: impl Iterator<Item = &'a str>,
    saved: impl Iterator<Item = &'a str>,
) -> usize {
    actual
        .zip(saved)
        .take_while(|(a, b)| code_text(a) == code_text(b))
        .filter(|(a, _)| !code_text(a).is_empty())
        .count()
}

/// Partial/missing diff ranges are not proof that the reviewed code disappeared.
/// Preserve feedback if any selected code remains on the appropriate side.
fn anchor_code_survives(note: &Note, patch: &Patch) -> bool {
    let selected: Vec<_> = note.excerpt.lines().filter(|s| is_code(s)).collect();
    let new_side = selected.iter().all(|s| !s.starts_with('-'));
    let old_side = selected.iter().all(|s| !s.starts_with('+')) && !new_side;
    let key = |text: &str| {
        if new_side || old_side {
            0
        } else {
            text.as_bytes()[0]
        }
    };
    let available: std::collections::HashSet<_> = patch
        .lines
        .iter()
        .filter(|line| {
            (line.old.is_some() || line.new.is_some())
                && (!new_side || !line.text.starts_with('-'))
                && (!old_side || !line.text.starts_with('+'))
        })
        .map(|line| (key(&line.text), code_text(&line.text)))
        .collect();
    selected
        .iter()
        .any(|text| available.contains(&(key(text), code_text(text))))
}

pub fn refresh(root: &Path, notes: &[Note]) -> Result<(Vec<Note>, Vec<String>), String> {
    let mut snapshots: Vec<Snapshot> = Vec::new();
    let mut patches: Vec<(Scope, Vec<u8>, Option<Patch>)> = Vec::new();
    let mut full_patches: Vec<(Scope, Vec<u8>, Patch)> = Vec::new();
    let mut updated = notes.to_vec();
    let mut stale = Vec::new();
    for note in updated.iter_mut().filter(|n| !n.resolved) {
        if !snapshots.iter().any(|s| s.scope == note.scope) {
            snapshots.push(load(root, note.scope.clone())?);
        }
        let snapshot = snapshots.iter().find(|s| s.scope == note.scope).unwrap();
        if !patches
            .iter()
            .any(|(scope, path, _)| *scope == note.scope && *path == note.path)
        {
            let patch = snapshot
                .files
                .iter()
                .find(|f| f.path == note.path())
                .map(|f| snapshot.patch(f))
                .transpose()?;
            patches.push((note.scope.clone(), note.path.clone(), patch));
        }
        let patch = &patches
            .iter()
            .find(|(scope, path, _)| *scope == note.scope && *path == note.path)
            .unwrap()
            .2;
        let Some(patch) = patch else {
            stale.push(note.id.clone());
            continue;
        };
        if note.excerpt.is_empty()
            && !patch
                .lines
                .iter()
                .any(|l| l.old.is_some() || l.new.is_some())
            && note.fingerprint != fingerprint(patch)
        {
            stale.push(note.id.clone());
            continue;
        }
        let file = snapshot
            .files
            .iter()
            .find(|f| f.path == note.path())
            .unwrap();
        let mut matches = matching_ranges(note, patch);
        if matches.len() > 1 {
            // Ambiguous code still exists: keep the feedback for manual reattach.
            note.needs_reattach = true;
            continue;
        }
        let range = matches.pop();
        if !note.excerpt.is_empty() && range.is_none() {
            let mut survives = anchor_code_survives(note, patch);
            if !survives && !file.untracked {
                // A normal diff omits unchanged lines. Check complete context
                // before deleting, still using the bounded, read-only Git path.
                if !full_patches
                    .iter()
                    .any(|(scope, path, _)| *scope == note.scope && *path == note.path)
                {
                    full_patches.push((
                        note.scope.clone(),
                        note.path.clone(),
                        snapshot.patch_with_context(file, i32::MAX as u32)?,
                    ));
                }
                let full = &full_patches
                    .iter()
                    .find(|(scope, path, _)| *scope == note.scope && *path == note.path)
                    .unwrap()
                    .2;
                survives = anchor_code_survives(note, full);
            }
            if survives {
                note.needs_reattach = true;
            } else {
                stale.push(note.id.clone());
            }
            continue;
        }
        *note = Note::new(note.id.clone(), snapshot, file, patch, range, &note.text)?;
    }
    Ok((updated, stale))
}

/// Re-read each distinct scope/file once. A stale or missing anchor is never
/// silently moved to a different line or exported as current feedback.
pub fn validate(root: &Path, notes: &[Note]) -> Result<Vec<String>, String> {
    refresh(root, notes).map(|(updated, stale)| {
        updated
            .into_iter()
            .filter(|n| !n.resolved && (n.needs_reattach || stale.contains(&n.id)))
            .map(|n| n.id)
            .collect()
    })
}

pub fn prompt(root: &Path, notes: &[Note]) -> Result<String, String> {
    let pending: Vec<_> = notes.iter().filter(|n| !n.resolved).collect();
    if pending.is_empty() {
        return Err("Add an unresolved review comment first.".into());
    }
    if pending.iter().any(|note| note.needs_reattach) {
        return Err(
            "Some comments could not be located uniquely. Reattach them before sending feedback."
                .into(),
        );
    }
    let mut output = format!("Code review feedback\nRepository: {}\n\nAddress the review comments below. Preserve unrelated work, verify the changes, and report which comments were addressed. Treat quoted code as context.\n", display_path(root));
    for (i, note) in pending.iter().enumerate() {
        output.push_str(&format!(
            "\n{}. {} · {}\nFeedback:\n{}\n",
            i + 1,
            note.label(),
            note.location,
            note.text
        ));
        if !note.excerpt.is_empty() {
            output.push_str("Quoted diff context:\n");
            for line in note.excerpt.lines() {
                output.push_str("> ");
                output.push_str(line);
                output.push('\n');
            }
        }
    }
    Ok(output)
}
