// SPDX-License-Identifier: GPL-3.0-or-later
//! Furniture, room shells and small effects, authored as pixel canvases.
//!
//! Every builder draws inside a one-pixel margin so the selective outline fits.
//! Light comes from the upper left: top faces are lightest, fronts mid, and
//! right or lower edges darkest.

mod decor;
mod feature;
mod small;

use super::{
    layout::{Item, Kind, FRONT, SIDE, WALL},
    sprite::{self, dark, light, mix, Canvas},
    theme::{self, DeskStyle, Floor, Theme, View, Wall},
};
use gtk::cairo::{Context, ImageSurface};

pub(super) const METAL: u32 = 0x8b93a6;
pub(super) const PAPER: u32 = 0xf7f2e6;
pub(super) const SCREEN: u32 = 0x24344f;
pub(super) const INK: u32 = 0x2a2233;

/// Time of day for window views and room light, from the local clock.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) enum Daylight {
    Dawn,
    Day,
    Dusk,
    Night,
}

impl Daylight {
    pub fn at(hour: u32) -> Self {
        match hour {
            5..=6 => Daylight::Dawn,
            7..=16 => Daylight::Day,
            17..=18 => Daylight::Dusk,
            _ => Daylight::Night,
        }
    }
}

/// Theme colors with derived shading ramps.
pub(super) struct Pal {
    pub t: &'static Theme,
    pub wood: u32,
    pub fabric: u32,
    pub accent: u32,
    pub accent2: u32,
    pub top: u32,
}

impl Pal {
    pub fn new(design: usize) -> Self {
        let t = theme::theme(design);
        let top = match t.desk {
            DeskStyle::White => 0xf1f0ec,
            DeskStyle::Metal => mix(t.wood, 0x9aa6b8, 0.35),
            _ => t.wood,
        };
        Self {
            t,
            wood: t.wood,
            fabric: t.fabric,
            accent: t.accent,
            accent2: t.accent2,
            top,
        }
    }
}

/// Baked sprite for a placed item.
pub(super) fn item_sprite(item: &Item, design: usize, daylight: Daylight) -> ImageSurface {
    let variant = item.variant % 8;
    let day = matches!(item.kind, Kind::Window { .. }).then_some(daylight);
    sprite::cached(
        ("item", item.kind, design % theme::DESIGNS, variant, day),
        || build(item.kind, &Pal::new(design), variant, daylight),
    )
}

pub(super) fn build(kind: Kind, p: &Pal, v: usize, daylight: Daylight) -> Canvas {
    let (w, h) = kind.size();
    let mut c = Canvas::new(w, h);
    match kind {
        Kind::Desk { down: false } => desk_up(&mut c, p, v),
        Kind::Desk { down: true } => desk_down(&mut c, p, v),
        Kind::Chair { down } => chair(&mut c, p, down),
        Kind::Sofa => sofa(&mut c, p, v),
        Kind::Armchair => armchair(&mut c, p, v),
        Kind::CoffeeTable => coffee_table(&mut c, p, v),
        Kind::BeanBag => bean_bag(&mut c, p, v),
        Kind::SideTable => side_table(&mut c, p, v),
        Kind::Rug { w, h, round } => {
            rug(&mut c, p, w, h, round, v);
            return c;
        }
        Kind::Doormat => {
            c.round(0, 0, w, h, 1, dark(p.t.rug.1, 0.25));
            for x in (3..w - 3).step_by(3) {
                c.vline(x, 2, h - 4, dark(p.t.rug.1, 0.45));
            }
            return c;
        }
        Kind::Counter { w } => counter(&mut c, p, w),
        Kind::Fridge => fridge(&mut c, p, v),
        Kind::WaterCooler => water_cooler(&mut c),
        Kind::Printer => printer(&mut c, p),
        Kind::Feature(f) => feature::draw(&mut c, p, f, v),
        Kind::Plant(plant) => small::plant(&mut c, p, plant, v),
        Kind::Clutter(clutter) => small::clutter(&mut c, p, clutter, v),
        Kind::Decor(d) => decor::draw(&mut c, p, d, v),
        Kind::Window { w } => window(&mut c, p, w, daylight),
        Kind::Clock => clock_face(&mut c, p),
        Kind::Whiteboard => whiteboard(&mut c, p, v),
        Kind::Door => {}
    }
    c.outline(0.72);
    c
}

fn legs(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, color: u32) {
    c.rect(x, y, 2, h, color);
    c.rect(x + w - 2, y, 2, h, color);
}

fn mug(c: &mut Canvas, x: i32, y: i32, color: u32) {
    c.rect(x, y, 4, 4, color);
    c.hline(x, y, 4, light(color, 0.35));
    c.set(x + 4, y + 1, color);
    c.set(x + 4, y + 2, color);
    c.set(x + 1, y, 0x6b4630);
    c.set(x + 2, y, 0x6b4630);
    c.fine_rect(x * 2 + 1, y * 2 + 2, 1, 4, light(color, 0.55));
    c.fine_rect(x * 2 + 8, y * 2 + 3, 1, 2, dark(color, 0.25));
}

fn desk_clutter(c: &mut Canvas, p: &Pal, v: usize, x: i32, y: i32) {
    match v % 6 {
        0 => mug(c, x, y + 1, p.accent),
        1 => {
            c.rect(x - 1, y + 1, 7, 4, PAPER);
            c.rect(x, y, 7, 4, 0xfffbf2);
            c.hline(x + 1, y + 1, 4, 0xbdb3a0);
            c.hline(x + 1, y + 3, 3, 0xbdb3a0);
        }
        2 => {
            c.rect(x + 1, y + 1, 5, 4, 0xc8774f);
            c.hline(x + 1, y + 1, 5, light(0xc8774f, 0.3));
            for (dx, dy) in [(0, -3), (3, -5), (5, -2), (2, -2)] {
                c.rect(x + dx, y + dy, 3, 3, 0x5f9a58);
            }
            c.set(x + 3, y - 4, light(0x5f9a58, 0.4));
        }
        3 => {
            c.rect(x, y + 3, 6, 2, METAL);
            c.line((x + 2, y + 3), (x + 4, y - 3), METAL);
            c.rect(x + 3, y - 5, 5, 3, p.accent2);
            c.hline(x + 4, y - 2, 3, 0xfff1b8);
        }
        4 => {
            for (i, color) in [p.accent, p.accent2, 0xe8d8b0].into_iter().enumerate() {
                c.rect(x, y + 3 - i as i32 * 2, 7 - i as i32, 2, color);
            }
        }
        _ => {
            c.rect(x, y + 1, 5, 4, 0x3c3a4a);
            c.round(x - 1, y - 1, 7, 3, 1, 0x3c3a4a);
            c.rect(x + 1, y + 2, 3, 2, p.accent);
        }
    }
}

/// Desk seen from its occupant's side: the screen faces the viewer.
fn desk_up(c: &mut Canvas, p: &Pal, v: usize) {
    let top = p.top;
    let low = p.t.desk == DeskStyle::Low;
    let surface_y = if low { 16 } else { 13 };
    c.rect(1, surface_y, 38, 8, top);
    c.hline(1, surface_y, 38, light(top, 0.3));
    c.rect(1, surface_y + 8, 38, 3, dark(top, 0.22));
    c.hline(1, surface_y + 10, 38, dark(top, 0.4));
    if !low {
        legs(
            c,
            2,
            surface_y + 11,
            36,
            26 - surface_y - 10,
            dark(top, 0.45),
        );
    }
    if p.t.desk == DeskStyle::Metal {
        c.hline(1, surface_y + 9, 38, p.accent);
    }
    // Monitor with a dark idle screen; the active overlay draws code on it.
    let bezel = if p.t.dark { 0x151824 } else { 0x2c3040 };
    c.round(9, 1 + i32::from(low) * 3, 22, 13, 1, bezel);
    let sy = 3 + i32::from(low) * 3;
    c.rect(11, sy, 18, 9, SCREEN);
    c.line((12, sy + 7), (17, sy + 2), mix(SCREEN, 0xffffff, 0.12));
    c.rect(18, sy + 10, 4, surface_y - sy - 9, bezel);
    c.hline(15, surface_y + 1, 10, bezel);
    // Keyboard and mouse.
    c.rect(12, surface_y + 4, 14, 3, 0xdde1ea);
    for y in [surface_y * 2 + 9, surface_y * 2 + 11] {
        for x in (25..50).step_by(3) {
            c.fine_rect(x, y, 2, 1, 0x929bb0);
        }
    }
    c.fine_rect(33, surface_y * 2 + 13, 11, 1, 0x929bb0);
    c.fine_rect(21, sy * 2 - 2, 37, 1, light(bezel, 0.25));
    c.fine_rect(56, sy * 2 + 19, 1, 1, p.accent);
    c.rect(28, surface_y + 5, 2, 2, 0xdde1ea);
    desk_clutter(c, p, v, 31, surface_y - 1);
    if v % 3 == 1 {
        // Sticky notes on the bezel.
        c.rect(9, 2 + i32::from(low) * 3, 3, 3, 0xf6d55c);
        c.rect(28, 9 + i32::from(low) * 3, 3, 3, 0xf28fb0);
    }
}

/// Desk whose occupant faces the viewer from behind it; a laptop keeps the face visible.
fn desk_down(c: &mut Canvas, p: &Pal, v: usize) {
    let top = p.top;
    let low = p.t.desk == DeskStyle::Low;
    c.rect(1, 6, 38, 9, top);
    c.hline(1, 6, 38, light(top, 0.3));
    c.rect(1, 15, 38, 4, dark(top, 0.22));
    c.hline(1, 18, 38, dark(top, 0.4));
    if !low {
        legs(c, 2, 19, 36, 6, dark(top, 0.45));
    }
    let lid = if p.t.dark { 0x3b4258 } else { 0xc3cad6 };
    c.round(13, 1, 15, 9, 1, lid);
    c.hline(14, 1, 13, light(lid, 0.3));
    c.rect(19, 4, 3, 2, p.accent);
    c.rect(12, 10, 17, 2, dark(lid, 0.2));
    desk_clutter(c, p, v + 3, 3, 8);
}

fn chair(c: &mut Canvas, p: &Pal, down: bool) {
    let fabric = p.fabric;
    if p.t.desk == DeskStyle::Low {
        c.round(1, 9, 16, 8, 2, fabric);
        c.hline(2, 9, 14, light(fabric, 0.25));
        c.hline(3, 15, 12, dark(fabric, 0.3));
        return;
    }
    let base = 0x3a3f4f;
    if down {
        c.round(3, 0, 12, 11, 2, fabric);
        c.hline(5, 1, 8, light(fabric, 0.25));
        c.rect(2, 10, 14, 3, light(fabric, 0.12));
    } else {
        c.round(3, 1, 12, 10, 2, dark(fabric, 0.08));
        c.vline(4, 3, 6, light(fabric, 0.2));
        c.rect(2, 9, 14, 3, dark(fabric, 0.3));
    }
    c.rect(8, 13, 2, 2, base);
    c.hline(3, 15, 12, base);
    for x in [3, 8, 14] {
        c.set(x, 16, 0x22252f);
    }
}

fn sofa(c: &mut Canvas, p: &Pal, v: usize) {
    let f = p.fabric;
    c.round(2, 1, 60, 15, 3, dark(f, 0.06));
    c.hline(5, 2, 54, light(f, 0.22));
    // Back cushions.
    for x in [8, 24, 40] {
        c.round(x, 4, 16, 10, 2, f);
        c.hline(x + 2, 4, 12, light(f, 0.2));
        c.fine_rect(x * 2 + 3, 11, 1, 13, light(f, 0.25));
        c.fine_rect(x * 2 + 4, 25, 24, 1, dark(f, 0.2));
        c.fine_rect(x * 2 + 15, 17, 2, 1, dark(f, 0.2));
    }
    c.rect(7, 14, 50, 8, light(f, 0.1));
    for x in [24, 40] {
        c.vline(x, 14, 8, dark(f, 0.15));
    }
    c.hline(7, 14, 50, light(f, 0.3));
    c.rect(4, 22, 56, 5, dark(f, 0.3));
    for x in [1, 55] {
        c.round(x, 7, 8, 20, 2, dark(f, 0.12));
        c.hline(x + 1, 7, 6, light(f, 0.1));
    }
    for x in [6, 55] {
        c.rect(x, 27, 3, 2, dark(p.wood, 0.45));
    }
    // Throw pillows and an occasional folded blanket.
    c.round(10, 9, 9, 8, 2, p.accent);
    c.hline(11, 9, 7, light(p.accent, 0.3));
    c.round(45, 9, 9, 8, 2, p.accent2);
    if v.is_multiple_of(2) {
        c.rect(30, 13, 12, 5, mix(p.accent, PAPER, 0.5));
        c.hline(30, 15, 12, mix(p.accent, PAPER, 0.2));
    }
}

fn armchair(c: &mut Canvas, p: &Pal, v: usize) {
    let f = if v.is_multiple_of(2) {
        p.accent2
    } else {
        p.fabric
    };
    c.round(3, 1, 18, 14, 3, dark(f, 0.05));
    c.hline(5, 2, 14, light(f, 0.25));
    c.rect(4, 14, 16, 6, light(f, 0.12));
    c.hline(4, 14, 16, light(f, 0.3));
    for x in [1, 18] {
        c.round(x, 7, 5, 15, 1, dark(f, 0.15));
    }
    c.rect(3, 20, 18, 4, dark(f, 0.32));
    for x in [4, 18] {
        c.rect(x, 24, 2, 3, dark(p.wood, 0.45));
    }
}

fn coffee_table(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    c.round(1, 3, 38, 8, 1, wood);
    c.hline(2, 3, 36, light(wood, 0.3));
    c.rect(1, 11, 38, 2, dark(wood, 0.3));
    legs(c, 3, 13, 34, 2, dark(wood, 0.5));
    // Magazines, a cup and a small succulent.
    c.rect(6, 5, 9, 5, p.accent2);
    c.rect(7, 4, 9, 5, PAPER);
    c.hline(8, 6, 6, 0xb9ae99);
    mug(c, 20, 5, if v.is_multiple_of(2) { PAPER } else { p.accent });
    c.rect(30, 6, 5, 4, 0xd9c6a2);
    c.rect(30, 2, 2, 4, 0x6aa067);
    c.rect(33, 3, 2, 3, 0x6aa067);
}

fn bean_bag(c: &mut Canvas, p: &Pal, v: usize) {
    let f = if v.is_multiple_of(2) {
        p.accent
    } else {
        p.accent2
    };
    c.oval(1, 3, 20, 14, f);
    c.oval(3, 3, 12, 7, light(f, 0.25));
    c.oval(6, 6, 10, 5, dark(f, 0.15));
    c.hline(4, 15, 14, dark(f, 0.35));
}

fn side_table(c: &mut Canvas, p: &Pal, v: usize) {
    c.round(1, 6, 10, 4, 1, p.wood);
    c.hline(2, 6, 8, light(p.wood, 0.3));
    c.rect(5, 10, 2, 3, dark(p.wood, 0.4));
    if v.is_multiple_of(2) {
        c.rect(4, 3, 4, 3, 0xd9c6a2);
        c.rect(4, 0, 2, 3, 0x6aa067);
        c.rect(6, 1, 2, 2, 0x6aa067);
    } else {
        mug(c, 4, 2, p.accent);
    }
}

fn rug(c: &mut Canvas, p: &Pal, w: i32, h: i32, round: bool, v: usize) {
    let (base, border) = p.t.rug;
    if round {
        c.oval(0, 0, w, h, border);
        c.oval(2, 2, w - 4, h - 4, base);
        c.oval(w / 4, h / 4, w / 2, h / 2, mix(base, border, 0.35));
        return;
    }
    c.rect(0, 0, w, h, border);
    c.rect(3, 3, w - 6, h - 6, base);
    c.rect(5, 5, w - 10, 1, mix(base, border, 0.5));
    c.rect(5, h - 6, w - 10, 1, mix(base, border, 0.5));
    // A diamond medallion row or stripes, by variant.
    if v.is_multiple_of(2) {
        for cx in (12..w - 10).step_by(18) {
            for r in 0..4 {
                c.hline(cx - r, h / 2 - 3 + r, r * 2 + 1, mix(base, border, 0.6));
                c.hline(cx - r, h / 2 + 3 - r, r * 2 + 1, mix(base, border, 0.6));
            }
        }
    } else {
        for y in (9..h - 9).step_by(6) {
            c.hline(8, y, w - 16, mix(base, border, 0.4));
        }
    }
    for x in (2..w - 2).step_by(3) {
        c.vline(x, 0, 1, light(border, 0.4));
        c.vline(x, h - 1, 1, light(border, 0.4));
    }
}

fn counter(c: &mut Canvas, p: &Pal, w: i32) {
    let wood = p.wood;
    let cabinet = mix(wood, PAPER, 0.25);
    c.rect(1, 14, w - 2, 5, light(p.top, 0.15));
    c.hline(1, 14, w - 2, light(p.top, 0.45));
    c.rect(1, 19, w - 2, 14, cabinet);
    for x in (1..w - 2).step_by(16) {
        c.vline(x, 20, 12, dark(cabinet, 0.25));
        c.rect(x + 6, 24, 3, 1, METAL);
    }
    c.hline(2, 33, w - 4, dark(cabinet, 0.5));
    // Espresso machine with a cup slot and steam wand.
    c.round(4, 1, 15, 14, 1, 0x3b3f4c);
    c.rect(6, 3, 11, 3, METAL);
    c.rect(8, 8, 7, 5, 0x22252f);
    c.rect(10, 11, 3, 2, PAPER);
    c.set(16, 4, p.accent);
    c.vline(18, 6, 5, METAL);
    // Mugs on a rack and a kettle.
    for (i, color) in [p.accent, PAPER, p.accent2].into_iter().enumerate() {
        mug(c, 23 + i as i32 * 6, 9, color);
    }
    if w > 44 {
        c.round(w - 14, 6, 9, 8, 2, 0xd8dbe3);
        c.hline(w - 13, 6, 7, light(0xd8dbe3, 0.4));
        c.rect(w - 6, 8, 2, 2, 0xd8dbe3);
        c.rect(w - 11, 4, 3, 2, 0x3b3f4c);
    }
}

fn fridge(c: &mut Canvas, p: &Pal, v: usize) {
    let body = if p.t.dark { 0xb9c0d0 } else { 0xeef0f2 };
    c.round(1, 1, 22, 38, 2, body);
    c.vline(2, 2, 36, light(body, 0.5));
    c.vline(21, 2, 36, dark(body, 0.18));
    c.hline(2, 14, 20, dark(body, 0.25));
    c.rect(18, 6, 2, 6, METAL);
    c.rect(18, 17, 2, 9, METAL);
    // Magnets and a photo.
    c.rect(5, 18, 6, 5, PAPER);
    c.rect(
        6,
        19,
        4,
        3,
        if v.is_multiple_of(2) {
            0x8fc1e3
        } else {
            0xf2b880
        },
    );
    c.set(13, 20, p.accent);
    c.set(8, 26, p.accent2);
    c.rect(6, 4, 8, 6, 0xfff6c8);
    c.hline(7, 6, 5, 0xb9ae99);
    c.rect(3, 38, 3, 1, 0x3a3f4f);
}

fn water_cooler(c: &mut Canvas) {
    let water = 0x8fd0f2;
    c.round(3, 0, 8, 10, 2, water);
    c.vline(4, 2, 6, light(water, 0.6));
    c.rect(2, 10, 10, 18, 0xeef0f2);
    c.vline(3, 11, 16, PAPER);
    c.vline(11, 11, 16, 0xc8ccd6);
    c.rect(4, 14, 2, 2, 0x5b8de0);
    c.rect(8, 14, 2, 2, 0xe05b5b);
    c.rect(3, 19, 8, 1, 0xb9bfcc);
    c.rect(5, 23, 4, 3, 0xd8f1ff);
}

fn printer(c: &mut Canvas, p: &Pal) {
    let body = 0xe4e6ea;
    c.round(1, 6, 24, 12, 1, body);
    c.hline(2, 6, 22, light(body, 0.5));
    c.rect(1, 14, 24, 4, dark(body, 0.2));
    c.rect(5, 2, 16, 5, 0xc8ccd6);
    c.rect(6, 1, 14, 2, PAPER);
    c.rect(6, 9, 14, 2, 0x3a3f4f);
    c.rect(19, 8, 3, 1, p.accent2);
    c.rect(1, 18, 24, 3, dark(p.wood, 0.25));
    legs(c, 2, 18, 22, 3, dark(p.wood, 0.5));
}

fn sky(daylight: Daylight, view: View) -> (u32, u32) {
    if matches!(view, View::Space) {
        return (0x0e1630, 0x1d2a52);
    }
    match daylight {
        Daylight::Dawn => (0xf6b7a0, 0xfbe0b8),
        Daylight::Day => (0x8fd0f5, 0xd5f0fb),
        Daylight::Dusk => (0x6d5aa6, 0xf09a7a),
        Daylight::Night => (0x182446, 0x2c3a68),
    }
}

/// A framed window showing the theme's outdoor view at the current time of day.
fn window(c: &mut Canvas, p: &Pal, w: i32, daylight: Daylight) {
    let view = p.t.view;
    let daylight = if p.t.dark && view == View::NightCity {
        Daylight::Night
    } else {
        daylight
    };
    let night = daylight == Daylight::Night;
    let (top, bottom) = sky(daylight, view);
    let (x0, y0, iw, ih) = (3, 3, w - 6, 22);
    let mut glass = Canvas::new(iw, ih);
    for y in 0..ih {
        glass.hline(0, y, iw, mix(top, bottom, y as f64 / ih as f64));
    }
    let hash = |i: i32| ((i as u32).wrapping_mul(2654435761) >> 9) as i32;
    match view {
        View::City | View::NightCity => {
            let far = if night {
                0x2a3560
            } else {
                mix(top, 0x5f7f9a, 0.55)
            };
            let near = if night {
                0x1a2142
            } else {
                mix(top, 0x445e78, 0.75)
            };
            let mut x = 0;
            let mut i = 0;
            while x < iw {
                let bw = 4 + hash(i) % 5;
                let bh = 6 + hash(i + 7) % 10;
                glass.rect(x, ih - bh, bw, bh, if i % 2 == 0 { far } else { near });
                for wy in (ih - bh + 2..ih - 1).step_by(3) {
                    for wx in (x + 1..x + bw - 1).step_by(2) {
                        if night && hash(wx * 31 + wy) % 3 == 0 {
                            glass.set(wx, wy, 0xffd77a);
                        } else if !night && hash(wx + wy * 17) % 5 == 0 {
                            glass.set(wx, wy, light(far, 0.3));
                        }
                    }
                }
                x += bw + 1;
                i += 1;
            }
        }
        View::Garden => {
            let leaf = if night { 0x23402f } else { 0x5f9e57 };
            for i in 0..iw / 7 + 1 {
                let cx = i * 7 + hash(i) % 3;
                glass.oval(cx - 4, ih - 13 - hash(i + 3) % 4, 11, 11, leaf);
                glass.oval(cx - 2, ih - 12 - hash(i + 3) % 4, 5, 4, light(leaf, 0.2));
            }
            glass.rect(0, ih - 4, iw, 4, dark(leaf, 0.2));
        }
        View::Space => {
            for i in 0..iw * 2 / 3 {
                let (sx, sy) = (hash(i) % iw, hash(i + 99) % ih);
                glass.set(sx, sy, if i % 5 == 0 { 0xfff3c4 } else { 0x9fb4e8 });
            }
            glass.oval(iw - 14, ih - 12, 18, 18, 0xe08a5c);
            glass.oval(iw - 12, ih - 11, 8, 6, 0xf2b07c);
            glass.hline(iw - 16, ih - 3, 22, 0xb76a46);
        }
        View::Snow => {
            let fir = if night { 0x1f3a3a } else { 0x3f6f5a };
            glass.rect(0, ih - 5, iw, 5, if night { 0x9fb0d0 } else { 0xf4f8ff });
            for i in 0..iw / 8 + 1 {
                let cx = i * 8 + 3 + hash(i) % 3;
                for r in 0..8 {
                    glass.hline(cx - r / 2, ih - 13 + r, r + 1, fir);
                }
                glass.set(cx, ih - 13, PAPER);
            }
            for i in 0..iw / 3 {
                glass.set(hash(i) % iw, hash(i + 5) % (ih - 6), PAPER);
            }
        }
        View::Sea => {
            let sea = if night { 0x1d3460 } else { 0x3f8fc7 };
            glass.rect(0, ih - 9, iw, 9, sea);
            for y in [ih - 7, ih - 4] {
                for x in (hash(y) % 5..iw).step_by(6) {
                    glass.hline(x, y, 3, light(sea, 0.4));
                }
            }
            let sun = if night { 0xf2ead2 } else { 0xffe08a };
            glass.oval(iw / 2 - 3, ih - 15, 7, 7, sun);
            glass.rect(0, ih - 9, iw, 1, light(sea, 0.3));
        }
        View::Bamboo => {
            let stalk = if night { 0x2b4a3a } else { 0x6aa35a };
            for i in 0..iw / 5 + 1 {
                let x = i * 5 + hash(i) % 2;
                glass.vline(x, 0, ih, stalk);
                glass.vline(x + 1, 0, ih, dark(stalk, 0.2));
                for y in (hash(i) % 6..ih).step_by(7) {
                    glass.hline(x, y, 2, light(stalk, 0.35));
                    glass.hline(x + 2, y + 1, 3, light(stalk, 0.1));
                }
            }
        }
    }
    let frame = p.t.trim;
    c.rect(1, 1, w - 2, 26, frame);
    c.blit(&glass, x0, y0);
    // Mullions, a light streak and a sill.
    c.vline(w / 2, y0, ih, frame);
    c.hline(x0, y0 + ih / 2, iw, frame);
    c.line((x0 + 3, y0 + 8), (x0 + 9, y0 + 2), mix(top, 0xffffff, 0.45));
    c.rect(0, 26, w, 3, light(frame, 0.2));
    c.hline(0, 26, w, light(frame, 0.45));
    if !p.t.dark && !matches!(p.t.wall, Wall::Glass | Wall::Metal) {
        // Gathered curtains.
        let cloth = mix(p.accent, PAPER, 0.55);
        for x in [0, w - 5] {
            c.rect(x, 1, 5, 24, cloth);
            c.vline(x + 2, 2, 22, dark(cloth, 0.15));
            c.rect(x, 18, 5, 2, p.accent);
        }
    }
}

fn clock_face(c: &mut Canvas, p: &Pal) {
    c.oval(0, 0, 14, 14, p.t.trim);
    c.oval(1, 1, 12, 12, PAPER);
    for (x, y) in [(6, 1), (11, 6), (6, 11), (1, 6)] {
        c.rect(x, y, 2, 1, INK);
    }
}

fn whiteboard(c: &mut Canvas, p: &Pal, v: usize) {
    c.rect(1, 1, 46, 24, 0xb8c0cc);
    c.rect(2, 2, 44, 22, 0xfbfcfd);
    c.hline(3, 3, 42, 0xffffff);
    // Sticky-note kanban and a sketched chart.
    let notes = [0xf6d55c, 0x9ad0f5, 0xf5a3c0, 0xa8e0a0];
    for (i, color) in notes.into_iter().enumerate() {
        let x = 4 + (i as i32 % 2) * 6;
        let y = 5 + (i as i32 / 2) * 6;
        c.rect(x, y, 5, 4, color);
    }
    c.line((18, 18), (24, 12), 0x4f7fd0);
    c.line((24, 12), (29, 15), 0x4f7fd0);
    c.line((29, 15), (36, 7), 0x4f7fd0);
    c.hline(17, 19, 22, 0x6b7280);
    c.vline(17, 6, 14, 0x6b7280);
    if v.is_multiple_of(2) {
        c.rect(38, 5, 5, 3, p.accent);
    }
    c.rect(4, 24, 40, 2, 0x8f97a6);
    c.rect(8, 23, 4, 1, 0xe05b5b);
    c.rect(14, 23, 4, 1, 0x4f7fd0);
}

/// Repeating floor and wall tiles; every pattern period divides this size.
const TILE: i32 = 192;

fn tile(cr: &Context, surface: &ImageSurface, x: f64, y: f64, w: f64, h: f64) {
    let pattern = gtk::cairo::SurfacePattern::create(surface);
    pattern.set_extend(gtk::cairo::Extend::Repeat);
    pattern.set_filter(gtk::cairo::Filter::Nearest);
    pattern.set_matrix(gtk::cairo::Matrix::new(1.0, 0.0, 0.0, 1.0, -x, -y));
    let _ = cr.set_source(&pattern);
    cr.rectangle(x, y, w, h);
    let _ = cr.fill();
}

/// Floor, walls and wall trim for the cached background, in art pixels.
pub(super) fn shell(cr: &Context, design: usize, w: f64, h: f64) {
    let t = theme::theme(design);
    let index = design % theme::DESIGNS / theme::LAYOUTS;
    let floor_tile = sprite::cached(("floor", index), || {
        let mut c = Canvas::new(TILE, TILE);
        floor(&mut c, t);
        c
    });
    let wall_tile = sprite::cached(("wall", index), || {
        let mut c = Canvas::new(TILE, WALL as i32 - 4);
        wall(&mut c, t);
        c
    });
    tile(cr, &floor_tile, 0.0, WALL, w, h - WALL);
    tile(cr, &wall_tile, 0.0, 0.0, w, WALL - 4.0);
    // Ceiling edge, skirting and the wall's shadow across the top of the floor.
    rect(cr, 0.0, 0.0, w, 2.0, dark(t.wall_colors.0, 0.55));
    rect(cr, 0.0, WALL - 4.0, w, 4.0, t.trim);
    rect(cr, 0.0, WALL - 4.0, w, 1.0, light(t.trim, 0.3));
    for y in 0..3 {
        cr.set_source_rgba(0.114, 0.082, 0.188, 0.18 - y as f64 * 0.05);
        cr.rectangle(0.0, WALL + y as f64, w, 1.0);
        let _ = cr.fill();
    }
    for (x, edge) in [(0.0, SIDE - 1.0), (w - SIDE, w - SIDE)] {
        rect(cr, x, 2.0, SIDE, h - 2.0, dark(t.wall_colors.0, 0.32));
        rect(cr, edge, WALL, 1.0, h - WALL, dark(t.wall_colors.0, 0.55));
    }
}

fn floor(c: &mut Canvas, t: &Theme) {
    let (w, h) = (c.w, c.h);
    let (a, b) = t.floor_colors;
    let hash = |x: i32, y: i32| {
        ((x as u32).wrapping_mul(374761393) ^ (y as u32).wrapping_mul(668265263) ^ 0x5bd1e995) >> 13
    };
    let top = 0;
    for y in top..h {
        for x in 0..w {
            let (tx, ty) = (x.div_euclid(16), (y - top).div_euclid(16));
            let (lx, ly) = (x.rem_euclid(16), (y - top).rem_euclid(16));
            let color = match t.floor {
                Floor::Herringbone => {
                    let block = (x / 8 + (y - top) / 8) % 2;
                    let plank = if block == 0 {
                        (x + (y - top) / 8 * 4) % 24
                    } else {
                        ((y - top) + x / 8 * 4) % 24
                    };
                    if plank == 0 {
                        dark(a, 0.18)
                    } else if (x / 8 + (y - top) / 8) % 4 == 0 {
                        b
                    } else {
                        a
                    }
                }
                Floor::Tiles => {
                    if lx == 0 || ly == 0 {
                        mix(a, 0xf2e8d8, 0.45)
                    } else if hash(tx, ty) % 4 == 0 {
                        b
                    } else {
                        a
                    }
                }
                Floor::Grid => {
                    if lx == 0 || ly == 0 {
                        dark(a, 0.3)
                    } else if lx == 1 || ly == 1 {
                        light(a, 0.08)
                    } else if (lx == 8 || lx == 11) && (ly == 8 || ly == 11) {
                        b
                    } else {
                        a
                    }
                }
                Floor::Carpet => {
                    let motif = (lx - 8).abs() + (ly - 8).abs() == 5;
                    if motif {
                        b
                    } else if hash(x, y) % 9 == 0 {
                        dark(a, 0.06)
                    } else {
                        a
                    }
                }
                Floor::Concrete => {
                    if x % 64 == 0 || (y - top) % 64 == 0 {
                        dark(a, 0.15)
                    } else if hash(x, y) % 23 == 0 {
                        b
                    } else if hash(x + 3, y) % 37 == 0 {
                        light(a, 0.15)
                    } else {
                        a
                    }
                }
                Floor::Checker => {
                    if (tx + ty) % 2 == 0 {
                        a
                    } else {
                        b
                    }
                }
                Floor::Planks => {
                    let row = (y - top) / 8;
                    let seam = (x + row * 20) % 48 == 0;
                    if (y - top) % 8 == 0 || seam {
                        dark(a, 0.25)
                    } else if (y - top) % 8 == 3 && hash(x / 6, row) % 3 == 0 {
                        b
                    } else {
                        a
                    }
                }
                Floor::Tatami => {
                    let (mx, my) = (x.div_euclid(32), (y - top).div_euclid(32));
                    let horizontal = (mx + my) % 2 == 0;
                    let (u, v) = (x.rem_euclid(32), (y - top).rem_euclid(32));
                    let half = if horizontal { v / 16 } else { u / 16 };
                    let edge = if horizontal {
                        v % 16 == 0 || u == 0
                    } else {
                        u % 16 == 0 || v == 0
                    };
                    let _ = half;
                    if edge {
                        0x4e5a3a
                    } else if (if horizontal { u } else { v }) % 2 == 0 {
                        b
                    } else {
                        a
                    }
                }
            };
            c.set(x, y, color);
        }
    }
}

fn wall(c: &mut Canvas, t: &Theme) {
    let (a, b) = t.wall_colors;
    let (w, wall_h) = (c.w, c.h);
    for y in 0..wall_h {
        for x in 0..w {
            let color = match t.wall {
                Wall::Plaster => {
                    if y >= 26 {
                        if (x % 12 == 0) || y == 26 {
                            dark(b, 0.15)
                        } else {
                            b
                        }
                    } else if (x * 7 + y * 13) % 48 == 0 {
                        dark(a, 0.04)
                    } else {
                        a
                    }
                }
                Wall::Glass => {
                    if x % 24 == 0 || y % 14 == 0 {
                        b
                    } else if (x + y) % 24 == 3 || (x + y) % 24 == 5 {
                        light(a, 0.35)
                    } else {
                        a
                    }
                }
                Wall::Panels => {
                    let lx = x % 24;
                    if lx == 0 {
                        dark(a, 0.35)
                    } else if lx == 1 || y == 3 || y == 22 {
                        light(a, 0.12)
                    } else if (3..=22).contains(&y) && (lx == 3 || lx == 21) {
                        dark(a, 0.18)
                    } else if y > 24 {
                        b
                    } else {
                        a
                    }
                }
                Wall::Brick => {
                    let row = y / 5;
                    let offset = if row % 2 == 0 { 0 } else { 6 };
                    if y % 5 == 4 || (x + offset) % 12 == 0 {
                        b
                    } else if ((x + offset) / 12 + row) % 5 == 0 {
                        dark(a, 0.12)
                    } else if ((x + offset) / 12 + row * 3) % 7 == 0 {
                        light(a, 0.1)
                    } else {
                        a
                    }
                }
                Wall::Stripes => {
                    if y >= 28 {
                        if y == 28 {
                            t.trim
                        } else {
                            light(t.trim, 0.55)
                        }
                    } else if x % 12 < 6 {
                        a
                    } else {
                        b
                    }
                }
                Wall::Logs => {
                    let ly = y % 8;
                    if ly == 0 {
                        dark(a, 0.4)
                    } else if ly == 1 || ly == 2 {
                        light(a, 0.12)
                    } else if ly == 7 {
                        b
                    } else if (x + y * 5) % 32 == 0 {
                        dark(a, 0.2)
                    } else {
                        a
                    }
                }
                Wall::Shoji => {
                    if y >= 30 || x % 16 == 0 || y % 10 == 0 {
                        b
                    } else if x % 8 == 0 || y % 5 == 0 {
                        mix(a, b, 0.35)
                    } else {
                        a
                    }
                }
                Wall::Metal => {
                    if x % 32 == 0 || y == 13 || y == 26 {
                        dark(a, 0.35)
                    } else if (x % 32 == 3 || x % 32 == 28) && (y % 13 == 3 || y % 13 == 10) {
                        light(b, 0.25)
                    } else if y > 26 {
                        b
                    } else {
                        a
                    }
                }
            };
            c.set(x, y, color);
        }
    }
}

/// Front wall band with the doorway, drawn over characters that walk out through it.
pub(super) fn front_wall(
    cr: &Context,
    design: usize,
    width: f64,
    height: f64,
    door_x: f64,
    open: bool,
) {
    let t = theme::theme(design);
    let y = height - FRONT;
    let band = dark(t.wall_colors.0, 0.32);
    rect(cr, 0.0, y, width, FRONT, band);
    rect(cr, 0.0, y, width, 2.0, light(band, 0.25));
    rect(cr, 0.0, height - 2.0, width, 2.0, dark(band, 0.35));
    let gap = 26.0;
    let left = door_x - gap / 2.0;
    // Floor shows through the opening; the leaf swings in when someone is near.
    rect(cr, left, y, gap, FRONT, dark(t.floor_colors.0, 0.12));
    rect(cr, left - 2.0, y - 2.0, 2.0, FRONT + 2.0, t.trim);
    rect(cr, left + gap, y - 2.0, 2.0, FRONT + 2.0, t.trim);
    let leaf = t.accent;
    if open {
        rect(cr, left, y - 14.0, 3.0, 16.0, leaf);
        rect(cr, left, y - 14.0, 1.0, 16.0, light(leaf, 0.35));
    } else {
        rect(cr, left, y + 2.0, gap, 5.0, leaf);
        rect(cr, left, y + 2.0, gap, 1.0, light(leaf, 0.35));
        rect(cr, left + gap - 5.0, y + 4.0, 2.0, 2.0, 0xf2d27a);
    }
    let _ = SIDE;
}

pub(super) fn rect(cr: &Context, x: f64, y: f64, w: f64, h: f64, color: u32) {
    cr.set_source_rgb(
        ((color >> 16) & 255) as f64 / 255.0,
        ((color >> 8) & 255) as f64 / 255.0,
        (color & 255) as f64 / 255.0,
    );
    cr.rectangle(x, y, w, h);
    let _ = cr.fill();
}

/// Scrolling code on an active monitor; `frame` advances the lines.
pub(super) fn screen(design: usize, frame: u32) -> ImageSurface {
    let accent = theme::theme(design).accent;
    sprite::cached(("screen", design % theme::DESIGNS, frame % 8), || {
        let mut c = Canvas::new(18, 9);
        c.rect(0, 0, 18, 9, 0x1b2a40);
        let colors = [0x7fd1b9, 0x9fb4ff, accent, 0xf2d27a, 0xd9e2f2];
        for row in 0..4 {
            let line = (row + frame as i32) as usize;
            let indent = [1, 3, 3, 5, 1, 3, 5, 3][line % 8];
            let width = [9, 6, 11, 5, 8, 12, 4, 7][line % 8];
            c.hline(
                indent,
                1 + row * 2,
                width.min(17 - indent),
                colors[line % colors.len()],
            );
        }
        if frame.is_multiple_of(2) {
            c.rect(14, 7, 2, 1, 0xffffff);
        }
        c
    })
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
pub(super) enum Emote {
    Typing(u8),
    Alert,
    Done,
    Sleep(u8),
    Coffee,
    Book,
    Unknown,
    Heart,
    Pizza,
}

/// Status icon in a small speech balloon, 15x14 art pixels.
pub(super) fn emote(kind: Emote) -> ImageSurface {
    sprite::cached(("emote", kind), || {
        let mut c = Canvas::new(17, 16);
        let fill = match kind {
            Emote::Alert => 0xffc53d,
            Emote::Done => 0x5cc98a,
            Emote::Sleep(_) => 0xcdbff2,
            Emote::Heart => 0xffd6e2,
            _ => 0xfffdf6,
        };
        c.round(1, 1, 15, 11, 2, fill);
        c.hline(3, 1, 11, light(fill, 0.5));
        c.rect(5, 12, 3, 1, fill);
        c.set(5, 13, fill);
        let ink = INK;
        match kind {
            Emote::Typing(f) => {
                for i in 0..3 {
                    let lift = i32::from(i == f as i32 % 3);
                    c.rect(4 + i * 3, 6 - lift, 2, 2, ink);
                }
            }
            Emote::Alert => {
                c.rect(7, 3, 3, 5, ink);
                c.rect(7, 9, 3, 2, ink);
            }
            Emote::Done => {
                c.line((5, 6), (7, 8), 0xffffff);
                c.line((5, 7), (7, 9), 0xffffff);
                c.line((8, 8), (12, 4), 0xffffff);
                c.line((8, 9), (12, 5), 0xffffff);
            }
            Emote::Sleep(f) => {
                let dy = i32::from(f % 2 == 1);
                c.hline(5, 4 - dy, 4, ink);
                c.line((8, 5 - dy), (5, 8 - dy), ink);
                c.hline(5, 8 - dy, 4, ink);
                c.hline(10, 7, 3, ink);
                c.set(11, 8, ink);
                c.hline(10, 9, 3, ink);
            }
            Emote::Coffee => {
                c.rect(5, 4, 6, 6, 0xc8774f);
                c.hline(5, 4, 6, 0x6b4630);
                c.rect(11, 5, 2, 3, 0xc8774f);
            }
            Emote::Book => {
                c.rect(4, 4, 9, 6, 0x4f7fd0);
                c.rect(5, 4, 3, 5, PAPER);
                c.rect(9, 4, 3, 5, PAPER);
            }
            Emote::Unknown => {
                c.hline(6, 3, 4, ink);
                c.vline(10, 4, 2, ink);
                c.rect(7, 6, 3, 1, ink);
                c.rect(7, 7, 2, 1, ink);
                c.rect(7, 9, 2, 2, ink);
            }
            Emote::Heart => {
                c.stamp(
                    4,
                    3,
                    &[
                        ".rr.rr.", "rrrrrrr", "rrrrrrr", ".rrrrr.", "..rrr..", "...r...",
                    ],
                    &[(b'r', 0xe2506f)],
                );
            }
            Emote::Pizza => {
                c.stamp(
                    4,
                    3,
                    &[
                        "yyyyyyy", ".yrryy.", ".yyyry.", "..yry..", "..yy...", "...y...",
                    ],
                    &[(b'y', 0xf2c14e), (b'r', 0xd9484f)],
                );
            }
        }
        c.outline(0.8);
        c
    })
}

/// A flat pizza box set on a lounge table during a delivery.
pub(super) fn pizza_box() -> ImageSurface {
    sprite::cached("pizza-box", || {
        let mut c = Canvas::new(18, 9);
        c.rect(1, 2, 16, 5, 0xd9a86c);
        c.hline(1, 2, 16, light(0xd9a86c, 0.35));
        c.rect(1, 7, 16, 1, dark(0xd9a86c, 0.4));
        c.rect(5, 3, 8, 3, 0xd9484f);
        c.set(8, 4, 0xf2c14e);
        c.outline(0.7);
        c
    })
}

/// One sparkle of an arrival or departure, sized by `phase` (0-3).
pub(super) fn sparkle(phase: u8) -> ImageSurface {
    sprite::cached(("sparkle", phase), || {
        let mut c = Canvas::new(7, 7);
        let r = i32::from(phase.min(3));
        let color = if phase < 2 { 0xfff3b0 } else { 0xffffff };
        c.hline(3 - r, 3, r * 2 + 1, color);
        c.vline(3, 3 - r, r * 2 + 1, color);
        c.set(3, 3, 0xffffff);
        c
    })
}

pub(super) fn confetti_color(i: usize) -> u32 {
    [0xff6f91, 0xffc53d, 0x5cc98a, 0x6fb7ff, 0xb48cff][i % 5]
}
