// SPDX-License-Identifier: GPL-3.0-or-later
//! Opt-in mapped GTK frame measurements; signals are not hardware input.

use super::*;
use std::time::{Duration, Instant};

pub(crate) async fn check_native_performance(path: &std::path::Path) {
    let (bridge, receiver) = Bridge::new();
    let stack = gtk::Stack::new();
    stack.add_named(
        &gtk::Label::new(Some("Isolated frame measurement")),
        Some("base"),
    );
    let panel = FlowPanel::new(&stack, bridge);
    let workspace = WorkspaceId::new();
    let key = |index: usize| SessionKey {
        provider: "fixture".into(),
        home: "/session-flow-performance".into(),
        id: index.to_string(),
    };
    let mut graph = Lineage::default();
    for family in 0..20 {
        for child in 1..50 {
            graph
                .link(key(family * 50 + child - 1), key(family * 50 + child))
                .unwrap();
        }
    }
    let nodes = graph
        .nodes()
        .into_iter()
        .map(|key| {
            (
                key.clone(),
                Node {
                    title: format!("세션 {} · experiment · 🪐", key.id),
                    detail: format!("Performance fixture {}", key.id),
                    available: true,
                    running: false,
                },
            )
        })
        .collect();
    panel.set_nodes(graph.clone(), nodes, &BTreeMap::new());
    panel.show_workspace(Some(workspace), graph.clone(), [key(0)].into());
    panel.toggle(workspace);
    // Measure warm interaction separately from force convergence and discovery.
    while panel.scene.borrow_mut().layout.step(&graph, None) {}
    let window = gtk::Window::builder()
        .title("Session Flow frame measurement")
        .default_width(1280)
        .default_height(900)
        .child(&panel.host)
        .build();
    window.present();
    glib::timeout_future(Duration::from_millis(500)).await;
    assert!(panel.area.is_mapped());
    panel.host.set_position(panel.host.height() / 3);
    glib::timeout_future(Duration::from_millis(100)).await;
    panel
        .scene
        .borrow_mut()
        .fit(panel.area.width() as f64, panel.area.height() as f64);
    panel.scene.borrow_mut().fit_during_settle = false;
    panel.scene.borrow_mut().pan_mode = true;
    let controllers = panel.area.observe_controllers();
    let drag = (0..controllers.n_items())
        .filter_map(|i| controllers.item(i)?.downcast::<gtk::GestureDrag>().ok())
        .find(|g| g.button() == 1)
        .unwrap();
    let scroll = (0..controllers.n_items())
        .find_map(|i| {
            controllers
                .item(i)?
                .downcast::<gtk::EventControllerScroll>()
                .ok()
        })
        .unwrap();
    let empty_draw = std::env::var_os("FLOWMUX_SESSION_FLOW_FRAME_EMPTY_DRAW").is_some();
    let draw_times = Rc::new(RefCell::new(Vec::<f64>::new()));
    let times = draw_times.clone();
    let scene = panel.scene.clone();
    panel.area.set_draw_func(move |area, cr, width, height| {
        let start = Instant::now();
        if empty_draw {
            cr.set_source_rgb(0.02, 0.03, 0.08);
            cr.paint().unwrap();
        } else {
            draw(area, cr, width, height, &scene.borrow());
        }
        times
            .borrow_mut()
            .push(start.elapsed().as_secs_f64() * 1000.0);
    });
    let clock = panel.area.frame_clock().unwrap();
    let last_paint = Rc::new(Cell::new(None::<Instant>));
    let pending_input = Rc::new(Cell::new(None::<Instant>));
    let intervals = Rc::new(RefCell::new(Vec::<f64>::new()));
    let latencies = Rc::new(RefCell::new(Vec::<f64>::new()));
    let (last, pending, frames, delays) = (
        last_paint.clone(),
        pending_input.clone(),
        intervals.clone(),
        latencies.clone(),
    );
    let handler = clock.connect_after_paint(move |_| {
        let now = Instant::now();
        if let Some(previous) = last.replace(Some(now)) {
            frames
                .borrow_mut()
                .push(now.duration_since(previous).as_secs_f64() * 1000.0);
        }
        if let Some(input) = pending.take() {
            delays
                .borrow_mut()
                .push(now.duration_since(input).as_secs_f64() * 1000.0);
        }
    });
    let mut trials = Vec::new();
    let positions = panel.positions();
    for trial in 0..5 {
        last_paint.set(None);
        intervals.borrow_mut().clear();
        latencies.borrow_mut().clear();
        draw_times.borrow_mut().clear();
        let camera = panel.scene.borrow().camera();
        let start = Instant::now();
        let mut inputs = 0;
        drag.emit_by_name::<()>("drag-begin", &[&40.0, &40.0]);
        while start.elapsed() < Duration::from_secs(10) {
            if pending_input.get().is_none() {
                pending_input.set(Some(Instant::now()));
            }
            let phase = start.elapsed().as_secs_f64();
            drag.emit_by_name::<()>(
                "drag-update",
                &[&(phase.sin() * 80.0), &(phase.cos() * 40.0)],
            );
            assert!(scroll.emit_by_name::<bool>("scroll", &[&0.0, &(phase.sin() * 0.03)]));
            inputs += 1;
            glib::timeout_future(Duration::from_millis(16)).await;
        }
        drag.emit_by_name::<()>("drag-end", &[&20.0, &20.0]);
        glib::timeout_future(Duration::from_millis(100)).await;
        assert!(panel.area.is_mapped());
        assert_ne!(
            panel.scene.borrow().camera(),
            camera,
            "input did not move the view"
        );
        assert_eq!(panel.positions(), positions, "pan/zoom moved nodes");
        assert!(
            intervals.borrow().len() >= 30,
            "too few mapped frames to measure"
        );
        trials.push(serde_json::json!({"trial":trial+1,"duration_seconds":start.elapsed().as_secs_f64(),"input_batches":inputs,
            "draw_cpu_ms":stats(&draw_times.borrow()),"after_paint_interval_ms":stats(&intervals.borrow()),"signal_to_after_paint_ms":stats(&latencies.borrow())}));
        while let Ok(command) = receiver.try_recv() {
            assert!(
                matches!(command, GtkCommand::SessionFlow(FlowAction::SaveView)),
                "pan/zoom requested a session action"
            );
        }
    }
    clock.disconnect(handler);
    let report = serde_json::json!({"nodes":1000,"families":20,"area_logical_size":[panel.area.width(),panel.area.height()],
        "diagnostic_empty_draw":empty_draw,"scale_factor":panel.area.scale_factor(),"renderer":std::env::var("GSK_RENDERER").ok(),"debug_assertions":cfg!(debug_assertions),
        "measurement":"Mapped GTK frame-clock after-paint intervals and controller-signal-to-after-paint delay. Not hardware input latency or compositor presentation time.",
        "trials":trials});
    std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    window.destroy();
    println!("MACOS_SESSION_FLOW_FRAME_MEASUREMENT_OK");
}

fn stats(values: &[f64]) -> serde_json::Value {
    assert!(!values.is_empty());
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    serde_json::json!({"samples":sorted.len(),"p50":sorted[(sorted.len()-1)/2],"p95":sorted[(sorted.len()*95).div_ceil(100)-1],"max":sorted.last()})
}
