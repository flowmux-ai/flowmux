// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared pixel rooms backed by the existing live-agent model.

mod scene;

use crate::bridge::{Bridge, GtkCommand};
use flowmux_core::{AgentBarItem, AgentBarModel, AgentStatus, SurfaceId, WorkspaceId};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

struct Resident {
    button: gtk::Button,
    icon: gtk::Image,
    name: gtk::Label,
    actor: Rc<RefCell<scene::Actor>>,
    drawing: gtk::DrawingArea,
}

pub(crate) struct AgentOffice {
    pub(crate) root: gtk::Box,
    pub(crate) close: gtk::Button,
    summary: gtk::Label,
    rooms: gtk::Box,
    residents: RefCell<HashMap<SurfaceId, Resident>>,
    layout: RefCell<Vec<(WorkspaceId, String, Vec<SurfaceId>)>>,
    bridge: Bridge,
}

impl AgentOffice {
    pub(crate) fn new(bridge: Bridge) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("flowmux-agent-office");
        root.add_css_class("flowmux-agent-office");
        root.set_hexpand(true);
        root.set_vexpand(true);
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new(
            "AgentOffice",
            "Your team's little pixel world",
        )));
        let close = gtk::Button::from_icon_name("go-previous-symbolic");
        close.set_tooltip_text(Some("Back to workspace (Esc)"));
        close.update_property(&[gtk::accessible::Property::Label("Back to workspace")]);
        header.pack_start(&close);
        let close_bridge = bridge.clone();
        close.connect_clicked(move |_| send_toggle(&close_bridge));
        let key = gtk::EventControllerKey::new();
        key.set_propagation_phase(gtk::PropagationPhase::Capture);
        let key_bridge = bridge.clone();
        key.connect_key_pressed(move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape {
                send_toggle(&key_bridge);
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
        root.add_controller(key);
        root.append(&header);
        let summary = gtk::Label::new(None);
        summary.add_css_class("flowmux-office-summary");
        summary.set_wrap(true);
        root.append(&summary);
        let rooms = gtk::Box::new(gtk::Orientation::Vertical, 18);
        rooms.add_css_class("flowmux-office-rooms");
        let scroll = gtk::ScrolledWindow::builder()
            .vexpand(true)
            .child(&rooms)
            .build();
        root.append(&scroll);
        let hint = gtk::Label::new(Some(
            "Select a teammate to visit their terminal  ·  Tab to select  ·  Esc to return",
        ));
        hint.add_css_class("flowmux-office-summary");
        hint.set_wrap(true);
        root.append(&hint);
        Self {
            root,
            close,
            summary,
            rooms,
            residents: RefCell::new(HashMap::new()),
            layout: RefCell::new(Vec::new()),
            bridge,
        }
    }

    pub(crate) fn render(&self, workspaces: &[(WorkspaceId, String)], model: &AgentBarModel) {
        let layout: Vec<_> = workspaces
            .iter()
            .map(|(id, title)| {
                let mut surfaces: Vec<_> = model
                    .items
                    .iter()
                    .filter(|item| item.workspace == *id)
                    .map(|item| item.surface)
                    .collect();
                // Activity priority changes must not shuffle desks or keyboard focus.
                surfaces.sort_by_key(|surface| surface.0);
                (*id, title.clone(), surfaces)
            })
            .collect();
        if *self.layout.borrow() != layout {
            while let Some(child) = self.rooms.first_child() {
                self.rooms.remove(&child);
            }
            self.residents.borrow_mut().clear();
            for (index, (id, title, surfaces)) in layout.iter().enumerate() {
                let room = gtk::Box::new(gtk::Orientation::Vertical, 6);
                room.set_halign(gtk::Align::Center);
                room.set_widget_name(&format!("flowmux-office-room-{id}"));
                let heading = gtk::Box::new(gtk::Orientation::Horizontal, 12);
                let number = gtk::Label::new(Some(&format!("{:02} /", index + 1)));
                number.add_css_class("dim-label");
                heading.append(&number);
                let name = gtk::Label::new(Some(title));
                name.add_css_class("heading");
                name.set_hexpand(true);
                name.set_xalign(0.0);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                heading.append(&name);
                heading.append(&gtk::Label::new(Some(&format!(
                    "{} teammates",
                    surfaces.len()
                ))));
                room.append(&heading);
                let stage = gtk::Fixed::new();
                let height = scene::height(surfaces.len());
                stage.set_size_request(scene::WIDTH * 2, height * 2);
                let area = gtk::DrawingArea::builder()
                    .content_width(scene::WIDTH * 2)
                    .content_height(height * 2)
                    .build();
                area.set_can_target(false);
                stage.put(&area, 0.0, 0.0);
                let mut actors = Vec::new();
                let mut targets = Vec::new();
                for (slot, surface) in surfaces.iter().enumerate() {
                    if let Some(item) = model.items.iter().find(|item| item.surface == *surface) {
                        let actor = Rc::new(RefCell::new(scene::Actor::new(
                            slot,
                            variant(*surface),
                            item.status,
                        )));
                        let resident = resident(item, actor.clone(), &area, &self.bridge);
                        stage.put(&resident.button, 0.0, 0.0);
                        position_button(&stage, &resident.button, &actor.borrow());
                        targets.push(resident.button.clone());
                        actors.push(actor);
                        self.residents.borrow_mut().insert(*surface, resident);
                    }
                }
                let frame = Rc::new(Cell::new(0));
                let draw_frame = frame.clone();
                let draw_actors = actors.clone();
                area.set_draw_func(move |_, cr, _, _| {
                    cr.scale(2.0, 2.0);
                    scene::draw_room(cr, height, &draw_actors, draw_frame.get());
                });
                let last = Cell::new(0);
                let weak_stage = stage.downgrade();
                area.add_tick_callback(move |area, clock| {
                    let now = clock.frame_time();
                    let previous = last.replace(now);
                    let animate = adw::is_animations_enabled(area);
                    let next = if animate { (now / 160_000) as u32 } else { 0 };
                    let dt = if previous == 0 {
                        0.0
                    } else {
                        ((now - previous) as f64 / 1_000_000.0).min(0.1)
                    };
                    let mut changed = frame.replace(next) != next;
                    if let Some(stage) = weak_stage.upgrade() {
                        for (actor, button) in actors.iter().zip(&targets) {
                            let mut actor = actor.borrow_mut();
                            // Keep a keyboard-focused target still until the user leaves it.
                            if !button.has_focus() {
                                changed |= actor.advance(dt, animate);
                                position_button(&stage, button, &actor);
                            }
                        }
                    }
                    if changed {
                        area.queue_draw();
                    }
                    gtk::glib::ControlFlow::Continue
                });
                room.append(&stage);
                if surfaces.is_empty() {
                    let empty = gtk::Label::new(Some("The office is quiet. Start an agent in this workspace to welcome a teammate."));
                    empty.set_wrap(true);
                    empty.add_css_class("dim-label");
                    room.append(&empty);
                }
                self.rooms.append(&room);
            }
            *self.layout.borrow_mut() = layout;
        }
        let working = model
            .items
            .iter()
            .filter(|item| item.status == AgentStatus::Working)
            .count();
        let waiting = model
            .items
            .iter()
            .filter(|item| item.status == AgentStatus::Blocked)
            .count();
        let resting = model
            .items
            .iter()
            .filter(|item| matches!(item.status, AgentStatus::Idle | AgentStatus::Done))
            .count();
        self.summary.set_text(&format!("{} rooms  ·  {working} working  ·  {waiting} need you  ·  {resting} resting  ·  120 character styles", workspaces.len()));
        for item in &model.items {
            if let Some(resident) = self.residents.borrow().get(&item.surface) {
                if resident.actor.borrow().status != item.status {
                    resident.actor.borrow_mut().set_status(item.status);
                    resident.drawing.queue_draw();
                }
                resident.name.set_text(&item.agent_name);
                resident
                    .icon
                    .set_icon_name(Some(crate::builtin_icons::agent_icon_name(
                        &item.agent_name,
                    )));
                let label = format!(
                    "{} · {} · {}",
                    item.agent_name,
                    item.surface_label,
                    status_label(item.status)
                );
                resident
                    .button
                    .update_property(&[gtk::accessible::Property::Label(&label)]);
                resident.button.set_tooltip_text(Some(&format!(
                    "{label}\n{}\n{} · style {:03}",
                    item.status_text,
                    scene::SPECIES[variant(item.surface) % 12],
                    variant(item.surface) + 1
                )));
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn rendered_status(&self, surface: SurfaceId) -> Option<AgentStatus> {
        self.residents
            .borrow()
            .get(&surface)
            .map(|resident| resident.actor.borrow().status)
    }
}

fn position_button(stage: &gtk::Fixed, button: &gtk::Button, actor: &scene::Actor) {
    stage.move_(
        button,
        actor.position.0 * 2.0 - 44.0,
        actor.position.1 * 2.0 - 66.0,
    );
}

fn send_toggle(bridge: &Bridge) {
    let bridge = bridge.clone();
    gtk::glib::MainContext::default().spawn_local(async move {
        let _ = bridge.tx.send(GtkCommand::ToggleAgentOffice).await;
    });
}

fn status_label(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "Working · at the desk",
        AgentStatus::Blocked => "Needs you · waiting for input",
        AgentStatus::Done => "Done · taking a break",
        AgentStatus::Idle => "Idle · resting",
        AgentStatus::Unknown => "Unknown · checking in",
    }
}

fn variant(surface: SurfaceId) -> usize {
    (surface.0.as_u128() % 120) as usize
}

fn resident(
    item: &AgentBarItem,
    actor: Rc<RefCell<scene::Actor>>,
    drawing: &gtk::DrawingArea,
    bridge: &Bridge,
) -> Resident {
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    heading.set_halign(gtk::Align::Center);
    heading.set_valign(gtk::Align::End);
    heading.set_margin_top(66);
    heading.add_css_class("flowmux-office-nameplate");
    let icon = crate::ui::agent_icon(&item.agent_name);
    icon.set_pixel_size(12);
    heading.append(&icon);
    let name = gtk::Label::new(Some(&item.agent_name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(10);
    heading.append(&name);
    let button = gtk::Button::new();
    button.add_css_class("flowmux-office-resident");
    button.set_size_request(88, 86);
    button.set_child(Some(&heading));
    button.set_widget_name(&format!("flowmux-office-resident-{}", item.surface));
    let (workspace, pane, surface) = (item.workspace, item.pane, item.surface);
    let bridge = bridge.clone();
    button.connect_clicked(move |_| {
        let bridge = bridge.clone();
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
    });
    Resident {
        button,
        icon,
        name,
        actor,
        drawing: drawing.clone(),
    }
}
