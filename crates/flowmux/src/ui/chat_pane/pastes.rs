// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};

use gtk::prelude::*;

pub(super) struct Pastes {
    pub root: gtk::ScrolledWindow,
    row: gtk::Box,
    items: RefCell<Vec<Paste>>,
    pub revision: Cell<u64>,
}

struct Paste {
    at: gtk::TextMark,
    text: String,
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
        text: String,
        remove: impl Fn(&gtk::TextMark) + 'static,
    ) {
        let at = buffer.create_mark(None, cursor, true);
        let lines = text.lines().count();
        let name = format!("Pasted text · {lines} lines");
        let chip = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        chip.add_css_class("flowmux-chat-paste");
        chip.append(&gtk::Label::new(Some(&name)));
        let button = gtk::Button::from_icon_name("window-close-symbolic");
        button.add_css_class("flat");
        button.update_property(&[gtk::accessible::Property::Label(&format!("Remove {name}"))]);
        button.set_tooltip_text(Some("Remove pasted text"));
        let mark = at.clone();
        button.connect_clicked(move |_| remove(&mark));
        chip.append(&button);
        let mut items = self.items.borrow_mut();
        let index = items.partition_point(|item| buffer.iter_at_mark(&item.at) <= *cursor);
        let previous = index.checked_sub(1).map(|index| &items[index].chip);
        self.row.insert_child_after(&chip, previous);
        items.insert(index, Paste { at, text, chip });
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

    pub fn snapshot(&self, buffer: &gtk::TextBuffer) -> Vec<(i32, String)> {
        self.items
            .borrow()
            .iter()
            .map(|item| (buffer.iter_at_mark(&item.at).offset(), item.text.clone()))
            .collect()
    }

    pub fn text(&self, buffer: &gtk::TextBuffer) -> String {
        let mut text = String::new();
        let mut previous = buffer.start_iter();
        for item in self.items.borrow().iter() {
            let at = buffer.iter_at_mark(&item.at);
            text.push_str(&buffer.text(&previous, &at, false));
            text.push_str(&item.text);
            previous = at;
        }
        text.push_str(&buffer.text(&previous, &buffer.end_iter(), false));
        text
    }
}
