// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::ui::browser_pane::{suspend_native_browser_views_for_window, NativeBrowserViewsSuspend};
use crate::ui::workspace_view::macos_tab_drag_point;
use std::rc::Weak;

struct Drag {
    start: (f64, f64),
    key: Option<gtk::EventControllerKey>,
    browser_views: Option<NativeBrowserViewsSuspend>,
}

impl Drop for Drag {
    fn drop(&mut self) {
        if let Some(key) = &self.key {
            if let Some(window) = key.widget() {
                window.remove_controller(key);
            }
        }
    }
}

pub(super) fn install(
    widget: &gtk::Widget,
    registry: Weak<RefCell<PaneRegistry>>,
    bridge: Bridge,
    payload: ManagedAgentDndPayload,
) {
    // AppKit's native drag can claim the sequence without delivering drops over WKWebView.
    let gesture = gtk::GestureDrag::new();
    gesture.set_button(gtk::gdk::BUTTON_PRIMARY);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let drag = Rc::new(RefCell::new(None::<Drag>));
    let for_begin = drag.clone();
    gesture.connect_drag_begin(move |_, x, y| {
        for_begin.replace(Some(Drag {
            start: (x, y),
            key: None,
            browser_views: None,
        }));
    });
    let for_motion = drag.clone();
    let registry_for_motion = registry.clone();
    gesture.connect_drag_update(move |gesture, dx, dy| {
        let Some(widget) = gesture.widget() else {
            return;
        };
        let Some(window) = widget.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let mut state = for_motion.borrow_mut();
        let Some(active) = state.as_mut() else { return };
        let starting = active.browser_views.is_none();
        if starting {
            if !widget.drag_check_threshold(0, 0, dx as i32, dy as i32) {
                return;
            }
            let key = gtk::EventControllerKey::new();
            key.set_propagation_phase(gtk::PropagationPhase::Capture);
            let gesture = gesture.downgrade();
            let registry = registry_for_motion.clone();
            let drag = Rc::downgrade(&for_motion);
            key.connect_key_pressed(move |_, key, _, _| {
                if key != gtk::gdk::Key::Escape {
                    return gtk::glib::Propagation::Proceed;
                }
                if let Some(drag) = drag.upgrade() {
                    drag.borrow_mut().take();
                }
                if let Some(registry) = registry.upgrade() {
                    registry.borrow().update_tab_drag_preview(None, None);
                }
                if let Some(gesture) = gesture.upgrade() {
                    gesture.reset();
                }
                gtk::glib::Propagation::Stop
            });
            window.add_controller(key.clone());
            active.key = Some(key);
            active.browser_views = Some(suspend_native_browser_views_for_window(&window));
        }
        let point = target_point(&widget, active.start, dx, dy);
        drop(state);
        if let Some(registry) = registry_for_motion.upgrade() {
            registry
                .borrow()
                .update_tab_drag_preview(None, point.as_ref().map(|(root, x, y)| (root, *x, *y)));
        }
        if starting {
            gesture.set_state(gtk::EventSequenceState::Claimed);
        }
    });
    let for_end = drag.clone();
    let registry_for_end = registry.clone();
    gesture.connect_drag_end(move |gesture, dx, dy| {
        let Some(active) = for_end.borrow_mut().take() else {
            return;
        };
        if active.browser_views.is_none() {
            return;
        }
        let Some(registry) = registry_for_end.upgrade() else {
            return;
        };
        let registry = registry.borrow();
        registry.update_tab_drag_preview(None, None);
        let Some((root, x, y)) = gesture
            .widget()
            .and_then(|widget| target_point(&widget, active.start, dx, dy))
        else {
            return;
        };
        let target = registry
            .tab_at_root_point(&root, x, y)
            .map(|(pane, tab, _, after)| (pane, Some((tab, after))))
            .or_else(|| {
                registry
                    .pane_at_root_point(&root, x, y)
                    .map(|(pane, _, _)| (pane, None))
            });
        tracing::debug!(agent = %payload.instance, ?target, "managed Agent drag released");
        if let Some((pane, tab_anchor)) = target.filter(|(pane, _)| !registry.is_ssh_pane(*pane)) {
            let (ack, _) = tokio::sync::oneshot::channel();
            bridge.send_from_ui(GtkCommand::OpenManagedAgent {
                pane: Some(pane),
                tab_anchor,
                payload: payload.clone(),
                ack,
            });
        }
    });
    gesture.connect_cancel(move |_, _| {
        if drag.borrow_mut().take().is_some() {
            if let Some(registry) = registry.upgrade() {
                registry.borrow().update_tab_drag_preview(None, None);
            }
        }
    });
    widget.add_controller(gesture);
}

fn target_point(
    widget: &gtk::Widget,
    (x, y): (f64, f64),
    dx: f64,
    dy: f64,
) -> Option<(gtk::Widget, f64, f64)> {
    let root = widget
        .root()
        .and_downcast::<gtk::Window>()?
        .upcast::<gtk::Widget>();
    let point = widget.compute_point(
        &root,
        &gtk::graphene::Point::new((x + dx) as f32, (y + dy) as f32),
    )?;
    macos_tab_drag_point(&root, f64::from(point.x()), f64::from(point.y()))
}

#[cfg(test)]
pub(in crate::ui) async fn check_native(
    registry: &Rc<RefCell<PaneRegistry>>,
    pane: flowmux_core::PaneId,
    target_tab: (flowmux_core::SurfaceId, &gtk::Widget),
    target_body: &gtk::Widget,
) {
    use std::time::Duration;

    let (bridge, commands) = Bridge::new();
    let first = AgentInstance {
        instance: AgentInstanceId::new(),
        epoch: uuid::Uuid::new_v4(),
        provider: "claude".into(),
        label: "First".into(),
        cwd: "/tmp".into(),
        pid: Some(42),
        process_start: Some(1),
        boot_id: None,
        state: ProcessState::Running,
        force_terminate_allowed: false,
        exit_status: None,
        view: None,
        attachment: None,
        view_owner: None,
        error: None,
        presence: None,
        shell: false,
    };
    let second = AgentInstance {
        instance: AgentInstanceId::new(),
        label: "Second".into(),
        ..first.clone()
    };
    let snapshot = RegistrySnapshot {
        available: true,
        agents: vec![first, second.clone()],
        ..Default::default()
    };
    let root = target_body
        .root()
        .unwrap()
        .downcast::<gtk::Window>()
        .unwrap();
    let layout = root.child().unwrap().downcast::<gtk::Box>().unwrap();
    let pane_widget = target_body.parent().unwrap();
    let contents = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    layout.remove(&pane_widget);
    contents.append(&pane_widget);
    layout.append(&contents);
    let mut sidebar = ManagedAgents::new(gtk::Orientation::Vertical);
    let mut bar = ManagedAgents::new(gtk::Orientation::Horizontal);
    for agents in [&mut sidebar, &mut bar] {
        agents.set_drag_registry(registry);
        agents.render(&snapshot, &bridge, Default::default(), |_| None);
    }
    contents.prepend(&sidebar.root);
    layout.append(&bar.root);
    let root = root.upcast::<gtk::Widget>();
    let point = |widget: &gtk::Widget, fraction: f32| {
        let bounds = widget.compute_bounds(&root)?;
        (bounds.width() > 0.0 && bounds.height() > 0.0).then_some((
            bounds.x() + bounds.width() * fraction,
            bounds.y() + bounds.height() * 0.8,
        ))
    };
    // Drops resolve the topmost native window, so both drop points must be unobscured.
    let owned = |widget: &gtk::Widget, fraction: f32| {
        point(widget, fraction).is_some_and(|(x, y)| {
            macos_tab_drag_point(&root, f64::from(x), f64::from(y))
                .is_some_and(|(target, _, _)| target == root)
        })
    };
    gtk::glib::future_with_timeout(Duration::from_secs(5), async {
        loop {
            if owned(target_body, 0.8)
                && owned(target_tab.1, 0.25)
                && point(bar.root.upcast_ref(), 0.5).is_some()
            {
                break;
            }
            gtk::glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("managed Agent drag smoke must own its test window");

    let key = (second.instance, second.epoch);
    let button = bar.rows.borrow()[&key].button.clone();
    let source = |widget: &gtk::Widget| {
        let controllers = widget.observe_controllers();
        (0..controllers.n_items())
            .find_map(|i| controllers.item(i)?.downcast::<gtk::GestureDrag>().ok())
            .expect("the managed card must receive drag events")
    };
    let also_sidebar = sidebar.rows.borrow()[&key].button.clone();
    let gesture = source(button.upcast_ref());
    let start = |g: &gtk::GestureDrag| g.emit_by_name::<()>("drag-begin", &[&2.0f64, &2.0f64]);
    let send = |g: &gtk::GestureDrag, signal: &str, target: &gtk::Widget, fraction: f32| {
        let widget = g.widget().unwrap();
        let from = widget
            .compute_point(&root, &gtk::graphene::Point::new(2.0, 2.0))
            .unwrap();
        let to = point(target, fraction).unwrap();
        g.emit_by_name::<()>(
            signal,
            &[&f64::from(to.0 - from.x()), &f64::from(to.1 - from.y())],
        );
    };
    start(&gesture);
    gesture.emit_by_name::<()>("drag-update", &[&1.0_f64, &1.0_f64]);
    gesture.emit_by_name::<()>("drag-end", &[&1.0_f64, &1.0_f64]);
    assert!(commands.try_recv().is_err());
    start(&gesture);
    send(&gesture, "drag-update", target_tab.1, 0.25);
    assert!(target_tab.1.has_css_class("flowmux-pane-tab-drop-before"));
    gesture.emit_by_name::<()>("cancel", &[&None::<gtk::gdk::EventSequence>]);
    send(&gesture, "drag-end", target_tab.1, 0.25);
    assert!(!target_tab.1.has_css_class("flowmux-pane-tab-drop-before"));
    assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    assert!(commands.try_recv().is_err());

    start(&gesture);
    send(&gesture, "drag-update", target_tab.1, 0.75);
    assert!(target_tab.1.has_css_class("flowmux-pane-tab-drop-after"));
    let controllers = root.observe_controllers();
    let escape = (0..controllers.n_items())
        .find_map(|i| {
            controllers
                .item(i)?
                .downcast::<gtk::EventControllerKey>()
                .ok()
        })
        .expect("active drag must capture Escape");
    assert!(escape.emit_by_name::<bool>(
        "key-pressed",
        &[
            &gtk::gdk::Key::Escape,
            &0u32,
            &gtk::gdk::ModifierType::empty()
        ],
    ));
    send(&gesture, "drag-end", target_tab.1, 0.75);
    assert!(escape.widget().is_none());
    assert!(!target_tab.1.has_css_class("flowmux-pane-tab-drop-after"));
    assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    assert!(commands.try_recv().is_err());

    start(&gesture);
    send(&gesture, "drag-update", target_tab.1, 0.75);
    let wrong_target = sidebar.root.upcast_ref();
    send(&gesture, "drag-update", wrong_target, 0.25);
    assert!(!target_tab.1.has_css_class("flowmux-pane-tab-drop-after"));
    send(&gesture, "drag-end", wrong_target, 0.25);
    assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    assert!(commands.try_recv().is_err());

    for (g, widget, fraction, anchor) in [
        (gesture, target_body, 0.85, None),
        (
            source(button.upcast_ref()),
            target_tab.1,
            0.25,
            Some((target_tab.0, false)),
        ),
        (
            source(also_sidebar.upcast_ref()),
            target_tab.1,
            0.75,
            Some((target_tab.0, true)),
        ),
    ] {
        start(&g);
        send(&g, "drag-update", widget, fraction);
        send(&g, "drag-end", widget, fraction);
        let command = commands.try_recv().unwrap();
        assert!(
            matches!(command, GtkCommand::OpenManagedAgent { pane: Some(p), tab_anchor, payload, .. }
            if p == pane && tab_anchor == anchor && payload.instance == second.instance && payload.epoch == second.epoch)
        );
        assert!(
            commands.try_recv().is_err(),
            "a drag must dispatch exactly one move"
        );
        assert!(!target_tab.1.has_css_class("flowmux-pane-tab-drop-before"));
        assert!(!target_tab.1.has_css_class("flowmux-pane-tab-drop-after"));
        assert!(!crate::ui::browser_pane::native_browser_views_are_suspended());
    }
    button.emit_clicked();
    assert!(
        matches!(commands.try_recv().unwrap(), GtkCommand::OpenManagedAgent { pane: None, payload, .. }
        if payload.instance == second.instance)
    );
    println!("MACOS_MANAGED_AGENT_DRAG_OK");
}
