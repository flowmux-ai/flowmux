// SPDX-License-Identifier: GPL-3.0-or-later
//! Run with `cargo run -p flowmux-core --release --example session_flow_benchmark --locked`.
//! Measures the model only: excludes provider discovery, GTK drawing and input delivery.

use flowmux_core::session_flow::{Layout, Lineage, SessionKey};
use std::hint::black_box;
use std::time::Instant;

fn summarize(samples: &mut [f64]) -> serde_json::Value {
    samples.sort_by(f64::total_cmp);
    let percentile = |percent: usize| samples[(samples.len() * percent).div_ceil(100) - 1];
    serde_json::json!({
        "samples": samples.len(),
        "p50_ms": percentile(50),
        "p95_ms": percentile(95),
        "max_ms": samples.last(),
    })
}

fn main() {
    let key = |family, node| SessionKey {
        provider: "fixture".into(),
        home: "/benchmark".into(),
        id: format!("{family:02}-{node:02}"),
    };
    let mut graph = Lineage::default();
    let mut anchors = Vec::new();
    for family in 0..20 {
        anchors.push(key(family, 25));
        for node in 1..50 {
            graph
                .link(key(family, node - 1), key(family, node))
                .unwrap();
        }
    }
    let mut preparation = Vec::new();
    let mut steps = Vec::new();
    let mut settling = Vec::new();
    for run in 0..32 {
        let started = Instant::now();
        let projected = black_box(&graph).project(black_box(anchors.iter()));
        let mut layout = Layout::default();
        layout.sync(&projected);
        let elapsed = started.elapsed().as_secs_f64() * 1000.0;
        if run >= 2 {
            preparation.push(elapsed);
        }
        let started = Instant::now();
        loop {
            let step_started = Instant::now();
            let active = layout.step(black_box(&projected), None);
            let elapsed = step_started.elapsed().as_secs_f64() * 1000.0;
            if run >= 2 {
                steps.push(elapsed);
            }
            if !active {
                break;
            }
        }
        black_box(&layout.positions);
        if run >= 2 {
            settling.push(started.elapsed().as_secs_f64() * 1000.0);
        }
    }
    println!(
        "{}",
        serde_json::json!({
            "scope": "model only; not UI frame or input latency",
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "debug_assertions": cfg!(debug_assertions),
            "nodes": 1000,
            "families": 20,
            "warmup_runs": 2,
            "measured_runs": 30,
            "projection_and_initial_layout": summarize(&mut preparation),
            "physics_step": summarize(&mut steps),
            "all_settling_steps_without_frame_wait": summarize(&mut settling),
        })
    );
}
