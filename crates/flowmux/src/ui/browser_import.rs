// SPDX-License-Identifier: GPL-3.0-or-later
use flowmux_browser::{BookmarkRepository, BrowserProfile};
use flowmux_cookies::source::BrowserId;
use flowmux_ipc::protocol::BrowserImportOptions;
use gtk::{gio, glib, prelude::*};

#[cfg(target_os = "macos")]
#[path = "browser_import/macos.rs"]
mod backend;
#[cfg(target_os = "linux")]
#[path = "browser_import/linux.rs"]
mod backend;

#[derive(Clone)]
pub(crate) struct BrowserImport {
    pub profile: BrowserProfile,
    pub store: backend::Store,
}

impl BrowserImport {
    #[cfg(target_os = "macos")]
    pub fn new(profile: BrowserProfile, view: &objc2_web_kit::WKWebView) -> Self {
        Self {
            profile,
            store: backend::Store(unsafe {
                view.configuration().websiteDataStore().httpCookieStore()
            }),
        }
    }
    #[cfg(target_os = "linux")]
    pub fn new(profile: BrowserProfile, view: &webkit6::WebView) -> Self {
        use webkit6::prelude::*;
        Self {
            profile,
            store: backend::Store(view.network_session().unwrap().cookie_manager().unwrap()),
        }
    }

    pub async fn apply(
        &self,
        options: BrowserImportOptions,
        control: &super::browser_control::BrowserControl,
        revision: u64,
    ) -> Result<serde_json::Value, String> {
        if !options.bookmarks && !options.cookies {
            return Err("select bookmarks and/or cookies".into());
        }
        if options.domain.is_some() && !options.cookies {
            return Err("domain filter requires cookies".into());
        }
        if options.domain.as_ref().is_some_and(|d| {
            d.trim_start_matches('.').is_empty()
                || d.chars()
                    .any(|c| !c.is_ascii_alphanumeric() && c != '.' && c != '-')
        }) {
            return Err("enter a domain name without a URL or wildcard".into());
        }
        let repository =
            BookmarkRepository::for_profile(&self.profile).map_err(|e| e.to_string())?;
        let repo = repository.clone();
        let (bookmarks, batch) = gio::spawn_blocking(move || -> Result<_, String> {
            let browser = BrowserId::parse(&options.source).map_err(|e| e.to_string())?;
            let source =
                flowmux_cookies::profiles::resolve(browser, options.source_profile.as_deref())
                    .map_err(|e| e.to_string())?;
            let bookmarks = if options.bookmarks {
                repo.load().map_err(|e| e.to_string())?;
                flowmux_cookies::bookmarks::read(&source).map_err(|e| e.to_string())?
            } else {
                Vec::new()
            };
            let batch = if options.cookies {
                flowmux_cookies::profiles::cookies(&source, options.domain.as_deref())
                    .map_err(|e| e.to_string())?
            } else {
                flowmux_cookies::profiles::CookieBatch::default()
            };
            Ok((bookmarks, batch))
        })
        .await
        .map_err(|_| "browser import worker failed")??;
        control.check_revision(revision)?;
        let (added_cookies, existing_cookies) = self
            .store
            .insert_missing(&batch.cookies, control, revision)
            .await?;
        control
            .check_revision(revision)
            .map_err(|e| format!("{e}; {added_cookies} cookies already imported"))?;
        let bookmark_count = bookmarks.len();
        // Write on the main thread so Stop cannot arrive between the revision check and commit.
        let added_bookmarks = if bookmarks.is_empty() {
            Ok(0)
        } else {
            repository.merge(bookmarks)
        }
        .map_err(|e| format!("{e}; {added_cookies} cookies already imported"))?;
        Ok(
            serde_json::json!({"status":"complete", "profile":self.profile.slug(),
            "bookmarks_added":added_bookmarks, "bookmarks_existing":bookmark_count - added_bookmarks,
            "cookies_added":added_cookies, "cookies_existing":existing_cookies, "cookies_skipped_isolated":batch.skipped_isolated, "cookies_skipped_expired":batch.skipped_expired}),
        )
    }

    pub fn connect(&self, button: &gtk::Button, control: &super::browser_control::BrowserControl) {
        let importer = self.clone();
        let control = control.clone();
        button.connect_clicked(move |button| {
            let parent = button.root().and_downcast::<gtk::Window>();
            importer.show(parent.as_ref(), &control);
        });
    }

    fn show(&self, parent: Option<&gtk::Window>, control: &super::browser_control::BrowserControl) {
        let window = gtk::Window::builder()
            .title("Import browser data")
            .default_width(480)
            .modal(true)
            .build();
        window.set_transient_for(parent);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
        content.set_margin_top(18);
        content.set_margin_bottom(18);
        content.set_margin_start(18);
        content.set_margin_end(18);
        let explanation = gtk::Label::new(Some(&format!("Import into {}. Existing bookmarks and cookies are kept. Source browser data stays unchanged.", self.profile.display_name())));
        explanation.set_wrap(true);
        explanation.set_xalign(0.0);
        let sources = gtk::DropDown::from_strings(&[]);
        sources.set_tooltip_text(Some(
            "Source browser profile (only readable profiles are listed)",
        ));
        let bookmarks =
            gtk::CheckButton::with_label("Bookmarks (web links; folders are flattened)");
        bookmarks.set_active(true);
        let cookies =
            gtk::CheckButton::with_label("Cookies (may request access to the system key store)");
        let domain = gtk::Entry::builder()
            .placeholder_text("Cookie domain, e.g. example.com (optional)")
            .build();
        domain.set_tooltip_text(Some("Only this domain and its subdomains"));
        let status = gtk::Label::new(Some("Finding browser profiles…"));
        status.set_wrap(true);
        status.set_selectable(true);
        status.set_xalign(0.0);
        let import = gtk::Button::with_label("Import");
        import.add_css_class("suggested-action");
        import.set_sensitive(false);
        for widget in [
            explanation.upcast::<gtk::Widget>(),
            sources.clone().upcast(),
            bookmarks.clone().upcast(),
            cookies.clone().upcast(),
            domain.clone().upcast(),
            status.clone().upcast(),
            import.clone().upcast(),
        ] {
            content.append(&widget);
        }
        window.set_child(Some(&content));
        window.present();
        let importer = self.clone();
        let control = control.clone();
        glib::spawn_future_local(async move {
            let profiles = match gio::spawn_blocking(flowmux_cookies::profiles::discover).await {
                Ok(Ok(profiles)) if !profiles.is_empty() => profiles,
                Ok(Ok(_)) => {
                    status.set_text("No supported browser profiles found.");
                    return;
                }
                Ok(Err(error)) => {
                    status.set_text(&error.to_string());
                    return;
                }
                Err(_) => {
                    status.set_text("Could not find browser profiles.");
                    return;
                }
            };
            let labels: Vec<_> = profiles
                .iter()
                .map(|p| format!("{} — {}", p.browser.slug(), p.name))
                .collect();
            sources.set_model(Some(&gtk::StringList::new(
                &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            )));
            sources.set_selected(0);
            status.set_text("Choose the source profile and data to import.");
            import.set_sensitive(true);
            import.connect_clicked(move |button| {
                let Some(source) = profiles.get(sources.selected() as usize) else { return; };
                let options = BrowserImportOptions { source: source.browser.slug().into(), source_profile: Some(source.path.clone()), bookmarks: bookmarks.is_active(), cookies: cookies.is_active(), domain: if cookies.is_active() && !domain.text().trim().is_empty() { Some(domain.text().trim().into()) } else { None } };
                let (operation, revision) = match control.admit(None, true) { Ok(value) => value, Err(error) => { status.set_text(&error); return; } };
                let importer = importer.clone();
                let control = control.clone();
                let button = button.clone();
                let status = status.clone();
                button.set_sensitive(false);
                status.set_text("Importing… Respond to any system key store prompt to continue.");
                glib::spawn_future_local(async move {
                    let _operation = operation;
                    match importer.apply(options, &control, revision).await {
                        Ok(result) => status.set_text(&format!("Imported {} bookmarks and {} cookies. Kept {} existing bookmarks and {} existing cookies. Skipped {} partitioned/container cookies and {} expired cookies.", result["bookmarks_added"], result["cookies_added"], result["bookmarks_existing"], result["cookies_existing"], result["cookies_skipped_isolated"], result["cookies_skipped_expired"])),
                        Err(error) => status.set_text(&format!("Import failed: {error}")),
                    }
                    button.set_sensitive(true);
                });
            });
        });
    }
}
