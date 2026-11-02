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
    let profiles = match gio::spawn_blocking(import::discover_profiles).await {
        Ok(profiles) if !profiles.is_empty() => profiles,
        _ => {
            message(
                parent,
                "No browser profiles found",
                "Check that your browser has saved cookies and Flowmux can access its profile.",
            )
            .await;
            return;
        }
    };
    if !source_is_current(button) {
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
    import_selected(parent, button, host, selected, current_url, apply, reload).await;
}

async fn import_selected(
    parent: &gtk::Window,
    button: &gtk::Button,
    host: String,
    selected: import::Profile,
    current_url: Rc<dyn Fn() -> String>,
    apply: Apply,
    reload: Rc<dyn Fn()>,
) {
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
        _ = &mut cancel_rx => return,
        result = worker => result.map_err(|_| "Cookie import worker failed".to_string()).and_then(|result| result),
    };
    progress.set_can_close(false);
    progress.set_response_enabled("cancel", false);
    let (title, body) = async {
        let imported = match result {
            Ok(imported) => imported,
            Err(error) => return ("Cookie import failed", error),
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
        return;
    }
    // libadwaita 1.5 can ignore close() before the dialog's first presentation ticks.
    progress.set_heading(Some(title));
    progress.set_body(&body);
    progress.set_response_label("cancel", "Close");
    progress.set_response_enabled("cancel", true);
    progress.set_can_close(true);
    let _ = cancel_rx.await;
}

#[cfg(all(test, target_os = "linux"))]
mod tests;

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
