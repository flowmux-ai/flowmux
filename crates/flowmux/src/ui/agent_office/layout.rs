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

pub(super) struct Plan {
    pub width: f64,
    pub height: f64,
    pub work: Rect,
    pub rest: Rect,
    pub capacity: usize,
    cols: usize,
    walkable: Vec<bool>,
    grid_width: usize,
}

impl Plan {
    pub fn new(count: usize, design: usize) -> Self {
        Self::build(count, design, None)
    }

    pub fn fit(count: usize, design: usize, aspect: f64) -> Self {
        Self::build(count, design, Some(aspect))
    }

    fn build(count: usize, design: usize, aspect: Option<f64>) -> Self {
        let capacity = count.max(2);
        let vertical = design % 4 >= 2;
        let dimensions = |cols: usize| {
            let zone_w = cols as f64 * 96.0 + 16.0;
            let zone_h = capacity.div_ceil(cols) as f64 * 128.0 + 16.0;
            if vertical {
                (zone_w + 32.0, zone_h * 2.0 + 128.0)
            } else {
                (zone_w * 2.0 + 48.0, zone_h + 96.0)
            }
        };
        let cols = if let Some(aspect) = aspect {
            // Choose rows and columns before sizing the room; never stretch the artwork.
            (1..=capacity)
                .min_by(|&a, &b| {
                    let (aw, ah) = dimensions(a);
                    let (bw, bh) = dimensions(b);
                    (aw / aspect).max(ah).total_cmp(&(bw / aspect).max(bh))
                })
                .unwrap()
        } else {
            ((capacity as f64 * if vertical { 2.0 } else { 0.7 })
                .sqrt()
                .ceil() as usize)
                .max(2)
        };
        let (mut width, mut height) = dimensions(cols);
        if let Some(aspect) = aspect {
            width = width.max(height * aspect);
            height = width / aspect;
        }
        let zone = Rect {
            x: 16.0,
            y: 64.0,
            w: if vertical {
                width - 32.0
            } else {
                (width - 48.0) / 2.0
            },
            h: if vertical {
                (height - 128.0) / 2.0
            } else {
                height - 96.0
            },
        };
        let other = if vertical {
            Rect {
                y: zone.y + zone.h + 16.0,
                ..zone
            }
        } else {
            Rect {
                x: zone.x + zone.w + 16.0,
                ..zone
            }
        };
        let (work, rest) = if design.is_multiple_of(2) {
            (zone, other)
        } else {
            (other, zone)
        };
        let grid_width = width as usize / 8 + 1;
        let grid_height = height as usize / 8 + 1;
        let mut plan = Self {
            width,
            height,
            work,
            rest,
            capacity,
            cols,
            walkable: vec![true; grid_width * grid_height],
            grid_width,
        };
        let obstacles: Vec<_> = (0..capacity)
            .flat_map(|slot| {
                let (x, y) = plan.desk(slot);
                let (rx, ry) = plan.sofa(slot);
                [
                    Rect {
                        x: x - 36.0,
                        y: y - 64.0,
                        w: 72.0,
                        h: 48.0,
                    },
                    Rect {
                        x: rx - 28.0,
                        y: ry - 40.0,
                        w: 56.0,
                        h: 32.0,
                    },
                ]
            })
            .collect();
        for i in 0..plan.walkable.len() {
            let x = (i % grid_width * 8) as f64;
            let y = (i / grid_width * 8) as f64;
            plan.walkable[i] = x >= 16.0
                && x <= width - 16.0
                && y >= 64.0
                && y <= height - 16.0
                && !obstacles.iter().any(|r| r.contains(x, y));
        }
        plan
    }

    fn seat(&self, zone: Rect, slot: usize) -> (f64, f64) {
        let pitch_x = (zone.w - 16.0) / self.cols as f64;
        let pitch_y = (zone.h - 16.0) / self.capacity.div_ceil(self.cols) as f64;
        let x = zone.x + pitch_x * (slot % self.cols) as f64 + pitch_x / 2.0;
        let y = zone.y + pitch_y * (slot / self.cols) as f64 + 104.0 + (pitch_y - 128.0) / 2.0;
        ((x / 8.0).round() * 8.0, (y / 8.0).round() * 8.0)
    }
    pub fn desk(&self, slot: usize) -> (f64, f64) {
        self.seat(self.work, slot)
    }
    pub fn sofa(&self, slot: usize) -> (f64, f64) {
        self.seat(self.rest, slot)
    }
    pub fn destination(&self, slot: usize, status: AgentStatus, ended: bool) -> (f64, f64) {
        if ended {
            return (self.desk(slot).0, self.height - 16.0);
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

    pub fn route(&self, start: (f64, f64), end: (f64, f64)) -> VecDeque<(f64, f64)> {
        let index = |(x, y): (f64, f64)| {
            (y / 8.0).round() as usize * self.grid_width + (x / 8.0).round() as usize
        };
        let first = index(start);
        let last = index(end);
        if first >= self.walkable.len() || last >= self.walkable.len() {
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
                if next < self.walkable.len() && self.walkable[next] && previous[next] == usize::MAX
                {
                    previous[next] = current;
                    queue.push_back(next);
                }
            }
        }
        if previous[last] == usize::MAX {
            return VecDeque::new();
        }
        let mut path = VecDeque::from([end]);
        let mut cursor = last;
        while cursor != first {
            path.push_front((
                (cursor % self.grid_width * 8) as f64,
                (cursor / self.grid_width * 8) as f64,
            ));
            cursor = previous[cursor];
        }
        path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        for design in 0..4 {
            for count in [2, 32, 100] {
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
        for design in 0..4 {
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
