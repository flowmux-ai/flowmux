// SPDX-License-Identifier: GPL-3.0-or-later
use flowmux_browser::snapshot::CaptureSize;
use std::io::Cursor;

pub(super) fn crop_png(png: &[u8], capture: CaptureSize) -> Result<Vec<u8>, String> {
    capture.check(1.0)?;
    let clip = capture.clip.ok_or("element capture has no bounds")?;
    let mut reader = image::ImageReader::with_format(Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|error| error.to_string())?;
    let scale_x = f64::from(image.width()) / capture.width;
    let scale_y = f64::from(image.height()) / capture.height;
    let x = (clip.x * scale_x).floor() as u32;
    let y = (clip.y * scale_y).floor() as u32;
    let right = ((clip.x + clip.width) * scale_x)
        .ceil()
        .min(f64::from(image.width())) as u32;
    let bottom = ((clip.y + clip.height) * scale_y)
        .ceil()
        .min(f64::from(image.height())) as u32;
    if right <= x || bottom <= y {
        return Err("element capture has no pixels".into());
    }
    let mut output = Cursor::new(Vec::new());
    image
        .crop_imm(x, y, right - x, bottom - y)
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(output.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_browser::snapshot::CaptureRect;

    #[test]
    fn element_pixels_follow_document_scale_and_bounds() {
        let image = image::RgbaImage::from_fn(80, 40, |x, y| {
            image::Rgba(if (20..40).contains(&x) && (10..20).contains(&y) {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            })
        });
        let mut png = Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut capture = CaptureSize {
            width: 40.0,
            height: 20.0,
            viewport: false,
            clip: Some(CaptureRect {
                x: 10.0,
                y: 5.0,
                width: 10.0,
                height: 5.0,
            }),
        };
        let result = image::load_from_memory(&crop_png(png.get_ref(), capture).unwrap())
            .unwrap()
            .to_rgba8();
        assert_eq!(result.dimensions(), (20, 10));
        assert!(result.pixels().all(|pixel| pixel.0 == [255, 0, 0, 255]));
        capture.clip.as_mut().unwrap().x = 35.0;
        assert!(crop_png(png.get_ref(), capture).is_err());
    }
}
