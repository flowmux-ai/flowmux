// SPDX-License-Identifier: GPL-3.0-or-later

use flowmux_browser::{Bookmark, BookmarkError, BookmarkRepository, BrowserProfile};
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
        popover.add_css_class("browser-popover");
        popover.set_child(Some(&list));
        crate::ui::popover_pos::set_menu_popover(&button, &popover);

        let repository = BookmarkRepository::for_profile(profile).ok();
        let list = list.downgrade();
        popover.connect_show(move |popover| {
            if let Some(list) = list.upgrade() {
                request_bookmarks(
                    repository.clone(),
                    &list,
                    popover,
                    current.clone(),
                    open.clone(),
                    BookmarkRepository::load,
                );
            }
        });
        Self { button }
    }
}

fn request_bookmarks(
    repository: Option<BookmarkRepository>,
    list: &gtk::Box,
    popover: &gtk::Popover,
    current: CurrentBookmark,
    open: OpenBookmark,
    operation: impl FnOnce(&BookmarkRepository) -> Result<Vec<Bookmark>, BookmarkError> + Send + 'static,
) {
    if !list.is_sensitive() {
        return;
    }
    // Disable synchronously so another click or reopening cannot overlap this request.
    list.set_sensitive(false);
    if let Some(error) = list
        .last_child()
        .filter(|child| child.has_css_class("error"))
    {
        list.remove(&error);
    }
    let loading = gtk::Label::new(Some("Updating bookmarks…"));
    loading.add_css_class("dim-label");
    list.append(&loading);

    let list = list.downgrade();
    let popover = popover.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let result = match repository.clone() {
            Some(repository) => gio::spawn_blocking(move || operation(&repository))
                .await
                .map_err(|_| "bookmark worker failed".to_string())
                .and_then(|result| result.map_err(|error| error.to_string())),
            None => Err("browser data directory unavailable".into()),
        };
        let (Some(list), Some(popover)) = (list.upgrade(), popover.upgrade()) else {
            return;
        };
        list.set_sensitive(true);
        match result {
            Ok(bookmarks) => render(repository, &list, &popover, current, open, bookmarks),
            Err(error) => {
                loading.set_text(&format!("Could not update bookmarks: {error}"));
                loading.set_wrap(true);
                loading.remove_css_class("dim-label");
                loading.add_css_class("error");
            }
        }
    });
}

fn render(
    repository: Option<BookmarkRepository>,
    list: &gtk::Box,
    popover: &gtk::Popover,
    current: CurrentBookmark,
    open: OpenBookmark,
    bookmarks: Vec<Bookmark>,
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
            request_bookmarks(
                Some(repository),
                &list,
                &popover,
                current_for_reload.clone(),
                open.clone(),
                move |repository| repository.add(bookmark),
            );
        });
    }
    list.append(&add);
    list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    match bookmarks.as_slice() {
        [] => {
            let empty = gtk::Label::new(Some("No bookmarks yet"));
            empty.add_css_class("dim-label");
            list.append(&empty);
        }
        _ => {
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
                        let (Some(list), Some(popover)) = (list.upgrade(), popover.upgrade())
                        else {
                            return;
                        };
                        let url = url.clone();
                        request_bookmarks(
                            Some(repository.clone()),
                            &list,
                            &popover,
                            current.clone(),
                            open.clone(),
                            move |repository| repository.remove(&url),
                        );
                    });
                } else {
                    remove.set_sensitive(false);
                }
                row.append(&open_button);
                row.append(&remove);
                list.append(&row);
            }
        }
    }
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

#[cfg(test)]
pub(crate) async fn check_pending_requests() {
    use std::sync::{atomic::AtomicUsize, atomic::Ordering, Arc};
    use std::time::Duration;

    let repository = BookmarkRepository::for_profile(&BrowserProfile::Default).unwrap();
    let list = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let popover = gtk::Popover::new();
    popover.set_child(Some(&list));
    let current: CurrentBookmark = Rc::new(|| None);
    let open: OpenBookmark = Rc::new(|_| {});
    render(
        Some(repository.clone()),
        &list,
        &popover,
        current.clone(),
        open.clone(),
        Vec::new(),
    );
    let add = list.first_child().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    for _ in 0..2 {
        let calls = calls.clone();
        request_bookmarks(
            Some(repository.clone()),
            &list,
            &popover,
            current.clone(),
            open.clone(),
            move |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied).into())
            },
        );
        assert!(
            !add.is_sensitive(),
            "pending mutations must disable their controls"
        );
    }
    glib::future_with_timeout(Duration::from_secs(5), async {
        while !list.is_sensitive() {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "overlapping request was ignored"
    );
    assert_eq!(
        list.first_child(),
        Some(add.clone()),
        "failure kept the controls"
    );
    assert!(list.last_child().unwrap().has_css_class("error"));
    request_bookmarks(Some(repository), &list, &popover, current, open, |_| {
        Ok(Vec::new())
    });
    glib::future_with_timeout(Duration::from_secs(5), async {
        while !list.is_sensitive() {
            glib::timeout_future(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        !list.last_child().unwrap().has_css_class("error"),
        "retry cleared the error"
    );
    assert_ne!(
        list.first_child(),
        Some(add),
        "successful retry rendered its result"
    );
}
