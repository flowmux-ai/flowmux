// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::io::Write;

async fn wait(mut ready: impl FnMut() -> bool) {
    glib::future_with_timeout(Duration::from_secs(10), async {
        while !ready() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("image viewer did not update");
}

fn pixels(picture: &gtk::Picture) -> Vec<u8> {
    let texture = picture
        .paintable()
        .unwrap()
        .downcast::<gdk::Texture>()
        .unwrap();
    let stride = texture.width() as usize * 4;
    let mut pixels = vec![0; stride * texture.height() as usize];
    texture.download(&mut pixels, stride);
    pixels
}

pub(crate) async fn check(app: &adw::Application, root: &Path) {
    assert!(
        tvg::available(),
        "install compatible ThorVG before running native viewer checks"
    );
    let parent = adw::ApplicationWindow::builder()
        .application(app)
        .title("Viewer test parent")
        .build();
    parent.present();
    let animation = r##"{"v":"5.7.4","fr":30,"ip":0,"op":30,"w":100,"h":80,"layers":[{"ty":1,"ind":1,"sw":20,"sh":20,"sc":"#ff0000","ip":0,"op":30,"st":0,"ks":{"o":{"a":0,"k":100},"r":{"a":0,"k":0},"a":{"a":0,"k":[0,0,0]},"s":{"a":0,"k":[100,100,100]},"p":{"a":1,"k":[{"t":0,"s":[10,20,0],"e":[70,20,0],"o":{"x":0.33,"y":0},"i":{"x":0.67,"y":1}},{"t":29,"s":[70,20,0]}]}}}]}"##;
    let json = root.join("animation.json");
    std::fs::write(&json, animation).unwrap();
    assert_eq!(
        crate::ui::file_browser::file_open_target(&json),
        crate::ui::file_browser::FileOpenTarget::ImageViewer
    );
    let settings = root.join("settings.json");
    std::fs::write(&settings, r#"{"theme":"dark"}"#).unwrap();
    assert_eq!(
        crate::ui::file_browser::file_open_target(&settings),
        crate::ui::file_browser::FileOpenTarget::Editor
    );
    let archive = root.join("animation.lottie");
    let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
    zip.start_file(
        "animations/main.json",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(animation.as_bytes()).unwrap();
    zip.finish().unwrap();
    let svg = root.join("vector.svg");
    std::fs::write(&svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="80"><rect width="100" height="80" fill="red"/></svg>"#).unwrap();
    let mut paths = vec![svg, json, archive];
    for ext in ["png", "jpg", "webp"] {
        let path = root.join(format!("image.{ext}"));
        image::RgbImage::from_pixel(100, 80, image::Rgb([255, 0, 0]))
            .save(&path)
            .unwrap();
        paths.push(path);
    }
    let mut windows = Vec::new();
    let mut animations = Vec::new();
    for path in paths {
        open_image_viewer(&parent, path.clone());
        let title = path.file_name().unwrap().to_str().unwrap();
        let window = app
            .windows()
            .into_iter()
            .find(|w| w.title().as_deref() == Some(title))
            .expect("internal viewer window");
        wait(|| window.is_mapped()).await;
        let toolbar = window
            .downcast_ref::<adw::ApplicationWindow>()
            .unwrap()
            .content()
            .unwrap()
            .downcast::<adw::ToolbarView>()
            .unwrap();
        let content = toolbar.content().unwrap();
        if let Ok(picture) = content.clone().downcast::<gtk::Picture>() {
            let frame = pixels(&picture);
            assert!(
                frame.as_chunks::<4>().0.iter().any(|rgba| rgba[3] > 0),
                "visible pixels for {title}"
            );
            if matches!(
                path.extension().and_then(|s| s.to_str()),
                Some("json" | "lottie")
            ) {
                animations.push((picture, frame));
            }
        } else {
            let vector = content.downcast::<SvgView>().unwrap();
            wait(|| vector.width() > 0 && vector.height() > 0).await;
            window.set_default_size(640, 480);
            wait(|| {
                vector.width() >= 600
                    && vector
                        .imp()
                        .texture
                        .borrow()
                        .as_ref()
                        .is_some_and(|(size, _)| size.0 > 100)
            })
            .await;
        }
        windows.push(window);
    }
    assert_eq!(windows.len(), 6);
    assert_eq!(animations.len(), 2);
    for (picture, before) in &animations {
        wait(|| pixels(picture) != *before).await;
    }
    for window in windows {
        window.close();
    }
    drop(animations);
    wait(|| *ENGINE_REFCOUNT.lock().unwrap() == 0).await;
    assert!(
        parent.is_visible(),
        "closing viewers must preserve the parent window"
    );
    parent.close();
    println!("MACOS_IMAGE_LOTTIE_VIEWER_OK");
}
