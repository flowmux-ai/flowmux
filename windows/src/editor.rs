// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows editor commands and bounded, filesystem-independent validation.
use anyhow::{ensure, Context};
use flowmux_core::{EditorFileState, EditorSessionState};
use flowmux_editor::{EditorSessionSnapshot, EDITOR_ZOOM_MAX, EDITOR_ZOOM_MIN};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

pub const MAX_DOCUMENTS: usize = 128;
pub const MAX_DOCUMENT_BYTES: usize = flowmux_editor::DEFAULT_MAX_DOCUMENT_BYTES as usize;
pub const MAX_PATH_UNITS: usize = 32767;

// Individual Args builders keep the debug Windows clap stack bounded.
#[derive(Debug, Clone, clap::Subcommand, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    Open(OpenArgs),
    Pick(PickArgs),
    Status(SurfaceArgs),
    Command(CommandArgs),
    CheckDisk(SurfaceArgs),
    Flush(SurfaceArgs),
}

#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PickArgs {
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Option<Uuid>,
}

#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OpenArgs {
    pub path: PathBuf,
    #[arg(long, value_parser = crate::command::parse_id)]
    pub pane: Option<Uuid>,
    #[arg(long)]
    pub root: Option<PathBuf>,
}

#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceArgs {
    #[arg(value_parser = crate::command::parse_id)]
    pub surface: Uuid,
}

#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandArgs {
    #[arg(value_parser = crate::command::parse_id)]
    pub surface: Uuid,
    #[arg(value_enum)]
    pub action: Action,
    #[arg(long)]
    pub text: Option<String>,
    #[arg(long)]
    pub path: Option<String>,
    #[arg(long)]
    #[serde(default)]
    pub overwrite: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Read,
    ReplaceText,
    Undo,
    Redo,
    Save,
    SaveAs,
    SaveAll,
    CloseDocument,
    DiscardDocument,
    Compare,
    KeepMine,
    Reload,
    Recover,
    DiscardRecovery,
}

pub fn validate_command(args: &CommandArgs) -> anyhow::Result<()> {
    ensure!(
        args.text.is_some() == (args.action == Action::ReplaceText),
        "--text is required only for replace-text"
    );
    ensure!(
        args.path.is_some() == (args.action == Action::SaveAs),
        "--path is required only for save-as"
    );
    ensure!(
        !args.overwrite || args.action == Action::SaveAs,
        "--overwrite is only valid for save-as"
    );
    if let Some(text) = &args.text {
        validate_text(text)?;
    }
    if let Some(path) = &args.path {
        validate_relative_path(Path::new(path))?;
    }
    Ok(())
}

pub fn validate_text(text: &str) -> anyhow::Result<()> {
    ensure!(
        text.len() <= MAX_DOCUMENT_BYTES,
        "editor text exceeds 16 MiB"
    );
    ensure!(
        !text.contains('\0'),
        "editor text contains NUL and cannot round-trip as a text document"
    );
    Ok(())
}

/// Reject device names, device namespaces, UNC and alternate data streams on
/// every platform, so pure tests exercise the same Windows input policy.
pub fn validate_path(path: &Path) -> anyhow::Result<()> {
    let text = path.to_str().context("editor path is not Unicode")?;
    ensure!(
        !text.is_empty() && text.encode_utf16().count() <= MAX_PATH_UNITS,
        "editor path is empty or too long"
    );
    ensure!(
        !text.chars().any(char::is_control),
        "editor path contains a control character"
    );
    let windows = text.replace('/', "\\");
    ensure!(
        !windows.starts_with("\\\\") && !windows.starts_with("\\??\\"),
        "UNC and device paths are unsupported"
    );
    let drive =
        windows.as_bytes().get(1) == Some(&b':') && windows.as_bytes()[0].is_ascii_alphabetic();
    let tail = if drive {
        ensure!(
            windows.as_bytes().get(2) == Some(&b'\\'),
            "drive-relative editor paths are unsupported"
        );
        &windows[2..]
    } else {
        windows.as_str()
    };
    ensure!(
        !tail.contains(':')
            && !tail
                .chars()
                .any(|c| matches!(c, '<' | '>' | '"' | '|' | '?' | '*')),
        "editor path contains an alternate stream or invalid Windows character"
    );
    for component in tail.split('\\').filter(|part| !part.is_empty()) {
        if matches!(component, "." | "..") {
            continue;
        }
        ensure!(
            !component.ends_with(['.', ' ']),
            "editor path components cannot end in a dot or space"
        );
        let stem = component
            .split('.')
            .next()
            .unwrap()
            .trim_end_matches(' ')
            .to_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|n| {
                    matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            });
        ensure!(!reserved, "editor path uses a reserved Windows device name");
    }
    Ok(())
}

pub fn validate_relative_path(path: &Path) -> anyhow::Result<()> {
    validate_path(path)?;
    let text = path.to_str().unwrap();
    ensure!(
        !path.is_absolute() && !text.starts_with(['/', '\\']) && !text.contains(':'),
        "save path must be workspace-relative"
    );
    ensure!(
        !text.split(['/', '\\']).any(|part| part == ".."),
        "save path cannot traverse outside its workspace"
    );
    ensure!(
        !path.components().any(|part| matches!(
            part,
            Component::Prefix(_) | Component::RootDir | Component::ParentDir
        )),
        "invalid relative editor path"
    );
    Ok(())
}

// std::fs::canonicalize returns a verbatim prefix on Windows. Do not persist
// that device spelling or require callers to bypass the external path policy.
pub fn display_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    let path = {
        match path.to_str().and_then(|text| text.strip_prefix("\\\\?\\")) {
            Some(text) => PathBuf::from(text),
            None => path,
        }
    };
    path
}

fn canonical_display(path: PathBuf) -> anyhow::Result<PathBuf> {
    let path = display_path(path);
    validate_path(&path)?;
    ensure!(
        path.is_absolute(),
        "canonical editor path is not a local absolute path"
    );
    Ok(path)
}

/// Filesystem work: call from the document worker, never a WebView callback.
pub fn canonical_root(root: &Path) -> anyhow::Result<PathBuf> {
    validate_path(root)?;
    let root =
        canonical_display(std::fs::canonicalize(root).context("cannot resolve editor root")?)?;
    ensure!(root.is_dir(), "editor root is not a directory");
    Ok(root)
}

pub fn resolve_existing(root: &Path, path: &Path) -> anyhow::Result<PathBuf> {
    validate_path(path)?;
    let requested = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    let resolved = canonical_display(
        std::fs::canonicalize(&requested).context("cannot resolve editor file")?,
    )?;
    ensure!(
        resolved.starts_with(root),
        "editor file is outside its workspace"
    );
    ensure!(resolved.is_file(), "editor path is not a regular file");
    Ok(resolved)
}

pub fn validate_session(state: &EditorSessionState) -> anyhow::Result<()> {
    ensure!(
        state.open_files.len() <= MAX_DOCUMENTS,
        "editor session exceeds 128 documents"
    );
    ensure!(
        state
            .zoom_percent
            .is_none_or(|zoom| (EDITOR_ZOOM_MIN..=EDITOR_ZOOM_MAX).contains(&zoom)),
        "invalid editor zoom"
    );
    let mut paths = HashSet::new();
    for file in &state.open_files {
        validate_path(&file.path)?;
        ensure!(paths.insert(&file.path), "duplicate editor document path");
        ensure!(
            file.scroll_top.is_finite() && file.scroll_top >= 0.0,
            "invalid editor scroll position"
        );
    }
    if let Some(active) = &state.active_file {
        ensure!(paths.contains(active), "active editor file is not open");
    }
    Ok(())
}

pub fn session_state(snapshot: EditorSessionSnapshot, zoom: u16) -> EditorSessionState {
    EditorSessionState {
        open_files: snapshot
            .open_files
            .into_iter()
            .map(|file| EditorFileState {
                path: display_path(file.path),
                cursor_line: file.view.cursor_line,
                cursor_column: file.view.cursor_column,
                scroll_top: file.view.scroll_top,
            })
            .collect(),
        active_file: snapshot.active_file.map(display_path),
        zoom_percent: Some(zoom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_paths_reject_devices_streams_and_aliases_without_normalizing_unicode() {
        for path in [
            r"\\server\share\file",
            r"\\?\C:\file",
            r"\\.\pipe\thing",
            r"C:relative",
            r"C:\a.txt:secret",
            r"dir\CON.txt",
            "LPT¹.log",
            "bad. ",
            "bad\0name",
        ] {
            assert!(validate_path(Path::new(path)).is_err(), "{path:?}");
        }
        for path in [r"C:\한글\한 é 😀.txt", "dir/한글.txt", "/tmp/editor.txt"] {
            assert!(validate_path(Path::new(path)).is_ok(), "{path:?}");
        }
        assert!(validate_relative_path(Path::new(r"..\outside.txt")).is_err());
        assert!(validate_relative_path(Path::new(r"C:\outside.txt")).is_err());
        assert!(validate_relative_path(Path::new("한글/한 é.txt")).is_ok());
    }

    #[test]
    fn text_and_session_limits_are_explicit() {
        assert!(validate_text(&"한".repeat(MAX_DOCUMENT_BYTES / 3 + 1)).is_err());
        assert!(validate_text("한 é 😀").is_ok());
        assert!(validate_text("a\0b").is_err());
        let mut state = EditorSessionState {
            active_file: Some("missing.txt".into()),
            ..EditorSessionState::default()
        };
        assert!(validate_session(&state).is_err());
        state.active_file = None;
        state.open_files.push(EditorFileState {
            path: "file.txt".into(),
            cursor_line: 0,
            cursor_column: 0,
            scroll_top: f64::NAN,
        });
        assert!(validate_session(&state).is_err());
    }
}
