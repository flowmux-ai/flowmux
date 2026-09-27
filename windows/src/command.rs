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
}

#[derive(Debug, Clone, Subcommand, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Command {
    /// Check local Windows and WebView2 prerequisites without connecting to a window.
    Doctor,
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
        };
        let decoded: Request =
            serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(decoded.caller_surface, Some(id));
        assert!(matches!(
            decoded.command,
            Command::ReadScreen {
                pane: None,
                surface: None
            }
        ));
    }
}
