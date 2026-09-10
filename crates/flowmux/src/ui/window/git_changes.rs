// SPDX-License-Identifier: GPL-3.0-or-later
//! On-demand file review in a window pinned to one local worktree.

use super::*;
use crate::ui::editor_pane::EditorPane;
use flowmux_editor::{EditorAppearance, HostMessage};
use flowmux_vcs::changes::{self, Area, Change, Preview};
use gtk::gdk;

struct ChangesUi {
    window: glib::WeakRef<gtk::Window>,
    list: glib::WeakRef<gtk::ListBox>,
    status: glib::WeakRef<gtk::Label>,
    heading: glib::WeakRef<gtk::Label>,
    comparison: glib::WeakRef<gtk::Label>,
    viewer: glib::WeakRef<gtk::Box>,
    action: glib::WeakRef<gtk::Button>,
    refresh: glib::WeakRef<gtk::Button>,
    start: PathBuf,
    root: RefCell<Option<PathBuf>>,
    appearance: EditorAppearance,
    editor: RefCell<Option<EditorPane>>,
    files: RefCell<Vec<Change>>,
    selected: RefCell<Option<(Change, Result<Preview, String>)>>,
    generation: Cell<u64>,
    writing: Cell<bool>,
}

impl ChangesUi {
    fn current(&self, generation: u64) -> bool {
        self.generation.get() == generation && self.window.upgrade().is_some_and(|w| w.is_visible())
    }

    fn advance(&self) -> u64 {
        let next = self.generation.get().wrapping_add(1);
        self.generation.set(next);
        next
    }

    fn message(&self, text: &str) {
        if let Some(status) = self.status.upgrade() {
            status.set_text(text);
        }
    }

    fn clear_preview(&self) {
        self.selected.borrow_mut().take();
        if let Some(action) = self.action.upgrade() {
            action.set_sensitive(false);
        }
        if let Some(editor) = self.editor.borrow().as_ref() {
            editor.root.set_visible(false);
        }
        if let Some(label) = self.comparison.upgrade() {
            label.set_text("Select a changed file");
        }
    }

    fn refresh(self: &Rc<Self>) {
        if self.writing.get() {
            return;
        }
        self.clear_preview();
        if let Some(list) = self.list.upgrade() {
            list.remove_all();
        }
        self.files.borrow_mut().clear();
        let generation = self.advance();
        self.message("Loading Git changes…");
        let start = self
            .root
            .borrow()
            .clone()
            .unwrap_or_else(|| self.start.clone());
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || changes::list(&start))
                .await
                .unwrap_or_else(|_| Err("Git status worker failed".into()));
            if !ui.current(generation) {
                return;
            }
            let result = match result {
                Ok(result) => result,
                Err(error) => {
                    ui.message(&error);
                    return;
                }
            };
            if let Some(heading) = ui.heading.upgrade() {
                heading.set_text(&result.root.display().to_string());
            }
            *ui.root.borrow_mut() = Some(result.root);
            let Some(list) = ui.list.upgrade() else {
                return;
            };
            *ui.files.borrow_mut() = result.files;
            for change in ui.files.borrow().iter() {
                let label = gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .label(format!("{}   {}", change.status, change.path.display()))
                    .build();
                label.set_margin_top(8);
                label.set_margin_bottom(8);
                label.set_margin_start(8);
                label.set_margin_end(8);
                let row = gtk::ListBoxRow::new();
                row.set_child(Some(&label));
                let tooltip = change
                    .original_path
                    .as_ref()
                    .map(|p| format!("{} → {}", p.display(), change.path.display()))
                    .unwrap_or_else(|| change.path.display().to_string());
                row.set_tooltip_text(Some(&tooltip));
                list.append(&row);
            }
            ui.message(if ui.files.borrow().is_empty() {
                "Working tree clean"
            } else {
                "Select a file to compare. Refresh to include external changes."
            });
        });
    }

    fn select(self: &Rc<Self>, index: Option<usize>) {
        let generation = self.advance();
        self.clear_preview();
        let Some(change) = index.and_then(|i| self.files.borrow().get(i).cloned()) else {
            return;
        };
        let Some(root) = self.root.borrow().clone() else {
            return;
        };
        if let Some(label) = self.comparison.upgrade() {
            let pair = match change.area {
                Area::Staged => "HEAD → Index",
                Area::Untracked => "Empty → Working file",
                _ => "Index → Working file",
            };
            label.set_text(&format!(
                "{} · {} · {}",
                change.area.label(),
                pair,
                change.path.display()
            ));
        }
        if let Some(action) = self.action.upgrade() {
            action.set_label(if change.area == Area::Staged {
                "Unstage file"
            } else {
                "Stage file"
            });
        }
        self.message("Loading diff…");
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let worker_change = change.clone();
            let worker_root = root.clone();
            let result =
                gtk::gio::spawn_blocking(move || changes::preview(&worker_root, &worker_change))
                    .await
                    .unwrap_or_else(|_| Err("Git diff worker failed".into()));
            if !ui.current(generation) {
                return;
            }
            if let Ok(preview) = &result {
                if ui.editor.borrow().is_none() {
                    match EditorPane::new(
                        PaneId::new(),
                        SurfaceId::new(),
                        root,
                        flowmux_core::EditorSessionState::default(),
                        ui.appearance.clone(),
                    ) {
                        Ok(editor) => {
                            if let Some(viewer) = ui.viewer.upgrade() {
                                viewer.append(&editor.root);
                            }
                            *ui.editor.borrow_mut() = Some(editor);
                        }
                        Err(error) => {
                            ui.message(&error);
                            return;
                        }
                    }
                }
                let editor = ui.editor.borrow();
                let editor = editor.as_ref().unwrap();
                editor.root.set_visible(true);
                if let Err(error) = editor.send(HostMessage::ShowGitDiff {
                    path: change.path.display().to_string(),
                    original: preview.original.clone(),
                    modified: preview.modified.clone(),
                }) {
                    ui.message(&error.to_string());
                    return;
                }
                ui.message(&preview.note);
            } else if let Err(error) = &result {
                ui.message(error);
            }
            if let Some(action) = ui.action.upgrade() {
                action.set_sensitive(change.area != Area::Conflict && !change.submodule);
            }
            *ui.selected.borrow_mut() = Some((change, result));
        });
    }

    fn apply(self: &Rc<Self>) {
        if self.writing.get() {
            return;
        }
        let Some((change, expected)) = self.selected.borrow().clone() else {
            return;
        };
        let Some(root) = self.root.borrow().clone() else {
            return;
        };
        self.writing.set(true);
        let generation = self.advance();
        if let Some(action) = self.action.upgrade() {
            action.set_sensitive(false);
        }
        if let Some(refresh) = self.refresh.upgrade() {
            refresh.set_sensitive(false);
        }
        if let Some(list) = self.list.upgrade() {
            list.set_sensitive(false);
        }
        self.message("Updating index…");
        let ui = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                // Reject changed previews before mutation. Git itself owns index.lock;
                // a failed write is never automatically retried.
                if changes::preview(&root, &change) != expected {
                    return Err(
                        "File changed since this comparison. Refresh before staging or unstaging."
                            .into(),
                    );
                }
                changes::set_staged(&root, &change)
            })
            .await
            .unwrap_or_else(|_| Err("Git index worker failed".into()));
            ui.writing.set(false);
            if !ui.current(generation) {
                return;
            }
            if let Some(refresh) = ui.refresh.upgrade() {
                refresh.set_sensitive(true);
            }
            if let Some(list) = ui.list.upgrade() {
                list.set_sensitive(true);
            }
            match result {
                Ok(()) => ui.refresh(),
                Err(error) => {
                    ui.clear_preview();
                    ui.message(&error);
                }
            }
        });
    }
}

fn open_changes(
    parent: &impl IsA<gtk::Window>,
    start: PathBuf,
    appearance: EditorAppearance,
) -> Rc<ChangesUi> {
    let dialog = gtk::Window::builder()
        .title("Git Changes")
        .transient_for(parent)
        .default_width(1100)
        .default_height(700)
        .build();
    dialog.set_widget_name("git-changes");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let heading = gtk::Label::builder()
        .label(start.display().to_string())
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    let refresh = gtk::Button::with_label("Refresh");
    refresh.set_tooltip_text(Some("Refresh Git changes (F5)"));
    refresh.set_widget_name("git-changes-refresh");
    let action = gtk::Button::with_label("Stage file");
    action.set_widget_name("git-changes-stage");
    action.set_sensitive(false);
    toolbar.append(&heading);
    toolbar.append(&refresh);
    toolbar.append(&action);
    content.append(&toolbar);
    let split = gtk::Paned::new(gtk::Orientation::Horizontal);
    split.set_position(280);
    split.set_vexpand(true);
    let list = gtk::ListBox::new();
    list.set_widget_name("git-changes-files");
    list.set_selection_mode(gtk::SelectionMode::Single);
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .min_content_width(160)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    split.set_start_child(Some(&scroll));
    let viewer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let comparison = gtk::Label::builder()
        .label("Select a changed file")
        .xalign(0.0)
        .wrap(true)
        .build();
    comparison.set_widget_name("git-changes-comparison");
    viewer.append(&comparison);
    split.set_end_child(Some(&viewer));
    content.append(&split);
    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    status.set_widget_name("git-changes-status");
    content.append(&status);
    dialog.set_child(Some(&content));
    let ui = Rc::new(ChangesUi {
        window: dialog.downgrade(),
        list: list.downgrade(),
        status: status.downgrade(),
        heading: heading.downgrade(),
        comparison: comparison.downgrade(),
        viewer: viewer.downgrade(),
        action: action.downgrade(),
        refresh: refresh.downgrade(),
        start,
        root: RefCell::new(None),
        appearance,
        editor: RefCell::new(None),
        files: RefCell::new(Vec::new()),
        selected: RefCell::new(None),
        generation: Cell::new(0),
        writing: Cell::new(false),
    });
    list.set_header_func({
        let ui = Rc::downgrade(&ui);
        move |row, previous| {
            let Some(ui) = ui.upgrade() else {
                return;
            };
            let files = ui.files.borrow();
            let Some(change) = files.get(row.index() as usize) else {
                return;
            };
            let previous_area = previous
                .and_then(|p| files.get(p.index() as usize))
                .map(|c| c.area);
            if previous_area == Some(change.area) {
                row.set_header(gtk::Widget::NONE);
            } else {
                let header = gtk::Label::builder()
                    .label(change.area.label())
                    .xalign(0.0)
                    .build();
                header.add_css_class("heading");
                header.set_margin_top(12);
                header.set_margin_bottom(6);
                header.set_margin_start(8);
                row.set_header(Some(&header));
            }
        }
    });
    refresh.connect_clicked({
        let ui = ui.clone();
        move |_| ui.refresh()
    });
    action.connect_clicked({
        let ui = ui.clone();
        move |_| ui.apply()
    });
    list.connect_row_selected({
        let ui = ui.clone();
        move |_, row| ui.select(row.map(|r| r.index() as usize))
    });
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let ui = ui.clone();
        move |_, key, _, _| {
            if key == gdk::Key::Escape {
                if let Some(window) = ui.window.upgrade() {
                    window.close();
                }
                glib::Propagation::Stop
            } else if key == gdk::Key::F5 {
                ui.refresh();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        }
    });
    dialog.add_controller(keys);
    dialog.connect_close_request({
        let ui = ui.clone();
        move |_| {
            ui.advance();
            if let Some(editor) = ui.editor.borrow_mut().take() {
                editor.prepare_for_close();
            }
            glib::Propagation::Proceed
        }
    });
    dialog.present();
    ui.refresh();
    ui
}

impl WindowController {
    pub(super) async fn show_git_changes(&self) {
        // The window stays pinned to its source, even while other workspaces run.
        for window in gtk::Window::list_toplevels()
            .into_iter()
            .filter_map(|w| w.downcast::<gtk::Window>().ok())
        {
            if window.widget_name() == "git-changes"
                && window.transient_for().as_ref() == Some(self.window.upcast_ref())
            {
                window.present();
                return;
            }
        }
        let Some(pane) = self.focused_pane.get() else {
            return;
        };
        if self.pane_registry.borrow().is_ssh_pane(pane) {
            self.clipboard_toast
                .show_with_message("Git Changes is available for local workspaces only");
            return;
        }
        let Some(start) = self.file_browser_root_for_pane(pane).await else {
            self.clipboard_toast
                .show_with_message("No working directory is available");
            return;
        };
        let appearance = self
            .theme
            .borrow()
            .editor_appearance(&self.options.borrow());
        open_changes(&self.window, start, appearance);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn wait_for(ui: &ChangesUi, ready: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while !ready() {
            assert!(
                std::time::Instant::now() < deadline,
                "Changes UI timed out: {:?}",
                ui.status.upgrade().map(|s| s.text())
            );
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    }

    fn select(ui: &ChangesUi, path: &str, area: Area) {
        let index = ui
            .files
            .borrow()
            .iter()
            .position(|c| c.path == std::path::Path::new(path) && c.area == area)
            .unwrap();
        let list = ui.list.upgrade().unwrap();
        list.select_row(list.row_at_index(index as i32).as_ref());
    }

    #[gtk::test]
    async fn git_changes_ui_reviews_stages_refreshes_and_rejects_stale_previews() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for args in [
            vec!["init", "-b", "main"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.test"],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .output()
                .unwrap()
                .status
                .success());
        }
        std::fs::write(root.join("file.txt"), "original\n").unwrap();
        let appearance = crate::theme::ResolvedTheme::resolve(&Default::default())
            .editor_appearance(&Default::default());
        let parent = gtk::Window::new();
        let ui = open_changes(&parent, root.to_path_buf(), appearance);
        wait_for(&ui, || ui.files.borrow().len() == 1).await;
        let header = ui
            .list
            .upgrade()
            .unwrap()
            .row_at_index(0)
            .unwrap()
            .header()
            .unwrap();
        wait_for(&ui, || header.is_mapped()).await;
        assert_eq!(header.downcast::<gtk::Label>().unwrap().text(), "Untracked");
        select(&ui, "file.txt", Area::Untracked);
        wait_for(&ui, || ui.selected.borrow().is_some()).await;
        assert!(ui.action.upgrade().unwrap().is_sensitive());
        assert!(ui
            .comparison
            .upgrade()
            .unwrap()
            .text()
            .contains("Empty → Working file"));
        // An external edit after review requires a new comparison.
        std::fs::write(root.join("file.txt"), "external\n").unwrap();
        ui.action.upgrade().unwrap().emit_clicked();
        wait_for(&ui, || !ui.writing.get()).await;
        assert!(ui
            .status
            .upgrade()
            .unwrap()
            .text()
            .contains("File changed since"));
        assert_eq!(changes::list(root).unwrap().files[0].area, Area::Untracked);
        ui.refresh();
        wait_for(&ui, || ui.files.borrow().len() == 1).await;
        select(&ui, "file.txt", Area::Untracked);
        wait_for(&ui, || ui.selected.borrow().is_some()).await;
        ui.action.upgrade().unwrap().emit_clicked();
        wait_for(&ui, || {
            ui.files.borrow().iter().any(|c| c.area == Area::Staged)
        })
        .await;
        select(&ui, "file.txt", Area::Staged);
        wait_for(&ui, || ui.selected.borrow().is_some()).await;
        assert_eq!(
            ui.action.upgrade().unwrap().label().as_deref(),
            Some("Unstage file")
        );
        ui.action.upgrade().unwrap().emit_clicked();
        wait_for(&ui, || {
            ui.files.borrow().iter().any(|c| c.area == Area::Untracked)
        })
        .await;
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "external\n"
        );
        std::fs::write(root.join("binary"), [0, 255]).unwrap();
        ui.refresh();
        wait_for(&ui, || ui.files.borrow().len() == 2).await;
        select(&ui, "binary", Area::Untracked);
        wait_for(&ui, || ui.selected.borrow().is_some()).await;
        assert!(ui.status.upgrade().unwrap().text().contains("Binary file"));
        assert!(ui.action.upgrade().unwrap().is_sensitive());
        assert!(!ui.editor.borrow().as_ref().unwrap().root.is_visible());
        // Queue one preview then a newer one: only the newest can publish.
        select(&ui, "file.txt", Area::Untracked);
        select(&ui, "binary", Area::Untracked);
        wait_for(&ui, || ui.selected.borrow().is_some()).await;
        assert_eq!(
            ui.selected.borrow().as_ref().unwrap().0.path,
            PathBuf::from("binary")
        );
        ui.refresh();
        let generation = ui.generation.get();
        ui.window.upgrade().unwrap().close();
        assert!(!ui.current(generation));
        glib::timeout_future(Duration::from_millis(100)).await;
        assert!(ui.editor.borrow().is_none());
        parent.destroy();
    }

    #[gtk::test]
    async fn git_changes_ui_non_repository_and_clean_states() {
        let dir = tempfile::tempdir().unwrap();
        let appearance = crate::theme::ResolvedTheme::resolve(&Default::default())
            .editor_appearance(&Default::default());
        let parent = gtk::Window::new();
        let ui = open_changes(&parent, dir.path().to_path_buf(), appearance);
        wait_for(&ui, || {
            ui.status.upgrade().unwrap().text() == "Not a Git repository"
        })
        .await;
        assert!(!ui.action.upgrade().unwrap().is_sensitive());
        assert!(std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .status
            .success());
        ui.refresh();
        wait_for(&ui, || {
            ui.status.upgrade().unwrap().text() == "Working tree clean"
        })
        .await;
        assert!(!ui.action.upgrade().unwrap().is_sensitive());
        assert!(ui.files.borrow().is_empty());
        ui.window.upgrade().unwrap().close();
        parent.destroy();
    }
}
