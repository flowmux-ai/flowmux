// SPDX-License-Identifier: GPL-3.0-or-later

//! Storage for pasted images and images a tool returns.
//!
//! Codex reports image files by path. Claude and ACP agents send base64 bytes, which are written
//! under the state directory so the history keeps a short path instead of the bytes.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
use async_channel::Sender;
use base64::Engine;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

use super::{emit, Event, TextKind, Turn};

/// Base64 frames are already capped at `MAX_FRAME`; this bounds one decoded image.
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// Keeps a looping tool from filling the transcript and the disk.
const MAX_PER_TURN: usize = 20;
pub const MAX_PASTED_IMAGES: usize = 4;
// Leave space for base64 and other prompt fields in an 8 MiB provider frame.
const MAX_PASTED_BYTES: u64 = 5 * 1024 * 1024;

pub fn save_pasted_image(png: &[u8]) -> Result<String> {
    ensure!(
        png.starts_with(b"\x89PNG\r\n\x1a\n"),
        "The clipboard image could not be encoded."
    );
    ensure!(
        png.len() as u64 <= MAX_PASTED_BYTES,
        "A pasted image must be at most 5 MiB."
    );
    let path = store(png, "png").context("Could not save the clipboard image.")?;
    Ok(path.file_stem().unwrap().to_string_lossy().into_owned())
}

pub fn pasted_image_path(id: &str) -> Option<PathBuf> {
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(())?;
    Some(
        flowmux_config::paths::state_dir()?
            .join("chat-images")
            .join(format!("{id}.png")),
    )
}

pub(super) fn input_paths(ids: &[String], supported: bool) -> Result<Vec<PathBuf>> {
    ensure!(
        supported || ids.is_empty(),
        "This agent does not accept image input."
    );
    ensure!(
        ids.len() <= MAX_PASTED_IMAGES,
        "Attach at most four images."
    );
    let mut total = 0;
    ids.iter()
        .map(|id| {
            let path = pasted_image_path(id).context("Invalid image attachment.")?;
            let info = path
                .symlink_metadata()
                .context("An attached image is unavailable.")?;
            ensure!(info.is_file(), "An attached image is unavailable.");
            total += info.len();
            ensure!(
                total <= MAX_PASTED_BYTES,
                "Attached images must total at most 5 MiB."
            );
            Ok(path)
        })
        .collect()
}

pub(super) async fn encode_input(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    tokio::fs::File::open(path)
        .await?
        .take(MAX_PASTED_BYTES + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(
        bytes.len() as u64 <= MAX_PASTED_BYTES,
        "An attached image is larger than 5 MiB."
    );
    Ok(base64::engine::general_purpose::STANDARD.encode(&bytes))
}

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
    store(&bytes, extension)
}

fn store(bytes: &[u8], extension: &str) -> Option<PathBuf> {
    let directory = flowmux_config::paths::state_dir()?.join("chat-images");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .ok()?;
    let digest = Sha256::digest(bytes);
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
        .and_then(|mut file| file.write_all(bytes))
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

    #[test]
    fn pasted_image_ids_cannot_reference_other_files() {
        assert!(pasted_image_path("/tmp/screenshot.png").is_none());
        assert!(pasted_image_path("../0123456789abcdef0123456789abcdef").is_none());
        assert!(input_paths(&["0123456789abcdef0123456789abcdef".into()], false).is_err());
        assert!(input_paths(&[], false).unwrap().is_empty());
    }
}
