// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_state::review_drafts::{Draft, DraftStore};
use flowmux_vcs::review::notes::{self, Note};

pub(super) struct Comments {
    pub panel: gtk::Notebook,
    writer: gtk::TextView,
    add: gtk::Button,
    cancel: gtk::Button,
    reanchor: gtk::CheckButton,
    list: gtk::StringList,
    selection: gtk::SingleSelection,
    edit: gtk::Button,
    resolve: gtk::Button,
    reload: gtk::Button,
    preview: gtk::TextView,
    prepare: gtk::Button,
    copy: gtk::Button,
    message: gtk::Label,
    draft: RefCell<Draft>,
    root: RefCell<Option<PathBuf>>,
    editing: RefCell<Option<String>>,
    anchor: RefCell<Option<Note>>,
    busy: Cell<bool>,
    ready: Cell<bool>,
    stale: RefCell<Vec<String>>,
}

fn text(view: &gtk::TextView) -> String {
    let buffer = view.buffer();
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), false)
        .into()
}

impl Comments {
    pub fn new() -> Self {
        let panel = gtk::Notebook::new();
        let write = gtk::Box::new(gtk::Orientation::Vertical, 6);
        margins(&write, 8);
        let hint = gtk::Label::builder().label("Select diff lines to comment on them, or leave the selection empty for the whole file.").xalign(0.0).wrap(true).build();
        write.append(&hint);
        let writer = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(6)
            .left_margin(6)
            .build();
        writer.set_widget_name("flowmux-review-comment");
        writer.update_property(&[gtk::accessible::Property::Label("Review comment")]);
        write.append(
            &gtk::ScrolledWindow::builder()
                .child(&writer)
                .vexpand(true)
                .min_content_height(60)
                .build(),
        );
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let add = gtk::Button::with_label("Save comment");
        add.add_css_class("suggested-action");
        let cancel = gtk::Button::with_label("Clear draft");
        let reanchor = gtk::CheckButton::with_label("Use current diff selection");
        reanchor.set_visible(false);
        for button in [&add, &cancel] {
            buttons.append(button);
        }
        buttons.append(&reanchor);
        write.append(&buttons);
        panel.append_page(&write, Some(&gtk::Label::new(Some("Write"))));

        let saved = gtk::Box::new(gtk::Orientation::Vertical, 6);
        margins(&saved, 8);
        let list = gtk::StringList::new(&[]);
        let selection = gtk::SingleSelection::new(Some(list.clone()));
        selection.set_autoselect(false);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::End)
                .build();
            margins(&label, 6);
            item.set_child(Some(&label));
        });
        factory.connect_bind(|_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let value = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .unwrap()
                .string();
            let label = item.child().and_downcast::<gtk::Label>().unwrap();
            label.set_text(&value);
            label.set_tooltip_text(Some(&value));
        });
        let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
        view.update_property(&[gtk::accessible::Property::Label("Saved review comments")]);
        saved.append(
            &gtk::ScrolledWindow::builder()
                .child(&view)
                .vexpand(true)
                .build(),
        );
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let edit = gtk::Button::with_label("Edit");
        let resolve = gtk::Button::with_label("Resolve / reopen");
        let reload = gtk::Button::with_label("Reload saved");
        for button in [&edit, &resolve, &reload] {
            buttons.append(button);
        }
        saved.append(&buttons);
        panel.append_page(&saved, Some(&gtk::Label::new(Some("Comments"))));

        let export = gtk::Box::new(gtk::Orientation::Vertical, 6);
        margins(&export, 8);
        let preview = gtk::TextView::builder()
            .editable(false)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::WordChar)
            .build();
        preview.update_property(&[gtk::accessible::Property::Label("Review preview")]);
        export.append(
            &gtk::ScrolledWindow::builder()
                .child(&preview)
                .vexpand(true)
                .build(),
        );
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let prepare = gtk::Button::with_label("Validate & preview");
        let copy = gtk::Button::with_label("Copy review");
        for button in [&prepare, &copy] {
            buttons.append(button);
        }
        export.append(&buttons);
        let message = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        export.append(&message);
        panel.append_page(&export, Some(&gtk::Label::new(Some("Deliver"))));
        Self {
            panel,
            writer,
            add,
            cancel,
            reanchor,
            list,
            selection,
            edit,
            resolve,
            reload,
            preview,
            prepare,
            copy,
            message,
            draft: RefCell::new(Draft::default()),
            root: RefCell::new(None),
            editing: RefCell::new(None),
            anchor: RefCell::new(None),
            busy: Cell::new(false),
            ready: Cell::new(false),
            stale: RefCell::new(Vec::new()),
        }
    }

    fn controls(&self) {
        let enabled = self.ready.get() && !self.busy.get();
        for button in [
            &self.add,
            &self.edit,
            &self.resolve,
            &self.prepare,
            &self.copy,
        ] {
            button.set_sensitive(enabled);
        }
        self.reload.set_sensitive(!self.busy.get());
        self.cancel.set_sensitive(!self.busy.get());
        self.writer.set_editable(!self.busy.get());
    }
}

impl ReviewWindow {
    pub fn has_unsaved_review(&self) -> bool {
        self.comments.busy.get() || !text(&self.comments.writer).is_empty()
    }

    pub(super) fn connect_comments(self: &Rc<Self>) {
        let c = &self.comments;
        c.controls();
        let weak = Rc::downgrade(self);
        c.writer.buffer().connect_changed(move |_| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            if this.comments.editing.borrow().is_some() || this.comments.anchor.borrow().is_some() {
                return;
            }
            let value = text(&this.comments.writer);
            if value.trim().is_empty() {
                return;
            }
            match this.current_note(uuid::Uuid::new_v4().to_string(), &value) {
                Ok(note) => {
                    this.status.set_text(&format!(
                        "Comment on {} · {}",
                        note.label(),
                        note.location
                    ));
                    *this.comments.anchor.borrow_mut() = Some(note);
                }
                Err(error) => this.status.set_text(&error),
            }
        });
        macro_rules! action {
            ($button:expr, $method:ident) => {{
                let weak = Rc::downgrade(self);
                $button.connect_clicked(move |_| {
                    if let Some(this) = weak.upgrade() {
                        this.$method();
                    }
                });
            }};
        }
        action!(c.add, save_comment);
        action!(c.cancel, clear_comment);
        action!(c.edit, edit_comment);
        action!(c.resolve, resolve_comment);
        action!(c.reload, load_comments);
        for (button, copy) in [(&c.prepare, false), (&c.copy, true)] {
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.prepare_review(copy);
                }
            });
        }
    }

    pub(super) fn init_comments(self: &Rc<Self>, root: PathBuf) {
        if self.comments.root.borrow().is_none() {
            *self.comments.root.borrow_mut() = Some(root);
            self.load_comments();
        }
    }

    fn load_comments(self: &Rc<Self>) {
        let Some(root) = self.comments.root.borrow().clone() else {
            return;
        };
        if self.comments.busy.replace(true) {
            return;
        }
        self.comments.controls();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result =
                gtk::gio::spawn_blocking(move || DraftStore::default_store()?.load(&root)).await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            this.comments.busy.set(false);
            match result {
                Ok(Ok(draft)) => {
                    *this.comments.draft.borrow_mut() = draft;
                    this.comments.ready.set(true);
                    this.render_comments();
                }
                Ok(Err(error)) => {
                    this.comments.ready.set(false);
                    this.status.set_text(&error);
                }
                Err(_) => {
                    this.comments.ready.set(false);
                    this.status
                        .set_text("Cannot load review comments. Use Reload saved to retry.");
                }
            }
            this.comments.controls();
        });
    }

    fn render_comments(&self) {
        let c = &self.comments;
        let draft = c.draft.borrow();
        let labels: Vec<String> = draft
            .notes
            .iter()
            .map(|n| {
                let state = if n.resolved {
                    "Resolved"
                } else if c.stale.borrow().contains(&n.id) {
                    "STALE"
                } else {
                    "Open"
                };
                format!(
                    "{state} · {} · {} · {}",
                    n.label(),
                    n.location,
                    n.text.lines().next().unwrap_or("")
                )
            })
            .collect();
        c.list.splice(
            0,
            c.list.n_items(),
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        c.preview.buffer().set_text("");
        c.message.set_text(&format!(
            "{} open comments · Validate again before delivery",
            draft.notes.iter().filter(|n| !n.resolved).count()
        ));
    }

    fn selected_note(&self) -> Option<Note> {
        self.comments
            .draft
            .borrow()
            .notes
            .get(self.comments.selection.selected() as usize)
            .cloned()
    }

    fn clear_comment(&self) {
        self.comments.writer.buffer().set_text("");
        self.comments.editing.borrow_mut().take();
        self.comments.anchor.borrow_mut().take();
        self.comments.reanchor.set_active(false);
        self.comments.reanchor.set_visible(false);
        self.comments.add.set_label("Save comment");
    }

    fn edit_comment(&self) {
        if !text(&self.comments.writer).is_empty() {
            self.status
                .set_text("Save or clear the current draft before editing another comment.");
            self.comments.panel.set_current_page(Some(0));
            return;
        }
        let Some(note) = self.selected_note() else {
            self.status.set_text("Select a saved comment first.");
            return;
        };
        *self.comments.editing.borrow_mut() = Some(note.id);
        self.comments.writer.buffer().set_text(&note.text);
        self.comments.reanchor.set_visible(true);
        self.comments.add.set_label("Save changes");
        self.comments.panel.set_current_page(Some(0));
    }

    fn current_note(&self, id: String, value: &str) -> Result<Note, String> {
        let snapshot = self.snapshot.borrow();
        let snapshot = snapshot.as_ref().ok_or("Wait for the file list to load.")?;
        let index = self
            .visible_files
            .borrow()
            .get(self.selection.selected() as usize)
            .copied()
            .ok_or("Select a changed file first.")?;
        let patch = self.patch.borrow();
        let patch = patch
            .as_ref()
            .ok_or("Wait for the selected diff to load.")?;
        let range = self.diff.buffer().selection_bounds().map(|(start, end)| {
            let first = self.page.get() * PAGE_LINES + start.line() as usize;
            let last = self.page.get() * PAGE_LINES
                + end.line() as usize
                + usize::from(end.line_offset() > 0);
            first..last.min(patch.lines.len())
        });
        Note::new(id, snapshot, &snapshot.files[index], patch, range, value)
    }

    fn save_comment(self: &Rc<Self>) {
        if self.comments.busy.get() || !self.comments.ready.get() {
            return;
        }
        let value = text(&self.comments.writer);
        if value.trim().is_empty() {
            self.status.set_text("Write a review comment first.");
            return;
        }
        let mut draft = self.comments.draft.borrow().clone();
        let editing = self.comments.editing.borrow().clone();
        let id = editing
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let note = if editing.is_some() && !self.comments.reanchor.is_active() {
            draft
                .notes
                .iter()
                .find(|n| n.id == id)
                .cloned()
                .ok_or_else(|| {
                    "This comment was removed. Clear the draft to add it again.".to_string()
                })
                .map(|mut n| {
                    n.text = value.trim().into();
                    n
                })
        } else if editing.is_some() {
            self.current_note(id.clone(), &value)
        } else {
            self.comments
                .anchor
                .borrow()
                .clone()
                .ok_or_else(|| {
                    "Select code before writing a comment. Clear this draft to start again.".into()
                })
                .map(|mut n| {
                    n.id = id.clone();
                    n.text = value.trim().into();
                    n
                })
        };
        match note {
            Ok(note) => {
                if let Some(index) = draft.notes.iter().position(|n| n.id == id) {
                    draft.notes[index] = note;
                } else {
                    draft.notes.push(note);
                }
                self.persist_comments(draft, true);
            }
            Err(error) => self.status.set_text(&error),
        }
    }

    fn resolve_comment(self: &Rc<Self>) {
        if self.comments.busy.get() || !self.comments.ready.get() {
            return;
        }
        let Some(note) = self.selected_note() else {
            self.status.set_text("Select a saved comment first.");
            return;
        };
        let mut draft = self.comments.draft.borrow().clone();
        let entry = draft.notes.iter_mut().find(|n| n.id == note.id).unwrap();
        entry.resolved = !entry.resolved;
        self.persist_comments(draft, false);
    }

    fn persist_comments(self: &Rc<Self>, draft: Draft, clear: bool) {
        let Some(root) = self.comments.root.borrow().clone() else {
            return;
        };
        if self.comments.busy.replace(true) {
            return;
        }
        self.comments.controls();
        self.status.set_text("Saving review…");
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
                        this.clear_comment();
                    }
                    this.render_comments();
                    this.status.set_text("Review saved locally.");
                }
                Ok(Err(error)) => this.status.set_text(&error),
                Err(_) => this
                    .status
                    .set_text("Cannot save comments. Your draft is still here; try again."),
            }
            this.comments.controls();
        });
    }

    fn prepare_review(self: &Rc<Self>, copy: bool) {
        let Some(root) = self.comments.root.borrow().clone() else {
            return;
        };
        if self.comments.busy.replace(true) {
            return;
        }
        self.comments.controls();
        self.comments
            .message
            .set_text("Checking comments against the current diff…");
        let notes = self.comments.draft.borrow().notes.clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let stale = notes::validate(&root, &notes)?;
                let prompt = notes::prompt(&root, &notes)?;
                Ok::<_, String>((stale, prompt))
            }).await;
            let Some(this) = weak.upgrade() else { return; };
            this.comments.busy.set(false);
            match result {
                Ok(Ok((stale, prompt))) => {
                    *this.comments.stale.borrow_mut() = stale.clone();
                    this.render_comments();
                    if stale.is_empty() {
                        this.comments.preview.buffer().set_text(&prompt);
                        if copy { this.window.clipboard().set_text(&prompt); }
                        this.comments.message.set_text(if copy { "Review copied. Paste it into your agent when ready." } else { "All open comments match the current diff." });
                    } else {
                        this.comments.message.set_text(&format!("{} stale comments. Refresh the diff, edit each STALE comment and select Use current diff selection, or resolve it.", stale.len()));
                    }
                }
                Ok(Err(error)) => { this.comments.preview.buffer().set_text(""); this.comments.message.set_text(&error); }
                Err(_) => this.comments.message.set_text("Cannot validate review. Try again."),
            }
            this.comments.controls();
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
    let c = &review.comments;
    c.add.emit_clicked();
    assert!(review.status.text().contains("Write a review"));
    let buffer = review.diff.buffer();
    let start = buffer.iter_at_line(0).unwrap();
    let end = buffer.iter_at_line(2).unwrap();
    buffer.select_range(&start, &end);
    c.writer
        .buffer()
        .set_text("마지막 페이지 두 줄을 검토해주세요.");
    c.add.emit_clicked();
    ready(review).await;
    assert_eq!(c.draft.borrow().notes.len(), 1);
    assert!(c.draft.borrow().notes[0].location.contains("19995"));
    assert!(text(&c.writer).is_empty());
    c.selection.set_selected(0);
    c.edit.emit_clicked();
    let long = "긴 리뷰 🧪\n여러 줄 피드백\n".repeat(1500);
    c.writer.buffer().set_text(&long);
    c.add.emit_clicked();
    ready(review).await;
    assert_eq!(c.draft.borrow().notes[0].text, long.trim());
    c.prepare.emit_clicked();
    ready(review).await;
    assert!(text(&c.preview).contains(long.trim()));
    c.copy.emit_clicked();
    ready(review).await;
    assert_eq!(
        review
            .window
            .clipboard()
            .read_text_future()
            .await
            .unwrap()
            .unwrap(),
        text(&c.preview)
    );
    // A second window cannot silently overwrite a concurrently saved draft.
    let store = DraftStore::default_store().unwrap();
    let root = c.root.borrow().clone().unwrap();
    let external = store.load(&root).unwrap();
    store.save(&root, &external).unwrap();
    c.selection.set_selected(0);
    c.edit.emit_clicked();
    c.writer.buffer().set_text("preserve conflict draft");
    c.add.emit_clicked();
    ready(review).await;
    assert!(review.status.text().contains("another window"));
    assert_eq!(text(&c.writer), "preserve conflict draft");
    c.reload.emit_clicked();
    ready(review).await;
    assert_eq!(text(&c.writer), "preserve conflict draft");
    c.add.emit_clicked();
    ready(review).await;
    assert_eq!(c.draft.borrow().notes[0].text, "preserve conflict draft");
    // Saving/reloading hundreds of comments retains long text and anchors.
    let mut draft = c.draft.borrow().clone();
    let template = draft.notes[0].clone();
    for i in 1..300 {
        let mut note = template.clone();
        note.id = i.to_string();
        note.text = long.clone();
        draft.notes.push(note);
    }
    review.persist_comments(draft, false);
    ready(review).await;
    c.reload.emit_clicked();
    ready(review).await;
    assert_eq!(c.list.n_items(), 300);
    let path = root.join("large.rs");
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, "external edit\n").unwrap();
    c.prepare.emit_clicked();
    ready(review).await;
    assert_eq!(c.stale.borrow().len(), 300);
    assert!(text(&c.preview).is_empty());
    assert!(c.message.text().contains("stale comments"));
    c.selection.set_selected(0);
    c.resolve.emit_clicked();
    ready(review).await;
    assert!(c.draft.borrow().notes[0].resolved);
    c.selection.set_selected(0);
    c.resolve.emit_clicked();
    ready(review).await;
    assert!(!c.draft.borrow().notes[0].resolved);
    std::fs::write(path, original).unwrap();
    c.writer
        .buffer()
        .set_text("unsaved survives hiding the review window");
    assert!(review.has_unsaved_review());
    review.window.set_visible(false);
    glib::timeout_future(std::time::Duration::from_millis(50)).await;
    review.window.present();
    assert_eq!(text(&c.writer), "unsaved survives hiding the review window");
    c.cancel.emit_clicked();
    assert!(!review.has_unsaved_review());
    c.panel.set_current_page(Some(1));
    println!("DIFF_REVIEW_COMMENTS_SMOKE_OK");
}
