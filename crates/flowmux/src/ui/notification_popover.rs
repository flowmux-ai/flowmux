// SPDX-License-Identifier: GPL-3.0-or-later
//! Persistent notification controls and rows; opening the list never acknowledges it.

use crate::bridge::{Bridge, GtkCommand};
use crate::notifications::{NotificationEntry, NotificationStore};
use flowmux_core::{NotificationId, NotificationLevel, WorkspaceId};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Filter {
    All,
    Unread,
    Input,
    Errors,
    Completed,
}

impl Filter {
    fn matches(self, entry: &NotificationEntry) -> bool {
        match self {
            Self::All => true,
            Self::Unread => !entry.read,
            Self::Input => entry.level == NotificationLevel::NeedsInput,
            Self::Errors => entry.level == NotificationLevel::Error,
            Self::Completed => entry.level == NotificationLevel::TurnCompleted,
        }
    }
}

pub(super) struct NotificationPopover {
    pub popover: gtk::Popover,
    store: NotificationStore,
    bridge: Bridge,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    empty: gtk::Label,
    heading: gtk::Label,
    mark_read: gtk::Button,
    filters: Vec<(Filter, &'static str, gtk::ToggleButton)>,
    filter: Cell<Filter>,
    workspace: gtk::DropDown,
    workspaces: RefCell<Vec<(Option<WorkspaceId>, String)>>,
    updating: Cell<bool>,
    rows: RefCell<HashMap<NotificationId, NotificationRow>>,
}

struct NotificationRow {
    row: gtk::ListBoxRow,
    title: gtk::Label,
    unread: gtk::Label,
}

impl NotificationPopover {
    pub fn new(store: NotificationStore, bridge: Bridge) -> Rc<Self> {
        let popover = gtk::Popover::new();
        popover.add_css_class("flowmux-notification-popover");
        popover.set_position(gtk::PositionType::Bottom);
        popover.set_halign(gtk::Align::Start);
        let root = gtk::Box::new(gtk::Orientation::Vertical, 12);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);
        root.set_size_request(580, -1);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let heading = gtk::Label::new(Some("Notifications"));
        heading.add_css_class("title-3");
        heading.set_hexpand(true);
        heading.set_xalign(0.0);
        header.append(&heading);
        let mark_read = gtk::Button::with_label("Mark all read");
        mark_read.add_css_class("flat");
        mark_read.set_tooltip_text(Some("Mark notifications in every workspace as read"));
        header.append(&mark_read);
        let more = gtk::MenuButton::new();
        more.set_icon_name("view-more-symbolic");
        more.set_tooltip_text(Some("Notification actions"));
        let menu = gtk::Popover::new();
        let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for (label, read_only) in [
            ("Delete read notifications", true),
            ("Delete all notifications", false),
        ] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("flat");
            let bridge = bridge.clone();
            let menu = menu.downgrade();
            button.connect_clicked(move |_| {
                if let Some(menu) = menu.upgrade() {
                    menu.popdown();
                }
                send(
                    &bridge,
                    if read_only {
                        GtkCommand::DeleteReadNotifications
                    } else {
                        GtkCommand::ClearAllNotifications
                    },
                );
            });
            actions.append(&button);
        }
        menu.set_child(Some(&actions));
        more.set_popover(Some(&menu));
        header.append(&more);
        root.append(&header);

        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabs.add_css_class("linked");
        let mut filters = Vec::new();
        for (filter, label) in [
            (Filter::All, "All"),
            (Filter::Unread, "Unread"),
            (Filter::Input, "Input"),
            (Filter::Errors, "Errors"),
            (Filter::Completed, "Done"),
        ] {
            let button = gtk::ToggleButton::with_label(label);
            button.set_hexpand(true);
            if let Some((_, _, first)) = filters.first() {
                button.set_group(Some(first));
            }
            tabs.append(&button);
            filters.push((filter, label, button));
        }
        filters[0].2.set_active(true);
        root.append(&tabs);
        let workspace = gtk::DropDown::from_strings(&["All workspaces"]);
        workspace.update_property(&[gtk::accessible::Property::Label("Filter by workspace")]);
        workspace.set_tooltip_text(Some("Filter by workspace"));
        root.append(&workspace);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroll.set_min_content_height(440);
        scroll.set_max_content_height(560);
        scroll.set_propagate_natural_height(true);
        let list = gtk::ListBox::new();
        list.set_selection_mode(gtk::SelectionMode::None);
        list.add_css_class("boxed-list");
        let empty = gtk::Label::new(Some("No notifications yet."));
        empty.set_margin_top(32);
        empty.set_margin_bottom(32);
        empty.add_css_class("dim-label");
        list.set_placeholder(Some(&empty));
        scroll.set_child(Some(&list));
        root.append(&scroll);
        popover.set_child(Some(&root));
        let this = Rc::new(Self {
            popover,
            store,
            bridge,
            list,
            scroll,
            empty,
            heading,
            mark_read,
            filters,
            filter: Cell::new(Filter::All),
            workspace,
            workspaces: RefCell::new(vec![(None, "All workspaces".into())]),
            updating: Cell::new(false),
            rows: RefCell::new(HashMap::new()),
        });
        let weak = Rc::downgrade(&this);
        this.popover.connect_show(move |_| {
            if let Some(this) = weak.upgrade() {
                this.refresh();
            }
        });
        let weak = Rc::downgrade(&this);
        this.mark_read.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                send(&this.bridge, GtkCommand::MarkAllNotificationsRead);
            }
        });
        for (filter, _, button) in &this.filters {
            let filter = *filter;
            let weak = Rc::downgrade(&this);
            button.connect_toggled(move |button| {
                if button.is_active() {
                    if let Some(this) = weak.upgrade() {
                        this.filter.set(filter);
                        this.refresh();
                        this.scroll.vadjustment().set_value(0.0);
                    }
                }
            });
        }
        let weak = Rc::downgrade(&this);
        this.workspace.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                if !this.updating.get() {
                    this.refresh();
                    this.scroll.vadjustment().set_value(0.0);
                }
            }
        });
        this
    }

    pub fn refresh(&self) {
        if self.updating.replace(true) {
            return;
        }
        let entries = self.store.entries();
        let unread = entries.iter().filter(|entry| !entry.read).count();
        self.heading
            .set_label(&format!("Notifications · {unread} unread"));
        self.mark_read.set_sensitive(unread > 0);
        let selected = self
            .workspaces
            .borrow()
            .get(self.workspace.selected() as usize)
            .and_then(|(id, _)| *id);
        let mut workspaces = vec![(None, "All workspaces".to_owned())];
        for entry in &entries {
            if let Some(id) = entry.workspace {
                if !workspaces.iter().any(|(existing, _)| *existing == Some(id)) {
                    workspaces.push((
                        Some(id),
                        entry
                            .workspace_label
                            .clone()
                            .unwrap_or_else(|| "Closed workspace".into()),
                    ));
                }
            }
        }
        if *self.workspaces.borrow() != workspaces {
            let index = workspaces
                .iter()
                .position(|(id, _)| *id == selected)
                .unwrap_or(0);
            let labels: Vec<_> = workspaces.iter().map(|(_, name)| name.as_str()).collect();
            self.workspace
                .set_model(Some(&gtk::StringList::new(&labels)));
            *self.workspaces.borrow_mut() = workspaces;
            self.workspace.set_selected(index as u32);
        }
        let selected = self.workspaces.borrow()[self.workspace.selected() as usize].0;
        for (filter, label, button) in &self.filters {
            let count = entries
                .iter()
                .filter(|entry| {
                    selected.is_none_or(|id| entry.workspace == Some(id)) && filter.matches(entry)
                })
                .count();
            button.set_label(&format!("{label} {count}"));
        }
        self.empty.set_label(if entries.is_empty() {
            "No notifications yet."
        } else {
            "No notifications match these filters."
        });
        let adjustment = self.scroll.vadjustment();
        let old_scroll = adjustment.value();
        let mut rows = self.rows.borrow_mut();
        let anchor = rows
            .values()
            .filter(|row| row.row.is_visible())
            .filter_map(|row| {
                let bounds = row.row.compute_bounds(&self.list)?;
                (old_scroll > 0.0 && bounds.y() as f64 >= old_scroll)
                    .then_some((row.row.clone(), bounds.y()))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let mut changed = false;
        rows.retain(|id, row| {
            let keep = entries.iter().any(|entry| entry.id == *id);
            if !keep {
                self.list.remove(&row.row);
                changed = true;
            }
            keep
        });
        for (index, entry) in entries.iter().rev().enumerate() {
            let row = rows.entry(entry.id).or_insert_with(|| {
                changed = true;
                let row = notification_row(entry, &self.bridge, &self.popover);
                self.list.insert(&row.row, index as i32);
                row
            });
            row.unread.set_visible(!entry.read);
            if entry.read {
                row.title.remove_css_class("heading");
            } else {
                row.title.add_css_class("heading");
            }
            row.row.set_visible(
                selected.is_none_or(|id| entry.workspace == Some(id))
                    && self.filter.get().matches(entry),
            );
        }
        if changed {
            if let Some((anchor, old_y)) = anchor {
                let list = self.list.downgrade();
                self.list.add_tick_callback(move |_, _| {
                    if let Some(list) = list.upgrade() {
                        if let Some(bounds) = anchor.compute_bounds(&list) {
                            adjustment.set_value(old_scroll + f64::from(bounds.y() - old_y));
                        }
                    }
                    gtk::glib::ControlFlow::Break
                });
            }
        }
        self.updating.set(false);
    }
}

fn send(bridge: &Bridge, command: GtkCommand) {
    let bridge = bridge.clone();
    gtk::glib::MainContext::default().spawn_local(async move {
        if let Err(error) = bridge.tx.send(command).await {
            tracing::warn!(%error, "notification action failed");
        }
    });
}

fn notification_row(
    entry: &NotificationEntry,
    bridge: &Bridge,
    popover: &gtk::Popover,
) -> NotificationRow {
    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_selectable(false);
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    let open = gtk::Button::new();
    open.add_css_class("flat");
    open.set_hexpand(true);
    open.set_tooltip_text(Some("Open source tab"));
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    body.set_margin_top(5);
    body.set_margin_bottom(5);
    let agent = entry.agent.as_deref().unwrap_or("");
    let icon = super::agent_icon(agent);
    icon.set_pixel_size(28);
    icon.set_valign(gtk::Align::Start);
    icon.update_property(&[gtk::accessible::Property::Label(if agent.is_empty() {
        "Notification"
    } else {
        agent
    })]);
    icon.set_tooltip_text(Some(if agent.is_empty() {
        "Notification"
    } else {
        agent
    }));
    body.append(&icon);
    let text = gtk::Box::new(gtk::Orientation::Vertical, 3);
    text.set_hexpand(true);
    let metadata = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let (kind, symbol, css) = match entry.level {
        NotificationLevel::Info => ("Information", "dialog-information-symbolic", "dim-label"),
        NotificationLevel::TurnCompleted => ("Completed", "emblem-ok-symbolic", "success"),
        NotificationLevel::NeedsInput => ("Input needed", "dialog-question-symbolic", "warning"),
        NotificationLevel::Error => ("Error", "dialog-error-symbolic", "error"),
    };
    let kind_icon = gtk::Image::from_icon_name(symbol);
    kind_icon.add_css_class(css);
    metadata.append(&kind_icon);
    let label = gtk::Label::new(Some(kind));
    label.add_css_class(css);
    label.add_css_class("caption");
    metadata.append(&label);
    let unread = gtk::Label::new(Some("●"));
    unread.add_css_class("accent");
    unread.set_tooltip_text(Some("Unread"));
    metadata.append(&unread);
    let when = gtk::Label::new(Some(
        &entry
            .created_at
            .with_timezone(&chrono::Local)
            .format("%b %d · %H:%M")
            .to_string(),
    ));
    when.add_css_class("caption");
    when.add_css_class("dim-label");
    when.set_hexpand(true);
    when.set_xalign(1.0);
    when.set_tooltip_text(Some(
        &entry
            .created_at
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S %Z")
            .to_string(),
    ));
    metadata.append(&when);
    text.append(&metadata);
    let title = wrapped_label(&entry.title, 2);
    text.append(&title);
    let source = [
        entry.workspace_label.as_deref(),
        entry.surface_label.as_deref(),
        entry.agent.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" / ");
    if !source.is_empty() {
        let label = wrapped_label(&source, 1);
        label.add_css_class("caption");
        label.add_css_class("dim-label");
        label.set_tooltip_text(Some(&source));
        text.append(&label);
    }
    if !entry.body.is_empty() {
        let label = wrapped_label(&entry.body, 3);
        label.set_tooltip_text(Some(&entry.body));
        text.append(&label);
    }
    body.append(&text);
    open.set_child(Some(&body));
    open.update_property(&[
        gtk::accessible::Property::Label(&format!("Open notification: {}", entry.title)),
        gtk::accessible::Property::Description("Open source tab"),
    ]);
    let id = entry.id;
    let weak_popover = popover.downgrade();
    let open_bridge = bridge.clone();
    open.connect_clicked(move |_| {
        send(&open_bridge, GtkCommand::OpenNotification { id });
        if let Some(popover) = weak_popover.upgrade() {
            popover.popdown();
        }
    });
    content.append(&open);
    let delete = gtk::Button::from_icon_name("user-trash-symbolic");
    delete.add_css_class("flat");
    delete.set_valign(gtk::Align::Center);
    delete.set_tooltip_text(Some("Delete notification"));
    let bridge = bridge.clone();
    delete.connect_clicked(move |_| send(&bridge, GtkCommand::DeleteNotification { id }));
    content.append(&delete);
    row.set_child(Some(&content));
    NotificationRow { row, title, unread }
}

fn wrapped_label(value: &str, lines: i32) -> gtk::Label {
    let label = gtk::Label::new(Some(value));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_lines(lines);
    label.set_max_width_chars(38);
    label
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push(
        store: &NotificationStore,
        level: NotificationLevel,
        workspace: WorkspaceId,
        agent: &str,
    ) -> NotificationId {
        let id = store
            .push(
                format!("{agent} update"),
                "Review the requested change".into(),
                level,
                None,
                None,
                Some(workspace),
            )
            .unwrap()
            .0;
        store.set_source_labels(
            id,
            Some(agent.into()),
            Some(format!("Project {agent}")),
            Some("Task tab".into()),
        );
        id
    }

    #[cfg(not(target_os = "macos"))]
    fn mount(panel: &NotificationPopover) -> gtk::Window {
        let window = gtk::Window::new();
        let button = gtk::MenuButton::new();
        button.set_popover(Some(&panel.popover));
        window.set_child(Some(&button));
        window.present();
        window
    }

    #[test]
    fn filters_distinguish_kind_from_read_state() {
        let store = NotificationStore::new();
        for level in [
            NotificationLevel::Info,
            NotificationLevel::TurnCompleted,
            NotificationLevel::NeedsInput,
            NotificationLevel::Error,
        ] {
            let id = push(&store, level, WorkspaceId::new(), "Codex");
            for read in [false, true] {
                if read {
                    store.mark_read(id);
                }
                let entry = store.find(id).unwrap();
                assert!(Filter::All.matches(&entry));
                assert_eq!(Filter::Unread.matches(&entry), !read);
                assert_eq!(
                    Filter::Input.matches(&entry),
                    level == NotificationLevel::NeedsInput
                );
                assert_eq!(
                    Filter::Errors.matches(&entry),
                    level == NotificationLevel::Error
                );
                assert_eq!(
                    Filter::Completed.matches(&entry),
                    level == NotificationLevel::TurnCompleted
                );
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    async fn filters_opening_and_refresh_preserve_unread_and_row_identity() {
        let store = NotificationStore::new();
        let first = WorkspaceId::new();
        let second = WorkspaceId::new();
        let input = push(&store, NotificationLevel::NeedsInput, first, "Claude");
        let error = push(&store, NotificationLevel::Error, second, "Codex");
        let (bridge, _) = Bridge::new();
        let panel = NotificationPopover::new(store.clone(), bridge);
        let window = mount(&panel);
        gtk::glib::timeout_future(std::time::Duration::from_millis(30)).await;
        panel.popover.popdown();
        panel.popover.popup();
        assert_eq!(
            store.unread_count(),
            2,
            "opening must not acknowledge unseen rows"
        );
        let original = panel.rows.borrow()[&input].row.clone();
        panel.filters[2].2.set_active(true);
        assert!(original.is_visible());
        assert!(!panel.rows.borrow()[&error].row.is_visible());
        panel.workspace.set_selected(2);
        assert!(!original.is_visible());
        assert_eq!(panel.filters[0].2.label().as_deref(), Some("All 1"));
        panel.workspace.set_selected(0);
        panel.filters[1].2.set_active(true);
        store.mark_read(input);
        panel.refresh();
        assert!(!original.is_visible());
        assert!(panel.rows.borrow()[&error].row.is_visible());
        assert_eq!(panel.rows.borrow()[&input].row, original);
        panel.popover.popdown();
        panel.popover.popup();
        assert_eq!(panel.filter.get(), Filter::Unread);
        assert_eq!(store.unread_count(), 1);
        let _ = store.remove(error);
        panel.refresh();
        assert_eq!(panel.rows.borrow().len(), 1);
        assert_eq!(panel.filters[1].2.label().as_deref(), Some("Unread 0"));
        assert_eq!(panel.empty.label(), "No notifications match these filters.");
        store.clear_all();
        panel.refresh();
        assert_eq!(panel.empty.label(), "No notifications yet.");
        assert!(!panel.mark_read.is_sensitive());
        window.close();
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    async fn rows_use_agent_icons_and_independent_native_buttons() {
        let store = NotificationStore::new();
        let id = push(
            &store,
            NotificationLevel::NeedsInput,
            WorkspaceId::new(),
            "Claude",
        );
        let (bridge, rx) = Bridge::new();
        let panel = NotificationPopover::new(store, bridge);
        let window = mount(&panel);
        gtk::glib::timeout_future(std::time::Duration::from_millis(30)).await;
        panel.refresh();
        let row = panel.rows.borrow()[&id].row.clone();
        let container = row.child().unwrap();
        let open = container
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        let icon = open
            .child()
            .unwrap()
            .first_child()
            .unwrap()
            .downcast::<gtk::Image>()
            .unwrap();
        assert_eq!(icon.icon_name().as_deref(), Some("flowmux-agent-claude"));
        assert_eq!(icon.pixel_size(), 28);
        open.emit_clicked();
        assert!(
            matches!(rx.recv().await.unwrap(), GtkCommand::OpenNotification { id: actual } if actual == id)
        );
        let delete = container
            .last_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        delete.emit_clicked();
        assert!(
            matches!(rx.recv().await.unwrap(), GtkCommand::DeleteNotification { id: actual } if actual == id)
        );
        assert!(rx.try_recv().is_err());
        panel.mark_read.emit_clicked();
        assert!(matches!(
            rx.recv().await.unwrap(),
            GtkCommand::MarkAllNotificationsRead
        ));
        window.close();
    }
}
