// SPDX-License-Identifier: GPL-3.0-or-later

#[cfg(target_os = "linux")]
#[path = "browser_pane_webkit.rs"]
mod imp;

#[cfg(target_os = "macos")]
#[path = "browser_pane_macos.rs"]
mod imp;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[path = "browser_pane_stub.rs"]
mod imp;

pub use imp::*;

fn normalize_uri(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return "about:blank".into();
    }
    if raw.split_once(':').is_some_and(|(scheme, _)| {
        ["http", "https", "about", "file"]
            .iter()
            .any(|known| scheme.eq_ignore_ascii_case(known))
    }) {
        return raw.to_string();
    }
    let path = if let Some(rest) = raw.strip_prefix("~/") {
        Some(gtk::glib::home_dir().join(rest))
    } else if std::path::Path::new(raw).is_absolute()
        || raw.starts_with("./")
        || raw.starts_with("../")
    {
        Some(std::path::PathBuf::from(raw))
    } else {
        None
    };
    if let Some(path) = path {
        use gtk::gio::prelude::FileExt;
        return gtk::gio::File::for_path(path).uri().to_string();
    }
    if raw.starts_with("localhost") || raw.starts_with("127.") || raw.starts_with("[::1]") {
        return format!("http://{raw}");
    }
    if raw.contains('.') && !raw.contains(' ') {
        return format!("https://{raw}");
    }
    format!("https://www.google.com/search?q={}", urlencode(raw))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            other => format!("%{:02X}", other),
        })
        .collect()
}

/// Keep caller-owned widgets out of WebKit's completion closure: a wedged renderer
/// may never invoke it, even after the IPC client has gone away.
fn javascript_completion(
    timeout: std::time::Duration,
    on_done: impl FnOnce(Result<String, String>) + 'static,
) -> impl FnOnce(Result<String, String>) + 'static {
    let callback = std::rc::Rc::new(std::cell::RefCell::new(Some(on_done)));
    let pending = callback.clone();
    let (tx, rx) = tokio::sync::oneshot::channel();
    gtk::glib::spawn_future_local(async move {
        let error = match gtk::glib::future_with_timeout(timeout, rx).await {
            Ok(Ok(())) => return,
            Ok(Err(_)) => "browser closed before script completion",
            Err(_) => "script timed out; its page effects may still complete",
        };
        let callback = pending.borrow_mut().take();
        if let Some(callback) = callback {
            callback(Err(error.into()));
        }
    });
    move |result| {
        let callback = callback.borrow_mut().take();
        let _ = tx.send(());
        // Preserve completion ordering relative to navigation and ref invalidation.
        if let Some(callback) = callback {
            callback(result);
        }
    }
}

#[cfg(test)]
mod completion_tests {
    use super::*;
    use std::{cell::Cell, rc::Rc, time::Duration};

    #[test]
    fn script_deadline_releases_captures_and_ignores_late_completion() {
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    let captured = Rc::new(());
                    let weak = Rc::downgrade(&captured);
                    let calls = Rc::new(Cell::new(0));
                    let observed = calls.clone();
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    let callback =
                        javascript_completion(Duration::from_millis(10), move |result| {
                            observed.set(observed.get() + 1);
                            drop(captured);
                            let _ = tx.send(result);
                        });
                    let result = gtk::glib::future_with_timeout(Duration::from_secs(5), rx)
                        .await
                        .expect("script deadline did not complete")
                        .unwrap();
                    assert!(result.unwrap_err().contains("effects may still complete"));
                    assert!(
                        weak.upgrade().is_none(),
                        "stalled script retained its caller"
                    );
                    assert_eq!(calls.get(), 1);
                    callback(Ok("late".into()));
                    assert_eq!(calls.get(), 1);
                })
            })
            .unwrap();
    }

    #[test]
    fn script_completion_preserves_results_and_handles_dropped_callbacks() {
        let context = gtk::glib::MainContext::new();
        context
            .with_thread_default(|| {
                context.block_on(async {
                    for result in [
                        Some(Ok("value".into())),
                        Some(Err("script error".into())),
                        None,
                    ] {
                        let expected = result.clone();
                        let (tx, mut rx) = tokio::sync::oneshot::channel();
                        let callback =
                            javascript_completion(Duration::from_secs(1), move |result| {
                                let _ = tx.send(result);
                            });
                        let actual = if let Some(result) = result {
                            callback(result);
                            rx.try_recv()
                                .expect("normal completion must remain synchronous")
                        } else {
                            drop(callback);
                            rx.await.unwrap()
                        };
                        if let Some(expected) = expected {
                            assert_eq!(actual, expected);
                        } else {
                            assert!(actual.unwrap_err().contains("closed"));
                        }
                    }
                })
            })
            .unwrap();
    }
}

#[cfg(test)]
mod address_tests {
    use super::*;

    #[test]
    fn normalize_uri_blank_input_returns_about_blank() {
        assert_eq!(normalize_uri(""), "about:blank");
        assert_eq!(normalize_uri("   "), "about:blank");
    }

    #[test]
    fn normalize_uri_preserves_explicit_schemes() {
        for raw in [
            "http://example.com",
            "https://example.com/path",
            "about:blank",
            "file:///tmp/x.html",
            "file:///tmp/a%20b.html#section",
            "FILE:///tmp/x.html",
            "HTTPS://example.com",
        ] {
            assert_eq!(normalize_uri(raw), raw);
        }
    }

    #[test]
    fn normalize_uri_promotes_localhost_to_http() {
        assert_eq!(normalize_uri("localhost:3000"), "http://localhost:3000");
        assert_eq!(normalize_uri("127.0.0.1:8080"), "http://127.0.0.1:8080");
    }

    #[test]
    fn normalize_uri_promotes_dotted_host_to_https() {
        assert_eq!(normalize_uri("example.com"), "https://example.com");
        assert_eq!(
            normalize_uri("docs.gtk.org/gtk4/"),
            "https://docs.gtk.org/gtk4/"
        );
    }

    #[test]
    fn normalize_uri_falls_back_to_search() {
        assert_eq!(
            normalize_uri("hello world"),
            "https://www.google.com/search?q=hello+world"
        );
    }

    #[test]
    fn normalize_uri_encodes_local_paths_without_treating_them_as_hosts() {
        use gtk::gio::prelude::FileExt;
        assert_eq!(normalize_uri(" /tmp/index.html "), "file:///tmp/index.html");
        assert_eq!(
            normalize_uri("/tmp/한 글 #1%.html"),
            "file:///tmp/%ED%95%9C%20%EA%B8%80%20%231%25.html"
        );
        for (raw, path) in [
            ("~/preview.html", gtk::glib::home_dir().join("preview.html")),
            (
                "./preview.html",
                std::env::current_dir().unwrap().join("preview.html"),
            ),
            (
                "../preview.html",
                std::env::current_dir()
                    .unwrap()
                    .parent()
                    .unwrap()
                    .join("preview.html"),
            ),
        ] {
            let uri = normalize_uri(raw);
            assert!(uri.starts_with("file:///"), "{raw}: {uri}");
            assert_eq!(gtk::gio::File::for_uri(&uri).path(), Some(path));
            assert_eq!(normalize_uri(&uri), uri);
        }
    }
}
