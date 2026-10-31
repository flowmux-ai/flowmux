// SPDX-License-Identifier: GPL-3.0-or-later
//! Independent sky layer: twinkling never invalidates the graph's retained snapshot.

use super::*;
use gtk::subclass::prelude::*;

const TWINKLING_STARS: u32 = 8;
const INTERVAL_US: i64 = 50_000;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Stars {
        pub(super) seed: Cell<u32>,
        pub(super) scene: std::cell::OnceCell<std::rc::Weak<RefCell<Scene>>>,
        pub(super) time: Cell<f64>,
        pub(super) tick: RefCell<Option<gtk::TickCallbackId>>,
        pub(super) last_tick: Cell<i64>,
        pub(super) window_handler:
            RefCell<Option<(glib::WeakRef<gtk::Window>, glib::SignalHandlerId)>>,
        pub(super) settings_handler: RefCell<Option<glib::SignalHandlerId>>,
        background: RefCell<Option<((i32, i32), gtk::gsk::RenderNode)>>,
        #[cfg(test)]
        pub(super) snapshots: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Stars {
        const NAME: &'static str = "FlowmuxSessionFlowStars";
        type Type = super::Stars;
        type ParentType = gtk::DrawingArea;
    }

    impl ObjectImpl for Stars {
        fn dispose(&self) {
            self.obj().stop();
            self.obj().disconnect_window();
            if let Some(handler) = self.settings_handler.borrow_mut().take() {
                self.obj().settings().disconnect(handler);
            }
        }
    }
    impl DrawingAreaImpl for Stars {}
    impl WidgetImpl for Stars {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let area = self.obj();
            let (width, height) = (area.width(), area.height());
            if width <= 0 || height <= 0 {
                return;
            }
            #[cfg(test)]
            self.snapshots.set(self.snapshots.get() + 1);
            let bounds = gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
            snapshot.push_clip(&bounds);
            let mut background = self.background.borrow_mut();
            if background.as_ref().map(|(size, _)| *size) != Some((width, height)) {
                let layer = gtk::Snapshot::new();
                {
                    let cr = layer.append_cairo(&bounds);
                    draw_background(&cr, width, height);
                }
                *background = layer.to_node().map(|node| ((width, height), node));
            }
            if let Some((_, node)) = background.as_ref() {
                snapshot.append_node(node);
            }
            for slot in 0..TWINKLING_STARS {
                let index = (self.seed.get() % 150 + slot * 37) % 150;
                let (x, y) = star_position(index, width, height);
                let pulse = pulse(index, self.time.get());
                // The luminous core's diameter grows by at most two logical pixels.
                let radius = 3.0 + 3.0 * pulse;
                let color = gtk::gdk::RGBA::new(0.80, 0.89, 1.0, 0.40 + 0.55 * pulse);
                let stops = [
                    gtk::gsk::ColorStop::new(0.0, color),
                    gtk::gsk::ColorStop::new(0.28, color),
                    gtk::gsk::ColorStop::new(0.55, gtk::gdk::RGBA::new(0.52, 0.70, 1.0, 0.12)),
                    gtk::gsk::ColorStop::new(1.0, gtk::gdk::RGBA::new(0.52, 0.70, 1.0, 0.0)),
                ];
                snapshot.append_radial_gradient(
                    &gtk::graphene::Rect::new(x - radius, y - radius, radius * 2.0, radius * 2.0),
                    &gtk::graphene::Point::new(x, y),
                    radius,
                    radius,
                    0.0,
                    1.0,
                    &stops,
                );
            }
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct Stars(ObjectSubclass<imp::Stars>)
        @extends gtk::DrawingArea, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Stars {
    pub(super) fn new(scene: &Rc<RefCell<Scene>>) -> Self {
        let area: Self = glib::Object::new();
        area.imp().seed.set(glib::random_int());
        assert!(area.imp().scene.set(Rc::downgrade(scene)).is_ok());
        area.set_can_target(false);
        area.set_hexpand(true);
        area.set_vexpand(true);
        area.connect_map(|area| {
            area.disconnect_window();
            if let Some(window) = area
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
            {
                let weak = area.downgrade();
                let handler = window.connect_is_active_notify(move |_| {
                    if let Some(area) = weak.upgrade() {
                        area.update_animation();
                    }
                });
                *area.imp().window_handler.borrow_mut() = Some((window.downgrade(), handler));
            }
            area.update_animation();
        });
        area.connect_unmap(|area| {
            area.stop();
            area.disconnect_window();
        });
        let weak = area.downgrade();
        let handler = area
            .settings()
            .connect_gtk_enable_animations_notify(move |_| {
                if let Some(area) = weak.upgrade() {
                    area.update_animation();
                }
            });
        *area.imp().settings_handler.borrow_mut() = Some(handler);
        area
    }

    fn stop(&self) {
        if let Some(timer) = self.imp().tick.borrow_mut().take() {
            timer.remove();
        }
    }

    fn disconnect_window(&self) {
        if let Some((window, handler)) = self.imp().window_handler.borrow_mut().take() {
            if let Some(window) = window.upgrade() {
                window.disconnect(handler);
            }
        }
    }

    fn update_animation(&self) {
        self.stop();
        if !self.is_mapped()
            || !self.settings().is_gtk_enable_animations()
            || self
                .root()
                .and_then(|root| root.downcast::<gtk::Window>().ok())
                .is_some_and(|window| !window.is_active())
        {
            return;
        }
        self.imp().last_tick.set(0);
        *self.imp().tick.borrow_mut() = Some(self.add_tick_callback(|area, clock| {
            let now = clock.frame_time();
            if now - area.imp().last_tick.get() < INTERVAL_US {
                return glib::ControlFlow::Continue;
            }
            area.imp().last_tick.set(now);
            if area
                .imp()
                .scene
                .get()
                .and_then(std::rc::Weak::upgrade)
                .is_some_and(|scene| {
                    let scene = scene.borrow();
                    scene.drag_active
                        || scene.last_zoom.is_some_and(|time| {
                            time.elapsed() < std::time::Duration::from_millis(200)
                        })
                })
            {
                return glib::ControlFlow::Continue;
            }
            area.imp()
                .time
                .set(area.imp().time.get() + INTERVAL_US as f64 / 1_000_000.0);
            area.queue_draw();
            glib::ControlFlow::Continue
        }));
    }
}

fn pulse(index: u32, time: f64) -> f32 {
    let period = 4.0 + (index % 7) as f64 * 0.6;
    let phase = index as f64 * 2.399_963;
    ((time * std::f64::consts::TAU / period + phase).sin() * 0.5 + 0.5) as f32
}

pub(super) fn star_position(index: u32, width: i32, height: i32) -> (f32, f32) {
    (
        (index.wrapping_mul(2654435761) % 10000) as f32 / 10000.0 * width as f32,
        (index.wrapping_mul(2246822519) % 10000) as f32 / 10000.0 * height as f32,
    )
}

#[cfg(all(test, target_os = "macos", feature = "native-smoke"))]
pub(crate) async fn check_native(panel: &FlowPanel) {
    use std::time::Duration;
    tests::check_pulses();
    // The native harness does not enter NSApplication's normal launch loop.
    let app =
        objc2_app_kit::NSApplication::sharedApplication(objc2::MainThreadMarker::new().unwrap());
    app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Regular);
    app.finishLaunching();
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    // No session polling/bridge updates: measure only the sky's invalidation path.
    let (bridge, _receiver) = Bridge::new();
    let probe = FlowPanel::new(&gtk::Stack::new(), bridge);
    probe.status.set_text(&panel.status.text());
    let workspace = {
        let scene = panel.scene.borrow();
        probe.set_nodes(
            scene.all.clone(),
            scene.nodes.clone(),
            &scene.layout.positions,
        );
        probe.show_workspace(scene.workspace, scene.graph.clone(), scene.anchors.clone());
        scene.workspace.unwrap()
    };
    probe.toggle(workspace);
    let window = gtk::Window::builder()
        .title("Session Flow sky isolation")
        .default_width(900)
        .default_height(360)
        .child(&probe.host)
        .build();
    window.present();
    glib::future_with_timeout(Duration::from_secs(5), async {
        while !window.is_active() {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("star fixture window must be active");
    let panel = &probe;
    let overlay = panel
        .area
        .parent()
        .unwrap()
        .downcast::<gtk::Overlay>()
        .unwrap();
    let stars = overlay.child().unwrap().downcast::<Stars>().unwrap();
    let settings = stars.settings();
    let animations = settings.is_gtk_enable_animations();
    settings.set_gtk_enable_animations(true);
    // Settle physics first, then prove sky updates do not redraw nodes/labels.
    {
        let mut scene = panel.scene.borrow_mut();
        let graph = scene.graph.clone();
        while scene.layout.step(&graph, None) {}
    }
    glib::timeout_future(Duration::from_millis(250)).await;
    let graph_draws = Rc::new(RefCell::new(Vec::new()));
    panel
        .area
        .clone()
        .downcast::<canvas::Canvas>()
        .unwrap()
        .set_snapshot_times(graph_draws.clone());
    let snapshots = stars.imp().snapshots.get();
    let time = stars.imp().time.get();
    glib::timeout_future(Duration::from_millis(350)).await;
    assert!(
        stars.imp().snapshots.get() >= snapshots + 3,
        "sky did not animate"
    );
    assert!(stars.imp().time.get() > time);
    assert!(
        graph_draws.borrow().is_empty(),
        "twinkling invalidated the static graph: {} graph snapshots / {} sky snapshots; physics active: {}",
        graph_draws.borrow().len(), stars.imp().snapshots.get() - snapshots, panel.animating.get()
    );
    panel.scene.borrow_mut().drag_active = true;
    let time = stars.imp().time.get();
    glib::timeout_future(Duration::from_millis(120)).await;
    assert_eq!(
        time,
        stars.imp().time.get(),
        "drag must take priority over twinkling"
    );
    panel.scene.borrow_mut().drag_active = false;
    panel.scene.borrow_mut().last_zoom = Some(std::time::Instant::now());
    glib::timeout_future(Duration::from_millis(120)).await;
    assert_eq!(
        time,
        stars.imp().time.get(),
        "zoom must take priority over twinkling"
    );
    panel.scene.borrow_mut().last_zoom = None;
    settings.set_gtk_enable_animations(false);
    assert!(
        stars.imp().tick.borrow().is_none(),
        "reduced motion left a timer running"
    );
    let time = stars.imp().time.get();
    glib::timeout_future(Duration::from_millis(120)).await;
    assert_eq!(time, stars.imp().time.get());
    settings.set_gtk_enable_animations(true);
    assert!(stars.imp().tick.borrow().is_some());
    if let Ok(path) = std::env::var("FLOWMUX_SESSION_FLOW_CAPTURE") {
        let clock = panel.root.frame_clock().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sender = RefCell::new(Some(tx));
        let root = panel.root.clone();
        let handler = clock.connect_after_paint(move |_| {
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(&root)).snapshot(
                &snapshot,
                root.width() as f64,
                root.height() as f64,
            );
            if let Some(node) = snapshot.to_node() {
                if let Some(tx) = sender.borrow_mut().take() {
                    let _ = tx.send(node);
                }
            }
        });
        panel.root.queue_draw();
        let rendered = glib::future_with_timeout(Duration::from_secs(3), rx).await;
        clock.disconnect(handler);
        let node = rendered
            .expect("star preview paint deadline")
            .expect("star preview renders");
        window
            .renderer()
            .unwrap()
            .render_texture(&node, None)
            .save_to_png(std::path::Path::new(&path).with_extension("stars.png"))
            .unwrap();
    }
    panel.root.set_visible(false);
    assert!(
        stars.imp().tick.borrow().is_none(),
        "hidden sky left a timer running"
    );
    panel.root.set_visible(true);
    assert!(stars.imp().tick.borrow().is_some());
    settings.set_gtk_enable_animations(animations);
    // Settings notification closures must not keep discarded layers alive.
    let orphan = Stars::new(&probe.scene);
    let weak = orphan.downgrade();
    drop(orphan);
    assert!(weak.upgrade().is_none());
    window.destroy();
    println!("MACOS_SESSION_FLOW_STARS_CACHE_MOTION_OK");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_twinkles_stay_bounded_and_have_distinct_phases() {
        check_pulses();
    }

    pub(super) fn check_pulses() {
        for seed in [0, 1, 149, u32::MAX] {
            let indices: BTreeSet<_> = (0..TWINKLING_STARS)
                .map(|slot| (seed % 150 + slot * 37) % 150)
                .collect();
            assert_eq!(indices.len(), 8);
            for index in indices {
                for tick in 0..100 {
                    let pulse = pulse(index, tick as f64 * 0.1);
                    assert!((0.0..=1.0).contains(&pulse));
                    let core_diameter_growth = 2.0 * 0.28 * 3.0 * pulse;
                    assert!(core_diameter_growth <= 2.0);
                }
            }
        }
        assert_ne!(pulse(1, 1.0), pulse(38, 1.0));
    }
}
