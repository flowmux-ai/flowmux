// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use gtk::glib::translate::ToGlibPtr;
use objc2::{MainThreadMarker, Message};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSWindow};
use std::time::Duration;

pub(crate) async fn check_managed() {
    let layout = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let pane_widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
    pane_widget.set_hexpand(true);
    pane_widget.set_vexpand(true);
    let tab = gtk::Label::new(Some("Destination tab")).upcast::<gtk::Widget>();
    let body = gtk::Label::new(Some("Destination pane")).upcast::<gtk::Widget>();
    body.set_vexpand(true);
    pane_widget.append(&tab);
    pane_widget.append(&body);
    layout.append(&pane_widget);
    let pane = PaneId::new();
    let surface = SurfaceId::new();
    let mut registry = PaneRegistry::default();
    registry.pane_frames.insert(pane, pane_widget.upcast());
    registry
        .surface_tabs
        .insert(pane, vec![(surface, tab.clone())]);
    let registry = Rc::new(RefCell::new(registry));
    let window = gtk::Window::builder()
        .title("Managed Agent drag smoke")
        .default_width(720)
        .default_height(480)
        .child(&layout)
        .build();
    activate_window(&window);
    super::super::managed_agents::check_native_drag(&registry, pane, (surface, &tab), &body).await;
    window.destroy();
}

fn activate_window(window: &gtk::Window) -> objc2::rc::Retained<NSWindow> {
    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    static LAUNCH: std::sync::Once = std::sync::Once::new();
    LAUNCH.call_once(|| {
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        app.finishLaunching();
    });
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    window.present();
    let surface = window.surface().expect("mapped drag smoke window");
    // SAFETY: a mapped GTK macOS surface owns this NSWindow.
    let native = unsafe {
        gdk_macos_surface_get_native_window(surface.to_glib_none().0)
            .cast::<NSWindow>()
            .as_ref()
            .expect("native drag smoke window")
            .retain()
    };
    native.makeKeyAndOrderFront(None);
    native
}

pub(crate) async fn check(
    window: &gtk::Window,
    registry: &Rc<RefCell<PaneRegistry>>,
    source: PaneId,
    target: PaneId,
) {
    let native = activate_window(window);

    let root = window.upcast_ref::<gtk::Widget>();
    let (tab, target_tab, frame, preview, zone) = {
        let r = registry.borrow();
        let (preview, zone) = &r.pane_drop_previews[&target];
        (
            r.surface_tabs[&source][0].1.clone(),
            r.surface_tabs[&target][0].1.clone(),
            r.pane_frames[&target].clone(),
            preview.clone(),
            zone.clone(),
        )
    };
    let controllers = tab.observe_controllers();
    let drag = (0..controllers.n_items())
        .find_map(|i| controllers.item(i)?.downcast::<gtk::GestureDrag>().ok())
        .expect("macOS tab drag gesture");
    gtk::glib::future_with_timeout(Duration::from_secs(5), async {
        loop {
            let bounds = frame.compute_bounds(root).unwrap();
            let x = f64::from(bounds.x() + bounds.width() * 0.85);
            let y = f64::from(bounds.y() + bounds.height() * 0.4);
            if native.isKeyWindow()
                && macos_tab_drag_point(root, x, y).is_some_and(|(target, _, _)| target == *root)
            {
                break;
            }
            gtk::glib::timeout_future(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("drag preview target must be the frontmost native window at the test point");
    let origin = tab
        .compute_point(root, &gtk::graphene::Point::new(0.0, 0.0))
        .unwrap();
    let motion = |x: f32, y: f32| {
        drag.emit_by_name::<()>(
            "drag-update",
            &[&f64::from(x - origin.x()), &f64::from(y - origin.y())],
        );
    };
    let bounds = frame.compute_bounds(root).unwrap();
    drag.emit_by_name::<()>("drag-begin", &[&0_f64, &0_f64]);
    drag.emit_by_name::<()>("drag-update", &[&2_f64, &2_f64]);
    assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    for (fx, fy, expected) in [
        (0.85, 0.4, PaneDropZone::SplitRight),
        (0.3, 0.85, PaneDropZone::SplitDown),
        (0.25, 0.3, PaneDropZone::Tab),
    ] {
        motion(
            bounds.x() + bounds.width() * fx,
            bounds.y() + bounds.height() * fy,
        );
        assert_eq!(zone.get(), expected);
        assert!(crate::ui::browser_pane::native_browser_views_are_suspended());
        // The preview is mapped before its first frame; wait for something to paint
        // instead of a fixed delay a slow runner can outlast.
        let node = gtk::glib::future_with_timeout(Duration::from_secs(10), async {
            loop {
                gtk::glib::timeout_future(Duration::from_millis(50)).await;
                if !preview.is_mapped() || preview.width() <= 0 {
                    continue;
                }
                let snapshot = gtk::Snapshot::new();
                gtk::WidgetPaintable::new(Some(&preview)).snapshot(
                    &snapshot,
                    preview.width() as f64,
                    preview.height() as f64,
                );
                if let Some(node) = snapshot.to_node() {
                    break node;
                }
            }
        })
        .await
        .expect("visible drop preview");
        let texture = window.renderer().unwrap().render_texture(node, None);
        let mut bytes = vec![0; texture.width() as usize * texture.height() as usize * 4];
        texture.download(&mut bytes, texture.width() as usize * 4);
        assert!(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .any(|p| p[3] > 0 && p[3] < 255),
            "preview must paint translucent pixels"
        );
        if let Some(directory) = std::env::var_os("FLOWMUX_BROWSER_UI_SNAPSHOTS") {
            std::fs::create_dir_all(&directory).unwrap();
            let snapshot = gtk::Snapshot::new();
            gtk::WidgetPaintable::new(Some(window)).snapshot(
                &snapshot,
                window.width() as f64,
                window.height() as f64,
            );
            window
                .renderer()
                .unwrap()
                .render_texture(snapshot.to_node().unwrap(), None)
                .save_to_png(
                    std::path::Path::new(&directory).join(format!("drag-{expected:?}.png")),
                )
                .unwrap();
        }
    }
    let tab_bounds = target_tab.compute_bounds(root).unwrap();
    for (fraction, class) in [
        (0.25, "flowmux-pane-tab-drop-before"),
        (0.75, "flowmux-pane-tab-drop-after"),
    ] {
        motion(
            tab_bounds.x() + tab_bounds.width() * fraction,
            tab_bounds.y() + tab_bounds.height() / 2.0,
        );
        assert_eq!(zone.get(), PaneDropZone::None);
        assert!(target_tab.has_css_class(class));
    }
    motion(-10.0, -10.0);
    assert!(!target_tab.has_css_class("flowmux-pane-tab-drop-after"));
    motion(
        bounds.x() + bounds.width() * 0.85,
        bounds.y() + bounds.height() * 0.4,
    );
    drag.emit_by_name::<()>("cancel", &[&None::<gtk::gdk::EventSequence>]);
    assert_eq!(zone.get(), PaneDropZone::None);
    assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    drag.emit_by_name::<()>("drag-begin", &[&0_f64, &0_f64]);
    motion(
        bounds.x() + bounds.width() * 0.85,
        bounds.y() + bounds.height() * 0.4,
    );
    drag.emit_by_name::<()>("drag-end", &[&0_f64, &0_f64]);
    assert_eq!(zone.get(), PaneDropZone::None);
    assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    println!("MACOS_TAB_DRAG_PREVIEW_OK");
}
