// SPDX-License-Identifier: GPL-3.0-or-later
//! Native session history panel. Disk reads and terminal actions live in the controller.

use crate::bridge::{Bridge, GtkCommand};
use adw::prelude::*;
use flowmux_state::session_history::{HistorySession, SessionAgent};
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum SessionPanelAction {
    Toggle,
    Refresh,
    Close,
    ToggleAgent(SessionAgent),
    Select {
        session: HistorySession,
        generation: u64,
    },
    Resume {
        session: HistorySession,
        generation: u64,
        yolo: bool,
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
    pub selected_agent: Rc<Cell<Option<SessionAgent>>>,
    agent_buttons: Vec<(SessionAgent, gtk::ToggleButton)>,
    rows: Rc<RefCell<Vec<HistorySession>>>,
    list: gtk::ListBox,
    search: gtk::SearchEntry,
    colors: Rc<RefCell<HashMap<PathBuf, String>>>,
}

impl SessionPanel {
    pub fn new(bridge: Bridge) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_widget_name("flowmux-session-panel");
        root.add_css_class("flowmux-session-panel");
        root.set_size_request(200, -1);
        root.set_visible(false);
        root.set_vexpand(true);
        root.set_margin_top(8);
        root.set_margin_bottom(8);
        root.set_margin_start(8);
        root.set_margin_end(8);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        header.set_margin_start(8);
        header.set_margin_end(8);
        let title = gtk::Label::new(Some("Agent sessions"));
        title.add_css_class("heading");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        header.append(&title);
        let agents = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        agents.set_margin_start(8);
        agents.set_margin_end(8);
        let agent_buttons = [
            SessionAgent::Claude,
            SessionAgent::Codex,
            SessionAgent::OpenCode,
            SessionAgent::Antigravity,
            SessionAgent::Cline,
            SessionAgent::Gemini,
        ]
        .into_iter()
        .map(|agent| {
            let button = gtk::ToggleButton::new();
            button.set_icon_name(crate::builtin_icons::agent_icon_name(agent.name()));
            button.add_css_class("flat");
            let label = format!("{} sessions", agent.name());
            button.set_tooltip_text(Some(&format!(
                "{label} · Click again to follow the focused pane"
            )));
            button.update_property(&[gtk::accessible::Property::Label(&label)]);
            let bridge = bridge.clone();
            button.connect_clicked(move |_| send(&bridge, SessionPanelAction::ToggleAgent(agent)));
            agents.append(&button);
            (agent, button)
        })
        .collect();
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
        root.append(&agents);
        let status = gtk::Label::new(None);
        status.set_margin_start(8);
        status.set_margin_end(8);
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.add_css_class("caption");
        status.add_css_class("dim-label");
        root.append(&status);
        let search = gtk::SearchEntry::new();
        search.set_margin_start(8);
        search.set_margin_end(8);
        search.set_placeholder_text(Some("Search sessions, paths, or IDs"));
        root.append(&search);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.add_css_class("flowmux-session-list");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(140)
            .vexpand(true)
            .build();
        let preview = gtk::TextView::new();
        preview.set_widget_name("flowmux-session-preview");
        preview.set_editable(false);
        preview.set_cursor_visible(false);
        preview.set_wrap_mode(gtk::WrapMode::WordChar);
        preview.set_left_margin(10);
        preview.set_right_margin(10);
        preview.set_top_margin(8);
        preview.set_bottom_margin(8);
        let preview_scroll = gtk::ScrolledWindow::builder()
            .child(&preview)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(150)
            .max_content_height(150)
            .propagate_natural_height(true)
            .build();
        let split = gtk::Paned::builder()
            .orientation(gtk::Orientation::Vertical)
            .start_child(&scroll)
            .end_child(&preview_scroll)
            .resize_start_child(true)
            .resize_end_child(false)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .vexpand(true)
            .build();
        root.append(&split);
        let resume = gtk::Button::with_label("Resume in new tab");
        resume.set_widget_name("flowmux-session-resume");
        resume.set_tooltip_text(Some(
            "Open the selected session in its project directory in a new terminal tab.",
        ));
        resume.set_sensitive(false);
        resume.set_hexpand(true);
        let yolo = gtk::CheckButton::with_label("YOLO");
        yolo.set_widget_name("flowmux-session-yolo");
        yolo.set_tooltip_text(Some(
            "Automatically approve agent actions for the resumed session.",
        ));
        yolo.set_active(true);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.append(&resume);
        actions.append(&yolo);
        root.append(&actions);
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
                        yolo: yolo.is_active(),
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
            selected_agent: Rc::new(Cell::new(None)),
            agent_buttons,
            colors: Rc::new(RefCell::new(HashMap::new())),
        }
    }

    pub fn toggle_agent(&self, agent: SessionAgent) {
        let selected = (self.selected_agent.get() != Some(agent)).then_some(agent);
        self.selected_agent.set(selected);
        for (agent, button) in &self.agent_buttons {
            button.set_active(selected == Some(*agent));
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
        let mut colors = self.colors.borrow_mut();
        let mut used: Vec<_> = colors.values().cloned().collect();
        for session in self.rows.borrow().iter() {
            let row = gtk::ListBoxRow::new();
            row.add_css_class("flowmux-session-row");
            row.set_tooltip_text(Some(&format!(
                "{} · {}\n{}\n{}\n{}",
                session.agent.name(),
                session.title,
                session.summary,
                session.cwd.display(),
                session.id
            )));
            let color = colors.entry(session.cwd.clone()).or_insert_with(|| {
                use std::hash::{Hash, Hasher};
                let mut hash = std::collections::hash_map::DefaultHasher::new();
                session.cwd.hash(&mut hash);
                let color = flowmux_core::pick_workspace_color(&used, hash.finish() as u128);
                used.push(color.clone());
                color
            });
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            content.append(&super::sidebar::color_bar(color));
            let icon = super::agent_icon(session.agent.name());
            icon.set_pixel_size(16);
            icon.set_valign(gtk::Align::Start);
            icon.set_margin_top(2);
            icon.set_tooltip_text(Some(session.agent.name()));
            icon.update_property(&[gtk::accessible::Property::Label(session.agent.name())]);
            content.append(&icon);
            let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
            text.set_hexpand(true);
            let title = gtk::Label::new(Some(&session.title));
            title.add_css_class("flowmux-session-title");
            title.set_xalign(0.0);
            title.set_ellipsize(gtk::pango::EllipsizeMode::End);
            title.set_max_width_chars(44);
            text.append(&title);
            let metadata = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            metadata.add_css_class("caption");
            metadata.add_css_class("dim-label");
            let project = gtk::Label::new(Some(
                &session
                    .cwd
                    .file_name()
                    .unwrap_or(session.cwd.as_os_str())
                    .to_string_lossy(),
            ));
            project.set_xalign(0.0);
            project.set_hexpand(true);
            project.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            project.set_max_width_chars(24);
            metadata.append(&project);
            let modified: chrono::DateTime<chrono::Local> = session.modified.into();
            metadata.append(&gtk::Label::new(Some(
                &modified.format("%m-%d %H:%M").to_string(),
            )));
            text.append(&metadata);
            content.append(&text);
            row.set_child(Some(&content));
            self.list.append(&row);
        }
        self.search.emit_by_name::<()>("search-changed", &[]);
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    async fn agent_buttons_select_one_provider_and_toggle_back_to_following_focus() {
        let (bridge, rx) = Bridge::new();
        let panel = SessionPanel::new(bridge);
        assert_eq!(panel.selected_agent.get(), None);
        for (agent, button) in &panel.agent_buttons {
            let icon = button.child().unwrap().downcast::<gtk::Image>().unwrap();
            assert_eq!(
                icon.icon_name().as_deref(),
                Some(crate::builtin_icons::agent_icon_name(agent.name()))
            );
            button.emit_clicked();
            match rx.recv().await.unwrap() {
                GtkCommand::SessionPanel(SessionPanelAction::ToggleAgent(received)) => {
                    assert_eq!(received, *agent);
                    panel.toggle_agent(received);
                }
                other => panic!("Unexpected command: {other:?}"),
            }
            assert_eq!(panel.selected_agent.get(), Some(*agent));
            assert_eq!(
                panel
                    .agent_buttons
                    .iter()
                    .filter(|(_, b)| b.is_active())
                    .count(),
                1
            );
        }
        let (agent, button) = panel.agent_buttons.last().unwrap();
        button.emit_clicked();
        match rx.recv().await.unwrap() {
            GtkCommand::SessionPanel(SessionPanelAction::ToggleAgent(received)) => {
                assert_eq!(received, *agent);
                panel.toggle_agent(received);
            }
            other => panic!("Unexpected command: {other:?}"),
        }
        assert_eq!(panel.selected_agent.get(), None);
        assert!(panel
            .agent_buttons
            .iter()
            .all(|(_, button)| !button.is_active()));
        assert!(
            rx.try_recv().is_err(),
            "synchronizing buttons must not emit actions"
        );
    }

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
        assert!(panel.root.measure(gtk::Orientation::Horizontal, -1).0 <= 360);
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
        let yolo = panel
            .resume
            .next_sibling()
            .unwrap()
            .downcast::<gtk::CheckButton>()
            .unwrap();
        assert!(yolo.is_active(), "YOLO must be checked by default");
        panel.resume.set_sensitive(true);
        for enabled in [true, false] {
            yolo.set_active(enabled);
            panel.resume.emit_clicked();
            yolo.set_active(!enabled);
            match rx.recv().await.unwrap() {
                GtkCommand::SessionPanel(SessionPanelAction::Resume {
                    session,
                    generation: received,
                    yolo,
                }) => {
                    assert_eq!(session.id, item.id);
                    assert_eq!(received, generation);
                    assert_eq!(yolo, enabled, "capture the mode when Resume is clicked");
                }
                other => panic!("Unexpected command: {other:?}"),
            }
        }
        panel.search.set_text("spaces");
        panel.search.emit_by_name::<()>("search-changed", &[]);
        assert!(row.is_visible());
        panel.search.set_text("missing");
        panel.search.emit_by_name::<()>("search-changed", &[]);
        assert!(!row.is_visible());
        panel.clear("Different pane");
        panel.set_rows(vec![item.clone()], generation);
        assert!(panel.list.row_at_index(0).is_none());
        assert!(!panel.resume.is_sensitive());

        let original_color = panel.colors.borrow()[&item.cwd].clone();
        let mut same_project = item.clone();
        same_project.agent = SessionAgent::Claude;
        same_project.id = uuid::Uuid::new_v4().to_string();
        let mut other_project = item.clone();
        other_project.id = uuid::Uuid::new_v4().to_string();
        other_project.cwd = "/other/project with spaces".into();
        panel.set_rows(
            vec![other_project.clone(), same_project, item.clone()],
            panel.generation.get(),
        );
        let colors = panel.colors.borrow();
        assert_eq!(colors.len(), 2, "sessions share their project's color");
        assert_eq!(colors[&item.cwd], original_color);
        assert_ne!(colors[&other_project.cwd], original_color);
    }
}
