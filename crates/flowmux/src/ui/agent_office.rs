// SPDX-License-Identifier: GPL-3.0-or-later
//! One adaptive office map backed by the existing live-agent model.

mod art;
mod layout;
mod scene;

use crate::bridge::{Bridge, GtkCommand};
use flowmux_core::{AgentBarItem, AgentBarModel, AgentStatus, PaneId, SurfaceId, WorkspaceId};
use gtk::prelude::*;
use layout::{Plan, Rect, OBJECT_SCALE};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
};

struct Resident {
    button: gtk::Button,
    bubble: gtk::Label,
    nameplate: gtk::Box,
    bubble_focused: Cell<bool>,
    icon: gtk::Image,
    name: gtk::Label,
    actor: scene::Actor,
    location: Rc<Cell<(WorkspaceId, PaneId, SurfaceId)>>,
}

struct Room {
    id: WorkspaceId,
    title: gtk::Button,
    design_button: gtk::Button,
    design: usize,
    plan: Rc<Plan>,
    planned_count: usize,
    members: Vec<SurfaceId>,
    bounds: Rect,
    scale: f64,
    origin: (f64, f64),
    fitted_size: Option<(f64, f64)>,
    background: RefCell<Option<scene::Background>>,
}

#[derive(Default)]
struct World {
    rooms: Vec<Room>,
    residents: HashMap<SurfaceId, Resident>,
    selected: Option<WorkspaceId>,
    all_button: gtk::glib::WeakRef<gtk::Button>,
    frame: u32,
}

impl World {
    fn replan(&mut self, index: usize) {
        let room = &mut self.rooms[index];
        room.fitted_size = None;
        room.background.take();
        room.planned_count = room.members.len();
        let mut capacity = room
            .members
            .iter()
            .filter_map(|id| self.residents.get(id))
            .map(|r| r.actor.slot + 1)
            .max()
            .unwrap_or(2);
        // Reclaim a mostly empty office without reshuffling seats on ordinary joins/leaves.
        let compact = capacity > room.members.len().max(3) * 2;
        if compact {
            room.members.sort_by_key(|id| self.residents[id].actor.slot);
            capacity = room.members.len();
        }
        room.plan = Rc::new(Plan::fit(
            capacity,
            room.design,
            room.plan.width / room.plan.height,
        ));
        for (slot, id) in room.members.iter().enumerate() {
            if let Some(resident) = self.residents.get_mut(id) {
                let slot = if compact { slot } else { resident.actor.slot };
                resident.actor.replan(slot, room.plan.clone());
            }
        }
    }

    fn place(&mut self, stage: &gtk::Fixed, width: i32, height: i32) {
        if let Some(button) = self.all_button.upgrade() {
            button.set_visible(self.selected.is_some() && self.rooms.len() > 1);
        }
        if width <= 0 || height <= 0 {
            return;
        }
        let weights: Vec<_> = self
            .rooms
            .iter()
            .filter(|r| self.selected.is_none_or(|id| id == r.id))
            .map(|r| (r.plan.capacity + 4) as f64)
            .collect();
        let bounds = layout::tiles(
            &weights,
            Rect {
                x: 0.0,
                y: 0.0,
                w: width as f64,
                h: height as f64,
            },
        );
        let mut bounds = bounds.into_iter();
        for room in &mut self.rooms {
            let visible = self.selected.is_none_or(|id| id == room.id);
            room.title.set_visible(visible);
            room.design_button.set_visible(visible);
            for id in &room.members {
                if let Some(r) = self.residents.get(id) {
                    r.button.set_visible(visible);
                    r.nameplate.set_visible(visible);
                    if !visible {
                        r.bubble.set_visible(false);
                    }
                }
            }
            if !visible {
                continue;
            }
            let rect = bounds.next().unwrap();
            room.bounds = rect;
            let header = 28.0_f64.min(rect.h * 0.18);
            let header_scale = header / 28.0;
            room.title
                .set_size_request(((rect.w - 8.0) / header_scale - 32.0).max(1.0) as i32, 28);
            transform(stage, &room.title, rect.x + 4.0, rect.y + 2.0, header_scale);
            transform(
                stage,
                &room.design_button,
                rect.x + rect.w - 30.0 * header_scale - 4.0,
                rect.y + 2.0,
                header_scale,
            );
            let inner_width = (rect.w - 2.0).max(1.0);
            let inner_height = (rect.h - header - 2.0).max(1.0);
            let aspect = inner_width / inner_height;
            if room.fitted_size != Some((inner_width, inner_height)) {
                room.plan = Rc::new(Plan::fit(room.plan.capacity, room.design, aspect));
                room.fitted_size = Some((inner_width, inner_height));
                room.background.take();
                for id in &room.members {
                    if let Some(resident) = self.residents.get_mut(id) {
                        resident
                            .actor
                            .replan(resident.actor.slot, room.plan.clone());
                    }
                }
            }
            room.scale = inner_width / room.plan.width;
            room.origin = (rect.x + 1.0, rect.y + header + 1.0);
            for id in &room.members {
                if let Some(resident) = self.residents.get(id) {
                    place_resident(stage, resident, room);
                }
            }
            place_bubbles(stage, &self.residents, room);
        }
    }
}

pub(crate) struct AgentOffice {
    pub(crate) root: gtk::Box,
    pub(crate) close: gtk::Button,
    summary: gtk::Label,
    drawing: gtk::DrawingArea,
    stage: gtk::Fixed,
    world: Rc<RefCell<World>>,
    bridge: Bridge,
}

impl AgentOffice {
    pub(crate) fn new(bridge: Bridge) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .accessible_role(gtk::AccessibleRole::Group)
            .build();
        root.set_widget_name("flowmux-agent-office");
        root.update_property(&[gtk::accessible::Property::Label("AgentOffice map")]);
        root.add_css_class("flowmux-agent-office");
        root.set_hexpand(true);
        root.set_vexpand(true);
        let world = Rc::new(RefCell::new(World::default()));
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new(
            "AgentOffice",
            "Your agents, under one roof",
        )));
        let close = gtk::Button::from_icon_name("go-previous-symbolic");
        close.set_tooltip_text(Some("Back to workspace (Esc)"));
        close.update_property(&[gtk::accessible::Property::Label("Back to workspace")]);
        header.pack_start(&close);
        let close_bridge = bridge.clone();
        close.connect_clicked(move |_| send_toggle(&close_bridge));
        let all = gtk::Button::with_label("All offices");
        all.set_visible(false);
        world.borrow_mut().all_button = all.downgrade();
        all.set_tooltip_text(Some("Fit every workspace into this window"));
        header.pack_end(&all);
        root.append(&header);
        let summary = gtk::Label::new(None);
        summary.add_css_class("flowmux-office-summary");
        summary.set_wrap(true);
        root.append(&summary);
        let drawing = gtk::DrawingArea::new();
        drawing.set_hexpand(true);
        drawing.set_vexpand(true);
        let stage = gtk::Fixed::new();
        stage.set_overflow(gtk::Overflow::Hidden);
        let map = gtk::Overlay::new();
        map.set_widget_name("flowmux-office-map");
        map.set_vexpand(true);
        map.set_child(Some(&drawing));
        map.add_overlay(&stage);
        map.set_measure_overlay(&stage, false);
        root.append(&map);
        let hint = gtk::Label::new(Some("Select an office name to enlarge  ·  Select a teammate to visit their terminal  ·  24 office designs"));
        hint.add_css_class("flowmux-office-summary");
        hint.set_wrap(true);
        root.append(&hint);
        let draw_world = world.clone();
        drawing.set_draw_func(move |drawing, cr, width, height| {
            cr.set_source_rgb(0.07, 0.10, 0.14);
            let _ = cr.paint();
            let world = draw_world.borrow();
            if world.rooms.is_empty() {
                cr.set_source_rgb(0.8, 0.8, 0.8);
                cr.set_font_size(16.0);
                cr.move_to((width as f64 / 2.0 - 170.0).max(10.0), height as f64 / 2.0);
                let _ = cr.show_text("Start an agent in a workspace to open an office.");
            }
            for room in &world.rooms {
                if world.selected.is_some_and(|id| id != room.id) {
                    continue;
                }
                let _ = cr.save();
                cr.rectangle(room.bounds.x, room.bounds.y, room.bounds.w, room.bounds.h);
                cr.clip();
                cr.translate(room.origin.0, room.origin.1);
                cr.scale(room.scale, room.scale);
                let actors: Vec<_> = room
                    .members
                    .iter()
                    .filter_map(|id| world.residents.get(id).map(|r| &r.actor))
                    .collect();
                let pixel_scale = room.scale * drawing.scale_factor() as f64;
                let mut background = room.background.borrow_mut();
                if background
                    .as_ref()
                    .is_none_or(|cache| cache.scale != pixel_scale)
                {
                    *background = scene::Background::new(&room.plan, room.design, pixel_scale).ok();
                }
                scene::draw_room(
                    cr,
                    &room.plan,
                    room.design,
                    &actors,
                    world.frame,
                    background.as_ref(),
                );
                let _ = cr.restore();
            }
        });
        let resize_world = world.clone();
        let resize_stage = stage.clone();
        drawing.connect_resize(move |_, w, h| resize_world.borrow_mut().place(&resize_stage, w, h));
        let all_world = world.clone();
        let all_stage = stage.downgrade();
        let all_drawing = drawing.downgrade();
        all.connect_clicked(move |_| {
            if let (Some(stage), Some(drawing)) = (all_stage.upgrade(), all_drawing.upgrade()) {
                let mut world = all_world.borrow_mut();
                world.selected = None;
                world.place(&stage, drawing.width(), drawing.height());
                drawing.queue_draw();
            }
        });
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
        let tick_world = world.clone();
        let weak_stage = stage.downgrade();
        let last = Cell::new(0);
        drawing.add_tick_callback(move |drawing, clock| {
            let now = clock.frame_time();
            let previous = last.get();
            if previous != 0 && now - previous < 33_000 {
                return gtk::glib::ControlFlow::Continue;
            }
            last.set(now);
            let dt = if previous == 0 {
                0.0
            } else {
                ((now - previous) as f64 / 1_000_000.0).min(0.1)
            };
            let animate = adw::is_animations_enabled(drawing);
            let frame = if animate { (now / 160_000) as u32 } else { 0 };
            let mut world = tick_world.borrow_mut();
            let mut changed = world.frame != frame;
            world.frame = frame;
            let Some(stage) = weak_stage.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            let mut expired = Vec::new();
            let mut positions: HashMap<_, _> = world
                .residents
                .iter()
                .map(|(id, r)| (*id, (r.location.get().0, r.actor.position)))
                .collect();
            let mut order: Vec<_> = world.residents.keys().copied().collect();
            order.sort_by_key(|id| id.0);
            for id in order {
                let resident = world.residents.get_mut(&id).unwrap();
                let focused = resident.button.has_focus()
                    || resident
                        .button
                        .state_flags()
                        .contains(gtk::StateFlags::PRELIGHT);
                changed |= resident.bubble_focused.replace(focused) != focused;
                changed |= resident.actor.status_age <= 6.1;
                let neighbors = positions
                    .iter()
                    .filter(|(other, (workspace, _))| {
                        **other != id && *workspace == resident.location.get().0
                    })
                    .map(|(_, (_, point))| *point);
                if !animate || !resident.actor.yield_to(dt, neighbors) {
                    changed |= resident.actor.advance(dt, animate);
                    positions.insert(id, (resident.location.get().0, resident.actor.position));
                }
                if resident.actor.departed() {
                    expired.push(id);
                }
            }
            for id in &expired {
                if let Some(resident) = world.residents.remove(id) {
                    stage.remove(&resident.button);
                    stage.remove(&resident.bubble);
                    stage.remove(&resident.nameplate);
                }
                for room in &mut world.rooms {
                    room.members.retain(|member| member != id);
                }
            }
            if !expired.is_empty() {
                for i in 0..world.rooms.len() {
                    if world.rooms[i].planned_count != world.rooms[i].members.len() {
                        world.replan(i);
                    }
                }
                world.place(&stage, drawing.width(), drawing.height());
                changed = true;
            } else if changed {
                for room in &world.rooms {
                    if world.selected.is_some_and(|id| id != room.id) {
                        continue;
                    }
                    for id in &room.members {
                        if let Some(resident) = world.residents.get(id) {
                            place_resident(&stage, resident, room);
                        }
                    }
                    place_bubbles(&stage, &world.residents, room);
                }
            }
            if changed {
                drawing.queue_draw();
            }
            gtk::glib::ControlFlow::Continue
        });
        Self {
            root,
            close,
            summary,
            drawing,
            stage,
            world,
            bridge,
        }
    }

    pub(crate) fn render(&self, workspaces: &[(WorkspaceId, String)], model: &AgentBarModel) {
        let mut world = self.world.borrow_mut();
        let mut rooms = Vec::new();
        for (id, name) in workspaces {
            let live_count = model.items.iter().filter(|i| i.workspace == *id).count();
            if live_count == 0 {
                continue;
            }
            let mut room = if let Some(index) = world.rooms.iter().position(|r| r.id == *id) {
                world.rooms.remove(index)
            } else {
                self.new_room(*id)
            };
            room.title.set_label(&format!("{name} · {live_count}"));
            if let Some(label) = room.title.child().and_downcast::<gtk::Label>() {
                label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            }
            let mut added: Vec<_> = model
                .items
                .iter()
                .filter(|item| item.workspace == *id && !room.members.contains(&item.surface))
                .collect();
            added.sort_by_key(|item| item.surface.0);
            for item in added {
                let slot = (0..)
                    .find(|slot| {
                        !room.members.iter().any(|id| {
                            world
                                .residents
                                .get(id)
                                .is_some_and(|r| r.actor.slot == *slot)
                        })
                    })
                    .unwrap();
                room.members.push(item.surface);
                let plan = if slot >= room.plan.capacity {
                    Rc::new(Plan::fit(
                        slot + 1,
                        room.design,
                        room.plan.width / room.plan.height,
                    ))
                } else {
                    room.plan.clone()
                };
                let resident = resident(item, slot, plan, &self.bridge);
                self.stage.put(&resident.button, 0.0, 0.0);
                self.stage.put(&resident.nameplate, 0.0, 0.0);
                self.stage.put(&resident.bubble, 0.0, 0.0);
                if let Some(old) = world.residents.insert(item.surface, resident) {
                    self.stage.remove(&old.button);
                    self.stage.remove(&old.bubble);
                    self.stage.remove(&old.nameplate);
                }
            }
            room.members.retain(|surface| {
                model
                    .items
                    .iter()
                    .find(|i| i.surface == *surface)
                    .is_none_or(|i| i.workspace == *id)
            });
            room.title.set_tooltip_text(Some(&format!(
                "{name} · {live_count} teammates\n{}\nSelect to enlarge",
                scene::design_name(room.design)
            )));
            rooms.push(room);
        }
        for old in &world.rooms {
            self.stage.remove(&old.title);
            self.stage.remove(&old.design_button);
        }
        world.rooms = rooms;
        if world
            .selected
            .is_some_and(|id| !world.rooms.iter().any(|room| room.id == id))
        {
            world.selected = None;
        }
        let retained: Vec<_> = world
            .rooms
            .iter()
            .flat_map(|room| room.members.iter().copied())
            .collect();
        world.residents.retain(|id, resident| {
            if retained.contains(id) {
                true
            } else {
                self.stage.remove(&resident.button);
                self.stage.remove(&resident.bubble);
                self.stage.remove(&resident.nameplate);
                false
            }
        });
        for index in 0..world.rooms.len() {
            if world.rooms[index].planned_count != world.rooms[index].members.len() {
                world.replan(index);
            }
        }
        for (id, resident) in &mut world.residents {
            if let Some(item) = model.items.iter().find(|item| item.surface == *id) {
                resident.actor.set_status(item.status);
                let activity = item.status_text.to_ascii_lowercase();
                resident.actor.reading = ["read", "search", "grep", "glob", "inspect"]
                    .iter()
                    .any(|word| activity.contains(word));
                resident
                    .location
                    .set((item.workspace, item.pane, item.surface));
                resident.name.set_text(&item.agent_name);
                resident
                    .icon
                    .set_icon_name(Some(crate::builtin_icons::agent_icon_name(
                        &item.agent_name,
                    )));
                resident.button.set_sensitive(true);
                let label = format!(
                    "{} · {} · {}",
                    item.agent_name,
                    item.surface_label,
                    status_label(item.status)
                );
                let detail = if item.status_text == item.status.as_str() {
                    ""
                } else {
                    item.status_text.as_str()
                };
                resident.bubble.set_text(&format!(
                    "{}{}{}",
                    speech(item.status),
                    if detail.is_empty() { "" } else { "\n" },
                    detail
                ));
                resident
                    .button
                    .update_property(&[gtk::accessible::Property::Label(&label)]);
                resident.button.set_tooltip_text(Some(&format!(
                    "{label}\n{}\n{} · style {:03}",
                    item.status_text,
                    scene::SPECIES[variant(*id) % 12],
                    variant(*id) + 1
                )));
            } else {
                resident.actor.finish();
                resident.bubble.set_text("Session ended\nHeading out");
                resident.button.set_sensitive(false);
                resident
                    .button
                    .update_property(&[gtk::accessible::Property::Label("Session ended")]);
            }
        }
        world.place(&self.stage, self.drawing.width(), self.drawing.height());
        let working = model
            .items
            .iter()
            .filter(|i| i.status == AgentStatus::Working)
            .count();
        let waiting = model
            .items
            .iter()
            .filter(|i| i.status == AgentStatus::Blocked)
            .count();
        let resting = model
            .items
            .iter()
            .filter(|i| matches!(i.status, AgentStatus::Idle | AgentStatus::Done))
            .count();
        self.summary.set_text(&format!("{} offices · {} teammates · {working} working · {waiting} need you · {resting} resting", world.rooms.len(), model.items.len()));
        self.drawing.queue_draw();
    }

    fn new_room(&self, id: WorkspaceId) -> Room {
        let saved = flowmux_state::office_designs::load(id).unwrap_or_else(|error| {
            tracing::warn!(%error, "Could not load office design");
            None
        });
        let design = saved
            .filter(|design| *design < 24)
            .map(usize::from)
            .unwrap_or((id.0.as_u128() % 24) as usize);
        let title = gtk::Button::new();
        title.add_css_class("flowmux-office-title");
        title.set_widget_name(&format!("flowmux-office-room-{id}"));
        let design_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        design_button.add_css_class("flowmux-office-title");
        design_button.set_size_request(28, 28);
        design_button.set_tooltip_text(Some("Next office design · 24 layouts and themes"));
        design_button.update_property(&[gtk::accessible::Property::Label("Next office design")]);
        self.stage.put(&title, 0.0, 0.0);
        self.stage.put(&design_button, 0.0, 0.0);
        for (button, change_design) in [(&title, false), (&design_button, true)] {
            let world = Rc::downgrade(&self.world);
            let stage = self.stage.downgrade();
            let drawing = self.drawing.downgrade();
            button.connect_clicked(move |_| {
                if let (Some(world), Some(stage), Some(drawing)) =
                    (world.upgrade(), stage.upgrade(), drawing.upgrade())
                {
                    let mut world = world.borrow_mut();
                    if change_design {
                        if let Some(index) = world.rooms.iter().position(|r| r.id == id) {
                            world.rooms[index].design = (world.rooms[index].design + 1) % 24;
                            let name = scene::design_name(world.rooms[index].design);
                            world.rooms[index]
                                .design_button
                                .set_tooltip_text(Some(&format!(
                                    "{name}\nNext office design · 24 layouts and themes"
                                )));
                            if let Err(error) = flowmux_state::office_designs::save(
                                id,
                                world.rooms[index].design as u8,
                            ) {
                                tracing::warn!(%error, "Could not save office design");
                                world.rooms[index]
                                    .design_button
                                    .set_tooltip_text(Some(&format!(
                                        "{name}\nCould not save design: {error}"
                                    )));
                            }
                            world.replan(index);
                        }
                    } else {
                        world.selected = if world.selected == Some(id) {
                            None
                        } else {
                            Some(id)
                        };
                    }
                    world.place(&stage, drawing.width(), drawing.height());
                    drawing.queue_draw();
                }
            });
        }
        Room {
            id,
            title,
            design_button,
            design,
            plan: Rc::new(Plan::new(0, design)),
            planned_count: 0,
            members: Vec::new(),
            bounds: Rect::default(),
            scale: 1.0,
            origin: (0.0, 0.0),
            fitted_size: None,
            background: RefCell::new(None),
        }
    }

    #[cfg(test)]
    pub(crate) fn room_titles(&self) -> Vec<String> {
        self.world
            .borrow()
            .rooms
            .iter()
            .map(|r| r.title.label().unwrap().to_string())
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn rendered_status(&self, surface: SurfaceId) -> Option<AgentStatus> {
        self.world
            .borrow()
            .residents
            .get(&surface)
            .filter(|r| r.actor.ended.is_none())
            .map(|r| r.actor.status)
    }
}

fn transform(stage: &gtk::Fixed, widget: &impl IsA<gtk::Widget>, x: f64, y: f64, scale: f64) {
    let transform = gtk::gsk::Transform::new()
        .translate(&gtk::graphene::Point::new(x as f32, y as f32))
        .scale(scale as f32, scale as f32);
    if stage.child_transform(widget).as_ref() != Some(&transform) {
        stage.set_child_transform(widget, Some(&transform));
    }
}
fn place_resident(stage: &gtk::Fixed, resident: &Resident, room: &Room) {
    transform(
        stage,
        &resident.button,
        room.origin.0 + (resident.actor.position.0 - 12.0 * OBJECT_SCALE) * room.scale,
        room.origin.1 + (resident.actor.position.1 - 48.0 * OBJECT_SCALE) * room.scale,
        room.scale * OBJECT_SCALE / 2.0,
    );
    transform(
        stage,
        &resident.nameplate,
        room.origin.0 + (resident.actor.position.0 - 40.0 * OBJECT_SCALE) * room.scale,
        room.origin.1 + (resident.actor.position.1 + 4.0 * OBJECT_SCALE) * room.scale,
        room.scale * OBJECT_SCALE / 2.0,
    );
}
fn place_bubbles(stage: &gtk::Fixed, residents: &HashMap<SurfaceId, Resident>, room: &Room) {
    let mut candidates: Vec<_> = room
        .members
        .iter()
        .filter_map(|id| residents.get(id))
        .collect();
    candidates.sort_by_key(|r| {
        let focus =
            r.button.has_focus() || r.button.state_flags().contains(gtk::StateFlags::PRELIGHT);
        (
            !focus,
            r.actor.status != AgentStatus::Blocked,
            r.actor.ended.is_none(),
            r.actor.slot,
        )
    });
    let scale = (room.scale / 2.0).clamp(0.75, 1.0);
    let mut occupied: Vec<Rect> = Vec::new();
    for r in candidates {
        let focus =
            r.button.has_focus() || r.button.state_flags().contains(gtk::StateFlags::PRELIGHT);
        let persistent = r.actor.status == AgentStatus::Blocked || r.actor.ended.is_some();
        let duration = if r.actor.status == AgentStatus::Done {
            6.0
        } else {
            3.0
        };
        if !focus && !persistent && r.actor.status_age > duration {
            r.bubble.set_visible(false);
            continue;
        }
        // Font fallback and multiple lines can exceed the requested minimum height.
        r.bubble.set_visible(true);
        let (_, width, _, _) = r.bubble.measure(gtk::Orientation::Horizontal, -1);
        let (_, height, _, _) = r.bubble.measure(gtk::Orientation::Vertical, width);
        let (w, h) = (width as f64 * scale, height as f64 * scale);
        if room.bounds.w < w + 8.0 || room.bounds.h < h + 36.0 {
            r.bubble.set_visible(false);
            continue;
        }
        let x = (room.origin.0 + r.actor.position.0 * room.scale - w / 2.0)
            .clamp(room.bounds.x + 4.0, room.bounds.x + room.bounds.w - w - 4.0);
        let above = room.origin.1 + (r.actor.position.1 - 50.0 * OBJECT_SCALE) * room.scale - h;
        let top = room.bounds.y + 32.0;
        let mut placed = false;
        for offset in [0.0, -h - 4.0, h + 4.0] {
            let rect = Rect {
                x,
                y: (above + offset).clamp(top, room.bounds.y + room.bounds.h - h - 4.0),
                w,
                h,
            };
            if occupied.iter().any(|other| {
                rect.x < other.x + other.w + 4.0
                    && other.x < rect.x + rect.w + 4.0
                    && rect.y < other.y + other.h + 4.0
                    && other.y < rect.y + rect.h + 4.0
            }) {
                continue;
            }
            transform(stage, &r.bubble, rect.x, rect.y, scale);
            placed = true;
            occupied.push(rect);
            break;
        }
        r.bubble.set_visible(placed);
    }
}
fn send_toggle(bridge: &Bridge) {
    let bridge = bridge.clone();
    gtk::glib::MainContext::default().spawn_local(async move {
        let _ = bridge.tx.send(GtkCommand::ToggleAgentOffice).await;
    });
}
fn speech(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Working => "Working…",
        AgentStatus::Blocked => "Need your input",
        AgentStatus::Done => "Task complete!",
        AgentStatus::Idle => "Taking a break",
        AgentStatus::Unknown => "Checking in…",
    }
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
fn resident(item: &AgentBarItem, slot: usize, plan: Rc<Plan>, bridge: &Bridge) -> Resident {
    let bubble = gtk::Label::new(None);
    bubble.add_css_class("flowmux-office-bubble");
    bubble.set_ellipsize(gtk::pango::EllipsizeMode::End);
    bubble.set_lines(2);
    bubble.set_max_width_chars(18);
    bubble.set_size_request(156, 46);
    bubble.set_can_target(false);
    let heading = gtk::Box::new(gtk::Orientation::Horizontal, 3);
    heading.set_halign(gtk::Align::Center);
    heading.add_css_class("flowmux-office-nameplate");
    let icon = crate::ui::agent_icon(&item.agent_name);
    icon.set_pixel_size(12);
    heading.append(&icon);
    let name = gtk::Label::new(Some(&item.agent_name));
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    name.set_max_width_chars(16);
    heading.append(&name);
    let nameplate = gtk::Box::new(gtk::Orientation::Vertical, 0);
    nameplate.set_size_request(160, -1);
    nameplate.set_can_target(false);
    nameplate.append(&heading);
    let button = gtk::Button::new();
    button.add_css_class("flowmux-office-resident");
    // Only the body is interactive; speech and the nameplate must not steal nearby clicks.
    button.set_size_request(48, 104);
    button.set_widget_name(&format!("flowmux-office-resident-{}", item.surface));
    let location = Rc::new(Cell::new((item.workspace, item.pane, item.surface)));
    let target = location.clone();
    let bridge = bridge.clone();
    button.connect_clicked(move |_| {
        let (workspace, pane, surface) = target.get();
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
        bubble,
        nameplate,
        bubble_focused: Cell::new(false),
        name,
        icon,
        actor: scene::Actor::new(slot, variant(item.surface), item.status, plan),
        location,
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    #[gtk::test]
    fn office_keeps_seats_reuses_vacancies_and_restores_design() {
        let (bridge, _receiver) = Bridge::new();
        let office = AgentOffice::new(bridge.clone());
        let workspace = WorkspaceId::new();
        let rooms = [(workspace, "Studio".into())];
        let mut model = AgentBarModel {
            visible: true,
            items: (1..=3)
                .map(|id| AgentBarItem {
                    workspace,
                    pane: PaneId::new(),
                    surface: SurfaceId(uuid::Uuid::from_u128(id)),
                    surface_label: "Terminal".into(),
                    agent_name: "codex".into(),
                    status: AgentStatus::Working,
                    visual_status: flowmux_core::AgentBarVisualStatus::Working,
                    seen: false,
                    status_text: "working".into(),
                    color: "#ffffff".into(),
                })
                .collect(),
        };
        office.render(&rooms, &model);
        let all = office.world.borrow().all_button.upgrade().unwrap();
        let title = office.world.borrow().rooms[0].title.clone();
        assert!(!all.is_visible());
        title.emit_clicked();
        assert!(
            !all.is_visible(),
            "one office never needs an overview button"
        );
        all.emit_clicked();
        assert!(!all.is_visible());
        let survivor = model.items[1].surface;
        assert_eq!(office.world.borrow().residents[&survivor].actor.slot, 1);
        let first = model.items.remove(0);
        office.render(&rooms, &model);
        {
            let mut world = office.world.borrow_mut();
            let departed = world.residents.remove(&first.surface).unwrap();
            office.stage.remove(&departed.button);
            office.stage.remove(&departed.bubble);
            office.stage.remove(&departed.nameplate);
            world.rooms[0].members.retain(|id| *id != first.surface);
            world.replan(0);
        }
        assert_eq!(office.world.borrow().residents[&survivor].actor.slot, 1);
        model.items.push(AgentBarItem {
            surface: SurfaceId::new(),
            ..first
        });
        office.render(&rooms, &model);
        assert_eq!(office.world.borrow().residents[&survivor].actor.slot, 1);
        assert_eq!(
            office.world.borrow().residents[&model.items[2].surface]
                .actor
                .slot,
            0
        );
        let button = office.world.borrow().rooms[0].design_button.clone();
        button.emit_clicked();
        let design = office.world.borrow().rooms[0].design;
        let reopened = AgentOffice::new(bridge);
        reopened.render(&rooms, &model);
        assert_eq!(reopened.world.borrow().rooms[0].design, design);
        reopened.render(
            &rooms,
            &AgentBarModel {
                visible: false,
                items: vec![],
            },
        );
        assert!(reopened.world.borrow().rooms.is_empty());
        reopened.render(&rooms, &model);
        assert_eq!(reopened.world.borrow().rooms[0].design, design);
        let mut world = reopened.world.borrow_mut();
        world
            .residents
            .get_mut(&survivor)
            .unwrap()
            .actor
            .replan(30, Rc::new(Plan::new(31, design)));
        world.replan(0);
        assert_eq!(
            world.rooms[0].plan.capacity, 3,
            "sparse offices reclaim unused furniture"
        );
        drop(world);
        let second = WorkspaceId::new();
        let rooms = [(workspace, "Studio".into()), (second, "Workshop".into())];
        for _ in 0..12 {
            model.items.push(AgentBarItem {
                workspace: second,
                surface: SurfaceId::new(),
                ..model.items[0].clone()
            });
        }
        reopened.render(&rooms, &model);
        let all = reopened.world.borrow().all_button.upgrade().unwrap();
        let title = reopened.world.borrow().rooms[0].title.clone();
        title.emit_clicked();
        assert!(
            all.is_visible(),
            "multiple offices expose the overview from detail"
        );
        all.emit_clicked();
        assert!(!all.is_visible());
        let mut world = reopened.world.borrow_mut();
        for (width, height) in [(1280, 700), (600, 900)] {
            world.place(&reopened.stage, width, height);
            assert_eq!(world.rooms.len(), 2);
            for resident in world.residents.values() {
                assert_ne!(resident.actor.action(), scene::Action::Walk);
            }
            assert!(world
                .rooms
                .iter()
                .all(|r| (r.scale - ((r.bounds.w - 2.0) / r.plan.width)).abs() < 0.001));
            let plans: Vec<_> = world.rooms.iter().map(|r| r.plan.clone()).collect();
            world.place(&reopened.stage, width, height);
            for (room, cached) in world.rooms.iter().zip(plans) {
                assert!(
                    Rc::ptr_eq(&room.plan, &cached),
                    "unchanged view must reuse its navigation grid"
                );
                assert!((room.plan.width * room.scale - (room.bounds.w - 2.0)).abs() < 0.01);
                let header = 28.0_f64.min(room.bounds.h * 0.18);
                assert!(
                    (room.plan.height * room.scale - (room.bounds.h - header - 2.0)).abs() < 0.01
                );
            }
        }
    }
}
