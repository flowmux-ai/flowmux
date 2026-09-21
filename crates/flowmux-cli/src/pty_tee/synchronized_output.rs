// SPDX-License-Identifier: GPL-3.0-or-later
//! Experimental bounded coalescing for terminals without DEC mode 2026.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const BEGIN: &[u8] = b"\x1b[?2026h";
const END: &[u8] = b"\x1b[?2026l";
const LIMIT: usize = 256 * 1024;
const TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct SynchronizedOutput {
    prefix: Vec<u8>,
    frame: Vec<u8>,
    active: bool,
    deadline: Option<Instant>,
}

impl SynchronizedOutput {
    pub(super) fn feed(&mut self, bytes: &[u8], output: &mut VecDeque<u8>) {
        for &byte in bytes {
            self.prefix.push(byte);
            while !self.prefix.is_empty()
                && !BEGIN.starts_with(&self.prefix)
                && !END.starts_with(&self.prefix)
            {
                let byte = self.prefix.remove(0);
                if self.active {
                    self.frame.push(byte);
                } else {
                    output.push_back(byte);
                }
            }
            if self.prefix == BEGIN {
                self.active = true;
                self.frame.append(&mut self.prefix);
            } else if self.prefix == END {
                self.flush(output);
            }
            if self.active || !self.prefix.is_empty() {
                self.deadline
                    .get_or_insert_with(|| Instant::now() + TIMEOUT);
            } else {
                self.deadline = None;
            }
            if self.frame.len() >= LIMIT {
                self.flush(output);
            }
        }
    }

    pub(super) fn timeout(&self) -> i32 {
        self.deadline.map_or(-1, |deadline| {
            deadline
                .saturating_duration_since(Instant::now())
                .as_millis() as i32
        })
    }

    pub(super) fn flush(&mut self, output: &mut VecDeque<u8>) {
        output.extend(self.frame.drain(..));
        output.extend(self.prefix.drain(..));
        self.active = false;
        self.deadline = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_frames_are_lossless_and_bounded() {
        let mut sync = SynchronizedOutput::default();
        let mut output = VecDeque::new();
        for byte in BEGIN {
            sync.feed(&[*byte], &mut output);
        }
        sync.feed(b"frame", &mut output);
        assert!(output.is_empty());
        for byte in END {
            sync.feed(&[*byte], &mut output);
        }
        assert_eq!(output.make_contiguous(), [BEGIN, b"frame", END].concat());
        output.clear();
        sync.feed(BEGIN, &mut output);
        sync.feed(&vec![b'x'; LIMIT], &mut output);
        assert!(!output.is_empty());
        sync.flush(&mut output);
        assert_eq!(output.len(), BEGIN.len() + LIMIT);
    }
}
