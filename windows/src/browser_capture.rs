// SPDX-License-Identifier: GPL-3.0-or-later
//! PNG viewport capture arguments and output bounds.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, clap::Args, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Args {
    #[arg(value_parser = crate::command::parse_id)]
    pub pane: Uuid,
    /// PNG destination, relative to the CLI working directory.
    pub path: PathBuf,
}

pub const MAX_BYTES: usize = 32 * 1024 * 1024;
pub fn dimensions(width: u32, height: u32) -> anyhow::Result<()> {
    anyhow::ensure!(
        width > 0
            && height > 0
            && width <= 8192
            && height <= 8192
            && u64::from(width) * u64::from(height) <= 8 * 1024 * 1024,
        "screenshot viewport must be nonempty, at most 8192 pixels per side and 8 Mi pixels"
    );
    Ok(())
}

/// Check the native encoder's header before saving. This is not a PNG decoder.
pub fn png_dimensions(bytes: &[u8]) -> anyhow::Result<(u32, u32)> {
    anyhow::ensure!(bytes.len() <= MAX_BYTES, "screenshot exceeds 32 MiB");
    anyhow::ensure!(
        bytes.len() >= 33
            && &bytes[..8] == b"\x89PNG\r\n\x1a\n"
            && bytes[8..12] == 13u32.to_be_bytes()
            && &bytes[12..16] == b"IHDR",
        "WebView2 returned an invalid PNG header"
    );
    let width = u32::from_be_bytes(bytes[16..20].try_into()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into()?);
    dimensions(width, height)?;
    Ok((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_truncated_headers_and_excessive_image_dimensions() {
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        header.extend(1024u32.to_be_bytes());
        header.extend(768u32.to_be_bytes());
        header.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(png_dimensions(&header).unwrap(), (1024, 768));
        for n in 0..header.len() {
            assert!(png_dimensions(&header[..n]).is_err());
        }
        assert!(dimensions(8192, 1024).is_ok());
        for (w, h) in [
            (0, 1),
            (1, 0),
            (8193, 1),
            (8192, 1025),
            (u32::MAX, u32::MAX),
        ] {
            assert!(dimensions(w, h).is_err());
        }
        header[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(png_dimensions(&header).is_err());
    }
    #[test]
    fn screenshot_cli_and_wire_preserve_unicode_path() {
        use crate::command::{Cli, Command};
        use clap::Parser;
        let pane = Uuid::new_v4();
        let cli = Cli::try_parse_from([
            "flowmux",
            "browser",
            "screenshot",
            &format!("pane:{pane}"),
            "한 글 한 😀.png",
        ])
        .unwrap();
        let Command::Browser {
            op: crate::browser::Op::Screenshot(args),
        } = cli.command
        else {
            panic!("wrong command")
        };
        assert_eq!(args.path, PathBuf::from("한 글 한 😀.png"));
        let wire = serde_json::to_value(crate::browser::Op::Screenshot(args)).unwrap();
        assert_eq!(wire["kind"], "screenshot");
        assert_eq!(wire["pane"], pane.to_string());
        assert_eq!(wire["path"], "한 글 한 😀.png");
    }
}
