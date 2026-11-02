// SPDX-License-Identifier: GPL-3.0-or-later
use adw::prelude::*;
use flowmux_cookies::{import, Cookie};
use gtk::{gio, glib};
use std::{future::Future, pin::Pin, rc::Rc};

type Apply = Rc<dyn Fn(Vec<Cookie>) -> Pin<Box<dyn Future<Output = Result<usize, String>>>>>;

pub(super) fn connect(
    button: &gtk::Button,
    profile: String,
    persistent: bool,
    current_url: Rc<dyn Fn() -> String>,
    apply: Apply,
    reload: Rc<dyn Fn()>,
) {
    button.connect_clicked(move |button| {
        if !source_is_current(button) {
            return;
        }
        let Some(parent) = button.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let profile = profile.clone();
        let current_url = current_url.clone();
        let apply = apply.clone();
        let reload = reload.clone();
        let button = button.clone();
        button.set_sensitive(false);
        glib::MainContext::default().spawn_local(async move {
            let _native = crate::ui::browser_pane::suspend_native_browser_views_for_window(&parent);
            run(
                &parent,
                &button,
                &profile,
                persistent,
                current_url,
                apply,
                reload,
            )
            .await;
            button.set_sensitive(true);
        });
    });
}

fn source_is_current(button: &gtk::Button) -> bool {
    if !button.is_mapped() {
        return false;
    }
    let mut child = button.clone().upcast::<gtk::Widget>();
    while let Some(parent) = child.parent() {
        // Crossfades keep the previous tab mapped after the active tab changes.
        if let Some(stack) = parent.downcast_ref::<gtk::Stack>() {
            if stack.visible_child().as_ref() != Some(&child) {
                return false;
            }
        }
        child = parent;
    }
    true
}

async fn message(parent: &gtk::Window, title: &str, body: &str) {
    if !parent.is_visible() {
        return;
    }
    let dialog = adw::AlertDialog::new(Some(title), Some(body));
    dialog.add_response("close", "Close");
    dialog.set_close_response("close");
    let response = response(&dialog);
    dialog.present(Some(parent));
    let _ = response.await;
}

#[cfg(target_os = "macos")]
fn recovery_actions(dialog: &adw::AlertDialog, error: &import::ImportError) {
    let (label, arguments): (&str, &'static [&str]) = match error {
        import::ImportError::ProfileAccess => (
            "Open Privacy Settings",
            &["x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"],
        ),
        import::ImportError::Keychain(_) => {
            ("Open Keychain Access", &["-b", "com.apple.keychainaccess"])
        }
        import::ImportError::Other(_) => return,
    };
    dialog.add_response("retry", "Retry");
    let settings = gtk::Button::with_label(label);
    let weak_dialog = dialog.downgrade();
    settings.connect_clicked(move |_| {
        let mut command = vec![std::ffi::OsStr::new("/usr/bin/open")];
        command.extend(arguments.iter().map(std::ffi::OsStr::new));
        let process = gio::Subprocess::newv(&command, gio::SubprocessFlags::NONE);
        let weak_dialog = weak_dialog.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = match process {
                Ok(process) => process.wait_check_future().await,
                Err(error) => Err(error),
            };
            if let (Err(_), Some(dialog)) = (result, weak_dialog.upgrade()) {
                dialog.set_body(&format!("{}\n\nCould not open {label}. Open it from the Apple menu or Applications, then return here and Retry.", dialog.body()));
            }
        });
    });
    dialog.set_extra_child(Some(&settings));
}

fn response(dialog: &adw::AlertDialog) -> tokio::sync::oneshot::Receiver<String> {
    // Also finish when the parent is destroyed without emitting a dialog response.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Rc::new(std::cell::RefCell::new(Some(tx)));
    let closed_tx = tx.clone();
    dialog.connect_unrealize(move |dialog| {
        if let Some(tx) = closed_tx.borrow_mut().take() {
            let _ = tx.send(dialog.close_response().to_string());
        }
    });
    dialog.connect_response(None, move |_, response| {
        if let Some(tx) = tx.borrow_mut().take() {
            let _ = tx.send(response.to_string());
        }
    });
    rx
}

async fn run(
    parent: &gtk::Window,
    button: &gtk::Button,
    profile: &str,
    persistent: bool,
    current_url: Rc<dyn Fn() -> String>,
    apply: Apply,
    reload: Rc<dyn Fn()>,
) {
    let host = match import::site_host(&current_url()) {
        Ok(host) => host,
        Err(error) => {
            message(parent, "Import cookies", &error).await;
            return;
        }
    };
    let found = gio::spawn_blocking(import::discover_profiles)
        .await
        .unwrap_or_default();
    #[cfg(target_os = "macos")]
    let found = {
        let mut found = found;
        while found.access_denied {
            if !source_is_current(button) {
                return;
            }
            let error = import::ImportError::ProfileAccess;
            let dialog = adw::AlertDialog::new(
                Some("Browser profile access required"),
                Some(&error.to_string()),
            );
            dialog.add_response("cancel", "Cancel");
            dialog.set_close_response("cancel");
            recovery_actions(&dialog, &error);
            if !found.profiles.is_empty() {
                dialog.add_response("continue", "Use Available Profiles");
            }
            let choice = response(&dialog);
            dialog.present(Some(parent));
            match choice.await.as_deref() {
                Ok("retry") if source_is_current(button) => {
                    found = gio::spawn_blocking(import::discover_profiles)
                        .await
                        .unwrap_or_default();
                }
                Ok("continue") if source_is_current(button) => break,
                _ => return,
            }
        }
        found
    };
    if !source_is_current(button) {
        return;
    }
    let profiles = found.profiles;
    if profiles.is_empty() {
        message(
            parent,
            "No browser profiles found",
            "Check that your browser has saved cookies and Flowmux can access its profile.",
        )
        .await;
        return;
    }
    let choices: Vec<&str> = profiles
        .iter()
        .map(|profile| profile.name.as_str())
        .collect();
    let dropdown = gtk::DropDown::from_strings(&choices);
    dropdown.set_widget_name("flowmux-import-source");
    let source = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let label = gtk::Label::new(Some("Import from"));
    label.set_xalign(0.0);
    source.append(&label);
    source.append(&dropdown);
    let persistence = if persistent {
        ""
    } else {
        "\nCookies will be lost when this temporary session ends."
    };
    let dialog = adw::AlertDialog::new(Some("Import cookies?"), Some(&format!(
        "Site: {host}\nFlowmux profile: {profile}\n\nMatching cookies will be replaced across tabs in this profile.{persistence}\nYour system may ask for keyring access."
    )));
    #[cfg(target_os = "macos")]
    dialog.set_body(&format!("Site: {host}\nFlowmux profile: {profile}\n\nMatching cookies will be replaced across tabs in this profile.{persistence}\n\nFlowmux needs read access to the selected browser profile. For encrypted cookies, macOS may ask to unlock its Keychain and allow Flowmux to read the browser's Safe Storage key. Choose Allow and enter your keychain password only in the macOS prompt."));
    dialog.set_extra_child(Some(&source));
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("import", "Import");
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("cancel"));
    dialog.set_response_appearance("import", adw::ResponseAppearance::Suggested);
    let choice = response(&dialog);
    dialog.present(Some(parent));
    if choice.await.as_deref() != Ok("import") || !source_is_current(button) {
        return;
    }
    let Some(selected) = profiles.get(dropdown.selected() as usize).cloned() else {
        return;
    };
    if import::site_host(&current_url()).as_deref() != Ok(&host) {
        message(
            parent,
            "Page changed",
            "Reopen Import cookies on the intended site.",
        )
        .await;
        return;
    }
    while import_selected(
        parent,
        button,
        host.clone(),
        selected.clone(),
        current_url.clone(),
        apply.clone(),
        reload.clone(),
    )
    .await
    {
        if !source_is_current(button) || import::site_host(&current_url()).as_deref() != Ok(&host) {
            message(
                parent,
                "Page changed",
                "Nothing more was imported. Reopen Import cookies on the intended site.",
            )
            .await;
            return;
        }
    }
}

async fn import_selected(
    parent: &gtk::Window,
    button: &gtk::Button,
    host: String,
    selected: import::Profile,
    current_url: Rc<dyn Fn() -> String>,
    apply: Apply,
    reload: Rc<dyn Fn()>,
) -> bool {
    let progress = adw::AlertDialog::new(
        Some("Importing cookies…"),
        Some("If prompted, approve or unlock the system keyring."),
    );
    progress.add_response("cancel", "Cancel");
    progress.set_close_response("cancel");
    let mut cancel_rx = response(&progress);
    progress.present(Some(parent));
    let host_for_worker = host.clone();
    let worker = gio::spawn_blocking(move || import::read(&selected, &host_for_worker));
    let result = tokio::select! {
        biased;
        _ = &mut cancel_rx => return false,
        result = worker => result.map_err(|_| import::ImportError::from("Cookie import worker failed")).and_then(|result| result),
    };
    progress.set_can_close(false);
    progress.set_response_enabled("cancel", false);
    #[cfg(target_os = "macos")]
    if let Err(error) = &result {
        recovery_actions(&progress, error);
    }
    let (title, body) = async {
        let imported = match result {
            Ok(imported) => imported,
            Err(error) => return ("Cookie import failed", error.to_string()),
        };
        if !source_is_current(button) || import::site_host(&current_url()).as_deref() != Ok(&host) {
            return (
                "Page changed",
                "Nothing was imported. Reopen Import cookies on the intended site.".to_string(),
            );
        }
        if imported.cookies.is_empty() {
            return (
                "No matching cookies",
                format!("No cookies available for {host} in this profile.\nSkipped: {} (expired or unsupported).", imported.skipped),
            );
        }
        match apply(imported.cookies).await {
            Ok(count) => {
                if source_is_current(button) && import::site_host(&current_url()).as_deref() == Ok(&host) {
                    reload();
                }
                (
                    "Cookies imported",
                    format!(
                        "Imported: {count} · Skipped: {}\n\nSome sites may still require sign-in.",
                        imported.skipped
                    ),
                )
            }
            Err(error) => ("Cookie import incomplete", error),
        }
    }
    .await;
    if !parent.is_visible() {
        return false;
    }
    // libadwaita 1.5 can ignore close() before the dialog's first presentation ticks.
    progress.set_heading(Some(title));
    progress.set_body(&body);
    progress.set_response_label("cancel", "Close");
    progress.set_response_enabled("cancel", true);
    progress.set_can_close(true);
    cancel_rx.await.as_deref() == Ok("retry")
}

#[cfg(all(test, target_os = "linux"))]
mod tests;

#[cfg(all(test, target_os = "macos", feature = "native-smoke"))]
pub(crate) mod macos_smoke;

#[cfg(target_os = "linux")]
pub(super) async fn apply_webkit(
    manager: webkit6::CookieManager,
    cookies: Vec<Cookie>,
) -> Result<usize, String> {
    let native = cookies
        .iter()
        .map(|cookie| {
            let url = format!(
                "{}://{}/",
                if cookie.secure { "https" } else { "http" },
                cookie.host.trim_start_matches('.')
            );
            let origin = glib::Uri::parse(&url, glib::UriFlags::NONE)
                .map_err(|_| "Invalid cookie origin")?;
            webkit6::soup::Cookie::parse(&import::cookie_header(cookie)?, Some(&origin))
                .ok_or_else(|| "WebKit rejected a cookie; nothing was imported".into())
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (index, cookie) in native.iter().enumerate() {
        manager.add_cookie_future(cookie).await.map_err(|_| {
            format!(
                "WebKit could not apply a cookie. {index} cookies were applied before the failure."
            )
        })?;
    }
    Ok(native.len())
}

#[cfg(target_os = "macos")]
pub(super) async fn apply_macos(
    store: objc2::rc::Retained<objc2_web_kit::WKHTTPCookieStore>,
    cookies: Vec<Cookie>,
) -> Result<usize, String> {
    use objc2_foundation::{NSDictionary, NSHTTPCookie, NSString, NSURL};
    use std::cell::RefCell;
    let native = cookies
        .iter()
        .map(|cookie| {
            let url = format!(
                "{}://{}/",
                if cookie.secure { "https" } else { "http" },
                cookie.host.trim_start_matches('.')
            );
            let url =
                NSURL::URLWithString(&NSString::from_str(&url)).ok_or("Invalid cookie origin")?;
            let name = NSString::from_str("Set-Cookie");
            let header = NSString::from_str(&import::cookie_header(cookie)?);
            let fields = NSDictionary::from_slices(&[&*name], &[&*header]);
            let parsed = NSHTTPCookie::cookiesWithResponseHeaderFields_forURL(&fields, &url);
            if parsed.count() != 1 {
                return Err("WebKit rejected a cookie; nothing was imported".into());
            }
            Ok(parsed.objectAtIndex(0))
        })
        .collect::<Result<Vec<_>, String>>()?;
    for (index, cookie) in native.iter().enumerate() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let tx = RefCell::new(Some(tx));
        let callback = block2::RcBlock::new(move || {
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(());
            }
        });
        unsafe {
            store.setCookie_completionHandler(cookie, Some(&callback));
        }
        rx.await
            .map_err(|_| format!("Cookie import stopped after {index} cookies"))?;
    }
    Ok(native.len())
}
