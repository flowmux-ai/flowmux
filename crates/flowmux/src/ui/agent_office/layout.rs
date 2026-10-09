// SPDX-License-Identifier: GPL-3.0-or-later
//! Capacity-aware floor plans, walkable routes, and a viewport-filling room map.

use flowmux_core::AgentStatus;
use std::collections::VecDeque;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

pub(super) fn tiles(weights: &[f64], bounds: Rect) -> Vec<Rect> {
    fn split(weights: &[f64], bounds: Rect, out: &mut Vec<Rect>) {
        if weights.is_empty() {
            return;
        }
        if weights.len() == 1 {
            out.push(bounds);
            return;
        }
        let total: f64 = weights.iter().sum();
        let mut cut = 1;
        let mut sum = weights[0];
        let mut partial = 0.0;
        for (index, weight) in weights.iter().take(weights.len() - 1).enumerate() {
            partial += weight;
            if (partial - total / 2.0).abs() < (sum - total / 2.0).abs() {
                cut = index + 1;
                sum = partial;
            }
        }
        let ratio = sum / total;
        let (first, second) = if bounds.w >= bounds.h {
            let w = bounds.w * ratio;
            (
                Rect { w, ..bounds },
                Rect {
                    x: bounds.x + w,
                    w: bounds.w - w,
                    ..bounds
                },
            )
        } else {
            let h = bounds.h * ratio;
            (
                Rect { h, ..bounds },
                Rect {
                    y: bounds.y + h,
                    h: bounds.h - h,
                    ..bounds
                },
            )
        };
        split(&weights[..cut], first, out);
        split(&weights[cut..], second, out);
    }
    let mut result = Vec::new();
    split(weights, bounds, &mut result);
    result
}

pub(super) struct Template {
    pub name: &'static str,
    work: &'static [Rect],
    rest: &'static [Rect],
}
const fn zone(x: f64, y: f64, w: f64, h: f64) -> Rect {
    Rect { x, y, w, h }
}

pub(super) const TEMPLATES: [Template; 24] = [
    Template {
        name: "West wing",
        work: &[zone(0., 0., 0.62, 1.)],
        rest: &[zone(0.68, 0., 0.32, 1.)],
    },
    Template {
        name: "East wing",
        work: &[zone(0.38, 0., 0.62, 1.)],
        rest: &[zone(0., 0., 0.32, 1.)],
    },
    Template {
        name: "North wing",
        work: &[zone(0., 0., 1., 0.62)],
        rest: &[zone(0., 0.68, 1., 0.32)],
    },
    Template {
        name: "South wing",
        work: &[zone(0., 0.38, 1., 0.62)],
        rest: &[zone(0., 0., 1., 0.32)],
    },
    Template {
        name: "Central lounge",
        work: &[zone(0., 0., 0.3, 1.), zone(0.7, 0., 0.3, 1.)],
        rest: &[zone(0.35, 0.12, 0.3, 0.76)],
    },
    Template {
        name: "Lounge avenue",
        work: &[zone(0., 0., 1., 0.3), zone(0., 0.7, 1., 0.3)],
        rest: &[zone(0.12, 0.35, 0.76, 0.3)],
    },
    Template {
        name: "Corner studio",
        work: &[zone(0., 0., 1., 0.42), zone(0., 0.48, 0.52, 0.52)],
        rest: &[zone(0.58, 0.48, 0.42, 0.52)],
    },
    Template {
        name: "Twin studios",
        work: &[zone(0., 0., 0.47, 0.65), zone(0.53, 0., 0.47, 0.65)],
        rest: &[zone(0., 0.72, 1., 0.28)],
    },
    Template {
        name: "Courtyard",
        work: &[
            zone(0., 0., 0.76, 0.24),
            zone(0., 0.3, 0.24, 0.7),
            zone(0.3, 0.76, 0.7, 0.24),
            zone(0.82, 0., 0.18, 0.7),
        ],
        rest: &[zone(0.3, 0.3, 0.46, 0.4)],
    },
    Template {
        name: "Four corners",
        work: &[
            zone(0., 0., 0.4, 0.4),
            zone(0.6, 0., 0.4, 0.4),
            zone(0., 0.6, 0.4, 0.4),
            zone(0.6, 0.6, 0.4, 0.4),
        ],
        rest: &[zone(0.43, 0.43, 0.14, 0.14)],
    },
    Template {
        name: "Gallery",
        work: &[zone(0., 0., 0.8, 0.3), zone(0., 0.7, 1., 0.3)],
        rest: &[zone(0.1, 0.36, 0.5, 0.28)],
    },
    Template {
        name: "Lounge hub",
        work: &[
            zone(0., 0., 0.3, 0.62),
            zone(0.7, 0., 0.3, 0.62),
            zone(0.2, 0.72, 0.6, 0.28),
        ],
        rest: &[zone(0.35, 0., 0.3, 0.62)],
    },
    Template {
        name: "Staggered studios",
        work: &[
            zone(0., 0., 0.58, 0.28),
            zone(0.42, 0.36, 0.58, 0.28),
            zone(0., 0.72, 0.58, 0.28),
        ],
        rest: &[
            zone(0.66, 0., 0.34, 0.28),
            zone(0., 0.36, 0.34, 0.28),
            zone(0.66, 0.72, 0.34, 0.28),
        ],
    },
    Template {
        name: "Diagonal commons",
        work: &[zone(0., 0., 0.47, 0.47), zone(0.53, 0.53, 0.47, 0.47)],
        rest: &[zone(0.53, 0., 0.47, 0.47), zone(0., 0.53, 0.47, 0.47)],
    },
    Template {
        name: "Long aisle",
        work: &[zone(0., 0., 0.42, 0.72), zone(0.58, 0., 0.42, 0.72)],
        rest: &[zone(0.25, 0.8, 0.5, 0.2)],
    },
    Template {
        name: "Quiet end",
        work: &[zone(0., 0., 0.7, 0.45), zone(0., 0.55, 0.7, 0.45)],
        rest: &[zone(0.78, 0., 0.22, 1.)],
    },
    Template {
        name: "Three pods",
        work: &[
            zone(0., 0., 0.28, 0.6),
            zone(0.36, 0., 0.28, 0.6),
            zone(0.72, 0., 0.28, 0.6),
        ],
        rest: &[zone(0., 0.68, 1., 0.32)],
    },
    Template {
        name: "Reading alcoves",
        work: &[zone(0.28, 0., 0.44, 1.)],
        rest: &[zone(0., 0., 0.2, 0.42), zone(0.8, 0.58, 0.2, 0.42)],
    },
    Template {
        name: "Workshop courts",
        work: &[
            zone(0., 0., 0.65, 0.3),
            zone(0., 0.38, 0.65, 0.24),
            zone(0., 0.7, 0.65, 0.3),
        ],
        rest: &[zone(0.73, 0.2, 0.27, 0.6)],
    },
    Template {
        name: "Welcome lounge",
        work: &[zone(0., 0.36, 0.45, 0.64), zone(0.55, 0.36, 0.45, 0.64)],
        rest: &[zone(0., 0., 0.72, 0.28)],
    },
    Template {
        name: "Crossroads",
        work: &[zone(0., 0., 0.42, 0.62), zone(0.58, 0.38, 0.42, 0.62)],
        rest: &[zone(0.58, 0., 0.42, 0.28), zone(0., 0.72, 0.42, 0.28)],
    },
    Template {
        name: "Campus lanes",
        work: &[
            zone(0., 0., 0.28, 1.),
            zone(0.36, 0., 0.28, 0.65),
            zone(0.72, 0., 0.28, 1.),
        ],
        rest: &[zone(0.36, 0.73, 0.28, 0.27)],
    },
    Template {
        name: "Open atrium",
        work: &[
            zone(0., 0., 1., 0.28),
            zone(0., 0.36, 0.28, 0.64),
            zone(0.72, 0.36, 0.28, 0.64),
        ],
        rest: &[zone(0.36, 0.36, 0.28, 0.64)],
    },
    Template {
        name: "Focus bays",
        work: &[
            zone(0., 0., 0.42, 0.28),
            zone(0., 0.36, 0.42, 0.28),
            zone(0., 0.72, 0.42, 0.28),
            zone(0.5, 0., 0.5, 0.58),
        ],
        rest: &[zone(0.5, 0.66, 0.5, 0.34)],
    },
];

pub(super) struct Plan {
    pub width: f64,
    pub height: f64,
    pub work: Vec<Rect>,
    pub rest: Vec<Rect>,
    pub capacity: usize,
    desks: Vec<(f64, f64)>,
    pub benches: Vec<(f64, f64)>,
    walkable: Vec<bool>,
    grid_width: usize,
}

impl Plan {
    pub fn new(count: usize, design: usize) -> Self {
        Self::fit(count, design, 1.6)
    }

    pub fn fit(count: usize, design: usize, aspect: f64) -> Self {
        let capacity = count.max(2);
        let bench_count = capacity.div_ceil(3);
        let template = &TEMPLATES[design % TEMPLATES.len()];
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.6
        };
        let mut width: f64 = 256.0;
        let mut height: f64 = 256.0;
        let mut grids = Vec::new();
        for (zones, count, pitch) in [
            (template.work, capacity, 96.0),
            (template.rest, bench_count, 144.0),
        ] {
            let mut columns = Vec::new();
            for (index, zone) in zones.iter().enumerate() {
                let seats = count / zones.len() + usize::from(index < count % zones.len());
                let size = |cols: usize| {
                    (
                        (cols as f64 * pitch + 16.0) / zone.w + 32.0,
                        (seats.div_ceil(cols) as f64 * 128.0 + 16.0) / zone.h + 96.0,
                    )
                };
                let cols = (1..=seats.max(1))
                    .min_by(|&a, &b| {
                        let (aw, ah) = size(a);
                        let (bw, bh) = size(b);
                        (aw / aspect).max(ah).total_cmp(&(bw / aspect).max(bh))
                    })
                    .unwrap();
                if seats > 0 {
                    let (w, h) = size(cols);
                    width = width.max(w);
                    height = height.max(h);
                }
                columns.push(cols);
            }
            grids.push(columns);
        }
        width = width.max(height * aspect);
        height = width / aspect;
        let scale_zones = |zones: &[Rect]| -> Vec<Rect> {
            zones
                .iter()
                .map(|r| Rect {
                    x: 16.0 + r.x * (width - 32.0),
                    y: 64.0 + r.y * (height - 96.0),
                    w: r.w * (width - 32.0),
                    h: r.h * (height - 96.0),
                })
                .collect()
        };
        let work = scale_zones(template.work);
        let rest = scale_zones(template.rest);
        let seats = |zones: &[Rect], count: usize, columns: &[usize]| -> Vec<(f64, f64)> {
            (0..count)
                .map(|slot| {
                    let index = slot % zones.len();
                    let zone = zones[index];
                    let local = slot / zones.len();
                    let cols = columns[index];
                    let count = count / zones.len() + usize::from(index < count % zones.len());
                    let pitch_x = (zone.w - 16.0) / cols as f64;
                    let pitch_y = (zone.h - 16.0) / count.div_ceil(cols) as f64;
                    let x = zone.x + pitch_x * (local % cols) as f64 + pitch_x / 2.0;
                    let y =
                        zone.y + pitch_y * (local / cols) as f64 + 104.0 + (pitch_y - 128.0) / 2.0;
                    ((x / 8.0).round() * 8.0, (y / 8.0).round() * 8.0)
                })
                .collect()
        };
        let desks = seats(&work, capacity, &grids[0]);
        let benches = seats(&rest, bench_count, &grids[1]);
        let grid_width = width as usize / 8 + 1;
        let grid_height = height as usize / 8 + 1;
        let mut plan = Self {
            width,
            height,
            work,
            rest,
            capacity,
            desks,
            benches,
            walkable: vec![true; grid_width * grid_height],
            grid_width,
        };
        let mut obstacles = Vec::new();
        for &(x, y) in &plan.desks {
            obstacles.push(Rect {
                x: x - 36.0,
                y: y - 64.0,
                w: 72.0,
                h: 48.0,
            });
            obstacles.push(Rect {
                x: x + 28.0,
                y: y - 4.0,
                w: 12.0,
                h: 12.0,
            });
        }
        for (slot, &(x, y)) in plan.benches.iter().enumerate() {
            obstacles.push(Rect {
                x: x - 44.0,
                y: y - 40.0,
                w: 88.0,
                h: 32.0,
            });
            obstacles.push(plan.coffee_table(slot));
        }
        obstacles.extend(plan.plants().map(|(x, y)| Rect {
            x: x - 8.0,
            y: y - 12.0,
            w: 16.0,
            h: 14.0,
        }));
        for i in 0..plan.walkable.len() {
            let (x, y) = plan.point(i);
            plan.walkable[i] = x >= 16.0
                && x <= width - 16.0
                && y >= 64.0
                && y <= height - 16.0
                && !obstacles.iter().any(|r| r.contains(x, y));
        }
        plan
    }
    pub fn desk(&self, slot: usize) -> (f64, f64) {
        self.desks[slot]
    }
    pub fn sofa(&self, slot: usize) -> (f64, f64) {
        let (x, y) = self.benches[slot / 3];
        (x + (slot % 3) as f64 * 24.0 - 24.0, y)
    }
    pub fn coffee_table(&self, slot: usize) -> Rect {
        let (x, y) = self.benches[slot];
        Rect {
            x: x + 50.0,
            y: y - 12.0,
            w: 17.0,
            h: 19.0,
        }
    }
    pub fn plants(&self) -> [(f64, f64); 2] {
        [
            (26.0, self.height - 10.0),
            (self.width - 26.0, self.height - 10.0),
        ]
    }
    pub fn destination(&self, slot: usize, status: AgentStatus, ended: bool) -> (f64, f64) {
        if ended {
            return (
                self.desk(slot).0,
                ((self.height - 24.0) / 8.0).floor() * 8.0,
            );
        }
        match status {
            AgentStatus::Done => self.sofa(slot),
            AgentStatus::Idle => {
                let (x, y) = self.sofa(slot);
                (x, y + 24.0)
            }
            AgentStatus::Blocked => {
                let (x, y) = self.desk(slot);
                (x, y + 24.0)
            }
            _ => self.desk(slot),
        }
    }

    pub fn rest_stop(&self, slot: usize, phase: u8) -> (f64, f64) {
        if phase == 0 {
            return self.sofa(slot);
        }
        let (x, y) = self.benches[slot / 3];
        let point = if phase == 1 {
            (x + 56.0, y + 24.0 + (slot % 3) as f64 * 16.0)
        } else {
            (x - 48.0 + (slot % 3) as f64 * 24.0, y + 48.0)
        };
        self.nearest_walkable(point)
    }

    fn point(&self, index: usize) -> (f64, f64) {
        (
            (index % self.grid_width * 8) as f64,
            (index / self.grid_width * 8) as f64,
        )
    }

    pub fn nearest_walkable(&self, point: (f64, f64)) -> (f64, f64) {
        let col = (point.0 / 8.0).round().max(0.0) as usize;
        let row = (point.1 / 8.0).round().max(0.0) as usize;
        if col < self.grid_width && row < self.walkable.len() / self.grid_width {
            let index = row * self.grid_width + col;
            if self.walkable[index] {
                return self.point(index);
            }
        }
        self.walkable
            .iter()
            .enumerate()
            .filter(|(_, open)| **open)
            .min_by(|(a, _), (b, _)| {
                let distance = |index| {
                    let (x, y) = self.point(index);
                    (x - point.0).powi(2) + (y - point.1).powi(2)
                };
                distance(*a).total_cmp(&distance(*b))
            })
            .map(|(index, _)| self.point(index))
            .unwrap_or(point)
    }

    pub fn route(&self, start: (f64, f64), end: (f64, f64)) -> VecDeque<(f64, f64)> {
        let index = |(x, y): (f64, f64)| {
            (y / 8.0).round() as usize * self.grid_width + (x / 8.0).round() as usize
        };
        // A moving actor can be between tiles, or under furniture moved by a reflow.
        let first = index(self.nearest_walkable(start));
        let last = index(end);
        if first >= self.walkable.len() || last >= self.walkable.len() || !self.walkable[last] {
            return VecDeque::new();
        }
        let mut previous = vec![usize::MAX; self.walkable.len()];
        previous[first] = first;
        let mut queue = VecDeque::from([first]);
        while let Some(current) = queue.pop_front() {
            if current == last {
                break;
            }
            for next in [
                current.checked_sub(1),
                current.checked_add(1),
                current.checked_sub(self.grid_width),
                current.checked_add(self.grid_width),
            ]
            .into_iter()
            .flatten()
            {
                if next < self.walkable.len()
                    && (next % self.grid_width).abs_diff(current % self.grid_width)
                        + (next / self.grid_width).abs_diff(current / self.grid_width)
                        == 1
                    && self.walkable[next]
                    && previous[next] == usize::MAX
                {
                    previous[next] = current;
                    queue.push_back(next);
                }
            }
        }
        if previous[last] == usize::MAX {
            return VecDeque::new();
        }
        let mut path = VecDeque::new();
        let mut cursor = last;
        while cursor != first {
            path.push_front((
                (cursor % self.grid_width * 8) as f64,
                (cursor / self.grid_width * 8) as f64,
            ));
            cursor = previous[cursor];
        }
        if start != self.point(first) || path.is_empty() {
            path.push_front(self.point(first));
        }
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn twenty_four_spatial_templates_and_shared_lounges() {
        let mut signatures = std::collections::HashSet::new();
        for design in 0..24 {
            let plan = Plan::fit(12, design, 1.6);
            let signature: Vec<_> = plan
                .work
                .iter()
                .chain(plan.rest.iter())
                .map(|r| {
                    (
                        (r.x / plan.width * 1000.0) as i32,
                        (r.y / plan.height * 1000.0) as i32,
                        (r.w / plan.width * 1000.0) as i32,
                        (r.h / plan.height * 1000.0) as i32,
                    )
                })
                .collect();
            assert!(
                signatures.insert(signature),
                "duplicate spatial template {design}"
            );
            assert_eq!(plan.benches.len(), 4);
            for slot in 0..12 {
                for phase in [1, 2] {
                    let end = plan.rest_stop(slot, phase);
                    assert_eq!(plan.route(plan.sofa(slot), end).back(), Some(&end));
                }
            }
        }
    }
    #[test]
    fn fractional_rooms_have_reachable_exits_and_tables_block_paths() {
        for design in 0..24 {
            for count in [2, 3, 8, 32] {
                for aspect in [0.7, 1.0, 1.3, 1.7, 2.1] {
                    let plan = Plan::fit(count, design, aspect);
                    for slot in 0..count {
                        let end = plan.destination(slot, AgentStatus::Working, true);
                        let route = plan.route(plan.desk(slot), end);
                        assert_eq!(
                            route.back(),
                            Some(&end),
                            "design={design}, count={count}, aspect={aspect}, slot={slot}"
                        );
                        for point in plan.route(plan.desk(slot), plan.sofa(slot)) {
                            assert!((0..plan.benches.len())
                                .all(|other| !plan.coffee_table(other).contains(point.0, point.1)));
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn agent_office_all_rooms_fit_without_overlap_at_any_count_or_aspect() {
        for (w, h) in [(1280.0, 620.0), (400.0, 900.0), (800.0, 300.0)] {
            for count in [1, 2, 3, 4, 9, 24, 100] {
                let weights: Vec<_> = (0..count).map(|i| (i % 7 + 1) as f64).collect();
                let rooms = tiles(
                    &weights,
                    Rect {
                        x: 0.0,
                        y: 0.0,
                        w,
                        h,
                    },
                );
                assert_eq!(rooms.len(), count);
                assert!((rooms.iter().map(|r| r.w * r.h).sum::<f64>() - w * h).abs() < 0.001);
                for (i, a) in rooms.iter().enumerate() {
                    assert!(
                        a.w > 0.0 && a.h > 0.0 && a.x + a.w <= w + 0.001 && a.y + a.h <= h + 0.001
                    );
                    for b in &rooms[i + 1..] {
                        assert!(
                            a.x + a.w <= b.x + 0.001
                                || b.x + b.w <= a.x + 0.001
                                || a.y + a.h <= b.y + 0.001
                                || b.y + b.h <= a.y + 0.001
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn office_desks_reflow_with_the_available_aspect() {
        for design in 0..4 {
            let narrow = Plan::fit(2, design, 0.2);
            let wide = Plan::fit(2, design, 2.4);
            assert_eq!(narrow.desk(0).0, narrow.desk(1).0);
            assert_ne!(narrow.desk(0).1, narrow.desk(1).1);
            assert_ne!(wide.desk(0).0, wide.desk(1).0);
            assert_eq!(wide.desk(0).1, wide.desk(1).1);
        }
    }
    #[test]
    fn fitted_offices_fill_their_tiles_and_keep_clear_routes() {
        for design in 0..24 {
            for count in [2, 32] {
                for aspect in [0.4, 1.0, 2.4] {
                    let plan = Plan::fit(count, design, aspect);
                    assert!((plan.width / plan.height - aspect).abs() < 0.000_001);
                    let mut seats = std::collections::HashSet::new();
                    for slot in 0..count {
                        for status in [
                            AgentStatus::Working,
                            AgentStatus::Blocked,
                            AgentStatus::Done,
                            AgentStatus::Idle,
                        ] {
                            let end = plan.destination(slot, status, false);
                            assert!(seats.insert((end.0 as i32, end.1 as i32)));
                            let path = plan.route(plan.desk(slot), end);
                            assert_eq!(path.back(), Some(&end));
                            for (x, y) in path {
                                assert!(
                                    plan.walkable
                                        [y as usize / 8 * plan.grid_width + x as usize / 8]
                                );
                            }
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn agent_office_dense_plans_have_unique_seats_and_obstacle_free_routes() {
        for design in 0..24 {
            let plan = Plan::new(100, design);
            let mut seats = std::collections::HashSet::new();
            for slot in 0..100 {
                for status in [
                    AgentStatus::Working,
                    AgentStatus::Blocked,
                    AgentStatus::Done,
                    AgentStatus::Idle,
                ] {
                    let end = plan.destination(slot, status, false);
                    assert!(seats.insert((end.0 as i32, end.1 as i32)));
                    let path = plan.route(plan.desk(slot), end);
                    assert_eq!(path.back(), Some(&end));
                    for (x, y) in path {
                        assert!(plan.walkable[y as usize / 8 * plan.grid_width + x as usize / 8]);
                    }
                }
            }
        }
    }
}
