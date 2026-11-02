// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_cookies::{cookie::SameSite, source::BrowserId};
use std::{cell::Cell, cell::RefCell, time::Duration};

fn fixture() -> (tempfile::TempDir, import::Profile) {
    let dir = tempfile::tempdir().unwrap();
    let database = dir.path().join("Cookies");
    rusqlite::Connection::open(&database)
        .unwrap()
        .execute_batch(
            "CREATE TABLE meta(key TEXT,value TEXT);
             INSERT INTO meta VALUES('version','24');
             CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,
                 expires_utc INTEGER,is_secure INTEGER,is_httponly INTEGER,
                 samesite INTEGER,encrypted_value BLOB);
             INSERT INTO cookies VALUES('example.test','sid','fixture','/',0,1,1,1,X'');",
        )
        .unwrap();
    (
        dir,
        import::Profile {
            browser: BrowserId::Chrome,
            name: "fixture".into(),
            database,
        },
    )
}

async fn wait(check: impl Fn() -> bool) {
    for _ in 0..250 {
        if check() {
            return;
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    assert!(check(), "cookie import UI did not reach the expected state");
}

async fn start(
    profile: import::Profile,
    apply: Apply,
    url: Rc<RefCell<String>>,
    reloads: Rc<Cell<usize>>,
) -> (adw::Window, glib::JoinHandle<()>) {
    adw::init().unwrap();
    let button = gtk::Button::with_label("Import cookies");
    let window = adw::Window::builder()
        .default_width(600)
        .default_height(400)
        .content(&button)
        .build();
    window.present();
    wait(|| button.is_mapped()).await;
    let parent = window.clone();
    let task = glib::MainContext::default().spawn_local(async move {
        import_selected(
            parent.upcast_ref(),
            &button,
            "example.test".into(),
            profile,
            Rc::new(move || url.borrow().clone()),
            apply,
            Rc::new(move || reloads.set(reloads.get() + 1)),
        )
        .await;
    });
    (window, task)
}

fn visible(window: &adw::Window) -> Option<adw::AlertDialog> {
    window.visible_dialog()?.downcast().ok()
}

async fn finish(window: &adw::Window, task: glib::JoinHandle<()>, title: &str) {
    wait(|| visible(window).is_some_and(|dialog| dialog.heading().as_deref() == Some(title))).await;
    let dialog = visible(window).unwrap();
    assert_eq!(
        window.dialogs().n_items(),
        1,
        "result stacked another dialog"
    );
    assert!(dialog.can_close());
    assert!(dialog.is_response_enabled("cancel"));
    assert_eq!(dialog.response_label("cancel"), "Close");
    // Let the native presentation ticks finish before simulating a user close.
    glib::timeout_future(Duration::from_millis(100)).await;
    assert!(dialog.close());
    tokio::select! {
        result = task => result.unwrap(),
        _ = glib::timeout_future_seconds(5) => panic!("closed import retained its task"),
    }
    wait(|| window.dialogs().n_items() == 0).await;
    window.destroy();
}

#[gtk::test]
async fn immediate_and_delayed_apply_outcomes_leave_one_dismissible_dialog() {
    for delay in [0, 100] {
        for result in [
            Ok(1),
            Err(
                "WebKit could not apply a cookie. 0 cookies were applied before the failure."
                    .into(),
            ),
            Err(
                "WebKit could not apply a cookie. 1 cookies were applied before the failure."
                    .into(),
            ),
        ] {
            let (_dir, profile) = fixture();
            let expected_reload = usize::from(result.is_ok());
            let title = if result.is_ok() {
                "Cookies imported"
            } else {
                "Cookie import incomplete"
            };
            let expected_error = result.as_ref().err().cloned();
            let reloads = Rc::new(Cell::new(0));
            let apply: Apply = Rc::new(move |cookies| {
                assert_eq!(cookies.len(), 1);
                let result = result.clone();
                Box::pin(async move {
                    glib::timeout_future(Duration::from_millis(delay)).await;
                    result
                })
            });
            let (window, task) = start(
                profile,
                apply,
                Rc::new(RefCell::new("https://example.test/".into())),
                reloads.clone(),
            )
            .await;
            wait(|| {
                visible(&window).is_some_and(|dialog| dialog.heading().as_deref() == Some(title))
            })
            .await;
            if let Some(error) = expected_error {
                assert_eq!(visible(&window).unwrap().body(), error);
            }
            assert_eq!(reloads.get(), expected_reload);
            finish(&window, task, title).await;
        }
    }
}

#[gtk::test]
async fn applying_disables_cancellation_and_does_not_reload_a_changed_site() {
    let (_dir, profile) = fixture();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let receiver = RefCell::new(Some(rx));
    let started = Rc::new(Cell::new(false));
    let flag = started.clone();
    let apply: Apply = Rc::new(move |_| {
        flag.set(true);
        let rx = receiver.borrow_mut().take().unwrap();
        Box::pin(async move { rx.await.unwrap() })
    });
    let url = Rc::new(RefCell::new("https://example.test/".into()));
    let reloads = Rc::new(Cell::new(0));
    let (window, task) = start(profile, apply, url.clone(), reloads.clone()).await;
    wait(|| started.get()).await;
    let progress = visible(&window).unwrap();
    assert_eq!(progress.heading().as_deref(), Some("Importing cookies…"));
    assert!(!progress.can_close());
    assert!(!progress.is_response_enabled("cancel"));
    assert!(!progress.close());
    *url.borrow_mut() = "https://other.test/".into();
    tx.send(Ok(1)).unwrap();
    finish(&window, task, "Cookies imported").await;
    assert_eq!(reloads.get(), 0, "import reloaded an unrelated site");
}

#[gtk::test]
async fn empty_failed_or_stale_reads_never_apply_or_reload() {
    for scenario in ["empty", "missing", "stale", "hidden", "locked", "limit"] {
        let (_dir, profile) = fixture();
        let writer = rusqlite::Connection::open(&profile.database).unwrap();
        if scenario == "empty" {
            writer.execute("DELETE FROM cookies", []).unwrap();
        } else if scenario == "missing" {
            std::fs::remove_file(&profile.database).unwrap();
        } else if scenario == "locked" {
            writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        } else if scenario == "limit" {
            writer.execute_batch(
                "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<2000)
                 INSERT INTO cookies SELECT 'example.test','cookie'||x,'fixture','/',0,1,1,1,X'' FROM n;",
            ).unwrap();
        }
        let url = if scenario == "stale" {
            "https://other.test/"
        } else {
            "https://example.test/"
        };
        let reloads = Rc::new(Cell::new(0));
        let (window, task) = start(
            profile,
            Rc::new(|_| panic!("invalid import reached native insertion")),
            Rc::new(RefCell::new(url.into())),
            reloads.clone(),
        )
        .await;
        if scenario == "hidden" {
            window.content().unwrap().set_visible(false);
        }
        let title = match scenario {
            "empty" => "No matching cookies",
            "missing" | "locked" | "limit" => "Cookie import failed",
            _ => "Page changed",
        };
        finish(&window, task, title).await;
        assert_eq!(reloads.get(), 0);
    }
}

#[gtk::test]
async fn destroying_the_window_during_apply_releases_the_import_task() {
    let (_dir, profile) = fixture();
    let (tx, rx) = tokio::sync::oneshot::channel();
    let receiver = RefCell::new(Some(rx));
    let started = Rc::new(Cell::new(false));
    let flag = started.clone();
    let reloads = Rc::new(Cell::new(0));
    let (window, task) = start(
        profile,
        Rc::new(move |_| {
            flag.set(true);
            let rx = receiver.borrow_mut().take().unwrap();
            Box::pin(async move { rx.await.unwrap() })
        }),
        Rc::new(RefCell::new("https://example.test/".into())),
        reloads.clone(),
    )
    .await;
    wait(|| started.get()).await;
    window.destroy();
    tx.send(Ok(1)).unwrap();
    tokio::select! {
        result = task => result.unwrap(),
        _ = glib::timeout_future_seconds(5) => panic!("destroyed window retained its import task"),
    }
    assert_eq!(reloads.get(), 0);
}

#[gtk::test]
async fn destroying_the_window_during_read_or_result_releases_the_import_task() {
    for reading in [true, false] {
        let (_dir, profile) = fixture();
        let writer = rusqlite::Connection::open(&profile.database).unwrap();
        if reading {
            writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        }
        let (window, task) = start(
            profile,
            Rc::new(move |_| {
                assert!(
                    !reading,
                    "closed window imported cookies after cancellation"
                );
                Box::pin(async { Ok(1) })
            }),
            Rc::new(RefCell::new("https://example.test/".into())),
            Rc::new(Cell::new(0)),
        )
        .await;
        let title = if reading {
            "Importing cookies…"
        } else {
            "Cookies imported"
        };
        wait(|| visible(&window).is_some_and(|dialog| dialog.heading().as_deref() == Some(title)))
            .await;
        window.destroy();
        tokio::select! {
            result = task => result.unwrap(),
            _ = glib::timeout_future_seconds(1) => panic!("destroyed window retained its import task"),
        }
        if reading {
            writer.execute_batch("ROLLBACK").unwrap();
        }
    }
}

#[gtk::test]
async fn destroying_the_window_releases_an_early_error_message() {
    adw::init().unwrap();
    let window = adw::Window::builder()
        .default_width(600)
        .default_height(400)
        .build();
    window.present();
    let parent = window.clone();
    let task = glib::MainContext::default().spawn_local(async move {
        message(
            parent.upcast_ref(),
            "Import cookies",
            "Open an HTTP or HTTPS site before importing cookies",
        )
        .await;
    });
    wait(|| visible(&window).is_some()).await;
    window.destroy();
    tokio::select! {
        result = task => result.unwrap(),
        _ = glib::timeout_future_seconds(1) => panic!("destroyed window retained its message task"),
    }
    tokio::select! {
        _ = message(window.upcast_ref(), "No browser profiles found", "No profiles") => (),
        _ = glib::timeout_future_seconds(1) => panic!("late discovery result opened a dialog on a destroyed window"),
    }
}

#[gtk::test]
async fn switching_tabs_rejects_the_still_mapped_outgoing_page() {
    adw::init().unwrap();
    let (_dir, profile) = fixture();
    let source = gtk::Button::with_label("Import cookies");
    let destination = gtk::Label::new(Some("Other tab"));
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(1000)
        .build();
    stack.add_child(&source);
    stack.add_child(&destination);
    let window = adw::Window::builder()
        .default_width(600)
        .default_height(400)
        .content(&stack)
        .build();
    window.present();
    wait(|| source.is_mapped()).await;
    glib::timeout_future(Duration::from_millis(100)).await;
    stack.set_visible_child(&destination);
    assert!(
        source.is_mapped(),
        "fixture must retain the outgoing tab during crossfade"
    );
    let parent = window.clone();
    let task = glib::MainContext::default().spawn_local(async move {
        import_selected(
            parent.upcast_ref(),
            &source,
            "example.test".into(),
            profile,
            Rc::new(|| "https://example.test/".into()),
            Rc::new(|_| panic!("outgoing tab imported cookies during crossfade")),
            Rc::new(|| panic!("outgoing tab reloaded during crossfade")),
        )
        .await;
    });
    finish(&window, task, "Page changed").await;
}

#[gtk::test]
async fn native_validation_precedes_all_writes_and_preserves_cookie_flags() {
    let session = webkit6::NetworkSession::new_ephemeral();
    let _view = webkit6::WebView::builder()
        .network_session(&session)
        .build();
    let manager = session.cookie_manager().unwrap();
    let cookie = Cookie {
        host: "example.test".into(),
        name: "sid".into(),
        value: "fixture".into(),
        path: "/".into(),
        expires_at: None,
        secure: true,
        http_only: true,
        same_site: SameSite::Strict,
    };
    let mut invalid = cookie.clone();
    invalid.value = "invalid;header".into();
    assert!(apply_webkit(manager.clone(), vec![cookie.clone(), invalid])
        .await
        .is_err());
    assert!(manager
        .cookies_future("https://example.test/")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        apply_webkit(manager.clone(), vec![cookie.clone()])
            .await
            .unwrap(),
        1
    );
    let mut cookies = manager
        .cookies_future("https://example.test/")
        .await
        .unwrap();
    assert_eq!(cookies.len(), 1);
    assert_eq!(cookies[0].value().as_deref(), Some("fixture"));
    assert!(cookies[0].is_secure());
    assert!(cookies[0].is_http_only());
    assert_eq!(
        cookies[0].same_site_policy(),
        webkit6::soup::SameSitePolicy::Strict
    );
    assert!(manager
        .cookies_future("https://other.test/")
        .await
        .unwrap()
        .is_empty());
    let other_session = webkit6::NetworkSession::new_ephemeral();
    let _other_view = webkit6::WebView::builder()
        .network_session(&other_session)
        .build();
    let other = other_session.cookie_manager().unwrap();
    assert!(other
        .cookies_future("https://example.test/")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        apply_webkit(manager.clone(), vec![cookie]).await.unwrap(),
        1
    );
    assert_eq!(
        manager
            .cookies_future("https://example.test/")
            .await
            .unwrap()
            .len(),
        1
    );
}
