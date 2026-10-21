// SPDX-License-Identifier: GPL-3.0-or-later

use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn cli_png_waits_for_mermaid_even_with_an_invalid_diagram() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("mermaid.md");
    let output = dir.path().join("mermaid.png");
    std::fs::write(&input, include_str!("fixtures/mermaid.md")).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_flowmux-md-viewer"))
        .args(["--render-png"])
        .arg(&output)
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());
    let rendered = image::open(output).unwrap().to_rgba8();
    let green = rendered
        .pixels()
        .filter(|p| p.0[0] < 40 && p.0[1] > 150 && p.0[1] < 190 && p.0[2] < 80)
        .count();
    assert!(
        green > 500,
        "PNG must contain the rendered green Mermaid node, found {green} pixels"
    );
}

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

#[test]
fn cli_png_includes_the_bottom_of_a_long_document_with_unicode_resource_paths() {
    let dir = tempfile::tempdir().unwrap();
    let resources = dir.path().join("리소스 directory");
    std::fs::create_dir(&resources).unwrap();
    let input = dir.path().join("long.md");
    let output = dir.path().join("long.png");
    image::RgbaImage::from_pixel(24, 16, image::Rgba([220, 10, 20, 255]))
        .save(resources.join("red 한글 %.png"))
        .unwrap();
    std::fs::write(
        &input,
        format!(
            "# Long document\n\n{}\n![red](<리소스 directory/red 한글 %.png>)",
            "Paragraph\n\n".repeat(80)
        ),
    )
    .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_flowmux-md-viewer"))
        .arg("--render-png")
        .arg(&output)
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());
    let image = image::open(output).unwrap().to_rgba8();
    assert!(image.height() > 2000);
    assert!(
        image
            .enumerate_pixels()
            .any(|(_, y, p)| y > image.height() * 3 / 4
                && p.0[0] > 180
                && p.0[1] < 80
                && p.0[2] < 80),
        "full document snapshot must include the final relative image"
    );
}

#[test]
fn cli_png_reports_unreadable_input_and_unwritable_destination() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("input.md");
    for create_input in [false, true] {
        if create_input {
            std::fs::write(&input, "# Test").unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_flowmux-md-viewer"))
            .arg("--render-png")
            .arg(dir.path().join("missing/output.png"))
            .arg(&input)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
    }
}
