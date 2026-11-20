// SPDX-License-Identifier: GPL-3.0-or-later
//! Constant-space accounting before buffered JSON deserialization.

/// Counts JSON delimiters outside strings across chunks. Syntax remains the
/// deserializer's responsibility; callers must stop after `check` returns false.
pub struct JsonStructureBudget {
    remaining: usize,
    quoted: bool,
    escaped: bool,
}

impl JsonStructureBudget {
    pub fn new(limit: usize) -> Self {
        Self {
            remaining: limit,
            quoted: false,
            escaped: false,
        }
    }

    pub fn check(&mut self, bytes: &[u8]) -> bool {
        for &byte in bytes {
            if self.quoted {
                if self.escaped {
                    self.escaped = false;
                } else if byte == b'\\' {
                    self.escaped = true;
                } else if byte == b'"' {
                    self.quoted = false;
                }
            } else if byte == b'"' {
                self.quoted = true;
            } else if matches!(byte, b'{' | b'}' | b'[' | b']' | b',' | b':') {
                if self.remaining == 0 {
                    return false;
                }
                self.remaining -= 1;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_escapes_and_delimiters_are_counted_across_every_chunk_boundary() {
        let json = br#"{"text":"\\\"{,}","items":[0,1]}"#;
        for split in 0..=json.len() {
            let mut budget = JsonStructureBudget::new(8);
            assert!(budget.check(&json[..split]), "{split}");
            assert!(budget.check(&json[split..]), "{split}");
            assert!(!budget.check(b","), "{split}");
        }
        assert!(!JsonStructureBudget::new(7).check(json));
    }
}
