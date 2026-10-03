// SPDX-License-Identifier: GPL-3.0-or-later
//! Portable review text with exact diff anchors. No provider-specific prompts.
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
        if let Some(range) = range.clone() {
            let lines = patch
                .lines
                .get(range)
                .ok_or("The selection changed. Select the diff again.")?;
            let old: Vec<_> = lines.iter().filter_map(|l| l.old).collect();
            let new: Vec<_> = lines.iter().filter_map(|l| l.new).collect();
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
        })
    }

    pub fn path(&self) -> PathBuf {
        OsString::from_vec(self.path.clone()).into()
    }
    pub fn label(&self) -> String {
        display_path(&self.path())
    }
}

/// Match the selected code, then use nearby context only to disambiguate repeats.
/// Never guess a location when multiple equal candidates remain.
pub fn locate(note: &Note, patch: &Patch) -> Option<std::ops::Range<usize>> {
    if note.excerpt.is_empty() {
        return None;
    }
    if note.fingerprint == fingerprint(patch) {
        if let Some(range) = &note.range {
            return Some(range.clone());
        }
    }
    let selected: Vec<_> = note.excerpt.lines().collect();
    let mut matches: Vec<_> = patch
        .lines
        .windows(selected.len())
        .enumerate()
        .filter(|(_, lines)| {
            lines
                .iter()
                .zip(&selected)
                .all(|(line, text)| line.text == *text)
        })
        .map(|(i, _)| i..i + selected.len())
        .collect();
    if matches.len() > 1 {
        matches.retain(|range| {
            let before: Vec<_> = patch.lines[..range.start]
                .iter()
                .rev()
                .filter(|l| l.old.is_some() || l.new.is_some())
                .take(note.before.len())
                .map(|l| &l.text)
                .collect();
            let after: Vec<_> = patch.lines[range.end..]
                .iter()
                .filter(|l| l.old.is_some() || l.new.is_some())
                .take(note.after.len())
                .map(|l| &l.text)
                .collect();
            before.iter().copied().eq(note.before.iter().rev())
                && after.iter().copied().eq(note.after.iter())
        });
    }
    (matches.len() == 1).then(|| matches.remove(0))
}

pub fn refresh(root: &Path, notes: &[Note]) -> Result<(Vec<Note>, Vec<String>), String> {
    let mut snapshots: Vec<Snapshot> = Vec::new();
    let mut patches: Vec<(Scope, Vec<u8>, Option<Patch>)> = Vec::new();
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
        let range = locate(note, patch);
        if !note.excerpt.is_empty() && range.is_none() {
            stale.push(note.id.clone());
            continue;
        }
        let file = snapshot
            .files
            .iter()
            .find(|f| f.path == note.path())
            .unwrap();
        *note = Note::new(note.id.clone(), snapshot, file, patch, range, &note.text)?;
    }
    Ok((updated, stale))
}

/// Re-read each distinct scope/file once. A stale or missing anchor is never
/// silently moved to a different line or exported as current feedback.
pub fn validate(root: &Path, notes: &[Note]) -> Result<Vec<String>, String> {
    refresh(root, notes).map(|(_, stale)| stale)
}

pub fn prompt(root: &Path, notes: &[Note]) -> Result<String, String> {
    let pending: Vec<_> = notes.iter().filter(|n| !n.resolved).collect();
    if pending.is_empty() {
        return Err("Add an unresolved review comment first.".into());
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
