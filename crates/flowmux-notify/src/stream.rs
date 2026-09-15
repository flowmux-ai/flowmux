// SPDX-License-Identifier: GPL-3.0-or-later
//! Extract OSC payloads from a byte stream, across arbitrary read boundaries.
//! `ESC ]` starts a sequence; BEL or `ESC \` ends it. Non-OSC bytes are ignored.
//!
//! Used by the PTY-side notification sniffer and `flowmux notify-stream`.

/// Maximum buffered OSC payload. The cap prevents an unterminated sequence
/// from consuming unbounded memory.
pub const MAX_OSC_PAYLOAD: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Outside any escape sequence.
    Ground,
    /// Last byte was ESC (0x1B).
    Esc,
    /// Inside ESC `]` payload.
    Osc,
    /// Inside ESC `]` payload after seeing ESC (looking for `\` to terminate).
    OscEsc,
    /// OSC payload exceeded [`MAX_OSC_PAYLOAD`]; we silently consume bytes
    /// until the sequence terminator (BEL or ST) and drop the partial
    /// payload. Without this, a misbehaving peer could grow `buf` until
    /// the host runs out of memory.
    OscOverflow,
    /// In overflow recovery and the previous byte was ESC; if the next byte
    /// is `\\` this is an ST terminator and the sequence ends.
    OscOverflowEsc,
}

pub struct OscExtractor<F: FnMut(&str)> {
    state: State,
    buf: Vec<u8>,
    on_osc: F,
    max_payload: usize,
}

impl<F: FnMut(&str)> OscExtractor<F> {
    pub fn new(on_osc: F) -> Self {
        Self::with_capacity(on_osc, MAX_OSC_PAYLOAD)
    }

    /// Construct an extractor with a custom payload cap.
    pub fn with_capacity(on_osc: F, max_payload: usize) -> Self {
        Self {
            state: State::Ground,
            buf: Vec::with_capacity(64),
            on_osc,
            max_payload,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.step(b);
        }
    }

    fn emit(&mut self) {
        if let Ok(s) = std::str::from_utf8(&self.buf) {
            (self.on_osc)(s);
        }
        self.buf.clear();
    }

    fn push_or_overflow(&mut self, b: u8) {
        if self.buf.len() >= self.max_payload {
            self.buf.clear();
            self.state = State::OscOverflow;
            return;
        }
        self.buf.push(b);
    }

    fn step(&mut self, b: u8) {
        match self.state {
            State::Ground => {
                if b == 0x1B {
                    self.state = State::Esc;
                }
            }
            State::Esc => {
                if b == b']' {
                    self.state = State::Osc;
                    self.buf.clear();
                } else {
                    // Any other CSI / sequence — ignore for our purposes.
                    self.state = State::Ground;
                }
            }
            State::Osc => match b {
                0x07 /* BEL */ => {
                    self.emit();
                    self.state = State::Ground;
                }
                0x1B /* ESC */ => {
                    self.state = State::OscEsc;
                }
                _ => self.push_or_overflow(b),
            },
            State::OscEsc => {
                if b == b'\\' {
                    // ST terminator (ESC \\)
                    self.emit();
                    self.state = State::Ground;
                } else if b == b']' {
                    // A nested opener discards the unfinished payload. Otherwise a later
                    // OSC (for example a color query) could be spliced into a notification.
                    self.buf.clear();
                    self.state = State::Osc;
                } else {
                    // Spurious ESC followed by an unrelated byte —
                    // treat both as payload data (some agents log
                    // literal ESC X mid-body).
                    self.push_or_overflow(0x1B);
                    self.push_or_overflow(b);
                    if self.state != State::OscOverflow {
                        self.state = State::Osc;
                    }
                }
            }
            State::OscOverflow => match b {
                0x07 /* BEL */ => {
                    // Sequence terminated; drop the oversized payload and
                    // resume normal parsing.
                    self.state = State::Ground;
                }
                0x1B /* ESC */ => {
                    // Watch for the second byte of the ST terminator while discarding overflow.
                    self.state = State::OscOverflowEsc;
                }
                _ => {}
            },
            State::OscOverflowEsc => {
                if b == b'\\' {
                    self.state = State::Ground;
                } else {
                    self.state = State::OscOverflow;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(bytes: &[u8]) -> Vec<String> {
        let mut out = vec![];
        let mut x = OscExtractor::new(|s| out.push(s.to_string()));
        x.feed(bytes);
        out
    }

    #[test]
    fn extracts_bel_terminated() {
        let s = b"hello\x1b]9;done\x07world";
        assert_eq!(collect(s), vec!["9;done"]);
    }

    #[test]
    fn extracts_st_terminated() {
        let s = b"\x1b]777;notify;Codex;ready\x1b\\";
        assert_eq!(collect(s), vec!["777;notify;Codex;ready"]);
    }

    #[test]
    fn handles_split_chunks() {
        let mut out = vec![];
        let mut x = OscExtractor::new(|s| out.push(s.to_string()));
        x.feed(b"\x1b]9");
        x.feed(b";split message");
        x.feed(b"\x07tail");
        assert_eq!(out, vec!["9;split message"]);
    }

    #[test]
    fn passes_through_non_osc_escapes() {
        // A CSI sequence (\x1b[31m) should not produce OSC events.
        assert!(collect(b"\x1b[31mred\x1b[0m").is_empty());
    }

    #[test]
    fn back_to_back_osc_sequences_emit_in_order() {
        let s = b"\x1b]9;a\x07\x1b]9;b\x07";
        assert_eq!(collect(s), vec!["9;a", "9;b"]);
    }

    #[test]
    fn st_terminator_inside_payload_takes_precedence_over_loose_esc() {
        // ESC followed by an unrelated byte remains payload; ESC \ terminates it.
        let s = b"\x1b]9;part1\x1bXpart2\x1b\\";
        assert_eq!(collect(s), vec!["9;part1\x1bXpart2"]);
    }

    #[test]
    fn multibyte_unicode_payload_round_trips() {
        // utf-8 emoji + Korean + Chinese.
        let s = "\x1b]9;클로드 完了 ✅\x07".as_bytes();
        assert_eq!(collect(s), vec!["9;클로드 完了 ✅"]);
    }

    #[test]
    fn drops_unterminated_osc_at_end_of_stream() {
        // Provokes the error path: the child never wrote BEL/ST. The
        // extractor must not emit a partial event.
        let s = b"\x1b]9;never finished";
        assert!(collect(s).is_empty());
    }

    #[test]
    fn oversized_payload_is_dropped_without_unbounded_growth() {
        // Drive the extractor with an OSC payload far longer than the cap.
        // We must (a) not emit the partial payload, (b) recover cleanly so
        // the next properly-sized OSC parses, (c) not retain any byte after
        // recovery completes.
        let mut out = vec![];
        let final_buf_len;
        {
            let mut x = OscExtractor::with_capacity(|s| out.push(s.to_string()), 16);
            x.feed(b"\x1b]9;");
            // 1 KiB of payload, no terminator.
            let payload = vec![b'A'; 1024];
            x.feed(&payload);
            // Now terminate the sequence: the partial payload must be dropped.
            x.feed(b"\x07");
            // A normal sequence afterward must still parse.
            x.feed(b"\x1b]9;ok\x07");
            final_buf_len = x.buf.len();
        }
        assert_eq!(out, vec!["9;ok".to_string()]);
        assert!(final_buf_len <= 16);
    }

    #[test]
    fn oversized_payload_recovers_via_st_terminator() {
        let mut out = vec![];
        {
            let mut x = OscExtractor::with_capacity(|s| out.push(s.to_string()), 32);
            x.feed(b"\x1b]9;");
            // Far above the cap.
            let payload = vec![b'B'; 256];
            x.feed(&payload);
            // ST terminator instead of BEL.
            x.feed(b"\x1b\\");
            // A small follow-up sequence that fits within the cap.
            x.feed(b"\x1b]9;recovered\x07");
        }
        assert_eq!(out, vec!["9;recovered".to_string()]);
    }

    #[test]
    fn back_to_back_overflows_recover_independently() {
        let mut out = vec![];
        {
            let mut x = OscExtractor::with_capacity(|s| out.push(s.to_string()), 32);
            // Two oversized OSC sequences, each properly terminated.
            for _ in 0..2 {
                x.feed(b"\x1b]9;");
                x.feed(&vec![b'X'; 256]);
                x.feed(b"\x07");
            }
            // Then a small one that should still parse (well under 32 bytes).
            x.feed(b"\x1b]9;tiny\x07");
        }
        assert_eq!(out, vec!["9;tiny".to_string()]);
    }

    #[test]
    fn esc_followed_by_non_open_bracket_returns_to_ground() {
        // The stream contains an ESC that is not part of any OSC
        // (e.g. a CSI). The extractor must reset cleanly so a later
        // proper OSC still parses.
        let s = b"\x1b[2J\x1b]9;ok\x07";
        assert_eq!(collect(s), vec!["9;ok"]);
    }

    /// A nested opener must discard the first payload so a subsequent color OSC
    /// cannot become part of an OSC 9 notification.
    #[test]
    fn nested_open_bracket_aborts_previous_payload_and_starts_new() {
        let s = b"\x1b]9;\x1b]4;0;rgb:11/22/33\x07";
        assert_eq!(collect(s), vec!["4;0;rgb:11/22/33"]);
    }

    /// Same idea but the abandoning sequence carried partial body
    /// bytes before the restart — those must also be discarded.
    #[test]
    fn nested_open_bracket_discards_partial_body_of_previous_osc() {
        let s = b"\x1b]9;leftover-bytes\x1b]9;clean\x07";
        assert_eq!(collect(s), vec!["9;clean"]);
    }
}
