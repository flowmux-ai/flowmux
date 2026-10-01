// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs only in the opt-in native test executable, on the AppKit main thread.
use super::*;
use crate::ui::browser_pane::BrowserPane;
use flowmux_state::{State, WindowOwner};
use std::io::{BufRead, Read, Write};

pub(crate) fn run() {
    let isolated = tempfile::tempdir().expect("isolated native smoke directory");
    for (key, directory) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
        ("FLOWMUX_RUNTIME_DIR", "run"),
    ] {
        let path = isolated.path().join(directory);
        std::fs::create_dir_all(&path).unwrap();
        std::env::set_var(key, path);
    }
    std::env::set_var("SHELL", "/bin/sh");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    adw::init().expect("native macOS display must be available");
    let app = adw::Application::builder()
        .application_id("com.flowmux.NativeSmoke")
        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
        .build();
    app.register(None::<&gtk::gio::Cancellable>).unwrap();
    glib::MainContext::default().block_on(async {
        glib::future_with_timeout(Duration::from_secs(120), check(&app, isolated.path()))
            .await
            .expect("native smoke exceeded 120 seconds");
    });
    println!("MACOS_NATIVE_SMOKE_OK");
}

async fn wait_until(description: &str, mut ready: impl FnMut() -> bool) {
    glib::future_with_timeout(Duration::from_secs(30), async {
        while !ready() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("native UI timed out: {description}"));
}

async fn javascript(browser: &BrowserPane, source: &str) -> String {
    let (tx, rx) = oneshot::channel();
    browser.evaluate_js(source, move |result| {
        let _ = tx.send(result);
    });
    glib::future_with_timeout(Duration::from_secs(10), rx)
        .await
        .expect("WKWebView JavaScript timed out")
        .unwrap()
        .expect("WKWebView JavaScript failed")
}

async fn check(app: &adw::Application, root: &std::path::Path) {
    let owner = WindowOwner::current();
    let store = StateStore::new_lazy_window(State::default(), owner);
    let workspace = store
        .create_workspace(Some("Native smoke".into()), root.into())
        .await;
    let ws = store.get_workspace(workspace).await.unwrap();
    let terminal_pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
    let (bridge, rx) = Bridge::new();
    let controller = WindowController::new(
        app,
        store.clone(),
        Arc::new(ResolvedTheme::load()),
        bridge.clone(),
        gtk::CssProvider::new(),
        None,
    );
    controller.render_workspace(&ws);
    spawn_dispatch_loop(rx, controller.clone());
    controller.window.present();
    wait_until("window mapped", || controller.window.is_mapped()).await;
    let terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(terminal_pane)
        .unwrap()
        .clone();
    let pid = terminal.pid.get().expect("terminal shell must be running");

    println!("MACOS_NATIVE_BROWSER_START");
    // A loopback fixture avoids file URL access differences between macOS versions.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let page = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        for line in std::io::BufReader::new((&stream).take(8192)).lines() {
            if line.unwrap().is_empty() {
                break;
            }
        }
        let body = "<!doctype html><title>Native smoke</title><input id=smoke>";
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let (ack, opened) = oneshot::channel();
    bridge
        .tx
        .send(GtkCommand::BrowserOpenSplit {
            target_pane: Some(terminal_pane),
            url,
            direction: SplitDirection::Vertical,
            ack,
        })
        .await
        .unwrap();
    let pane = opened.await.unwrap().unwrap().pane;
    let browser = controller
        .pane_registry
        .borrow()
        .active_browser(pane)
        .unwrap()
        .clone();
    wait_until("browser loaded", || {
        browser.current_title() == "Native smoke"
    })
    .await;
    page.join().unwrap();
    assert_eq!(
        javascript(
            &browser,
            "document.getElementById('smoke').value='retained'; window.smoke=42; 'ready'"
        )
        .await,
        "ready"
    );
    browser.grab_focus();
    assert_eq!(
        javascript(
            &browser,
            "document.getElementById('smoke').focus(); document.activeElement.id"
        )
        .await,
        "smoke"
    );
    controller.rerender_workspace(&store.get_workspace(workspace).await.unwrap());
    assert_eq!(
        javascript(
            &browser,
            "JSON.stringify([window.smoke, document.getElementById('smoke').value])"
        )
        .await,
        "[42,\"retained\"]"
    );
    assert_eq!(
        controller
            .pane_registry
            .borrow()
            .active_terminal(terminal_pane)
            .unwrap()
            .pid
            .get(),
        Some(pid)
    );
    println!("MACOS_NATIVE_BROWSER_OK");

    let file = root.join("unsaved.txt");
    std::fs::write(&file, "original\n").unwrap();
    let (_, surface) = store
        .add_editor_surface_to_pane(pane, root.into())
        .await
        .unwrap();
    controller
        .attach_or_rerender_surface(workspace, pane, surface)
        .await
        .unwrap();
    let editor = controller.pane_registry.borrow().editors[&surface].clone();
    editor.open_file(&file).unwrap();
    editor.flush_pending_changes().await.unwrap();
    editor.grab_focus();
    wait_until("editor focused", || editor.has_native_focus()).await;
    editor.insert_smoke_text("unsaved smoke");
    wait_until("editor dirty", || !editor.dirty_document_paths().is_empty()).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original\n");
    controller.focus_pane(terminal_pane);
    wait_until("terminal focused", || !editor.has_native_focus()).await;
    controller.focus_pane(pane);
    wait_until("editor refocused", || editor.has_native_focus()).await;
    println!("MACOS_NATIVE_FOCUS_OK");

    controller.window.close();
    wait_until("dirty dialog presented", || {
        controller.window.visible_dialog().is_some()
    })
    .await;
    let dialog = controller
        .window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert_eq!(
        dialog.heading().as_deref(),
        Some("Save changes before closing?")
    );
    assert!(dialog.close());
    wait_until("dirty dialog closed", || {
        !controller.window_close.prompting.get() && controller.window.visible_dialog().is_none()
    })
    .await;
    assert!(controller.window.is_visible());
    assert!(!editor.dirty_document_paths().is_empty());
    assert_eq!(terminal.pid.get(), Some(pid));
    println!("MACOS_NATIVE_CLOSE_CANCEL_OK");

    editor.save_all_dirty().unwrap();
    wait_until("editor saved", || editor.dirty_document_paths().is_empty()).await;
    let state_path = flowmux_state::default_path().unwrap();
    if state_path.exists() {
        std::fs::remove_file(&state_path).unwrap();
    }
    std::fs::create_dir_all(&state_path).unwrap();
    controller.window.close();
    wait_until("save error presented", || {
        controller.window.visible_dialog().is_some()
    })
    .await;
    let dialog = controller
        .window
        .visible_dialog()
        .unwrap()
        .downcast::<adw::AlertDialog>()
        .unwrap();
    assert_eq!(dialog.heading().as_deref(), Some("Could not save session"));
    assert!(controller.window.is_visible());
    assert!(!controller.window_close.approved.get());
    let (ack, screen) = oneshot::channel();
    bridge
        .send(GtkCommand::PaneReadScreen {
            pane: terminal_pane,
            ack,
        })
        .await
        .unwrap();
    assert!(glib::future_with_timeout(Duration::from_secs(2), screen)
        .await
        .unwrap()
        .unwrap()
        .is_ok());
    std::fs::remove_dir(&state_path).unwrap();
    assert!(dialog.close());
    wait_until("save error closed", || {
        !controller.window_close.prompting.get() && controller.window.visible_dialog().is_none()
    })
    .await;
    controller.window.close();
    wait_until("window closed", || {
        controller.window_close.approved.get() && !controller.window.is_visible()
    })
    .await;
    let saved = flowmux_state::load().unwrap();
    assert!(saved
        .windows
        .iter()
        .any(|window| window.instance_id == owner.instance_id));
    println!("MACOS_NATIVE_SAVE_RETRY_OK");
}
