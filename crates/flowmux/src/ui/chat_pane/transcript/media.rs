// SPDX-License-Identifier: GPL-3.0-or-later

//! Images and videos an answer embeds with Markdown image syntax.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gtk::{gdk, gio, glib, prelude::*};

use super::viewer::{self, Content};
use super::{text_label, FILE_LINK};

const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_DECODE_BYTES: u64 = 64 * 1024 * 1024;
/// Previews are thumbnails at their display size; the viewer shows the full image.
const PREVIEW: (u32, u32) = (640, 240);

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

/// An image's encoded bytes, or the file holding them.
#[derive(Clone)]
pub(super) enum Pixels {
    File(PathBuf),
    Data(Arc<Vec<u8>>),
}

/// The widget for `![alt](src)`.
pub(super) fn media(src: &str, alt: &str, base: &Path) -> gtk::Widget {
    match classify(src, base) {
        Some(Source::Data(bytes)) => image(alt, Pixels::Data(Arc::new(bytes))),
        Some(Source::Image(path)) => image(alt, Pixels::File(path)),
        Some(Source::Video(path)) => video(alt, &path),
        Some(Source::Remote(url)) => link(alt, &url, &url).upcast(),
        Some(Source::File(path)) => {
            let target = format!("{FILE_LINK}{}", path.display());
            link(alt, &target, &path.display().to_string()).upcast()
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

/// Decodes `pixels` to fit within `bound`, within the image viewer's memory budget.
pub(super) fn decode_pixels(
    pixels: &Pixels,
    bound: (u32, u32),
) -> Result<image::RgbaImage, String> {
    match pixels {
        Pixels::File(path) => decode(&read_limited(path)?, bound),
        Pixels::Data(bytes) => decode(bytes, bound),
    }
}

fn decode(bytes: &[u8], bound: (u32, u32)) -> Result<image::RgbaImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| "The image format is unknown.".to_string())?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|_| "The image could not be decoded.".to_string())?;
    Ok(if image.width() > bound.0 || image.height() > bound.1 {
        image.thumbnail(bound.0, bound.1).into_rgba8()
    } else {
        image.into_rgba8()
    })
}

pub(super) fn texture(rgba: image::RgbaImage) -> gdk::MemoryTexture {
    let (width, height) = rgba.dimensions();
    gdk::MemoryTexture::new(
        width as i32,
        height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(rgba.into_raw()),
        width as usize * 4,
    )
}

fn title(alt: &str, path: Option<&Path>, fallback: &str) -> String {
    if !alt.is_empty() {
        return alt.into();
    }
    path.and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| fallback.into())
}

fn caption(text: &str) -> gtk::Label {
    let caption = text_label(text);
    caption.add_css_class("caption");
    caption.add_css_class("dim-label");
    caption
}

fn image(alt: &str, pixels: Pixels) -> gtk::Widget {
    let frame = gtk::Box::new(gtk::Orientation::Vertical, 4);
    frame.add_css_class("flowmux-chat-media");
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Contain)
        .build();
    // A picture's natural height follows the width it is offered, which is the whole chat when
    // rows get spare height. The clamp offers no more than the thumbnail's width.
    let clamp = adw::Clamp::builder().child(&picture).build();
    picture.set_alternative_text(Some(alt).filter(|alt| !alt.is_empty()));
    let name = title(
        alt,
        match &pixels {
            Pixels::File(path) => Some(path),
            Pixels::Data(_) => None,
        },
        "Image",
    );
    // A button, so the preview opens from the keyboard too.
    let open = gtk::Button::builder()
        .child(&clamp)
        .halign(gtk::Align::Start)
        .tooltip_text("View image")
        .visible(false)
        .build();
    open.add_css_class("flat");
    open.add_css_class("flowmux-chat-media-open");
    open.update_property(&[gtk::accessible::Property::Label(&format!("View {name}"))]);
    let status = caption("Loading image…");
    frame.append(&open);
    frame.append(&status);
    if !alt.is_empty() {
        frame.append(&caption(alt));
    }
    let shown = pixels.clone();
    open.connect_clicked(move |open| {
        viewer::show(open.upcast_ref(), &name, Content::Image(shown.clone()));
    });
    // Decode once the preview is on screen, off the main thread.
    let pixels = std::cell::Cell::new(Some(pixels));
    status.connect_map(move |status| {
        let Some(pixels) = pixels.take() else {
            return;
        };
        let open = open.downgrade();
        let clamp = clamp.downgrade();
        let picture = picture.downgrade();
        let status = status.downgrade();
        glib::spawn_future_local(async move {
            let decoded = gio::spawn_blocking(move || decode_pixels(&pixels, PREVIEW))
                .await
                .unwrap_or_else(|_| Err("The image could not be decoded.".into()));
            let (Some(open), Some(clamp), Some(picture), Some(status)) = (
                open.upgrade(),
                clamp.upgrade(),
                picture.upgrade(),
                status.upgrade(),
            ) else {
                return;
            };
            match decoded {
                Ok(rgba) => {
                    // A long transcript gives rows their minimum height, which is zero for a
                    // picture that can shrink; the thumbnail's height keeps it at full size.
                    picture.set_height_request(rgba.height() as i32);
                    clamp.set_maximum_size(rgba.width() as i32);
                    clamp.set_tightening_threshold(rgba.width() as i32);
                    picture.set_paintable(Some(&texture(rgba)));
                    open.set_visible(true);
                    status.set_visible(false);
                }
                Err(error) => status.set_label(&error),
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
    let name = title(alt, Some(path), "Video");
    video.update_property(&[gtk::accessible::Property::Label(&name)]);
    let error = caption("");
    error.set_visible(false);
    show_errors(&video, &error);
    let link = link(
        &name,
        &format!("{FILE_LINK}{}", path.display()),
        &path.display().to_string(),
    );
    link.set_hexpand(true);
    let expand = gtk::Button::from_icon_name("view-fullscreen-symbolic");
    expand.add_css_class("flat");
    expand.set_tooltip_text(Some("View larger"));
    expand.update_property(&[gtk::accessible::Property::Label(&format!(
        "View {name} larger"
    ))]);
    let path = path.to_path_buf();
    expand.connect_clicked(move |expand| {
        viewer::show(expand.upcast_ref(), &name, Content::Video(path.clone()));
    });
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.append(&link);
    row.append(&expand);
    frame.append(&video);
    frame.append(&error);
    frame.append(&row);
    frame.upcast()
}

/// Replaces `video` with the stream's error, which would otherwise leave an empty box.
pub(super) fn show_errors(video: &gtk::Video, status: &gtk::Label) {
    let Some(stream) = video.media_stream() else {
        return;
    };
    let show = {
        let video = video.downgrade();
        let status = status.downgrade();
        move |stream: &gtk::MediaStream| {
            let (Some(video), Some(status), Some(error)) =
                (video.upgrade(), status.upgrade(), stream.error())
            else {
                return;
            };
            video.set_visible(false);
            // GStreamer messages carry element paths and source lines; keep them for the tooltip.
            status.set_label("The video cannot play.");
            status.set_tooltip_text(Some(error.message()));
            status.set_visible(true);
        }
    };
    show(&stream);
    stream.connect_error_notify(show);
}

/// A link that opens on request: remote media in the browser, local files in Flowmux.
fn link(alt: &str, target: &str, tooltip: &str) -> gtk::Label {
    let text = if alt.is_empty() { tooltip } else { alt };
    let label = text_label("");
    label.set_markup(&format!(
        "🖼 <a href=\"{}\">{}</a>",
        glib::markup_escape_text(target),
        glib::markup_escape_text(text)
    ));
    label.set_tooltip_text(Some(tooltip));
    label
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
        let pixels = Pixels::Data(Arc::new(png));
        let preview = decode_pixels(&pixels, PREVIEW).unwrap();
        assert!(preview.width() <= PREVIEW.0 && preview.height() <= PREVIEW.1);
        // The viewer decodes the original again rather than enlarging the thumbnail.
        let full = decode_pixels(&pixels, viewer::FULL).unwrap();
        assert_eq!(full.dimensions(), (2000, 500));
        assert!(decode(b"not an image", PREVIEW).is_err());
    }
}
