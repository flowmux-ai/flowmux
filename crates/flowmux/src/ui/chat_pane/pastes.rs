// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use anyhow::{anyhow, ensure, Context, Result};
use gtk::{gdk, gio, prelude::*};

#[derive(Clone)]
pub(super) enum Content {
    Text(String),
    Image(String),
}

pub(super) struct Pastes {
    pub root: gtk::ScrolledWindow,
    row: gtk::Box,
    items: RefCell<Vec<Paste>>,
    pub revision: Cell<u64>,
}

struct Paste {
    at: gtk::TextMark,
    content: Content,
    chip: gtk::Box,
}

impl Pastes {
    pub fn new() -> Self {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        row.set_margin_start(8);
        row.set_margin_end(8);
        row.set_margin_top(6);
        let root = gtk::ScrolledWindow::builder()
            .child(&row)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .visible(false)
            .build();
        Self {
            root,
            row,
            items: RefCell::default(),
            revision: Cell::new(0),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.borrow().is_empty()
    }

    pub fn push(
        &self,
        buffer: &gtk::TextBuffer,
        cursor: &gtk::TextIter,
        content: Content,
        remove: impl Fn(&gtk::TextMark) + 'static,
    ) {
        let at = buffer.create_mark(None, cursor, true);
        let name = match &content {
            Content::Text(text) => format!("Pasted text · {} lines", text.lines().count()),
            Content::Image(_) => "Pasted image".into(),
        };
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        chip.add_css_class("flowmux-chat-paste");
        if let Content::Image(id) = &content {
            chip.append(&super::transcript::attachment_thumbnail(id));
        }
        chip.append(&gtk::Label::new(Some(&name)));
        let button = gtk::Button::from_icon_name("window-close-symbolic");
        button.add_css_class("flat");
        button.update_property(&[gtk::accessible::Property::Label(&format!("Remove {name}"))]);
        button.set_tooltip_text(Some(match &content {
            Content::Text(_) => "Remove pasted text",
            Content::Image(_) => "Remove pasted image",
        }));
        let mark = at.clone();
        button.connect_clicked(move |_| remove(&mark));
        chip.append(&button);
        let mut items = self.items.borrow_mut();
        let index = items.partition_point(|item| buffer.iter_at_mark(&item.at) <= *cursor);
        let previous = index.checked_sub(1).map(|index| &items[index].chip);
        self.row.insert_child_after(&chip, previous);
        items.insert(index, Paste { at, content, chip });
        self.root.set_visible(true);
    }

    pub fn remove(&self, buffer: &gtk::TextBuffer, mark: &gtk::TextMark) -> bool {
        let mut items = self.items.borrow_mut();
        let Some(index) = items.iter().position(|item| item.at == *mark) else {
            return false;
        };
        let item = items.remove(index);
        buffer.delete_mark(&item.at);
        self.row.remove(&item.chip);
        self.root.set_visible(!items.is_empty());
        true
    }

    pub fn at_cursor(&self, buffer: &gtk::TextBuffer) -> Option<gtk::TextMark> {
        if buffer.has_selection() {
            return None;
        }
        self.items
            .borrow()
            .iter()
            .rfind(|item| buffer.iter_at_mark(&item.at).offset() == buffer.cursor_position())
            .map(|item| item.at.clone())
    }

    pub fn clear(&self, buffer: &gtk::TextBuffer) {
        self.revision.set(self.revision.get().wrapping_add(1));
        for item in self.items.borrow_mut().drain(..) {
            buffer.delete_mark(&item.at);
            self.row.remove(&item.chip);
        }
        self.root.set_visible(false);
    }

    pub fn snapshot(&self, buffer: &gtk::TextBuffer) -> Vec<(i32, Content)> {
        self.items
            .borrow()
            .iter()
            .map(|item| (buffer.iter_at_mark(&item.at).offset(), item.content.clone()))
            .collect()
    }

    pub fn images(&self) -> Vec<String> {
        self.items
            .borrow()
            .iter()
            .filter_map(|item| match &item.content {
                Content::Image(id) => Some(id.clone()),
                Content::Text(_) => None,
            })
            .collect()
    }

    pub fn text(&self, buffer: &gtk::TextBuffer) -> String {
        let mut text = String::new();
        let mut previous = buffer.start_iter();
        for item in self.items.borrow().iter() {
            let Content::Text(value) = &item.content else {
                continue;
            };
            let at = buffer.iter_at_mark(&item.at);
            text.push_str(&buffer.text(&previous, &at, false));
            text.push_str(value);
            previous = at;
        }
        text.push_str(&buffer.text(&previous, &buffer.end_iter(), false));
        text
    }
}

pub(super) async fn read_image(clipboard: &gdk::Clipboard) -> Result<String> {
    let texture = clipboard
        .read_texture_future()
        .await
        .context("Could not read the clipboard image.")?
        .context("The clipboard contains no image.")?;
    // Bound the PNG encoder and thumbnail decoder's working memory.
    ensure!(
        i64::from(texture.width()) * i64::from(texture.height()) <= 16_000_000,
        "A pasted image must be at most 16 megapixels."
    );
    gio::spawn_blocking(move || super::runtime::save_pasted_image(&texture.save_to_png_bytes()))
        .await
        .map_err(|_| anyhow!("Could not save the clipboard image."))?
}
