// SPDX-License-Identifier: GPL-3.0-or-later
//! Terminal input mode tracking shared by terminal compatibility glue and
//! the terminal backend compatibility layer.

use std::borrow::Cow;

#[derive(Debug, Default, Clone)]
pub struct TerminalInputModes {
    application_cursor: bool,
    alternate_screen: bool,
    alternate_screen_exited: bool,
    output_escape: Vec<u8>,
}

impl TerminalInputModes {
    pub fn application_cursor(&self) -> bool {
        self.application_cursor
    }

    pub fn alternate_screen(&self) -> bool {
        self.alternate_screen
    }

    pub fn take_alternate_screen_exit(&mut self) -> bool {
        std::mem::take(&mut self.alternate_screen_exited)
    }

    /// Observe bytes emitted by the terminal application and update the
    /// input modes those bytes select. This tracks DECCKM for cursor input and
    /// alternate-screen exit so the caller can restore shell-owned UI state.
    pub fn observe_output(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.output_escape.is_empty() {
                if byte == 0x1b {
                    self.output_escape.push(byte);
                }
                continue;
            }

            self.output_escape.push(byte);
            if self.output_escape.len() > 32 {
                self.output_escape.clear();
                continue;
            }

            if self.output_escape == b"\x1b=" || self.output_escape == b"\x1b>" {
                self.output_escape.clear();
                continue;
            }

            if self.output_escape == b"\x1bc" {
                self.application_cursor = false;
                self.alternate_screen_exited |= self.alternate_screen;
                self.alternate_screen = false;
                self.output_escape.clear();
                continue;
            }

            if self.output_escape.len() > 2
                && self.output_escape.starts_with(b"\x1b[")
                && (0x40..=0x7e).contains(&byte)
            {
                self.apply_csi();
                self.output_escape.clear();
            } else if !self.output_escape.starts_with(b"\x1b[") && self.output_escape.len() >= 2 {
                self.output_escape.clear();
            }
        }
    }

    /// Rewrite normal cursor-key bytes into application-cursor bytes when
    /// the foreground program has enabled DECCKM. This is the same mode
    /// switch represented by xterm terminfo `smkx`/`rmkx`.
    pub fn rewrite_input<'a>(&self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if !self.application_cursor {
            return Cow::Borrowed(bytes);
        }

        let mut out = Vec::with_capacity(bytes.len());
        let mut changed = false;
        let mut i = 0;
        while i < bytes.len() {
            if i + 2 < bytes.len() && bytes[i] == 0x1b && bytes[i + 1] == b'[' {
                if let Some(final_byte) = app_cursor_final(bytes[i + 2]) {
                    out.extend_from_slice(&[0x1b, b'O', final_byte]);
                    changed = true;
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }

        if changed {
            Cow::Owned(out)
        } else {
            Cow::Borrowed(bytes)
        }
    }

    fn apply_csi(&mut self) {
        let Some(final_byte) = self.output_escape.last().copied() else {
            return;
        };
        if final_byte != b'h' && final_byte != b'l' {
            return;
        }
        let params = &self.output_escape[2..self.output_escape.len() - 1];
        let Some(private_params) = params.strip_prefix(b"?") else {
            return;
        };
        let has_decckm = private_params
            .split(|b| *b == b';')
            .any(|param| param == b"1");
        if has_decckm {
            self.application_cursor = final_byte == b'h';
        }
        let has_alternate_screen = private_params
            .split(|b| *b == b';')
            .any(|param| matches!(param, b"47" | b"1047" | b"1049"));
        if has_alternate_screen {
            let enabled = final_byte == b'h';
            self.alternate_screen_exited |= self.alternate_screen && !enabled;
            self.alternate_screen = enabled;
        }
    }
}

fn app_cursor_final(final_byte: u8) -> Option<u8> {
    match final_byte {
        b'A' | b'B' | b'C' | b'D' | b'H' | b'F' => Some(final_byte),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cursor_keys_stay_in_normal_mode() {
        let modes = TerminalInputModes::default();

        assert_eq!(modes.rewrite_input(b"\x1b[A").as_ref(), b"\x1b[A");
        assert_eq!(modes.rewrite_input(b"\x1b[B").as_ref(), b"\x1b[B");
        assert!(!modes.application_cursor());
    }

    #[test]
    fn smkx_application_cursor_mode_rewrites_tig_arrow_keys() {
        let mut modes = TerminalInputModes::default();
        modes.observe_output(b"\x1b[?1h\x1b=");

        assert!(modes.application_cursor());
        assert_eq!(modes.rewrite_input(b"\x1b[A").as_ref(), b"\x1bOA");
        assert_eq!(modes.rewrite_input(b"\x1b[B").as_ref(), b"\x1bOB");
        assert_eq!(modes.rewrite_input(b"\x1b[C").as_ref(), b"\x1bOC");
        assert_eq!(modes.rewrite_input(b"\x1b[D").as_ref(), b"\x1bOD");
    }

    #[test]
    fn rmkx_restores_normal_cursor_mode() {
        let mut modes = TerminalInputModes::default();
        modes.observe_output(b"\x1b[?1h\x1b=");
        modes.observe_output(b"\x1b[?1l\x1b>");

        assert!(!modes.application_cursor());
        assert_eq!(modes.rewrite_input(b"\x1b[A").as_ref(), b"\x1b[A");
        assert_eq!(modes.rewrite_input(b"\x1b[B").as_ref(), b"\x1b[B");
    }

    #[test]
    fn decckm_tracking_survives_split_output_chunks() {
        let mut modes = TerminalInputModes::default();
        modes.observe_output(b"\x1b[?");
        modes.observe_output(b"1h");

        assert!(modes.application_cursor());
        assert_eq!(modes.rewrite_input(b"\x1b[A").as_ref(), b"\x1bOA");
    }

    #[test]
    fn non_cursor_input_is_preserved_in_application_cursor_mode() {
        let mut modes = TerminalInputModes::default();
        modes.observe_output(b"\x1b[?1h");

        assert_eq!(modes.rewrite_input(b"abc\r\x1b").as_ref(), b"abc\r\x1b");
        assert_eq!(modes.rewrite_input(b"\x1b[3~").as_ref(), b"\x1b[3~");
        assert_eq!(modes.rewrite_input(b"\x1bOP").as_ref(), b"\x1bOP");
    }

    #[test]
    fn private_csi_list_updates_when_decckm_is_present() {
        let mut modes = TerminalInputModes::default();
        modes.observe_output(b"\x1b[?7;1h");

        assert!(modes.application_cursor());
    }

    #[test]
    fn alternate_screen_exit_is_reported_once_across_output_chunks() {
        let mut modes = TerminalInputModes::default();
        modes.observe_output(b"\x1b[?10");
        modes.observe_output(b"49h");
        assert!(modes.alternate_screen());
        assert!(!modes.take_alternate_screen_exit());

        modes.observe_output(b"\x1b[?1049l");
        assert!(!modes.alternate_screen());
        assert!(modes.take_alternate_screen_exit());
        assert!(!modes.take_alternate_screen_exit());
    }

    #[test]
    fn full_reset_restores_initial_modes_and_reports_alternate_screen_exit() {
        let mut modes = TerminalInputModes::default();
        modes.observe_output(b"\x1b[?1h\x1b[?1049h");

        modes.observe_output(b"\x1b");
        modes.observe_output(b"c");

        assert!(!modes.application_cursor());
        assert!(!modes.alternate_screen());
        assert!(modes.take_alternate_screen_exit());
    }
}
