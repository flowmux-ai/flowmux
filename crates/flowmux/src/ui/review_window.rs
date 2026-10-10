// SPDX-License-Identifier: GPL-3.0-or-later
//! Native, file-at-a-time diff review on both GTK backends.

use adw::prelude::*;
use flowmux_vcs::review::{self, Patch, Scope, Snapshot};
use gtk::{glib, pango};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

mod comments;
mod history;
mod targets;
pub(crate) use targets::ReviewTarget;

pub(crate) struct ReviewWindow {
    pub root_widget: gtk::Box,
    parent: adw::ApplicationWindow,
    host: RefCell<gtk::Stack>,
    return_to: RefCell<Option<glib::WeakRef<gtk::Widget>>>,
    pub root: PathBuf,
    pub workspace: Cell<Option<flowmux_core::WorkspaceId>>,
    pub pane: flowmux_core::PaneId,
    refresh: gtk::Button,
    history: history::History,
    search: gtk::SearchEntry,
    files: gtk::StringList,
    selection: gtk::SingleSelection,
    visible_files: RefCell<Vec<usize>>,
    pub status: gtk::Label,
    heading: gtk::Label,
    comparison: gtk::Label,
    commit_message: gtk::Label,
    commit_details: gtk::ScrolledWindow,
    diff: gtk::TextView,
    display_lines: RefCell<Vec<Option<usize>>>,
    inline_widgets: RefCell<Vec<gtk::Widget>>,
    pending_note: RefCell<Option<review::notes::Note>>,
    selected_path: RefCell<Option<PathBuf>>,
    snapshot: RefCell<Option<Snapshot>>,
    patch: RefCell<Option<Rc<Patch>>>,
    generation: Cell<u64>,
    patch_generation: Cell<u64>,
    scroll_generation: Cell<u64>,
    comments: comments::Comments,
    targets: targets::Targets,
}

impl ReviewWindow {
    pub fn new(
        parent: &adw::ApplicationWindow,
        host: &gtk::Stack,
        pane: flowmux_core::PaneId,
        root: PathBuf,
    ) -> Rc<Self> {
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        content.set_widget_name("flowmux-diff-review");
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        margins(&header, 8);
        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        back.set_tooltip_text(Some("Back to previous tab"));
        header.append(&back);
        let title = gtk::Label::builder()
            .label("Code Review")
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        title.add_css_class("title-3");
        header.append(&title);
        content.append(&header);
        let history = history::History::new();
        header.append(&history.menu);
        let refresh = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh.set_tooltip_text(Some("Refresh changes"));
        header.append(&refresh);
        let comparison = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .build();
        comparison.add_css_class("caption");
        comparison.set_widget_name("flowmux-review-comparison");
        margins(&comparison, 8);
        content.append(&comparison);

        let commit_message = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .selectable(true)
            .build();
        commit_message.set_widget_name("flowmux-review-commit-message");
        margins(&commit_message, 8);
        let commit_details = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(120)
            .child(&commit_message)
            .visible(false)
            .build();
        content.append(&commit_details);

        let left = gtk::Box::new(gtk::Orientation::Vertical, 6);
        margins(&left, 8);
        let search = gtk::SearchEntry::builder()
            .placeholder_text("Filter files")
            .build();
        left.append(&search);
        let files = gtk::StringList::new(&[]);
        let selection = gtk::SingleSelection::new(Some(files.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(pango::EllipsizeMode::Middle)
                .build();
            margins(&label, 7);
            item.set_child(Some(&label));
        });
        factory.connect_bind(|_, object| {
            let item = object.downcast_ref::<gtk::ListItem>().unwrap();
            let text = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .unwrap()
                .string();
            let label = item.child().and_downcast::<gtk::Label>().unwrap();
            label.set_text(&text);
            label.set_tooltip_text(Some(&text));
        });
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.set_widget_name("flowmux-review-files");
        list.update_property(&[gtk::accessible::Property::Label("Changed files")]);
        left.append(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .vexpand(true)
                .min_content_width(120)
                .build(),
        );

        let right = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let heading = gtk::Label::builder()
            .label("Select a changed file")
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .build();
        margins(&heading, 8);
        heading.add_css_class("heading");
        let file_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.set_hexpand(true);
        file_header.append(&heading);
        let comment = gtk::Button::with_label("+ Comment");
        comment.set_tooltip_text(Some("Comment on selected lines (C)"));
        let whole_file = gtk::Button::with_label("File comment");
        whole_file.set_tooltip_text(Some("Comment on whole file"));
        file_header.append(&comment);
        file_header.append(&whole_file);
        right.append(&file_header);
        let diff = gtk::TextView::builder()
            .editable(false)
            .cursor_visible(true)
            .monospace(true)
            .wrap_mode(gtk::WrapMode::None)
            .left_margin(10)
            .right_margin(10)
            .top_margin(8)
            .bottom_margin(8)
            .build();
        diff.set_widget_name("flowmux-review-diff");
        let code_style = gtk::CssProvider::new();
        code_style.load_from_string(
            "textview, textview text { font-family: monospace; font-size: 13px; }",
        );
        #[allow(deprecated)]
        diff.style_context()
            .add_provider(&code_style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        diff.update_property(&[gtk::accessible::Property::Label(
            "Diff with old and new line numbers",
        )]);
        right.append(
            &gtk::ScrolledWindow::builder()
                .child(&diff)
                .hexpand(true)
                .vexpand(true)
                .build(),
        );
        let comments = comments::Comments::new();
        let targets = targets::Targets::new();
        header.append(&comments.menu);
        header.append(&targets.menu);
        for menu in [&comments.menu, &targets.menu] {
            // Keep the arrow anchored to the button; align the popup body
            // to its right edge without inventing a shifted pointing target.
            menu.popover().unwrap().set_halign(gtk::Align::End);
        }
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&left)
            .end_child(&right)
            .resize_start_child(false)
            .resize_end_child(true)
            .shrink_start_child(true)
            .shrink_end_child(true)
            .position(160)
            .vexpand(true)
            .build();
        content.append(&split);
        let status = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .build();
        margins(&status, 10);
        content.append(&status);
        host.add_child(&content);
        let this = Rc::new(Self {
            root_widget: content,
            parent: parent.clone(),
            host: RefCell::new(host.clone()),
            return_to: RefCell::new(None),
            root,
            workspace: Cell::new(None),
            pane,
            refresh,
            history,
            search,
            files,
            selection,
            visible_files: RefCell::new(Vec::new()),
            status,
            heading,
            comparison,
            commit_message,
            commit_details,
            diff,
            display_lines: RefCell::new(Vec::new()),
            inline_widgets: RefCell::new(Vec::new()),
            pending_note: RefCell::new(None),
            selected_path: RefCell::new(None),
            snapshot: RefCell::new(None),
            patch: RefCell::new(None),
            generation: Cell::new(0),
            patch_generation: Cell::new(0),
            scroll_generation: Cell::new(0),
            comments,
            targets,
        });
        let weak = Rc::downgrade(&this);
        this.refresh.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });
        let weak = Rc::downgrade(&this);
        this.search.connect_search_changed(move |_| {
            if let Some(this) = weak.upgrade() {
                this.filter_files();
            }
        });
        let weak = Rc::downgrade(&this);
        this.selection.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.load_selected();
            }
        });
        let weak = Rc::downgrade(&this);
        back.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.hide();
            }
        });
        for (button, whole) in [(&comment, false), (&whole_file, true)] {
            let weak = Rc::downgrade(&this);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.begin_comment(whole);
                }
            });
        }
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(&this);
        click.connect_released(move |_, _, x, y| {
            if x > 35.0 {
                return;
            }
            if let Some(this) = weak.upgrade() {
                if this.diff.pick(x, y, gtk::PickFlags::DEFAULT).as_ref()
                    != Some(this.diff.upcast_ref())
                {
                    return;
                }
                let (x, y) = this.diff.window_to_buffer_coords(
                    gtk::TextWindowType::Widget,
                    x as i32,
                    y as i32,
                );
                if let Some(iter) = this.diff.iter_at_location(x, y) {
                    this.diff.buffer().place_cursor(&iter);
                    this.begin_comment(false);
                }
            }
        });
        this.diff.add_controller(click);
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&this);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !this.diff.has_focus() {
                return glib::Propagation::Proceed;
            }
            if modifiers.is_empty() && key == gtk::gdk::Key::c {
                this.begin_comment(false);
                return glib::Propagation::Stop;
            }
            if modifiers.is_empty() && matches!(key, gtk::gdk::Key::n | gtk::gdk::Key::p) {
                this.next_hunk(key == gtk::gdk::Key::n);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        this.diff.add_controller(keys);
        let weak = Rc::downgrade(&this);
        if let Some(adjustment) = this.diff.hadjustment() {
            adjustment.connect_page_size_notify(move |adjustment| {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                let width = (adjustment.page_size() as i32 - 48).max(240);
                for widget in this.inline_widgets.borrow().iter() {
                    if widget.width_request() != width {
                        widget.set_size_request(width, -1);
                    }
                }
            });
        }
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&this);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(this) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if key == gtk::gdk::Key::Escape {
                this.cancel_or_close();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        this.root_widget.add_controller(keys);
        let weak = Rc::downgrade(&this);
        this.root_widget.connect_unmap(move |_| {
            if let Some(this) = weak.upgrade() {
                this.release_preview();
            }
        });
        let weak = Rc::downgrade(&this);
        this.root_widget.connect_map(move |_| {
            if let Some(this) = weak.upgrade() {
                if this.snapshot.borrow().is_none()
                    && this.refresh.is_sensitive()
                    && !this.has_unsaved_review()
                {
                    this.reload();
                }
            }
        });
        this.connect_comments();
        this.connect_history();
        this.reload();
        this
    }

    fn reload(self: &Rc<Self>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.patch_generation
            .set(self.patch_generation.get().wrapping_add(1));
        self.snapshot.borrow_mut().take();
        self.commit_details.set_visible(false);
        self.commit_message.set_text("");
        self.update_delivery_controls();
        self.patch.borrow_mut().take();
        self.files.splice(0, self.files.n_items(), &[]);
        self.diff.buffer().set_text("");
        self.status.set_text("Loading changes…");
        self.refresh.set_sensitive(false);
        let root = self.root.clone();
        let scope = self.history.scope.borrow().clone();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || review::load(&root, scope)).await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            if this.generation.get() != generation {
                return;
            }
            this.refresh.set_sensitive(true);
            match result {
                Ok(Ok(snapshot)) => {
                    if let Some(message) = &snapshot.commit_message {
                        this.commit_message.set_text(message);
                        this.commit_details.set_visible(true);
                    }
                    this.comparison
                        .set_text(&review::display_path(&snapshot.root));
                    this.comparison.set_tooltip_text(Some(match snapshot.scope {
                        Scope::Commit(_) => "Selected commit compared with its first parent (empty tree for a root commit)",
                        _ => "Current checkout changes since HEAD, including staged edits and new files",
                    }));
                    let empty = matches!(snapshot.scope, Scope::Commit(_));
                    this.status.set_text(&if snapshot.files.is_empty() {
                        if empty { "No changes in this commit." } else { "No uncommitted changes." }.into()
                    } else {
                        format!("{} changed files", snapshot.files.len())
                    });
                    *this.snapshot.borrow_mut() = Some(snapshot);
                    this.init_comments(this.root.clone());
                    this.filter_files();
                }
                Ok(Err(error)) => this.status.set_text(&error),
                Err(_) => this.status.set_text("Cannot load changes. Try Refresh."),
            }
        });
    }

    fn filter_files(self: &Rc<Self>) {
        // Replacing the list emits selection changes. Only load the final
        // selection, otherwise an intermediate empty list consumes pending notes.
        let notifications = self.selection.freeze_notify();
        let query = self.search.text().to_lowercase();
        let (indices, labels): (Vec<_>, Vec<_>) = self
            .snapshot
            .borrow()
            .as_ref()
            .into_iter()
            .flat_map(|s| s.files.iter().enumerate())
            .filter(|(_, f)| f.label().to_lowercase().contains(&query))
            .map(|(i, f)| (i, format!("{}  {}", f.status, f.label())))
            .unzip();
        *self.visible_files.borrow_mut() = indices;
        self.selection.set_selected(gtk::INVALID_LIST_POSITION);
        self.files.splice(
            0,
            self.files.n_items(),
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        );
        let wanted = self
            .pending_note
            .borrow()
            .as_ref()
            .map(|n| n.path())
            .or_else(|| self.selected_path.borrow().clone());
        let index = wanted.as_ref().and_then(|path| {
            let snapshot = self.snapshot.borrow();
            let snapshot = snapshot.as_ref()?;
            self.visible_files
                .borrow()
                .iter()
                .position(|&i| &snapshot.files[i].path == path)
        });
        if self.pending_note.borrow().is_some() && index.is_none() {
            drop(notifications);
            if self.pending_note.borrow().is_some() {
                self.load_selected();
            }
        } else if !labels.is_empty() {
            self.selection.set_selected(index.unwrap_or(0) as u32);
        }
    }

    fn load_selected(self: &Rc<Self>) {
        let generation = self.patch_generation.get().wrapping_add(1);
        self.patch_generation.set(generation);
        self.patch.borrow_mut().take();
        self.show_page();
        if self.snapshot.borrow().is_none() {
            return;
        }
        let Some(index) = self
            .visible_files
            .borrow()
            .get(self.selection.selected() as usize)
            .copied()
        else {
            let pending = self.pending_note.borrow().clone();
            if let Some(note) = pending {
                self.heading
                    .set_text(&format!("{} · no longer in this comparison", note.label()));
                *self.selected_path.borrow_mut() = Some(note.path());
                *self.patch.borrow_mut() = Some(Rc::new(review::parse_patch("")));
                self.show_page();
                self.pending_note.borrow_mut().take();
                self.focus_pending_comment(&note);
            } else {
                self.heading.set_text("Select a changed file");
            }
            return;
        };
        let Some(snapshot) = self.snapshot.borrow().clone() else {
            return;
        };
        let Some(file) = snapshot.files.get(index).cloned() else {
            return;
        };
        *self.selected_path.borrow_mut() = Some(file.path.clone());
        self.heading.set_text(&file.label());
        self.heading.set_tooltip_text(Some(&file.label()));
        self.diff.buffer().set_text("Loading diff…");
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || snapshot.patch(&file)).await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            if this.patch_generation.get() != generation {
                return;
            }
            match result {
                Ok(Ok(patch)) => {
                    *this.patch.borrow_mut() = Some(Rc::new(patch));
                    this.show_page();
                    if let Some(note) = this.pending_note.borrow_mut().take() {
                        this.focus_pending_comment(&note);
                    }
                }
                Ok(Err(error)) => this.diff.buffer().set_text(&error),
                Err(_) => this
                    .diff
                    .buffer()
                    .set_text("Cannot read this diff. Select the file again to retry."),
            }
        });
    }

    #[cfg(test)]
    pub(crate) async fn smoke_wait_for_files(&self, expected: &[&str]) {
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while self.patch.borrow().is_none()
                || !self.comments.ready.get()
                || self.comments.busy.get()
            {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let snapshot = self.snapshot.borrow();
        let snapshot = snapshot.as_ref().unwrap();
        assert_eq!(snapshot.scope, Scope::WorkingTree);
        assert_eq!(
            snapshot.files.iter().map(|f| f.label()).collect::<Vec<_>>(),
            expected
        );
    }

    pub fn is_presented(&self) -> bool {
        self.host.borrow().visible_child().as_ref() == Some(self.root_widget.upcast_ref())
    }

    pub fn focus(&self) {
        if gtk::prelude::GtkWindowExt::focus(&self.parent)
            .is_some_and(|focus| focus.is_ancestor(&self.root_widget))
        {
            return;
        }
        if self.comments.composer.is_mapped() {
            self.comments
                .composer
                .child_focus(gtk::DirectionType::TabForward);
        } else {
            self.diff.grab_focus();
        }
    }

    pub fn attach(&self, host: &gtk::Stack) {
        if *self.host.borrow() != *host {
            let visible =
                self.host.borrow().visible_child().as_ref() == Some(self.root_widget.upcast_ref());
            self.host.borrow().remove(&self.root_widget);
            let previous = host.visible_child();
            host.add_child(&self.root_widget);
            *self.host.borrow_mut() = host.clone();
            *self.return_to.borrow_mut() = previous.map(|widget| widget.downgrade());
            if visible {
                host.set_visible_child(&self.root_widget);
            }
        }
    }

    pub fn detach(&self) {
        self.hide();
        self.host.borrow().remove(&self.root_widget);
    }

    pub fn present(self: &Rc<Self>) {
        let host = self.host.borrow();
        if host.visible_child().as_ref() != Some(self.root_widget.upcast_ref()) {
            *self.return_to.borrow_mut() = host.visible_child().map(|widget| widget.downgrade());
            if !self.has_unsaved_review()
                && (self.snapshot.borrow().is_some() || self.refresh.is_sensitive())
            {
                self.reload();
            }
        }
        host.set_visible_child(&self.root_widget);
        self.parent.present();
        self.focus();
    }

    pub fn hide(&self) {
        self.comments.menu.popdown();
        self.targets.menu.popdown();
        let host = self.host.borrow();
        if host.visible_child().as_ref() == Some(self.root_widget.upcast_ref()) {
            if let Some(previous) = self
                .return_to
                .borrow()
                .as_ref()
                .and_then(glib::WeakRef::upgrade)
                .filter(|p| p.parent().as_ref() == Some(host.upcast_ref()))
            {
                host.set_visible_child(&previous);
                previous.child_focus(gtk::DirectionType::TabForward);
            }
        }
        self.release_preview();
    }

    fn release_preview(&self) {
        if !self.has_unsaved_review() {
            self.generation.set(self.generation.get().wrapping_add(1));
            self.patch_generation
                .set(self.patch_generation.get().wrapping_add(1));
            self.scroll_generation
                .set(self.scroll_generation.get().wrapping_add(1));
            self.snapshot.borrow_mut().take();
            self.patch.borrow_mut().take();
            self.refresh.set_sensitive(true);
            self.files.splice(0, self.files.n_items(), &[]);
            *self.visible_files.borrow_mut() = Vec::new();
            *self.display_lines.borrow_mut() = Vec::new();
            for widget in self.inline_widgets.borrow_mut().drain(..) {
                if widget.parent().as_ref() == Some(self.diff.upcast_ref()) {
                    self.diff.remove(&widget);
                }
            }
            self.diff.buffer().set_text("");
        }
    }

    fn next_hunk(self: &Rc<Self>, forward: bool) {
        let buffer = self.diff.buffer();
        let current = buffer.iter_at_offset(buffer.cursor_position()).line() as usize;
        let patch = self.patch.borrow();
        let Some(patch) = patch.as_ref() else {
            return;
        };
        let rows: Vec<_> = self
            .display_lines
            .borrow()
            .iter()
            .enumerate()
            .filter_map(|(i, raw)| {
                raw.filter(|&r| patch.lines[r].text.starts_with("@@"))
                    .map(|_| i)
            })
            .collect();
        let next = if forward {
            rows.iter().copied().find(|&i| i > current)
        } else {
            rows.iter().copied().rev().find(|&i| i < current)
        };
        if let Some(iter) = next.and_then(|i| buffer.iter_at_line(i as i32)) {
            buffer.place_cursor(&iter);
            self.scroll_to_cursor(true, 0.2);
        }
    }

    fn scroll_to_cursor(self: &Rc<Self>, align: bool, yalign: f64) {
        let generation = self.scroll_generation.get().wrapping_add(1);
        self.scroll_generation.set(generation);
        let weak = Rc::downgrade(self);
        let requested = Cell::new(false);
        self.diff.add_tick_callback(move |diff, _| {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if this.scroll_generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let buffer = diff.buffer();
            let mark = buffer.get_insert();
            let iter = buffer.iter_at_mark(&mark);
            let card = iter
                .child_anchor()
                .and_then(|anchor| anchor.widgets().into_iter().next());
            let Some(card) = card else {
                diff.scroll_to_mark(&mark, 0.05, align, 0.0, yalign);
                return glib::ControlFlow::Break;
            };
            // TextView validates long buffers incrementally. A clamped or
            // unchanged adjustment does not mean the target has been reached.
            // Measure the allocated child on a later frame, and let GTK own
            // validation/animation instead of overwriting its adjustment.
            let oversized = card.height() > diff.height();
            let crowded = (card.height() + card.margin_top() + card.margin_bottom()) as f64
                > diff.height() as f64 * 0.9;
            // A nearly full-height card cannot fit inside a 5% scroll margin.
            // Center it without that margin; align an oversized card at its top.
            // Update the destination before accepting a transient visible frame.
            diff.scroll_to_mark(
                &mark,
                if crowded { 0.0 } else { 0.05 },
                align || crowded,
                0.0,
                if oversized {
                    0.0
                } else if crowded {
                    0.5
                } else {
                    yalign
                },
            );
            if requested.replace(true) && card.is_mapped() && card.height() > 0 {
                if let Some(bounds) = card.compute_bounds(diff) {
                    let visible = bounds.y() >= 0.0
                        && if oversized {
                            bounds.y() <= (card.margin_top() + diff.top_margin()) as f32
                        } else {
                            bounds.y() + bounds.height() <= diff.height() as f32
                        };
                    if visible {
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    #[allow(deprecated)]
    fn show_page(self: &Rc<Self>) {
        self.scroll_generation
            .set(self.scroll_generation.get().wrapping_add(1));
        let restore_focus = gtk::prelude::GtkWindowExt::focus(&self.parent)
            .filter(|focus| focus.is_ancestor(&self.comments.composer));
        let cursor = self
            .diff
            .buffer()
            .iter_at_offset(self.diff.buffer().cursor_position())
            .line() as usize;
        let previous_line = self
            .display_lines
            .borrow()
            .iter()
            .take(cursor + 1)
            .rev()
            .find_map(|line| *line)
            .unwrap_or(0);
        for widget in self.inline_widgets.borrow_mut().drain(..) {
            if widget.parent().as_ref() == Some(self.diff.upcast_ref()) {
                self.diff.remove(&widget);
            }
        }
        let patch = self.patch.borrow().clone();
        let buffer = self.diff.buffer();
        buffer.set_text("");
        let Some(patch) = patch else {
            self.display_lines.borrow_mut().clear();
            return;
        };
        let path = self.selected_path.borrow().clone();
        let mut cards = path
            .as_deref()
            .map(|path| self.note_cards(&patch, path))
            .unwrap_or_default();
        cards.sort_by_key(|(position, _, _)| *position);
        let mut cards = cards.into_iter().peekable();
        let mut map = Vec::new();
        let wanted_note = self
            .pending_note
            .borrow()
            .as_ref()
            .map(|note| note.id.clone());
        let mut comment_row = None;
        for (name, color) in [
            ("addition", "success_color"),
            ("deletion", "error_color"),
            ("hunk", "accent_color"),
        ] {
            let table = buffer.tag_table();
            let tag = table.lookup(name).unwrap_or_else(|| {
                let tag = gtk::TextTag::builder().name(name).build();
                table.add(&tag);
                tag
            });
            tag.set_foreground_rgba(self.diff.style_context().lookup_color(color).as_ref());
        }
        for raw in 0..=patch.lines.len() {
            while cards
                .peek()
                .is_some_and(|(position, _, _)| *position <= raw)
            {
                let (_, id, widget) = cards.next().unwrap();
                if wanted_note.as_ref().map_or(
                    widget == self.comments.composer.clone().upcast::<gtk::Widget>(),
                    |wanted| *wanted == id,
                ) {
                    comment_row = Some(map.len());
                }
                let mut end = buffer.end_iter();
                let anchor = buffer.create_child_anchor(&mut end);
                widget.set_size_request((self.diff.width() - 48).max(240), -1);
                self.diff.add_child_at_anchor(&widget, &anchor);
                buffer.insert(&mut buffer.end_iter(), "\n");
                map.push(None);
                self.inline_widgets.borrow_mut().push(widget);
            }
            let Some(line) = patch.lines.get(raw) else {
                break;
            };
            if line.old.is_none()
                && line.new.is_none()
                && ["diff --git ", "index ", "--- ", "+++ "]
                    .iter()
                    .any(|prefix| line.text.starts_with(prefix))
            {
                continue;
            }
            let code = line.old.is_some() || line.new.is_some();
            let text = format!(
                "{} {:>5} {:>5}  {}\n",
                if code { "+" } else { " " },
                line.old.map(|n| n.to_string()).unwrap_or_default(),
                line.new.map(|n| n.to_string()).unwrap_or_default(),
                line.text
            );
            let tag = if line.new.is_some() && line.old.is_none() {
                Some("addition")
            } else if line.old.is_some() && line.new.is_none() {
                Some("deletion")
            } else if line.text.starts_with("@@") {
                Some("hunk")
            } else {
                None
            };
            if let Some(tag) = tag {
                buffer.insert(&mut buffer.end_iter(), &text[..2]);
                buffer.insert_with_tags_by_name(&mut buffer.end_iter(), &text[2..], &[tag]);
            } else {
                buffer.insert(&mut buffer.end_iter(), &text);
            }
            map.push(Some(raw));
        }
        let row = comment_row.unwrap_or_else(|| {
            map.iter()
                .position(|raw| raw.is_some_and(|r| r >= previous_line))
                .unwrap_or(0)
        });
        *self.display_lines.borrow_mut() = map;
        if let Some(iter) = buffer.iter_at_line(row as i32) {
            buffer.place_cursor(&iter);
            // Child anchors contribute their allocated card heights. Defer
            // scrolling until TextView has validated those line heights.
            self.scroll_to_cursor(false, 0.0);
        }
        if let Some(focus) = restore_focus.filter(|focus| focus.is_mapped()) {
            focus.grab_focus();
        }
    }
}

#[cfg(test)]
pub(crate) async fn smoke(parent: &adw::ApplicationWindow) {
    let test_window = adw::ApplicationWindow::builder()
        .transient_for(parent)
        .default_width(1100)
        .default_height(760)
        .build();
    let parent = &test_window;
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::fs::write(
        dir.path().join("large.rs"),
        (0..20_000)
            .map(|i| format!("line {i} 내용\n"))
            .collect::<String>(),
    )
    .unwrap();
    let host = gtk::Stack::new();
    let terminal = gtk::Label::new(Some("Terminal remains mounted"));
    host.add_child(&terminal);
    parent.set_content(Some(&host));
    let review = ReviewWindow::new(
        parent,
        &host,
        flowmux_core::PaneId::new(),
        dir.path().into(),
    );
    review.present();
    glib::future_with_timeout(std::time::Duration::from_secs(20), async {
        while review.patch.borrow().is_none() {
            glib::timeout_future(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let buffer = review.diff.buffer();
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false);
    assert!(text.contains("line 0 ") && text.contains("line 19999 "));
    assert!(!text.contains("diff --git") && !text.contains("index 0000"));
    assert!(review.root_widget.is_mapped());
    comments::smoke(&review).await;
    history::smoke(parent).await;
    review.smoke_set_draft("Keep this unsaved review");
    review.hide();
    assert!(
        review.patch.borrow().is_some(),
        "draft context must survive hiding"
    );
    assert!(review.has_unsaved_review());
    review.present();
    review.smoke_set_draft("");
    review.hide();
    assert!(review.patch.borrow().is_none());
    assert!(review.snapshot.borrow().is_none());
    assert_eq!(review.diff.buffer().char_count(), 0);
    assert_eq!(host.visible_child(), Some(terminal.upcast()));
    review.present();
    assert_eq!(
        host.visible_child(),
        Some(review.root_widget.clone().upcast())
    );
    // Exercise embedded-view unmapping without recreating the native window surface.
    host.set_visible(false);
    assert!(!review.root_widget.is_mapped());
    assert!(review.patch.borrow().is_none());
    host.set_visible(true);
    assert!(review.root_widget.is_mapped());
    glib::future_with_timeout(std::time::Duration::from_secs(20), async {
        while review.patch.borrow().is_none() {
            glib::timeout_future(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    host.remove(&review.root_widget);
    test_window.destroy();
    println!("DIFF_REVIEW_CONTINUOUS_EMBEDDED_OK");
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    #[gtk::test]
    async fn diff_review_native_smoke() {
        adw::init().unwrap();
        let parent = adw::ApplicationWindow::builder().build();
        smoke(&parent).await;
        parent.destroy();
    }
}

fn margins(widget: &impl IsA<gtk::Widget>, value: i32) {
    widget.set_margin_start(value);
    widget.set_margin_end(value);
    widget.set_margin_top(value);
    widget.set_margin_bottom(value);
}
