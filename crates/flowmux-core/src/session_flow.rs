// SPDX-License-Identifier: GPL-3.0-or-later
//! Session-owned lineage and geometry. A workspace supplies anchors, never edges.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;

/// IDs are only unique within one provider's native history store.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionKey {
    pub provider: String,
    pub home: PathBuf,
    pub id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lineage {
    /// Child -> parent. A fork has one source; subagent relationships do not belong here.
    parents: BTreeMap<SessionKey, SessionKey>,
    branches: BTreeSet<SessionKey>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkError {
    DifferentStore,
    Cycle,
    ConflictingParent,
}

impl Lineage {
    /// Idempotent, and leaves the graph unchanged on invalid/conflicting evidence.
    pub fn link(&mut self, parent: SessionKey, child: SessionKey) -> Result<(), LinkError> {
        if parent.provider != child.provider || parent.home != child.home {
            return Err(LinkError::DifferentStore);
        }
        if let Some(existing) = self.parents.get(&child) {
            return if existing == &parent {
                Ok(())
            } else {
                Err(LinkError::ConflictingParent)
            };
        }
        let mut cursor = &parent;
        loop {
            if cursor == &child {
                return Err(LinkError::Cycle);
            }
            if !self.branches.contains(&child) {
                break;
            }
            match self.parents.get(cursor) {
                Some(next) => cursor = next,
                None => break,
            }
        }
        self.branches.insert(parent.clone());
        self.parents.insert(child, parent);
        Ok(())
    }

    pub fn edges(&self) -> impl Iterator<Item = (&SessionKey, &SessionKey)> {
        self.parents.iter().map(|(child, parent)| (parent, child))
    }

    /// Full connected families: ancestors, siblings and descendants of every anchor.
    /// Unrelated singleton sessions are omitted. Ordering is stable across workspaces.
    pub fn project<'a>(&self, anchors: impl IntoIterator<Item = &'a SessionKey>) -> Self {
        let mut adjacent: BTreeMap<&SessionKey, Vec<&SessionKey>> = BTreeMap::new();
        for (parent, child) in self.edges() {
            adjacent.entry(parent).or_default().push(child);
            adjacent.entry(child).or_default().push(parent);
        }
        let mut seen = BTreeSet::new();
        let mut queue: VecDeque<_> = anchors.into_iter().collect();
        while let Some(key) = queue.pop_front() {
            if seen.insert(key) {
                if let Some(neighbors) = adjacent.get(key) {
                    queue.extend(neighbors.iter().copied());
                }
            }
        }
        let parents: BTreeMap<_, _> = self
            .parents
            .iter()
            .filter(|(child, _)| seen.contains(child))
            .map(|(child, parent)| (child.clone(), parent.clone()))
            .collect();
        Self {
            branches: parents.values().cloned().collect(),
            parents,
        }
    }

    /// Stable family membership, independent of any workspace or selected node.
    pub fn families(&self) -> Vec<BTreeSet<SessionKey>> {
        let mut children: BTreeMap<&SessionKey, Vec<&SessionKey>> = BTreeMap::new();
        for (parent, child) in self.edges() {
            children.entry(parent).or_default().push(child);
        }
        let mut families = Vec::new();
        for root in self.roots() {
            let mut nodes = BTreeSet::new();
            let mut queue = VecDeque::from([root]);
            while let Some(key) = queue.pop_front() {
                nodes.insert(key.clone());
                if let Some(next) = children.get(key) {
                    queue.extend(next.iter().copied());
                }
            }
            families.push(nodes);
        }
        families
    }

    fn roots(&self) -> impl Iterator<Item = &SessionKey> {
        self.branches
            .iter()
            .filter(|key| !self.parents.contains_key(*key))
    }

    pub fn has_multiple_families(&self) -> bool {
        self.roots().nth(1).is_some()
    }

    pub fn nodes(&self) -> BTreeSet<&SessionKey> {
        self.edges().flat_map(|(p, c)| [p, c]).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.parents.is_empty()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

fn point_bounds(points: impl Iterator<Item = Point>) -> Option<(Point, Point)> {
    let mut points = points.filter(|p| p.finite());
    let first = points.next()?;
    let (mut min, mut max) = (first, first);
    for p in points {
        min.x = min.x.min(p.x);
        min.y = min.y.min(p.y);
        max.x = max.x.max(p.x);
        max.y = max.y.max(p.y);
    }
    Some((min, max))
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub origin: Point,
    pub zoom: f64,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            origin: Point::default(),
            zoom: 1.0,
        }
    }
}

impl Camera {
    pub fn screen(self, point: Point) -> Point {
        Point {
            x: point.x * self.zoom + self.origin.x,
            y: point.y * self.zoom + self.origin.y,
        }
    }

    pub fn world(self, point: Point) -> Point {
        Point {
            x: (point.x - self.origin.x) / self.zoom,
            y: (point.y - self.origin.y) / self.zoom,
        }
    }

    pub fn zoom_at(&mut self, cursor: Point, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 || !cursor.finite() {
            return;
        }
        let world = self.world(cursor);
        self.zoom = (self.zoom * factor).clamp(0.1, 4.0);
        self.origin = Point {
            x: cursor.x - world.x * self.zoom,
            y: cursor.y - world.y * self.zoom,
        };
    }

    pub fn fit(&mut self, points: impl Iterator<Item = Point>, width: f64, height: f64) {
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return;
        }
        let Some((min, max)) = point_bounds(points) else {
            return;
        };
        self.zoom = ((width - 100.0).max(1.0) / (max.x - min.x + 100.0))
            .min((height - 60.0).max(1.0) / (max.y - min.y + 60.0))
            .clamp(0.1, 2.0);
        self.origin = Point {
            x: width / 2.0 - (min.x + max.x) / 2.0 * self.zoom,
            y: height / 2.0 - (min.y + max.y) / 2.0 * self.zoom,
        };
    }
}

/// The same family always starts at the same geometry, independent of its anchors.
/// Callers share these positions across workspace projections; cameras stay local.
#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub positions: BTreeMap<SessionKey, Point>,
    velocity: BTreeMap<SessionKey, Point>,
    centers: BTreeMap<SessionKey, Point>,
    ticks: usize,
}

impl Layout {
    pub fn sync(&mut self, lineage: &Lineage) {
        let mut offset = 0.0;
        self.centers.clear();
        for family in lineage.families() {
            let radius = 100.0 + (family.len() as f64).sqrt() * 32.0;
            let center = point_bounds(
                family
                    .iter()
                    .filter_map(|key| self.positions.get(key).copied()),
            )
            .map_or(
                Point {
                    x: offset + radius,
                    y: 0.0,
                },
                |(min, max)| Point {
                    x: min.x.midpoint(max.x),
                    y: min.y.midpoint(max.y),
                },
            );
            offset += radius * 2.0 + 120.0;
            for key in family {
                self.centers.insert(key.clone(), center);
                self.positions.entry(key.clone()).or_insert_with(|| {
                    // Fixed FNV seed, unlike randomized HashMap seeds. No workspace ID involved.
                    let hash = key
                        .provider
                        .bytes()
                        .chain(key.home.to_string_lossy().bytes())
                        .chain(key.id.bytes())
                        .fold(2166136261u64, |h, b| {
                            (h ^ u64::from(b)).wrapping_mul(16777619)
                        });
                    let angle = (hash % 62832) as f64 / 10000.0;
                    let distance = radius * (0.3 + ((hash >> 16) % 700) as f64 / 1000.0);
                    Point {
                        x: center.x + angle.cos() * distance,
                        y: center.y + angle.sin() * distance,
                    }
                });
            }
        }
        self.wake();
    }

    /// Arrange the visible families in centered rows, preserving each family's shape.
    pub fn pack(&mut self, lineage: &Lineage, aspect: f64) -> bool {
        const GAP: f64 = 90.0;
        if !aspect.is_finite() || aspect <= 0.0 || !lineage.has_multiple_families() {
            return false;
        }
        let families: Vec<_> = lineage
            .families()
            .into_iter()
            .filter_map(|keys| {
                let (min, max) = point_bounds(
                    keys.iter()
                        .filter_map(|key| self.positions.get(key).copied()),
                )?;
                let width = max.x - min.x;
                let height = max.y - min.y;
                (width + height)
                    .is_finite()
                    .then_some((keys, min, width, height))
            })
            .collect();
        if families.len() < 2 {
            return false;
        }
        let area: f64 = families
            .iter()
            .map(|(_, _, w, h)| (w + GAP) * (h + GAP))
            .sum();
        let widest = families.iter().map(|(_, _, w, _)| *w).fold(0.0, f64::max);
        let target_width = (area * aspect).sqrt().max(widest);
        let mut remaining = families.as_slice();
        let mut y = 0.0;
        let mut moved = false;
        while !remaining.is_empty() {
            let mut row_width = 0.0;
            let mut row_height: f64 = 0.0;
            let mut count = 0;
            for (_, _, width, height) in remaining {
                let next_width = row_width + width + GAP;
                if count > 0 && next_width - GAP > target_width {
                    break;
                }
                row_width = next_width;
                row_height = row_height.max(*height);
                count += 1;
            }
            let (row, rest) = remaining.split_at(count);
            let mut x = -(row_width - GAP) / 2.0;
            for (keys, min, width, height) in row {
                let delta = Point {
                    x: x - min.x,
                    y: y + (row_height - height) / 2.0 - min.y,
                };
                x += width + GAP;
                if !delta.finite() || delta.x.abs().max(delta.y.abs()) < 0.5 {
                    continue;
                }
                for key in keys {
                    if let Some(point) = self.positions.get_mut(key) {
                        point.x += delta.x;
                        point.y += delta.y;
                    }
                    if let Some(center) = self.centers.get_mut(key) {
                        center.x += delta.x;
                        center.y += delta.y;
                    }
                }
                moved = true;
            }
            y += row_height + GAP;
            remaining = rest;
        }
        moved
    }

    pub fn wake(&mut self) {
        self.ticks = 0;
    }

    pub fn drag(&mut self, key: &SessionKey, point: Point) {
        if point.finite() && self.positions.contains_key(key) {
            self.positions.insert(key.clone(), point);
            self.velocity.remove(key);
            self.wake();
        }
    }

    /// Bounded fixed steps, with damping and a hard convergence budget. Never call
    /// when hidden or reduced motion is enabled. Return true while still moving.
    pub fn step(&mut self, lineage: &Lineage, held: Option<&SessionKey>) -> bool {
        if self.ticks >= 300 || lineage.is_empty() {
            return false;
        }
        self.ticks += 1;
        let keys: Vec<_> = lineage.nodes().into_iter().collect();
        let mut forces = vec![Point::default(); keys.len()];
        let indices: BTreeMap<_, _> = keys.iter().enumerate().map(|(i, k)| (*k, i)).collect();
        // Local spatial buckets keep repulsion bounded for long histories.
        let mut grid: BTreeMap<(i64, i64), Vec<usize>> = BTreeMap::new();
        for (i, key) in keys.iter().enumerate() {
            let Some(p) = self.positions.get(*key) else {
                continue;
            };
            grid.entry(((p.x / 240.0).floor() as i64, (p.y / 240.0).floor() as i64))
                .or_default()
                .push(i);
            let center = self.centers.get(*key).copied().unwrap_or_default();
            forces[i] = Point {
                x: (center.x - p.x) * 0.0008,
                y: (center.y - p.y) * 0.0008,
            };
        }
        for (i, key) in keys.iter().enumerate() {
            let Some(p) = self.positions.get(*key) else {
                continue;
            };
            let cell = ((p.x / 240.0).floor() as i64, (p.y / 240.0).floor() as i64);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for j in grid
                        .get(&(cell.0.saturating_add(dx), cell.1.saturating_add(dy)))
                        .into_iter()
                        .flatten()
                        .filter(|j| **j > i)
                        .take(64)
                    {
                        let q = self.positions[keys[*j]];
                        let (mut x, y) = (p.x - q.x, p.y - q.y);
                        if x.abs() + y.abs() < 0.01 {
                            x = 1.0;
                        }
                        let d2 = (x * x + y * y).max(100.0);
                        let scale = 650.0 / (d2 * d2.sqrt());
                        forces[i].x += x * scale;
                        forces[i].y += y * scale;
                        forces[*j].x -= x * scale;
                        forces[*j].y -= y * scale;
                    }
                }
            }
        }
        for (parent, child) in lineage.edges() {
            let (Some(p), Some(c)) = (self.positions.get(parent), self.positions.get(child)) else {
                continue;
            };
            let (x, y) = (c.x - p.x, c.y - p.y);
            let distance = (x * x + y * y).sqrt().max(1.0);
            let scale = (distance - 105.0) * 0.008 / distance;
            let (i, j) = (indices[parent], indices[child]);
            forces[i].x += x * scale;
            forces[i].y += y * scale;
            forces[j].x -= x * scale;
            forces[j].y -= y * scale;
        }
        let mut speed: f64 = 0.0;
        for (i, key) in keys.iter().enumerate() {
            if held == Some(*key) {
                continue;
            }
            let Some(p) = self.positions.get_mut(*key) else {
                continue;
            };
            let v = self.velocity.entry((*key).clone()).or_default();
            v.x = ((v.x + forces[i].x) * 0.78).clamp(-12.0, 12.0);
            v.y = ((v.y + forces[i].y) * 0.78).clamp(-12.0, 12.0);
            p.x += v.x;
            p.y += v.y;
            speed = speed.max(v.x.abs() + v.y.abs());
        }
        if speed < 0.04 && self.ticks > 30 {
            self.ticks = 300;
        }
        self.ticks < 300
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(id: &str) -> SessionKey {
        SessionKey {
            provider: "codex".into(),
            home: "/store".into(),
            id: id.into(),
        }
    }
    fn graph() -> Lineage {
        let mut g = Lineage::default();
        for (p, c) in [("a", "b"), ("a", "c"), ("b", "d"), ("x", "y")] {
            g.link(key(p), key(c)).unwrap();
        }
        g
    }
    #[test]
    fn any_family_anchor_in_any_workspace_projects_identical_topology() {
        let g = graph();
        assert_eq!(g.project([&key("d")]), g.project([&key("a"), &key("c")]));
        assert_eq!(g.project([&key("d")]).nodes().len(), 4);
        assert_eq!(g.project([&key("d"), &key("y")]), g);
        assert!(g.project([&key("unknown")]).is_empty());
    }
    #[test]
    fn links_reject_cycles_conflicts_and_cross_store_collisions_atomically() {
        let mut g = graph();
        let before = g.clone();
        assert_eq!(g.link(key("d"), key("a")), Err(LinkError::Cycle));
        assert_eq!(g.link(key("a"), key("a")), Err(LinkError::Cycle));
        assert_eq!(
            g.link(key("x"), key("b")),
            Err(LinkError::ConflictingParent)
        );
        let mut foreign = key("q");
        foreign.home = "/other".into();
        assert_eq!(g.link(foreign, key("q")), Err(LinkError::DifferentStore));
        assert_eq!(g.link(key("a"), key("b")), Ok(()));
        assert_eq!(g, before);
    }
    #[test]
    fn large_forest_layout_keeps_families_separate_and_work_bounded() {
        let started = std::time::Instant::now();
        let mut graph = Lineage::default();
        for family in 0..20 {
            for node in 1..500 {
                graph
                    .link(
                        key(&format!("{family:02}-{previous:03}", previous = node - 1)),
                        key(&format!("{family:02}-{node:03}")),
                    )
                    .unwrap();
            }
        }
        let families = graph.families();
        assert_eq!(families.len(), 20);
        assert!(families.iter().all(|family| family.len() == 500));
        let mut layout = Layout::default();
        layout.sync(&graph);
        let mut previous_max = f64::NEG_INFINITY;
        for family in families {
            let min = family
                .iter()
                .map(|key| layout.positions[key].x)
                .fold(f64::INFINITY, f64::min);
            let max = family
                .iter()
                .map(|key| layout.positions[key].x)
                .fold(f64::NEG_INFINITY, f64::max);
            assert!(min > previous_max);
            previous_max = max;
        }
        layout.step(&graph, None);
        assert!(layout.positions.values().all(|point| point.finite()));
        assert_eq!(layout.positions.len(), 10_000);
        eprintln!("SESSION_FLOW_10000_NODES elapsed={:?}", started.elapsed());
    }

    #[test]
    fn zoom_keeps_cursor_world_position_and_clamps_invalid_input() {
        let mut camera = Camera::default();
        let cursor = Point { x: 123.0, y: 85.0 };
        let before = camera.world(cursor);
        camera.zoom_at(cursor, 2.0);
        assert_eq!(camera.world(cursor), before);
        camera.zoom_at(cursor, 1000.0);
        assert_eq!(camera.zoom, 4.0);
        camera.zoom_at(cursor, 0.0001);
        assert_eq!(camera.zoom, 0.1);
        let valid = camera;
        camera.zoom_at(cursor, f64::NAN);
        assert_eq!(camera, valid);
    }
    #[test]
    fn fit_handles_empty_singleton_and_nonfinite_positions() {
        let mut c = Camera::default();
        c.fit([].into_iter(), 800.0, 300.0);
        assert_eq!(c, Camera::default());
        c.fit(
            [
                Point { x: 99.0, y: -20.0 },
                Point {
                    x: f64::NAN,
                    y: 0.0,
                },
            ]
            .into_iter(),
            800.0,
            300.0,
        );
        assert_eq!(
            c.screen(Point { x: 99.0, y: -20.0 }),
            Point { x: 400.0, y: 150.0 }
        );
    }

    #[test]
    fn packing_saved_families_closes_hidden_gaps_and_preserves_links() {
        let mut all = Lineage::default();
        let mut layout = Layout::default();
        for i in 0..9 {
            let parent = key(&format!("{i}-parent"));
            let child = key(&format!("{i}-child"));
            all.link(parent.clone(), child.clone()).unwrap();
            let x = f64::from(i) * 1500.0;
            layout.positions.insert(parent, Point { x, y: 0.0 });
            layout.positions.insert(
                child,
                Point {
                    x: x + 105.0,
                    y: 15.0,
                },
            );
        }
        let visible = all.project([&key("0-parent"), &key("7-parent"), &key("8-parent")]);
        let hidden = layout.positions[&key("1-parent")];
        assert!(layout.pack(&visible, 4.0));
        assert!(!layout.pack(&visible, 4.0));
        assert_eq!(layout.positions[&key("1-parent")], hidden);
        for (parent, child) in visible.edges() {
            let (p, c) = (layout.positions[parent], layout.positions[child]);
            assert_eq!((c.x - p.x, c.y - p.y), (105.0, 15.0));
        }
        let points = || visible.nodes().into_iter().map(|key| layout.positions[key]);
        let (min, max) = point_bounds(points()).unwrap();
        assert_eq!(max.x - min.x, 3.0 * 105.0 + 2.0 * 90.0);
        let mut camera = Camera::default();
        camera.fit(points(), 1000.0, 250.0);
        let (min, max) = (camera.screen(min), camera.screen(max));
        assert!((min.x + max.x - 1000.0).abs() < 0.01);
        assert!((min.y + max.y - 250.0).abs() < 0.01);
        assert!(min.x > 0.0 && max.x < 1000.0);

        assert!(layout.pack(&visible, 0.7));
        let (min, max) =
            point_bounds(visible.nodes().into_iter().map(|key| layout.positions[key])).unwrap();
        assert_eq!(max.x - min.x, 105.0);
        assert_eq!(max.y - min.y, 3.0 * 15.0 + 2.0 * 90.0);
    }
    #[test]
    fn shared_geometry_is_deterministic_dragged_nodes_stay_and_simulation_stops() {
        let g = graph().project([&key("d")]);
        let mut a = Layout::default();
        let mut b = Layout::default();
        a.sync(&g);
        b.sync(&graph().project([&key("a")]));
        assert_eq!(a.positions, b.positions);
        let held = Point { x: 0.0, y: 0.0 };
        a.drag(&key("a"), held);
        for _ in 0..400 {
            a.step(&g, Some(&key("a")));
        }
        assert_eq!(a.positions[&key("a")], held);
        assert!(!a.step(&g, None));
        assert!(a.positions.values().all(|p| p.finite()));
        let before = a.positions.clone();
        a.drag(
            &key("a"),
            Point {
                x: f64::INFINITY,
                y: 1.0,
            },
        );
        assert_eq!(a.positions, before);
    }
    #[test]
    fn long_families_project_without_recursion_and_coincident_nodes_separate() {
        let mut g = Lineage::default();
        for i in 0..1200 {
            g.link(key(&i.to_string()), key(&(i + 1).to_string()))
                .unwrap();
        }
        assert_eq!(g.project([&key("600")]).nodes().len(), 1201);
        let mut layout = Layout::default();
        layout.sync(&graph());
        for p in layout.positions.values_mut() {
            *p = Point::default();
        }
        assert!(layout.step(&graph(), None));
        assert_ne!(layout.positions[&key("a")], layout.positions[&key("b")]);
    }
}
