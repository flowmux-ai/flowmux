// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows-only Files command grammar and retained tree/selection state.
//! No filesystem or native UI calls; the host validates response ownership.

use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use uuid::Uuid;

pub const PAGE_SIZE: usize = 500;
pub const MAX_ENTRIES: usize = 20_000;
pub const MAX_PATH_BYTES: usize = 16 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;
pub const MAX_EXPANDED: usize = 64;
pub const MAX_DEPTH: u16 = 64;
pub const MAX_WARNINGS: usize = 8;
// Leave room for host status fields, including up to 20k native selection indices
// and independently bounded selected-path summaries. Host checks final wire size.
const PAGE_RESERVE: usize = 256 * 1024;

#[derive(Debug, Clone, clap::Subcommand, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    Show(ShowArgs),
    Status(StatusArgs),
    Expand(RowArgs),
    Collapse(RowArgs),
    Select(SelectArgs),
    More(TokenArgs),
    Refresh(PaneArgs),
    Open(RowArgs),
    Hide(PaneArgs),
    /// Copy one closed regular file without replacing an existing destination.
    Copy(ActionArgs),
    /// Rename one closed regular file within its current directory.
    Rename(RenameArgs),
    /// Move one closed regular file within the captured root and volume.
    Move(ActionArgs),
    OperationStatus(OperationArgs),
    OperationCancel(OperationArgs),
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    #[arg(long)]
    pub token: Uuid,
    #[arg(long)]
    pub index: usize,
    #[arg(long)]
    pub destination: String,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenameArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    #[arg(long)]
    pub token: Uuid,
    #[arg(long)]
    pub index: usize,
    #[arg(long)]
    pub name: String,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationArgs {
    #[arg(long)]
    pub id: Uuid,
}

#[cfg(test)]
mod action_grammar_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn action_arguments_preserve_unicode_and_require_retained_identity() {
        let pane = Uuid::new_v4().to_string();
        let token = Uuid::new_v4().to_string();
        for name in ["copy", "move"] {
            let parsed = crate::command::Cli::try_parse_from([
                "flowmuxctl",
                "files",
                name,
                "--pane",
                &pane,
                "--token",
                &token,
                "--index",
                "3",
                "--destination",
                "한글/한 é😀.txt",
            ])
            .unwrap();
            let crate::command::Command::Files { op } = parsed.command else {
                panic!("wrong command");
            };
            let encoded = serde_json::to_value(&op).unwrap();
            assert_eq!(encoded["destination"], "한글/한 é😀.txt");
            assert_eq!(encoded["index"], 3);
            assert!(serde_json::from_value::<Op>(encoded).is_ok());
            assert!(crate::command::Cli::try_parse_from([
                "flowmuxctl",
                "files",
                name,
                "--pane",
                &pane,
                "--index",
                "3",
                "--destination",
                "한글/한 é😀.txt",
            ])
            .is_err());
        }
        let parsed = crate::command::Cli::try_parse_from([
            "flowmuxctl",
            "files",
            "rename",
            "--pane",
            &pane,
            "--token",
            &token,
            "--index",
            "0",
            "--name",
            "새 이름😀.txt",
        ])
        .unwrap();
        let crate::command::Command::Files {
            op: Op::Rename(args),
        } = parsed.command
        else {
            panic!("wrong command");
        };
        assert_eq!(args.name, "새 이름😀.txt");
        assert!(serde_json::from_value::<Op>(
            serde_json::json!({"op":"operation_cancel", "id":token, "force":true})
        )
        .is_err());
    }
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Uuid,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShowArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    #[arg(long)]
    pub root: Option<PathBuf>,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    #[arg(long, default_value_t = 0)]
    #[serde(default)]
    pub offset: usize,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    #[arg(long)]
    pub token: Uuid,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    #[arg(long)]
    pub token: Uuid,
    #[arg(long)]
    pub index: usize,
}
#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    #[arg(long)]
    pub token: Uuid,
    #[arg(long)]
    pub index: usize,
    #[arg(long, value_enum, default_value_t = SelectionMode::Replace)]
    #[serde(default)]
    pub mode: SelectionMode,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectionMode {
    #[default]
    Replace,
    Toggle,
    Range,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Owner {
    pub workspace: Uuid,
    pub pane: Uuid,
    pub instance: Uuid,
    pub generation: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Row {
    pub path: String,
    pub name: String,
    pub directory: bool,
    pub reparse: bool,
    pub depth: u16,
    pub expanded: bool,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub path: String,
    pub message: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub owner: Owner,
    pub root: PathBuf,
    /// Preorder rows; child paths always follow their directory row.
    pub rows: Vec<Row>,
    pub warnings: Vec<Warning>,
    pub truncated: bool,
    pub visited_entries: usize,
    /// Root UTF-8 bytes plus every retained row's path/name bytes.
    pub path_bytes: usize,
}
#[derive(Debug, Clone, Serialize)]
pub struct VisibleRow {
    /// Absolute index in the retained snapshot, never a page-local index.
    pub index: usize,
    #[serde(flatten)]
    pub row: Row,
    pub selected: bool,
    pub caret: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct Page {
    pub token: Option<Uuid>,
    pub owner: Option<Owner>,
    pub root: Option<PathBuf>,
    pub offset: usize,
    pub next_offset: Option<usize>,
    pub known_rows: usize,
    pub snapshot_rows: usize,
    pub rendered_rows: usize,
    pub selected_count: usize,
    pub hidden_selected_count: usize,
    pub more_available: bool,
    pub truncated: bool,
    pub stale: bool,
    pub error: Option<String>,
    pub warnings: Vec<Warning>,
    pub rows: Vec<VisibleRow>,
}

/// Normalize separators only. Unicode and case remain unchanged.
pub fn relative(value: &str) -> anyhow::Result<String> {
    ensure!(
        !value.is_empty() && value.len() <= MAX_PATH_BYTES,
        "Files path is empty or too long"
    );
    let value = value.replace('\\', "/");
    crate::editor::validate_relative_path(Path::new(&value))?;
    ensure!(
        value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != ".."),
        "Files paths require nonempty ordinary components"
    );
    Ok(value)
}
pub fn short_error(value: impl std::fmt::Display) -> String {
    value.to_string().chars().take(512).collect()
}
impl Snapshot {
    pub fn validate(&self) -> anyhow::Result<()> {
        crate::editor::validate_path(&self.root)?;
        let root = self.root.to_str().context("Files root is not Unicode")?;
        ensure!(
            self.root.is_absolute() && root.len() <= MAX_PATH_BYTES,
            "Files root must be a bounded local absolute path"
        );
        ensure!(
            self.rows.len() <= MAX_ENTRIES && self.visited_entries <= MAX_ENTRIES,
            "Files snapshot entry limit exceeded"
        );
        ensure!(
            self.warnings.len() <= MAX_WARNINGS,
            "Files warning limit exceeded"
        );
        let mut bytes = root.len();
        let mut paths = HashSet::new();
        let mut ancestors: Vec<&Row> = Vec::new();
        let mut expanded = 0;
        for row in &self.rows {
            ensure!(
                relative(&row.path)? == row.path,
                "Files snapshot path is not slash-normalized"
            );
            ensure!(
                row.path.rsplit('/').next() == Some(row.name.as_str()),
                "Files row name differs from its path"
            );
            ensure!(
                paths.insert(&row.path),
                "Files snapshot contains duplicate paths"
            );
            ensure!(
                row.depth <= MAX_DEPTH && row.depth as usize <= ancestors.len(),
                "Files tree depth/order is invalid"
            );
            ancestors.truncate(row.depth as usize);
            let expected = if let Some(parent) = ancestors.last() {
                ensure!(
                    parent.directory && !parent.reparse,
                    "Files row has a non-directory parent"
                );
                format!("{}/{}", parent.path, row.name)
            } else {
                row.name.clone()
            };
            ensure!(
                expected == row.path,
                "Files row is outside its preorder parent"
            );
            ensure!(
                !row.expanded || (row.directory && !row.reparse),
                "unsupported Files row cannot be expanded"
            );
            expanded += usize::from(row.expanded);
            bytes = bytes
                .checked_add(row.path.len() + row.name.len())
                .context("Files byte count overflow")?;
            ensure!(
                bytes <= MAX_SNAPSHOT_BYTES,
                "Files snapshot exceeds four MiB"
            );
            ancestors.push(row);
        }
        ensure!(expanded <= MAX_EXPANDED, "Files expansion limit exceeded");
        ensure!(
            bytes == self.path_bytes,
            "Files snapshot byte accounting differs"
        );
        for warning in &self.warnings {
            ensure!(
                warning.path.is_empty() || relative(&warning.path)? == warning.path,
                "invalid Files warning path"
            );
            ensure!(
                warning.message.chars().count() <= 512,
                "Files warning text exceeds limit"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct Model {
    snapshot: Option<Snapshot>,
    token: Option<Uuid>,
    expanded: HashSet<String>,
    selected: HashSet<String>,
    anchor: Option<String>,
    caret: Option<String>,
    visible_limit: usize,
    stale: bool,
    error: Option<String>,
}
impl Model {
    /// Host must first validate exact pending owner/root/generation. This method
    /// is atomic on invalid input and does not perform filesystem validation.
    pub fn apply(&mut self, snapshot: Snapshot) -> anyhow::Result<Uuid> {
        snapshot.validate()?;
        let same_root = self.snapshot.as_ref().is_some_and(|old| {
            old.owner.instance == snapshot.owner.instance
                && old.owner.pane == snapshot.owner.pane
                && old.owner.workspace == snapshot.owner.workspace
                && crate::editor_search::same_path(&old.root, &snapshot.root)
        });
        let paths: HashSet<_> = snapshot.rows.iter().map(|row| row.path.as_str()).collect();
        // A collapsed directory was not enumerated. Its missing descendants are
        // unknown, not deleted; likewise a bounded/failed partial listing cannot
        // prove that every previously selected path disappeared.
        let unloaded: HashSet<_> = snapshot
            .rows
            .iter()
            .filter(|row| row.directory && !row.reparse && !row.expanded)
            .map(|row| row.path.as_str())
            .collect();
        let warned: HashSet<_> = snapshot
            .warnings
            .iter()
            .map(|warning| warning.path.as_str())
            .collect();
        let uninspected = |path: &str| {
            snapshot.truncated
                || warned.contains("")
                // Inspect only this path's bounded ancestors. Scanning every
                // collapsed row per selected path can otherwise cost 20k² UI
                // comparisons when a large directory is replaced on refresh.
                || path.rmatch_indices('/').any(|(end, _)| {
                    let parent = &path[..end];
                    unloaded.contains(parent) || warned.contains(parent)
                })
        };
        let exists_or_unknown = |path: &str| paths.contains(path) || uninspected(path);
        let mut expanded: HashSet<_> = snapshot
            .rows
            .iter()
            .filter(|row| row.expanded)
            .map(|row| row.path.clone())
            .collect();
        if same_root {
            expanded.extend(
                self.expanded
                    .iter()
                    .filter(|path| uninspected(path))
                    .cloned(),
            );
        }
        ensure!(
            expanded.len() <= MAX_EXPANDED,
            "Files retained expansion limit exceeded"
        );
        if !same_root {
            self.selected.clear();
            self.anchor = None;
            self.caret = None;
            self.visible_limit = PAGE_SIZE;
        }
        self.selected.retain(|path| exists_or_unknown(path));
        if self
            .anchor
            .as_ref()
            .is_some_and(|path| !exists_or_unknown(path))
        {
            self.anchor = None;
        }
        if self
            .caret
            .as_ref()
            .is_some_and(|path| !exists_or_unknown(path))
        {
            self.caret = None;
        }
        self.expanded = expanded;
        self.snapshot = Some(snapshot);
        self.stale = false;
        self.error = None;
        Ok(self.renew())
    }
    pub fn fail(&mut self, error: impl std::fmt::Display) {
        self.stale = true;
        self.error = Some(short_error(error));
        self.renew();
    }
    pub fn token(&self) -> Option<Uuid> {
        self.token
    }
    pub fn cache_bytes(&self) -> usize {
        // Logical owned UTF-8 storage, not an allocator or process RAM estimate.
        // Host adds row/hash/native UTF-16 allocations to its aggregate budget.
        self.snapshot.as_ref().map_or(0, |s| {
            s.path_bytes
                + s.warnings
                    .iter()
                    .map(|warning| warning.path.len() + warning.message.len())
                    .sum::<usize>()
        }) + self.selected.iter().map(String::len).sum::<usize>()
            + self.expanded.iter().map(String::len).sum::<usize>()
            + self.anchor.as_ref().map_or(0, String::len)
            + self.caret.as_ref().map_or(0, String::len)
            + self.error.as_ref().map_or(0, String::len)
    }
    pub fn root(&self) -> Option<&Path> {
        self.snapshot.as_ref().map(|s| s.root.as_path())
    }
    pub fn snapshot_rows(&self) -> usize {
        self.snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.rows.len())
    }
    /// Retained index of the selection anchor, only while it is rendered.
    pub fn anchor_index(&self) -> Option<usize> {
        let anchor = self.anchor.as_ref()?;
        let rows = &self.snapshot.as_ref()?.rows;
        self.visible_indices()
            .into_iter()
            .take(self.visible_limit)
            .find(|index| rows[*index].path == *anchor)
    }
    pub fn refresh_inputs(&self) -> Vec<String> {
        let mut result: Vec<_> = self.expanded.iter().cloned().collect();
        result.sort();
        result
    }
    pub fn selected_paths(&self) -> Vec<String> {
        let mut paths = self.snapshot.as_ref().map_or_else(Vec::new, |s| {
            s.rows
                .iter()
                .filter(|r| self.selected.contains(&r.path))
                .map(|r| r.path.clone())
                .collect()
        });
        let present: HashSet<_> = paths.iter().map(String::as_str).collect();
        let mut uninspected: Vec<_> = self
            .selected
            .iter()
            .filter(|path| !present.contains(path.as_str()))
            .cloned()
            .collect();
        uninspected.sort();
        paths.extend(uninspected);
        paths
    }
    /// Reconcile only a confirmed single-file rename/move before refreshing.
    /// This does not create a new selection for an unselected source.
    pub fn remap_selected_path(&mut self, source: &str, destination: &str) {
        if self.selected.remove(source) {
            self.selected.insert(destination.to_owned());
        }
        if self.caret.as_deref() == Some(source) {
            self.caret = Some(destination.to_owned());
        }
        if self.anchor.as_deref() == Some(source) {
            self.anchor = Some(destination.to_owned());
        }
    }
    fn renew(&mut self) -> Uuid {
        let token = Uuid::new_v4();
        self.token = Some(token);
        token
    }
    fn check_token(&self, token: Uuid) -> anyhow::Result<()> {
        ensure!(self.token == Some(token), "Files snapshot token is stale");
        ensure!(
            !self.stale,
            "Files snapshot is stale; refresh before acting"
        );
        Ok(())
    }
    fn visible_indices(&self) -> Vec<usize> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut hidden = None;
        let mut result = Vec::new();
        for (index, row) in snapshot.rows.iter().enumerate() {
            if hidden.is_some_and(|depth| row.depth > depth) {
                continue;
            }
            hidden = None;
            result.push(index);
            if row.directory && !self.expanded.contains(&row.path) {
                hidden = Some(row.depth);
            }
        }
        result
    }
    fn row(&self, token: Uuid, index: usize) -> anyhow::Result<&Row> {
        self.check_token(token)?;
        ensure!(
            self.visible_indices()
                .into_iter()
                .take(self.visible_limit)
                .any(|i| i == index),
            "Files row is not currently rendered"
        );
        self.snapshot
            .as_ref()
            .and_then(|s| s.rows.get(index))
            .context("Files row is unavailable")
    }
    /// Returns true only when an expansion change needs a new directory snapshot.
    /// Collapse can render immediately; its cached children retain logical selection.
    pub fn set_expanded(
        &mut self,
        token: Uuid,
        index: usize,
        expanded: bool,
    ) -> anyhow::Result<bool> {
        let row = self.row(token, index)?;
        ensure!(
            row.directory && !row.reparse,
            "Files row is not an expandable directory"
        );
        let path = row.path.clone();
        if self.expanded.contains(&path) == expanded {
            return Ok(false);
        }
        if expanded {
            ensure!(
                self.expanded.len() < MAX_EXPANDED,
                "Files allows at most 64 expanded directories"
            );
            self.expanded.insert(path);
        } else {
            self.expanded.remove(&path);
        }
        self.renew();
        Ok(expanded)
    }
    pub fn expand(&mut self, token: Uuid, index: usize) -> anyhow::Result<bool> {
        self.set_expanded(token, index, true)
    }
    pub fn collapse(&mut self, token: Uuid, index: usize) -> anyhow::Result<bool> {
        self.set_expanded(token, index, false)
    }
    pub fn select(&mut self, token: Uuid, index: usize, mode: SelectionMode) -> anyhow::Result<()> {
        let path = self.row(token, index)?.path.clone();
        match mode {
            SelectionMode::Replace => {
                self.selected.clear();
                self.selected.insert(path.clone());
                self.anchor = Some(path.clone());
            }
            SelectionMode::Toggle => {
                if !self.selected.contains(&path) {
                    ensure!(
                        self.selected.len() < MAX_ENTRIES
                            && self.selected.iter().map(String::len).sum::<usize>() + path.len()
                                <= MAX_SNAPSHOT_BYTES,
                        "Files selected-path cache limit reached"
                    );
                }
                if !self.selected.remove(&path) {
                    self.selected.insert(path.clone());
                }
                self.anchor = Some(path.clone());
            }
            SelectionMode::Range => {
                let indices: Vec<_> = self
                    .visible_indices()
                    .into_iter()
                    .take(self.visible_limit)
                    .collect();
                let rows = &self.snapshot.as_ref().unwrap().rows;
                let target = indices.iter().position(|i| *i == index).unwrap();
                let anchor = self
                    .anchor
                    .as_ref()
                    .and_then(|path| indices.iter().position(|i| rows[*i].path == *path))
                    .unwrap_or(target);
                self.selected.clear();
                self.selected.extend(
                    indices[anchor.min(target)..=anchor.max(target)]
                        .iter()
                        .map(|i| rows[*i].path.clone()),
                );
                self.anchor = Some(rows[indices[anchor]].path.clone());
            }
        }
        self.caret = Some(path);
        Ok(())
    }
    /// Apply one real native selection notification atomically. Indices are
    /// retained-row indices translated by the panel, not native list offsets.
    pub fn select_native(
        &mut self,
        token: Uuid,
        indices: &[usize],
        caret: Option<usize>,
        anchor: Option<usize>,
        retain_hidden: bool,
    ) -> anyhow::Result<()> {
        self.check_token(token)?;
        ensure!(
            indices.len() <= MAX_ENTRIES,
            "Files native selection exceeds its row limit"
        );
        let rendered: HashSet<_> = self
            .visible_indices()
            .into_iter()
            .take(self.visible_limit)
            .collect();
        let rows = &self
            .snapshot
            .as_ref()
            .context("Files snapshot is unavailable")?
            .rows;
        let mut seen = HashSet::new();
        for index in indices {
            ensure!(
                rendered.contains(index) && seen.insert(*index),
                "Files native selection contains unavailable or duplicate rows"
            );
        }
        for index in [caret, anchor].into_iter().flatten() {
            ensure!(
                rendered.contains(&index),
                "Files native caret/anchor is not rendered"
            );
        }
        let rendered_paths: HashSet<_> = rendered
            .iter()
            .map(|index| rows[*index].path.as_str())
            .collect();
        let mut selected: HashSet<String> = if retain_hidden {
            self.selected
                .iter()
                .filter(|path| !rendered_paths.contains(path.as_str()))
                .cloned()
                .collect()
        } else {
            HashSet::new()
        };
        selected.extend(indices.iter().map(|index| rows[*index].path.clone()));
        ensure!(
            selected.len() <= MAX_ENTRIES
                && selected.iter().map(String::len).sum::<usize>() <= MAX_SNAPSHOT_BYTES,
            "Files selected-path cache limit reached"
        );
        self.selected = selected;
        self.caret = caret.map(|index| rows[index].path.clone());
        self.anchor = anchor.map(|index| rows[index].path.clone());
        Ok(())
    }
    pub fn more(&mut self, token: Uuid) -> anyhow::Result<usize> {
        self.check_token(token)?;
        self.visible_limit = self
            .visible_limit
            .saturating_add(PAGE_SIZE)
            .min(MAX_ENTRIES);
        Ok(self.visible_indices().len().min(self.visible_limit))
    }
    /// Return only a validated relative path. Host still uses normal async Open.
    pub fn open_path(&self, token: Uuid, index: usize) -> anyhow::Result<PathBuf> {
        let row = self.row(token, index)?;
        ensure!(
            !row.directory && !row.reparse,
            "Files row is not a supported regular file"
        );
        Ok(PathBuf::from(&row.path))
    }
    pub fn rendered_rows(&self) -> Vec<VisibleRow> {
        self.visible_indices()
            .into_iter()
            .take(self.visible_limit)
            .map(|index| self.view(index))
            .collect()
    }
    fn view(&self, index: usize) -> VisibleRow {
        let mut row = self.snapshot.as_ref().unwrap().rows[index].clone();
        row.expanded = self.expanded.contains(&row.path);
        VisibleRow {
            index,
            selected: self.selected.contains(&row.path),
            caret: self.caret.as_ref() == Some(&row.path),
            row,
        }
    }
    /// Offset is a visible rendered-row offset; returned indices remain absolute.
    pub fn page(&self, offset: usize) -> anyhow::Result<Page> {
        let indices = self.visible_indices();
        let rendered = indices.len().min(self.visible_limit);
        ensure!(
            offset <= rendered,
            "Files page offset exceeds rendered rows"
        );
        let mut rows = Vec::new();
        let mut bytes = PAGE_RESERVE
            + self
                .snapshot
                .as_ref()
                .map(|snapshot| serde_json::to_vec(&snapshot.warnings).map(|bytes| bytes.len()))
                .transpose()?
                .unwrap_or(0);
        for index in indices.iter().take(rendered).skip(offset).take(PAGE_SIZE) {
            let row = self.view(*index);
            let size = serde_json::to_vec(&row)?.len() + 1;
            if bytes + size > MAX_PAGE_BYTES {
                break;
            }
            bytes += size;
            rows.push(row);
        }
        let next = offset + rows.len();
        let visible_selected = indices
            .iter()
            .take(rendered)
            .filter(|index| {
                self.selected
                    .contains(&self.snapshot.as_ref().unwrap().rows[**index].path)
            })
            .count();
        let result = Page {
            token: self.token,
            owner: self.snapshot.as_ref().map(|s| s.owner.clone()),
            root: self.snapshot.as_ref().map(|s| s.root.clone()),
            offset,
            next_offset: (next < rendered).then_some(next),
            known_rows: indices.len(),
            snapshot_rows: self.snapshot.as_ref().map_or(0, |s| s.rows.len()),
            rendered_rows: rendered,
            selected_count: self.selected.len(),
            hidden_selected_count: self.selected.len() - visible_selected,
            more_available: rendered < indices.len(),
            truncated: self.snapshot.as_ref().is_some_and(|s| s.truncated),
            stale: self.stale,
            error: self.error.clone(),
            warnings: self
                .snapshot
                .as_ref()
                .map_or_else(Vec::new, |s| s.warnings.clone()),
            rows,
        };
        ensure!(
            serde_json::to_vec(&result)?.len() <= MAX_PAGE_BYTES,
            "Files page exceeds its response budget"
        );
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn snapshot(rows: Vec<Row>) -> Snapshot {
        let root = if cfg!(windows) {
            PathBuf::from(r"C:\owned-한글")
        } else {
            PathBuf::from("/owned-한글")
        };
        Snapshot {
            owner: Owner {
                workspace: Uuid::new_v4(),
                pane: Uuid::new_v4(),
                instance: Uuid::new_v4(),
                generation: 1,
            },
            path_bytes: root.to_str().unwrap().len()
                + rows
                    .iter()
                    .map(|r| r.path.len() + r.name.len())
                    .sum::<usize>(),
            root,
            visited_entries: rows.len(),
            rows,
            warnings: Vec::new(),
            truncated: false,
        }
    }
    fn row(path: &str, directory: bool, expanded: bool) -> Row {
        Row {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            directory,
            reparse: false,
            depth: path.bytes().filter(|b| *b == b'/').count() as u16,
            expanded,
        }
    }
    #[test]
    fn pages_add_exact_500_and_indices_are_retained_indices() {
        let mut model = Model::default();
        let token = model
            .apply(snapshot(
                (0..1105)
                    .map(|i| row(&format!("file-{i:04}.txt"), false, false))
                    .collect(),
            ))
            .unwrap();
        assert_eq!(model.page(0).unwrap().rows.len(), 500);
        assert!(model.open_path(token, 500).is_err());
        assert_eq!(model.more(token).unwrap(), 1000);
        assert_eq!(model.page(500).unwrap().rows[0].index, 500);
        assert_eq!(model.more(token).unwrap(), 1105);
        assert_eq!(model.page(1000).unwrap().rows.len(), 105);
        assert!(!model.page(1000).unwrap().more_available);
        assert!(model.page(1106).is_err());
    }
    #[test]
    fn selection_survives_collapse_without_becoming_another_row() {
        let mut model = Model::default();
        let token = model
            .apply(snapshot(vec![
                row("dir", true, true),
                row("dir/한.txt", false, false),
                row("dir/한.txt", false, false),
                row("last.txt", false, false),
            ]))
            .unwrap();
        model.select(token, 1, SelectionMode::Replace).unwrap();
        model.select(token, 2, SelectionMode::Range).unwrap();
        assert_eq!(model.selected_paths(), ["dir/한.txt", "dir/한.txt"]);
        model.collapse(token, 0).unwrap();
        assert!(model.open_path(token, 3).is_err());
        let token = model.token().unwrap();
        let page = model.page(0).unwrap();
        assert_eq!(
            page.rows.iter().map(|r| r.index).collect::<Vec<_>>(),
            [0, 3]
        );
        assert_eq!(page.hidden_selected_count, 2);
        model.expand(token, 0).unwrap();
        assert_eq!(
            model
                .page(0)
                .unwrap()
                .rows
                .iter()
                .filter(|r| r.selected)
                .count(),
            2
        );
        model
            .select(model.token().unwrap(), 3, SelectionMode::Toggle)
            .unwrap();
        assert_eq!(model.selected_paths().len(), 3);
    }
    #[test]
    fn refresh_retains_paths_not_old_indices_and_invalid_input_is_atomic() {
        let mut model = Model::default();
        let mut initial = snapshot(vec![row("b.txt", false, false), row("c.txt", false, false)]);
        let token = model.apply(initial.clone()).unwrap();
        model.select(token, 0, SelectionMode::Replace).unwrap();
        initial.rows.insert(0, row("a.txt", false, false));
        initial.owner.generation += 1;
        initial.visited_entries += 1;
        initial.path_bytes += "a.txt".len() * 2;
        let token2 = model.apply(initial.clone()).unwrap();
        assert_ne!(token, token2);
        assert_eq!(
            model
                .page(0)
                .unwrap()
                .rows
                .iter()
                .find(|r| r.selected)
                .unwrap()
                .index,
            1
        );
        initial.rows[0].path = "../escape".into();
        assert!(model.apply(initial).is_err());
        assert_eq!(model.token(), Some(token2));
        model.fail("directory denied");
        assert!(model.page(0).unwrap().stale);
        assert!(model.open_path(model.token().unwrap(), 1).is_err());
    }
    #[test]
    fn native_selection_is_atomic_and_preserves_exact_caret_anchor() {
        let mut model = Model::default();
        let token = model
            .apply(snapshot(vec![
                row("dir", true, true),
                row("dir/한.txt", false, false),
                row("a.txt", false, false),
                row("b.txt", false, false),
            ]))
            .unwrap();
        model.select(token, 1, SelectionMode::Replace).unwrap();
        model.collapse(token, 0).unwrap();
        let token = model.token().unwrap();
        model
            .select_native(token, &[2, 3], Some(3), Some(2), true)
            .unwrap();
        assert_eq!(model.selected_paths(), ["dir/한.txt", "a.txt", "b.txt"]);
        assert_eq!(model.caret.as_deref(), Some("b.txt"));
        assert_eq!(model.anchor.as_deref(), Some("a.txt"));
        assert_eq!(model.anchor_index(), Some(2));
        assert_eq!(model.page(0).unwrap().hidden_selected_count, 1);
        let selected = model.selected_paths();
        assert!(model
            .select_native(token, &[1], Some(1), None, false)
            .is_err());
        assert!(model
            .select_native(token, &[2, 2], Some(2), None, false)
            .is_err());
        assert_eq!(model.selected_paths(), selected);
        model
            .select_native(token, &[3], Some(3), Some(3), false)
            .unwrap();
        assert_eq!(model.selected_paths(), ["b.txt"]);
    }

    #[test]
    fn refresh_does_not_treat_uninspected_collapsed_children_as_deleted() {
        let mut model = Model::default();
        let original = snapshot(vec![
            row("dir", true, true),
            row("dir/한.txt", false, false),
        ]);
        let token = model.apply(original.clone()).unwrap();
        model.select(token, 1, SelectionMode::Replace).unwrap();
        model.collapse(token, 0).unwrap();
        let mut collapsed = original.clone();
        collapsed.rows = vec![row("dir", true, false)];
        collapsed.path_bytes = collapsed.root.to_str().unwrap().len() + 6;
        collapsed.visited_entries = 1;
        collapsed.owner.generation += 1;
        model.apply(collapsed.clone()).unwrap();
        assert_eq!(model.selected_paths(), ["dir/한.txt"]);
        assert_eq!(model.page(0).unwrap().hidden_selected_count, 1);
        // A successful enumeration of the expanded directory can prove deletion.
        collapsed.rows[0].expanded = true;
        collapsed.owner.generation += 1;
        model.apply(collapsed).unwrap();
        assert!(model.selected_paths().is_empty());
    }

    #[test]
    fn maximum_size_refresh_and_native_selection_remain_path_bounded() {
        let mut model = Model::default();
        let initial = snapshot(
            (0..MAX_ENTRIES)
                .map(|index| row(&format!("old-{index:05}.txt"), false, false))
                .collect(),
        );
        let mut replacement = initial.clone();
        let token = model.apply(initial).unwrap();
        for _ in 1..MAX_ENTRIES / PAGE_SIZE {
            model.more(token).unwrap();
        }
        let indices: Vec<_> = (0..MAX_ENTRIES).collect();
        model
            .select_native(token, &indices, Some(MAX_ENTRIES - 1), Some(0), false)
            .unwrap();
        assert_eq!(model.selected_paths().len(), MAX_ENTRIES);
        replacement.rows = (0..MAX_ENTRIES)
            .map(|index| row(&format!("new-{index:05}"), true, false))
            .collect();
        replacement.path_bytes = replacement.root.to_str().unwrap().len()
            + replacement
                .rows
                .iter()
                .map(|row| row.path.len() + row.name.len())
                .sum::<usize>();
        replacement.owner.generation += 1;
        model.apply(replacement).unwrap();
        let page = model.page(0).unwrap();
        assert_eq!(page.known_rows, MAX_ENTRIES);
        assert_eq!(page.selected_count, 0);
        assert!(model.selected_paths().is_empty());
        assert!(model.refresh_inputs().is_empty());
        assert!(model.cache_bytes() <= MAX_SNAPSHOT_BYTES);
    }

    #[test]
    fn invalid_tree_structure_and_budget_are_rejected_before_apply() {
        let mut invalid = snapshot(vec![
            row("file", false, false),
            row("file/child", false, false),
        ]);
        assert!(invalid.validate().is_err());
        invalid = snapshot(vec![row("dir", true, true)]);
        invalid.rows[0].reparse = true;
        assert!(invalid.validate().is_err());
        invalid = snapshot(vec![row("a", false, false), row("a", false, false)]);
        assert!(invalid.validate().is_err());
        invalid = snapshot(
            (0..MAX_ENTRIES + 1)
                .map(|i| row(&format!("file-{i}"), false, false))
                .collect(),
        );
        assert!(invalid.validate().is_err());
    }
    #[test]
    fn confirmed_move_remaps_selection_without_selecting_an_unselected_file() {
        let mut model = Model::default();
        let mut initial = snapshot(vec![
            row("원본.txt", false, false),
            row("다른.txt", false, false),
        ]);
        let token = model.apply(initial.clone()).unwrap();
        model.select(token, 0, SelectionMode::Replace).unwrap();
        model.remap_selected_path("원본.txt", "한😀.txt");
        model.remap_selected_path("다른.txt", "unselected.txt");
        assert_eq!(model.selected_paths(), ["한😀.txt"]);
        assert_eq!(model.caret.as_deref(), Some("한😀.txt"));
        assert_eq!(model.anchor.as_deref(), Some("한😀.txt"));
        initial.rows = vec![
            row("한😀.txt", false, false),
            row("unselected.txt", false, false),
        ];
        initial.path_bytes = initial.root.to_str().unwrap().len()
            + initial
                .rows
                .iter()
                .map(|row| row.path.len() + row.name.len())
                .sum::<usize>();
        initial.owner.generation += 1;
        model.apply(initial).unwrap();
        assert_eq!(model.selected_paths(), ["한😀.txt"]);
    }
    #[test]
    fn path_validation_and_byte_pages_preserve_exact_unicode() {
        assert_eq!(relative("한글\\한 é😀.txt").unwrap(), "한글/한 é😀.txt");
        for invalid in ["../x", "a//b", "a/./b", "C:/x", "NUL", "a:b", "a\0b"] {
            assert!(relative(invalid).is_err(), "{invalid:?}");
        }
        let names: Vec<_> = (0..500)
            .map(|i| format!("{i}-{}.txt", "한".repeat(1300)))
            .collect();
        let mut model = Model::default();
        let mut listing = snapshot(names.iter().map(|name| row(name, false, false)).collect());
        listing.warnings = (0..MAX_WARNINGS)
            .map(|index| Warning {
                path: format!("{index}-{}", "한".repeat(5000)),
                message: "\"\\\n😀".repeat(128),
            })
            .collect();
        model.apply(listing).unwrap();
        let first = model.page(0).unwrap();
        assert!(first.rows.len() < 500 && !first.rows.is_empty());
        assert!(serde_json::to_vec(&first).unwrap().len() <= MAX_PAGE_BYTES);
        let next = model.page(first.next_offset.unwrap()).unwrap();
        assert_eq!(next.rows[0].index, first.rows.len());
        let envelope = serde_json::json!({
            "page": first,
            "native_selected_indices": (0..MAX_ENTRIES).collect::<Vec<_>>(),
            "selected_paths": ["한".repeat(5461)],
            "visible_selected_paths": ["한".repeat(1820)],
            "captured_root": format!("C:\\{}", "a\\".repeat(8190)),
            "error": "\"\\\n😀".repeat(128),
        });
        let encoded = serde_json::to_vec(&envelope).unwrap();
        assert!(encoded.len() <= MAX_PAGE_BYTES);
        let decoded: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded["page"]["rows"][0]["name"], names[0]);
        assert_eq!(
            decoded["page"]["warnings"][0]["message"],
            "\"\\\n😀".repeat(128)
        );
    }
}
