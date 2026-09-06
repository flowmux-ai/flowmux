// SPDX-License-Identifier: GPL-3.0-or-later

use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn cli_renders_png_for_markdown_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let input = dir.path().join("sample.md");
    let output = dir.path().join("sample.png");
    let image_path = dir.path().join("red.png");

    let image = image::RgbaImage::from_pixel(24, 16, image::Rgba([220, 10, 20, 255]));
    image.save(&image_path).expect("save test image");
    std::fs::write(
        &input,
        "# Title\n\nBody with **strong text**.\n\n![red](red.png)\n\n| A | B |\n|---|---|\n| 1 | 2 |\n",
    )
    .expect("write markdown");

    let status = Command::new(env!("CARGO_BIN_EXE_flowmux-md-viewer"))
        .arg("--render-png")
        .arg(&output)
        .arg("--width")
        .arg("640")
        .arg(&input)
        .status()
        .expect("run renderer");
    assert!(status.success(), "renderer exited with {status}");

    let rendered = image::open(&output).expect("open rendered png").to_rgba8();
    assert!(
        rendered.width() >= 640,
        "rendered width should be at least the requested CSS width"
    );
    assert!(rendered.height() > 120);
    assert!(
        rendered
            .pixels()
            .any(|pixel| pixel.0 != [255, 255, 255, 255]),
        "rendered PNG should not be blank"
    );
    assert!(
        rendered
            .pixels()
            .any(|pixel| pixel.0[0] > 180 && pixel.0[1] < 80 && pixel.0[2] < 80),
        "relative image should render into the PNG"
    );
}

#[test]
fn cli_gui_accepts_markdown_file_argument() {
    let dir = tempfile::tempdir().expect("tempdir");
    let markdown = dir.path().join("README.md");
    std::fs::write(&markdown, "# GUI\n\nBody").expect("write markdown");

    let mut child = Command::new(env!("CARGO_BIN_EXE_flowmux-md-viewer"))
        .arg(&markdown)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn viewer");

    std::thread::sleep(Duration::from_millis(800));
    let exited_early = child.try_wait().expect("poll viewer").is_some();
    if !exited_early {
        child.kill().expect("stop viewer");
    }
    let output = child.wait_with_output().expect("collect viewer output");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("This application can not open files"),
        "viewer handed the Markdown path back to GApplication: {stderr}"
    );
    assert!(
        !exited_early,
        "viewer should keep a GUI open for Markdown files: {stderr}"
    );
}
