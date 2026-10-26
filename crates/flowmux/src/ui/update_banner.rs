// SPDX-License-Identifier: GPL-3.0-or-later
//! Side-panel banner for self-update. Renders [`BannerState`] into an
//! `adw::Banner` pinned above the side panel footer, spawns the
//! periodic release check, and starts the install when the user asks.

use crate::update::origin::{InstallOrigin, UpdateGate};
use crate::update::{self, BannerState, Event};
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// Widget properties for a banner state:
/// `(title, button_label, revealed, progress_percent, ignore_visible)`.
/// `None` for the button label hides the button. Pure so the mapping
/// stays unit-testable without GTK.
fn banner_props(
    state: &BannerState,
    origin: InstallOrigin,
) -> (String, Option<&'static str>, bool, Option<u8>, bool) {
    use crate::update::Stage;
    match state {
        BannerState::Hidden | BannerState::Current | BannerState::Ignored(_) => {
            (String::new(), None, false, None, false)
        }
        BannerState::Available(v) => (
            format!("flowmux {v} is available"),
            Some(match update::origin::update_gate(origin) {
                UpdateGate::SourceBuild => "Update",
                #[cfg(target_os = "macos")]
                UpdateGate::MacosDmg => "Update",
                UpdateGate::ReleasePage => "Open release page (.deb)",
            }),
            true,
            None,
            true,
        ),
        BannerState::Running(Stage::Fetching, percent, v) => (
            format!("Updating to {v} — downloading {percent}%"),
            None,
            true,
            Some(*percent),
            false,
        ),
        BannerState::Running(Stage::Installing, percent, v) => (
            format!("Updating to {v} — installing {percent}%"),
            None,
            true,
            Some(*percent),
            false,
        ),
        BannerState::Done(v) => (
            format!("flowmux {v} is installed. Restart flowmux to use it."),
            Some("Dismiss"),
            true,
            None,
            false,
        ),
        BannerState::Failed(message, v) => (
            format!("Update to {v} failed: {message} (see ~/.cache/flowmux/update.log)"),
            Some(match update::origin::update_gate(origin) {
                UpdateGate::SourceBuild => "Retry",
                #[cfg(target_os = "macos")]
                UpdateGate::MacosDmg => "Retry",
                UpdateGate::ReleasePage => "Open release page (.deb)",
            }),
            true,
            None,
            false,
        ),
    }
}

#[derive(Clone)]
pub struct UpdateBanner {
    inner: Rc<UpdateBannerInner>,
}

struct UpdateBannerInner {
    root: gtk::Box,
    banner: adw::Banner,
    ignore_button: gtk::Button,
    progress: gtk::ProgressBar,
    state: Rc<RefCell<BannerState>>,
    tx: async_channel::Sender<Event>,
    tokio_handle: Option<tokio::runtime::Handle>,
    origin: InstallOrigin,
    check_task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for UpdateBannerInner {
    fn drop(&mut self) {
        self.tx.close();
        if let Some(task) = self.check_task.take() {
            task.abort();
        }
        // Install tasks own only a sender and must finish even after the UI closes.
    }
}

impl UpdateBanner {
    #[cfg(all(test, target_os = "macos", feature = "native-smoke"))]
    pub(crate) async fn check_native_dmg_update(app: &gtk::Application) {
        let banner = Self::new(None);
        assert_eq!(banner.inner.origin, InstallOrigin::MacosBundle);
        let window = gtk::Window::builder()
            .application(app)
            .child(banner.widget())
            .build();
        window.present();
        let version = update::check::Version(9, 8, 7);
        banner.dispatch(Event::Available(version));
        assert_eq!(
            banner.inner.banner.button_label().as_deref(),
            Some("Update")
        );
        banner.dispatch(Event::Start(version));
        banner.dispatch(Event::Progress(update::Stage::Installing, 75));
        assert_eq!(banner.inner.progress.text().as_deref(), Some("75%"));
        banner.dispatch(Event::Done(version));
        assert!(banner.inner.banner.title().contains("Restart flowmux"));
        gtk::glib::timeout_future(std::time::Duration::from_millis(100)).await;
        assert!(window.is_mapped());
        window.destroy();
        println!("MACOS_DMG_UPDATE_UI_OK");
    }

    /// Build the (hidden) banner and start the release check on
    /// `tokio_handle`. Without a handle — tests, degraded startup —
    /// the banner stays permanently hidden.
    pub fn new(tokio_handle: Option<tokio::runtime::Handle>) -> Self {
        let banner = adw::Banner::new("");
        banner.set_revealed(false);
        banner.set_hexpand(true);
        banner.set_widget_name("flowmux-update-banner");
        let ignore_button = gtk::Button::with_label("Not now");
        ignore_button.add_css_class("flat");
        ignore_button.set_tooltip_text(Some(
            "Hide this release prompt. You can update later in Options.",
        ));
        ignore_button.set_widget_name("flowmux-update-ignore-button");
        ignore_button.set_visible(false);
        ignore_button.set_valign(gtk::Align::Center);
        ignore_button.set_margin_end(6);
        let progress = gtk::ProgressBar::new();
        progress.set_hexpand(true);
        progress.set_margin_start(12);
        progress.set_margin_end(12);
        progress.set_margin_bottom(6);
        progress.set_show_text(true);
        progress.set_visible(false);

        let banner_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        banner_row.append(&banner);
        banner_row.append(&ignore_button);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&banner_row);
        root.append(&progress);

        let (tx, rx) = async_channel::unbounded::<Event>();
        let origin = update::origin::install_origin();
        let check_task = tokio_handle
            .as_ref()
            .map(|handle| handle.spawn(update::install::check_loop(tx.clone())));
        let this = Self {
            inner: Rc::new(UpdateBannerInner {
                root,
                banner: banner.clone(),
                ignore_button: ignore_button.clone(),
                progress,
                state: Rc::new(RefCell::new(
                    update::ignored_version()
                        .map(BannerState::Ignored)
                        .unwrap_or(BannerState::Hidden),
                )),
                tx,
                tokio_handle,
                origin,
                check_task,
            }),
        };

        let for_events = Rc::downgrade(&this.inner);
        gtk::glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = rx.recv().await {
                let Some(inner) = for_events.upgrade() else {
                    break;
                };
                Self { inner }.dispatch(event);
            }
        });

        let for_click = Rc::downgrade(&this.inner);
        banner.connect_button_clicked(move |banner| {
            let Some(inner) = for_click.upgrade() else {
                return;
            };
            let for_click = Self { inner };
            let actionable = for_click.inner.state.borrow().actionable_version();
            match actionable {
                Some(version) => {
                    if update::origin::update_gate(for_click.inner.origin)
                        != UpdateGate::ReleasePage
                    {
                        for_click.start_install(version);
                    } else if let Err(error) = open_release_page(for_click.inner.origin, version) {
                        tracing::warn!(%error, %version, "could not open release page");
                    }
                }
                // Done state: the button is "Dismiss". Keep the state —
                // BannerState::Done ignores re-announcements of the
                // installed release, so the banner stays dormant until
                // a strictly newer tag appears.
                None => banner.set_revealed(false),
            }
        });

        let for_ignore = Rc::downgrade(&this.inner);
        ignore_button.connect_clicked(move |_| {
            let Some(inner) = for_ignore.upgrade() else {
                return;
            };
            let for_ignore = Self { inner };
            let version = match *for_ignore.inner.state.borrow() {
                BannerState::Available(version) => version,
                _ => return,
            };
            if let Err(error) = update::persist_ignored_version(version) {
                tracing::warn!(%error, %version, "failed to persist ignored update version");
            }
            for_ignore.dispatch(Event::Ignored(version));
        });

        this
    }

    /// Run on the native GTK main thread with isolated state; no network or installer.
    #[cfg(all(test, feature = "native-smoke"))]
    pub(crate) async fn check_native_lifetime(app: &gtk::Application) {
        use crate::update::{check::Version, Stage};
        use std::time::Duration;

        async fn wait_for(description: &str, mut ready: impl FnMut() -> bool) {
            gtk::glib::future_with_timeout(Duration::from_secs(10), async {
                while !ready() {
                    gtk::glib::timeout_future(Duration::from_millis(20)).await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("update banner lifetime timed out: {description}"));
        }

        let survivor = Self::new(None);
        let survivor_window = gtk::Window::builder()
            .application(app)
            .child(survivor.widget())
            .build();
        survivor_window.present();
        wait_for("surviving window mapped", || survivor_window.is_mapped()).await;
        for deliver_events in [false, true] {
            let banner = Self::new(None);
            let window = gtk::Window::builder()
                .application(app)
                .child(banner.widget())
                .build();
            window.present();
            wait_for("closing window mapped", || window.is_mapped()).await;
            let inner = Rc::downgrade(&banner.inner);
            let state = Rc::downgrade(&banner.inner.state);
            let root = banner.widget().downgrade();
            let button = banner.inner.banner.downgrade();
            let ignore = banner.inner.ignore_button.downgrade();
            // A background install/check may retain a sender after the UI is gone.
            let producer = banner.inner.tx.clone();
            if deliver_events {
                let version = Version(99, 0, 0);
                producer.try_send(Event::Start(version)).unwrap();
                producer
                    .try_send(Event::Progress(Stage::Installing, 42))
                    .unwrap();
                wait_for("receiver renders progress", || {
                    banner.state() == BannerState::Running(Stage::Installing, 42, version)
                })
                .await;
                assert_eq!(banner.inner.progress.text().as_deref(), Some("42%"));
            }
            window.close();
            window.set_child(gtk::Widget::NONE);
            drop(window);
            drop(banner);
            wait_for("root, state and signal widgets released", || {
                inner.upgrade().is_none()
                    && state.upgrade().is_none()
                    && root.upgrade().is_none()
                    && button.upgrade().is_none()
                    && ignore.upgrade().is_none()
            })
            .await;
            wait_for("receiver future ended", || producer.receiver_count() == 0).await;
            assert!(producer.is_closed());
            assert!(producer.try_send(Event::Done(Version(99, 0, 0))).is_err());
        }
        let version = Version(99, 0, 1);
        survivor.inner.tx.try_send(Event::Done(version)).unwrap();
        wait_for("surviving banner handles events", || {
            survivor.state() == BannerState::Done(version)
        })
        .await;
        assert!(survivor.inner.banner.is_revealed());
        survivor
            .inner
            .banner
            .emit_by_name::<()>("button-clicked", &[]);
        assert!(!survivor.inner.banner.is_revealed());
        assert_eq!(survivor.state(), BannerState::Done(version));
        let tx = survivor.inner.tx.clone();
        survivor_window.close();
        survivor_window.set_child(gtk::Widget::NONE);
        drop(survivor_window);
        drop(survivor);
        wait_for("surviving receiver also ends on close", || {
            tx.receiver_count() == 0
        })
        .await;
        println!("MACOS_NATIVE_UPDATE_BANNER_LIFETIME_OK");
    }

    pub fn widget(&self) -> &gtk::Box {
        &self.inner.root
    }

    pub(crate) fn state(&self) -> BannerState {
        self.inner.state.borrow().clone()
    }

    pub(crate) fn install_origin(&self) -> InstallOrigin {
        self.inner.origin
    }

    /// Run a release check immediately and report the resulting shared state
    /// back on the GTK main context. This is used by Options > Update while the
    /// periodic startup check continues to use the banner's event channel.
    pub(crate) fn check_now(
        &self,
        on_complete: Box<dyn FnOnce(Result<BannerState, String>)>,
    ) -> bool {
        let Some(handle) = &self.inner.tokio_handle else {
            return false;
        };
        let (result_tx, result_rx) = async_channel::bounded(1);
        handle.spawn(async move {
            let result = update::install::check_once()
                .await
                .map_err(|error| format!("{error:#}"));
            let _ = result_tx.send(result).await;
        });

        let this = Rc::downgrade(&self.inner);
        gtk::glib::MainContext::default().spawn_local(async move {
            let result = result_rx.recv().await;
            let Some(inner) = this.upgrade() else {
                on_complete(Err("update banner closed".into()));
                return;
            };
            let this = Self { inner };
            let result = match result {
                Ok(Ok(Some(version))) => {
                    this.dispatch(Event::Available(version));
                    Ok(this.state())
                }
                Ok(Ok(None)) => {
                    this.dispatch(Event::Current);
                    Ok(this.state())
                }
                Ok(Err(error)) => Err(error),
                Err(error) => Err(format!("release check stopped: {error}")),
            };
            on_complete(result);
        });
        true
    }

    /// Start `version` through the same progress channel used by the banner.
    /// Returns `false` when no runtime is available or that release is already
    /// installing/installed, allowing other UI entry points to avoid duplicates.
    pub(crate) fn start_install(&self, version: update::check::Version) -> bool {
        if update::origin::update_gate(self.inner.origin) == UpdateGate::ReleasePage {
            return false;
        }
        let Some(handle) = &self.inner.tokio_handle else {
            return false;
        };
        let next = self.state().apply(Event::Start(version));
        if next == *self.inner.state.borrow() {
            return false;
        }
        self.render(next);
        handle.spawn(update::install::run_install(version, self.inner.tx.clone()));
        true
    }

    fn dispatch(&self, event: Event) {
        let next = self.inner.state.borrow().clone().apply(event);
        self.render(next);
    }

    fn render(&self, next: BannerState) {
        let (title, button, revealed, progress_percent, ignore_visible) =
            banner_props(&next, self.inner.origin);
        self.inner.banner.set_title(&title);
        self.inner.banner.set_button_label(button);
        self.inner.banner.set_revealed(revealed);
        self.inner.ignore_button.set_visible(ignore_visible);
        self.inner.progress.set_visible(progress_percent.is_some());
        if let Some(percent) = progress_percent {
            self.inner.progress.set_fraction(f64::from(percent) / 100.0);
            self.inner.progress.set_text(Some(&format!("{percent}%")));
        } else {
            self.inner.progress.set_fraction(0.0);
            self.inner.progress.set_text(None);
        }
        *self.inner.state.borrow_mut() = next;
    }
}

pub(crate) fn open_release_page(
    origin: InstallOrigin,
    version: update::check::Version,
) -> Result<(), gtk::glib::Error> {
    update::install::record_release_page_decision(origin, version);
    gtk::gio::AppInfo::launch_default_for_uri(
        &update::origin::release_page_url(version),
        None::<&gtk::gio::AppLaunchContext>,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::{check::Version, Stage};

    const V: Version = Version(0, 8, 0);

    #[test]
    fn hidden_state_reveals_nothing() {
        assert_eq!(
            banner_props(&BannerState::Hidden, InstallOrigin::Source),
            (String::new(), None, false, None, false)
        );
    }

    #[test]
    fn current_state_reveals_nothing() {
        assert_eq!(
            banner_props(&BannerState::Current, InstallOrigin::Source),
            (String::new(), None, false, None, false)
        );
    }

    #[test]
    fn ignored_state_reveals_nothing() {
        assert_eq!(
            banner_props(&BannerState::Ignored(V), InstallOrigin::Source),
            (String::new(), None, false, None, false)
        );
    }

    #[test]
    fn available_offers_the_update_button() {
        let (title, button, revealed, progress, ignore) =
            banner_props(&BannerState::Available(V), InstallOrigin::Source);
        assert!(
            title.contains("0.8.0"),
            "title should name the release: {title}"
        );
        assert_eq!(button, Some("Update"));
        assert!(revealed);
        assert_eq!(progress, None);
        assert!(ignore);
    }

    #[test]
    fn packaged_install_offers_release_page_instead_of_source_update() {
        let (_, button, revealed, progress, _) =
            banner_props(&BannerState::Available(V), InstallOrigin::Deb);
        assert_eq!(button, Some("Open release page (.deb)"));
        assert!(revealed);
        assert_eq!(progress, None);
    }

    #[test]
    fn running_shows_the_stage_and_no_button() {
        for (stage, needle) in [
            (Stage::Fetching, "downloading"),
            (Stage::Installing, "installing"),
        ] {
            let (title, button, revealed, progress, ignore) =
                banner_props(&BannerState::Running(stage, 42, V), InstallOrigin::Source);
            assert!(title.contains(needle), "{title} should mention {needle}");
            assert!(title.contains("42%"), "{title} should show the percentage");
            assert_eq!(button, None, "no button while running");
            assert!(revealed);
            assert_eq!(progress, Some(42));
            assert!(!ignore);
        }
    }

    #[gtk::test]
    fn running_renders_a_determinate_labeled_progress_bar() {
        let banner = UpdateBanner::new(None);
        banner.render(BannerState::Running(Stage::Fetching, 42, V));
        assert!(banner.inner.progress.is_visible());
        assert!((banner.inner.progress.fraction() - 0.42).abs() < f64::EPSILON);
        assert_eq!(banner.inner.progress.text().as_deref(), Some("42%"));
    }

    #[test]
    fn done_announces_next_launch_and_dismisses() {
        let (title, button, revealed, progress, ignore) =
            banner_props(&BannerState::Done(V), InstallOrigin::Source);
        assert!(title.contains("Restart flowmux"), "{title}");
        assert_eq!(button, Some("Dismiss"));
        assert!(revealed);
        assert_eq!(progress, None);
        assert!(!ignore);
    }

    #[test]
    fn failed_offers_retry_and_points_at_the_log() {
        let (title, button, revealed, progress, ignore) = banner_props(
            &BannerState::Failed("git fetch exited with 128".into(), V),
            InstallOrigin::Source,
        );
        assert!(title.contains("git fetch exited with 128"), "{title}");
        assert!(
            title.contains("update.log"),
            "{title} should point at the log"
        );
        assert_eq!(button, Some("Retry"));
        assert!(revealed);
        assert_eq!(progress, None);
        assert!(!ignore);
    }
}
