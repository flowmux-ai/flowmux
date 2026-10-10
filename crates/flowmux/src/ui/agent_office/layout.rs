// SPDX-License-Identifier: GPL-3.0-or-later
//! Capacity-aware floor plans, walkable routes, and a viewport-filling room map.

use flowmux_core::AgentStatus;
use std::collections::VecDeque;

pub(super) const OBJECT_SCALE: f64 = 1.25;

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn enlarged(self, origin: (f64, f64)) -> Self {
        Self {
            x: origin.0 + (self.x - origin.0) * OBJECT_SCALE,
            y: origin.1 + (self.y - origin.1) * OBJECT_SCALE,
            w: self.w * OBJECT_SCALE,
            h: self.h * OBJECT_SCALE,
        }
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.h
    }
}

pub(super) fn tiles(weights: &[f64], bounds: Rect) -> Vec<Rect> {
    if weights.is_empty() {
        return Vec::new();
    }
    let total: f64 = weights.iter().sum();
    // Variable row lengths give large teams their own row without squeezing small teams.
    [false, true]
        .into_iter()
        .map(|vertical| {
            let (width, height) = if vertical {
                (bounds.h, bounds.w)
            } else {
                (bounds.w, bounds.h)
            };
            let target = if vertical { 1.0 / 1.25 } else { 1.25 };
            let mut scores = vec![f64::INFINITY; weights.len() + 1];
            let mut previous = vec![0; weights.len() + 1];
            scores[0] = 0.0;
            for end in 1..=weights.len() {
                let (mut sum, mut smallest, mut largest) = (0.0, f64::INFINITY, 0.0_f64);
                for start in (0..end).rev() {
                    sum += weights[start];
                    smallest = smallest.min(weights[start]);
                    largest = largest.max(weights[start]);
                    let aspect = width * total / (height * sum * sum * target);
                    let score = scores[start]
                        .max((smallest * aspect).ln().abs())
                        .max((largest * aspect).ln().abs());
                    if score < scores[end] {
                        scores[end] = score;
                        previous[end] = start;
                    }
                }
            }
            let mut rows = Vec::new();
            let mut end = weights.len();
            while end > 0 {
                let start = previous[end];
                rows.push(start..end);
                end = start;
            }
            let mut result = Vec::with_capacity(weights.len());
            let mut y = 0.0;
            for row in rows.into_iter().rev() {
                let members = &weights[row];
                let sum: f64 = members.iter().sum();
                let h = height * sum / total;
                let mut x = 0.0;
                for &weight in members {
                    let w = width * weight / sum;
                    result.push(if vertical {
                        Rect {
                            x: bounds.x + y,
                            y: bounds.y + x,
                            w: h,
                            h: w,
                        }
                    } else {
                        Rect {
                            x: bounds.x + x,
                            y: bounds.y + y,
                            w,
                            h,
                        }
                    });
                    x += w;
                }
                y += h;
            }
            (scores[weights.len()], result)
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap()
        .1
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
        name: "Window desks",
        work: &[zone(0.0, 0.0, 0.6, 1.0)],
        rest: &[zone(0.66, 0.0, 0.34, 1.0)],
    },
    Template {
        name: "Conversation nook",
        work: &[zone(0.4, 0.0, 0.6, 1.0)],
        rest: &[zone(0.0, 0.0, 0.34, 1.0)],
    },
    Template {
        name: "Long worktable",
        work: &[zone(0.0, 0.0, 1.0, 0.58)],
        rest: &[zone(0.0, 0.64, 1.0, 0.36)],
    },
    Template {
        name: "Front porch",
        work: &[zone(0.0, 0.42, 1.0, 0.58)],
        rest: &[zone(0.0, 0.0, 1.0, 0.36)],
    },
    Template {
        name: "Growing aisle",
        work: &[zone(0.0, 0.0, 0.58, 1.0)],
        rest: &[zone(0.64, 0.06, 0.36, 0.88)],
    },
    Template {
        name: "Glass court",
        work: &[zone(0.0, 0.0, 0.46, 0.58), zone(0.54, 0.0, 0.46, 0.58)],
        rest: &[zone(0.08, 0.66, 0.84, 0.34)],
    },
    Template {
        name: "Seed library",
        work: &[zone(0.42, 0.0, 0.58, 1.0)],
        rest: &[zone(0.0, 0.08, 0.36, 0.84)],
    },
    Template {
        name: "Canopy desks",
        work: &[zone(0.0, 0.4, 0.46, 0.6), zone(0.54, 0.4, 0.46, 0.6)],
        rest: &[zone(0.08, 0.0, 0.84, 0.32)],
    },
    Template {
        name: "Signal bays",
        work: &[zone(0.0, 0.0, 0.62, 1.0)],
        rest: &[zone(0.68, 0.04, 0.32, 0.92)],
    },
    Template {
        name: "Paired consoles",
        work: &[zone(0.0, 0.0, 0.46, 0.62), zone(0.54, 0.0, 0.46, 0.62)],
        rest: &[zone(0.0, 0.7, 1.0, 0.3)],
    },
    Template {
        name: "Observation deck",
        work: &[zone(0.0, 0.4, 1.0, 0.6)],
        rest: &[zone(0.06, 0.0, 0.88, 0.32)],
    },
    Template {
        name: "Relay room",
        work: &[zone(0.4, 0.0, 0.6, 0.46), zone(0.4, 0.54, 0.6, 0.46)],
        rest: &[zone(0.0, 0.0, 0.32, 1.0)],
    },
    Template {
        name: "Archive lane",
        work: &[zone(0.0, 0.0, 0.56, 1.0)],
        rest: &[zone(0.64, 0.0, 0.36, 1.0)],
    },
    Template {
        name: "Reading court",
        work: &[zone(0.0, 0.44, 1.0, 0.56)],
        rest: &[zone(0.0, 0.0, 1.0, 0.36)],
    },
    Template {
        name: "Quiet alcoves",
        work: &[zone(0.42, 0.0, 0.58, 0.46), zone(0.42, 0.54, 0.58, 0.46)],
        rest: &[zone(0.0, 0.06, 0.34, 0.88)],
    },
    Template {
        name: "Study pairs",
        work: &[zone(0.0, 0.0, 0.46, 0.56), zone(0.54, 0.0, 0.46, 0.56)],
        rest: &[zone(0.04, 0.64, 0.92, 0.36)],
    },
    Template {
        name: "Assembly line",
        work: &[zone(0.0, 0.0, 0.64, 1.0)],
        rest: &[zone(0.7, 0.0, 0.3, 1.0)],
    },
    Template {
        name: "Tool island",
        work: &[zone(0.0, 0.0, 1.0, 0.6)],
        rest: &[zone(0.06, 0.68, 0.88, 0.32)],
    },
    Template {
        name: "Twin benches",
        work: &[zone(0.0, 0.4, 0.46, 0.6), zone(0.54, 0.4, 0.46, 0.6)],
        rest: &[zone(0.0, 0.0, 1.0, 0.3)],
    },
    Template {
        name: "Material wall",
        work: &[zone(0.4, 0.0, 0.6, 1.0)],
        rest: &[zone(0.0, 0.04, 0.32, 0.92)],
    },
    Template {
        name: "Counter seats",
        work: &[zone(0.0, 0.0, 0.58, 0.46), zone(0.0, 0.54, 0.58, 0.46)],
        rest: &[zone(0.66, 0.0, 0.34, 1.0)],
    },
    Template {
        name: "Corner gathering",
        work: &[zone(0.44, 0.0, 0.56, 1.0)],
        rest: &[zone(0.0, 0.06, 0.36, 0.88)],
    },
    Template {
        name: "Morning tables",
        work: &[zone(0.0, 0.0, 0.46, 0.6), zone(0.54, 0.0, 0.46, 0.6)],
        rest: &[zone(0.0, 0.68, 1.0, 0.32)],
    },
    Template {
        name: "Evening commons",
        work: &[zone(0.0, 0.46, 1.0, 0.54)],
        rest: &[zone(0.04, 0.0, 0.92, 0.38)],
    },
];

pub(super) struct Plan {
    pub width: f64,
    pub height: f64,
    #[cfg(test)]
    pub work: Vec<Rect>,
    #[cfg(test)]
    pub rest: Vec<Rect>,
    pub capacity: usize,
    desks: Vec<(f64, f64)>,
    south: Vec<bool>,
    pub fixtures: Vec<Rect>,
    pub benches: Vec<(f64, f64)>,
    walkable: Vec<bool>,
    grid_width: usize,
}

impl Plan {
    pub fn new(count: usize, design: usize) -> Self {
        Self::fit(count, design, 1.6)
    }

    pub fn fit(count: usize, design: usize, aspect: f64) -> Self {
        let capacity = count.max(1);
        let bench_count = capacity.div_ceil(3);
        let template = &TEMPLATES[design % TEMPLATES.len()];
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.6
        };
        let groups = template.work.len().min(capacity);
        let group_count = capacity.div_ceil(groups);
        let outer_horizontal = template.work[0].x + template.work[0].w <= template.rest[0].x
            || template.rest[0].x + template.rest[0].w <= template.work[0].x;
        let rest_first = if outer_horizontal {
            template.rest[0].x < template.work[0].x
        } else {
            template.rest[0].y < template.work[0].y
        };
        let inner_horizontal = groups == 1 || template.work[0].x != template.work[1].x;
        // Keep each design's corridor character, without reserving a fixed fraction of the floor.
        let unused = 1.0
            - template
                .work
                .iter()
                .chain(template.rest)
                .map(|r| r.w * r.h)
                .sum::<f64>();
        let gap = 8.0 + unused * 48.0;
        let arrange = |work_cols: usize, rest_cols: usize, turn: bool| {
            let horizontal = outer_horizontal != turn;
            let split_horizontal = inner_horizontal != turn;
            let mut work = Vec::with_capacity(groups);
            let (mut work_w, mut work_h) = (0.0_f64, 0.0_f64);
            for i in 0..groups {
                let count = capacity / groups + usize::from(i < capacity % groups);
                let cols = work_cols.min(count);
                // Preserve template proportions as small margins around the occupied cells.
                let (w, h) = (
                    cols as f64 * 96.0 + 16.0 + template.work[i].w * 16.0,
                    count.div_ceil(cols) as f64 * 112.0 + 16.0 + template.work[i].h * 16.0,
                );
                work.push(Rect {
                    x: if split_horizontal { work_w } else { 0.0 },
                    y: if split_horizontal { 0.0 } else { work_h },
                    w,
                    h,
                });
                if split_horizontal {
                    work_w += w + gap;
                    work_h = work_h.max(h);
                } else {
                    work_h += h + gap;
                    work_w = work_w.max(w);
                }
            }
            if split_horizontal {
                work_w -= gap;
            } else {
                work_h -= gap;
            }
            let (rest_w, rest_h) = (
                rest_cols as f64 * 144.0 + 16.0 + template.rest[0].w * 16.0,
                bench_count.div_ceil(rest_cols) as f64 * 112.0 + 16.0 + template.rest[0].h * 16.0,
            );
            let (content_w, content_h) = if horizontal {
                (work_w + gap + rest_w, work_h.max(rest_h))
            } else {
                (work_w.max(rest_w), work_h + gap + rest_h)
            };
            let width = (content_w + 32.0).max((content_h + 96.0) * aspect);
            let height = width / aspect;
            let left = 16.0 + (width - 32.0 - content_w) / 2.0;
            let top = 64.0 + (height - 96.0 - content_h) / 2.0;
            let mut rest = Rect {
                x: left,
                y: top,
                w: rest_w,
                h: rest_h,
            };
            let (work_x, work_y) = if horizontal {
                rest.y += (content_h - rest_h) / 2.0;
                if !rest_first {
                    rest.x += work_w + gap;
                }
                (
                    left + if rest_first { rest_w + gap } else { 0.0 },
                    top + (content_h - work_h) / 2.0,
                )
            } else {
                rest.x += (content_w - rest_w) / 2.0;
                if !rest_first {
                    rest.y += work_h + gap;
                }
                (
                    left + (content_w - work_w) / 2.0,
                    top + if rest_first { rest_h + gap } else { 0.0 },
                )
            };
            for r in &mut work {
                r.x += work_x
                    + if split_horizontal {
                        0.0
                    } else {
                        (work_w - r.w) / 2.0
                    };
                r.y += work_y
                    + if split_horizontal {
                        (work_h - r.h) / 2.0
                    } else {
                        0.0
                    };
            }
            (width, height, work, vec![rest])
        };
        let mut best = (f64::INFINITY, 1, 1, false);
        for turn in [false, true] {
            for work_cols in 1..=group_count {
                for rest_cols in 1..=bench_count {
                    let (width, _, _, _) = arrange(work_cols, rest_cols, turn);
                    if width < best.0 {
                        best = (width, work_cols, rest_cols, turn);
                    }
                }
            }
        }
        let (_, work_cols, rest_cols, turn) = best;
        let (width, height, work, rest) = arrange(work_cols, rest_cols, turn);
        let grids = [vec![work_cols; groups], vec![rest_cols]];
        let seats =
            |zones: &[Rect], count: usize, columns: &[usize], max_pitch: f64| -> Vec<(f64, f64)> {
                (0..count)
                    .map(|slot| {
                        let index = slot % zones.len();
                        let zone = zones[index];
                        let local = slot / zones.len();
                        let count = count / zones.len() + usize::from(index < count % zones.len());
                        let cols = columns[index].min(count);
                        let rows = count.div_ceil(cols);
                        let pitch_x = ((zone.w - 16.0) / cols as f64).min(max_pitch);
                        let pitch_y = ((zone.h - 16.0) / rows as f64).min(112.0);
                        let left = zone.x + (zone.w - 16.0 - pitch_x * cols as f64) / 2.0;
                        let top = zone.y + (zone.h - 16.0 - pitch_y * rows as f64) / 2.0;
                        let x = left + pitch_x * (local % cols) as f64 + pitch_x / 2.0;
                        let y =
                            top + pitch_y * (local / cols) as f64 + 96.0 + (pitch_y - 112.0) / 2.0;
                        ((x / 8.0).round() * 8.0, (y / 8.0).round() * 8.0)
                    })
                    .collect()
            };
        let mut desks = seats(&work, capacity, &grids[0], 112.0);
        let south: Vec<_> = (0..capacity)
            .map(|slot| {
                let zone = slot % work.len();
                let row = slot / work.len() / grids[0][zone];
                capacity >= 4 && (row + design) % 2 == 1
            })
            .collect();
        for (slot, point) in desks.iter_mut().enumerate() {
            point.1 -= if south[slot] { 64.0 } else { 16.0 };
        }
        let benches = seats(&rest, bench_count, &grids[1], 144.0);
        let fixtures = benches
            .iter()
            .take(rest.len())
            .map(|&(x, y)| Rect {
                x: x - 40.0,
                y: y - 88.0,
                w: 80.0,
                h: 24.0,
            })
            .collect();
        let grid_width = width as usize / 8 + 1;
        let grid_height = height as usize / 8 + 1;
        let mut plan = Self {
            width,
            height,
            #[cfg(test)]
            work,
            #[cfg(test)]
            rest,
            capacity,
            desks,
            south,
            fixtures,
            benches,
            walkable: vec![true; grid_width * grid_height],
            grid_width,
        };
        let mut obstacles = Vec::new();
        for (slot, &(x, y)) in plan.desks.iter().enumerate() {
            obstacles.push(
                Rect {
                    x: x - 36.0,
                    y: y + if plan.south[slot] { 16.0 } else { -64.0 },
                    w: 72.0,
                    h: 48.0,
                }
                .enlarged((x, y)),
            );
            obstacles.push(
                Rect {
                    x: x + 28.0,
                    y: y - 4.0,
                    w: 12.0,
                    h: 12.0,
                }
                .enlarged((x, y)),
            );
        }
        for (slot, &(x, y)) in plan.benches.iter().enumerate() {
            obstacles.push(
                Rect {
                    x: x - 44.0,
                    y: y - 40.0,
                    w: 88.0,
                    h: 32.0,
                }
                .enlarged((x, y)),
            );
            obstacles.push(plan.coffee_table(slot).enlarged((x, y)));
        }
        obstacles.extend(
            plan.fixtures
                .iter()
                .map(|r| r.enlarged((r.x + r.w / 2.0, r.y + r.h))),
        );
        obstacles.extend(plan.plants().map(|(x, y)| {
            Rect {
                x: x - 8.0,
                y: y - 12.0,
                w: 16.0,
                h: 14.0,
            }
            .enlarged((x, y))
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
    pub fn desk_faces_south(&self, slot: usize) -> bool {
        self.south[slot]
    }
    pub fn sofa(&self, slot: usize) -> (f64, f64) {
        let (x, y) = self.benches[slot / 3];
        (x + (slot % 3) as f64 * 32.0 - 32.0, y)
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
                (x, y + if self.south[slot] { -24.0 } else { 24.0 })
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
            (x + 72.0, y + 24.0 + (slot % 3) as f64 * 16.0)
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
            for count in [1, 2, 3, 8, 32] {
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
                            assert!((0..plan.benches.len()).all(|other| !plan
                                .coffee_table(other)
                                .enlarged(plan.benches[other])
                                .contains(point.0, point.1)));
                            assert!(plan.fixtures.iter().all(|r| !r
                                .enlarged((r.x + r.w / 2.0, r.y + r.h))
                                .contains(point.0, point.1)));
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
        let rooms = tiles(
            &[36., 6., 6., 6., 6., 6., 6., 6.],
            Rect {
                x: 0.,
                y: 0.,
                w: 1280.,
                h: 620.,
            },
        );
        assert!(
            rooms.iter().all(|r| (0.5..2.5).contains(&(r.w / r.h))),
            "a dense team must not turn small offices into slivers"
        );
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
            for count in [1, 2, 32] {
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
    fn occupied_floor_stays_compact_as_teams_grow() {
        let greenhouse = Plan::fit(4, 5, 1.5);
        assert!(
            greenhouse.width < 580.0,
            "small teams must fill their office too"
        );
        for design in 0..24 {
            for count in [0, 1] {
                let plan = Plan::new(count, design);
                assert_eq!(plan.desks.len(), 1);
                assert_eq!(plan.benches.len(), 1);
            }
            for count in [12_usize, 32, 100] {
                for aspect in [0.7, 1.0, 1.6, 2.4] {
                    let plan = Plan::fit(count, design, aspect);
                    let cells = (count * 96 * 112 + count.div_ceil(3) * 144 * 112) as f64;
                    let floor = (plan.width - 32.0) * (plan.height - 96.0);
                    assert!(
                        cells / floor > 0.45,
                        "excess empty floor: design={design}, count={count}, aspect={aspect}"
                    );
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

#[cfg(test)]
mod audit_tests {
    use super::*;
    #[test]
    #[ignore = "exhaustive 7,680-plan audit; run explicitly with optimized tests"]
    fn audit_every_destination_reachable_for_small_and_odd_counts() {
        let mut failures = Vec::new();
        for design in 0..24 {
            for count in 1..=40 {
                for aspect in [0.3, 0.5, 0.8, 1.0, 1.4, 1.9, 2.6, 3.5] {
                    let plan = Plan::fit(count, design, aspect);
                    let mut seats = std::collections::HashSet::new();
                    for slot in 0..count {
                        let exit = plan.destination(slot, AgentStatus::Working, true);
                        for status in [
                            AgentStatus::Working,
                            AgentStatus::Blocked,
                            AgentStatus::Done,
                            AgentStatus::Idle,
                        ] {
                            let end = plan.destination(slot, status, false);
                            if !seats.insert((end.0 as i32, end.1 as i32)) {
                                failures.push(format!(
                                    "dup seat d={design} n={count} a={aspect} s={slot} {status:?}"
                                ));
                            }
                            if plan.route(plan.desk(slot), end).back() != Some(&end) {
                                failures.push(format!("unreachable d={design} n={count} a={aspect} s={slot} {status:?}"));
                            }
                            if plan.route(end, exit).back() != Some(&exit) {
                                failures.push(format!("no exit d={design} n={count} a={aspect} s={slot} from {status:?}"));
                            }
                        }
                        for phase in [1u8, 2] {
                            let stop = plan.rest_stop(slot, phase);
                            if plan.route(plan.sofa(slot), stop).back() != Some(&stop) {
                                failures.push(format!(
                                    "rest stop d={design} n={count} a={aspect} s={slot} p={phase}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        assert!(
            failures.is_empty(),
            "{} failures:\n{}",
            failures.len(),
            failures
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}
