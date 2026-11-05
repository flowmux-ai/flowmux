// SPDX-License-Identifier: GPL-3.0-or-later
//! Web pages in browser tabs follow the system light/dark preference, while
//! native chrome follows the terminal theme through a forced libadwaita
//! color scheme.
//!
//! WebKitGTK has no per-view color-scheme API. It reads
//! `gtk-application-prefer-dark-theme` from the display's GtkSettings, and
//! libadwaita writes its forced scheme there. Each libadwaita write is
//! replaced with the system preference from the settings portal.

use gtk::{gio, glib, prelude::*};
use std::{cell::Cell, rc::Rc};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";
const COLOR_SCHEME: &str = "color-scheme";

/// Install before libadwaita's color scheme is forced, so its writes are
/// replaced inside its own update and do not trigger its warning.
pub(crate) fn install(display: &gtk::gdk::Display) {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| install_once(display));
}

fn install_once(display: &gtk::gdk::Display) {
    let settings = gtk::Settings::for_display(display);
    // ponytail: portal only; without one, pages stay light like libadwaita's
    // no-preference default.
    let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok();
    let system_dark = Rc::new(Cell::new(bus.as_ref().is_some_and(read_system_dark)));

    settings.connect_gtk_application_prefer_dark_theme_notify({
        let system_dark = system_dark.clone();
        move |settings| {
            if settings.is_gtk_application_prefer_dark_theme() != system_dark.get() {
                settings.set_gtk_application_prefer_dark_theme(system_dark.get());
            }
        }
    });
    if settings.is_gtk_application_prefer_dark_theme() != system_dark.get() {
        settings.set_gtk_application_prefer_dark_theme(system_dark.get());
    }

    let Some(bus) = bus else {
        return;
    };
    bus.signal_subscribe(
        Some(PORTAL),
        Some(SETTINGS),
        Some("SettingChanged"),
        Some(PORTAL_PATH),
        Some(APPEARANCE),
        gio::DBusSignalFlags::NONE,
        move |_, _, _, _, _, params| {
            let Some((_, key, value)) = params.get::<(String, String, glib::Variant)>() else {
                return;
            };
            if key == COLOR_SCHEME {
                system_dark.set(prefers_dark(&value));
                settings.set_gtk_application_prefer_dark_theme(system_dark.get());
            }
        },
    );
    // The subscription lives for the whole application.
    std::mem::forget(bus);
}

fn read_system_dark(bus: &gio::DBusConnection) -> bool {
    bus.call_sync(
        Some(PORTAL),
        PORTAL_PATH,
        SETTINGS,
        "ReadOne",
        Some(&(APPEARANCE, COLOR_SCHEME).to_variant()),
        Some(glib::VariantTy::new("(v)").unwrap()),
        gio::DBusCallFlags::NONE,
        1000,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|reply| reply.get::<(glib::Variant,)>())
    .is_some_and(|(value,)| prefers_dark(&value))
}

/// The portal's `color-scheme`: 0 no preference, 1 prefer dark, 2 prefer light.
fn prefers_dark(value: &glib::Variant) -> bool {
    value.get::<u32>() == Some(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_portal_prefer_dark_is_dark() {
        assert!(prefers_dark(&1u32.to_variant()));
        assert!(!prefers_dark(&0u32.to_variant()));
        assert!(!prefers_dark(&2u32.to_variant()));
        assert!(!prefers_dark(&"1".to_variant()));
    }
}
