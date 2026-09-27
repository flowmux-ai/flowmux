// SPDX-License-Identifier: GPL-3.0-or-later
//! The trusted terminal bridge is separate from external browser content.
//! Sequence numbers describe xterm's completed parser writes, not JS delivery.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use uuid::Uuid;

pub const VERSION: u8 = 1;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
pub const OUTPUT_WINDOW_BYTES: usize = 256 * 1024;
pub const OUTPUT_CHUNK_BYTES: usize = 16 * 1024;
pub const TERMINAL_ORIGIN: &str = "http://flowmux-terminal.localhost";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u8,
    pub surface: Uuid,
    pub generation: Uuid,
    pub token: Uuid,
    pub message: ClientMessage,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    RetryCommandPrompt,
    Ready,
    SettingsApplied {
        revision: Uuid,
        terminal: crate::settings::TerminalSettings,
        background: String,
        foreground: String,
    },
    Restored,
    Input {
        data: String,
    },
    Pasted {
        request: Option<Uuid>,
        sequence: u64,
        outcome: crate::paste::Outcome,
    },
    Selected {
        request: Uuid,
        sequence: u64,
        result: crate::selection::Snapshot,
    },
    BinaryInput {
        data: String,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Ack {
        sequence: u64,
    },
    Focus,
    FocusDirection {
        direction: crate::command::FocusDirection,
    },
    TogglePaneZoom,
    Title {
        title: String,
    },
    Cwd {
        path: String,
    },
    Link {
        url: String,
    },
    Screen {
        request: Uuid,
        sequence: u64,
        text: String,
    },
    Found {
        request: Uuid,
        sequence: u64,
        result: serde_json::Value,
    },
    SearchResults {
        search: Uuid,
        sequence: u64,
        total: usize,
        hits: Vec<crate::output_search::Match>,
        error: Option<String>,
    },
    SearchOpened {
        request: Uuid,
        search: Uuid,
        sequence: u64,
        error: Option<String>,
        selection: String,
        line: u32,
        column: u16,
        selected: bool,
    },
    Snapshot {
        request: Uuid,
        sequence: u64,
        screen: crate::state::SavedScreen,
    },
    SnapshotError {
        request: Uuid,
        message: String,
    },
    Fault {
        message: String,
    },
    Diagnostic {
        event: serde_json::Value,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMessage {
    Visibility {
        visible: bool,
    },
    Selection {
        request: Uuid,
        after: u64,
        action: crate::selection::Action,
    },
    ShellStatus {
        error: Option<String>,
    },
    Settings {
        document: crate::settings::Document,
    },
    Output {
        sequence: u64,
        data: String,
    },
    ReadScreen {
        request: Uuid,
        after: u64,
    },
    Find {
        request: Uuid,
        after: u64,
        query: String,
        previous: bool,
        match_case: bool,
        regex: bool,
        close: bool,
        focus: bool,
    },
    OpenFind,
    SearchBuffer {
        search: Uuid,
        after: u64,
        query: String,
        match_case: bool,
        skip: usize,
        limit: usize,
    },
    CancelSearch {
        search: Uuid,
    },
    OpenSearchHit {
        request: Uuid,
        search: Uuid,
        after: u64,
        hit: u32,
        commit: bool,
    },
    Snapshot {
        request: Uuid,
        after: u64,
    },
    Restore {
        screen: crate::state::SavedScreen,
    },
    Focus,
    Paste {
        request: Uuid,
        after: u64,
        text: String,
    },
    PasteResult {
        error: Option<String>,
    },
    Exit {
        code: u32,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct Identity {
    pub version: u8,
    pub surface: Uuid,
    pub generation: Uuid,
    pub token: Uuid,
}

impl Identity {
    pub fn new(surface: Uuid) -> Self {
        Self {
            version: VERSION,
            surface,
            generation: Uuid::new_v4(),
            token: Uuid::new_v4(),
        }
    }

    pub fn decode(&self, origin: &str, body: &str) -> anyhow::Result<ClientMessage> {
        anyhow::ensure!(
            origin == TERMINAL_ORIGIN || origin.starts_with(&format!("{TERMINAL_ORIGIN}/")),
            "untrusted terminal origin"
        );
        anyhow::ensure!(
            body.len() <= MAX_MESSAGE_BYTES,
            "terminal message exceeds limit"
        );
        let envelope: Envelope = serde_json::from_str(body)?;
        anyhow::ensure!(
            envelope.version == VERSION
                && envelope.surface == self.surface
                && envelope.generation == self.generation
                && envelope.token == self.token,
            "stale or foreign terminal message"
        );
        if let ClientMessage::Resize { cols, rows } = envelope.message {
            anyhow::ensure!(
                (2..=1000).contains(&cols) && (1..=1000).contains(&rows),
                "invalid terminal size"
            );
        }
        Ok(envelope.message)
    }
}

/// Bounded in-flight output; the session reader must stop when credit is used.
#[derive(Debug, Default)]
pub struct OutputWindow {
    sent: u64,
    acknowledged: u64,
    bytes: usize,
    pending: VecDeque<(u64, usize)>,
}

impl OutputWindow {
    pub fn can_send(&self, bytes: usize) -> bool {
        bytes > 0 && bytes <= OUTPUT_CHUNK_BYTES && self.bytes + bytes <= OUTPUT_WINDOW_BYTES
    }

    pub fn sent(&mut self, bytes: usize) -> anyhow::Result<u64> {
        anyhow::ensure!(
            self.can_send(bytes),
            "output credit exhausted or invalid chunk"
        );
        self.sent = self
            .sent
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("output sequence exhausted"))?;
        self.pending.push_back((self.sent, bytes));
        self.bytes += bytes;
        Ok(self.sent)
    }

    pub fn acknowledge(&mut self, sequence: u64) -> anyhow::Result<()> {
        anyhow::ensure!(
            sequence >= self.acknowledged && sequence <= self.sent,
            "invalid output acknowledgement"
        );
        while self.pending.front().is_some_and(|(id, _)| *id <= sequence) {
            self.bytes -= self.pending.pop_front().unwrap().1;
        }
        self.acknowledged = sequence;
        Ok(())
    }

    pub fn barrier(&self) -> u64 {
        self.sent
    }
    pub fn acknowledged(&self) -> u64 {
        self.acknowledged
    }
    pub fn in_flight_bytes(&self) -> usize {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_surface_and_foreign_origin_cannot_write_to_a_terminal() {
        let id = Identity::new(Uuid::new_v4());
        let mut body = serde_json::to_value(&id).unwrap();
        body["message"] = serde_json::json!({"type":"input", "data":"한글\r"});
        assert!(id.decode(TERMINAL_ORIGIN, &body.to_string()).is_ok());
        assert!(id
            .decode("http://flowmux-terminal.localhost.evil/", &body.to_string())
            .is_err());
        body["generation"] = serde_json::json!(Uuid::new_v4());
        assert!(id.decode(TERMINAL_ORIGIN, &body.to_string()).is_err());
    }

    #[test]
    fn parser_ack_releases_only_output_already_sent() {
        let mut window = OutputWindow::default();
        for _ in 0..OUTPUT_WINDOW_BYTES / OUTPUT_CHUNK_BYTES {
            window.sent(OUTPUT_CHUNK_BYTES).unwrap();
        }
        assert!(!window.can_send(1));
        assert!(window.acknowledge(window.barrier() + 1).is_err());
        window.acknowledge(1).unwrap();
        assert!(window.can_send(OUTPUT_CHUNK_BYTES));
        window.acknowledge(1).unwrap();
        assert_eq!(
            window.in_flight_bytes(),
            OUTPUT_WINDOW_BYTES - OUTPUT_CHUNK_BYTES
        );
        window.acknowledge(window.barrier()).unwrap();
        assert_eq!(window.in_flight_bytes(), 0);
        assert!(window.acknowledge(0).is_err());
    }
}
