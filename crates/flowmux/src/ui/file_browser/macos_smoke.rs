// SPDX-License-Identifier: GPL-3.0-or-later
//! Cross-volume paste through the live panel and its ordinary worker.
//! Run with test-file-move-failure.c loaded into the isolated native executable.
use super::*;
use std::os::unix::fs::MetadataExt;
use std::time::Duration;

async fn wait(description: &str, mut ready: impl FnMut() -> bool) {
    glib::future_with_timeout(Duration::from_secs(15), async {
        while !ready() {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("file browser timed out: {description}"));
}

pub(crate) async fn check(parent: &impl IsA<gtk::Window>, root: &Path, volume: &Path) {
    let panel = FileBrowserPanel::new();
    let window = gtk::Window::builder()
        .transient_for(parent)
        .default_width(480)
        .default_height(640)
        .child(panel.widget())
        .build();
    window.present();
    assert_ne!(
        fs::metadata(root).unwrap().dev(),
        fs::metadata(volume).unwrap().dev()
    );
    let destination = tempfile::tempdir_in(volume).unwrap();
    let output = destination.path().join("output");
    fs::create_dir(&output).unwrap();
    for mode in ["cancel", "error", "normal"] {
        let source = root.join(mode);
        fs::create_dir(&source).unwrap();
        for name in ["a.txt", "b.txt"] {
            fs::write(source.join(name), name.as_bytes()).unwrap();
        }
        let entered = root.join(format!("{mode}-entered"));
        let release = root.join(format!("{mode}-release"));
        std::env::set_var("FLOWMUX_FILE_MOVE_SMOKE_SOURCE", &source);
        std::env::set_var("FLOWMUX_FILE_MOVE_SMOKE_ENTERED", &entered);
        std::env::set_var("FLOWMUX_FILE_MOVE_SMOKE_RELEASE", &release);
        std::env::set_var("FLOWMUX_FILE_MOVE_SMOKE_MODE", mode);
        panel.show_for_root_with_state(root.into(), None);
        panel.focus_path(source.clone());
        assert_eq!(panel.model.borrow().focused.as_ref(), Some(&source));
        panel.cut_focused();
        panel.show_for_root_with_state(destination.path().into(), None);
        panel.focus_path(output.clone());
        assert_eq!(panel.model.borrow().focused.as_ref(), Some(&output));
        wait("panel mapped", || panel.root.is_mapped()).await;
        panel.paste_from_clipboard();
        if mode == "cancel" {
            wait("first source deletion", || {
                assert!(
                    panel.file_operation_in_progress.get() || entered.exists(),
                    "paste finished before injection: status={}, source={source:?}, destination={output:?}",
                    panel.status.text()
                );
                entered.exists()
            }).await;
            assert!(panel.cancel_operation_button.is_visible());
            panel.cancel_operation_button.emit_clicked();
            fs::write(&release, b"continue").unwrap();
        }
        wait("paste finished", || !panel.file_operation_in_progress.get()).await;
        std::env::remove_var("FLOWMUX_FILE_MOVE_SMOKE_SOURCE");
        if mode != "normal" {
            assert!(entered.exists(), "failure injector did not run");
            assert_eq!(fs::read_dir(&source).unwrap().count(), 1);
            if mode == "cancel" {
                assert_eq!(panel.status.text(), "Paste cancelled");
            } else {
                assert!(panel.status.text().starts_with("Paste failed:"));
            }
            assert_eq!(
                panel.model.borrow().clipboard.as_ref().unwrap().paths,
                [source.clone()]
            );
        } else {
            assert!(!source.exists());
            assert!(panel.model.borrow().clipboard.is_none());
        }
        let copied = output.join(source.file_name().unwrap());
        for name in ["a.txt", "b.txt"] {
            assert_eq!(fs::read(copied.join(name)).unwrap(), name.as_bytes());
        }
        println!("MACOS_FILE_MOVE_BYTES_OK mode={mode}");
    }
    window.destroy();
}
