// SPDX-License-Identifier: GPL-3.0-or-later
//! Validate terminal-reported local Windows paths without filesystem/network IO.
use anyhow::ensure;
use std::path::PathBuf;

pub fn local_path(text: &str) -> anyhow::Result<PathBuf> {
    ensure!(
        text.encode_utf16().count() <= 32767,
        "cwd exceeds Windows path limit"
    );
    let bytes = text.as_bytes();
    ensure!(
        bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'),
        "cwd must be an absolute local drive path"
    );
    let tail = text[3..].replace('/', "\\");
    ensure!(
        !tail
            .chars()
            .any(|c| c.is_control() || "<>:\"|?*".contains(c)),
        "invalid cwd characters"
    );
    let mut parts = Vec::new();
    for part in tail.split('\\') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            _ => parts.push(part),
        }
    }
    Ok(PathBuf::from(format!(
        "{}:\\{}",
        (bytes[0] as char).to_ascii_uppercase(),
        parts.join("\\")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keeps_unicode_and_punctuation_without_expansion_or_remote_io() {
        assert_eq!(
            local_path("c:/작업 e\u{301}/with space/%#;'name/../한글").unwrap(),
            PathBuf::from("C:\\작업 e\u{301}\\with space\\한글")
        );
        assert_eq!(
            local_path("D:\\%TEMP%\\a;b#'c").unwrap(),
            PathBuf::from("D:\\%TEMP%\\a;b#'c")
        );
        for bad in [
            "C:relative",
            "folder",
            "/remote/home",
            "\\\\server\\share",
            "\\\\?\\C:\\folder",
            "C:\\folder\0x",
            "C:\\x:stream",
            "C:\\line\nbreak",
        ] {
            assert!(local_path(bad).is_err(), "accepted {bad:?}");
        }
    }
}
