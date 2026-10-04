// SPDX-License-Identifier: GPL-3.0-or-later

use flowmux_browser::{Bookmark, BookmarkRepository, BrowserProfile};
use gtk::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

type CurrentBookmark = Rc<dyn Fn() -> Option<Bookmark>>;
type OpenBookmark = Rc<dyn Fn(&str)>;

#[derive(Clone)]
pub struct BookmarkMenu {
    pub button: gtk::MenuButton,
}

impl BookmarkMenu {
    pub fn new(profile: &BrowserProfile, current: CurrentBookmark, open: OpenBookmark) -> Self {
        let button = gtk::MenuButton::builder()
            .icon_name("user-bookmarks-symbolic")
            .tooltip_text("Bookmarks")
            .build();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        list.set_margin_top(8);
        list.set_margin_bottom(8);
        list.set_margin_start(8);
        list.set_margin_end(8);
        list.set_size_request(320, -1);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&list));
        crate::ui::popover_pos::set_menu_popover(&button, &popover);

        let repository = BookmarkRepository::for_profile(profile).ok();
        request_reload(repository, &list, &popover, current, open);
        Self { button }
    }
}

fn request_reload(
    repository: Option<BookmarkRepository>,
    list: &gtk::Box,
    popover: &gtk::Popover,
    current: CurrentBookmark,
    open: OpenBookmark,
) {
    clear_box(list);
    let loading = gtk::Label::new(Some("Loading bookmarks…"));
    loading.add_css_class("dim-label");
    list.append(&loading);

    let list = list.downgrade();
    let popover = popover.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let result = match repository.clone() {
            Some(repository) => gio::spawn_blocking(move || repository.load())
                .await
                .map_err(|_| "bookmark worker failed".to_string())
                .and_then(|result| result.map_err(|error| error.to_string())),
            None => Err("browser data directory unavailable".into()),
        };
        let (Some(list), Some(popover)) = (list.upgrade(), popover.upgrade()) else {
            return;
        };
        render(repository, &list, &popover, current, open, result);
    });
}

fn render(
    repository: Option<BookmarkRepository>,
    list: &gtk::Box,
    popover: &gtk::Popover,
    current: CurrentBookmark,
    open: OpenBookmark,
    result: Result<Vec<Bookmark>, String>,
) {
    clear_box(list);
    let add = gtk::Button::with_label("Bookmark this page");
    add.set_halign(gtk::Align::Fill);
    add.set_sensitive(repository.is_some());
    {
        let repository = repository.clone();
        let list = list.downgrade();
        let popover = popover.downgrade();
        let current_for_click = current.clone();
        let current_for_reload = current.clone();
        let open = open.clone();
        add.connect_clicked(move |_| {
            let Some(bookmark) = current_for_click() else {
                return;
            };
            if bookmark.url.is_empty() || bookmark.url == "about:blank" {
                return;
            }
            let (Some(repository), Some(list), Some(popover)) =
                (repository.clone(), list.upgrade(), popover.upgrade())
            else {
                return;
            };
            let repository_for_worker = repository.clone();
            let list = list.downgrade();
            let popover = popover.downgrade();
            let current = current_for_reload.clone();
            let open = open.clone();
            glib::MainContext::default().spawn_local(async move {
                let _ = gio::spawn_blocking(move || repository_for_worker.add(bookmark)).await;
                if let (Some(list), Some(popover)) = (list.upgrade(), popover.upgrade()) {
                    request_reload(Some(repository), &list, &popover, current, open);
                }
            });
        });
    }
    list.append(&add);
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    match result {
        Ok(bookmarks) if bookmarks.is_empty() => {
            let empty = gtk::Label::new(Some("No bookmarks yet"));
            empty.add_css_class("dim-label");
            list.append(&empty);
        }
        Ok(bookmarks) => {
            for bookmark in bookmarks {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let open_button = gtk::Button::new();
                open_button.set_hexpand(true);
                open_button.set_halign(gtk::Align::Fill);
                let labels = gtk::Box::new(gtk::Orientation::Vertical, 2);
                let title = gtk::Label::new(Some(&bookmark.title));
                title.set_xalign(0.0);
                title.set_ellipsize(gtk::pango::EllipsizeMode::End);
                let url = gtk::Label::new(Some(&bookmark.url));
                url.set_xalign(0.0);
                url.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                url.add_css_class("dim-label");
                labels.append(&title);
                labels.append(&url);
                open_button.set_child(Some(&labels));
                {
                    let url = bookmark.url.clone();
                    let open = open.clone();
                    let popover = popover.downgrade();
                    open_button.connect_clicked(move |_| {
                        open(&url);
                        if let Some(popover) = popover.upgrade() {
                            popover.popdown();
                        }
                    });
                }
                let remove = gtk::Button::from_icon_name("edit-delete-symbolic");
                remove.set_tooltip_text(Some("Remove bookmark"));
                remove.add_css_class("flat");
                if let Some(repository) = repository.clone() {
                    let url = bookmark.url;
                    let list = list.downgrade();
                    let popover = popover.downgrade();
                    let current = current.clone();
                    let open = open.clone();
                    remove.connect_clicked(move |_| {
                        let repository_for_worker = repository.clone();
                        let repository_for_reload = repository.clone();
                        let url = url.clone();
                        let list = list.clone();
                        let popover = popover.clone();
                        let current = current.clone();
                        let open = open.clone();
                        glib::MainContext::default().spawn_local(async move {
                            let _ = gio::spawn_blocking(move || repository_for_worker.remove(&url))
                                .await;
                            if let (Some(list), Some(popover)) = (list.upgrade(), popover.upgrade())
                            {
                                request_reload(
                                    Some(repository_for_reload),
                                    &list,
                                    &popover,
                                    current,
                                    open,
                                );
                            }
                        });
                    });
                } else {
                    remove.set_sensitive(false);
                }
                row.append(&open_button);
                row.append(&remove);
                list.append(&row);
            }
        }
        Err(error) => {
            let error = gtk::Label::new(Some(&error));
            error.set_wrap(true);
            error.add_css_class("error");
            list.append(&error);
        }
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}
