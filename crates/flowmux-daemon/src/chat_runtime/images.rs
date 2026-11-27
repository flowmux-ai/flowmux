// SPDX-License-Identifier: GPL-3.0-or-later

//! Images a tool returns, shown in the answer as Markdown images.
//!
//! Codex reports image files by path. Claude and ACP agents send base64 bytes, which are written
//! under the state directory so the history keeps a short path instead of the bytes.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::Result;
use async_channel::Sender;
use base64::Engine;
use sha2::{Digest, Sha256};

use super::{emit, Event, TextKind, Turn};

/// Base64 frames are already capped at `MAX_FRAME`; this bounds one decoded image.
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// Keeps a looping tool from filling the transcript and the disk.
const MAX_PER_TURN: usize = 20;

/// Adds the image at `path` to the answer as its own paragraph.
pub(super) async fn show(
    turn: &mut Turn,
    events: &Sender<Event>,
    id: String,
    alt: &str,
    path: &Path,
) -> Result<()> {
    if turn.images >= MAX_PER_TURN {
        return Ok(());
    }
    let Some(text) = markdown(alt, path) else {
        return Ok(());
    };
    turn.images += 1;
    emit(
        events,
        Event::Text {
            id,
            kind: TextKind::Answer,
            text,
            append: false,
        },
    )
    .await
}

/// Writes base64 `data` of a raster `mime` type to the state directory, once per content.
// ponytail: saved images are never pruned; tie them to chat deletion if the directory grows.
pub(super) fn save(data: &str, mime: &str) -> Option<PathBuf> {
    let extension = match mime.split(';').next()?.trim().to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        // SVG is script-capable markup, not a picture.
        _ => return None,
    };
    if data.len() / 4 * 3 > MAX_BYTES {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()
        .filter(|bytes| !bytes.is_empty())?;
    let directory = flowmux_config::paths::state_dir()?.join("chat-images");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .ok()?;
    let digest = Sha256::digest(&bytes);
    let name: String = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let path = directory.join(format!("{name}.{extension}"));
    if path.exists() {
        return Some(path);
    }
    let partial = directory.join(format!(".{name}.{}", uuid::Uuid::new_v4()));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&partial)
        .and_then(|mut file| file.write_all(&bytes))
        .and_then(|()| std::fs::rename(&partial, &path));
    if written.is_err() {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    Some(path)
}

/// `![alt](<path>)`, or `None` for a path Markdown cannot hold.
fn markdown(alt: &str, path: &Path) -> Option<String> {
    let path = path.to_str().filter(|path| !path.contains(['\n', '\r']))?;
    let mut text = String::from("![");
    for c in alt.chars().filter(|c| !c.is_control()).take(120) {
        if matches!(c, '\\' | '[' | ']') {
            text.push('\\');
        }
        text.push(c);
    }
    text.push_str("](<");
    for c in path.chars() {
        if matches!(c, '\\' | '<' | '>') {
            text.push('\\');
        }
        text.push(c);
    }
    text.push_str(">)");
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_keeps_paths_with_spaces_and_brackets() {
        assert_eq!(
            markdown("A [b]", Path::new("/tmp/shot (1) <x>.png")).as_deref(),
            Some(r"![A \[b\]](</tmp/shot (1) \<x\>.png>)")
        );
        assert_eq!(markdown("a", Path::new("/tmp/a\nb.png")), None);
    }

    #[test]
    fn only_raster_images_are_saved() {
        assert_eq!(save("PHN2Zz48L3N2Zz4=", "image/svg+xml"), None);
        assert_eq!(save("not base64!", "image/png"), None);
    }
}
