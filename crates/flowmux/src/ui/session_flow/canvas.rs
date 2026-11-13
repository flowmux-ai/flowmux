// SPDX-License-Identifier: GPL-3.0-or-later
//! Render the cosmic graph with retained GSK resources instead of a full-frame Cairo upload.

use super::*;
use gtk::subclass::prelude::*;
use std::cell::OnceCell;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Canvas {
        pub(super) scene: OnceCell<Rc<RefCell<Scene>>>,
        pub(super) empty_draw: Cell<bool>,
        sprites: RefCell<Option<(i32, [gtk::gdk::Texture; 3])>>,
        arrow: RefCell<Option<(i32, gtk::gdk::Texture)>>,
        #[cfg(all(test, target_os = "macos", feature = "native-smoke"))]
        pub(super) snapshot_times: RefCell<Option<Rc<RefCell<Vec<f64>>>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Canvas {
        const NAME: &'static str = "FlowmuxSessionFlowCanvas";
        type Type = super::Canvas;
        type ParentType = gtk::DrawingArea;
    }

    impl ObjectImpl for Canvas {}
    impl DrawingAreaImpl for Canvas {}

    impl WidgetImpl for Canvas {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            #[cfg(all(test, target_os = "macos", feature = "native-smoke"))]
            let started = std::time::Instant::now();
            let area = self.obj();
            let (width, height) = (area.width(), area.height());
            if width <= 0 || height <= 0 {
                return;
            }
            let bounds = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
            snapshot.push_clip(&bounds);
            if self.empty_draw.get() {
                snapshot.append_color(&gtk::gdk::RGBA::new(0.02, 0.03, 0.08, 1.0), &bounds);
            } else {
                let mut scene = self.scene.get().unwrap().borrow_mut();
                if !scene.graph.is_empty() && scene.view().is_some_and(|v| !v.initialized) {
                    scene.fit(width as f64, height as f64);
                    scene.fit_during_settle = true;
                }
                let camera = scene.camera();
                let color = gtk::gdk::RGBA::new(0.39, 0.51, 0.78, 0.55);
                let scale = area.scale_factor().max(1);
                let mut arrow = self.arrow.borrow_mut();
                if arrow.as_ref().map(|(old_scale, _)| *old_scale) != Some(scale) {
                    let mut surface = gtk::cairo::ImageSurface::create(
                        gtk::cairo::Format::ARgb32,
                        8 * scale,
                        8 * scale,
                    )
                    .unwrap();
                    {
                        let cr = gtk::cairo::Context::new(&surface).unwrap();
                        cr.scale(scale as f64, scale as f64);
                        cr.translate(7.0, 4.0);
                        cr.set_source_rgba(0.39, 0.51, 0.78, 0.55);
                        cr.move_to(0.0, 0.0);
                        cr.line_to(-0.45_f64.cos() * 7.0, -0.45_f64.sin() * 7.0);
                        cr.line_to(-0.45_f64.cos() * 7.0, 0.45_f64.sin() * 7.0);
                        cr.close_path();
                        let _ = cr.fill();
                    }
                    let stride = surface.stride() as usize;
                    let bytes = glib::Bytes::from_owned(surface.data().unwrap().to_vec());
                    #[cfg(target_endian = "little")]
                    let format = gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied;
                    #[cfg(target_endian = "big")]
                    let format = gtk::gdk::MemoryFormat::A8r8g8b8Premultiplied;
                    let texture =
                        gtk::gdk::MemoryTexture::new(8 * scale, 8 * scale, format, &bytes, stride);
                    *arrow = Some((scale, texture.upcast()));
                }
                let arrow = &arrow.as_ref().unwrap().1;
                for (parent, child) in scene.graph.edges() {
                    let (Some(p), Some(c)) = (
                        scene.layout.positions.get(parent),
                        scene.layout.positions.get(child),
                    ) else {
                        continue;
                    };
                    let (p, c) = (camera.screen(*p), camera.screen(*c));
                    let (x, y) = (p.x as f32, p.y as f32);
                    let length = (c.x - p.x).hypot(c.y - p.y) as f32;
                    let angle = (c.y - p.y).atan2(c.x - p.x).to_degrees() as f32;
                    // Persisted coordinates are f64; GSK geometry is f32.
                    if ![x, y, length]
                        .into_iter()
                        .all(|v| v.is_finite() && v.abs() <= f32::MAX / 4.0)
                        || !angle.is_finite()
                        || length <= 0.0
                    {
                        continue;
                    }
                    snapshot.save();
                    snapshot.translate(&gtk::graphene::Point::new(x, y));
                    snapshot.rotate(angle);
                    snapshot
                        .append_color(&color, &gtk::graphene::Rect::new(0.0, -0.6, length, 1.2));
                    snapshot.translate(&gtk::graphene::Point::new(length - 14.0, 0.0));
                    snapshot.append_texture(arrow, &gtk::graphene::Rect::new(-7.0, -4.0, 8.0, 8.0));
                    snapshot.restore();
                }
                let mut sprites = self.sprites.borrow_mut();
                if sprites.as_ref().map(|(old_scale, _)| *old_scale) != Some(scale) {
                    *sprites = Some((
                        scale,
                        [
                            star_texture(scale, (0.43, 0.86, 0.94), 8.0, true),
                            star_texture(scale, (0.65, 0.57, 0.95), 6.0, true),
                            star_texture(scale, (0.40, 0.43, 0.53), 6.0, false),
                        ],
                    ));
                }
                draw_nodes(
                    snapshot,
                    area.upcast_ref(),
                    width,
                    height,
                    &scene,
                    &sprites.as_ref().unwrap().1,
                );
            }
            snapshot.pop();
            #[cfg(all(test, target_os = "macos", feature = "native-smoke"))]
            if let Some(times) = self.snapshot_times.borrow().as_ref() {
                times
                    .borrow_mut()
                    .push(started.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
}

// Three tiny HiDPI sprites are uploaded once per scale factor. Node movement
// only places existing textures; gradients and glow never run per-node per-frame.
fn star_texture(
    scale: i32,
    rgb: (f64, f64, f64),
    radius: f64,
    luminous: bool,
) -> gtk::gdk::Texture {
    let size = (radius * 2.6).ceil() as i32 * 2;
    let mut surface =
        gtk::cairo::ImageSurface::create(gtk::cairo::Format::ARgb32, size * scale, size * scale)
            .unwrap();
    {
        let cr = gtk::cairo::Context::new(&surface).unwrap();
        cr.scale(scale as f64, scale as f64);
        cr.translate(size as f64 / 2.0, size as f64 / 2.0);
        let glow = gtk::cairo::RadialGradient::new(0.0, 0.0, radius * 0.4, 0.0, 0.0, radius * 2.6);
        glow.add_color_stop_rgba(0.0, rgb.0, rgb.1, rgb.2, if luminous { 0.48 } else { 0.15 });
        glow.add_color_stop_rgba(
            0.45,
            rgb.0,
            rgb.1,
            rgb.2,
            if luminous { 0.15 } else { 0.05 },
        );
        glow.add_color_stop_rgba(1.0, rgb.0, rgb.1, rgb.2, 0.0);
        let _ = cr.set_source(&glow);
        cr.arc(0.0, 0.0, radius * 2.6, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
        if luminous {
            let _ = cr.save();
            let ray = gtk::cairo::LinearGradient::new(-radius * 1.8, 0.0, radius * 1.8, 0.0);
            ray.add_color_stop_rgba(0.0, rgb.0, rgb.1, rgb.2, 0.0);
            ray.add_color_stop_rgba(0.5, rgb.0, rgb.1, rgb.2, 0.6);
            ray.add_color_stop_rgba(1.0, rgb.0, rgb.1, rgb.2, 0.0);
            for _ in 0..2 {
                let _ = cr.set_source(&ray);
                cr.rectangle(-radius * 1.8, -0.5, radius * 3.6, 1.0);
                let _ = cr.fill();
                cr.rotate(std::f64::consts::FRAC_PI_2);
            }
            let _ = cr.restore();
        }
        let core =
            gtk::cairo::RadialGradient::new(-radius * 0.2, -radius * 0.25, 0.0, 0.0, 0.0, radius);
        let white = if luminous { 0.82 } else { 0.25 };
        core.add_color_stop_rgb(
            0.0,
            rgb.0 + (1.0 - rgb.0) * white,
            rgb.1 + (1.0 - rgb.1) * white,
            rgb.2 + (1.0 - rgb.2) * white,
        );
        core.add_color_stop_rgb(0.42, rgb.0, rgb.1, rgb.2);
        core.add_color_stop_rgba(0.85, rgb.0 * 0.65, rgb.1 * 0.65, rgb.2 * 0.65, 0.9);
        core.add_color_stop_rgba(1.0, rgb.0, rgb.1, rgb.2, 0.0);
        let _ = cr.set_source(&core);
        cr.arc(0.0, 0.0, radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
    let stride = surface.stride() as usize;
    let bytes = glib::Bytes::from_owned(surface.data().unwrap().to_vec());
    #[cfg(target_endian = "little")]
    let format = gtk::gdk::MemoryFormat::B8g8r8a8Premultiplied;
    #[cfg(target_endian = "big")]
    let format = gtk::gdk::MemoryFormat::A8r8g8b8Premultiplied;
    gtk::gdk::MemoryTexture::new(size * scale, size * scale, format, &bytes, stride).upcast()
}

fn draw_nodes(
    snapshot: &gtk::Snapshot,
    area: &gtk::DrawingArea,
    width: i32,
    height: i32,
    scene: &Scene,
    sprites: &[gtk::gdk::Texture; 3],
) {
    let camera = scene.camera();
    let selected = scene
        .workspace
        .and_then(|id| scene.views.get(&id))
        .and_then(|v| v.selected.as_ref());
    let sizes = sprites
        .each_ref()
        .map(|texture| texture.width() as f32 / area.scale_factor().max(1) as f32);
    let keys: Vec<_> = scene.graph.nodes().into_iter().collect();
    for &key in &keys {
        let Some(point) = scene.layout.positions.get(key) else {
            continue;
        };
        let p = camera.screen(*point);
        if !p.finite()
            || p.x < -140.0
            || p.y < -50.0
            || p.x > width as f64 + 140.0
            || p.y > height as f64 + 50.0
        {
            continue;
        }
        let available = scene.nodes.get(key).is_some_and(|n| n.available);
        let anchor = scene.anchors.contains(key);
        let focused = selected == Some(key);
        let rgb = if !available {
            (0.40, 0.43, 0.53)
        } else if anchor {
            (0.43, 0.86, 0.94)
        } else {
            (0.65, 0.57, 0.95)
        };
        let color = gtk::gdk::RGBA::new(rgb.0, rgb.1, rgb.2, 1.0);
        let (x, y) = (p.x as f32, p.y as f32);
        let sprite = if !available {
            2
        } else if anchor {
            0
        } else {
            1
        };
        let size = sizes[sprite];
        snapshot.append_texture(
            &sprites[sprite],
            &gtk::graphene::Rect::new(x - size / 2.0, y - size / 2.0, size, size),
        );
        let stroke = if available {
            color
        } else {
            gtk::gdk::RGBA::new(0.82, 0.84, 0.90, 1.0)
        };
        if !available {
            snapshot.save();
            snapshot.translate(&gtk::graphene::Point::new(x - 5.0, y + 5.0));
            snapshot.rotate(-45.0);
            snapshot.append_color(
                &stroke,
                &gtk::graphene::Rect::new(0.0, -0.75, 10.0 * 2.0_f32.sqrt(), 1.5),
            );
            snapshot.restore();
        }
        if focused {
            let rect = gtk::graphene::Rect::new(x - 14.75, y - 14.75, 29.5, 29.5);
            snapshot.append_border(
                &gtk::gsk::RoundedRect::from_rect(rect, 14.75),
                &[1.5; 4],
                &[stroke; 4],
            );
        }
    }
    for label in layout_labels(area, scene, width, height, keys) {
        let available = scene.nodes.get(label.key).is_some_and(|n| n.available);
        let color = if available {
            gtk::gdk::RGBA::new(0.83, 0.86, 0.94, 1.0)
        } else {
            gtk::gdk::RGBA::new(0.55, 0.57, 0.64, 1.0)
        };
        snapshot.save();
        snapshot.translate(&gtk::graphene::Point::new(
            (label.bounds.x() + 2) as f32,
            (label.bounds.y() + 2) as f32,
        ));
        snapshot.append_layout(&label.layout, &color);
        snapshot.restore();
    }
}

glib::wrapper! {
    pub struct Canvas(ObjectSubclass<imp::Canvas>)
        @extends gtk::DrawingArea, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Canvas {
    pub(super) fn new(scene: Rc<RefCell<Scene>>) -> Self {
        let area: Self = glib::Object::new();
        assert!(area.imp().scene.set(scene).is_ok());
        area
    }

    #[cfg(all(test, target_os = "macos", feature = "native-smoke"))]
    pub(super) fn set_empty_draw(&self, empty: bool) {
        self.imp().empty_draw.set(empty);
    }

    #[cfg(all(test, target_os = "macos", feature = "native-smoke"))]
    pub(super) fn set_snapshot_times(&self, times: Rc<RefCell<Vec<f64>>>) {
        *self.imp().snapshot_times.borrow_mut() = Some(times);
    }
}

#[cfg(test)]
pub(super) fn check_snapshot(area: &gtk::DrawingArea) {
    let canvas = area.clone().downcast::<Canvas>().unwrap();
    area.allocate(800, 260, -1, None);
    let render = || {
        let snapshot = gtk::Snapshot::new();
        canvas.imp().snapshot(&snapshot);
        let Some(node) = snapshot.to_node() else {
            return false;
        };
        let bounds = node.bounds();
        assert!(bounds.x().is_finite() && bounds.y().is_finite());
        assert!(bounds.width().is_finite() && bounds.height().is_finite());
        assert!(bounds.x() >= 0.0 && bounds.y() >= 0.0);
        assert!(bounds.x() + bounds.width() <= 800.0);
        assert!(bounds.y() + bounds.height() <= 260.0);
        true
    };
    assert!(render());
    let scene = canvas.imp().scene.get().unwrap();
    let positions = scene.borrow().layout.positions.clone();
    // Finite saved f64 positions may overflow GSK's f32 coordinates.
    for coordinate in [f64::MAX, f32::MAX as f64 * 0.75] {
        for (index, position) in scene.borrow_mut().layout.positions.values_mut().enumerate() {
            *position = Point {
                x: coordinate,
                y: if index % 2 == 0 { 0.0 } else { coordinate },
            };
        }
        render();
    }
    scene.borrow_mut().layout.positions = positions;
    render();
}
