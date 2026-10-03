// SPDX-License-Identifier: GPL-3.0-or-later
//! Native, file-at-a-time diff review on both GTK backends.

use adw::prelude::*;
use flowmux_vcs::review::{self, Patch, Scope, Snapshot};
use gtk::{glib, pango};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

const PAGE_LINES: usize = 500;
mod comments;

pub(crate) struct ReviewWindow {
    pub window: adw::Window,
    pub root: PathBuf,
    scope: gtk::DropDown,
    base: gtk::Entry,
    refresh: gtk::Button,
    search: gtk::SearchEntry,
    files: gtk::StringList,
    selection: gtk::SingleSelection,
    visible_files: RefCell<Vec<usize>>,
    pub status: gtk::Label,
    heading: gtk::Label,
    diff: gtk::TextView,
    previous: gtk::Button,
    next: gtk::Button,
    page_label: gtk::Label,
    page: Cell<usize>,
    snapshot: RefCell<Option<Snapshot>>,
    patch: RefCell<Option<Patch>>,
    generation: Cell<u64>,
    patch_generation: Cell<u64>,
    comments: comments::Comments,
}

impl ReviewWindow {
    pub fn new(parent: &adw::ApplicationWindow, root: PathBuf) -> Rc<Self> {
        let window = adw::Window::builder()
            .title("Diff review")
            .transient_for(parent)
            .destroy_with_parent(true)
            .hide_on_close(true)
            .default_width(1040)
            .default_height(720)
            .build();
        window.set_widget_name("flowmux-diff-review");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = adw::HeaderBar::new();
        let title = adw::WindowTitle::new("Diff review", &review::display_path(&root));
        header.set_title_widget(Some(&title));
        content.append(&header);
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        margins(&controls, 10);
        let scope =
            gtk::DropDown::from_strings(&["All changes", "Unstaged", "Staged", "Branch changes"]);
        scope.set_tooltip_text(Some("All changes compares tracked files with HEAD and includes untracked files. Branch changes compares commits from the merge base."));
        scope.update_property(&[gtk::accessible::Property::Label("Diff scope")]);
        let base = gtk::Entry::builder()
            .placeholder_text("Base branch or commit")
            .hexpand(true)
            .visible(false)
            .build();
        base.update_property(&[gtk::accessible::Property::Label("Base branch or commit")]);
        let refresh = gtk::Button::with_label("Refresh");
        controls.append(&scope);
        controls.append(&base);
        controls.append(&refresh);
        content.append(&controls);

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
                .min_content_width(180)
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
        right.append(&heading);
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
        let paging = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        margins(&paging, 8);
        let previous = gtk::Button::with_label("Previous page");
        let next = gtk::Button::with_label("Next page");
        let page_label = gtk::Label::builder().hexpand(true).build();
        paging.append(&previous);
        paging.append(&page_label);
        paging.append(&next);
        right.append(&paging);
        let comments = comments::Comments::new();
        let review_split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&right)
            .end_child(&comments.panel)
            .resize_start_child(true)
            .resize_end_child(false)
            .shrink_start_child(true)
            .shrink_end_child(true)
            .position(340)
            .build();
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Horizontal)
            .start_child(&left)
            .end_child(&review_split)
            .resize_start_child(false)
            .resize_end_child(true)
            .shrink_start_child(true)
            .shrink_end_child(true)
            .position(240)
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
        window.set_content(Some(&content));
        let this = Rc::new(Self {
            window,
            root,
            scope,
            base,
            refresh,
            search,
            files,
            selection,
            visible_files: RefCell::new(Vec::new()),
            status,
            heading,
            diff,
            previous,
            next,
            page_label,
            page: Cell::new(0),
            snapshot: RefCell::new(None),
            patch: RefCell::new(None),
            generation: Cell::new(0),
            patch_generation: Cell::new(0),
            comments,
        });
        let weak = Rc::downgrade(&this);
        this.refresh.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.reload();
            }
        });
        let weak = Rc::downgrade(&this);
        this.scope.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.base.set_visible(this.scope.selected() == 3);
                this.reload();
            }
        });
        let weak = Rc::downgrade(&this);
        this.base.connect_activate(move |_| {
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
        for (button, forward) in [(&this.previous, false), (&this.next, true)] {
            let weak = Rc::downgrade(&this);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.page.set(if forward {
                        this.page.get() + 1
                    } else {
                        this.page.get().saturating_sub(1)
                    });
                    this.show_page();
                }
            });
        }
        this.connect_comments();
        this.reload();
        this
    }

    fn scope(&self) -> Scope {
        match self.scope.selected() {
            1 => Scope::Unstaged,
            2 => Scope::Staged,
            3 => Scope::Branch(self.base.text().into()),
            _ => Scope::WorkingTree,
        }
    }

    fn reload(self: &Rc<Self>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.patch_generation
            .set(self.patch_generation.get().wrapping_add(1));
        self.snapshot.borrow_mut().take();
        self.patch.borrow_mut().take();
        self.files.splice(0, self.files.n_items(), &[]);
        self.diff.buffer().set_text("");
        self.status.set_text("Loading changes…");
        self.refresh.set_sensitive(false);
        let root = self.root.clone();
        let scope = self.scope();
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
                    this.init_comments(snapshot.root.clone());
                    this.status.set_text(&if snapshot.files.is_empty() {
                        "No changes in this scope.".into()
                    } else {
                        format!(
                            "{} changed files · Read-only comparison",
                            snapshot.files.len()
                        )
                    });
                    *this.snapshot.borrow_mut() = Some(snapshot);
                    this.filter_files();
                }
                Ok(Err(error)) => this.status.set_text(&error),
                Err(_) => this.status.set_text("Cannot load changes. Try Refresh."),
            }
        });
    }

    fn filter_files(self: &Rc<Self>) {
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
        if !labels.is_empty() {
            self.selection.set_selected(0);
        }
    }

    fn load_selected(self: &Rc<Self>) {
        let generation = self.patch_generation.get().wrapping_add(1);
        self.patch_generation.set(generation);
        self.patch.borrow_mut().take();
        self.page.set(0);
        self.show_page();
        let Some(index) = self
            .visible_files
            .borrow()
            .get(self.selection.selected() as usize)
            .copied()
        else {
            self.heading.set_text("Select a changed file");
            return;
        };
        let Some(snapshot) = self.snapshot.borrow().clone() else {
            return;
        };
        let Some(file) = snapshot.files.get(index).cloned() else {
            return;
        };
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
                    *this.patch.borrow_mut() = Some(patch);
                    this.show_page();
                }
                Ok(Err(error)) => this.diff.buffer().set_text(&error),
                Err(_) => this
                    .diff
                    .buffer()
                    .set_text("Cannot read this diff. Select the file again to retry."),
            }
        });
    }

    #[allow(deprecated)] // Named theme colors are available on our GTK 4.12 floor.
    fn show_page(&self) {
        let patch = self.patch.borrow();
        let count = patch.as_ref().map_or(0, |p| p.lines.len());
        let pages = count.div_ceil(PAGE_LINES).max(1);
        let page = self.page.get().min(pages - 1);
        self.page.set(page);
        self.previous.set_sensitive(page > 0);
        self.next.set_sensitive(page + 1 < pages);
        self.page_label.set_text(&format!(
            "Page {} / {} · {} diff lines",
            page + 1,
            pages,
            count
        ));
        let text = patch
            .as_ref()
            .map(|patch| {
                patch
                    .lines
                    .iter()
                    .skip(page * PAGE_LINES)
                    .take(PAGE_LINES)
                    .map(|line| {
                        format!(
                            "{:>6} {:>6}  {}\n",
                            line.old.map(|n| n.to_string()).unwrap_or_default(),
                            line.new.map(|n| n.to_string()).unwrap_or_default(),
                            line.text
                        )
                    })
                    .collect::<String>()
            })
            .unwrap_or_default();
        self.diff.buffer().set_text(&text);
        for (name, color) in [
            ("addition", "success_color"),
            ("deletion", "error_color"),
            ("hunk", "accent_color"),
        ] {
            let table = self.diff.buffer().tag_table();
            let tag = table.lookup(name).unwrap_or_else(|| {
                let tag = gtk::TextTag::builder().name(name).build();
                table.add(&tag);
                tag
            });
            tag.set_foreground_rgba(self.diff.style_context().lookup_color(color).as_ref());
        }
        if let Some(patch) = patch.as_ref() {
            for (i, line) in patch
                .lines
                .iter()
                .skip(page * PAGE_LINES)
                .take(PAGE_LINES)
                .enumerate()
            {
                let tag = if line.new.is_some() && line.old.is_none() {
                    "addition"
                } else if line.old.is_some() && line.new.is_none() {
                    "deletion"
                } else if line.text.starts_with("@@") {
                    "hunk"
                } else {
                    continue;
                };
                if let Some(start) = self.diff.buffer().iter_at_line(i as i32) {
                    let mut end = start;
                    end.forward_to_line_end();
                    self.diff.buffer().apply_tag_by_name(tag, &start, &end);
                }
            }
        }
        let start = self.diff.buffer().start_iter();
        self.diff.buffer().place_cursor(&start);
        self.diff
            .scroll_to_iter(&mut self.diff.buffer().start_iter(), 0.0, false, 0.0, 0.0);
    }
}

#[cfg(test)]
pub(crate) async fn smoke(parent: &adw::ApplicationWindow) {
    use std::process::Command;
    async fn wait(mut condition: impl FnMut() -> bool) {
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while !condition() {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("review UI timed out");
    }
    let directory = tempfile::tempdir().unwrap();
    assert!(Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(directory.path())
        .output()
        .unwrap()
        .status
        .success());
    for i in 0..600 {
        std::fs::write(
            directory.path().join(format!("file-{i:04}.rs")),
            "fn small() {}\n",
        )
        .unwrap();
    }
    std::fs::write(
        directory.path().join("large.rs"),
        (0..20_000)
            .map(|i| format!("line {i} 내용\n"))
            .collect::<String>(),
    )
    .unwrap();
    let review = ReviewWindow::new(parent, directory.path().into());
    println!("REVIEW_SMOKE_PRESENT");
    review.window.present();
    wait(|| review.patch.borrow().is_some() && review.window.is_mapped()).await;
    assert_eq!(review.files.n_items(), 601);
    assert!(!review.next.is_sensitive());
    review.search.set_text("large");
    wait(|| {
        review
            .patch
            .borrow()
            .as_ref()
            .is_some_and(|p| p.lines.len() > 20_000)
    })
    .await;
    assert_eq!(review.files.n_items(), 1);
    assert!(review.next.is_sensitive());
    review.next.emit_clicked();
    assert_eq!(review.page.get(), 1);
    assert!(review
        .diff
        .buffer()
        .text(
            &review.diff.buffer().start_iter(),
            &review.diff.buffer().end_iter(),
            false
        )
        .contains("line 500"));
    for _ in 0..50 {
        if review.next.is_sensitive() {
            review.next.emit_clicked();
        }
    }
    assert!(review
        .diff
        .buffer()
        .text(
            &review.diff.buffer().start_iter(),
            &review.diff.buffer().end_iter(),
            false
        )
        .contains("line 19999"));
    comments::smoke(&review).await;
    review.window.set_default_size(720, 500);
    println!("REVIEW_SMOKE_RESIZE");
    glib::timeout_future(std::time::Duration::from_millis(100)).await;
    assert!(review.diff.width() > 200);
    if let Some(directory) = std::env::var_os("FLOWMUX_REVIEW_SNAPSHOT_DIR") {
        std::fs::create_dir_all(&directory).unwrap();
        let snapshot = gtk::Snapshot::new();
        gtk::WidgetPaintable::new(Some(&review.window)).snapshot(
            &snapshot,
            review.window.width() as f64,
            review.window.height() as f64,
        );
        let node = snapshot.to_node().unwrap();
        review
            .window
            .native()
            .unwrap()
            .renderer()
            .unwrap()
            .render_texture(&node, None)
            .save_to_png(PathBuf::from(directory).join("diff-review.png"))
            .unwrap();
    }
    // Fast scope/filter changes must not render an older in-flight patch.
    review.scope.set_selected(2);
    println!("REVIEW_SMOKE_SCOPES");
    review.scope.set_selected(0);
    review.scope.set_selected(2);
    wait(|| review.refresh.is_sensitive()).await;
    assert_eq!(review.files.n_items(), 0);
    assert!(review.status.text().contains("No changes"));
    review.scope.set_selected(3);
    review.base.set_text("missing-base");
    review.refresh.emit_clicked();
    wait(|| review.refresh.is_sensitive()).await;
    assert!(review.status.text().contains("Git:"));
    review.scope.set_selected(0);
    wait(|| review.patch.borrow().is_some()).await;
    review.window.close();
    println!("REVIEW_SMOKE_CLOSED");
    assert!(!review.window.is_visible());
    // Native macOS unmaps asynchronously; a second user action arrives on a
    // later event-loop turn, not inside the close signal's call stack.
    glib::timeout_future(std::time::Duration::from_millis(50)).await;
    review.window.present();
    println!("REVIEW_SMOKE_REOPENED");
    assert!(review.window.is_visible());
    wait(|| review.window.is_mapped()).await;
    glib::timeout_future(std::time::Duration::from_millis(50)).await;
    review.window.destroy();
    println!("DIFF_REVIEW_NATIVE_SMOKE_OK");
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
