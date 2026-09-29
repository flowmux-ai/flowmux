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
#[serde(rename_all = "snake_case")]
pub enum TerminalMenuAction {
    SplitRight,
    SplitDown,
    CopyPath,
    ClosePane,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub version: u8,
    pub surface: Uuid,
    pub generation: Uuid,
    pub token: Uuid,
    #[serde(default)]
    pub session: Option<Uuid>,
    pub message: ClientMessage,
}

impl Envelope {
    pub fn accepts_session(&self, current: Uuid) -> bool {
        !matches!(
            &self.message,
            ClientMessage::Input { .. }
                | ClientMessage::BinaryInput { .. }
                | ClientMessage::Pasted { request: None, .. }
                | ClientMessage::Title { .. }
                | ClientMessage::Cwd { .. }
                | ClientMessage::Shortcut { .. }
                | ClientMessage::TerminalMenuAction { .. }
                | ClientMessage::Focus
        ) || self.session == Some(current)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ClientMessage {
    KeyMode {
        request: Uuid,
        sequence: u64,
        outcome: crate::keys::ModeOutcome,
    },
    Minimap {
        request: Uuid,
        sequence: u64,
        outcome: crate::minimap::Outcome,
    },
    RetryCommandPrompt,
    SshConnect,
    SshReady {
        session: Uuid,
    },
    Ready,
    SettingsApplied {
        revision: Uuid,
        rendered_font_size: u16,
        terminal: Box<crate::settings::TerminalSettings>,
        bindings: Vec<crate::keybindings::Binding>,
        background: String,
        foreground: String,
        colors: Box<crate::theme::ResolvedTheme>,
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
    TerminalMenuAction {
        pane: Uuid,
        action: TerminalMenuAction,
    },
    #[cfg(debug_assertions)]
    TerminalMenuTested {
        request: Uuid,
        state: serde_json::Value,
    },
    Shortcut {
        action: String,
        chord: crate::keybindings::Chord,
        revision: Uuid,
    },
    #[cfg(debug_assertions)]
    ShortcutTested {
        request: Uuid,
        forwarded: bool,
    },
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
        outcome: crate::screen::Outcome,
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
    SessionStart {
        session: Uuid,
    },
    TerminalMenuState {
        pane: Uuid,
        split: bool,
        close: bool,
    },
    #[cfg(debug_assertions)]
    TestTerminalMenu {
        request: Uuid,
        event: serde_json::Value,
    },
    KeyMode {
        request: Uuid,
        after: u64,
    },
    Minimap {
        request: Uuid,
        after: u64,
        action: crate::minimap::Action,
    },
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
    SshStatus {
        state: String,
        error: Option<String>,
    },
    Settings {
        document: Box<crate::settings::Document>,
        bindings: Vec<crate::keybindings::Binding>,
        colors: Box<crate::theme::ResolvedTheme>,
    },
    Output {
        sequence: u64,
        data: String,
    },
    ReadScreen {
        request: Uuid,
        after: u64,
        recent: bool,
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
    OpenFind {
        focus: bool,
    },
    #[cfg(debug_assertions)]
    TestShortcut {
        request: Uuid,
        event: serde_json::Value,
    },
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

    pub fn decode(&self, origin: &str, body: &str) -> anyhow::Result<Envelope> {
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
        if let ClientMessage::Resize { cols, rows } = &envelope.message {
            anyhow::ensure!(
                (2..=1000).contains(cols) && (1..=1000).contains(rows),
                "invalid terminal size"
            );
        }
        Ok(envelope)
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
    /// Start a replacement session after output already owned by its renderer.
    /// The previous session's bytes are not charged to the new credit window.
    pub fn after(sequence: u64) -> Self {
        Self {
            sent: sequence,
            acknowledged: sequence,
            ..Self::default()
        }
    }

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
    fn replacement_session_rejects_stale_side_effects_without_rejecting_transport() {
        let identity = Identity::new(Uuid::new_v4());
        let current = Uuid::new_v4();
        let previous = Uuid::new_v4();
        let mut body = serde_json::to_value(&identity).unwrap();
        let effects = [
            serde_json::json!({"type":"input","data":"한글 한\r"}),
            serde_json::json!({"type":"binary_input","data":"\u{1b}"}),
            serde_json::json!({"type":"pasted","request":null,"sequence":7,
                "outcome":{"status":"ok","data":"한글","bracketed":false}}),
            serde_json::json!({"type":"title","title":"이전 제목"}),
            serde_json::json!({"type":"cwd","path":"/이전/한"}),
            serde_json::json!({"type":"shortcut","action":"new-tab","revision":current,
                "chord":{"code":"KeyT","ctrl":true,"alt":false,"shift":true}}),
            serde_json::json!({"type":"terminal_menu_action","pane":current,"action":"split_right"}),
            serde_json::json!({"type":"focus"}),
        ];
        for message in effects {
            body["message"] = message;
            body.as_object_mut().unwrap().remove("session");
            assert!(!identity
                .decode(TERMINAL_ORIGIN, &body.to_string())
                .unwrap()
                .accepts_session(current));
            for session in [None, Some(previous), Some(current)] {
                body["session"] = serde_json::to_value(session).unwrap();
                let envelope = identity.decode(TERMINAL_ORIGIN, &body.to_string()).unwrap();
                assert_eq!(envelope.accepts_session(current), session == Some(current));
            }
        }
        for message in [
            serde_json::json!({"type":"ack","sequence":7}),
            serde_json::json!({"type":"ready"}),
            serde_json::json!({"type":"restored"}),
            serde_json::json!({"type":"resize","cols":80,"rows":24}),
            serde_json::json!({"type":"key_mode","request":current,"sequence":7,
                "outcome":{"status":"ok","application_cursor":false}}),
            serde_json::json!({"type":"pasted","request":current,"sequence":7,
                "outcome":{"status":"ok","data":"한글","bracketed":false}}),
            serde_json::json!({"type":"screen","request":current,"sequence":7,
                "outcome":{"status":"error","message":"closed"}}),
        ] {
            body["message"] = message;
            for session in [None, Some(previous)] {
                body["session"] = serde_json::to_value(session).unwrap();
                assert!(identity
                    .decode(TERMINAL_ORIGIN, &body.to_string())
                    .unwrap()
                    .accepts_session(current));
            }
        }
        body.as_object_mut().unwrap().remove("session");
        assert!(identity
            .decode(TERMINAL_ORIGIN, &body.to_string())
            .unwrap()
            .accepts_session(current));
        body["message"] = serde_json::json!({"type":"resize","cols":1,"rows":24});
        assert!(identity.decode(TERMINAL_ORIGIN, &body.to_string()).is_err());
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

    #[test]
    fn replacement_output_continues_sequences_and_preserves_credit_at_the_limit() {
        let after = u64::MAX - 2;
        let mut window = OutputWindow::after(after);
        assert_eq!(window.barrier(), after);
        assert_eq!(window.acknowledged(), after);
        assert_eq!(window.in_flight_bytes(), 0);
        assert!(window.acknowledge(after - 1).is_err());
        assert!(window.acknowledge(after + 1).is_err());
        window.acknowledge(after).unwrap();
        assert_eq!(window.sent(OUTPUT_CHUNK_BYTES).unwrap(), after + 1);
        assert_eq!(window.sent(1).unwrap(), u64::MAX);
        window.acknowledge(after + 1).unwrap();
        assert_eq!(window.in_flight_bytes(), 1);
        assert!(window.sent(1).is_err());
        assert_eq!(window.barrier(), u64::MAX);
        assert_eq!(window.in_flight_bytes(), 1);
        window.acknowledge(u64::MAX).unwrap();
        assert_eq!(window.in_flight_bytes(), 0);
        window.acknowledge(u64::MAX).unwrap();
    }
}
