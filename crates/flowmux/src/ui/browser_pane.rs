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
