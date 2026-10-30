// SPDX-License-Identifier: GPL-3.0-or-later
//! Session Flow's cosmic canvas. The scene is shared; only view/camera state belongs to a workspace.

use crate::bridge::{Bridge, GtkCommand};
use flowmux_core::session_flow::{Camera, Layout, Lineage, Point, SessionKey};
use flowmux_core::{PaneId, SurfaceId, WorkspaceId};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum FlowAction {
    Toggle(WorkspaceId),
    Refresh,
    SaveView,
    Open(SessionKey),
    Fork { pane: PaneId, surface: SurfaceId },
}

#[derive(Clone, Debug)]
pub(crate) struct Node {
    pub title: String,
    pub detail: String,
    pub available: bool,
    pub running: bool,
}

use flowmux_state::session_flow::SavedView as View;

#[derive(Default)]
struct Scene {
    layout: Layout,
    all: Lineage,
    graph: Lineage,
    nodes: BTreeMap<SessionKey, Node>,
    anchors: BTreeSet<SessionKey>,
    workspace: Option<WorkspaceId>,
    views: HashMap<WorkspaceId, View>,
    cursor: Point,
    drag_start: Point,
    drag_origin: Point,
    held: Option<SessionKey>,
    moved: bool,
}

impl Scene {
    fn camera(&self) -> Camera {
        self.workspace
            .and_then(|id| self.views.get(&id))
            .map(|v| v.camera)
            .unwrap_or_default()
    }
    fn view(&mut self) -> Option<&mut View> {
        Some(self.views.entry(self.workspace?).or_default())
    }
    fn hit(&self, cursor: Point) -> Option<SessionKey> {
        let camera = self.camera();
        self.graph
            .nodes()
            .into_iter()
            .find(|key| {
                self.layout.positions.get(*key).is_some_and(|point| {
                    let p = camera.screen(*point);
                    (p.x - cursor.x).hypot(p.y - cursor.y) <= 18.0
                })
            })
            .cloned()
    }
    fn fit(&mut self, width: f64, height: f64) {
        let mut camera = self.camera();
        camera.fit(
            self.graph
                .nodes()
                .into_iter()
                .filter_map(|key| self.layout.positions.get(key).copied()),
            width,
            height,
        );
        if let Some(view) = self.view() {
            view.camera = camera;
            view.initialized = true;
        }
    }
}

#[derive(Clone)]
pub(crate) struct FlowPanel {
    pub host: gtk::Paned,
    pub root: gtk::Box,
    pub area: gtk::DrawingArea,
    pub status: gtk::Label,
    scene: Rc<RefCell<Scene>>,
    animating: Rc<Cell<bool>>,
    bridge: Bridge,
    save_pending: Rc<Cell<bool>>,
}

struct WeakPanel {
    host: glib::WeakRef<gtk::Paned>,
    root: glib::WeakRef<gtk::Box>,
    area: glib::WeakRef<gtk::DrawingArea>,
    status: glib::WeakRef<gtk::Label>,
    scene: Rc<RefCell<Scene>>,
    animating: Rc<Cell<bool>>,
    bridge: Bridge,
    save_pending: Rc<Cell<bool>>,
}
impl WeakPanel {
    fn upgrade(&self) -> Option<FlowPanel> {
        Some(FlowPanel {
            host: self.host.upgrade()?,
            root: self.root.upgrade()?,
            area: self.area.upgrade()?,
            status: self.status.upgrade()?,
            scene: self.scene.clone(),
            animating: self.animating.clone(),
            bridge: self.bridge.clone(),
            save_pending: self.save_pending.clone(),
        })
    }
}

impl FlowPanel {
    fn downgrade(&self) -> WeakPanel {
        WeakPanel {
            host: self.host.downgrade(),
            root: self.root.downgrade(),
            area: self.area.downgrade(),
            status: self.status.downgrade(),
            scene: self.scene.clone(),
            animating: self.animating.clone(),
            bridge: self.bridge.clone(),
            save_pending: self.save_pending.clone(),
        }
    }
    pub fn new(stack: &gtk::Stack, bridge: Bridge) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("flowmux-session-flow");
        root.set_visible(false);
        let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        toolbar.set_margin_start(12);
        toolbar.set_margin_end(8);
        let title = gtk::Label::new(Some("Session Flow"));
        title.add_css_class("heading");
        toolbar.append(&title);
        let status = gtk::Label::new(Some("Fork a session to begin its flow"));
        status.set_hexpand(true);
        status.set_xalign(0.0);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.add_css_class("caption");
        toolbar.append(&status);
        root.append(&toolbar);
        let area = gtk::DrawingArea::new();
        area.set_widget_name("flowmux-session-flow-canvas");
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.set_focusable(true);
        area.set_content_height(140);
        area.set_tooltip_text(Some("Scroll to zoom · drag a node to move it · drag space to pan · arrows to select · Enter to open"));
        root.append(&area);
        let host = gtk::Paned::new(gtk::Orientation::Vertical);
        host.set_hexpand(true);
        host.set_vexpand(true);
        host.set_start_child(Some(&root));
        host.set_end_child(Some(stack));
        host.set_resize_start_child(false);
        host.set_resize_end_child(true);
        host.set_shrink_start_child(false);
        host.set_shrink_end_child(false);
        let panel = Self {
            host,
            root,
            area,
            status,
            scene: Rc::new(RefCell::new(Scene::default())),
            animating: Rc::new(Cell::new(false)),
            bridge,
            save_pending: Rc::new(Cell::new(false)),
        };
        for (label, tooltip, action) in [
            ("−", "Zoom out", 0),
            ("+", "Zoom in", 1),
            ("Fit", "Fit all families", 2),
            ("↻", "Refresh native session history", 3),
            ("×", "Close Session Flow", 4),
        ] {
            let button = gtk::Button::with_label(label);
            button.add_css_class("flat");
            button.set_tooltip_text(Some(tooltip));
            let p = panel.downgrade();
            button.connect_clicked(move |_| {
                let Some(p) = p.upgrade() else {
                    return;
                };
                if action <= 2 {
                    p.schedule_save();
                }
                match action {
                    0 | 1 => {
                        let cursor = Point {
                            x: p.area.width() as f64 / 2.0,
                            y: p.area.height() as f64 / 2.0,
                        };
                        if let Some(v) = p.scene.borrow_mut().view() {
                            v.camera
                                .zoom_at(cursor, if action == 0 { 0.8 } else { 1.25 });
                        }
                        p.area.queue_draw();
                    }
                    2 => {
                        p.scene
                            .borrow_mut()
                            .fit(p.area.width() as f64, p.area.height() as f64);
                        p.area.queue_draw();
                    }
                    3 => p.send(FlowAction::Refresh),
                    _ => {
                        if let Some(id) = p.scene.borrow().workspace {
                            p.send(FlowAction::Toggle(id));
                        }
                    }
                }
            });
            toolbar.append(&button);
        }
        panel.connect_canvas();
        panel
    }

    fn schedule_save(&self) {
        if self.save_pending.replace(true) {
            return;
        }
        let weak = self.downgrade();
        glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
            if let Some(panel) = weak.upgrade() {
                panel.save_pending.set(false);
                panel.send(FlowAction::SaveView);
            }
        });
    }

    fn send(&self, action: FlowAction) {
        let _ = self.bridge.tx.try_send(GtkCommand::SessionFlow(action));
    }

    pub fn show_workspace(
        &self,
        workspace: Option<WorkspaceId>,
        graph: Lineage,
        anchors: BTreeSet<SessionKey>,
    ) {
        let mut scene = self.scene.borrow_mut();
        scene.workspace = workspace;
        scene.graph = graph;
        scene.anchors = anchors;
        let view = scene.view().cloned().unwrap_or_default();
        drop(scene);
        self.root.set_visible(view.open);
        if view.open && view.height > 0 {
            self.host.set_position(view.height);
        }
        self.area.queue_draw();
        self.animate();
    }

    pub fn toggle(&self, workspace: WorkspaceId) {
        let mut scene = self.scene.borrow_mut();
        scene.workspace = Some(workspace);
        let view = scene.views.entry(workspace).or_default();
        view.open = !view.open;
        if view.height == 0 {
            view.height = (self.host.height() / 3).max(180);
        }
        let (open, height) = (view.open, view.height);
        drop(scene);
        self.root.set_visible(open);
        self.host.set_position(height);
        self.area.queue_draw();
        self.animate();
    }

    pub fn set_nodes(
        &self,
        all: Lineage,
        nodes: BTreeMap<SessionKey, Node>,
        positions: &BTreeMap<SessionKey, Point>,
    ) {
        let mut scene = self.scene.borrow_mut();
        for (key, point) in positions {
            scene.layout.positions.entry(key.clone()).or_insert(*point);
        }
        if scene.all != all {
            scene.layout.sync(&all);
        }
        scene.all = all;
        scene.nodes = nodes;
        drop(scene);
        self.area.queue_draw();
        self.animate();
    }

    pub fn saved_views(&self) -> BTreeMap<uuid::Uuid, View> {
        self.scene
            .borrow()
            .views
            .iter()
            .map(|(id, view)| (id.0, view.clone()))
            .collect()
    }

    pub fn restore_views(&self, views: &BTreeMap<uuid::Uuid, View>) {
        let mut scene = self.scene.borrow_mut();
        for (id, view) in views {
            scene
                .views
                .entry(WorkspaceId(*id))
                .or_insert_with(|| view.clone());
        }
    }

    pub fn positions(&self) -> BTreeMap<SessionKey, Point> {
        self.scene.borrow().layout.positions.clone()
    }

    fn activate(&self, key: SessionKey) {
        let mut scene = self.scene.borrow_mut();
        let node = scene.nodes.get(&key).cloned();
        if let Some(node) = node {
            self.status.set_text(&node.detail);
            if node.available {
                if let Some(view) = scene.view() {
                    view.selected = Some(key.clone());
                }
                drop(scene);
                self.send(FlowAction::Open(key));
            }
        }
        self.area.queue_draw();
    }

    fn animate(&self) {
        if self.animating.get()
            || !self.area.is_mapped()
            || !self.area.settings().is_gtk_enable_animations()
        {
            return;
        }
        self.animating.set(true);
        let scene = self.scene.clone();
        let running = self.animating.clone();
        self.area.add_tick_callback(move |area, _| {
            if !area.is_mapped() || !area.settings().is_gtk_enable_animations() {
                running.set(false);
                return glib::ControlFlow::Break;
            }
            let mut scene = scene.borrow_mut();
            let Scene {
                layout, all, held, ..
            } = &mut *scene;
            let active = layout.step(all, held.as_ref());
            area.queue_draw();
            if active {
                glib::ControlFlow::Continue
            } else {
                running.set(false);
                glib::ControlFlow::Break
            }
        });
    }

    fn connect_canvas(&self) {
        let scene = self.scene.clone();
        self.area.set_draw_func(move |area, cr, width, height| {
            let mut scene = scene.borrow_mut();
            if !scene.graph.is_empty() && scene.view().is_some_and(|v| !v.initialized) {
                scene.fit(width as f64, height as f64);
            }
            draw(area, cr, width, height, &scene);
        });
        let p = self.downgrade();
        self.area.connect_map(move |_| {
            if let Some(p) = p.upgrade() {
                p.animate();
            }
        });
        let scene = self.scene.clone();
        self.host.connect_position_notify(move |host| {
            if let Some(view) = scene.borrow_mut().view() {
                if view.open {
                    view.height = host.position();
                }
            }
        });
        let motion = gtk::EventControllerMotion::new();
        let scene = self.scene.clone();
        let area = self.area.downgrade();
        motion.connect_motion(move |_, x, y| {
            let mut scene = scene.borrow_mut();
            scene.cursor = Point { x, y };
            if let Some(area) = area.upgrade() {
                let node = scene
                    .hit(scene.cursor)
                    .and_then(|key| scene.nodes.get(&key));
                area.set_cursor_from_name(Some(if node.is_some_and(|n| n.available) {
                    "pointer"
                } else {
                    "grab"
                }));
                area.set_tooltip_text(
                    node.map(|n| n.detail.as_str())
                        .or(Some("Scroll to zoom · drag space to pan")),
                );
            }
        });
        self.area.add_controller(motion);
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let scene = self.scene.clone();
        let area = self.area.downgrade();
        let panel = self.downgrade();
        scroll.connect_scroll(move |_, _, dy| {
            if let Some(panel) = panel.upgrade() {
                panel.schedule_save();
            }
            let mut scene = scene.borrow_mut();
            let cursor = scene.cursor;
            if let Some(v) = scene.view() {
                v.camera.zoom_at(cursor, (-dy * 0.15).exp());
            }
            if let Some(area) = area.upgrade() {
                area.queue_draw();
            }
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);
        let drag = gtk::GestureDrag::new();
        drag.set_button(1);
        let p = self.downgrade();
        drag.connect_drag_begin(move |_, x, y| {
            let Some(p) = p.upgrade() else {
                return;
            };
            p.area.grab_focus();
            let mut s = p.scene.borrow_mut();
            s.drag_start = Point { x, y };
            s.held = s.hit(s.drag_start);
            s.moved = false;
            s.drag_origin = s
                .held
                .as_ref()
                .and_then(|k| s.layout.positions.get(k))
                .copied()
                .unwrap_or(s.camera().origin);
        });
        let p = self.downgrade();
        drag.connect_drag_update(move |_, x, y| {
            let Some(p) = p.upgrade() else {
                return;
            };
            let mut s = p.scene.borrow_mut();
            if x.hypot(y) < 4.0 && !s.moved {
                return;
            }
            s.moved = true;
            let origin = s.drag_origin;
            if let Some(key) = s.held.clone() {
                let zoom = s.camera().zoom;
                s.layout.drag(
                    &key,
                    Point {
                        x: origin.x + x / zoom,
                        y: origin.y + y / zoom,
                    },
                );
            } else if let Some(v) = s.view() {
                v.camera.origin = Point {
                    x: origin.x + x,
                    y: origin.y + y,
                };
            }
            drop(s);
            p.area.queue_draw();
            p.animate();
        });
        let p = self.downgrade();
        drag.connect_drag_end(move |_, _, _| {
            let Some(p) = p.upgrade() else {
                return;
            };
            let mut s = p.scene.borrow_mut();
            let key = s.held.take();
            let moved = s.moved;
            p.schedule_save();
            drop(s);
            if !moved {
                if let Some(key) = key {
                    p.activate(key);
                }
            }
        });
        let scene = self.scene.clone();
        drag.connect_cancel(move |_, _| {
            let mut s = scene.borrow_mut();
            s.held = None;
            s.moved = false;
        });
        self.area.add_controller(drag);
        let keys = gtk::EventControllerKey::new();
        let p = self.downgrade();
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(p) = p.upgrade() else {
                return glib::Propagation::Proceed;
            };
            use gtk::gdk::Key;
            let mut s = p.scene.borrow_mut();
            match key {
                Key::Return | Key::KP_Enter => {
                    let selected = s.view().and_then(|v| v.selected.clone());
                    drop(s);
                    if let Some(key) = selected {
                        p.activate(key);
                    }
                }
                Key::Left | Key::Right | Key::Up | Key::Down => {
                    let nodes: Vec<_> = s.graph.nodes().into_iter().cloned().collect();
                    if nodes.is_empty() {
                        return glib::Propagation::Stop;
                    }
                    let selected = s.view().and_then(|v| v.selected.clone());
                    let current = nodes.iter().position(|k| Some(k) == selected.as_ref());
                    let index = match current {
                        None => 0,
                        Some(i) if key == Key::Left || key == Key::Up => {
                            (i + nodes.len() - 1) % nodes.len()
                        }
                        Some(i) => (i + 1) % nodes.len(),
                    };
                    if let Some(node) = s.nodes.get(&nodes[index]) {
                        p.status.set_text(&node.detail);
                    }
                    if let Some(v) = s.view() {
                        v.selected = Some(nodes[index].clone());
                    }
                    drop(s);
                    p.area.queue_draw();
                }
                Key::Escape => {
                    let id = s.workspace;
                    drop(s);
                    if let Some(id) = id {
                        p.send(FlowAction::Toggle(id));
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.area.add_controller(keys);
    }
}

fn draw(area: &gtk::DrawingArea, cr: &gtk::cairo::Context, width: i32, height: i32, scene: &Scene) {
    cr.set_source_rgb(0.025, 0.035, 0.08);
    let _ = cr.paint();
    for i in 0..150u32 {
        let x = ((i.wrapping_mul(2654435761) % 10000) as f64 / 10000.0) * width as f64;
        let y = ((i.wrapping_mul(2246822519) % 10000) as f64 / 10000.0) * height as f64;
        cr.set_source_rgba(0.65, 0.76, 1.0, 0.12 + (i % 3) as f64 * 0.06);
        cr.arc(
            x,
            y,
            if i % 7 == 0 { 1.2 } else { 0.65 },
            0.0,
            std::f64::consts::TAU,
        );
        let _ = cr.fill();
    }
    let camera = scene.camera();
    cr.set_line_width(1.2);
    for (parent, child) in scene.graph.edges() {
        let (Some(p), Some(c)) = (
            scene.layout.positions.get(parent),
            scene.layout.positions.get(child),
        ) else {
            continue;
        };
        let (p, c) = (camera.screen(*p), camera.screen(*c));
        cr.set_source_rgba(0.39, 0.51, 0.78, 0.55);
        cr.move_to(p.x, p.y);
        cr.line_to(c.x, c.y);
        let _ = cr.stroke();
        let angle = (c.y - p.y).atan2(c.x - p.x);
        let tip = Point {
            x: c.x - angle.cos() * 14.0,
            y: c.y - angle.sin() * 14.0,
        };
        cr.move_to(tip.x, tip.y);
        cr.line_to(
            tip.x - (angle - 0.45).cos() * 7.0,
            tip.y - (angle - 0.45).sin() * 7.0,
        );
        cr.line_to(
            tip.x - (angle + 0.45).cos() * 7.0,
            tip.y - (angle + 0.45).sin() * 7.0,
        );
        cr.close_path();
        let _ = cr.fill();
    }
    let selected = scene
        .workspace
        .and_then(|id| scene.views.get(&id))
        .and_then(|v| v.selected.as_ref());
    for key in scene.graph.nodes() {
        let Some(point) = scene.layout.positions.get(key) else {
            continue;
        };
        let p = camera.screen(*point);
        if p.x < -140.0 || p.y < -50.0 || p.x > width as f64 + 140.0 || p.y > height as f64 + 50.0 {
            continue;
        }
        let node = scene.nodes.get(key);
        let available = node.is_some_and(|n| n.available);
        let anchor = scene.anchors.contains(key);
        let focused = selected == Some(key);
        let color = if !available {
            (0.40, 0.43, 0.53)
        } else if anchor {
            (0.43, 0.86, 0.94)
        } else {
            (0.65, 0.57, 0.95)
        };
        if anchor || focused {
            cr.set_source_rgba(color.0, color.1, color.2, 0.12);
            cr.arc(p.x, p.y, 20.0, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
        cr.set_source_rgb(color.0, color.1, color.2);
        cr.arc(
            p.x,
            p.y,
            if anchor { 8.0 } else { 6.0 },
            0.0,
            std::f64::consts::TAU,
        );
        let _ = cr.fill();
        if focused {
            cr.set_line_width(1.5);
            cr.arc(p.x, p.y, 14.0, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
        }
        let title = node.map(|n| n.title.as_str()).unwrap_or(&key.id);
        let label = if !available {
            format!("{title} · unavailable")
        } else if node.is_some_and(|n| n.running) {
            format!("{title} · open")
        } else {
            title.to_string()
        };
        let layout = area.create_pango_layout(Some(&label));
        layout.set_width(170 * gtk::pango::SCALE);
        layout.set_ellipsize(gtk::pango::EllipsizeMode::End);
        layout.set_alignment(gtk::pango::Alignment::Center);
        layout.set_font_description(Some(&gtk::pango::FontDescription::from_string("Sans 9")));
        cr.set_source_rgb(
            if available { 0.83 } else { 0.55 },
            if available { 0.86 } else { 0.57 },
            if available { 0.94 } else { 0.64 },
        );
        cr.move_to(p.x - 85.0, p.y + 17.0);
        pangocairo::functions::show_layout(cr, &layout);
    }
}

#[cfg(all(target_os = "macos", feature = "native-smoke"))]
impl FlowPanel {
    pub(crate) fn check_native_interactions(
        &self,
        available: &SessionKey,
        unavailable: &SessionKey,
    ) {
        let mut scene = self.scene.borrow_mut();
        assert!(!scene.nodes[unavailable].available);
        let cursor = scene.camera().screen(scene.layout.positions[available]);
        assert_eq!(scene.hit(cursor).as_ref(), Some(available));
        let world = scene.camera().world(cursor);
        scene.view().unwrap().camera.zoom_at(cursor, 1.5);
        let after = scene.camera().world(cursor);
        assert!((world.x - after.x).abs() < 0.0001 && (world.y - after.y).abs() < 0.0001);
        let point = Point { x: 100.0, y: 200.0 };
        scene.layout.drag(available, point);
        assert_eq!(scene.layout.positions[available], point);
        scene.fit(self.area.width() as f64, self.area.height() as f64);
        let selected = scene.view().unwrap().selected.clone();
        drop(scene);
        self.activate(unavailable.clone());
        assert_eq!(self.scene.borrow_mut().view().unwrap().selected, selected);
    }
}
