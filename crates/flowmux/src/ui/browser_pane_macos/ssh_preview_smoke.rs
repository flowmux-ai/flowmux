// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

impl BrowserPane {
    pub(crate) async fn check_native_ssh_preview(app: &gtk::Application) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let server = {
            let requests = requests.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                let deadline = std::time::Instant::now() + Duration::from_secs(60);
                while !stop.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                    if let Ok((mut stream, _)) = listener.accept() {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut bytes = [0; 4096];
                        let count = stream.read(&mut bytes).unwrap_or(0);
                        // WebKit can preconnect without sending an HTTP request.
                        if count == 0 {
                            continue;
                        }
                        requests.fetch_add(1, Ordering::Relaxed);
                        let body = "<title>SSH preview</title><input value='live'>";
                        let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        let pane = Self::new(
            PaneId::new(),
            SurfaceId::new(),
            Some(&base),
            PaneCallbacks::noop_for_test(),
            BrowserEngine::Webkit,
            false,
        );
        let window = gtk::Window::builder()
            .application(app)
            .default_width(800)
            .default_height(500)
            .child(&pane.root)
            .build();
        window.present();
        wait(|| {
            pane.current_title() == "SSH preview" && !unsafe { pane.native.web_view.isLoading() }
        })
        .await;
        let before = requests.load(Ordering::Relaxed);
        assert!(before > 0);
        pane.expire_ssh_preview();
        pane.expire_ssh_preview();
        wait(|| {
            pane.current_url() == "about:blank" && !unsafe { pane.native.web_view.isLoading() }
        })
        .await;
        assert!(!pane.root.is_sensitive());
        let (tx, rx) = tokio::sync::oneshot::channel();
        pane.evaluate_js("42", move |result| {
            let _ = tx.send(result);
        });
        assert!(
            rx.await
                .unwrap()
                .unwrap_err()
                .contains("SSH preview expired"),
            "expired preview must reject CLI JavaScript"
        );
        pane.load_uri(&format!("{base}/stale"));
        pane.reload();
        assert!(!pane.go_back());
        assert!(!pane.go_forward());
        // Toolbar and native redirects bypass the public methods.
        load_uri_native(&pane.native.web_view, &format!("{base}/stale"));
        glib::timeout_future(Duration::from_millis(500)).await;
        assert_eq!(pane.current_url(), "about:blank");
        assert_eq!(
            requests.load(Ordering::Relaxed),
            before,
            "retired port was contacted"
        );
        // Reconnection must get a fresh browser; retiring one must not block another.
        let fresh = Self::new(
            PaneId::new(),
            SurfaceId::new(),
            Some(&base),
            PaneCallbacks::noop_for_test(),
            BrowserEngine::Webkit,
            false,
        );
        window.set_child(Some(&fresh.root));
        wait(|| fresh.current_title() == "SSH preview").await;
        assert!(requests.load(Ordering::Relaxed) > before);
        fresh.prepare_for_close();
        pane.prepare_for_close();
        window.destroy();
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        println!("MACOS_SSH_PREVIEW_EXPIRY_OK");
    }
}

async fn wait(mut ready: impl FnMut() -> bool) {
    glib::future_with_timeout(Duration::from_secs(20), async {
        while !ready() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("SSH preview native state timed out");
}
