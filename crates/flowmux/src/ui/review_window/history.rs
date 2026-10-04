// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_vcs::review::history;

pub(super) struct History {
    pub menu: gtk::MenuButton,
    pub scope: RefCell<Scope>,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    status: gtk::Button,
    commits: RefCell<Vec<history::Commit>>,
    tip: RefCell<Option<String>>,
    loading: Cell<bool>,
    has_more: Cell<bool>,
    generation: Cell<u64>,
}

impl History {
    pub fn new() -> Self {
        let menu = gtk::MenuButton::builder()
            .label("Unstaged + Staged")
            .build();
        menu.set_tooltip_text(Some("Choose uncommitted changes or a commit to review"));
        menu.set_widget_name("flowmux-review-scope");
        let content = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.append(&Self::label(
            "Unstaged + Staged",
            "Includes new untracked files",
        ));
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_width(300)
            .min_content_height(320)
            .child(&list)
            .build();
        content.append(&scroll);
        let status = gtk::Button::with_label("Loading commits…");
        status.add_css_class("flat");
        content.append(&status);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&content));
        menu.set_popover(Some(&popover));
        Self {
            menu,
            scope: RefCell::new(Scope::WorkingTree),
            list,
            scroll,
            status,
            commits: RefCell::new(Vec::new()),
            tip: RefCell::new(None),
            loading: Cell::new(false),
            has_more: Cell::new(true),
            generation: Cell::new(0),
        }
    }

    fn label(text: &str, tooltip: &str) -> gtk::Label {
        let label = gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(48)
            .build();
        label.set_tooltip_text(Some(tooltip));
        margins(&label, 8);
        label
    }
}

impl ReviewWindow {
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) async fn smoke_select_latest_commit(self: &Rc<Self>) -> String {
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while !self.refresh.is_sensitive() {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.smoke_wait_for_comments().await;
        self.history.menu.popup();
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while self.history.loading.get() {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let oid = self.history.commits.borrow()[0].oid.clone();
        let row = self.history.list.row_at_index(1).unwrap();
        self.history
            .list
            .emit_by_name::<()>("row-activated", &[&row]);
        assert_eq!(
            *self.history.scope.borrow(),
            Scope::Commit(oid.clone()),
            "{}",
            self.status.text()
        );
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while self.patch.borrow().is_none() {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        self.smoke_wait_for_comments().await;
        assert_eq!(
            self.snapshot.borrow().as_ref().unwrap().scope,
            Scope::Commit(oid.clone())
        );
        oid
    }

    pub(super) fn connect_history(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.history
            .menu
            .popover()
            .unwrap()
            .connect_visible_notify(move |popover| {
                if !popover.is_visible() {
                    return;
                }
                let Some(this) = weak.upgrade() else {
                    return;
                };
                // A fresh opening follows the current HEAD. Subsequent pages stay
                // pinned to that HEAD, even if another agent commits while scrolling.
                this.history
                    .generation
                    .set(this.history.generation.get().wrapping_add(1));
                this.history.loading.set(false);
                this.history.has_more.set(true);
                this.history.tip.borrow_mut().take();
                this.history.commits.borrow_mut().clear();
                while let Some(row) = this.history.list.row_at_index(1) {
                    this.history.list.remove(&row);
                }
                this.history.scroll.vadjustment().set_value(0.0);
                this.load_history_page();
            });
        let weak = Rc::downgrade(self);
        self.history
            .scroll
            .vadjustment()
            .connect_value_changed(move |adjustment| {
                if adjustment.upper() > adjustment.page_size()
                    && adjustment.value() + adjustment.page_size() >= adjustment.upper() - 24.0
                {
                    if let Some(this) = weak.upgrade() {
                        this.load_history_page();
                    }
                }
            });
        let weak = Rc::downgrade(self);
        self.history.status.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.load_history_page();
            }
        });
        let weak = Rc::downgrade(self);
        self.history.list.connect_row_activated(move |_, row| {
            let Some(this) = weak.upgrade() else {
                return;
            };
            let commit = if row.index() == 0 {
                None
            } else {
                this.history
                    .commits
                    .borrow()
                    .get(row.index() as usize - 1)
                    .cloned()
            };
            this.select_review_scope(commit);
        });
    }

    fn select_review_scope(self: &Rc<Self>, commit: Option<history::Commit>) {
        self.history.menu.popdown();
        let scope = commit
            .as_ref()
            .map_or(Scope::WorkingTree, |c| Scope::Commit(c.oid.clone()));
        if *self.history.scope.borrow() == scope {
            return;
        }
        if self.has_unsaved_review() {
            self.status
                .set_text("Save or cancel the current comment before switching review scope.");
            self.focus_comment();
            return;
        }
        self.clear_comment();
        self.pending_note.borrow_mut().take();
        *self.history.scope.borrow_mut() = scope;
        // Keep the toolbar compact; the full subject and OID remain in the tooltip.
        if let Some(commit) = commit {
            self.history
                .menu
                .set_label(&format!("Commit {}", &commit.oid[..8]));
            self.history
                .menu
                .set_tooltip_text(Some(&format!("{}\n{}", commit.oid, commit.subject)));
        } else {
            self.history.menu.set_label("Unstaged + Staged");
            self.history.menu.set_tooltip_text(Some(
                "Current checkout changes, including new untracked files",
            ));
        }
        self.selected_path.borrow_mut().take();
        self.search.set_text("");
        self.reload();
        self.render_comments();
        self.update_delivery_controls();
    }

    fn load_history_page(self: &Rc<Self>) {
        if self.history.loading.get() || !self.history.has_more.get() {
            return;
        }
        self.history.loading.set(true);
        self.history.status.set_label("Loading commits…");
        self.history.status.set_sensitive(false);
        let generation = self.history.generation.get();
        let root = self.root.clone();
        let tip = self.history.tip.borrow().clone();
        let offset = self.history.commits.borrow().len();
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let result =
                gtk::gio::spawn_blocking(move || history::load(&root, tip.as_deref(), offset))
                    .await;
            let Some(this) = weak.upgrade() else {
                return;
            };
            if this.history.generation.get() != generation {
                return;
            }
            match result {
                Ok(Ok(page)) => {
                    *this.history.tip.borrow_mut() = page.tip;
                    this.history.has_more.set(page.has_more);
                    for commit in &page.commits {
                        let text = format!("{}  {}", &commit.oid[..8], commit.subject);
                        this.history.list.append(&History::label(
                            &text,
                            &format!("{}\n{}", commit.oid, commit.subject),
                        ));
                    }
                    this.history.commits.borrow_mut().extend(page.commits);
                    this.history.status.set_label(if page.has_more {
                        "Scroll for more · Load 50 more"
                    } else if this.history.commits.borrow().is_empty() {
                        "No commits yet"
                    } else {
                        "All commits loaded"
                    });
                    this.history.status.set_sensitive(page.has_more);
                }
                error => {
                    let message = match error {
                        Ok(Err(e)) => e,
                        _ => "Cannot load commits".into(),
                    };
                    this.history
                        .status
                        .set_label("Could not load commits · Retry");
                    this.history.status.set_tooltip_text(Some(&message));
                    this.history.status.set_sensitive(true);
                }
            }
            this.history.loading.set(false);
        });
    }
}

#[cfg(test)]
pub(super) async fn smoke(parent: &adw::ApplicationWindow) {
    use flowmux_state::review_drafts::DraftStore;
    async fn wait_for(check: impl Fn() -> bool) {
        glib::future_with_timeout(std::time::Duration::from_secs(20), async {
            while !check() {
                glib::timeout_future(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    let dir = gtk::gio::spawn_blocking(|| {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "JunsuChoi")
                .env("GIT_AUTHOR_EMAIL", "jsuya.choi@samsung.com")
                .env("GIT_COMMITTER_NAME", "JunsuChoi")
                .env("GIT_COMMITTER_EMAIL", "jsuya.choi@samsung.com")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        for i in 0..103 {
            std::fs::write(dir.path().join("committed.txt"), format!("revision {i}\n")).unwrap();
            git(&["add", "."]);
            git(&[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                &format!("Review commit {i}"),
            ]);
        }
        std::fs::write(dir.path().join("staged.txt"), "staged only\n").unwrap();
        git(&["add", "staged.txt"]);
        std::fs::write(dir.path().join("committed.txt"), "working change\n").unwrap();
        std::fs::write(dir.path().join("new.txt"), "untracked\n").unwrap();
        dir
    })
    .await
    .unwrap();
    let window = adw::ApplicationWindow::builder()
        .transient_for(parent)
        .default_width(1000)
        .default_height(700)
        .build();
    let host = gtk::Stack::new();
    host.add_child(&gtk::Label::new(Some("Terminal")));
    window.set_content(Some(&host));
    let review = ReviewWindow::new(
        &window,
        &host,
        flowmux_core::PaneId::new(),
        dir.path().into(),
    );
    review.present();
    window.present();
    review
        .smoke_wait_for_files(&["committed.txt", "new.txt", "staged.txt"])
        .await;
    review.smoke_save_file_comment("Working review only").await;
    review.history.menu.popup();
    wait_for(|| !review.history.loading.get() && review.history.commits.borrow().len() == 50).await;
    assert!(review.history.menu.popover().unwrap().is_mapped());
    assert_eq!(
        review.history.commits.borrow()[0].subject,
        "Review commit 102"
    );
    let latest = review.history.commits.borrow()[0].clone();
    // Drive the real scrolled dropdown, not the pagination loader directly.
    let adjustment = review.history.scroll.vadjustment();
    wait_for(|| adjustment.upper() > adjustment.page_size()).await;
    adjustment.set_value(adjustment.upper() - adjustment.page_size());
    wait_for(|| !review.history.loading.get() && review.history.commits.borrow().len() == 100)
        .await;
    wait_for(|| adjustment.value() + adjustment.page_size() < adjustment.upper() - 24.0).await;
    adjustment.set_value(adjustment.upper() - adjustment.page_size());
    wait_for(|| !review.history.loading.get() && review.history.commits.borrow().len() == 103)
        .await;
    assert!(!review.history.has_more.get());
    let oldest = review.history.commits.borrow()[102].clone();
    let row = review.history.list.row_at_index(1).unwrap();
    review
        .history
        .list
        .emit_by_name::<()>("row-activated", &[&row]);
    wait_for(|| {
        review.patch.borrow().is_some()
            && review
                .snapshot
                .borrow()
                .as_ref()
                .is_some_and(|s| s.scope == Scope::Commit(latest.oid.clone()))
    })
    .await;
    review.smoke_wait_for_comments().await;
    assert_eq!(review.files.n_items(), 1);
    assert!(review
        .patch
        .borrow()
        .as_ref()
        .unwrap()
        .text
        .contains("+revision 102"));
    assert_eq!(review.targets.menu.label().as_deref(), Some("Send · 0"));
    review
        .smoke_save_file_comment("Latest commit review only")
        .await;
    review.send_review(None);
    review.smoke_wait_for_comments().await;
    let copied = window
        .clipboard()
        .read_text_future()
        .await
        .unwrap()
        .unwrap();
    assert!(copied.contains(&latest.oid) && copied.contains("Latest commit review only"));
    assert!(!copied.contains("Working review only"));
    review.select_review_scope(Some(oldest.clone()));
    wait_for(|| {
        review.patch.borrow().is_some()
            && review
                .snapshot
                .borrow()
                .as_ref()
                .is_some_and(|s| s.scope == Scope::Commit(oldest.oid.clone()))
    })
    .await;
    review.smoke_wait_for_comments().await;
    assert!(review
        .patch
        .borrow()
        .as_ref()
        .unwrap()
        .text
        .contains("+revision 0"));
    assert_eq!(review.targets.menu.label().as_deref(), Some("Send · 0"));
    review.begin_comment(true);
    review.smoke_set_draft("unsaved historical feedback");
    review.select_review_scope(None);
    assert_eq!(
        *review.history.scope.borrow(),
        Scope::Commit(oldest.oid.clone())
    );
    assert!(review.has_unsaved_review());
    review.cancel_or_close();
    review
        .smoke_save_file_comment("Root commit review only")
        .await;
    review.select_review_scope(None);
    review
        .smoke_wait_for_files(&["committed.txt", "new.txt", "staged.txt"])
        .await;
    assert_eq!(review.targets.menu.label().as_deref(), Some("Send · 1"));
    review.send_review(None);
    review.smoke_wait_for_comments().await;
    let copied = window
        .clipboard()
        .read_text_future()
        .await
        .unwrap()
        .unwrap();
    assert!(
        copied.contains("Review target: Unstaged + Staged")
            && copied.contains("Working review only")
    );
    assert!(
        !copied.contains("Latest commit review only")
            && !copied.contains("Root commit review only")
    );
    review.smoke_remove_scope_comments();
    review.smoke_wait_for_comments().await;
    let stored = DraftStore::default_store()
        .unwrap()
        .load(&review.root)
        .unwrap();
    assert_eq!(stored.notes.len(), 2);
    assert!(stored
        .notes
        .iter()
        .all(|n| matches!(n.scope, Scope::Commit(_))));
    review.select_review_scope(Some(latest.clone()));
    wait_for(|| review.patch.borrow().is_some()).await;
    review.smoke_wait_for_comments().await;
    review.hide();
    review.present();
    wait_for(|| review.patch.borrow().is_some()).await;
    review.smoke_wait_for_comments().await;
    assert_eq!(
        review.snapshot.borrow().as_ref().unwrap().scope,
        Scope::Commit(latest.oid)
    );
    assert_eq!(review.targets.menu.label().as_deref(), Some("Send · 1"));
    review.detach();
    window.close();
    println!("DIFF_REVIEW_COMMIT_HISTORY_PAGING_SCOPES_OK");
}
