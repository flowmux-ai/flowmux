// SPDX-License-Identifier: GPL-3.0-or-later
//! Bottom bar for live AI agents across all workspaces.

use crate::bridge::{Bridge, GtkCommand};
use crate::ui::{agent_icon, agent_status_css_class, agent_status_indicator};
use flowmux_core::{
    AgentBarItem, AgentBarModel, AgentStatus, SurfaceId, AGENT_BAR_ITEM_MAX_WIDTH_PX,
};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::str::FromStr;

#[derive(Clone)]
pub(crate) struct AgentBar {
    pub(crate) root: gtk::Box,
    items: gtk::Box,
    summary: gtk::Label,
    item_buttons: Rc<RefCell<HashMap<SurfaceId, AgentBarItemWidget>>>,
    item_order: Rc<RefCell<Vec<SurfaceId>>>,
    bridge: Bridge,
}

impl AgentBar {
    pub(crate) fn new(bridge: Bridge) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        root.add_css_class("flowmux-agent-bar");
        root.set_visible(false);

        let label = gtk::Label::new(Some("Agents"));
        label.add_css_class("caption-heading");
        label.set_halign(gtk::Align::Start);
        let heading = gtk::Box::new(gtk::Orientation::Vertical, 2);
        heading.set_valign(gtk::Align::Center);
        heading.append(&label);
        let summary = crate::ui::agent_attention_summary_label();
        heading.append(&summary);
        root.append(&heading);

        let items = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        items.set_halign(gtk::Align::Start);

        let scroll = gtk::ScrolledWindow::new();
        scroll.set_hexpand(true);
        scroll.set_hscrollbar_policy(gtk::PolicyType::Automatic);
        scroll.set_vscrollbar_policy(gtk::PolicyType::Never);
        // Horizontal scrolling keeps item min widths intact; equal shrinking
        // would make multiple agent names/statuses unreadable before ellipsize.
        scroll.set_child(Some(&items));
        root.append(&scroll);

        Self {
            root,
            items,
            summary,
            item_buttons: Rc::new(RefCell::new(HashMap::new())),
            item_order: Rc::new(RefCell::new(Vec::new())),
            bridge,
        }
    }

    pub(crate) fn render(
        &self,
        model: &AgentBarModel,
        attention_surfaces: &HashSet<SurfaceId>,
        focused_surface: Option<SurfaceId>,
    ) {
        crate::ui::update_agent_attention_summary(&self.summary, model.attention_summary());
        let mut buttons = self.item_buttons.borrow_mut();
        buttons.retain(|surface, item| {
            let keep = model.visible && model.items.iter().any(|item| item.surface == *surface);
            if !keep {
                self.items.remove(&item.root);
            }
            keep
        });
        self.root.set_visible(model.visible);
        if !model.visible {
            return;
        }

        let mut previous: Option<gtk::Widget> = None;
        for item in self.ordered_items(model) {
            let widget = buttons.entry(item.surface).or_insert_with(|| {
                let widget = self.item_button(item);
                self.items.append(&widget.root);
                widget
            });
            widget.update(item);
            for (class, enabled) in [
                (
                    "flowmux-attention",
                    attention_surfaces.contains(&item.surface),
                ),
                ("focused", focused_surface == Some(item.surface)),
            ] {
                if enabled {
                    widget.button.add_css_class(class);
                } else {
                    widget.button.remove_css_class(class);
                }
            }
            if widget.root.prev_sibling() != previous {
                self.items
                    .reorder_child_after(&widget.root, previous.as_ref());
            }
            previous = Some(widget.root.clone().upcast());
        }
    }

    pub(crate) fn mark_attention(&self, surface: SurfaceId) {
        if let Some(button) = self.item_buttons.borrow().get(&surface) {
            button.button.add_css_class("flowmux-attention");
        }
    }

    pub(crate) fn clear_attention(&self, surface: SurfaceId) {
        if let Some(button) = self.item_buttons.borrow().get(&surface) {
            button.button.remove_css_class("flowmux-attention");
        }
    }

    fn item_button(&self, item: &AgentBarItem) -> AgentBarItemWidget {
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("flowmux-agent-bar-item");
        button.set_size_request(AGENT_BAR_ITEM_MAX_WIDTH_PX as i32, -1);
        button.set_hexpand(false);
        button.set_child(Some(&item_content(item)));
        let target = Rc::new(Cell::new((item.workspace, item.pane, item.surface)));
        let activated = Rc::new(Cell::new(false));
        let bridge = self.bridge.clone();
        let clicked_target = target.clone();
        let activated_for_button = activated.clone();
        button.connect_clicked(move |_| {
            let (workspace, pane, surface) = clicked_target.get();
            open_agent_bar_item(
                bridge.clone(),
                activated_for_button.clone(),
                workspace,
                pane,
                surface,
            );
        });

        let click = gtk::GestureClick::new();
        click.set_button(gtk::gdk::BUTTON_PRIMARY);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let bridge = self.bridge.clone();
        let released_target = target.clone();
        click.connect_released(move |gesture, _n_press, _x, _y| {
            let (workspace, pane, surface) = released_target.get();
            gesture.set_state(gtk::EventSequenceState::Claimed);
            open_agent_bar_item(bridge.clone(), activated.clone(), workspace, pane, surface);
        });
        button.add_controller(click);

        self.attach_dnd_handlers(&button, item.surface);

        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        button.set_hexpand(true);
        root.append(&button);
        let widget = AgentBarItemWidget {
            root,
            button,
            item: item.clone(),
            target,
        };
        widget.refresh_status(item);
        widget
    }

    fn ordered_items<'a>(&self, model: &'a AgentBarModel) -> Vec<&'a AgentBarItem> {
        let live_surfaces: Vec<SurfaceId> = model.items.iter().map(|item| item.surface).collect();
        {
            let mut order = self.item_order.borrow_mut();
            order.retain(|surface| live_surfaces.contains(surface));
            for surface in &live_surfaces {
                if !order.contains(surface) {
                    order.push(*surface);
                }
            }
        }

        let order = self.item_order.borrow();
        order
            .iter()
            .filter_map(|surface| model.items.iter().find(|item| item.surface == *surface))
            .collect()
    }

    fn attach_dnd_handlers(&self, button: &gtk::Button, surface: SurfaceId) {
        let drag_source = gtk::DragSource::new();
        drag_source.set_actions(gtk::gdk::DragAction::MOVE);
        drag_source.connect_prepare(move |_, _, _| {
            Some(gtk::gdk::ContentProvider::for_value(
                &surface.to_string().to_value(),
            ))
        });
        let button_for_begin = button.downgrade();
        drag_source.connect_drag_begin(move |_, _| {
            if let Some(button) = button_for_begin.upgrade() {
                button.set_opacity(0.4);
                button.add_css_class("flowmux-dragging");
            }
        });
        let button_for_end = button.downgrade();
        drag_source.connect_drag_end(move |_, _, _| {
            if let Some(button) = button_for_end.upgrade() {
                button.set_opacity(1.0);
                button.remove_css_class("flowmux-dragging");
            }
        });
        let button_for_cancel = button.downgrade();
        drag_source.connect_drag_cancel(move |_, _, _| {
            if let Some(button) = button_for_cancel.upgrade() {
                button.set_opacity(1.0);
                button.remove_css_class("flowmux-dragging");
            }
            false
        });
        button.add_controller(drag_source);

        let drop_target =
            gtk::DropTarget::new(gtk::glib::types::Type::STRING, gtk::gdk::DragAction::MOVE);
        let button_for_motion = button.downgrade();
        drop_target.connect_motion(move |_, x, _y| {
            let Some(button) = button_for_motion.upgrade() else {
                return gtk::gdk::DragAction::empty();
            };
            let width = button.width();
            let before = if width > 0 {
                x < (width as f64) / 2.0
            } else {
                true
            };
            if before {
                button.remove_css_class("flowmux-drop-after");
                button.add_css_class("flowmux-drop-before");
            } else {
                button.remove_css_class("flowmux-drop-before");
                button.add_css_class("flowmux-drop-after");
            }
            gtk::gdk::DragAction::MOVE
        });
        let button_for_leave = button.downgrade();
        drop_target.connect_leave(move |_| {
            if let Some(button) = button_for_leave.upgrade() {
                button.remove_css_class("flowmux-drop-before");
                button.remove_css_class("flowmux-drop-after");
            }
        });
        let order_for_drop = self.item_order.clone();
        let items_for_drop = self.items.downgrade();
        let item_buttons_for_drop = Rc::downgrade(&self.item_buttons);
        let button_for_drop = button.downgrade();
        drop_target.connect_drop(move |_, value, x, _y| {
            let Some(button) = button_for_drop.upgrade() else {
                return false;
            };
            let Some(items) = items_for_drop.upgrade() else {
                return false;
            };
            let Some(item_buttons) = item_buttons_for_drop.upgrade() else {
                return false;
            };
            button.remove_css_class("flowmux-drop-before");
            button.remove_css_class("flowmux-drop-after");
            let Ok(payload) = value.get::<String>() else {
                return false;
            };
            let Ok(source) = SurfaceId::from_str(&payload) else {
                return false;
            };
            if source == surface {
                return false;
            }
            let mut order = order_for_drop.borrow_mut();
            let target_width = button.width();
            let before = if target_width > 0 {
                x < (target_width as f64) / 2.0
            } else {
                true
            };
            let Some(next_order) = reordered_agent_bar_order(&order, source, surface, before)
            else {
                return false;
            };
            *order = next_order;
            reorder_item_widgets(&items, &item_buttons.borrow(), &order);
            true
        });
        button.add_controller(drop_target);
    }
}

struct AgentBarItemWidget {
    root: gtk::Box,
    button: gtk::Button,
    item: AgentBarItem,
    target: Rc<Cell<(flowmux_core::WorkspaceId, flowmux_core::PaneId, SurfaceId)>>,
}

impl AgentBarItemWidget {
    fn update(&mut self, item: &AgentBarItem) {
        if self.item != *item {
            self.button.set_child(Some(&item_content(item)));
            self.refresh_status(item);
            self.item = item.clone();
        }
        self.target.set((item.workspace, item.pane, item.surface));
    }

    fn refresh_status(&self, item: &AgentBarItem) {
        if item.status == AgentStatus::Working {
            self.button.add_css_class("flowmux-agent-bar-working");
        } else {
            self.button.remove_css_class("flowmux-agent-bar-working");
        }
        self.button
            .set_tooltip_text(Some(&flowmux_core::agent_status_tooltip(
                &item.agent_name,
                &item.status_text,
                item.status_evidence,
                &item.pending_waits,
            )));
    }
}

fn item_content(item: &AgentBarItem) -> gtk::Box {
    let visual_status = crate::ui::agent_attention_visual_status(item.status, item.attention());
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    row.set_valign(gtk::Align::Center);

    let stripe = color_stripe(&item.color);
    row.append(&stripe);

    row.append(&agent_status_indicator(visual_status, item.seen));

    let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
    text.set_hexpand(true);

    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    heading.append(&agent_icon(&item.agent_name));

    let name = gtk::Label::new(Some(item.agent_name.as_str()));
    name.set_halign(gtk::Align::Start);
    name.set_xalign(0.0);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_single_line_mode(true);
    name.set_lines(1);
    name.set_max_width_chars((AGENT_BAR_ITEM_MAX_WIDTH_PX / 10) as i32);
    name.add_css_class("caption-heading");
    heading.append(&name);
    text.append(&heading);

    let status = gtk::Label::new(Some(item.status_text.as_str()));
    status.set_halign(gtk::Align::Start);
    status.set_xalign(0.0);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_single_line_mode(true);
    status.set_lines(1);
    status.set_max_width_chars((AGENT_BAR_ITEM_MAX_WIDTH_PX / 8) as i32);
    status.add_css_class("caption");
    status.add_css_class(agent_status_css_class(visual_status, item.seen));
    text.append(&status);

    row.append(&text);
    row
}

fn reordered_agent_bar_order(
    order: &[SurfaceId],
    source: SurfaceId,
    target: SurfaceId,
    before: bool,
) -> Option<Vec<SurfaceId>> {
    if source == target {
        return None;
    }
    let source_idx = order.iter().position(|candidate| *candidate == source)?;
    let target_idx = order.iter().position(|candidate| *candidate == target)?;
    let new_index = match (before, source_idx < target_idx) {
        (true, true) => target_idx.saturating_sub(1),
        (true, false) => target_idx,
        (false, true) => target_idx,
        (false, false) => target_idx + 1,
    };
    if new_index == source_idx {
        return None;
    }
    let mut next = order.to_vec();
    let moved = next.remove(source_idx);
    next.insert(new_index.min(next.len()), moved);
    Some(next)
}

fn reorder_item_widgets(
    items: &gtk::Box,
    buttons: &HashMap<SurfaceId, AgentBarItemWidget>,
    order: &[SurfaceId],
) {
    let mut previous: Option<gtk::Widget> = None;
    for surface in order {
        if let Some(widget) = buttons.get(surface) {
            let button = &widget.root;
            items.reorder_child_after(button, previous.as_ref());
            previous = Some(button.clone().upcast());
        }
    }
}

fn open_agent_bar_item(
    bridge: Bridge,
    activated: Rc<Cell<bool>>,
    workspace: flowmux_core::WorkspaceId,
    pane: flowmux_core::PaneId,
    surface: SurfaceId,
) {
    if activated.replace(true) {
        return;
    }
    let reset = activated.clone();
    gtk::glib::idle_add_local_once(move || {
        reset.set(false);
    });
    gtk::glib::MainContext::default().spawn_local(async move {
        let _ = bridge
            .tx
            .send(GtkCommand::OpenAgentBarItem {
                workspace,
                pane,
                surface,
            })
            .await;
    });
}

fn color_stripe(color: &str) -> gtk::DrawingArea {
    let rgba =
        gtk::gdk::RGBA::parse(color).unwrap_or_else(|_| gtk::gdk::RGBA::new(0.45, 0.55, 0.65, 1.0));
    let stripe = gtk::DrawingArea::new();
    stripe.set_content_width(4);
    stripe.set_content_height(29);
    stripe.add_css_class("flowmux-agent-bar-color");
    stripe.set_draw_func(move |_, cr, width, height| {
        cr.set_source_rgba(
            rgba.red() as f64,
            rgba.green() as f64,
            rgba.blue() as f64,
            rgba.alpha() as f64,
        );
        cr.rectangle(0.0, 0.0, width as f64, height as f64);
        let _ = cr.fill();
    });
    stripe
}

#[cfg(test)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) async fn check_stable_agent_bar_items() {
    let (bridge, rx) = Bridge::new();
    let bar = AgentBar::new(bridge);
    let mut model = AgentBarModel {
        visible: true,
        items: (0..24)
            .map(|_| AgentBarItem {
                workspace: flowmux_core::WorkspaceId::new(),
                pane: flowmux_core::PaneId::new(),
                surface: SurfaceId::new(),
                surface_label: "Test task".into(),
                agent_name: "test agent".into(),
                status: AgentStatus::Working,
                visual_status: flowmux_core::AgentBarVisualStatus::Working,
                seen: true,
                status_text: "Running tests".into(),
                status_evidence: None,
                pending_waits: Vec::new(),
                turn_outcome: None,
                color: "#abcdef".into(),
            })
            .collect(),
    };
    let surface = model.items[0].surface;
    bar.render(&model, &HashSet::from([surface]), Some(surface));
    let window = gtk::Window::builder()
        .default_width(600)
        .default_height(120)
        .child(&bar.root)
        .build();
    window.present();
    gtk::glib::timeout_future(std::time::Duration::from_millis(50)).await;
    let button = bar.item_buttons.borrow()[&surface].button.clone();
    assert_eq!(
        button.parent().unwrap().first_child(),
        Some(button.clone().upcast())
    );
    assert!(
        button.next_sibling().is_none(),
        "an agent row has one navigation target"
    );
    assert!(button.grab_focus());
    assert!(button.is_focus());
    let content = button.child().unwrap();
    let controller_count = button.observe_controllers().n_items();
    let scroll = bar
        .root
        .last_child()
        .unwrap()
        .downcast::<gtk::ScrolledWindow>()
        .unwrap();
    let adjustment = scroll.hadjustment();
    assert!(adjustment.upper() - adjustment.page_size() > 120.0);
    adjustment.set_value(120.0);
    gtk::glib::timeout_future(std::time::Duration::from_millis(30)).await;
    let scroll_before = adjustment.value();
    bar.render(&model, &HashSet::from([surface]), Some(surface));
    assert_eq!(bar.items.first_child(), button.parent());
    assert_eq!(
        button.child(),
        Some(content.clone()),
        "unchanged content must survive a refresh"
    );
    let old_content = content.downgrade();
    drop(content);

    model.items[0].status = AgentStatus::Blocked;
    model.items[0].visual_status = flowmux_core::AgentBarVisualStatus::Waiting;
    model.items[0].status_text = "Waiting for answer".into();
    model.items[0].workspace = flowmux_core::WorkspaceId::new();
    model.items[0].pane = flowmux_core::PaneId::new();
    let target = (model.items[0].workspace, model.items[0].pane, surface);
    bar.render(&model, &HashSet::new(), None);
    gtk::glib::timeout_future(std::time::Duration::from_millis(50)).await;
    assert_eq!(bar.items.first_child(), button.parent());
    assert!(
        button.is_focus(),
        "status refresh must preserve keyboard focus"
    );
    assert!(
        (adjustment.value() - scroll_before).abs() < 1.0,
        "status refresh moved horizontal scroll"
    );
    assert!(old_content.upgrade().is_none());
    assert!(button
        .tooltip_text()
        .unwrap()
        .contains("Waiting for answer"));
    assert!(!button.has_css_class("flowmux-agent-bar-working"));
    assert!(!button.has_css_class("flowmux-attention"));
    assert!(!button.has_css_class("focused"));
    assert_eq!(button.observe_controllers().n_items(), controller_count);
    let drop_button = bar.item_buttons.borrow()[&model.items[1].surface]
        .button
        .clone();
    let controllers = drop_button.observe_controllers();
    let drop_target = (0..controllers.n_items())
        .find_map(|i| {
            controllers
                .item(i)
                .unwrap()
                .downcast::<gtk::DropTarget>()
                .ok()
        })
        .unwrap();
    assert!(drop_target.emit_by_name::<bool>(
        "drop",
        &[
            &gtk::glib::BoxedValue(surface.to_string().to_value()),
            &(drop_button.width() as f64 - 1.0),
            &0.0f64,
        ]
    ));
    drop(drop_target);
    drop(controllers);
    drop(drop_button);
    bar.render(&model, &HashSet::new(), None);
    assert_eq!(
        bar.items.first_child().unwrap().next_sibling(),
        button.parent()
    );
    assert!(button.is_focus(), "reordering must preserve keyboard focus");
    button.emit_clicked();
    button.emit_clicked();
    let command = gtk::glib::future_with_timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(command, GtkCommand::OpenAgentBarItem { workspace, pane, surface }
        if (workspace, pane, surface) == target),
        "reused button must route to current location"
    );
    assert!(
        rx.try_recv().is_err(),
        "one activation must not navigate twice"
    );
    let weak = button.downgrade();
    drop(button);
    model.items.remove(0);
    bar.render(&model, &HashSet::new(), None);
    gtk::glib::timeout_future(std::time::Duration::from_millis(50)).await;
    assert!(
        weak.upgrade().is_none(),
        "removed surfaces must release widgets and handlers"
    );
    assert!(!bar.item_order.borrow().contains(&surface));
    bar.render(
        &AgentBarModel {
            visible: false,
            items: Vec::new(),
        },
        &HashSet::new(),
        None,
    );
    assert!(bar.item_buttons.borrow().is_empty());
    assert!(bar.items.first_child().is_none());
    assert!(!bar.root.is_visible());
    window.destroy();
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::{AgentBarVisualStatus, PaneId, WorkspaceId};

    fn working_item() -> AgentBarItem {
        AgentBarItem {
            status_evidence: None,
            pending_waits: Vec::new(),
            turn_outcome: None,
            workspace: WorkspaceId::new(),
            pane: PaneId::new(),
            surface: SurfaceId::new(),
            surface_label: "terminal".into(),
            agent_name: "codex".into(),
            status: AgentStatus::Working,
            visual_status: AgentBarVisualStatus::Working,
            seen: true,
            status_text: "working".into(),
            color: "#f59e0b".into(),
        }
    }

    #[test]
    fn reordered_agent_bar_order_moves_items_before_or_after_target() {
        let a = SurfaceId::new();
        let b = SurfaceId::new();
        let c = SurfaceId::new();
        let order = vec![a, b, c];

        assert_eq!(
            reordered_agent_bar_order(&order, a, c, true),
            Some(vec![b, a, c])
        );
        assert_eq!(
            reordered_agent_bar_order(&order, a, c, false),
            Some(vec![b, c, a])
        );
        assert_eq!(
            reordered_agent_bar_order(&order, c, a, true),
            Some(vec![c, a, b])
        );
        assert_eq!(
            reordered_agent_bar_order(&order, c, a, false),
            Some(vec![a, c, b])
        );
        assert_eq!(reordered_agent_bar_order(&order, b, b, true), None);
    }

    #[test]
    fn acknowledged_blocked_agent_keeps_warning_color() {
        assert_eq!(
            agent_status_css_class(AgentStatus::Blocked, false),
            "flowmux-sidebar-agent-blocked"
        );
        assert_eq!(
            agent_status_css_class(AgentStatus::Blocked, true),
            "flowmux-sidebar-agent-blocked"
        );
        assert_eq!(
            agent_status_css_class(AgentStatus::Done, true),
            "flowmux-sidebar-agent-idle"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    async fn refreshing_agent_bar_preserves_live_buttons() {
        check_stable_agent_bar_items().await;
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn working_item_uses_spinner_and_highlight_class() {
        let (bridge, _rx) = Bridge::new();
        let bar = AgentBar::new(bridge);
        let item = working_item();

        let button = bar.item_button(&item).button;
        assert!(button.has_css_class("flowmux-agent-bar-working"));
        let row = button.child().unwrap().downcast::<gtk::Box>().unwrap();
        let spinner = row
            .first_child()
            .unwrap()
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Spinner>()
            .unwrap();
        assert!(spinner.is_spinning());

        let text = spinner
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Box>()
            .unwrap();
        let heading = text.first_child().unwrap().downcast::<gtk::Box>().unwrap();
        let icon = heading
            .first_child()
            .unwrap()
            .downcast::<gtk::Image>()
            .unwrap();
        assert_eq!(
            icon.icon_name().as_deref(),
            Some("flowmux-agent-codex-symbolic")
        );
        assert_eq!(icon.pixel_size(), 14);
        assert_eq!(
            icon.next_sibling()
                .unwrap()
                .downcast::<gtk::Label>()
                .unwrap()
                .label(),
            "codex"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn rerender_releases_buttons_with_dnd_controllers() {
        let (bridge, _rx) = Bridge::new();
        let bar = AgentBar::new(bridge);
        let item = working_item();
        bar.render(
            &AgentBarModel {
                visible: true,
                items: vec![item.clone()],
            },
            &HashSet::new(),
            None,
        );
        let weak = bar.item_buttons.borrow()[&item.surface].button.downgrade();

        bar.render(
            &AgentBarModel {
                visible: false,
                items: Vec::new(),
            },
            &HashSet::new(),
            None,
        );

        assert!(weak.upgrade().is_none());
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn dropping_bar_releases_current_dnd_item_graph() {
        let (bridge, _rx) = Bridge::new();
        let bar = AgentBar::new(bridge);
        let item = working_item();
        bar.render(
            &AgentBarModel {
                visible: true,
                items: vec![item.clone()],
            },
            &HashSet::new(),
            None,
        );
        let button = bar.item_buttons.borrow()[&item.surface].button.downgrade();
        let items = bar.items.downgrade();

        drop(bar);

        assert!(button.upgrade().is_none());
        assert!(items.upgrade().is_none());
    }
}
