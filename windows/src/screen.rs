// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded plain-text grid reads, distinct from styled history persistence.
use serde::{Deserialize, Serialize};

pub const MAX_TEXT_BYTES: usize = 128 * 1024;
pub const RECENT_ROWS: u16 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Viewport,
    Recent,
}

impl Mode {
    pub fn from_recent(recent: bool) -> Self {
        if recent {
            Self::Recent
        } else {
            Self::Viewport
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Buffer {
    Normal,
    Alternate,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cursor {
    pub row: u32,
    pub column: u16,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Metadata {
    pub mode: Mode,
    pub buffer: Buffer,
    pub cols: u16,
    pub rows: u16,
    pub buffer_rows: u32,
    pub first_row: u32,
    pub row_count: u16,
    pub viewport_row: u32,
    pub base_row: u32,
    pub cursor: Cursor,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub text: String,
    pub metadata: Metadata,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Ok { snapshot: Snapshot },
    Error { message: String },
}
impl Outcome {
    pub fn validate(&self, mode: Mode) -> anyhow::Result<()> {
        let Self::Ok { snapshot } = self else {
            let Self::Error { message } = self else {
                unreachable!()
            };
            anyhow::ensure!(message.len() <= 512, "invalid screen error");
            return Ok(());
        };
        let m = &snapshot.metadata;
        anyhow::ensure!(
            snapshot.text.len() <= MAX_TEXT_BYTES,
            "screen exceeds limit"
        );
        anyhow::ensure!(
            m.mode == mode && (2..=1000).contains(&m.cols) && (1..=1000).contains(&m.rows)
                && (u32::from(m.rows)..=101_000).contains(&m.buffer_rows)
                && m.base_row == m.buffer_rows - u32::from(m.rows)
                && m.viewport_row <= m.base_row
                && m.cursor.row >= m.base_row && m.cursor.row < m.buffer_rows
                // A pending autowrap cursor may sit just beyond the last cell.
                && m.cursor.column <= m.cols,
            "invalid screen geometry"
        );
        anyhow::ensure!(
            m.buffer != Buffer::Alternate || (m.base_row == 0 && m.viewport_row == 0),
            "invalid alternate screen geometry"
        );
        let count = if mode == Mode::Recent && m.buffer == Buffer::Normal {
            u32::from(RECENT_ROWS).min(m.buffer_rows) as u16
        } else {
            m.rows
        };
        let first = if mode == Mode::Recent {
            m.buffer_rows - u32::from(count)
        } else {
            m.viewport_row
        };
        anyhow::ensure!(
            m.first_row == first
                && m.row_count == count
                && snapshot.text.split('\n').count() == usize::from(count),
            "invalid screen row range"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample(mode: Mode, buffer: Buffer) -> Outcome {
        let (rows, buffer_rows, viewport_row) = if buffer == Buffer::Normal {
            (24, 200, 17)
        } else {
            (100, 100, 0)
        };
        let row_count = if mode == Mode::Recent && buffer == Buffer::Normal {
            80
        } else {
            rows
        };
        Outcome::Ok {
            snapshot: Snapshot {
                text: vec!["한글 한 😀"; usize::from(row_count)].join("\n"),
                metadata: Metadata {
                    mode,
                    buffer,
                    cols: 80,
                    rows,
                    buffer_rows,
                    first_row: if mode == Mode::Recent {
                        buffer_rows - u32::from(row_count)
                    } else {
                        viewport_row
                    },
                    row_count,
                    viewport_row,
                    base_row: buffer_rows - u32::from(rows),
                    cursor: Cursor {
                        row: buffer_rows - u32::from(rows),
                        column: 80,
                    },
                },
            },
        }
    }
    #[test]
    fn reads_validate_buffer_ranges_and_reject_wrong_mode_and_forged_geometry() {
        for mode in [Mode::Viewport, Mode::Recent] {
            for buffer in [Buffer::Normal, Buffer::Alternate] {
                sample(mode, buffer).validate(mode).unwrap();
            }
        }
        assert!(sample(Mode::Recent, Buffer::Normal)
            .validate(Mode::Viewport)
            .is_err());
        let mut outcome = sample(Mode::Recent, Buffer::Normal);
        let Outcome::Ok { snapshot } = &mut outcome else {
            unreachable!()
        };
        snapshot.metadata.first_row = u32::MAX;
        assert!(outcome.validate(Mode::Recent).is_err());
        let mut outcome = sample(Mode::Viewport, Buffer::Alternate);
        let Outcome::Ok { snapshot } = &mut outcome else {
            unreachable!()
        };
        snapshot.metadata.cursor.row = 100;
        assert!(outcome.validate(Mode::Viewport).is_err());
    }
    #[test]
    fn bounded_text_is_exact_and_even_json_escaping_fits_the_bridge_frame() {
        let mut outcome = sample(Mode::Viewport, Buffer::Normal);
        let Outcome::Ok { snapshot } = &mut outcome else {
            unreachable!()
        };
        snapshot.text = "\u{1}".repeat(MAX_TEXT_BYTES - 23) + &"\n".repeat(23);
        outcome.validate(Mode::Viewport).unwrap();
        let message = crate::protocol::Envelope {
            version: crate::protocol::VERSION,
            surface: uuid::Uuid::new_v4(),
            generation: uuid::Uuid::new_v4(),
            token: uuid::Uuid::new_v4(),
            message: crate::protocol::ClientMessage::Screen {
                request: uuid::Uuid::new_v4(),
                sequence: 1,
                outcome,
            },
        };
        assert!(serde_json::to_vec(&message).unwrap().len() < crate::protocol::MAX_MESSAGE_BYTES);
        let crate::protocol::ClientMessage::Screen { mut outcome, .. } = message.message else {
            unreachable!()
        };
        let Outcome::Ok { snapshot } = &mut outcome else {
            unreachable!()
        };
        snapshot.text.push('한');
        assert!(outcome.validate(Mode::Viewport).is_err());
        assert!(Outcome::Error {
            message: "e".repeat(513)
        }
        .validate(Mode::Recent)
        .is_err());
    }
}
