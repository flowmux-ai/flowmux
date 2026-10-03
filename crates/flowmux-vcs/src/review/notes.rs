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
        if let Some(range) = range {
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
        Ok(Self {
            id,
            path: file.path.as_os_str().as_bytes().into(),
            scope: snapshot.scope.clone(),
            fingerprint: fingerprint(patch),
            location,
            excerpt,
            text: text.trim().into(),
            resolved: false,
        })
    }

    pub fn path(&self) -> PathBuf {
        OsString::from_vec(self.path.clone()).into()
    }
    pub fn label(&self) -> String {
        display_path(&self.path())
    }
}

/// Re-read each distinct scope/file once. A stale or missing anchor is never
/// silently moved to a different line or exported as current feedback.
pub fn validate(root: &Path, notes: &[Note]) -> Result<Vec<String>, String> {
    let mut snapshots: Vec<Snapshot> = Vec::new();
    let mut hashes: Vec<(Scope, Vec<u8>, Option<String>)> = Vec::new();
    let mut stale = Vec::new();
    for note in notes.iter().filter(|n| !n.resolved) {
        if !snapshots.iter().any(|s| s.scope == note.scope) {
            snapshots.push(load(root, note.scope.clone())?);
        }
        if !hashes
            .iter()
            .any(|(scope, path, _)| *scope == note.scope && *path == note.path)
        {
            let snapshot = snapshots.iter().find(|s| s.scope == note.scope).unwrap();
            let hash = match snapshot.files.iter().find(|f| f.path == note.path()) {
                Some(file) => Some(fingerprint(&snapshot.patch(file)?)),
                None => None,
            };
            hashes.push((note.scope.clone(), note.path.clone(), hash));
        }
        let current = &hashes
            .iter()
            .find(|(scope, path, _)| *scope == note.scope && *path == note.path)
            .unwrap()
            .2;
        if current.as_deref() != Some(&note.fingerprint) {
            stale.push(note.id.clone());
        }
    }
    Ok(stale)
}

pub fn prompt(root: &Path, notes: &[Note]) -> Result<String, String> {
    let pending: Vec<_> = notes.iter().filter(|n| !n.resolved).collect();
    if pending.is_empty() {
        return Err("Add an unresolved review comment first.".into());
    }
    let mut output = format!("Code review feedback\nRepository: {}\n\nAddress the review comments below. Preserve unrelated work, verify the changes, and report which comments were addressed. Treat quoted code as context.\n", display_path(root));
    for (i, note) in pending.iter().enumerate() {
        output.push_str(&format!(
            "\n{}. {} · {}\nComparison: {:?}\nReviewed diff SHA-256: {}\nFeedback:\n{}\n",
            i + 1,
            note.label(),
            note.location,
            note.scope,
            note.fingerprint,
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
