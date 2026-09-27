// SPDX-License-Identifier: GPL-3.0-or-later
use clap::{Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "flowmux", about = "Open a native Windows flowmux window")]
pub struct Launch {
    #[arg(long, conflicts_with = "restore_window")]
    pub cwd: Option<PathBuf>,
    #[arg(long, conflicts_with = "restore_window")]
    pub new_window: bool,
    /// Open without reading or saving persistent window state.
    #[arg(long, conflicts_with = "restore_window")]
    pub temporary: bool,
    #[arg(long)]
    pub restore_window: Option<Uuid>,
}

#[derive(Debug, Parser)]
#[command(
    name = "flowmuxctl",
    version,
    about = "Control a native Windows flowmux window"
)]
pub struct Cli {
    #[arg(long, global = true)]
    pub pipe: Option<String>,
    #[arg(long, global = true)]
    pub json: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    #[serde(flatten)]
    pub command: Command,
    /// The surface remains stable when its inherited pane/workspace IDs become stale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_surface: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller_cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Command {
    /// Check local Windows and WebView2 prerequisites without connecting to a window.
    Doctor,
    /// Print the session-local PowerShell prompt integration for manual reinstallation.
    ShellIntegration,
    Identify,
    Capabilities,
    Tree,
    ReadScreen {
        #[arg(value_parser = parse_id)]
        pane: Option<Uuid>,
        /// Read a specific tab, including an inactive tab, without changing focus.
        #[arg(long, value_parser = parse_id, conflicts_with = "pane")]
        surface: Option<Uuid>,
    },
    /// Find in a terminal's retained output without activating an inactive tab.
    Find {
        #[arg(required_unless_present = "close", conflicts_with = "close")]
        query: Option<String>,
        #[arg(long, value_parser = parse_id)]
        surface: Option<Uuid>,
        #[arg(long, conflicts_with = "close")]
        #[serde(default)]
        previous: bool,
        #[arg(long, conflicts_with = "close")]
        #[serde(default)]
        match_case: bool,
        #[arg(long, conflicts_with = "close")]
        #[serde(default)]
        regex: bool,
        #[arg(long)]
        #[serde(default)]
        close: bool,
    },
    /// Start a paged literal search across all terminals in this window.
    SearchAll {
        query: String,
        #[arg(long)]
        #[serde(default)]
        match_case: bool,
        #[arg(long, default_value_t = 0)]
        #[serde(default)]
        offset: usize,
    },
    /// Poll a search, including its matching lines and unavailable terminals.
    SearchResults {
        search: Uuid,
    },
    /// Cancel a search and invalidate its retained result references.
    SearchCancel {
        search: Uuid,
    },
    /// Select a result from the current page after verifying its retained text.
    SearchOpen {
        search: Uuid,
        index: usize,
    },
    SendKeys {
        #[arg(value_parser = parse_id)]
        pane: Uuid,
        text: String,
    },
    SendKey {
        key: String,
        #[arg(long, value_parser = parse_id)]
        pane: Option<Uuid>,
    },
    Split {
        #[arg(value_enum, default_value = "vertical")]
        direction: Direction,
    },
    NewTab,
    NewWorkspace {
        #[arg(long)]
        cwd: Option<PathBuf>,
    },
    FocusPane {
        #[arg(value_parser = parse_id)]
        pane: Uuid,
    },
    /// Resize a split or the immediate parent of a pane (ratio of the first child).
    ResizePane {
        #[arg(value_parser = parse_id)]
        pane: Uuid,
        #[arg(long, allow_hyphen_values = true)]
        ratio: f32,
    },
    /// Focus the nearest pane in a direction within the source workspace.
    FocusDirection {
        #[arg(value_enum)]
        direction: FocusDirection,
        #[arg(long, value_parser = parse_id)]
        pane: Option<Uuid>,
    },
    /// Maximize a pane, or restore its existing split layout.
    TogglePaneZoom {
        #[arg(value_parser = parse_id)]
        pane: Option<Uuid>,
    },
    FocusTab {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
    },
    CloseTab {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
    },
    /// Move a running tab without restarting its process or terminal view.
    MoveTab {
        #[arg(value_parser = parse_id)]
        surface: Uuid,
        #[arg(long, value_parser = parse_id)]
        to_pane: Uuid,
        /// Zero-based destination position; omitted means append.
        #[arg(long)]
        index: Option<usize>,
    },
    /// Close this Windows window and terminate its terminal process trees.
    Quit {
        /// Close even when saving fails; keep the last completed checkpoint.
        #[arg(long)]
        #[serde(default)]
        discard_state: bool,
    },
    /// Save layout and styled terminal history without closing the window.
    SaveState,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Vertical,
    Horizontal,
}

#[derive(Debug, Clone, Copy, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

fn parse_id(text: &str) -> Result<Uuid, String> {
    let value = text
        .strip_prefix("pane:")
        .or_else(|| text.strip_prefix("surface:"))
        .unwrap_or(text);
    Uuid::parse_str(value).map_err(|e| e.to_string())
}

pub fn key_bytes(key: &str) -> anyhow::Result<Vec<u8>> {
    let bytes: &[u8] = match key.to_ascii_lowercase().as_str() {
        "enter" => b"\r",
        "tab" => b"\t",
        "escape" | "esc" => b"\x1b",
        "backspace" => b"\x7f",
        "arrowup" | "up" => b"\x1b[A",
        "arrowdown" | "down" => b"\x1b[B",
        "arrowright" | "right" => b"\x1b[C",
        "arrowleft" | "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "delete" => b"\x1b[3~",
        "shiftenter" | "shift+enter" => b"\x1b\r",
        "ctrl+c" => b"\x03",
        "ctrl+d" => b"\x04",
        "ctrl+l" => b"\x0c",
        _ => anyhow::bail!("unsupported named key: {key}"),
    };
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_preserves_old_ipc_commands_and_roundtrips_stable_surface() {
        let old: Request = serde_json::from_str(r#"{"method":"identify"}"#).unwrap();
        assert!(old.caller_surface.is_none());
        assert!(old.caller_cwd.is_none());
        let old_quit: Request = serde_json::from_str(r#"{"method":"quit"}"#).unwrap();
        assert!(matches!(
            old_quit.command,
            Command::Quit {
                discard_state: false
            }
        ));
        let old_read: Request =
            serde_json::from_str(r#"{"method":"read_screen","pane":null}"#).unwrap();
        assert!(matches!(
            old_read.command,
            Command::ReadScreen {
                pane: None,
                surface: None
            }
        ));
        let id = Uuid::new_v4();
        let original = Request {
            command: Command::ReadScreen {
                pane: None,
                surface: None,
            },
            caller_surface: Some(id),
            caller_cwd: Some("C:\\한글 folder".into()),
        };
        let decoded: Request =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(decoded.caller_surface, Some(id));
        assert_eq!(decoded.caller_cwd, original.caller_cwd);
        assert!(matches!(
            decoded.command,
            Command::ReadScreen {
                pane: None,
                surface: None
            }
        ));
    }
}
