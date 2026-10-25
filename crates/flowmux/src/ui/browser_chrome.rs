// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared browser chrome; WebView behavior stays in the platform backends.
use gtk::prelude::*;
use std::cell::Cell;

pub(super) fn address_is_editing(address: &gtk::Entry) -> bool {
    address
        .root()
        .and_then(|root| root.focus())
        .is_some_and(|focus| {
            focus == *address.upcast_ref::<gtk::Widget>() || focus.is_ancestor(address)
        })
}

#[cfg(test)]
#[path = "browser_chrome_tests.rs"]
pub(super) mod tests;

pub(super) struct BrowserChrome {
    pub bar: gtk::Box,
    pub back: gtk::Button,
    pub forward: gtk::Button,
    pub reload: gtk::Button,
    pub address: gtk::Entry,
    pub find: gtk::Button,
    pub zoom_out: gtk::Button,
    pub zoom_reset: gtk::Button,
    pub zoom_in: gtk::Button,
    pub inspector: gtk::Button,
    pub find_entry: gtk::SearchEntry,
    pub find_bar: gtk::Box,
}

pub(super) fn icon_button(icon: &str, tooltip: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.add_css_class("browser-icon");
    button.set_tooltip_text(Some(tooltip));
    button.set_valign(gtk::Align::Center);
    button
}

impl BrowserChrome {
    pub fn new(bookmarks: &gtk::MenuButton, downloads: &gtk::MenuButton, profile: &str) -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.add_css_class("browser-toolbar");
        let back = icon_button("go-previous-symbolic", "Back");
        let forward = icon_button("go-next-symbolic", "Forward");
        let reload = icon_button("view-refresh-symbolic", "Reload page");
        back.set_sensitive(false);
        forward.set_sensitive(false);
        let address = gtk::Entry::builder()
            .hexpand(true)
            .width_chars(1)
            .placeholder_text("Enter URL")
            .build();
        address.add_css_class("browser-address");
        address.set_icon_from_icon_name(
            gtk::EntryIconPosition::Primary,
            Some("web-browser-symbolic"),
        );
        address.set_icon_activatable(gtk::EntryIconPosition::Primary, false);
        let find = gtk::Button::with_label("Find in page");
        let inspector = gtk::Button::with_label("Developer tools");
        inspector.set_widget_name("flowmux-web-inspector");
        let zoom_out = icon_button("zoom-out-symbolic", "Zoom out");
        let zoom_in = icon_button("zoom-in-symbolic", "Zoom in");
        let zoom_reset = gtk::Button::with_label("100%");
        zoom_reset.set_tooltip_text(Some("Reset page zoom"));
        let zoom_badge = gtk::Button::with_label("100%");
        zoom_badge.add_css_class("browser-zoom-badge");
        zoom_badge.set_tooltip_text(Some("Reset page zoom"));
        zoom_badge.set_visible(false);
        {
            let badge = zoom_badge.downgrade();
            zoom_reset.connect_label_notify(move |reset| {
                if let Some(badge) = badge.upgrade() {
                    let label = reset.label().unwrap_or_default();
                    badge.set_label(&label);
                    badge.set_visible(label != "100%");
                }
            });
            let reset = zoom_reset.downgrade();
            zoom_badge.connect_clicked(move |_| {
                if let Some(reset) = reset.upgrade() {
                    reset.emit_clicked();
                }
            });
        }

        let more = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Browser tools")
            .valign(gtk::Align::Center)
            .build();
        more.add_css_class("browser-icon");
        let menu = gtk::Box::new(gtk::Orientation::Vertical, 6);
        menu.add_css_class("browser-menu");
        let overflow = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        overflow.set_visible(false);
        menu.append(&overflow);
        menu.append(&find);
        let zoom = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let zoom_title = gtk::Label::new(Some("Page zoom"));
        zoom_title.set_hexpand(true);
        zoom_title.set_xalign(0.0);
        zoom.append(&zoom_title);
        zoom.append(&zoom_out);
        zoom.append(&zoom_reset);
        zoom.append(&zoom_in);
        menu.append(&zoom);
        menu.append(&inspector);
        menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let profile = gtk::Label::new(Some(profile.trim_start_matches("Bookmarks\n")));
        profile.set_wrap(true);
        profile.set_max_width_chars(28);
        profile.set_xalign(0.0);
        profile.add_css_class("dim-label");
        menu.append(&profile);
        let popover = gtk::Popover::builder().child(&menu).build();
        popover.add_css_class("browser-popover");
        crate::ui::popover_pos::set_menu_popover(&more, &popover);
        for button in [&find, &inspector] {
            button.add_css_class("browser-menu-action");
            let popover = popover.downgrade();
            button.connect_clicked(move |_| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
            });
        }
        let bookmark_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bookmark_slot.append(bookmarks);
        let download_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        download_slot.append(downloads);
        for button in [bookmarks, downloads] {
            button.add_css_class("browser-icon");
            button.set_valign(gtk::Align::Center);
        }
        bar.append(&back);
        bar.append(&forward);
        bar.append(&reload);
        bar.append(&address);
        bar.append(&zoom_badge);
        bar.append(&bookmark_slot);
        bar.append(&download_slot);
        bar.append(&more);
        // Reuse the real controls so overflow keeps the same popovers and callbacks.
        let items = [
            (
                bookmarks.clone().upcast::<gtk::Widget>().downgrade(),
                bookmark_slot.downgrade(),
            ),
            (
                downloads.clone().upcast::<gtk::Widget>().downgrade(),
                download_slot.downgrade(),
            ),
        ];
        let overflow_weak = overflow.downgrade();
        let compact = Cell::new(false);
        bar.add_tick_callback(move |bar, _| {
            let next = bar.width() < 560;
            if compact.replace(next) != next {
                if let Some(overflow) = overflow_weak.upgrade() {
                    for (widget, slot) in &items {
                        if let (Some(widget), Some(slot)) = (widget.upgrade(), slot.upgrade()) {
                            if let Some(parent) = widget.parent().and_downcast::<gtk::Box>() {
                                parent.remove(&widget);
                            }
                            if next {
                                overflow.append(&widget);
                            } else {
                                slot.append(&widget);
                            }
                            slot.set_visible(!next);
                        }
                    }
                    overflow.set_visible(next);
                }
            }
            gtk::glib::ControlFlow::Continue
        });
        {
            let more = more.downgrade();
            downloads.connect_tooltip_text_notify(move |downloads| {
                if let Some(more) = more.upgrade() {
                    more.set_tooltip_text(Some(&format!(
                        "Browser tools · {}",
                        downloads.tooltip_text().unwrap_or_default()
                    )));
                }
            });
        }
        let find_entry = gtk::SearchEntry::builder()
            .placeholder_text("Find in page…")
            .hexpand(true)
            .visible(false)
            .build();
        let find_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        find_bar.add_css_class("browser-find");
        find_bar.set_visible(false);
        find_bar.append(&find_entry);
        let close = icon_button("window-close-symbolic", "Close find");
        find_bar.append(&close);
        {
            let bar = find_bar.downgrade();
            find_entry.connect_visible_notify(move |entry| {
                if let Some(bar) = bar.upgrade() {
                    bar.set_visible(entry.get_visible());
                }
            });
            let entry = find_entry.downgrade();
            close.connect_clicked(move |_| {
                if let Some(entry) = entry.upgrade() {
                    entry.emit_stop_search();
                }
            });
        }
        Self {
            bar,
            back,
            forward,
            reload,
            address,
            find,
            zoom_out,
            zoom_reset,
            zoom_in,
            inspector,
            find_entry,
            find_bar,
        }
    }
}
