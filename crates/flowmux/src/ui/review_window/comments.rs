// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_state::review_drafts::{Draft, DraftStore};
use flowmux_vcs::review::notes::{self, Note};

#[cfg(test)]
mod scenarios;

pub(super) struct Comments {
    pub composer: gtk::Box,
    writer: gtk::TextView,
    hint: gtk::Label,
    save: gtk::Button,
    cancel: gtk::Button,
    pub menu: gtk::MenuButton,
    navigation: gtk::Box,
    draft: RefCell<Draft>,
    anchor: RefCell<Option<Note>>,
    editing: Cell<bool>,
    pub(super) busy: Cell<bool>,
    pub(super) ready: Cell<bool>,
    initialized: Cell<bool>,
    syncing: Cell<bool>,
    edit_conflict: Cell<bool>,
    stale: RefCell<Vec<String>>,
}

fn text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .into()
}

fn style_card(card: &gtk::Box) {
    // TextView's built-in click handler grabs focus even for events from an
    // anchored child. Let the card's children handle pointer input, then stop
    // it before it reaches the surrounding read-only diff.
    let pointer = gtk::EventControllerLegacy::new();
    pointer.set_propagation_phase(gtk::PropagationPhase::Bubble);
    pointer.connect_event(|_, event| {
        if matches!(
            event.event_type(),
            gtk::gdk::EventType::ButtonPress
                | gtk::gdk::EventType::ButtonRelease
                | gtk::gdk::EventType::MotionNotify
                | gtk::gdk::EventType::TouchBegin
                | gtk::gdk::EventType::TouchUpdate
                | gtk::gdk::EventType::TouchEnd
        ) {
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    card.add_controller(pointer);
    card.add_css_class("card");
    card.add_css_class("review-comment");
    let style = gtk::CssProvider::new();
    style.load_from_string(
        ".review-comment { padding: 10px; font-family: sans-serif; font-size: 12px; }",
    );
    #[allow(deprecated)]
    card.style_context()
        .add_provider(&style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 2);
}

impl Comments {
    pub fn new() -> Self {
        let composer = gtk::Box::new(gtk::Orientation::Vertical, 8);
        style_card(&composer);
        margins(&composer, 8);
        let hint = gtk::Label::builder().xalign(0.0).wrap(true).build();
        hint.add_css_class("caption");
        composer.append(&hint);
        let writer = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .build();
        writer.set_widget_name("flowmux-review-comment");
        writer.update_property(&[gtk::accessible::Property::Label("Review comment")]);
        composer.append(
            &gtk::ScrolledWindow::builder()
                .child(&writer)
                .min_content_height(85)
                .max_content_height(180)
                .propagate_natural_height(true)
                .build(),
        );
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let save = gtk::Button::with_label("Add comment");
        save.add_css_class("suggested-action");
        let cancel = gtk::Button::with_label("Cancel");
        actions.append(&save);
        actions.append(&cancel);
        composer.append(&actions);
        let menu = gtk::MenuButton::builder().label("Comments · 0").build();
        let navigation = gtk::Box::new(gtk::Orientation::Vertical, 4);
        margins(&navigation, 8);
        let popover = gtk::Popover::new();
        popover.set_child(Some(
            &gtk::ScrolledWindow::builder()
                .child(&navigation)
                .max_content_height(420)
                .min_content_width(320)
                .propagate_natural_height(true)
                .build(),
        ));
        crate::ui::popover_pos::set_menu_popover(&menu, &popover);
        Self {
            composer,
            writer,
            hint,
            save,
            cancel,
            menu,
            navigation,
            draft: RefCell::new(Draft::default()),
            anchor: RefCell::new(None),
            editing: Cell::new(false),
            busy: Cell::new(false),
            ready: Cell::new(false),
            initialized: Cell::new(false),
            syncing: Cell::new(false),
            edit_conflict: Cell::new(false),
            stale: RefCell::new(Vec::new()),
        }
    }
}

impl ReviewWindow {
    #[cfg(test)]
    pub(super) async fn smoke_save_file_comment(self: &Rc<Self>, value: &str) {
        self.smoke_wait_for_comments().await;
        self.begin_comment(true);
        self.smoke_set_draft(value);
        self.comments.save.emit_clicked();
        self.smoke_wait_for_comments().await;
        assert_eq!(self.status.text(), "Comment saved");
    }

    #[cfg(test)]
    pub(super) async fn smoke_wait_for_comments(&self) {
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while self.comments.busy.get() || !self.comments.ready.get() {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    #[cfg(test)]
    pub(super) fn smoke_remove_scope_comments(&self) {
        self.comments.menu.popup();
        self.comments
            .navigation
            .last_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
    }

    #[cfg(all(test, target_os = "macos"))]
    pub(crate) async fn smoke_prepare_unchanged_edit(self: &Rc<Self>, value: &str) {
        self.begin_comment(true);
        self.comments.writer.buffer().set_text(value);
        self.comments.save.emit_clicked();
        glib::future_with_timeout(std::time::Duration::from_secs(10), async {
            while self.comments.busy.get() {
                glib::timeout_future(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let card = self.inline_widgets.borrow()[0].clone();
        card.last_child()
            .unwrap()
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        assert!(!self.has_unsaved_review());
    }

    #[cfg(test)]
    pub(crate) fn smoke_set_draft(&self, value: &str) {
        self.comments.writer.buffer().set_text(value);
    }

    pub fn has_unsaved_review(&self) -> bool {
        if self.comments.busy.get() || self.comments.edit_conflict.get() {
            return true;
        }
        let value = text(&self.comments.writer);
        let anchor = self.comments.anchor.borrow();
        match anchor.as_ref().filter(|_| self.comments.editing.get()) {
            Some(note) => value.trim() != note.text.trim(),
            None => !value.trim().is_empty(),
        }
    }

    pub fn delivery_message(&self, message: &str) {
        self.status.set_text(message);
    }

    pub(super) fn update_delivery_controls(&self) {
        let enabled = self.comments.ready.get() && !self.comments.busy.get();
        self.comments.save.set_sensitive(enabled);
        self.comments
            .cancel
            .set_sensitive(!self.comments.busy.get());
        self.comments.writer.set_editable(!self.comments.busy.get());
        self.targets.menu.set_sensitive(
            enabled
                && self.snapshot.borrow().is_some()
                && self
                    .comments
                    .draft
                    .borrow()
                    .notes
                    .iter()
                    .any(|n| !n.resolved && n.scope == *self.history.scope.borrow()),
        );
    }

    pub(super) fn connect_comments(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.comments.save.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.save_comment();
            }
        });
        let weak = Rc::downgrade(self);
        self.comments.cancel.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.cancel_comment();
            }
        });
        let key = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        key.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if key == gtk::gdk::Key::Escape {
                this.cancel_comment();
                return glib::Propagation::Stop;
            }
            if key == gtk::gdk::Key::Return
                && modifiers.intersects(
                    gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::META_MASK,
                )
            {
                this.save_comment();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.comments.writer.add_controller(key);
        // Keep recovery available even when the first storage read fails.
        self.render_comments();
        self.update_delivery_controls();
    }

    pub(super) fn init_comments(self: &Rc<Self>, _root: PathBuf) {
        self.load_comments();
        if !self.comments.initialized.replace(true) {
            let weak = Rc::downgrade(self);
            glib::timeout_add_local(std::time::Duration::from_millis(500), move || {
                let Some(this) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                this.sync_comments();
                glib::ControlFlow::Continue
            });
        }
    }

    fn sync_comments(self: &Rc<Self>) {
        if !self.root_widget.is_mapped()
            || !self.comments.ready.get()
            || self.comments.busy.get()
            || self.comments.syncing.get()
            || self.comments.menu.popover().is_some_and(|p| p.is_visible())
            || self.targets.menu.popover().is_some_and(|p| p.is_visible())
        {
            return;
        }
        self.comments.syncing.set(true);
        let revision = self.comments.draft.borrow().revision;
        let root = self.root.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result =
                gtk::gio::spawn_blocking(move || DraftStore::default_store()?.load(&root)).await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.comments.syncing.set(false);
            if this.comments.busy.get()
                || this.comments.draft.borrow().revision != revision
                || this.comments.menu.popover().is_some_and(|p| p.is_visible())
                || this.targets.menu.popover().is_some_and(|p| p.is_visible())
            {
                return;
            }
            if let Ok(Ok(draft)) = result {
                if draft.revision != revision {
                    this.apply_loaded_comments(draft);
                    this.update_delivery_controls();
                }
            }
        });
    }

    fn apply_loaded_comments(self: &Rc<Self>, mut draft: Draft) {
        for note in &mut draft.notes {
            if !matches!(note.scope, Scope::Commit(_)) {
                note.scope = Scope::WorkingTree;
            }
            // Resolve was replaced by Delete, including for legacy comments.
            note.resolved = false;
        }
        let anchor = self.comments.anchor.borrow().clone();
        if let Some(anchor) = anchor.filter(|_| self.comments.editing.get()) {
            let changed = self
                .comments
                .draft
                .borrow()
                .notes
                .iter()
                .find(|n| n.id == anchor.id)
                != draft.notes.iter().find(|n| n.id == anchor.id);
            if changed {
                if self.has_unsaved_review() {
                    self.comments.edit_conflict.set(true);
                    self.status.set_text("This comment changed in another pane. Your text is preserved; cancel this edit to use the updated comment.");
                } else {
                    self.clear_comment();
                }
            }
        }
        self.comments
            .stale
            .borrow_mut()
            .retain(|id| draft.notes.iter().any(|n| &n.id == id));
        *self.comments.draft.borrow_mut() = draft;
        self.comments.ready.set(true);
        self.render_comments();
    }

    fn load_comments(self: &Rc<Self>) {
        if self.comments.busy.replace(true) {
            return;
        }
        self.update_delivery_controls();
        let root = self.root.clone();
        let scope = self.history.scope.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let store = DraftStore::default_store()?;
                store.refresh_scope(&root, &store.load(&root)?, &scope)
            })
            .await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.comments.busy.set(false);
            match result {
                Ok(Ok(draft)) => this.apply_loaded_comments(draft),
                Ok(Err(error)) => this.status.set_text(&error),
                Err(_) => this
                    .status
                    .set_text("Cannot load comments. Open Comments and retry."),
            }
            this.update_delivery_controls();
        });
    }

    pub(super) fn begin_comment(self: &Rc<Self>, whole_file: bool) {
        if self.comments.busy.get() {
            return;
        }
        if self.comments.anchor.borrow().is_some() {
            self.focus_comment();
            return;
        }
        let snapshot = self.snapshot.borrow();
        let patch = self.patch.borrow();
        let (Some(snapshot), Some(patch)) = (snapshot.as_ref(), patch.as_ref()) else {
            return;
        };
        let Some(index) = self
            .visible_files
            .borrow()
            .get(self.selection.selected() as usize)
            .copied()
        else {
            return;
        };
        let range = if whole_file {
            None
        } else {
            let buffer = self.diff.buffer();
            let (start, end) = buffer.selection_bounds().unwrap_or_else(|| {
                let start = buffer.iter_at_offset(buffer.cursor_position());
                let mut end = start;
                end.forward_to_line_end();
                (start, end)
            });
            let map = self.display_lines.borrow();
            let first = map.get(start.line() as usize).copied().flatten();
            let last_line = (end.line()
                - i32::from(end.line_offset() == 0 && end.line() > start.line()))
            .max(start.line());
            let last = map.get(last_line as usize).copied().flatten();
            match (first, last) {
                (Some(a), Some(b)) => Some(a..b + 1),
                _ => {
                    self.status.set_text("Select a code line to comment on it.");
                    return;
                }
            }
        };
        let note = Note::new(
            uuid::Uuid::new_v4().to_string(),
            snapshot,
            &snapshot.files[index],
            patch,
            range,
            "draft",
        );
        match note {
            Ok(mut note) => {
                note.text.clear();
                self.comments.hint.set_text(&note.location);
                *self.comments.anchor.borrow_mut() = Some(note);
                self.comments.editing.set(false);
                self.comments.save.set_label("Add comment");
                self.show_page();
                self.comments.writer.grab_focus();
            }
            Err(error) => self.status.set_text(&error),
        }
    }

    fn cancel_comment(self: &Rc<Self>) {
        if self.comments.busy.get() {
            return;
        }
        self.diff.grab_focus();
        self.clear_comment();
        self.show_page();
        self.status.set_text("Comment cancelled");
    }

    pub(super) fn focus_comment(self: &Rc<Self>) {
        let note = self.comments.anchor.borrow().clone();
        if let Some(note) = note {
            if self.comments.writer.is_mapped() {
                let buffer = self.diff.buffer();
                for (row, raw) in self.display_lines.borrow().iter().enumerate() {
                    if raw.is_none() {
                        if let Some(iter) = buffer.iter_at_line(row as i32) {
                            if iter.child_anchor().is_some_and(|anchor| {
                                anchor
                                    .widgets()
                                    .contains(self.comments.composer.upcast_ref())
                            }) {
                                buffer.place_cursor(&iter);
                                self.scroll_to_cursor(false, 0.0);
                                break;
                            }
                        }
                    }
                }
                self.comments.writer.grab_focus();
            } else {
                self.jump_to_note(&note);
            }
        }
    }

    pub(super) fn focus_pending_comment(&self, note: &Note) {
        if self
            .comments
            .anchor
            .borrow()
            .as_ref()
            .is_some_and(|n| n.id == note.id)
        {
            self.comments.writer.grab_focus();
        }
    }

    pub(super) fn cancel_or_close(self: &Rc<Self>) {
        for menu in [&self.comments.menu, &self.targets.menu, &self.history.menu] {
            if menu.popover().is_some_and(|p| p.is_visible()) {
                menu.popdown();
                self.diff.grab_focus();
                return;
            }
        }
        if self.comments.anchor.borrow().is_some() {
            self.cancel_comment();
        } else {
            self.hide();
        }
    }

    pub(super) fn clear_comment(&self) {
        self.comments.writer.buffer().set_text("");
        self.comments.anchor.borrow_mut().take();
        self.comments.editing.set(false);
        self.comments.edit_conflict.set(false);
    }

    fn save_comment(self: &Rc<Self>) {
        if self.comments.busy.get() || !self.comments.ready.get() {
            return;
        }
        if self.comments.edit_conflict.get() {
            self.status.set_text("This comment changed in another pane. Your text is preserved; cancel this edit to use the updated comment.");
            return;
        }
        let Some(mut note) = self.comments.anchor.borrow().clone() else {
            return;
        };
        let value = text(&self.comments.writer);
        if value.trim().is_empty() {
            self.status.set_text("Write a comment first.");
            return;
        }
        note.text = value.trim().into();
        let mut draft = self.comments.draft.borrow().clone();
        if let Some(existing) = draft.notes.iter_mut().find(|n| n.id == note.id) {
            *existing = note;
        } else {
            draft.notes.push(note);
        }
        self.persist_comments(draft, true, "Comment saved");
    }

    fn persist_comments(self: &Rc<Self>, draft: Draft, clear: bool, message: &'static str) {
        if self.comments.busy.replace(true) {
            return;
        }
        self.update_delivery_controls();
        let root = self.root.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result =
                gtk::gio::spawn_blocking(move || DraftStore::default_store()?.save(&root, &draft))
                    .await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.comments.busy.set(false);
            match result {
                Ok(Ok(draft)) => {
                    *this.comments.draft.borrow_mut() = draft;
                    if clear {
                        if let Some(note) = this.comments.anchor.borrow().as_ref() {
                            if this.patch.borrow().as_ref().is_some_and(|patch| {
                                this.selected_path.borrow().as_ref() == Some(&note.path())
                                    && note.fingerprint == notes::fingerprint(patch)
                            }) {
                                this.comments.stale.borrow_mut().retain(|id| id != &note.id);
                            }
                        }
                        this.diff.grab_focus();
                        this.clear_comment();
                    }
                    this.render_comments();
                    this.status.set_text(message);
                }
                Ok(Err(error)) => this.status.set_text(&error),
                Err(_) => this
                    .status
                    .set_text("Could not save. Your comment is still here; retry."),
            }
            this.update_delivery_controls();
        });
    }

    pub(super) fn render_comments(self: &Rc<Self>) {
        while let Some(child) = self.comments.navigation.first_child() {
            self.comments.navigation.remove(&child);
        }
        let notes: Vec<_> = self
            .comments
            .draft
            .borrow()
            .notes
            .iter()
            .filter(|n| n.scope == *self.history.scope.borrow())
            .cloned()
            .collect();
        let count = notes.len();
        self.comments.menu.set_label(&format!("Comments · {count}"));
        self.targets.menu.set_label(&format!("Send · {count}"));
        for note in notes {
            let label = format!(
                "{}{} · {}\n{}",
                if note.resolved { "✓ " } else { "" },
                note.label(),
                note.location,
                note.text.lines().next().unwrap_or_default()
            );
            let button = gtk::Button::new();
            button.set_child(Some(
                &gtk::Label::builder()
                    .label(&label)
                    .xalign(0.0)
                    .max_width_chars(42)
                    .ellipsize(pango::EllipsizeMode::End)
                    .build(),
            ));
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.comments.menu.popdown();
                    this.jump_to_note(&note);
                }
            });
            self.comments.navigation.append(&button);
        }
        let reload = gtk::Button::with_label("Reload saved comments");
        let weak = Rc::downgrade(self);
        reload.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.comments.menu.popdown();
                this.load_comments();
            }
        });
        self.comments.navigation.append(&reload);
        let remove_all = gtk::Button::with_label("Remove all comments");
        remove_all.add_css_class("destructive-action");
        remove_all.set_sensitive(count > 0);
        let weak = Rc::downgrade(self);
        remove_all.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.comments.menu.popdown();
                if this.comments.busy.get() || !this.comments.ready.get() {
                    return;
                }
                let mut draft = this.comments.draft.borrow().clone();
                draft
                    .notes
                    .retain(|n| n.scope != *this.history.scope.borrow());
                this.persist_comments(draft, true, "All comments removed");
            }
        });
        self.comments.navigation.append(&remove_all);
        self.show_page();
    }

    fn jump_to_note(self: &Rc<Self>, note: &Note) {
        *self.pending_note.borrow_mut() = Some(note.clone());
        self.search.set_text("");
        self.reload();
    }

    pub(super) fn note_cards(
        self: &Rc<Self>,
        patch: &Patch,
        path: &std::path::Path,
    ) -> Vec<(usize, String, gtk::Widget)> {
        let mut cards = Vec::new();
        let mut composer_added = false;
        let notes: Vec<_> = self
            .comments
            .draft
            .borrow()
            .notes
            .iter()
            .filter(|n| n.path() == path && n.scope == *self.history.scope.borrow())
            .cloned()
            .collect();
        for mut note in notes {
            let range = notes::locate(&note, patch);
            if self.comments.editing.get()
                && self
                    .comments
                    .anchor
                    .borrow()
                    .as_ref()
                    .is_some_and(|n| n.id == note.id)
            {
                cards.push((
                    range.as_ref().map_or(0, |r| r.end),
                    note.id.clone(),
                    self.comments.composer.clone().upcast(),
                ));
                composer_added = true;
                continue;
            }
            if let Some(range) = range.clone() {
                if let Some(snapshot) = self.snapshot.borrow().as_ref() {
                    if let Some(file) = snapshot.files.iter().find(|f| f.path == note.path()) {
                        if let Ok(mut updated) = Note::new(
                            note.id.clone(),
                            snapshot,
                            file,
                            patch,
                            Some(range),
                            &note.text,
                        ) {
                            updated.resolved = note.resolved;
                            // A displayed patch can predate Send's validation.
                            // Rendering it must not clear a persisted conflict.
                            updated.needs_reattach = note.needs_reattach;
                            note = updated;
                        }
                    }
                }
            }
            let uncertain = note.needs_reattach
                || self.comments.stale.borrow().contains(&note.id)
                || (!note.excerpt.is_empty() && range.is_none());
            let position = range.as_ref().map_or(0, |r| r.end);
            let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
            style_card(&card);
            margins(&card, 8);
            let label = gtk::Label::builder()
                .label(if uncertain {
                    "Code changed · review this comment"
                } else if note.resolved {
                    "Resolved"
                } else {
                    &note.location
                })
                .xalign(0.0)
                .wrap(true)
                .build();
            label.add_css_class("caption");
            card.append(&label);
            if !note.resolved {
                let body = gtk::Label::builder()
                    .label(&note.text)
                    .xalign(0.0)
                    .wrap(true)
                    .selectable(true)
                    .max_width_chars(72)
                    .build();
                card.append(&body);
            }
            let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let edit = gtk::Button::with_label("Edit");
            let weak = Rc::downgrade(self);
            let edited = note.clone();
            edit.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    if this.comments.busy.get() {
                        return;
                    }
                    if this.comments.anchor.borrow().is_some() {
                        this.focus_comment();
                        return;
                    }
                    this.comments.hint.set_text(&edited.location);
                    this.comments.writer.buffer().set_text(&edited.text);
                    *this.comments.anchor.borrow_mut() = Some(edited.clone());
                    this.comments.editing.set(true);
                    this.comments.save.set_label("Save comment");
                    this.show_page();
                    this.comments.writer.grab_focus();
                }
            });
            let delete = gtk::Button::with_label("Delete");
            let weak = Rc::downgrade(self);
            let id = note.id.clone();
            delete.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    let mut draft = this.comments.draft.borrow().clone();
                    draft.notes.retain(|note| note.id != id);
                    this.persist_comments(draft, false, "Comment deleted");
                }
            });
            actions.append(&edit);
            actions.append(&delete);
            if uncertain {
                let attach = gtk::Button::with_label("Reattach");
                attach.set_tooltip_text(Some("Attach to selected lines"));
                let weak = Rc::downgrade(self);
                let old = note.clone();
                attach.connect_clicked(move |_| {
                    if let Some(this) = weak.upgrade() {
                        if this.comments.anchor.borrow().is_some() {
                            return;
                        }
                        this.begin_comment(false);
                        {
                            let mut anchor = this.comments.anchor.borrow_mut();
                            let Some(anchor) = anchor.as_mut() else {
                                return;
                            };
                            anchor.id = old.id.clone();
                        }
                        this.comments.writer.buffer().set_text(&old.text);
                        this.comments.editing.set(true);
                        this.comments.save.set_label("Save comment");
                        this.show_page();
                        this.comments.writer.grab_focus();
                    }
                });
                actions.append(&attach);
            }
            card.append(&actions);
            cards.push((position, note.id, card.upcast()));
        }
        if let Some(note) = self.comments.anchor.borrow().as_ref().filter(|n| {
            n.path() == path && n.scope == *self.history.scope.borrow() && !composer_added
        }) {
            let position = notes::locate(note, patch).map_or(0, |r| r.end);
            cards.push((
                position,
                note.id.clone(),
                self.comments.composer.clone().upcast(),
            ));
        }
        cards
    }

    pub(super) fn send_review(self: &Rc<Self>, target: Option<ReviewTarget>) {
        self.targets.menu.popdown();
        if self.comments.busy.get() {
            return;
        }
        if self.comments.edit_conflict.get() {
            self.status.set_text("This comment changed in another pane. Your text is preserved; cancel this edit to use the updated comment.");
            self.focus_comment();
            return;
        }
        if self.has_unsaved_review() {
            self.status
                .set_text("Save the comment you are writing before sending.");
            self.focus_comment();
            return;
        }
        // Merely opening Edit does not create an unsaved change.
        if self.comments.anchor.borrow().is_some() {
            self.clear_comment();
            self.show_page();
        }
        if self.comments.busy.replace(true) {
            return;
        }
        self.update_delivery_controls();
        self.targets.menu.popdown();
        let root = self.root.clone();
        let draft = self.comments.draft.borrow().clone();
        let scope = self.history.scope.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let draft = DraftStore::default_store()?.refresh_scope(&root, &draft, &scope)?;
                let selected: Vec<_> = draft
                    .notes
                    .iter()
                    .filter(|n| n.scope == scope)
                    .cloned()
                    .collect();
                let prompt = if selected.is_empty() {
                    Ok(None)
                } else {
                    notes::prompt(&root, &selected).map(Some)
                };
                Ok::<_, String>((draft, prompt))
            })
            .await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.comments.busy.set(false);
            match result {
                Ok(Ok((draft, prompt))) => {
                    *this.comments.draft.borrow_mut() = draft;
                    this.comments.stale.borrow_mut().clear();
                    this.render_comments();
                    if let Ok(Some(prompt)) = &prompt {
                        if let Some(target) = target {
                            if !this
                                .targets
                                .send(crate::bridge::GtkCommand::FocusReviewTarget {
                                    pane: this.pane,
                                    target,
                                    prompt: prompt.clone(),
                                })
                            {
                                this.status.set_text(
                                    "Agent connection unavailable. Try again or copy feedback.",
                                );
                            }
                        } else {
                            this.parent.clipboard().set_text(prompt);
                            this.status.set_text("Feedback copied");
                        }
                    } else if let Err(error) = prompt {
                        this.status.set_text(&error);
                    } else {
                        this.status
                            .set_text("Obsolete comments removed. No feedback to send.");
                    }
                }
                Ok(Err(error)) => this.status.set_text(&error),
                Err(_) => this
                    .status
                    .set_text("Could not prepare feedback. Try again."),
            }
            this.update_delivery_controls();
        });
    }
}

#[cfg(test)]
pub(super) async fn smoke(review: &Rc<ReviewWindow>) {
    async fn ready(review: &ReviewWindow) {
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while review.comments.busy.get() || !review.comments.ready.get() {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    ready(review).await;
    let row = review
        .display_lines
        .borrow()
        .iter()
        .position(|raw| {
            raw.is_some_and(|i| {
                review.patch.borrow().as_ref().unwrap().lines[i]
                    .text
                    .starts_with("+line 19999 ")
            })
        })
        .unwrap();
    let buffer = review.diff.buffer();
    let start = buffer.iter_at_line(row as i32).unwrap();
    let mut end = start;
    end.forward_to_line_end();
    buffer.select_range(&start, &end);
    review.begin_comment(false);
    assert!(review.comments.composer.parent().is_some());
    review
        .comments
        .writer
        .buffer()
        .set_text("cancel this draft");
    review.comments.cancel.grab_focus();
    review.comments.cancel.emit_clicked();
    assert!(review.comments.anchor.borrow().is_none());
    assert!(text(&review.comments.writer).is_empty());
    assert_eq!(
        review.host.borrow().visible_child().as_ref(),
        Some(review.root_widget.upcast_ref())
    );
    let start = review.diff.buffer().iter_at_line(row as i32).unwrap();
    review.diff.buffer().place_cursor(&start);
    review.begin_comment(false);
    review
        .comments
        .writer
        .buffer()
        .set_text("escape this draft");
    review.comments.cancel.grab_focus();
    review.cancel_or_close();
    assert!(review.comments.anchor.borrow().is_none());
    assert!(text(&review.comments.writer).is_empty());
    let start = review.diff.buffer().iter_at_line(row as i32).unwrap();
    review.diff.buffer().place_cursor(&start);
    review.begin_comment(false);
    review
        .comments
        .writer
        .buffer()
        .set_text("마지막 줄의 경계 조건을 확인해 주세요.\nKeep the existing behavior.");
    review.comments.save.emit_clicked();
    ready(review).await;
    let saved = review.comments.draft.borrow().notes[0].clone();
    assert_eq!(saved.location, "new lines 20000–20000");
    assert!(saved.text.contains("경계 조건"));
    assert!(review.comments.anchor.borrow().is_none());
    assert_eq!(review.inline_widgets.borrow().len(), 1);
    review.comments.menu.popup();
    review.cancel_or_close();
    assert!(!review.comments.menu.popover().unwrap().is_visible());
    assert_eq!(
        review.host.borrow().visible_child().as_ref(),
        Some(review.root_widget.upcast_ref())
    );
    super::targets::smoke(review).await;
    println!("DIFF_REVIEW_CANCEL_AND_MENU_DISMISS_OK");
    // Code above the comment shifts; feedback must follow rather than block.
    let file = review.root.join("large.rs");
    let contents = std::fs::read_to_string(&file).unwrap();
    let last = contents.lines().last().unwrap();
    let shifted = contents.strip_suffix(&format!("{last}\n")).unwrap();
    std::fs::write(&file, format!("inserted\n{shifted}    {last}\n")).unwrap();
    review.send_review(None);
    ready(review).await;
    let clipboard = review
        .parent
        .clipboard()
        .read_text_future()
        .await
        .unwrap()
        .unwrap();
    assert!(clipboard.contains("new lines 20001–20001"));
    assert!(clipboard.contains(&format!("+    {last}")));
    assert_eq!(
        review.comments.draft.borrow().notes[0].new_lines,
        Some((20001, 20001))
    );
    println!("CODE_REVIEW_CODE_ANCHOR_RELOCATION_OK");
    assert!(!clipboard.contains("SHA-256"));
    // Refresh and navigation take the reader to the comment's code.
    let moved = review.comments.draft.borrow().notes[0].clone();
    review.jump_to_note(&moved);
    glib::future_with_timeout(std::time::Duration::from_secs(20), async {
        while review.patch.borrow().is_none() {
            glib::timeout_future(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        review.selected_path.borrow().as_ref(),
        Some(&file.strip_prefix(&review.root).unwrap().to_path_buf())
    );
    // Persistence round-trip remains independent of window visibility.
    review.load_comments();
    ready(review).await;
    assert_eq!(review.comments.draft.borrow().notes[0].text, saved.text);
    // Legacy resolved notes remain in the list after Resolve was replaced by Delete.
    let original = review.comments.draft.borrow().notes.clone();
    let mut legacy = review.comments.draft.borrow().clone();
    legacy.notes = (0..5)
        .map(|i| {
            let mut note = original[0].clone();
            note.id = format!("legacy-count-{i}");
            note.text = format!("Legacy count comment {i}");
            note.resolved = i < 2;
            note
        })
        .collect();
    DraftStore::default_store()
        .unwrap()
        .save(&review.root, &legacy)
        .unwrap();
    review.load_comments();
    ready(review).await;
    assert_eq!(review.comments.navigation.observe_children().n_items(), 7);
    assert_eq!(
        review.comments.menu.label().as_deref(),
        Some("Comments · 5")
    );
    assert_eq!(review.targets.menu.label().as_deref(), Some("Send · 5"));
    assert_eq!(review.inline_widgets.borrow().len(), 5);
    review.send_review(None);
    ready(review).await;
    let feedback = review
        .parent
        .clipboard()
        .read_text_future()
        .await
        .unwrap()
        .unwrap();
    for i in 0..5 {
        assert!(feedback.contains(&format!("Legacy count comment {i}")));
    }
    // Editing a stacked comment must keep its slot and reveal the whole composer.
    for index in [0, 4] {
        // Reproduce incremental layout exposing a target before its scroll
        // range has caught up. An unchanged adjustment is not proof that the
        // composer is onscreen, even after the first allocation frame.
        let adjustment = review.diff.vadjustment().unwrap();
        let pending_upper = Rc::new(std::cell::Cell::new(adjustment.upper()));
        let upper = pending_upper.clone();
        let range_pending = adjustment.connect_upper_notify(move |adjustment| {
            if adjustment.upper() > adjustment.page_size() {
                upper.set(adjustment.upper());
                adjustment.set_upper(adjustment.page_size());
            }
        });
        let range_pending = std::cell::RefCell::new(Some(range_pending));
        let frames = std::cell::Cell::new(0);
        review.diff.add_tick_callback(move |_, _| {
            frames.set(frames.get() + 1);
            if frames.get() == 3 {
                adjustment.disconnect(range_pending.borrow_mut().take().unwrap());
                adjustment.set_upper(pending_upper.get());
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        let card = review.inline_widgets.borrow()[index].clone();
        card.last_child()
            .unwrap()
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        assert_eq!(
            review.inline_widgets.borrow()[index],
            review.comments.composer.clone().upcast::<gtk::Widget>()
        );
        assert!(
            !review.has_unsaved_review(),
            "unchanged Edit must not block Send"
        );
        let original_text = text(&review.comments.writer);
        review.comments.writer.buffer().set_text("changed draft");
        assert!(review.has_unsaved_review());
        review.send_review(None);
        assert!(review.status.text().starts_with("Save the comment"));
        assert_eq!(text(&review.comments.writer), "changed draft");
        review.comments.writer.buffer().set_text(&original_text);
        assert!(!review.has_unsaved_review());
        glib::future_with_timeout(std::time::Duration::from_secs(10), async {
            loop {
                let bounds = review
                    .comments
                    .composer
                    .compute_bounds(&review.diff)
                    .unwrap();
                if bounds.y() >= 0.0 && bounds.y() + bounds.height() <= review.diff.height() as f32
                {
                    break;
                }
                glib::timeout_future(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "composer must become fully visible after card layout and scrolling: {error:?}; card={:?}, viewport={}, adjustment={:?}",
                review.comments.composer.compute_bounds(&review.diff),
                review.diff.height(),
                review.diff.vadjustment().map(|a| (a.value(), a.upper(), a.page_size())),
            )
        });
        review.send_review(None);
        ready(review).await;
        assert_eq!(review.status.text(), "Feedback copied");
        assert!(review.comments.anchor.borrow().is_none());
    }
    println!("DIFF_REVIEW_COMMENT_SCROLL_GEOMETRY_OK");
    let remove_all = review
        .comments
        .navigation
        .last_child()
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    assert_eq!(remove_all.label().as_deref(), Some("Remove all comments"));
    assert_eq!(
        remove_all
            .prev_sibling()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .label()
            .as_deref(),
        Some("Reload saved comments")
    );
    remove_all.emit_clicked();
    ready(review).await;
    assert_eq!(review.status.text(), "All comments removed");
    review.load_comments();
    ready(review).await;
    assert!(review.comments.draft.borrow().notes.is_empty());
    assert!(review.inline_widgets.borrow().is_empty());
    assert_eq!(
        review.comments.menu.label().as_deref(),
        Some("Comments · 0")
    );
    assert_eq!(review.targets.menu.label().as_deref(), Some("Send · 0"));
    assert!(!review
        .comments
        .navigation
        .last_child()
        .unwrap()
        .is_sensitive());
    println!("DIFF_REVIEW_REMOVE_ALL_COMMENTS_OK");
    let mut restored = review.comments.draft.borrow().clone();
    restored.notes = original;
    DraftStore::default_store()
        .unwrap()
        .save(&review.root, &restored)
        .unwrap();
    review.load_comments();
    ready(review).await;
    println!("DIFF_REVIEW_LEGACY_COUNT_AND_FEEDBACK_OK");
    // Refresh removes obsolete saved comments, including their inline cards.
    std::fs::remove_file(&file).unwrap();
    review.jump_to_note(&moved);
    glib::future_with_timeout(std::time::Duration::from_secs(20), async {
        while !review.refresh.is_sensitive() || review.comments.busy.get() {
            glib::timeout_future(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(review.heading.text().contains("no longer"));
    assert!(review.inline_widgets.borrow().is_empty());
    assert!(review.comments.draft.borrow().notes.is_empty());
    println!("DIFF_REVIEW_INLINE_COMMENTS_RELOCATION_PERSISTENCE_OK");
    event_smoke(review, &moved).await;
    sync_smoke(&review.parent).await;
    scenarios::run(&review.parent).await;
}

#[cfg(test)]
async fn sync_smoke(parent: &adw::ApplicationWindow) {
    async fn wait_for(mut predicate: impl FnMut() -> bool) {
        glib::future_with_timeout(std::time::Duration::from_secs(15), async {
            while !predicate() {
                glib::timeout_future(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    fn edit_first(review: &Rc<ReviewWindow>) {
        let card = review.inline_widgets.borrow()[0].clone();
        card.last_child()
            .unwrap()
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
    }
    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    for dir in &dirs {
        assert!(std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .status
            .success());
        std::fs::write(dir.path().join("sync.txt"), "sync fixture\n").unwrap();
    }
    let mut windows = Vec::new();
    let mut views = Vec::new();
    for root in [dirs[0].path(), dirs[0].path(), dirs[1].path()] {
        let window = adw::ApplicationWindow::builder()
            .transient_for(parent)
            .default_width(900)
            .default_height(700)
            .build();
        let host = gtk::Stack::new();
        window.set_content(Some(&host));
        let view = ReviewWindow::new(&window, &host, flowmux_core::PaneId::new(), root.into());
        view.present();
        views.push(view);
        windows.push(window);
    }
    wait_for(|| {
        views.iter().all(|v| {
            v.root_widget.is_mapped()
                && v.comments.ready.get()
                && v.patch.borrow().is_some()
                && !v.comments.busy.get()
        })
    })
    .await;
    let (left, right, other) = (&views[0], &views[1], &views[2]);
    right.begin_comment(true);
    right
        .comments
        .writer
        .buffer()
        .set_text("preserved local draft");
    left.begin_comment(true);
    left.comments.writer.buffer().set_text("shared comment");
    left.save_comment();
    wait_for(|| right.comments.draft.borrow().notes.len() == 1).await;
    assert_eq!(text(&right.comments.writer), "preserved local draft");
    assert!(other.comments.draft.borrow().notes.is_empty());
    right.save_comment();
    wait_for(|| left.comments.draft.borrow().notes.len() == 2).await;
    assert_eq!(left.comments.menu.label().as_deref(), Some("Comments · 2"));
    // A menu's focused rows must survive an incoming update until it closes.
    right.comments.menu.popup();
    let row = right.comments.navigation.first_child().unwrap();
    row.grab_focus();
    edit_first(left);
    left.comments.writer.buffer().set_text("updated by left");
    left.save_comment();
    wait_for(|| !left.comments.busy.get()).await;
    glib::timeout_future(std::time::Duration::from_millis(700)).await;
    assert!(row.parent().is_some());
    assert_eq!(
        right.comments.draft.borrow().notes[0].text,
        "shared comment"
    );
    right.comments.menu.popdown();
    wait_for(|| right.comments.draft.borrow().notes[0].text == "updated by left").await;
    // Concurrent edits preserve local text and cannot overwrite the newer note.
    edit_first(right);
    right
        .comments
        .writer
        .buffer()
        .set_text("local conflicting edit");
    edit_first(left);
    left.comments.writer.buffer().set_text("newer saved edit");
    left.save_comment();
    wait_for(|| right.comments.edit_conflict.get()).await;
    assert_eq!(text(&right.comments.writer), "local conflicting edit");
    right.save_comment();
    assert!(!right.comments.busy.get());
    assert_eq!(
        right.comments.draft.borrow().notes[0].text,
        "newer saved edit"
    );
    right.cancel_comment();
    // An unchanged editor follows a remote deletion rather than resurrecting it.
    edit_first(right);
    left.comments
        .navigation
        .last_child()
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap()
        .emit_clicked();
    wait_for(|| right.comments.draft.borrow().notes.is_empty()).await;
    assert!(right.comments.anchor.borrow().is_none());
    assert_eq!(right.comments.menu.label().as_deref(), Some("Comments · 0"));
    assert_eq!(right.targets.menu.label().as_deref(), Some("Send · 0"));
    assert!(other.comments.draft.borrow().notes.is_empty());
    // Pruning from another view preserves an in-progress edit, but cannot
    // silently resurrect the obsolete saved comment.
    left.begin_comment(true);
    left.comments
        .writer
        .buffer()
        .set_text("obsolete saved comment");
    left.save_comment();
    wait_for(|| !left.comments.busy.get() && right.comments.draft.borrow().notes.len() == 1).await;
    edit_first(right);
    right
        .comments
        .writer
        .buffer()
        .set_text("keep my unsaved edit");
    std::fs::remove_file(dirs[0].path().join("sync.txt")).unwrap();
    left.reload();
    wait_for(|| {
        left.comments.draft.borrow().notes.is_empty() && right.comments.edit_conflict.get()
    })
    .await;
    assert_eq!(text(&right.comments.writer), "keep my unsaved edit");
    right.save_comment();
    assert!(!right.comments.busy.get());
    assert!(DraftStore::default_store()
        .unwrap()
        .load(&left.root)
        .unwrap()
        .notes
        .is_empty());
    right.cancel_comment();

    // If every comment expires immediately before Send, clear the saved list
    // and do not copy or dispatch an empty feedback batch.
    std::fs::write(dirs[0].path().join("sync.txt"), "fresh code\n").unwrap();
    left.reload();
    wait_for(|| !left.comments.busy.get() && left.patch.borrow().is_some()).await;
    left.begin_comment(true);
    left.comments
        .writer
        .buffer()
        .set_text("expires before send");
    left.save_comment();
    wait_for(|| !left.comments.busy.get()).await;
    let clipboard = left.parent.clipboard().read_text_future().await.unwrap();
    std::fs::remove_file(dirs[0].path().join("sync.txt")).unwrap();
    left.send_review(None);
    wait_for(|| !left.comments.busy.get()).await;
    assert!(left.comments.draft.borrow().notes.is_empty());
    assert!(!left.targets.menu.is_sensitive());
    assert!(left.status.text().contains("No feedback to send"));
    assert_eq!(
        left.parent.clipboard().read_text_future().await.unwrap(),
        clipboard
    );
    println!("CODE_REVIEW_AUTO_PRUNE_PRESERVES_DRAFTS_OK");
    for window in windows {
        window.destroy();
    }
    println!("DIFF_REVIEW_LIVE_COMMENT_SYNC_OK");
}

#[cfg(test)]
async fn event_smoke(review: &Rc<ReviewWindow>, missing: &Note) {
    async fn settled(review: &ReviewWindow) {
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while !review.refresh.is_sensitive()
                || review.patch.borrow().is_none()
                || review.comments.busy.get()
            {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    fn button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
        if let Some(button) = widget.downcast_ref::<gtk::Button>() {
            if button.label().as_deref() == Some(label) {
                return Some(button.clone());
            }
        }
        let mut child = widget.first_child();
        while let Some(widget) = child {
            if let Some(found) = button(&widget, label) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }
    for name in ["alpha.txt", "beta.txt"] {
        std::fs::write(review.root.join(name), "one\ntwo\nthree\n").unwrap();
    }
    review.reload();
    settled(review).await;
    review.selection.set_selected(1);
    settled(review).await;
    assert_eq!(
        review.selected_path.borrow().as_deref(),
        Some(std::path::Path::new("beta.txt"))
    );
    review.begin_comment(true);
    review.comments.writer.buffer().set_text("unsaved beta");
    review.selection.set_selected(0);
    settled(review).await;
    assert!(!review.comments.writer.is_mapped());
    // Starting another comment must reveal the existing draft, not focus a
    // detached TextView and leave the UI apparently unresponsive.
    review.begin_comment(true);
    settled(review).await;
    assert_eq!(
        review.selected_path.borrow().as_deref(),
        Some(std::path::Path::new("beta.txt"))
    );
    assert!(review.comments.writer.is_mapped());
    assert_eq!(
        gtk::prelude::GtkWindowExt::focus(&review.parent).as_ref(),
        Some(review.comments.writer.upcast_ref())
    );
    assert_eq!(text(&review.comments.writer), "unsaved beta");
    review.show_page();
    assert_eq!(
        gtk::prelude::GtkWindowExt::focus(&review.parent).as_ref(),
        Some(review.comments.writer.upcast_ref())
    );
    // A rejected delivery closes its menu and keeps the draft editable.
    review.targets.menu.popup();
    review.send_review(None);
    assert!(!review.targets.menu.popover().unwrap().is_visible());
    assert_eq!(
        gtk::prelude::GtkWindowExt::focus(&review.parent).as_ref(),
        Some(review.comments.writer.upcast_ref())
    );
    // Force a real storage revision conflict; failure must retain the text.
    DraftStore::default_store()
        .unwrap()
        .save(&review.root, &review.comments.draft.borrow())
        .unwrap();
    review.save_comment();
    settled(review).await;
    assert!(review.status.text().contains("another window"));
    assert_eq!(text(&review.comments.writer), "unsaved beta");
    assert!(review.comments.writer.is_editable());
    review.load_comments();
    settled(review).await;
    review.save_comment();
    settled(review).await;
    let saved = review.comments.draft.borrow().notes.last().unwrap().clone();
    assert_eq!(saved.path(), std::path::Path::new("beta.txt"));
    assert!(review.comments.anchor.borrow().is_none());
    button(review.diff.upcast_ref(), "Edit")
        .unwrap()
        .emit_clicked();
    assert_eq!(text(&review.comments.writer), "unsaved beta");
    review.comments.writer.buffer().set_text("edited beta");
    review.save_comment();
    settled(review).await;
    // Navigate from a different selected row, including a nonmatching filter.
    review.selection.set_selected(0);
    settled(review).await;
    review.search.set_text("alpha");
    review.filter_files();
    settled(review).await;
    review.jump_to_note(&saved);
    settled(review).await;
    assert_eq!(
        review.selected_path.borrow().as_deref(),
        Some(std::path::Path::new("beta.txt"))
    );
    // Reattach without code selected must not create an orphan writer draft.
    review.comments.stale.borrow_mut().push(saved.id);
    review.show_page();
    review
        .diff
        .buffer()
        .place_cursor(&review.diff.buffer().start_iter());
    button(review.diff.upcast_ref(), "Reattach")
        .unwrap()
        .emit_clicked();
    assert!(review.comments.anchor.borrow().is_none());
    assert!(text(&review.comments.writer).is_empty());
    let row = review
        .display_lines
        .borrow()
        .iter()
        .position(|raw| {
            raw.is_some_and(|i| {
                review.patch.borrow().as_ref().unwrap().lines[i]
                    .new
                    .is_some()
            })
        })
        .unwrap();
    review
        .diff
        .buffer()
        .place_cursor(&review.diff.buffer().iter_at_line(row as i32).unwrap());
    button(review.diff.upcast_ref(), "Reattach")
        .unwrap()
        .emit_clicked();
    assert_eq!(text(&review.comments.writer), "edited beta");
    assert!(review.comments.editing.get());
    review.save_comment();
    settled(review).await;
    assert!(review.comments.stale.borrow().is_empty());
    assert!(review
        .comments
        .draft
        .borrow()
        .notes
        .last()
        .unwrap()
        .range
        .is_some());
    // Delete removes the saved note, without discarding another open draft.
    let deleted_id = review
        .comments
        .draft
        .borrow()
        .notes
        .last()
        .unwrap()
        .id
        .clone();
    review.begin_comment(true);
    review.comments.writer.buffer().set_text("keep this draft");
    button(review.diff.upcast_ref(), "Delete")
        .unwrap()
        .emit_clicked();
    settled(review).await;
    assert_eq!(review.status.text(), "Comment deleted");
    assert!(!review
        .comments
        .draft
        .borrow()
        .notes
        .iter()
        .any(|n| n.id == deleted_id));
    assert_eq!(text(&review.comments.writer), "keep this draft");
    assert_eq!(
        review.comments.menu.label().as_deref(),
        Some("Comments · 0")
    );
    assert_eq!(review.targets.menu.label().as_deref(), Some("Send · 0"));
    review.load_comments();
    settled(review).await;
    assert!(!review
        .comments
        .draft
        .borrow()
        .notes
        .iter()
        .any(|n| n.id == deleted_id));
    assert!(button(review.diff.upcast_ref(), "Delete").is_none());
    review.cancel_comment();
    // Out-of-order patch requests must finish on the last requested file.
    review.selection.set_selected(0);
    review.selection.set_selected(1);
    review.selection.set_selected(0);
    settled(review).await;
    assert_eq!(
        review.selected_path.borrow().as_deref(),
        Some(std::path::Path::new("alpha.txt"))
    );
    review.jump_to_note(missing);
    settled(review).await;
    assert!(review.heading.text().contains("no longer"));
    assert!(review.inline_widgets.borrow().is_empty());
    // Initial storage failure must still expose a working retry action.
    let database = flowmux_config::paths::state_dir()
        .unwrap()
        .join("reviews.sqlite3");
    let backup = database.with_extension("smoke-backup");
    std::fs::rename(&database, &backup).unwrap();
    std::fs::create_dir(&database).unwrap();
    let failed = ReviewWindow::new(
        &review.parent,
        &review.host.borrow(),
        flowmux_core::PaneId::new(),
        review.root.clone(),
    );
    settled(&failed).await;
    assert!(!failed.comments.ready.get());
    assert!(!failed.comments.save.is_sensitive());
    let retry = button(
        failed.comments.navigation.upcast_ref(),
        "Reload saved comments",
    )
    .unwrap();
    std::fs::remove_dir(&database).unwrap();
    std::fs::rename(&backup, &database).unwrap();
    retry.emit_clicked();
    settled(&failed).await;
    assert!(failed.comments.ready.get());
    assert_eq!(
        failed.comments.draft.borrow().notes.len(),
        review.comments.draft.borrow().notes.len()
    );
    failed.detach();
    *review.selected_path.borrow_mut() = Some("alpha.txt".into());
    *review.patch.borrow_mut() = Some(review::parse_patch(
        "@@ -1 +1 @@\n-old\n+new\n@@ -20 +20 @@\n-before\n+after\n",
    ));
    review.show_page();
    let buffer = review.diff.buffer();
    buffer.place_cursor(&buffer.start_iter());
    review.next_hunk(true);
    assert_eq!(buffer.iter_at_offset(buffer.cursor_position()).line(), 3);
    review.next_hunk(true);
    assert_eq!(buffer.iter_at_offset(buffer.cursor_position()).line(), 3);
    review.next_hunk(false);
    assert_eq!(buffer.cursor_position(), 0);
    review.next_hunk(false);
    assert_eq!(buffer.cursor_position(), 0);
    review.selection.set_selected(0);
    settled(review).await;
    review.begin_comment(true);
    review.save_comment();
    assert_eq!(review.status.text(), "Write a comment first.");
    review.cancel_comment();
    println!("DIFF_REVIEW_DRAFT_NAVIGATION_EDIT_DELETE_CONFLICT_OK");
}
