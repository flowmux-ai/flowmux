// SPDX-License-Identifier: GPL-3.0-or-later

//! Images and videos an answer embeds with Markdown image syntax.

use std::path::{Path, PathBuf};

use gtk::{gdk, gio, glib, prelude::*};

use super::{text_label, FILE_LINK};

const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
/// Previews are thumbnails; the image viewer shows the full image.
const PREVIEW: (u32, u32) = (640, 360);
const PREVIEW_HEIGHT: u32 = 240;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// Shown as a link so an answer cannot make Flowmux fetch from the network.
    Remote(String),
    Data(Vec<u8>),
    Image(PathBuf),
    Video(PathBuf),
    /// Some other local file; it opens like a file link.
    File(PathBuf),
}

pub(super) fn classify(src: &str, base: &Path) -> Option<Source> {
    let src = src.trim();
    if src.starts_with("https://") || src.starts_with("http://") {
        return Some(Source::Remote(src.into()));
    }
    if let Some(data) = src.strip_prefix("data:image/") {
        let (_, encoded) = data.split_once(";base64,")?;
        let bytes = glib::base64_decode(encoded.trim());
        return (!bytes.is_empty()).then_some(Source::Data(bytes));
    }
    if src.is_empty() || src.contains("://") && !src.starts_with("file://") {
        return None;
    }
    let path = Path::new(src.strip_prefix("file://").unwrap_or(src));
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    Some(match extension.as_deref() {
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") => Source::Image(path),
        Some("mp4" | "webm" | "mov" | "mkv" | "m4v" | "ogv") => Source::Video(path),
        _ => Source::File(path),
    })
}

/// The widget for `![alt](src)`.
pub(super) fn media(src: &str, alt: &str, base: &Path) -> gtk::Widget {
    match classify(src, base) {
        Some(Source::Data(bytes)) => image(alt, None, move || Ok(bytes)),
        Some(Source::Image(path)) => {
            let read = path.clone();
            image(alt, Some(path), move || read_limited(&read))
        }
        Some(Source::Video(path)) => video(alt, &path),
        Some(Source::Remote(url)) => link(alt, &url, &url),
        Some(Source::File(path)) => {
            let target = format!("{FILE_LINK}{}", path.display());
            link(alt, &target, &path.display().to_string())
        }
        None => text_label(alt).upcast(),
    }
}

fn read_limited(path: &Path) -> Result<Vec<u8>, String> {
    let size = std::fs::metadata(path)
        .map_err(|_| "The image is unavailable.".to_string())?
        .len();
    if size > MAX_FILE_BYTES {
        return Err("The image is larger than 32 MiB.".into());
    }
    std::fs::read(path).map_err(|_| "The image is unavailable.".into())
}

/// A thumbnail no larger than `PREVIEW`, decoded within the image viewer's memory budget.
pub(super) fn decode(bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "The image format is unknown.".to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "The image could not be decoded.".to_string())?;
    Ok(if image.width() > PREVIEW.0 || image.height() > PREVIEW.1 {
        image.thumbnail(PREVIEW.0, PREVIEW.1).into_rgba8()
    } else {
        image.into_rgba8()
    })
}

fn caption(text: &str) -> gtk::Label {
    let caption = text_label(text);
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption
}

fn image(
    alt: &str,
    path: Option<PathBuf>,
    load: impl FnOnce() -> Result<Vec<u8>, String> + Send + 'static,
) -> gtk::Widget {
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 4);
    frame.add_css_class("flowmux-chat-media");
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .halign(gtk::Align::Start)
        .build();
    picture.set_alternative_text(Some(alt).filter(|alt| !alt.is_empty()));
    picture.update_property(&[gtk::accessible::Property::Label(if alt.is_empty() {
        "Image"
    } else {
        alt
    })]);
    let status = caption("Loading image…");
    frame.append(&picture);
    frame.append(&status);
    if !alt.is_empty() {
        frame.append(&caption(alt));
    }
    if let Some(path) = path.clone() {
        picture.set_cursor_from_name(Some("pointer"));
        picture.set_tooltip_text(Some(&format!("Open {}", path.display())));
        let click = gtk::GestureClick::new();
        click.connect_released(move |click, _, _, _| {
            if let Some(widget) = click.widget() {
                let _ = widget.activate_action(
                    "chat.open-file",
                    Some(&path.display().to_string().to_variant()),
                );
            }
        });
        picture.add_controller(click);
    }
    // Decode once the preview is on screen, off the main thread.
    let load = std::cell::Cell::new(Some(load));
    picture.connect_map(move |picture| {
        let Some(load) = load.take() else {
            return;
        };
        let picture = picture.downgrade();
        let status = status.downgrade();
        glib::spawn_future_local(async move {
            let decoded = gio::spawn_blocking(move || decode(&load()?))
                .await
                .unwrap_or_else(|_| Err("The image could not be decoded.".into()));
            let (Some(picture), Some(status)) = (picture.upgrade(), status.upgrade()) else {
                return;
            };
            match decoded {
                Ok(rgba) => {
                    let (width, height) = rgba.dimensions();
                    let texture = gdk::MemoryTexture::new(
                        width as i32,
                        height as i32,
                        gdk::MemoryFormat::R8g8b8a8,
                        &glib::Bytes::from_owned(rgba.into_raw()),
                        width as usize * 4,
                    );
                    picture.set_paintable(Some(&texture));
                    // The height sets the preview's size; a narrow chat scales it down.
                    picture.set_height_request(height.min(PREVIEW_HEIGHT) as i32);
                    status.set_visible(false);
                }
                Err(error) => {
                    picture.set_visible(false);
                    status.set_label(&error);
                }
            }
        });
    });
    frame.upcast()
}

fn video(alt: &str, path: &Path) -> gtk::Widget {
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 4);
    frame.add_css_class("flowmux-chat-media");
    let video = gtk::Video::builder()
        .autoplay(false)
        .file(&gio::File::for_path(path))
        .height_request(240)
        .build();
    video.update_property(&[gtk::accessible::Property::Label(if alt.is_empty() {
        "Video"
    } else {
        alt
    })]);
    frame.append(&video);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let label = if alt.is_empty() { name } else { alt.into() };
    frame.append(&link(
        &label,
        &format!("{FILE_LINK}{}", path.display()),
        &path.display().to_string(),
    ));
    frame.upcast()
}

/// A link that opens on request: remote media in the browser, local files in Flowmux.
fn link(alt: &str, target: &str, tooltip: &str) -> gtk::Widget {
    let text = if alt.is_empty() { tooltip } else { alt };
    let label = text_label("");
    label.set_markup(&format!(
        "🖼 <a href=\"{}\">{}</a>",
        glib::markup_escape_text(target),
        glib::markup_escape_text(text)
    ));
    label.set_tooltip_text(Some(tooltip));
    label.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_resolve_against_the_chat_directory() {
        let base = Path::new("/work");
        assert_eq!(
            classify("shots/a.PNG", base),
            Some(Source::Image("/work/shots/a.PNG".into()))
        );
        assert_eq!(
            classify("file:///tmp/clip.webm", base),
            Some(Source::Video("/tmp/clip.webm".into()))
        );
        assert_eq!(
            classify("https://x.dev/a.png", base),
            Some(Source::Remote("https://x.dev/a.png".into()))
        );
        assert_eq!(classify("ftp://x.dev/a.png", base), None);
        assert_eq!(
            classify("notes.txt", base),
            Some(Source::File("/work/notes.txt".into()))
        );
        assert!(matches!(
            classify("data:image/png;base64,aGk=", base),
            Some(Source::Data(bytes)) if bytes == b"hi"
        ));
    }

    #[test]
    fn previews_are_thumbnails() {
        let mut png = Vec::new();
        image::RgbaImage::new(2000, 500)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let preview = decode(&png).unwrap();
        assert!(preview.width() <= PREVIEW.0 && preview.height() <= PREVIEW.1);
        assert!(decode(b"not an image").is_err());
    }
}
