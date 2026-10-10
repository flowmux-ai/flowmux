// SPDX-License-Identifier: GPL-3.0-or-later
//! Furnished floor plans in art pixels: work and social blocks, walls, door,
//! decoration that fills spare floor, and walkable routes between them.

use super::theme::{self, Clutter, Decor, Feature, Plant};
use flowmux_core::AgentStatus;
use std::collections::VecDeque;

/// Height of the back wall; the floor starts below it.
pub(super) const WALL: f64 = 44.0;
/// The front wall band that holds the door.
pub(super) const FRONT: f64 = 10.0;
pub(super) const SIDE: f64 = 6.0;
const PAD: f64 = 10.0;
const AISLE: f64 = 18.0;
const GAP: f64 = 20.0;
const CELL: f64 = 48.0;
const ROW: f64 = 80.0;
const POD: f64 = 136.0;
const GRID: f64 = 8.0;
/// Depth reserved for wall-side furniture when a room has spare floor.
const STRIP: f64 = 30.0;
const SIDE_STRIP: f64 = 24.0;
/// Rooms never enlarge art beyond this many logical pixels per art pixel.
pub(super) const MAX_SCALE: f64 = 4.0;
/// Every office keeps this shape, so its floor plan never depends on the window.
pub(super) const ASPECT: f64 = 16.0 / 9.0;
/// Logical height of the office name bar above each plan.
pub(super) const TITLE: f64 = 28.0;

#[cfg(test)]
thread_local! {
    pub(super) static PLAN_BUILDS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
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

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w
            && other.x < self.x + self.w
            && self.y < other.y + other.h
            && other.y < self.y + self.h
    }

    fn inflate(self, by: f64) -> Self {
        Self {
            x: self.x - by,
            y: self.y - by,
            w: self.w + by * 2.0,
            h: self.h + by * 2.0,
        }
    }
}

/// Space between neighbouring offices on the map.
const SPACING: f64 = 4.0;

/// Lowest-then-leftmost spot for a `w` x `h` box on `skyline` (x, width, top
/// segments) within `width` x `height`.
fn skyline_spot(
    skyline: &[(f64, f64, f64)],
    w: f64,
    h: f64,
    width: f64,
    height: f64,
) -> Option<(f64, f64)> {
    let mut best: Option<(f64, f64)> = None;
    for &(x, _, _) in skyline {
        if x + w > width + 1e-6 {
            continue;
        }
        let top = skyline
            .iter()
            .filter(|(sx, sw, _)| *sx < x + w - 1e-6 && sx + sw > x + 1e-6)
            .map(|s| s.2)
            .fold(0.0, f64::max);
        if top + h <= height + 1e-6
            && best.is_none_or(|(bx, by)| top < by - 1e-6 || (top < by + 1e-6 && x < bx))
        {
            best = Some((x, top));
        }
    }
    best
}

fn raise(skyline: &mut Vec<(f64, f64, f64)>, x: f64, w: f64, top: f64) {
    let mut next = Vec::with_capacity(skyline.len() + 2);
    for &(sx, sw, sy) in skyline.iter() {
        let end = sx + sw;
        if end <= x + 1e-6 || sx >= x + w - 1e-6 {
            next.push((sx, sw, sy));
            continue;
        }
        if sx < x {
            next.push((sx, x - sx, sy));
        }
        if end > x + w {
            next.push((x + w, end - x - w, sy));
        }
    }
    next.push((x, w, top));
    next.sort_by(|a, b| a.0.total_cmp(&b.0));
    *skyline = next;
}

/// Places offices of fixed plan sizes at one shared scale, the largest that
/// fits `bounds`, so every office keeps its proportions and its people match
/// the size in every other office. Larger offices go first and smaller ones
/// stack beside them. Returns the scale and each office's rectangle, title
/// bar included, in the order of `sizes`.
pub(super) fn arrange(sizes: &[(f64, f64)], bounds: Rect) -> (f64, Vec<Rect>) {
    if sizes.is_empty() || bounds.w <= 0.0 || bounds.h <= 0.0 {
        return (1.0, vec![Rect::default(); sizes.len()]);
    }
    let mut order: Vec<usize> = (0..sizes.len()).collect();
    order.sort_by(|a, b| {
        (sizes[*b].0 * sizes[*b].1)
            .total_cmp(&(sizes[*a].0 * sizes[*a].1))
            .then(a.cmp(b))
    });
    let tile = |(w, h): (f64, f64), scale: f64| (w * scale + 2.0, h * scale + TITLE + 2.0);
    let pack = |scale: f64| -> Option<Vec<Rect>> {
        let mut skyline = vec![(0.0, bounds.w + SPACING, 0.0)];
        let mut rects = vec![Rect::default(); sizes.len()];
        for &index in &order {
            let (w, h) = tile(sizes[index], scale);
            let (x, y) = skyline_spot(
                &skyline,
                w + SPACING,
                h + SPACING,
                bounds.w + SPACING,
                bounds.h + SPACING,
            )?;
            raise(&mut skyline, x, w + SPACING, y + h + SPACING);
            rects[index] = Rect { x, y, w, h };
        }
        Some(rects)
    };
    let (mut low, mut high) = (0.02, MAX_SCALE);
    if pack(high).is_some() {
        low = high;
    }
    for _ in 0..40 {
        let middle = (low + high) / 2.0;
        if pack(middle).is_some() {
            low = middle;
        } else {
            high = middle;
        }
    }
    let scale = low;
    let Some(mut rects) = pack(scale) else {
        return (scale, vec![Rect::default(); sizes.len()]);
    };
    // Centre the whole arrangement in the window.
    let right = rects.iter().map(|r| r.x + r.w).fold(0.0, f64::max);
    let bottom = rects.iter().map(|r| r.y + r.h).fold(0.0, f64::max);
    let (dx, dy) = ((bounds.w - right) / 2.0, (bounds.h - bottom) / 2.0);
    for r in &mut rects {
        r.x = (bounds.x + r.x + dx).round();
        r.y = (bounds.y + r.y + dy).round();
    }
    (scale, rects)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Kind {
    /// `down` desks face the viewer; their occupant sits behind them.
    Desk {
        down: bool,
    },
    Chair {
        down: bool,
    },
    Sofa,
    Armchair,
    CoffeeTable,
    BeanBag,
    SideTable,
    Rug {
        w: i32,
        h: i32,
        round: bool,
    },
    Counter {
        w: i32,
    },
    Fridge,
    WaterCooler,
    Printer,
    Feature(Feature),
    Plant(Plant),
    Clutter(Clutter),
    Decor(Decor),
    Window {
        w: i32,
    },
    Clock,
    Whiteboard,
    Door,
    Doormat,
}

impl Kind {
    pub fn size(self) -> (i32, i32) {
        match self {
            Kind::Desk { down: false } => (40, 28),
            Kind::Desk { down: true } => (40, 26),
            Kind::Chair { .. } => (18, 18),
            Kind::Sofa => (64, 30),
            Kind::Armchair => (24, 28),
            Kind::CoffeeTable => (40, 16),
            Kind::BeanBag => (22, 18),
            Kind::SideTable => (12, 14),
            Kind::Rug { w, h, .. } => (w, h),
            Kind::Counter { w } => (w, 36),
            Kind::Fridge => (24, 40),
            Kind::WaterCooler => (14, 30),
            Kind::Printer => (26, 22),
            Kind::Feature(_) => (48, 48),
            Kind::Plant(_) => (22, 34),
            Kind::Clutter(_) => (18, 24),
            Kind::Decor(_) => (32, 26),
            Kind::Window { w } => (w, 30),
            Kind::Clock => (14, 14),
            Kind::Whiteboard => (48, 28),
            Kind::Door => (30, 12),
            Kind::Doormat => (28, 10),
        }
    }

    /// Lower part of the sprite that blocks walking.
    fn footprint(self) -> Option<(f64, f64, f64, f64)> {
        match self {
            Kind::Desk { down: false } => Some((0.0, 4.0, 40.0, 22.0)),
            Kind::Desk { down: true } => Some((0.0, 4.0, 40.0, 20.0)),
            Kind::Sofa => Some((2.0, 2.0, 60.0, 16.0)),
            Kind::Armchair => Some((2.0, 2.0, 20.0, 12.0)),
            Kind::CoffeeTable => Some((0.0, 2.0, 40.0, 12.0)),
            Kind::SideTable => Some((0.0, 6.0, 12.0, 8.0)),
            Kind::Counter { w } => Some((0.0, 14.0, w as f64, 20.0)),
            Kind::Fridge => Some((0.0, 14.0, 24.0, 24.0)),
            Kind::WaterCooler => Some((0.0, 18.0, 14.0, 10.0)),
            Kind::Printer => Some((0.0, 8.0, 26.0, 12.0)),
            Kind::Feature(_) => Some((2.0, 30.0, 44.0, 16.0)),
            Kind::Plant(_) => Some((4.0, 24.0, 14.0, 9.0)),
            Kind::Clutter(_) => Some((2.0, 14.0, 14.0, 9.0)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layer {
    /// Painted into the cached background beneath everything.
    Floor,
    /// Mounted on the back wall; also cached.
    Wall,
    /// Depth-sorted with the characters by `sort`.
    Stand,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Item {
    pub kind: Kind,
    pub x: f64,
    pub y: f64,
    pub sort: f64,
    pub layer: Layer,
    pub variant: usize,
    /// Desk slot for monitors and seats that animate with their occupant.
    pub slot: Option<usize>,
}

impl Item {
    fn new(kind: Kind, x: f64, y: f64, variant: usize) -> Self {
        let (_, h) = kind.size();
        let layer = match kind {
            Kind::Rug { .. } | Kind::Doormat => Layer::Floor,
            Kind::Decor(_) | Kind::Window { .. } | Kind::Clock | Kind::Whiteboard => Layer::Wall,
            _ => Layer::Stand,
        };
        Self {
            kind,
            x: x.round(),
            y: y.round(),
            sort: (y + h as f64).round(),
            layer,
            variant,
            slot: None,
        }
    }

    pub fn rect(&self) -> Rect {
        let (w, h) = self.kind.size();
        Rect {
            x: self.x,
            y: self.y,
            w: w as f64,
            h: h as f64,
        }
    }

    fn block(&self) -> Option<Rect> {
        self.kind.footprint().map(|(x, y, w, h)| Rect {
            x: self.x + x,
            y: self.y + y,
            w,
            h,
        })
    }
}

/// What a resting teammate does at a point of interest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Activity {
    Coffee,
    Browse,
    Tend,
    Gaze,
    Present,
    Snack,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Poi {
    pub point: (f64, f64),
    pub activity: Activity,
}

#[derive(Clone, Copy)]
enum Family {
    Rows,
    Pods,
    Islands,
    Commons,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Unit {
    Lounge,
    Beans,
    Pantry,
    Feature(usize),
    Greenery,
}

impl Unit {
    fn size(self) -> (f64, f64) {
        match self {
            Unit::Lounge => (128.0, 92.0),
            Unit::Beans => (76.0, 60.0),
            Unit::Pantry => (124.0, 64.0),
            Unit::Feature(_) => (60.0, 66.0),
            Unit::Greenery => (52.0, 44.0),
        }
    }

    fn seats(self) -> usize {
        match self {
            Unit::Lounge => 5,
            Unit::Beans => 2,
            _ => 0,
        }
    }
}

#[derive(Clone)]
enum Block {
    Work { first: usize, count: usize },
    Social,
}

#[derive(Clone, Copy, Debug)]
struct Shape {
    horizontal: bool,
    cols: usize,
    social_cols: usize,
}

pub(super) struct Plan {
    pub width: f64,
    pub height: f64,
    pub capacity: usize,
    pub design: usize,
    pub items: Vec<Item>,
    pub pois: Vec<Poi>,
    /// Coffee tables where shared snacks are set down.
    pub tables: Vec<(f64, f64)>,
    /// Floor point in front of the whiteboard for stand-ups.
    pub board: (f64, f64),
    door: (f64, f64),
    desks: Vec<(f64, f64)>,
    south: Vec<bool>,
    blocked: Vec<(f64, f64)>,
    seats: Vec<(f64, f64)>,
    walkable: Vec<bool>,
    grid_width: usize,
}

fn family(design: usize) -> Family {
    match design % theme::LAYOUTS {
        0 => Family::Rows,
        1 => Family::Pods,
        2 => Family::Islands,
        _ => Family::Commons,
    }
}

fn blocks(count: usize, design: usize) -> Vec<Block> {
    match family(design) {
        Family::Rows | Family::Pods => vec![Block::Work { first: 0, count }, Block::Social],
        Family::Islands => vec![Block::Social, Block::Work { first: 0, count }],
        Family::Commons => {
            let left = count.div_ceil(2);
            let mut blocks = vec![Block::Work {
                first: 0,
                count: left,
            }];
            blocks.push(Block::Social);
            if count > left {
                blocks.push(Block::Work {
                    first: left,
                    count: count - left,
                });
            }
            blocks
        }
    }
}

fn pods(count: usize, design: usize) -> bool {
    count >= 4 && matches!(family(design), Family::Pods | Family::Islands)
}

fn work_size(count: usize, cols: usize, design: usize) -> (f64, f64) {
    let cols = cols.min(count).max(1);
    let islands = matches!(family(design), Family::Islands);
    let gaps = if islands { (cols - 1) / 2 } else { 0 };
    let w = cols as f64 * CELL + gaps as f64 * 16.0;
    let h = if pods(count, design) {
        count.div_ceil(cols * 2) as f64 * POD
    } else {
        count.div_ceil(cols) as f64 * ROW
    };
    (w, h)
}

fn social_units(capacity: usize) -> Vec<Unit> {
    let mut units = Vec::new();
    let mut seats = 0;
    while seats < capacity {
        let unit = if capacity - seats > 2 {
            Unit::Lounge
        } else {
            Unit::Beans
        };
        seats += unit.seats();
        units.push(unit);
    }
    units.push(Unit::Pantry);
    units.push(Unit::Feature(0));
    units
}

/// Shelf-packs units into rows no wider than `width`; returns positions and total size.
fn pack(units: &[Unit], width: f64) -> Option<(Vec<(f64, f64)>, f64, f64)> {
    let mut positions = Vec::with_capacity(units.len());
    let (mut x, mut y, mut row_h, mut max_w) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f64);
    for unit in units {
        let (w, h) = unit.size();
        if w > width + 0.01 {
            return None;
        }
        if x > 0.0 && x + w > width + 0.01 {
            y += row_h + 12.0;
            x = 0.0;
            row_h = 0.0;
        }
        positions.push((x, y));
        x += w + 12.0;
        max_w = max_w.max(x - 12.0);
        row_h = row_h.max(h);
    }
    Some((positions, max_w, y + row_h))
}

fn social_size(units: &[Unit], cols: usize) -> (f64, f64) {
    // Column count picks a target width; rows wrap greedily under it.
    let widest = units.iter().map(|u| u.size().0).fold(0.0, f64::max);
    let target = (cols as f64 * 132.0).max(widest);
    let (_, w, h) = pack(units, target).unwrap();
    (w, h)
}

fn block_size(block: &Block, shape: &Shape, units: &[Unit], design: usize) -> (f64, f64) {
    match block {
        Block::Work { count, .. } => work_size(*count, shape.cols, design),
        Block::Social => social_size(units, shape.social_cols),
    }
}

fn content(blocks: &[Block], shape: &Shape, units: &[Unit], design: usize) -> (f64, f64) {
    let sizes: Vec<_> = blocks
        .iter()
        .map(|b| block_size(b, shape, units, design))
        .collect();
    let gaps = GAP * (sizes.len() - 1) as f64;
    let (w, h) = if shape.horizontal {
        (
            sizes.iter().map(|s| s.0).sum::<f64>() + gaps,
            sizes.iter().map(|s| s.1).fold(0.0, f64::max),
        )
    } else {
        (
            sizes.iter().map(|s| s.0).fold(0.0, f64::max),
            sizes.iter().map(|s| s.1).sum::<f64>() + gaps,
        )
    };
    (w + 2.0 * (SIDE + PAD), h + WALL + PAD + AISLE + FRONT)
}

fn best_shape(capacity: usize, design: usize, target_w: f64, target_h: f64) -> (Shape, f64, f64) {
    let blocks = blocks(capacity, design);
    let units = social_units(capacity);
    let widest = blocks
        .iter()
        .filter_map(|b| match b {
            Block::Work { count, .. } => Some(*count),
            Block::Social => None,
        })
        .max()
        .unwrap_or(1);
    let mut best: Option<(f64, f64, Shape, f64, f64)> = None;
    for horizontal in [true, false] {
        for cols in 1..=widest.max(1) {
            for social_cols in 1..=units.len().min(6) {
                let shape = Shape {
                    horizontal,
                    cols,
                    social_cols,
                };
                let (w, h) = content(&blocks, &shape, &units, design);
                let scale = (target_w / w).min(target_h / h);
                // Near-equal scales prefer the arrangement whose shape matches the room.
                let mismatch = ((w / h) / (target_w / target_h)).ln().abs();
                if best.as_ref().is_none_or(|b| {
                    scale > b.0 * 1.03 || (scale > b.0 / 1.03 && mismatch < b.1 - 1e-9)
                }) {
                    best = Some((scale, mismatch, shape, w, h));
                }
            }
        }
    }
    let (_, _, shape, w, h) = best.unwrap();
    (shape, w, h)
}

impl Plan {
    /// Smallest plan for `count` seats, enlarged to `aspect`.
    pub fn fit(count: usize, design: usize, aspect: f64) -> Self {
        #[cfg(test)]
        PLAN_BUILDS.with(|builds| builds.set(builds.get() + 1));
        let aspect = if aspect.is_finite() && aspect > 0.0 {
            aspect
        } else {
            1.6
        };
        let capacity = count.max(1);
        let (shape, w, h) = best_shape(capacity, design, aspect * 1000.0, 1000.0);
        let (w, h) = if w / h < aspect {
            (h * aspect, h)
        } else {
            (w, w / aspect)
        };
        Self::build(capacity, design, shape, w, h)
    }

    fn build(capacity: usize, design: usize, shape: Shape, width: f64, height: f64) -> Self {
        let theme = theme::theme(design);
        let blocks = blocks(capacity, design);
        let mut units = social_units(capacity);
        let mut plan = Self {
            width,
            height,
            capacity,
            design,
            items: Vec::new(),
            pois: Vec::new(),
            tables: Vec::new(),
            board: (width / 2.0, WALL + 12.0),
            door: (width / 2.0, height - FRONT / 2.0),
            desks: vec![(0.0, 0.0); capacity],
            south: vec![false; capacity],
            blocked: vec![(0.0, 0.0); capacity],
            seats: Vec::with_capacity(capacity),
            walkable: Vec::new(),
            grid_width: 0,
        };
        let sizes: Vec<_> = blocks
            .iter()
            .map(|b| block_size(b, &shape, &units, design))
            .collect();
        let (needed_w, needed_h) = content(&blocks, &shape, &units, design);
        // Spare room first becomes a strip of furniture along the back and side walls.
        let back = if height - needed_h >= STRIP + 8.0 {
            STRIP
        } else {
            0.0
        };
        let sides = if width - needed_w >= 2.0 * SIDE_STRIP + 16.0 {
            SIDE_STRIP
        } else {
            0.0
        };
        // Spread each block over its share of the floor along the main axis.
        let floor = Rect {
            x: SIDE + PAD + sides,
            y: WALL + PAD + back,
            w: width - 2.0 * (SIDE + PAD + sides),
            h: height - WALL - PAD - back - AISLE - FRONT,
        };
        let gaps = GAP * (sizes.len() - 1) as f64;
        let main: f64 = if shape.horizontal {
            sizes.iter().map(|s| s.0).sum()
        } else {
            sizes.iter().map(|s| s.1).sum()
        };
        let room = if shape.horizontal { floor.w } else { floor.h } - gaps;
        let mut cursor = if shape.horizontal { floor.x } else { floor.y };
        let mut rects = Vec::new();
        for size in &sizes {
            let share = if shape.horizontal { size.0 } else { size.1 } / main * room;
            rects.push(if shape.horizontal {
                Rect {
                    x: cursor,
                    y: floor.y,
                    w: share,
                    h: floor.h,
                }
            } else {
                Rect {
                    x: floor.x,
                    y: cursor,
                    w: floor.w,
                    h: share,
                }
            });
            cursor += share + GAP;
        }
        for (block, rect) in blocks.iter().zip(&rects) {
            match block {
                Block::Work { first, count } => {
                    plan.furnish_work(*first, *count, shape.cols, *rect)
                }
                Block::Social => plan.furnish_social(&mut units, *rect, theme),
            }
        }
        plan.door = if shape.horizontal && rects.len() > 1 {
            (
                (rects[0].x + rects[0].w + GAP / 2.0).round(),
                height - FRONT / 2.0,
            )
        } else if matches!(family(design), Family::Rows | Family::Pods) {
            ((width - 40.0).round(), height - FRONT / 2.0)
        } else {
            (40.0, height - FRONT / 2.0)
        };
        let (door_x, _) = plan.door;
        plan.items.push(Item::new(
            Kind::Door,
            door_x - 15.0,
            height - FRONT - 2.0,
            0,
        ));
        plan.items.push(Item::new(
            Kind::Doormat,
            door_x - 14.0,
            height - FRONT - 12.0,
            0,
        ));
        plan.decorate_wall(theme);
        plan.grid();
        plan.line_walls(theme);
        plan.fill_gaps(theme);
        plan.line_front(theme);
        plan
    }

    fn furnish_work(&mut self, first: usize, count: usize, cols: usize, rect: Rect) {
        let design = self.design;
        let pods = pods(self.capacity, design) && count >= 2;
        let cols = cols.min(count).max(1);
        let (w, _) = work_size(count, cols, design);
        let rows = if pods {
            count.div_ceil(cols * 2)
        } else {
            count.div_ceil(cols)
        };
        let unit = if pods { POD } else { ROW };
        // Spare height loosens rows a little before the remainder becomes margin.
        let pitch = (rect.h / rows as f64).clamp(unit, unit + 24.0);
        let pitch = snap(pitch);
        let left = snap(rect.x + (rect.w - w) / 2.0);
        let top = snap(rect.y + (rect.h - pitch * rows as f64) / 2.0 + (pitch - unit) / 2.0);
        let islands = matches!(family(design), Family::Islands);
        let column_x = |col: usize| {
            left + col as f64 * CELL
                + if islands {
                    (col / 2) as f64 * 16.0
                } else {
                    0.0
                }
        };
        for local in 0..count {
            let slot = first + local;
            let (col, row_top, down) = if pods {
                let pod = local / (cols * 2);
                let index = local % (cols * 2);
                // Rear-facing seats fill first, so their screens stay visible.
                (index % cols, top + pod as f64 * pitch, index >= cols)
            } else {
                (local % cols, top + (local / cols) as f64 * pitch, false)
            };
            let x = column_x(col);
            let variant = slot * 7 + design;
            let (desk, feet, chair, blocked) = if !pods {
                (
                    (x + 4.0, row_top + 8.0),
                    (x + 24.0, row_top + 56.0),
                    (x + 15.0, row_top + 46.0),
                    (x + 24.0, row_top + 72.0),
                )
            } else if down {
                (
                    (x + 4.0, row_top + 42.0),
                    (x + 24.0, row_top + 40.0),
                    (x + 15.0, row_top + 18.0),
                    (x + 24.0, row_top + 16.0),
                )
            } else {
                (
                    (x + 4.0, row_top + 66.0),
                    (x + 24.0, row_top + 112.0),
                    (x + 15.0, row_top + 102.0),
                    (x + 24.0, row_top + 128.0),
                )
            };
            let mut item = Item::new(Kind::Desk { down }, desk.0, desk.1, variant);
            item.slot = Some(slot);
            self.items.push(item);
            let mut seat = Item::new(Kind::Chair { down }, chair.0, chair.1, variant);
            // A rear-view chair back covers the seated occupant; a front-view one sits behind.
            seat.sort = if down { feet.1 - 1.0 } else { feet.1 + 1.0 };
            seat.slot = Some(slot);
            self.items.push(seat);
            self.desks[slot] = feet;
            self.south[slot] = down;
            self.blocked[slot] = blocked;
        }
        // Row ends get planters when the block has spare width.
        let margin = (w - rect.w).abs() / 2.0;
        if margin >= 28.0 {
            let plants = theme::theme(design).plants;
            for row in 0..rows {
                let y = top + row as f64 * pitch + if pods { 30.0 } else { 6.0 };
                for (side, x) in [(0, left - 26.0), (1, left + w + 4.0)] {
                    self.items.push(Item::new(
                        Kind::Plant(plants[(row + side) % 2]),
                        x,
                        y,
                        row + side,
                    ));
                }
            }
        }
    }

    fn furnish_social(&mut self, units: &mut Vec<Unit>, rect: Rect, theme: &theme::Theme) {
        // Grow the social block with more focal pieces and greenery while they fit.
        let base = units.len();
        // At most three focal pieces per room keep them special; greenery fills the rest.
        for extra in 0..10 {
            let next = match extra {
                0 => Unit::Feature(1),
                3 => Unit::Feature(0),
                _ => Unit::Greenery,
            };
            units.push(next);
            let fits = pack(units, rect.w).is_some_and(|(_, _, h)| h <= rect.h);
            if !fits {
                units.pop();
                break;
            }
        }
        let _ = base;
        let (positions, w, h) = pack(units, rect.w).unwrap_or_else(|| {
            let (_, w, h) = pack(units, f64::INFINITY).unwrap();
            (pack(units, f64::INFINITY).unwrap().0, w, h)
        });
        // Rows are centred individually so short rows do not hug the left edge.
        let mut rows: Vec<(f64, f64, usize, usize)> = Vec::new();
        for (index, (x, y)) in positions.iter().enumerate() {
            let right = x + units[index].size().0;
            match rows.last_mut() {
                Some(row) if row.0 == *y => {
                    row.1 = row.1.max(right);
                    row.3 = index + 1;
                }
                _ => rows.push((*y, right, index, index + 1)),
            }
        }
        let top = rect.y + ((rect.h - h) / 2.0).max(0.0);
        let _ = w;
        let mut feature_index = 0;
        for (y, row_w, start, end) in rows {
            let left = rect.x + (rect.w - row_w) / 2.0;
            for index in start..end {
                let (x, _) = positions[index];
                let origin = (snap(left + x), snap(top + y));
                let unit = units[index];
                let variant = match unit {
                    Unit::Feature(_) => {
                        feature_index += 1;
                        feature_index
                    }
                    _ => index,
                };
                self.place_unit(unit, origin, theme, variant);
            }
        }
    }

    fn place_unit(&mut self, unit: Unit, (x, y): (f64, f64), theme: &theme::Theme, variant: usize) {
        let push = |plan: &mut Self, kind, dx: f64, dy: f64, v| {
            plan.items.push(Item::new(kind, x + dx, y + dy, v));
        };
        match unit {
            Unit::Lounge => {
                push(
                    self,
                    Kind::Rug {
                        w: 116,
                        h: 70,
                        round: false,
                    },
                    6.0,
                    20.0,
                    variant,
                );
                push(self, Kind::Sofa, 32.0, 10.0, variant);
                push(self, Kind::Armchair, 4.0, 30.0, variant);
                push(self, Kind::Armchair, 100.0, 30.0, variant + 1);
                push(self, Kind::CoffeeTable, 44.0, 62.0, variant);
                push(self, Kind::Clutter(Clutter::FloorLamp), 110.0, 2.0, variant);
                for dx in [48.0, 64.0, 80.0] {
                    self.seats.push((x + dx, y + 48.0));
                }
                self.seats.push((x + 16.0, y + 64.0));
                self.seats.push((x + 112.0, y + 64.0));
                self.tables.push((x + 64.0, y + 70.0));
            }
            Unit::Beans => {
                push(
                    self,
                    Kind::Rug {
                        w: 64,
                        h: 40,
                        round: true,
                    },
                    6.0,
                    16.0,
                    variant,
                );
                push(self, Kind::BeanBag, 13.0, 22.0, variant);
                push(self, Kind::BeanBag, 45.0, 22.0, variant + 1);
                push(self, Kind::SideTable, 34.0, 16.0, variant);
                self.seats.push((x + 24.0, y + 40.0));
                self.seats.push((x + 56.0, y + 40.0));
                self.tables.push((x + 38.0, y + 30.0));
            }
            Unit::Pantry => {
                push(self, Kind::Counter { w: 70 }, 0.0, 4.0, variant);
                push(self, Kind::Fridge, 74.0, 0.0, variant);
                push(self, Kind::WaterCooler, 104.0, 10.0, variant);
                self.pois.push(Poi {
                    point: (x + 24.0, y + 56.0),
                    activity: Activity::Coffee,
                });
                self.pois.push(Poi {
                    point: (x + 88.0, y + 56.0),
                    activity: Activity::Browse,
                });
                self.pois.push(Poi {
                    point: (x + 112.0, y + 56.0),
                    activity: Activity::Coffee,
                });
            }
            Unit::Feature(index) => {
                let feature = theme.features[index];
                push(self, Kind::Feature(feature), 6.0, 2.0, variant);
                self.pois.push(Poi {
                    point: (x + 32.0, y + 64.0),
                    activity: activity(feature),
                });
            }
            Unit::Greenery => {
                push(
                    self,
                    Kind::Plant(theme.plants[variant % 2]),
                    2.0,
                    6.0,
                    variant,
                );
                if variant % 3 == 1 {
                    push(self, Kind::Printer, 24.0, 18.0, variant);
                } else {
                    push(
                        self,
                        Kind::Clutter(theme.clutter[variant % 2]),
                        28.0,
                        18.0,
                        variant,
                    );
                }
                self.pois.push(Poi {
                    point: (x + 16.0, y + 48.0),
                    activity: Activity::Tend,
                });
            }
        }
    }

    fn decorate_wall(&mut self, theme: &theme::Theme) {
        let span = self.width - 2.0 * SIDE - 12.0;
        let window_w = if span > 360.0 { 72 } else { 56 };
        // A repeating rhythm of windows and decor, with the whiteboard and clock near the centre.
        let rhythm = [
            Kind::Window { w: window_w },
            Kind::Decor(theme.decor[0]),
            Kind::Window { w: window_w },
            Kind::Decor(theme.decor[1]),
        ];
        let width = |pieces: &[Kind]| {
            pieces.iter().map(|k| k.size().0 as f64).sum::<f64>()
                + 14.0 * pieces.len().saturating_sub(1) as f64
        };
        let mut pieces = vec![Kind::Whiteboard, Kind::Clock];
        let mut index = 0;
        loop {
            let piece = rhythm[index / 2 % rhythm.len()];
            let mut next = pieces.clone();
            // Grow outward on alternating sides to keep the wall balanced.
            if index % 2 == 0 {
                next.push(piece);
            } else {
                next.insert(0, piece);
            }
            if width(&next) > span {
                break;
            }
            pieces = next;
            index += 1;
        }
        if width(&pieces) > span {
            pieces = vec![Kind::Window { w: 40 }];
        }
        // Even spacing across the whole wall rather than a centred cluster.
        let total: f64 = pieces.iter().map(|k| k.size().0 as f64).sum();
        let gap = ((span - total) / (pieces.len() + 1) as f64).max(4.0);
        let mut x = SIDE + 6.0 + gap;
        let mut seen: Vec<Kind> = Vec::new();
        for kind in pieces {
            // Repeated pieces count their copies so neighbours show different artwork.
            let index = seen.iter().filter(|k| **k == kind).count();
            seen.push(kind);
            let (w, h) = kind.size();
            let y = match kind {
                Kind::Window { .. } => 7.0,
                Kind::Clock => 10.0,
                _ => (WALL - 8.0 - h as f64).max(4.0),
            };
            if kind == Kind::Whiteboard {
                self.board = ((x + w as f64 / 2.0).round(), WALL + 14.0);
            }
            self.items.push(Item::new(kind, x.round(), y, index));
            x += w as f64 + gap;
        }
    }

    fn grid(&mut self) {
        self.grid_width = (self.width / GRID) as usize + 1;
        let grid_height = (self.height / GRID) as usize + 1;
        let blocks: Vec<_> = self
            .items
            .iter()
            .filter_map(|item| item.block().map(|r| r.inflate(3.0)))
            .collect();
        let door = self.door;
        let floor_bottom = self.height - FRONT;
        self.walkable = (0..grid_width_cells(self.grid_width, grid_height))
            .map(|index| {
                let x = (index % self.grid_width) as f64 * GRID;
                let y = (index / self.grid_width) as f64 * GRID;
                let inside = x >= SIDE + 4.0
                    && x <= self.width - SIDE - 4.0
                    && y >= WALL + 4.0
                    && y <= floor_bottom - 2.0;
                let doorway = (x - door.0).abs() <= GRID && y >= floor_bottom - GRID;
                (inside || doorway) && !blocks.iter().any(|r| r.contains(x, y))
            })
            .collect();
    }

    /// Key points that must stay reachable from the door.
    fn anchors(&self) -> Vec<(f64, f64)> {
        let mut points = vec![self.door, self.board];
        points.extend(&self.desks);
        points.extend(&self.blocked);
        points.extend(&self.seats);
        points.extend(self.pois.iter().map(|p| p.point));
        points
    }

    fn reachable(&self) -> Vec<bool> {
        let mut seen = vec![false; self.walkable.len()];
        let start = self.cell(self.nearest_walkable(self.door));
        if start >= seen.len() {
            return seen;
        }
        seen[start] = true;
        let mut queue = VecDeque::from([start]);
        while let Some(current) = queue.pop_front() {
            for next in self.neighbors(current) {
                if !seen[next] {
                    seen[next] = true;
                    queue.push_back(next);
                }
            }
        }
        seen
    }

    /// Grid cells that must stay open and reachable, resolved once per decoration pass.
    fn anchor_cells(&self) -> Vec<usize> {
        self.anchors()
            .into_iter()
            .map(|p| self.cell(self.nearest_walkable(p)))
            .collect()
    }

    fn connected(&self, cells: &[usize]) -> bool {
        let seen = self.reachable();
        cells
            .iter()
            .all(|&cell| cell < seen.len() && seen[cell] && self.walkable[cell])
    }

    /// Rectangles that new decoration must stay clear of.
    fn taken(&self) -> Vec<Rect> {
        let mut taken: Vec<Rect> = self
            .items
            .iter()
            .filter(|i| i.layer != Layer::Wall)
            .map(|i| i.rect().inflate(2.0))
            .collect();
        // The doorway and the path just inside it stay open.
        taken.push(Rect {
            x: self.door.0 - 22.0,
            y: self.height - FRONT - 40.0,
            w: 44.0,
            h: 40.0,
        });
        taken
    }

    /// Places `group` if it fits on open floor and every anchor stays reachable.
    fn try_place(&mut self, group: &[Item], taken: &mut Vec<Rect>, cells: &[usize]) -> bool {
        let anchors: Vec<_> = cells.iter().map(|&c| self.point(c)).collect();
        let floor = Rect {
            x: SIDE + 1.0,
            y: WALL - 2.0,
            w: self.width - 2.0 * SIDE - 2.0,
            h: self.height - WALL - FRONT + 1.0,
        };
        for item in group {
            let r = item.rect();
            let inside = r.x >= floor.x
                && r.y >= floor.y
                && r.x + r.w <= floor.x + floor.w
                && r.y + r.h <= floor.y + floor.h;
            let blocking = item.block().is_some();
            if !inside
                || taken.iter().any(|t| t.intersects(&r))
                || (blocking
                    && anchors
                        .iter()
                        .any(|p| item.block().unwrap().inflate(10.0).contains(p.0, p.1)))
            {
                return false;
            }
        }
        let before = self.walkable.clone();
        for block in group.iter().filter_map(Item::block) {
            let block = block.inflate(3.0);
            let (x0, x1) = ((block.x / GRID).ceil() as usize, (block.x + block.w) / GRID);
            let (y0, y1) = ((block.y / GRID).ceil() as usize, (block.y + block.h) / GRID);
            for row in y0..=(y1 as usize) {
                for col in x0..=(x1 as usize) {
                    let index = row * self.grid_width + col;
                    if col < self.grid_width && index < self.walkable.len() {
                        self.walkable[index] = false;
                    }
                }
            }
        }
        if group.iter().any(|i| i.block().is_some()) && !self.connected(cells) {
            self.walkable = before;
            return false;
        }
        for item in group {
            taken.push(item.rect().inflate(3.0));
            self.items.push(*item);
        }
        true
    }

    /// True when no furniture lies within `rect`.
    fn hole(taken: &[Rect], rect: Rect) -> bool {
        !taken.iter().any(|t| t.intersects(&rect))
    }

    /// Wall-side furniture in small clusters with breathing room between them.
    fn line_walls(&mut self, theme: &theme::Theme) {
        let mut taken = self.taken();
        let cells = self.anchor_cells();
        let clusters: [&[Kind]; 4] = [
            &[
                Kind::Plant(theme.plants[0]),
                Kind::Clutter(theme.clutter[0]),
            ],
            &[Kind::Clutter(theme.clutter[1])],
            &[Kind::Clutter(Clutter::Bin), Kind::Plant(theme.plants[1])],
            &[Kind::Clutter(theme.clutter[0])],
        ];
        let mut index = self.design;
        let mut x = SIDE + 4.0;
        while x < self.width - SIDE - 20.0 {
            let cluster = clusters[index % clusters.len()];
            let mut cx = x;
            let group: Vec<_> = cluster
                .iter()
                .enumerate()
                .map(|(i, kind)| {
                    let item = Item::new(*kind, cx, WALL - 1.0 + (i % 2) as f64 * 4.0, index);
                    cx += kind.size().0 as f64 + 2.0;
                    item
                })
                .collect();
            let span = Rect {
                x: x - 12.0,
                y: WALL,
                w: cx - x + 24.0,
                h: 34.0,
            };
            if Self::hole(&taken, span) && self.try_place(&group, &mut taken, &cells) {
                x = cx + 150.0 + (index % 3) as f64 * 40.0;
                index += 1;
            } else {
                x += 8.0;
            }
        }
        for side in [0, 1] {
            let mut y = WALL + 44.0;
            let mut index = self.design + side * 2;
            while y < self.height - FRONT - 36.0 {
                let kind = clusters[(index + 1) % clusters.len()][0];
                let (w, h) = kind.size();
                let x = if side == 0 {
                    SIDE + 2.0
                } else {
                    self.width - SIDE - 2.0 - w as f64
                };
                let item = Item::new(kind, x, y, index);
                let span = item.rect().inflate(12.0);
                if Self::hole(&taken, span) && self.try_place(&[item], &mut taken, &cells) {
                    y += h as f64 + 130.0;
                    index += 1;
                } else {
                    y += 8.0;
                }
            }
        }
    }

    /// Plants and clutter standing against the front wall, clear of the doorway.
    fn line_front(&mut self, theme: &theme::Theme) {
        let mut taken = self.taken();
        let cells = self.anchor_cells();
        let kinds = [
            Kind::Plant(theme.plants[1]),
            Kind::Clutter(theme.clutter[1]),
            Kind::Plant(theme.plants[0]),
            Kind::Clutter(Clutter::Bin),
        ];
        let mut index = self.design + 1;
        let mut x = SIDE + 30.0;
        while x < self.width - SIDE - 40.0 {
            let kind = kinds[index % kinds.len()];
            let (w, h) = kind.size();
            let item = Item::new(kind, x, self.height - FRONT - h as f64 - 1.0, index);
            let span = item.rect().inflate(10.0);
            if Self::hole(&taken, span) && self.try_place(&[item], &mut taken, &cells) {
                x += w as f64 + 170.0 + (index % 2) as f64 * 40.0;
                index += 1;
            } else {
                x += 8.0;
            }
        }
    }

    /// Furnishes large empty stretches with small corners; smaller gaps stay open floor.
    fn fill_gaps(&mut self, theme: &theme::Theme) {
        self.fill_holes(theme, 60.0);
    }

    fn fill_holes(&mut self, theme: &theme::Theme, size: f64) {
        let mut taken = self.taken();
        let cells = self.anchor_cells();
        let mut variant = self.design;
        let mut y = WALL + 4.0;
        while y + size < self.height - FRONT - 8.0 {
            let mut x = SIDE + 4.0;
            while x + size < self.width - SIDE {
                let area = Rect {
                    x: x - 8.0,
                    y: y - 8.0,
                    w: size + 16.0,
                    h: size + 16.0,
                };
                if Self::hole(&taken, area) {
                    let interior = y > WALL + 40.0 && y + size < self.height - FRONT - 48.0;
                    let options = if size < 60.0 && !interior {
                        Vec::new()
                    } else if size < 60.0 {
                        let kind = match variant % 3 {
                            0 => Kind::Plant(theme.plants[variant / 3 % 2]),
                            1 => Kind::Clutter(theme.clutter[variant / 3 % 2]),
                            _ => Kind::Clutter(Clutter::SmallPot),
                        };
                        vec![vec![Item::new(kind, x + 6.0, y, variant)]]
                    } else {
                        nooks(theme, variant, x + 8.0, y + 8.0)
                    };
                    let features = self
                        .items
                        .iter()
                        .filter(|i| matches!(i.kind, Kind::Feature(_)))
                        .count();
                    let options: Vec<_> = options
                        .into_iter()
                        .filter(|group| {
                            features < 4
                                || !group.iter().any(|i| matches!(i.kind, Kind::Feature(_)))
                        })
                        .collect();
                    if options
                        .iter()
                        .any(|group| self.try_place(group, &mut taken, &cells))
                    {
                        variant += 1;
                        x += size * 3.0;
                        continue;
                    }
                }
                x += 8.0;
            }
            y += 8.0;
        }
    }

    pub fn desk(&self, slot: usize) -> (f64, f64) {
        self.desks[slot]
    }

    /// True when the occupant sits behind the desk, facing the viewer.
    pub fn desk_faces_south(&self, slot: usize) -> bool {
        self.south[slot]
    }

    /// The slot's own lounge seat.
    pub fn seat(&self, slot: usize) -> (f64, f64) {
        self.seats[slot % self.seats.len()]
    }

    pub fn door(&self) -> (f64, f64) {
        self.door
    }

    pub fn destination(&self, slot: usize, status: AgentStatus, ended: bool) -> (f64, f64) {
        if ended {
            return self.door;
        }
        match status {
            AgentStatus::Done | AgentStatus::Idle => self.seat(slot),
            AgentStatus::Blocked => self.blocked[slot],
            _ => self.desk(slot),
        }
    }

    /// A point of interest for a resting teammate; phase 0 is the lounge seat.
    pub fn rest_stop(&self, slot: usize, phase: u8) -> ((f64, f64), Option<Activity>) {
        if phase == 0 || self.pois.is_empty() {
            return (self.seat(slot), None);
        }
        let poi = self.pois[(slot * 5 + phase as usize * 3) % self.pois.len()];
        let jitter = (slot % 3) as f64 * 10.0 - 10.0;
        let point = self.nearest_walkable((poi.point.0 + jitter, poi.point.1));
        (point, Some(poi.activity))
    }

    fn cell(&self, (x, y): (f64, f64)) -> usize {
        let col = (x / GRID).round().max(0.0) as usize;
        let row = (y / GRID).round().max(0.0) as usize;
        if col >= self.grid_width {
            return usize::MAX;
        }
        row * self.grid_width + col
    }

    fn point(&self, index: usize) -> (f64, f64) {
        (
            (index % self.grid_width) as f64 * GRID,
            (index / self.grid_width) as f64 * GRID,
        )
    }

    fn open(&self, (x, y): (f64, f64)) -> bool {
        let index = self.cell((x, y));
        index < self.walkable.len() && self.walkable[index]
    }

    fn neighbors(&self, current: usize) -> impl Iterator<Item = usize> + '_ {
        let col = current % self.grid_width;
        [
            (col > 0).then(|| current - 1),
            (col + 1 < self.grid_width).then_some(current + 1),
            current.checked_sub(self.grid_width),
            Some(current + self.grid_width),
        ]
        .into_iter()
        .flatten()
        .filter(|next| *next < self.walkable.len() && self.walkable[*next])
    }

    pub fn nearest_walkable(&self, point: (f64, f64)) -> (f64, f64) {
        if self.open(point) {
            return point;
        }
        self.walkable
            .iter()
            .enumerate()
            .filter(|(_, open)| **open)
            .map(|(index, _)| self.point(index))
            .min_by(|a, b| {
                let da = (a.0 - point.0).powi(2) + (a.1 - point.1).powi(2);
                let db = (b.0 - point.0).powi(2) + (b.1 - point.1).powi(2);
                da.total_cmp(&db)
            })
            .unwrap_or(point)
    }

    /// True when a straight walk stays on open floor.
    fn clear(&self, a: (f64, f64), b: (f64, f64)) -> bool {
        let length = (b.0 - a.0).hypot(b.1 - a.1);
        let steps = (length / 3.0).ceil().max(1.0) as usize;
        (0..=steps).all(|i| {
            let t = i as f64 / steps as f64;
            self.open((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t))
        })
    }

    /// Shortest open route, with grid corners pulled into straight segments.
    pub fn route(&self, start: (f64, f64), end: (f64, f64)) -> VecDeque<(f64, f64)> {
        let first = self.cell(self.nearest_walkable(start));
        let last = self.cell(self.nearest_walkable(end));
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
            for next in self.neighbors(current) {
                if previous[next] == usize::MAX {
                    previous[next] = current;
                    queue.push_back(next);
                }
            }
        }
        if previous[last] == usize::MAX {
            return VecDeque::new();
        }
        let mut cells = Vec::new();
        let mut cursor = last;
        while cursor != first {
            cells.push(self.point(cursor));
            cursor = previous[cursor];
        }
        cells.push(self.point(first));
        cells.reverse();
        cells.push(end);
        let mut path = VecDeque::new();
        let mut anchor = start;
        let mut index = 0;
        while index < cells.len() {
            let mut far = index;
            for (candidate, &point) in cells.iter().enumerate().skip(index) {
                if self.clear(anchor, point) {
                    far = candidate;
                }
            }
            anchor = cells[far];
            path.push_back(anchor);
            index = far + 1;
        }
        if path.back() != Some(&end) {
            path.push_back(end);
        }
        path
    }

    #[cfg(test)]
    fn walkable_at(&self, point: (f64, f64)) -> bool {
        self.open(point)
    }
}

fn snap(value: f64) -> f64 {
    (value / GRID).round() * GRID
}

fn grid_width_cells(width: usize, height: usize) -> usize {
    width * height
}

/// Small furnished corners within a 60x60 square, chosen by `variant`, anchored at `x, y`.
fn nooks(theme: &theme::Theme, variant: usize, x: f64, y: f64) -> Vec<Vec<Item>> {
    let feature = Kind::Feature(theme.features[variant % 2]);
    let plant = Kind::Plant(theme.plants[variant % 2]);
    let other = Kind::Plant(theme.plants[(variant + 1) % 2]);
    let clutter = Kind::Clutter(theme.clutter[variant % 2]);
    let reading = vec![
        Item::new(
            Kind::Rug {
                w: 56,
                h: 34,
                round: true,
            },
            x,
            y + 24.0,
            variant,
        ),
        Item::new(Kind::Armchair, x + 6.0, y + 16.0, variant),
        Item::new(Kind::SideTable, x + 34.0, y + 26.0, variant),
        Item::new(plant, x + 36.0, y, variant),
    ];
    let focal = vec![
        Item::new(feature, x, y + 6.0, variant),
        Item::new(other, x + 40.0, y + 22.0, variant + 1),
    ];
    let pair = vec![
        Item::new(plant, x + 8.0, y + 10.0, variant),
        Item::new(clutter, x + 32.0, y + 20.0, variant),
    ];
    match variant % 3 {
        0 => vec![reading, focal, pair],
        1 => vec![focal, pair, reading],
        _ => vec![pair, reading, focal],
    }
}

fn activity(feature: Feature) -> Activity {
    match feature {
        Feature::Planter
        | Feature::PottingBench
        | Feature::HydroPod
        | Feature::Bonsai
        | Feature::WoodPile => Activity::Tend,
        Feature::Telescope
        | Feature::Fireplace
        | Feature::SurfRack
        | Feature::DeckChair
        | Feature::StoneLantern => Activity::Gaze,
        Feature::Easel | Feature::Globe => Activity::Present,
        Feature::EspressoBar | Feature::Vending | Feature::Gumball | Feature::PastryCase => {
            Activity::Coffee
        }
        _ => Activity::Browse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    const STATUSES: [AgentStatus; 4] = [
        AgentStatus::Working,
        AgentStatus::Blocked,
        AgentStatus::Done,
        AgentStatus::Idle,
    ];

    #[test]
    fn every_design_routes_all_destinations_from_the_door() {
        for design in 0..theme::DESIGNS {
            for count in [1, 2, 3, 5, 8, 32] {
                for aspect in [0.5, 1.0, 1.8, 2.6] {
                    let plan = Plan::fit(count, design, aspect);
                    let context = format!("design={design} count={count} aspect={aspect}");
                    for slot in 0..count {
                        for status in STATUSES {
                            let end = plan.destination(slot, status, false);
                            let route = plan.route(plan.door(), end);
                            assert_eq!(route.back(), Some(&end), "{context} {slot} {status:?}");
                        }
                        let route = plan.route(plan.desk(slot), plan.door());
                        assert_eq!(route.back(), Some(&plan.door()), "{context} exit");
                        for phase in [1, 2] {
                            let (stop, _) = plan.rest_stop(slot, phase);
                            let route = plan.route(plan.seat(slot), stop);
                            assert_eq!(route.back(), Some(&stop), "{context} rest {phase}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn seats_and_waiting_spots_are_unique_and_open() {
        for design in 0..theme::DESIGNS {
            for count in [1, 4, 17, 100] {
                let plan = Plan::fit(count, design, 1.6);
                for status in STATUSES {
                    let ends: HashSet<_> = (0..count)
                        .map(|slot| {
                            let end = plan.destination(slot, status, false);
                            assert!(plan.walkable_at(end), "{design} {count} {status:?} {end:?}");
                            (end.0 as i32, end.1 as i32)
                        })
                        .collect();
                    assert_eq!(ends.len(), count, "design={design} {status:?}");
                }
            }
        }
    }

    #[test]
    fn routes_never_cross_furniture() {
        for design in 0..theme::DESIGNS {
            let plan = Plan::fit(12, design, 1.6);
            for slot in 0..12 {
                let path = plan.route(plan.door(), plan.desk(slot));
                let mut previous = plan.door();
                for point in path {
                    assert!(
                        plan.clear(previous, point) || point == plan.desk(slot),
                        "design={design} slot={slot} {previous:?}->{point:?}"
                    );
                    previous = point;
                }
            }
        }
    }

    #[test]
    fn office_desks_reflow_with_the_available_aspect() {
        for design in 0..theme::DESIGNS {
            let narrow = Plan::fit(2, design, 0.2);
            let wide = Plan::fit(2, design, 2.4);
            assert_eq!(narrow.desk(0).0, narrow.desk(1).0, "design={design}");
            assert_ne!(narrow.desk(0).1, narrow.desk(1).1, "design={design}");
            assert_ne!(wide.desk(0).0, wide.desk(1).0, "design={design}");
            assert_eq!(wide.desk(0).1, wide.desk(1).1, "design={design}");
        }
    }

    #[test]
    fn office_plans_depend_only_on_team_size_and_design() {
        for design in [0, 13, 30, 47] {
            let a = Plan::fit(5, design, ASPECT);
            let b = Plan::fit(5, design, ASPECT);
            assert_eq!(a.items, b.items);
            assert_eq!((a.width, a.height), (b.width, b.height));
            assert!(((a.width / a.height) - ASPECT).abs() < 1e-9);
        }
    }

    #[test]
    fn spare_floor_is_lightly_furnished() {
        // Loose decoration stays sparse: a few pieces per desk, never a carpet of clutter.
        for design in 0..theme::DESIGNS {
            for count in [1, 3, 12, 32] {
                let plan = Plan::fit(count, design, ASPECT);
                let decor = plan
                    .items
                    .iter()
                    .filter(|i| matches!(i.kind, Kind::Plant(_) | Kind::Clutter(_)))
                    .count();
                assert!(decor >= 2, "design={design} count={count} decor={decor}");
                assert!(
                    decor <= 6 + count * 2,
                    "design={design} count={count} decor={decor}"
                );
            }
        }
    }

    #[test]
    fn floor_plans_differ_across_layouts() {
        for theme_index in 0..theme::THEMES.len() {
            let signatures: HashSet<_> = (0..theme::LAYOUTS)
                .map(|layout| {
                    let plan = Plan::fit(6, theme_index * theme::LAYOUTS + layout, 1.6);
                    (0..6)
                        .map(|s| {
                            let d = plan.desk(s);
                            (
                                (d.0 / plan.width * 100.0) as i32,
                                (d.1 / plan.height * 100.0) as i32,
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            assert_eq!(signatures.len(), theme::LAYOUTS);
        }
    }

    #[test]
    fn map_shares_one_scale_without_overlap_or_distortion() {
        let plans: Vec<_> = [32, 1, 3, 1, 2, 1, 5, 1]
            .into_iter()
            .enumerate()
            .map(|(i, count)| Plan::fit(count, i * 7, ASPECT))
            .collect();
        let sizes: Vec<_> = plans.iter().map(|p| (p.width, p.height)).collect();
        for (w, h) in [
            (1280.0, 620.0),
            (900.0, 900.0),
            (600.0, 1000.0),
            (2400.0, 1300.0),
        ] {
            let bounds = Rect {
                x: 0.0,
                y: 0.0,
                w,
                h,
            };
            let (scale, rects) = arrange(&sizes, bounds);
            assert!(scale > 0.0 && scale <= MAX_SCALE);
            for (index, r) in rects.iter().enumerate() {
                assert!((r.w - (sizes[index].0 * scale + 2.0)).abs() < 1e-6);
                assert!((r.h - (sizes[index].1 * scale + TITLE + 2.0)).abs() < 1e-6);
                assert!(r.x >= -0.5 && r.y >= -0.5 && r.x + r.w <= w + 0.5 && r.y + r.h <= h + 0.5);
                for other in &rects[index + 1..] {
                    assert!(!r.inflate(-0.6).intersects(other), "{w}x{h}");
                }
            }
            // A slightly larger scale no longer fits: the map is as large as it can be.
            let used: f64 = rects.iter().map(|r| r.w * r.h).sum();
            assert!(used / (w * h) > 0.55, "{w}x{h} uses {:.2}", used / (w * h));
        }
    }
}
