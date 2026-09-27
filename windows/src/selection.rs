// SPDX-License-Identifier: GPL-3.0-or-later
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, clap::Subcommand, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    /// Read the current or retained selection without accessing the clipboard.
    Read,
    /// Select the entire retained buffer without changing keyboard focus.
    All,
    /// Clear both the visible selection and its retained copy.
    Clear,
    /// Select cells in the active normal/alternate buffer, including scrollback.
    Range { row: u32, column: u16, length: u32 },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub text: String,
    pub source: Source,
    pub position: Option<Position>,
    pub buffer: Buffer,
    pub error: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    None,
    Live,
    Retained,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Buffer {
    Normal,
    Alternate,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub start: Point,
    pub end: Point,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

impl Snapshot {
    pub fn validate(&self) -> anyhow::Result<()> {
        crate::paste::validate(&self.text)?;
        anyhow::ensure!(
            self.error.as_ref().is_none_or(|e| e.len() <= 512),
            "invalid selection error"
        );
        Ok(())
    }
}
