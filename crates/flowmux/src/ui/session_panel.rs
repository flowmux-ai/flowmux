// SPDX-License-Identifier: GPL-3.0-or-later
//! Native session history panel. Disk reads and terminal actions live in the controller.

use crate::bridge::{Bridge, GtkCommand};
use adw::prelude::*;
use flowmux_state::session_history::HistorySession;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum SessionPanelAction {
    Toggle,
    Refresh,
    Close,
    Select {
        session: HistorySession,
        generation: u64,
    },
    Resume {
        session: HistorySession,
        generation: u64,
    },
}

fn send(bridge: &Bridge, action: SessionPanelAction) {
    let bridge = bridge.clone();
    glib::MainContext::default().spawn_local(async move {
        let _ = bridge.tx.send(GtkCommand::SessionPanel(action)).await;
    });
}

#[derive(Clone)]
pub struct SessionPanel {
    pub root: gtk::Box,
    pub status: gtk::Label,
    pub preview: gtk::TextView,
    pub resume: gtk::Button,
    pub generation: Rc<Cell<u64>>,
    pub selection: Rc<RefCell<Option<String>>>,
    rows: Rc<RefCell<Vec<HistorySession>>>,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
}

impl SessionPanel {
    pub fn new(bridge: Bridge) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_widget_name("flowmux-session-panel");
        root.set_size_request(200, -1);
        root.set_visible(false);
        root.set_vexpand(true);
        root.set_margin_top(8);
        root.set_margin_bottom(8);
        root.set_margin_start(8);
        root.set_margin_end(8);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        let title = gtk::Label::new(Some("Agent sessions"));
        title.add_css_class("heading");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        header.append(&title);
        for (icon, label, action) in [
            (
                "view-refresh-symbolic",
                "Refresh sessions",
                SessionPanelAction::Refresh,
            ),
            (
                "window-close-symbolic",
                "Close sessions",
                SessionPanelAction::Close,
            ),
        ] {
            let button = gtk::Button::from_icon_name(icon);
            button.add_css_class("flat");
            button.set_tooltip_text(Some(label));
            button.update_property(&[gtk::accessible::Property::Label(label)]);
            let bridge = bridge.clone();
            button.connect_clicked(move |_| send(&bridge, action.clone()));
            header.append(&button);
        }
        root.append(&header);
        let status = gtk::Label::new(None);
        status.set_xalign(0.0);
        status.set_wrap(true);
        root.append(&status);
        let search = gtk::SearchEntry::new();
        search.set_placeholder_text(Some("Search sessions, paths, or IDs"));
        root.append(&search);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("boxed-list");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(140)
            .build();
        let preview = gtk::TextView::new();
        preview.set_widget_name("flowmux-session-preview");
        preview.set_editable(false);
        preview.set_cursor_visible(false);
        preview.set_wrap_mode(gtk::WrapMode::WordChar);
        let preview_scroll = gtk::ScrolledWindow::builder()
            .child(&preview)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(120)
            .build();
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&scroll)
            .end_child(&preview_scroll)
            .position(260)
            .vexpand(true)
            .build();
        root.append(&split);
        let resume = gtk::Button::with_label("Resume in focused tab");
        resume.set_widget_name("flowmux-session-resume");
        resume.set_tooltip_text(Some("Insert the native resume command into an empty agent prompt, then press Enter there to confirm."));
        resume.set_sensitive(false);
        root.append(&resume);
        let generation = Rc::new(Cell::new(0));
        let rows = Rc::new(RefCell::new(Vec::<HistorySession>::new()));
        let rows_for_selection = rows.clone();
        let generation_for_selection = generation.clone();
        let bridge_for_selection = bridge.clone();
        list.connect_row_selected(move |_, row| {
            if let Some(session) = row.and_then(|row| {
                rows_for_selection
                    .borrow()
                    .get(row.index() as usize)
                    .cloned()
            }) {
                send(
                    &bridge_for_selection,
                    SessionPanelAction::Select {
                        session,
                        generation: generation_for_selection.get(),
                    },
                );
            }
        });
        let rows_for_resume = rows.clone();
        let list_for_resume = list.clone();
        let generation_for_resume = generation.clone();
        let bridge_for_resume = bridge.clone();
        resume.connect_clicked(move |_| {
            if let Some(session) = list_for_resume
                .selected_row()
                .and_then(|row| rows_for_resume.borrow().get(row.index() as usize).cloned())
            {
                send(
                    &bridge_for_resume,
                    SessionPanelAction::Resume {
                        session,
                        generation: generation_for_resume.get(),
                    },
                );
            }
        });
        let search_list = list.clone();
        search.connect_search_changed(move |search| {
            let query = search.text().to_lowercase();
            let mut child = search_list.first_child();
            while let Some(row) = child {
                let next = row.next_sibling();
                row.set_visible(
                    row.tooltip_text()
                        .is_some_and(|text| text.to_lowercase().contains(&query)),
                );
                child = next;
            }
        });
        let bridge_for_key = bridge.clone();
        let key = gtk::EventControllerKey::new();
        key.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                send(&bridge_for_key, SessionPanelAction::Close);
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        root.add_controller(key);
        Self {
            root,
            status,
            preview,
            resume,
            list,
            search,
            generation,
            rows,
            selection: Rc::new(RefCell::new(None)),
        }
    }

    pub fn clear(&self, status: &str) -> u64 {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        self.selection.borrow_mut().take();
        self.resume.set_sensitive(false);
        self.preview.buffer().set_text("");
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.rows.borrow_mut().clear();
        self.status.set_text(status);
        generation
    }

    pub fn set_rows(&self, sessions: Vec<HistorySession>, generation: u64) {
        if generation != self.generation.get() {
            return;
        }
        *self.rows.borrow_mut() = sessions;
        for session in self.rows.borrow().iter() {
            let row = gtk::ListBoxRow::new();
            row.set_tooltip_text(Some(&format!(
                "{}\n{}\n{}\n{}",
                session.title,
                session.summary,
                session.cwd.display(),
                session.id
            )));
            let content = gtk::Box::new(gtk::Orientation::Vertical, 3);
            for text in [
                &session.title,
                &session.summary,
                &session.cwd.to_string_lossy().into_owned(),
            ] {
                let label = gtk::Label::new(Some(text));
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
                label.set_max_width_chars(44);
                content.append(&label);
            }
            row.set_child(Some(&content));
            self.list.append(&row);
        }
        self.search.emit_by_name::<()>("search-changed", &[]);
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    use flowmux_state::session_history::SessionAgent;

    #[gtk::test]
    async fn selection_search_and_refresh_keep_native_rows_consistent() {
        let (bridge, rx) = Bridge::new();
        let panel = SessionPanel::new(bridge);
        assert!(!panel.root.is_visible());
        panel.root.set_visible(true);
        let generation = panel.clear("Loading");
        let item = HistorySession {
            agent: SessionAgent::Codex,
            id: "12345678-1234-4234-8234-123456789abc".into(),
            title: "한글 title".into(),
            summary: "summary".into(),
            cwd: "/project with spaces".into(),
            path: "/unused".into(),
            modified: std::time::SystemTime::now(),
        };
        panel.set_rows(vec![item.clone()], generation);
        assert!(panel.root.measure(gtk::Orientation::Horizontal, -1).0 <= 220);
        let row = panel.list.row_at_index(0).unwrap();
        panel.list.select_row(Some(&row));
        match rx.recv().await.unwrap() {
            GtkCommand::SessionPanel(SessionPanelAction::Select {
                session,
                generation: received,
            }) => {
                assert_eq!(session.id, item.id);
                assert_eq!(received, generation);
            }
            other => panic!("Unexpected command: {other:?}"),
        }
        panel.search.set_text("spaces");
        panel.search.emit_by_name::<()>("search-changed", &[]);
        assert!(row.is_visible());
        panel.search.set_text("missing");
        panel.search.emit_by_name::<()>("search-changed", &[]);
        assert!(!row.is_visible());
        panel.clear("Different pane");
        panel.set_rows(vec![item], generation);
        assert!(panel.list.row_at_index(0).is_none());
        assert!(!panel.resume.is_sensitive());
    }
}
