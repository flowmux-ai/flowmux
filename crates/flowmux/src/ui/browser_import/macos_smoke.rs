// SPDX-License-Identifier: GPL-3.0-or-later
//! Permission recovery uses synthetic files; never reads the user's cookies/keychain.
use super::*;
use flowmux_cookies::source::BrowserId;
use objc2::{rc::Retained, MainThreadMarker};
use objc2_foundation::{NSArray, NSHTTPCookie};
use objc2_web_kit::{WKHTTPCookieStore, WKWebsiteDataStore};
use std::{cell::Cell, cell::RefCell, os::unix::fs::PermissionsExt, time::Duration};

async fn wait(mut ready: impl FnMut() -> bool) {
    glib::future_with_timeout(Duration::from_secs(10), async {
        while !ready() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cookie permission UI did not reach expected state");
}

async fn cookies(store: &WKHTTPCookieStore) -> Retained<NSArray<NSHTTPCookie>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = RefCell::new(Some(tx));
    let callback =
        block2::RcBlock::new(move |cookies: std::ptr::NonNull<NSArray<NSHTTPCookie>>| {
            let cookies = unsafe { Retained::retain(cookies.as_ptr()).unwrap() };
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(cookies);
            }
        });
    unsafe { store.getAllCookies(&callback) };
    glib::future_with_timeout(Duration::from_secs(10), rx)
        .await
        .unwrap()
        .unwrap()
}

fn click_response(dialog: &adw::AlertDialog, response: &str) {
    let label = dialog.response_label(response);
    let mut widgets = vec![dialog.clone().upcast::<gtk::Widget>()];
    while let Some(widget) = widgets.pop() {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_ref() == Some(&label) {
                button.emit_clicked();
                return;
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            child = widget.next_sibling();
            widgets.push(widget);
        }
    }
    panic!("missing cookie import response button: {response}");
}

pub(crate) async fn check(app: &adw::Application) {
    let fixture = tempfile::tempdir().unwrap();
    let database = fixture.path().join("Cookies");
    let writer = rusqlite::Connection::open(&database).unwrap();
    writer
        .execute_batch(
            "CREATE TABLE meta(key TEXT,value TEXT);
        INSERT INTO meta VALUES('version','24');
        CREATE TABLE cookies(host_key TEXT,name TEXT,value TEXT,path TEXT,expires_utc INTEGER,
            is_secure INTEGER,is_httponly INTEGER,samesite INTEGER,encrypted_value BLOB);
        INSERT INTO cookies VALUES('example.test','sid','fixture','/',0,1,1,1,X'');",
        )
        .unwrap();
    drop(writer);
    let original = std::fs::read(&database).unwrap();
    let profile = import::Profile {
        browser: BrowserId::Chrome,
        name: "Synthetic Chrome".into(),
        database: database.clone(),
    };
    let button = gtk::Button::with_label("Import cookies for this site");
    let window = adw::Window::builder()
        .application(app)
        .title("Flowmux cookie permissions")
        .default_width(680)
        .default_height(520)
        .content(&button)
        .build();
    window.present();
    wait(|| button.is_mapped()).await;
    let data_store =
        unsafe { WKWebsiteDataStore::nonPersistentDataStore(MainThreadMarker::new().unwrap()) };
    let store = unsafe { data_store.httpCookieStore() };
    let reloads = Rc::new(Cell::new(0));

    for (denied, stale, reply) in [
        (true, false, "cancel"),
        (true, false, "retry"),
        (false, true, "cancel"),
        (false, false, "cancel"),
    ] {
        std::fs::set_permissions(
            &database,
            std::fs::Permissions::from_mode(if denied { 0o0 } else { 0o600 }),
        )
        .unwrap();
        let parent = window.clone();
        let source = button.clone();
        let selected = profile.clone();
        let destination = store.clone();
        let count = reloads.clone();
        let task = glib::MainContext::default().spawn_local(async move {
            import_selected(
                parent.upcast_ref(),
                &source,
                "example.test".into(),
                selected,
                Rc::new(move || {
                    if stale {
                        "https://other.test/"
                    } else {
                        "https://example.test/"
                    }
                    .into()
                }),
                Rc::new(move |cookies| Box::pin(apply_macos(destination.clone(), cookies))),
                Rc::new(move || count.set(count.get() + 1)),
            )
            .await
        });
        let title = if denied {
            "Cookie import failed"
        } else if stale {
            "Page changed"
        } else {
            "Cookies imported"
        };
        wait(|| {
            window
                .visible_dialog()
                .and_downcast::<adw::AlertDialog>()
                .is_some_and(|dialog| dialog.heading().as_deref() == Some(title))
        })
        .await;
        let dialog = window
            .visible_dialog()
            .and_downcast::<adw::AlertDialog>()
            .unwrap();
        assert_eq!(window.dialogs().n_items(), 1);
        if denied {
            assert!(dialog.body().contains("Full Disk Access"));
            assert!(dialog.has_response("retry"));
            assert_eq!(
                dialog
                    .extra_child()
                    .and_downcast::<gtk::Button>()
                    .unwrap()
                    .label()
                    .as_deref(),
                Some("Open Privacy Settings")
            );
            assert_eq!(cookies(&store).await.count(), 0);
            assert_eq!(reloads.get(), 0);
        } else if stale {
            assert_eq!(cookies(&store).await.count(), 0);
            assert_eq!(reloads.get(), 0);
        } else {
            assert!(!dialog.has_response("retry"));
            let imported = cookies(&store).await;
            assert_eq!(imported.count(), 1);
            let cookie = imported.objectAtIndex(0);
            assert_eq!(cookie.value().to_string(), "fixture");
            assert!(cookie.isHTTPOnly());
            assert!(cookie.isSecure());
            assert_eq!(reloads.get(), 1);
        }
        glib::timeout_future(Duration::from_millis(150)).await;
        click_response(&dialog, reply);
        assert_eq!(task.await.unwrap(), reply == "retry");
        wait(|| window.dialogs().n_items() == 0).await;
    }
    assert_eq!(
        std::fs::read(database).unwrap(),
        original,
        "source database changed"
    );

    // Keychain recovery is independent of file permission recovery.
    let dialog = adw::AlertDialog::new(
        Some("Cookie import failed"),
        Some("Synthetic Keychain denial"),
    );
    dialog.add_response("cancel", "Close");
    recovery_actions(
        &dialog,
        &import::ImportError::Keychain("Synthetic Keychain denial".into()),
    );
    assert_eq!(
        dialog
            .extra_child()
            .and_downcast::<gtk::Button>()
            .unwrap()
            .label()
            .as_deref(),
        Some("Open Keychain Access")
    );
    assert!(dialog.has_response("retry"));
    window.destroy();
    println!("MACOS_COOKIE_IMPORT_PERMISSIONS_OK");
}
