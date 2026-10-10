// SPDX-License-Identifier: GPL-3.0-or-later
//! Wall-mounted decoration, 32x26 art pixels, hung on the back wall.
//!
//! Pieces are seen straight on. Each sits on its own frame, plate or backing so
//! it reads against every wall finish; rails are lit along the top and left and
//! shaded along the bottom and right for a little depth.

use super::{Pal, INK, METAL, PAPER, SCREEN};
use crate::ui::agent_office::{
    sprite::{dark, light, luma, mix, Canvas, CLEAR},
    theme::Decor,
};

const CORK: u32 = 0xc89a62;
const LEAF: u32 = 0x5f9a58;
const CLAY: u32 = 0xc8774f;
const BRASS: u32 = 0xcfa046;
const CHALK: u32 = 0xeef0e4;
const ROPE: u32 = 0xe2cfa6;
const NOTES: [u32; 4] = [0xf6d55c, 0xf5a3c0, 0x9ad0f5, 0xa8e0a0];

type Area = (i32, i32, i32, i32);

pub(in crate::ui::agent_office) fn draw(c: &mut Canvas, p: &Pal, decor: Decor, v: usize) {
    match decor {
        Decor::Pinboard => pinboard(c, p, v),
        Decor::Frame => painting(c, p, v),
        Decor::HangingPlants => hanging_plants(c, p, v),
        Decor::Shelf => shelf(c, p, v),
        Decor::MonitorWall => monitor_wall(c, p, v),
        Decor::StarChart => star_chart(c, v),
        Decor::Pegboard => pegboard(c, p, v),
        Decor::Poster => poster(c, p, v),
        Decor::Chalkboard => chalkboard(c, p, v),
        Decor::Bunting => bunting(c, p, v),
        Decor::Neon => neon(c, p, v),
        Decor::Porthole => porthole(c, p, v),
        Decor::Skis => skis(c, p),
        Decor::Lifebuoy => lifebuoy(c, p),
        Decor::Scroll => scroll(c, p, v),
    }
}

fn hash(i: i32) -> i32 {
    ((i as u32).wrapping_mul(2_654_435_761) >> 9) as i32
}

fn wall_is_dark(p: &Pal) -> bool {
    luma(p.t.wall_colors.0) < 0.5
}

/// `base`, or `alt` when `base` would melt into the wall behind it.
fn against_wall(p: &Pal, base: u32, alt: u32) -> u32 {
    if (luma(base) - luma(p.t.wall_colors.0)).abs() < 0.14 {
        alt
    } else {
        base
    }
}

/// Flat slab lit from the upper left.
fn slab(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, color: u32) {
    c.rect(x, y, w, h, color);
    c.vline(x, y, h, light(color, 0.18));
    c.vline(x + w - 1, y, h, dark(color, 0.3));
    c.hline(x, y, w, light(color, 0.4));
    c.hline(x, y + h - 1, w, dark(color, 0.42));
}

/// Rails `t` pixels wide around a filled panel; returns the panel area.
fn framed(c: &mut Canvas, (x, y, w, h): Area, t: i32, rail: u32, fill: u32) -> Area {
    slab(c, x, y, w, h, rail);
    let (ix, iy, iw, ih) = (x + t, y + t, w - 2 * t, h - 2 * t);
    if t > 1 {
        // Inner lip: the lower rail faces up into the light, the upper one away.
        c.hline(ix - 1, iy - 1, iw + 2, dark(rail, 0.25));
        c.vline(ix - 1, iy - 1, ih + 2, dark(rail, 0.15));
        c.hline(ix - 1, iy + ih, iw + 2, light(rail, 0.25));
        c.vline(ix + iw, iy, ih + 1, light(rail, 0.12));
    }
    c.rect(ix, iy, iw, ih, fill);
    (ix, iy, iw, ih)
}

/// The rails' shadow falling across the top and left of a panel.
fn inset_shadow(c: &mut Canvas, (x, y, w, h): Area, amount: f64) {
    for xx in x..x + w {
        let color = c.get(xx, y);
        c.set(xx, y, dark(color, amount));
    }
    for yy in y + 1..y + h {
        let color = c.get(x, yy);
        c.set(x, yy, dark(color, amount * 0.7));
    }
}

fn inside_oval(x: i32, y: i32, (ox, oy, w, h): Area) -> bool {
    let (rx, ry) = (w as f64 / 2.0, h as f64 / 2.0);
    let dx = (x - ox) as f64 + 0.5 - rx;
    let dy = (y - oy) as f64 + 0.5 - ry;
    (dx / rx).powi(2) + (dy / ry).powi(2) <= 1.0
}

/// Brightness of a ring's tube seen face on, lit from the upper left:
/// about 1 on the lit crest, 0 on the far side.
fn ring_light(dx: f64, dy: f64, mid: f64, half: f64) -> f64 {
    let d = (dx * dx + dy * dy).sqrt().max(0.01);
    let s = ((d - mid) / half).clamp(-1.0, 1.0);
    let (nx, ny, nz) = (dx / d * s, dy / d * s, (1.0 - s * s).sqrt());
    (nx * -0.55 + ny * -0.6 + nz * 0.58).clamp(-1.0, 1.0) * 0.5 + 0.5
}

fn ramp(base: u32, lit: f64) -> u32 {
    match lit {
        l if l > 0.86 => light(base, 0.5),
        l if l > 0.68 => light(base, 0.22),
        l if l > 0.4 => base,
        l if l > 0.22 => dark(base, 0.22),
        _ => dark(base, 0.42),
    }
}

// --- Pinboard ---------------------------------------------------------------

fn pin(c: &mut Canvas, x: i32, y: i32, color: u32) {
    c.set(x, y, light(color, 0.55));
    c.set(x + 1, y, color);
    c.set(x, y + 1, color);
    c.set(x + 1, y + 1, dark(color, 0.35));
}

/// A pinned card casting a shadow on the board behind it.
fn card(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, color: u32, under: u32) {
    c.rect(x + 1, y + 1, w, h, dark(under, 0.32));
    c.rect(x, y, w, h, color);
    c.hline(x, y, w, light(color, 0.35));
    c.set(x + w - 1, y + h - 1, dark(color, 0.2));
}

fn pinboard(c: &mut Canvas, p: &Pal, v: usize) {
    let rail = against_wall(p, p.wood, dark(p.wood, 0.4));
    let board = framed(c, (1, 1, 30, 24), 2, rail, CORK);
    for i in 0..80 {
        let (x, y) = (3 + hash(i) % 26, 3 + hash(i + 97) % 20);
        let speck = if i % 3 == 0 {
            light(CORK, 0.2)
        } else {
            dark(CORK, 0.14)
        };
        c.set(x, y, speck);
    }
    inset_shadow(c, board, 0.28);
    let flip = v % 2 == 1;
    let at = |x: i32, w: i32| if flip { 32 - x - w } else { x };
    let notes = [NOTES[v % 4], NOTES[(v + 1) % 4], NOTES[(v + 2) % 4]];

    // Instant photo of a sunny hill.
    let (px, py) = (at(4, 8), 4);
    card(c, px, py, 8, 10, PAPER, CORK);
    for r in 0..4 {
        c.hline(
            px + 1,
            py + 1 + r,
            6,
            mix(0x8fd0f5, 0xd8f1fb, r as f64 / 3.0),
        );
    }
    c.rect(px + 1, py + 5, 6, 2, 0x6aa35a);
    c.hline(px + 2, py + 4, 3, 0x7fbd68);
    c.set(px + 5, py + 2, 0xffd25c);
    // Month card with one day circled.
    let (kx, ky) = (at(4, 8), 16);
    card(c, kx, ky, 8, 6, PAPER, CORK);
    c.rect(kx, ky, 8, 2, p.accent2);
    c.hline(kx, ky, 8, light(p.accent2, 0.3));
    for i in 0..3 {
        c.set(kx + 1 + i * 2, ky + 3, 0xb3a894);
        c.set(kx + 1 + i * 2, ky + 4, 0xb3a894);
    }
    c.set(kx + 6, ky + 4, p.accent);
    c.set(kx + 6, ky + 3, light(p.accent, 0.3));
    // Sticky notes and a lined page.
    let (nx, ny) = (at(14, 7), 4);
    card(c, nx, ny, 7, 6, notes[0], CORK);
    c.hline(nx + 1, ny + 2, 5, dark(notes[0], 0.42));
    c.hline(nx + 1, ny + 4, 3, dark(notes[0], 0.42));
    let (sx, sy) = (at(13, 8), 12);
    card(c, sx, sy, 8, 10, PAPER, CORK);
    for (r, len) in [(2, 6), (4, 5), (6, 6), (8, 3)] {
        c.hline(sx + 1, sy + r, len, 0xbdb3a0);
    }
    let (qx, qy) = (at(23, 5), 5);
    card(c, qx, qy, 5, 5, notes[1], CORK);
    let ink = dark(notes[1], 0.45);
    c.stamp(qx + 1, qy + 1, &["k.k", "kkk", ".k."], &[(b'k', ink)]);
    let (bx, by) = (at(23, 5), 14);
    card(c, bx, by, 5, 6, notes[2], CORK);
    c.hline(bx + 1, by + 2, 3, dark(notes[2], 0.42));
    c.hline(bx + 1, by + 4, 2, dark(notes[2], 0.42));
    pin(c, px + 3, py - 1, p.accent);
    pin(c, nx + 3, ny - 1, p.accent2);
    pin(c, sx + 3, sy, p.accent);
    pin(c, qx + 2, qy - 1, 0x3fa65a);
    pin(c, bx + 2, by, p.accent2);
    pin(c, kx + 3, ky - 1, 0x3fa65a);
}

// --- Framed painting -------------------------------------------------------

fn painting(c: &mut Canvas, p: &Pal, v: usize) {
    let gilt = wall_is_dark(p);
    let rail = if gilt { BRASS } else { dark(p.wood, 0.28) };
    let area = framed(c, (1, 1, 30, 24), 3, rail, PAPER);
    let (x, y, w, h) = area;
    let mut art = Canvas::new(w, h);
    match v % 4 {
        0 => landscape(&mut art, p),
        1 => abstract_art(&mut art, p),
        2 => cat_portrait(&mut art, p),
        _ => bouquet(&mut art, p),
    }
    c.blit(&art, x, y);
    inset_shadow(c, area, 0.32);
    // Carved corner bosses.
    for (cx, cy) in [(2, 2), (28, 2), (2, 22), (28, 22)] {
        c.set(cx, cy, light(rail, 0.55));
        c.set(cx + 1, cy + 1, dark(rail, 0.3));
    }
    // A small brass title plate on the bottom rail.
    c.hline(13, 23, 6, light(BRASS, 0.2));
    c.hline(14, 23, 4, light(BRASS, 0.45));
}

fn landscape(a: &mut Canvas, p: &Pal) {
    let rock = 0x7b8db8;
    for y in 0..13 {
        a.hline(0, y, a.w, mix(0x86c5ea, 0xf7e3bc, y as f64 / 12.0));
    }
    a.oval(16, 2, 5, 5, 0xfff1b5);
    a.hline(3, 4, 5, 0xffffff);
    a.hline(4, 3, 2, 0xffffff);
    for x in 0..a.w {
        let near = 10 - (x - 7).abs();
        let far = 8 - (x - 16).abs();
        let height = near.max(far);
        if height <= 0 {
            continue;
        }
        let top = 13 - height;
        let shaded = if near >= far { x > 7 } else { x > 16 };
        let color = if shaded { dark(rock, 0.22) } else { rock };
        a.vline(x, top, 13 - top, color);
        if height >= 6 {
            let snow = if shaded { 0xd8deef } else { 0xf6f8ff };
            a.vline(x, top, 2 + i32::from(height >= 8), snow);
        }
    }
    for x in 0..a.w {
        let top = 12 + ((x as f64 * 0.45).sin() * 1.3).round() as i32;
        a.vline(x, top, a.h - top, 0x7bb35e);
        a.set(x, top, light(0x7bb35e, 0.25));
    }
    a.rect(0, 15, a.w, 3, 0x5f9a58);
    // A fir and a few wildflowers.
    for r in 0..5 {
        a.hline(19 - r / 2, 8 + r, r + 1 - (r % 2), 0x3f6f5a);
    }
    a.vline(19, 13, 2, 0x6b4630);
    for (fx, fy) in [(3, 16), (7, 15), (12, 16), (22, 16)] {
        a.set(fx, fy, if fx % 2 == 0 { PAPER } else { p.accent });
    }
}

fn abstract_art(a: &mut Canvas, p: &Pal) {
    let ground = 0xf1e4c8;
    a.rect(0, 0, a.w, a.h, ground);
    a.rect(0, 12, a.w, 6, mix(ground, p.accent2, 0.22));
    let sun = (2, 2, 12, 12);
    a.oval(sun.0, sun.1, sun.2, sun.3, p.accent);
    for y in 4..16 {
        for x in 11..19 {
            let both = inside_oval(x, y, sun);
            a.set(
                x,
                y,
                if both {
                    dark(mix(p.accent, p.accent2, 0.5), 0.35)
                } else {
                    p.accent2
                },
            );
        }
    }
    a.oval(17, 1, 5, 5, 0xe8b84a);
    a.line((1, 16), (22, 8), INK);
    a.set(20, 14, INK);
    a.set(5, 5, light(p.accent, 0.35));
}

fn cat_portrait(a: &mut Canvas, p: &Pal) {
    let fur = 0xeaa65a;
    a.rect(0, 0, a.w, a.h, mix(p.accent2, PAPER, 0.55));
    a.oval(-2, -3, 12, 10, mix(p.accent2, PAPER, 0.7));
    let coat = p.fabric;
    a.oval(4, 12, 16, 10, coat);
    a.hline(6, 12, 12, light(coat, 0.3));
    a.oval(5, 3, 14, 10, fur);
    for (x, y) in [(6, 1), (6, 2), (7, 2), (6, 3), (7, 3), (8, 3)] {
        a.set(x, y, fur);
        a.set(23 - x, y, fur);
    }
    a.set(7, 3, 0xf2a9a6);
    a.set(16, 3, 0xf2a9a6);
    a.oval(9, 8, 6, 4, light(fur, 0.55));
    a.vline(11, 3, 2, dark(fur, 0.25));
    a.vline(12, 3, 2, dark(fur, 0.25));
    a.rect(8, 6, 1, 2, INK);
    a.rect(15, 6, 1, 2, INK);
    a.hline(11, 9, 2, 0xd9667a);
    a.set(7, 9, 0xf28a8f);
    a.set(16, 9, 0xf28a8f);
    // Bow tie.
    a.rect(9, 13, 2, 2, p.accent);
    a.rect(13, 13, 2, 2, p.accent);
    a.hline(11, 13, 2, dark(p.accent, 0.3));
}

fn bouquet(a: &mut Canvas, p: &Pal) {
    let backdrop = mix(p.accent2, INK, 0.45);
    a.rect(0, 0, a.w, a.h, backdrop);
    a.rect(0, 15, a.w, 3, mix(0xc99260, backdrop, 0.2));
    a.hline(0, 15, a.w, light(0xc99260, 0.2));
    for (sx, sy) in [(7, 4), (12, 2), (16, 5), (10, 6), (14, 7)] {
        a.line((12, 10), (sx, sy), 0x4f8a4a);
    }
    a.set(8, 8, LEAF);
    a.set(16, 8, LEAF);
    a.set(15, 9, LEAF);
    for (i, (fx, fy)) in [(7, 4), (12, 2), (16, 5), (10, 6), (14, 7)]
        .into_iter()
        .enumerate()
    {
        let petal = [p.accent, 0xf6d55c, 0xf5a3c0, PAPER, p.accent][i];
        a.oval(fx - 1, fy - 1, 3, 3, petal);
        a.set(fx - 1, fy - 1, light(petal, 0.4));
        a.set(
            fx,
            fy,
            if petal == 0xf6d55c {
                0xc87a2c
            } else {
                0xf6d55c
            },
        );
    }
    a.round(9, 10, 6, 6, 1, 0xe3ebf4);
    a.vline(9, 11, 4, 0xffffff);
    a.vline(14, 11, 4, 0xb4c0d2);
    a.hline(9, 12, 6, p.accent2);
}

// --- Hanging plants --------------------------------------------------------

/// A small leaf, lit on its upper left.
fn leaf(c: &mut Canvas, x: i32, y: i32, color: u32) {
    c.set(x, y, light(color, 0.38));
    c.set(x + 1, y, color);
    c.set(x, y + 1, color);
    c.set(x + 1, y + 1, dark(color, 0.28));
}

/// Macrame cords from a wall hook down to a pot rim at `pot_y`.
fn cords(c: &mut Canvas, cx: i32, pot_y: i32) {
    let knot = dark(ROPE, 0.28);
    c.set(cx, 0, METAL);
    c.set(cx - 1, 1, METAL);
    c.set(cx + 1, 1, dark(METAL, 0.3));
    c.set(cx, 2, ROPE);
    c.rect(cx - 1, 3, 3, 2, knot);
    c.hline(cx - 1, 3, 3, ROPE);
    c.line((cx - 1, 5), (cx - 4, pot_y - 1), ROPE);
    c.line((cx + 1, 5), (cx + 3, pot_y - 1), ROPE);
    c.vline(cx, 5, pot_y - 5, light(ROPE, 0.2));
}

/// The pot, drawn in front of the foliage, cradled in netting with a tassel.
fn pot(c: &mut Canvas, cx: i32, pot_y: i32, color: u32) {
    let knot = dark(ROPE, 0.28);
    let left = cx - 5;
    c.rect(left, pot_y, 10, 2, light(color, 0.1));
    c.hline(left, pot_y, 10, light(color, 0.42));
    c.set(left + 9, pot_y + 1, dark(color, 0.3));
    c.rect(left + 1, pot_y + 2, 8, 3, color);
    c.rect(left + 2, pot_y + 5, 6, 2, color);
    c.vline(left + 1, pot_y + 2, 3, light(color, 0.2));
    c.set(left + 2, pot_y + 2, light(color, 0.45));
    c.vline(left + 8, pot_y + 2, 3, dark(color, 0.3));
    c.vline(left + 7, pot_y + 5, 2, dark(color, 0.3));
    c.hline(left + 3, pot_y + 6, 4, dark(color, 0.36));
    c.hline(left + 1, pot_y + 2, 8, dark(color, 0.2));
    let net = mix(ROPE, color, 0.2);
    c.line((left + 1, pot_y + 2), (cx, pot_y + 6), net);
    c.line((left + 8, pot_y + 2), (cx, pot_y + 6), net);
    c.rect(cx - 1, pot_y + 7, 3, 1, knot);
    c.vline(cx - 1, pot_y + 8, 3, ROPE);
    c.vline(cx, pot_y + 8, 4, light(ROPE, 0.2));
    c.vline(cx + 1, pot_y + 8, 3, dark(ROPE, 0.15));
}

/// Heart-leaved vines: a mound over the rim and strands falling past the pot.
fn pothos(c: &mut Canvas, cx: i32, pot_y: i32, color: u32, trail: i32) {
    let deep = dark(LEAF, 0.34);
    c.oval(cx - 6, pot_y - 5, 12, 7, deep);
    for (dx, dy) in [
        (-5, -2),
        (-3, -4),
        (-1, -5),
        (1, -4),
        (3, -3),
        (-2, -2),
        (0, -2),
        (2, -1),
    ] {
        leaf(c, cx + dx, pot_y + dy, LEAF);
    }
    pot(c, cx, pot_y, color);
    for (vx, len, out) in [(cx - 6, trail, -1), (cx + 5, trail - 4, 1)] {
        for i in 0..len {
            let y = pot_y + 1 + i;
            let x = vx + i32::from((i / 4) % 2 == 1) * out;
            c.set(x, y, deep);
            if i % 3 == 1 {
                let lx = if out < 0 {
                    x - 1 - (i / 3) % 2
                } else {
                    x + (i / 3) % 2
                };
                leaf(c, lx, y, LEAF);
            }
        }
        leaf(c, vx - 1 + out.max(0), pot_y + len, LEAF);
    }
}

/// Arching striped blades spilling over the rim, with plantlets on runners.
fn spider_plant(c: &mut Canvas, cx: i32, pot_y: i32, color: u32, trail: i32) {
    let blade = 0x7fbd5c;
    let pale = 0xd8e6a6;
    let deep = dark(blade, 0.38);
    for s in [-1, 1] {
        let edge = cx + if s < 0 { -6 } else { 5 };
        // Long blades fall past the rim; short ones stand up from the crown.
        c.line((cx, pot_y), (cx + s * 3, pot_y - 4), blade);
        c.line((cx + s * 3, pot_y - 4), (edge, pot_y - 1), blade);
        c.line((edge, pot_y - 1), (edge, pot_y + 3), deep);
        c.line((cx, pot_y), (cx + s * 2, pot_y - 6), pale);
        c.set(cx + s * 3, pot_y - 6, blade);
        c.line((cx + s, pot_y), (cx + s * 5, pot_y - 3), deep);
        c.set(cx + s * 3, pot_y - 4, light(blade, 0.45));
    }
    c.vline(cx, pot_y - 5, 5, blade);
    c.set(cx, pot_y - 5, light(blade, 0.45));
    pot(c, cx, pot_y, color);
    // Runners with baby plants.
    for (rx, len) in [(cx - 7, trail), (cx + 6, trail - 5)] {
        c.vline(rx, pot_y + 2, len - 2, deep);
        let y = pot_y + len;
        c.set(rx - 1, y - 1, blade);
        c.set(rx + 1, y - 1, blade);
        c.hline(rx - 1, y, 3, blade);
        c.set(rx, y - 2, pale);
        c.set(rx - 2, y - 2, light(blade, 0.3));
        c.set(rx + 2, y - 2, blade);
    }
}

fn hanging_plants(c: &mut Canvas, p: &Pal, v: usize) {
    let glaze = against_wall(p, mix(p.accent2, PAPER, 0.35), dark(p.accent2, 0.1));
    let swap = v % 2 == 1;
    let (left_pot, right_pot) = if swap { (glaze, CLAY) } else { (CLAY, glaze) };
    cords(c, 8, 12);
    cords(c, 23, 7);
    if swap {
        spider_plant(c, 8, 12, left_pot, 11);
        pothos(c, 23, 7, right_pot, 16);
    } else {
        pothos(c, 8, 12, left_pot, 11);
        spider_plant(c, 23, 7, right_pot, 16);
    }
}

// --- Shelf ----------------------------------------------------------------

fn book(c: &mut Canvas, x: i32, bottom: i32, w: i32, h: i32, color: u32) {
    let top = bottom - h + 1;
    c.rect(x, top, w, h, color);
    c.hline(x, top, w, light(color, 0.35));
    c.vline(x + w - 1, top + 1, h - 1, dark(color, 0.25));
    c.hline(x, top + 2, w, light(color, 0.45));
    c.hline(x, bottom - 1, w, dark(color, 0.2));
}

fn board(c: &mut Canvas, y: i32, color: u32) {
    c.rect(1, y, 30, 3, color);
    c.hline(1, y, 30, light(color, 0.4));
    c.hline(1, y + 2, 30, dark(color, 0.35));
    c.vline(30, y, 3, dark(color, 0.28));
    let bracket = dark(METAL, 0.35);
    for x in [5, 25] {
        c.vline(x, y + 3, 2, bracket);
        c.set(x + 1, y + 3, bracket);
    }
}

fn shelf(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = against_wall(
        p,
        p.wood,
        if wall_is_dark(p) {
            light(p.wood, 0.4)
        } else {
            dark(p.wood, 0.3)
        },
    );
    let spines = [
        p.accent,
        p.accent2,
        p.fabric,
        0xe8d8b0,
        dark(p.accent2, 0.3),
        0x8a63b8,
    ];
    let s = |i: usize| spines[(i + v) % spines.len()];
    // Upper tier: upright books, a bookend and a leafy pot.
    for (x, w, h, i) in [
        (2, 3, 8, 0),
        (5, 2, 9, 1),
        (7, 3, 7, 2),
        (10, 2, 8, 3),
        (12, 3, 9, 4),
    ] {
        book(c, x, 9, w, h, s(i));
    }
    c.rect(15, 5, 2, 5, METAL);
    c.set(15, 5, light(METAL, 0.4));
    c.rect(20, 6, 6, 4, CLAY);
    c.hline(20, 6, 6, light(CLAY, 0.35));
    c.vline(25, 7, 3, dark(CLAY, 0.3));
    for (dx, dy) in [
        (0, -3),
        (2, -4),
        (4, -3),
        (1, -1),
        (3, -2),
        (5, -1),
        (-1, -1),
    ] {
        c.rect(20 + dx, 6 + dy - 1, 2, 2, LEAF);
        c.set(20 + dx, 6 + dy - 1, light(LEAF, 0.35));
    }
    c.rect(27, 6, 3, 4, light(p.accent2, 0.5));
    c.hline(27, 6, 3, dark(p.wood, 0.2));
    c.set(28, 8, light(p.accent2, 0.8));
    board(c, 10, wood);
    // Vines from the pot spill over the board edge.
    for (x, len) in [(25, 4), (23, 3)] {
        for i in 0..len {
            c.set(x + i % 2, 11 + i, dark(LEAF, 0.2));
        }
        c.rect(x, 11 + len, 2, 2, LEAF);
        c.set(x, 11 + len, light(LEAF, 0.35));
    }
    // Lower tier: a lying stack, a trinket and a tiny framed photo.
    for (i, (x, y, w)) in [(2, 19, 10), (3, 17, 9), (2, 15, 9)]
        .into_iter()
        .enumerate()
    {
        let color = s(i + 2);
        c.rect(x, y, w, 2, color);
        c.hline(x, y, w, light(color, 0.35));
        c.vline(x + w - 1, y, 2, PAPER);
        c.set(x + 2, y + 1, light(color, 0.5));
    }
    match v % 3 {
        0 => {
            // Beckoning cat figurine.
            let cat = 0xf6f1e6;
            c.stamp(
                14,
                13,
                &[
                    "c...c", "ccccc", "ckcck", "ccrcc", "ccccc", "cbccc", "ccccc",
                ],
                &[(b'c', cat), (b'k', INK), (b'r', 0xf28a8f), (b'b', p.accent)],
            );
            c.vline(18, 14, 6, dark(cat, 0.2));
            c.set(13, 17, cat);
            c.set(13, 16, cat);
        }
        1 => {
            // Desk globe.
            c.oval(14, 12, 6, 6, 0x4f86c6);
            c.rect(15, 13, 2, 2, 0x6aa35a);
            c.rect(17, 15, 2, 1, 0x6aa35a);
            c.set(15, 13, light(0x4f86c6, 0.5));
            c.vline(16, 18, 2, BRASS);
            c.hline(14, 20, 6, dark(BRASS, 0.2));
        }
        _ => {
            // Hourglass.
            c.hline(14, 13, 6, p.wood);
            c.hline(14, 20, 6, p.wood);
            c.stamp(
                14,
                14,
                &["gssssg", ".gssg.", "..gg..", "..sg..", ".gssg.", "gssssg"],
                &[(b'g', 0xcfe3ef), (b's', 0xe8c27a)],
            );
            c.hline(15, 14, 4, 0xcfe3ef);
        }
    }
    slab(c, 22, 14, 7, 7, p.accent2);
    c.rect(23, 15, 5, 5, 0x9fd4f0);
    c.rect(23, 18, 5, 2, 0x6aa35a);
    c.set(26, 16, 0xffe08a);
    board(c, 21, wood);
}

// --- Monitor wall -------------------------------------------------------------

fn monitor_wall(c: &mut Canvas, p: &Pal, v: usize) {
    let bezel = 0x262b3b;
    // Steel housing on dark walls so the bank stands off them.
    let case = if wall_is_dark(p) {
        mix(METAL, bezel, 0.4)
    } else {
        bezel
    };
    slab(c, 1, 1, 30, 24, case);
    c.rect(15, 2, 2, 21, dark(bezel, 0.4));
    c.hline(2, 12, 28, dark(bezel, 0.4));
    let bg = dark(SCREEN, 0.3);
    for (i, (x, y)) in [(2, 2), (17, 2), (2, 13), (17, 13)].into_iter().enumerate() {
        c.rect(x, y, 13, 10, bg);
        screen(c, p, (x, y, 13, 10), (i * 2 + v) % 6, bg);
        c.set(x, y, light(bg, 0.3));
        c.set(x + 1, y, light(bg, 0.15));
        c.set(x, y + 1, light(bg, 0.15));
    }
    c.hline(2, 23, 28, dark(case, 0.2));
    c.set(26, 23, p.accent2);
    c.set(28, 23, p.accent);
    c.hline(4, 23, 3, light(case, 0.2));
}

fn screen(c: &mut Canvas, p: &Pal, (x, y, w, h): Area, kind: usize, bg: u32) {
    let (a, b) = (p.accent, p.accent2);
    let dim = mix(bg, a, 0.22);
    match kind {
        0 => {
            for gx in (x + 1..x + w).step_by(3) {
                for gy in (y + 1..y + h).step_by(3) {
                    c.set(gx, gy, dim);
                }
            }
            let point = |i: i32| y + h - 2 - i * (h - 4) / w + (hash(i + 3) % 3 - 1);
            for i in 0..w {
                for yy in point(i) + 1..y + h {
                    c.set(x + i, yy, mix(bg, a, 0.2));
                }
            }
            for i in 1..w {
                c.line((x + i - 1, point(i - 1)), (x + i, point(i)), a);
            }
            c.set(x + w - 1, point(w - 1), light(a, 0.6));
        }
        1 => {
            c.hline(x + 1, y + h - 1, w - 2, dim);
            for (i, bh) in [4, 7, 5, 8].into_iter().enumerate() {
                let bx = x + 1 + i as i32 * 3;
                c.rect(bx, y + h - 1 - bh, 2, bh, if i % 2 == 0 { b } else { a });
                c.hline(
                    bx,
                    y + h - 1 - bh,
                    2,
                    light(if i % 2 == 0 { b } else { a }, 0.5),
                );
            }
        }
        2 => {
            let land = mix(bg, a, 0.42);
            for (lx, ly, lw, lh) in [
                (1, 2, 4, 3),
                (2, 5, 2, 3),
                (6, 1, 3, 2),
                (7, 3, 2, 5),
                (9, 2, 3, 3),
                (10, 6, 2, 2),
            ] {
                c.rect(x + lx, y + ly, lw, lh, land);
            }
            c.hline(x, y + 4, w, dim);
            for (dx, dy) in [(3, 3), (8, 5), (11, 3)] {
                c.set(x + dx, y + dy, light(b, 0.4));
                c.set(x + dx + 1, y + dy, b);
            }
        }
        3 => {
            let ring = mix(bg, a, 0.45);
            c.oval(x + 2, y, 10, 10, ring);
            c.oval(x + 3, y + 1, 8, 8, bg);
            c.oval(x + 4, y + 2, 6, 6, dim);
            c.oval(x + 5, y + 3, 4, 4, bg);
            c.hline(x + 2, y + 5, 10, dim);
            c.vline(x + 7, y, 10, dim);
            c.line((x + 7, y + 5), (x + 10, y + 2), a);
            c.line((x + 7, y + 4), (x + 9, y + 1), mix(bg, a, 0.55));
            c.set(x + 9, y + 7, b);
            c.set(x + 4, y + 3, light(b, 0.3));
        }
        4 => {
            c.hline(x, y + h / 2, w, dim);
            let mut last = y + h / 2;
            for i in 0..w {
                let yy = y + h / 2
                    - ((i as f64 * 0.95).sin() * 3.0 * if i % 4 == 1 { 1.3 } else { 1.0 }).round()
                        as i32;
                c.line((x + i - 1, last), (x + i, yy), b);
                last = yy;
            }
        }
        _ => {
            for (r, len) in [(1, 8), (3, 5), (5, 9), (7, 4)] {
                c.set(x + 1, y + r, b);
                c.hline(x + 3, y + r, len, mix(bg, a, 0.6));
            }
            c.rect(x + 8, y + 7, 2, 1, light(a, 0.5));
        }
    }
}

// --- Star chart -------------------------------------------------------------

/// A bright star with a soft four-point glint.
fn glint(a: &mut Canvas, x: i32, y: i32, night: u32) {
    let core = 0xfff3c4;
    a.set(x, y, core);
    for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
        a.set(x + dx, y + dy, mix(night, core, 0.55));
    }
}

fn star_chart(c: &mut Canvas, v: usize) {
    let night = 0x1a2350;
    let pale = 0x9fb4e8;
    let area = framed(c, (1, 1, 30, 24), 2, BRASS, night);
    let (x, y, w, h) = area;
    let mut a = Canvas::new(w, h);
    a.rect(0, 0, w, h, night);
    // Printed border with corner ticks.
    let rule = mix(night, pale, 0.3);
    a.rect(1, 1, w - 2, h - 2, rule);
    a.rect(2, 2, w - 4, h - 4, night);
    for (tx, ty) in [(1, 1), (w - 2, 1), (1, h - 2), (w - 2, h - 2)] {
        a.set(tx, ty, mix(night, pale, 0.6));
    }
    // A dotted ecliptic sweeping across.
    for i in (3..w - 3).step_by(2) {
        let t = (i - w / 2) as f64 / (w / 2) as f64;
        a.set(
            i,
            8 + (t * t * 6.0).round() as i32,
            mix(night, 0xe0b05a, 0.35),
        );
    }
    for i in 0..12 {
        a.set(
            3 + hash(i) % (w - 6),
            3 + hash(i + 40) % (h - 6),
            mix(night, pale, 0.35),
        );
    }
    let lines = mix(night, pale, 0.55);
    let groups: [&[(i32, i32)]; 3] = [
        &[
            (4, 14),
            (7, 13),
            (10, 14),
            (12, 16),
            (16, 17),
            (17, 14),
            (13, 12),
            (12, 16),
        ],
        &[(13, 4), (15, 7), (18, 5), (20, 8), (22, 4)],
        &[(20, 11), (23, 14), (20, 16), (20, 11)],
    ];
    for group in groups {
        for pair in group.windows(2) {
            a.line(pair[0], pair[1], lines);
        }
        for &(sx, sy) in group {
            a.set(sx, sy, 0xdfe8ff);
        }
    }
    for (sx, sy) in [(13, 12), (20, 11), (18, 5)] {
        glint(&mut a, sx, sy, night);
    }
    // Crescent moon.
    let moon = 0xf6e7b0;
    a.oval(3, 3, 6, 6, moon);
    a.oval(5, 2, 6, 6, night);
    a.set(3, 5, light(moon, 0.5));
    let art = if v % 2 == 1 { a.flipped() } else { a };
    c.blit(&art, x, y);
    inset_shadow(c, area, 0.25);
    for (cx, cy) in [(2, 2), (28, 2), (2, 22), (28, 22)] {
        c.set(cx, cy, light(BRASS, 0.6));
    }
}

// --- Pegboard ---------------------------------------------------------------

fn peg(c: &mut Canvas, x: i32, y: i32) {
    c.set(x, y, light(METAL, 0.45));
    c.set(x, y + 1, dark(METAL, 0.2));
}

/// Lays `layer` over `c`, darkening what it covers one pixel down and right.
fn with_shadow(c: &mut Canvas, layer: &Canvas, amount: f64) {
    for y in 0..layer.h {
        for x in 0..layer.w {
            if layer.get(x, y) != CLEAR && layer.get(x + 1, y + 1) == CLEAR {
                let under = c.get(x + 1, y + 1);
                if under != CLEAR {
                    c.set(x + 1, y + 1, dark(under, amount));
                }
            }
        }
    }
    c.blit(layer, 0, 0);
}

/// A four-pixel ring with a two-pixel hole showing `hole` through it.
fn loop_ring(c: &mut Canvas, x: i32, y: i32, color: u32, hole: u32) {
    c.oval(x, y, 4, 4, color);
    c.set(x + 1, y, light(color, 0.4));
    c.set(x, y + 1, light(color, 0.25));
    c.set(x + 3, y + 2, dark(color, 0.3));
    c.set(x + 2, y + 3, dark(color, 0.3));
    c.rect(x + 1, y + 1, 2, 2, hole);
}

fn pegboard(c: &mut Canvas, p: &Pal, v: usize) {
    let board = 0xd3ab7a;
    let rail = against_wall(p, p.t.trim, dark(p.t.trim, 0.4));
    let area = framed(c, (1, 1, 30, 24), 1, rail, board);
    for gx in (4..29).step_by(3) {
        for gy in (4..23).step_by(3) {
            c.set(gx, gy, dark(board, 0.36));
        }
    }
    inset_shadow(c, area, 0.2);
    let steel = METAL;
    let shine = light(METAL, 0.5);
    let edge = dark(METAL, 0.32);
    let grip = p.accent;
    if v % 2 == 1 {
        // A painted outline marks where the saw goes back.
        let marker = dark(board, 0.3);
        c.hline(9, 19, 11, marker);
        c.vline(9, 19, 4, marker);
        c.line((9, 22), (19, 20), marker);
        c.vline(19, 19, 2, marker);
    }
    let mut t = Canvas::new(c.w, c.h);
    // Hammer: a flat face on the left, a split claw on the right.
    peg(&mut t, 6, 3);
    t.stamp(
        3,
        5,
        &["ssssss.", "mmmmmmm", "ddddd.d"],
        &[(b's', shine), (b'm', steel), (b'd', edge)],
    );
    let wood = 0xb5814f;
    t.rect(5, 8, 2, 14, wood);
    t.vline(5, 8, 14, light(wood, 0.3));
    t.rect(5, 16, 2, 6, grip);
    t.vline(5, 16, 6, light(grip, 0.3));
    t.hline(5, 21, 2, dark(grip, 0.3));
    // Combination wrench: open jaw above, ring below.
    peg(&mut t, 13, 3);
    t.stamp(
        11,
        5,
        &[
            "sm..ms", "mm..mm", "mmmmmd", ".smmd.", "..sd..", "..sd..", "..sd..", "..sd..",
            "..sd..", ".smmd.", "sm..md", "mm..md", ".dddd.",
        ],
        &[(b's', shine), (b'm', steel), (b'd', edge)],
    );
    // Scissors hung by one finger loop, blades open below the pivot.
    let handle = p.accent2;
    loop_ring(&mut t, 18, 3, handle, board);
    loop_ring(&mut t, 22, 3, handle, board);
    t.set(19, 4, shine);
    t.set(21, 7, handle);
    t.set(22, 7, handle);
    t.rect(21, 8, 2, 1, light(METAL, 0.2));
    for i in 0..6 {
        t.set(20 - i / 2, 9 + i, shine);
        t.set(21 - i / 2, 9 + i, steel);
        t.set(22 + i / 2, 9 + i, steel);
        t.set(23 + i / 2, 9 + i, edge);
    }
    // Screwdriver.
    peg(&mut t, 28, 3);
    t.rect(27, 5, 3, 4, grip);
    t.vline(27, 5, 4, light(grip, 0.35));
    t.hline(27, 8, 3, dark(grip, 0.3));
    t.vline(28, 9, 5, steel);
    t.set(28, 13, edge);
    // Tape roll on a peg.
    let tape = if v.is_multiple_of(2) {
        0xf1c84b
    } else {
        p.fabric
    };
    t.oval(22, 16, 7, 7, tape);
    t.rect(24, 18, 3, 3, board);
    t.set(24, 18, dark(board, 0.3));
    t.set(25, 18, light(METAL, 0.45));
    t.set(25, 19, METAL);
    t.set(23, 17, light(tape, 0.5));
    t.set(22, 19, light(tape, 0.3));
    t.set(24, 16, light(tape, 0.3));
    t.set(28, 20, dark(tape, 0.3));
    t.set(26, 22, dark(tape, 0.3));
    t.set(27, 21, dark(tape, 0.3));
    if v.is_multiple_of(2) {
        // Parts tray.
        t.rect(9, 19, 11, 4, p.fabric);
        t.hline(9, 19, 11, light(p.fabric, 0.35));
        t.hline(9, 22, 11, dark(p.fabric, 0.35));
        t.hline(10, 20, 9, dark(p.fabric, 0.3));
        t.hline(11, 19, 2, 0xf1c84b);
        t.set(16, 19, shine);
    }
    with_shadow(c, &t, 0.32);
}

// --- Poster -----------------------------------------------------------------

fn text_bars(a: &mut Canvas, x: i32, y: i32, widths: &[i32], color: u32) {
    for (i, &w) in widths.iter().enumerate() {
        a.hline(x, y + i as i32 * 2, w, color);
    }
}

fn poster(c: &mut Canvas, p: &Pal, v: usize) {
    let (x, y, w, h) = (4, 1, 24, 24);
    c.rect(x, y, w, h, PAPER);
    c.vline(x + w - 1, y, h, dark(PAPER, 0.12));
    c.hline(x, y + h - 1, w, dark(PAPER, 0.16));
    let (aw, ah) = (w - 2, h - 2);
    let mut a = Canvas::new(aw, ah);
    let deep = dark(mix(p.accent, p.accent2, 0.5), 0.62);
    match v % 4 {
        0 => {
            // Striped sun sinking into the sea.
            for yy in 0..ah {
                a.hline(0, yy, aw, mix(deep, p.accent, yy as f64 / 14.0 * 0.8));
            }
            a.oval(4, 4, 14, 14, p.accent2);
            a.oval(6, 5, 5, 3, light(p.accent2, 0.4));
            for yy in [10, 12] {
                for xx in 0..aw {
                    if inside_oval(xx, yy, (4, 4, 14, 14)) {
                        a.set(xx, yy, mix(deep, p.accent, yy as f64 / 14.0 * 0.8));
                    }
                }
            }
            let sea = dark(deep, 0.15);
            a.rect(0, 14, aw, ah - 14, sea);
            a.hline(0, 14, aw, mix(sea, p.accent2, 0.35));
            for (row, half) in [(16, 5), (18, 4), (20, 2)] {
                a.hline(11 - half, row, half * 2, mix(sea, p.accent2, 0.6));
            }
            a.hline(2, 17, 2, mix(sea, PAPER, 0.3));
            a.hline(18, 19, 2, mix(sea, PAPER, 0.3));
            text_bars(&mut a, 2, 1, &[9], PAPER);
        }
        1 => {
            // Lightning bolt on rays.
            a.rect(0, 0, aw, ah, p.accent);
            for i in -ah..aw {
                if i.rem_euclid(6) < 3 {
                    a.line((i, ah), (i + ah, 0), light(p.accent, 0.14));
                }
            }
            let bolt = [
                "......#####",
                ".....#####.",
                ".....####..",
                "....####...",
                "....###....",
                "...#######.",
                "...######..",
                "......###..",
                ".....###...",
                ".....##....",
                "....##.....",
                "....#......",
                "...#.......",
            ];
            a.stamp(6, 4, &bolt, &[(b'#', deep)]);
            a.stamp(5, 3, &bolt, &[(b'#', p.accent2)]);
            a.stamp(5, 3, &["......#####"], &[(b'#', light(p.accent2, 0.5))]);
            text_bars(&mut a, 3, 18, &[16, 9], deep);
        }
        2 => {
            // Ringed planet.
            a.rect(0, 0, aw, ah, deep);
            for i in 0..14 {
                a.set(
                    hash(i) % aw,
                    hash(i + 21) % 16,
                    if i % 3 == 0 { p.accent2 } else { PAPER },
                );
            }
            let ring = |a: &mut Canvas, front: bool| {
                for i in 0..64 {
                    let t = i as f64 / 64.0 * std::f64::consts::TAU;
                    let (rx, ry) = (11.0 + 10.0 * t.cos(), 9.5 + 3.0 * t.sin() - 1.6 * t.cos());
                    if (t.sin() > 0.0) == front {
                        a.set(rx.round() as i32, ry.round() as i32, light(p.accent, 0.2));
                    }
                }
            };
            ring(&mut a, false);
            a.oval(5, 3, 13, 13, p.accent2);
            a.oval(6, 4, 6, 5, light(p.accent2, 0.35));
            a.hline(5, 11, 13, dark(p.accent2, 0.2));
            for yy in 3..16 {
                for xx in 13..18 {
                    if inside_oval(xx, yy, (5, 3, 13, 13)) && xx + yy > 26 {
                        a.set(xx, yy, dark(p.accent2, 0.3));
                    }
                }
            }
            ring(&mut a, true);
            text_bars(&mut a, 3, 18, &[16, 10], p.accent);
        }
        _ => {
            // Overlapping shapes, modernist style.
            let ground = 0xf2e6cc;
            a.rect(0, 0, aw, ah, ground);
            a.oval(1, 1, 13, 13, p.accent);
            for r in 0..12 {
                a.hline(19 - r, 3 + r, r * 2 / 3 + 1, p.accent2);
            }
            a.rect(9, 8, 7, 7, deep);
            for yy in 8..15 {
                for xx in 9..16 {
                    if inside_oval(xx, yy, (1, 1, 13, 13)) {
                        a.set(xx, yy, mix(deep, p.accent, 0.45));
                    }
                }
            }
            text_bars(&mut a, 2, 17, &[18, 12, 7], INK);
        }
    }
    c.blit(&a, x + 1, y + 1);
    // Tape at the corners.
    let tape = 0xf3ead2;
    for (tx, ty) in [
        (x - 1, y - 1),
        (x + w - 2, y - 1),
        (x - 1, y + h - 2),
        (x + w - 2, y + h - 2),
    ] {
        c.rect(tx, ty, 3, 2, tape);
        c.set(tx, ty, light(tape, 0.4));
    }
}

// --- Chalkboard -------------------------------------------------------------

fn chalkboard(c: &mut Canvas, p: &Pal, v: usize) {
    let slate = 0x2e3d36;
    let cord = 0xcdb68a;
    c.line((16, 1), (6, 4), cord);
    c.line((16, 1), (25, 4), cord);
    c.rect(15, 0, 2, 2, METAL);
    c.set(15, 0, light(METAL, 0.5));
    let area = framed(c, (2, 3, 28, 21), 2, p.wood, slate);
    let (x, y, w, h) = area;
    for i in 0..26 {
        let sx = x + hash(i * 3) % w;
        let sy = y + hash(i * 3 + 1) % h;
        c.set(sx, sy, mix(slate, CHALK, 0.12));
    }
    inset_shadow(c, area, 0.25);
    let dim = mix(slate, CHALK, 0.5);
    let pink = mix(0xf5a3c0, slate, 0.1);
    let yellow = mix(0xf6d55c, slate, 0.1);
    let mint = mix(0xa8e0a0, slate, 0.1);
    // Heading in chalk capitals with a flourish under it.
    c.stamp(
        6,
        6,
        &[
            "#...#.###.#..#.#..#",
            "##.##.#...##.#.#..#",
            "#.#.#.##..#.##.#..#",
            "#...#.###.#..#..##.",
        ],
        &[(b'#', CHALK)],
    );
    c.hline(7, 11, 7, dim);
    c.hline(17, 11, 7, dim);
    c.set(15, 11, pink);
    // Doodled cup with rising steam, opposite the menu lines.
    let left = v.is_multiple_of(2);
    let cup_x = if left { 4 } else { 19 };
    c.stamp(
        cup_x,
        13,
        &[
            "...p.p..", "..p.p...", ".#####..", ".#...##.", ".#...#.#", ".#...##.", "..###...",
            "#######.",
        ],
        &[(b'#', CHALK), (b'p', pink)],
    );
    let text_x = if left { 13 } else { 5 };
    let prices = [yellow, mint, yellow, pink];
    for (i, len) in [9, 7, 8, 6].into_iter().enumerate() {
        let row = 13 + i as i32 * 2;
        c.hline(text_x, row, len, dim);
        c.hline(text_x + 11, row, 2, prices[i]);
    }
    // Chalk ledge.
    slab(c, 1, 23, 30, 2, dark(p.wood, 0.1));
    c.hline(8, 22, 3, CHALK);
    c.hline(20, 22, 2, pink);
}

// --- Bunting ------------------------------------------------------------------

fn bunting(c: &mut Canvas, p: &Pal, v: usize) {
    let colors = [
        p.accent,
        0xf6eedb,
        p.accent2,
        p.fabric,
        mix(p.accent, 0xf6d55c, 0.5),
    ];
    let string = 0xf3ead6;
    let flag = ["#####", "#####", ".###.", ".###.", "..#.."];
    // Two staggered garlands, the lower one shorter.
    for (row, (x0, x1, top, sag)) in [(1, 30, 2, 6.0), (5, 26, 11, 4.0)].into_iter().enumerate() {
        let span = (x1 - x0) as f64;
        let y_at = |x: i32| {
            let t = (x - x0) as f64 / span * 2.0 - 1.0;
            top + (sag * (1.0 - t * t)).round() as i32
        };
        for x in x0..=x1 {
            c.set(x, y_at(x), string);
        }
        c.set(x0, top - 1, METAL);
        c.set(x1, top - 1, METAL);
        let mut i = row * 2 + v;
        let mut fx = x0 + 2 + row as i32 * 2;
        while fx + 4 < x1 {
            let color = colors[i % colors.len()];
            let fy = y_at(fx + 2) + 1;
            c.stamp(fx, fy, &flag, &[(b'#', color)]);
            c.hline(fx, fy, 5, dark(color, 0.22));
            c.vline(fx, fy + 1, 2, light(color, 0.25));
            c.set(fx + 4, fy + 1, dark(color, 0.18));
            if i % 3 == 1 {
                c.set(fx + 2, fy + 2, light(color, 0.6));
            }
            c.set(fx + 2, y_at(fx + 2), string);
            fx += 6;
            i += 1;
        }
    }
}

// --- Neon sign ----------------------------------------------------------------

/// A glass tube along `path` with a soft glow and a bright core, on `plate`.
fn tube(c: &mut Canvas, path: &[(i32, i32)], color: u32, plate: u32) {
    let mut core = Canvas::new(c.w, c.h);
    for pair in path.windows(2) {
        core.line(pair[0], pair[1], color);
    }
    let glow = mix(color, plate, 0.6);
    let lit = |c: &Canvas, x, y| c.get(x, y) == plate || c.get(x, y) == mix(color, plate, 0.6);
    for y in 0..c.h {
        for x in 0..c.w {
            if core.get(x, y) == CLEAR {
                continue;
            }
            for dy in -2..=2 {
                for dx in -2_i32..=2 {
                    if dx * dx + dy * dy <= 5 && c.get(x + dx, y + dy) == plate {
                        c.set(x + dx, y + dy, glow);
                    }
                }
            }
        }
    }
    for y in 0..c.h {
        for x in 0..c.w {
            if core.get(x, y) == CLEAR {
                continue;
            }
            for (dx, dy) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
                if lit(c, x + dx, y + dy) {
                    c.set(x + dx, y + dy, color);
                }
            }
        }
    }
    for y in 0..c.h {
        for x in 0..c.w {
            if core.get(x, y) != CLEAR {
                c.set(x, y, light(color, 0.65));
            }
        }
    }
}

fn neon(c: &mut Canvas, p: &Pal, v: usize) {
    let plate = 0x1c1730;
    c.round(1, 1, 30, 24, 2, light(plate, 0.2));
    c.round(2, 2, 29, 23, 2, dark(plate, 0.45));
    c.round(2, 2, 28, 22, 2, plate);
    for (sx, sy) in [(4, 4), (27, 4), (4, 21), (27, 21)] {
        c.set(sx, sy, METAL);
    }
    let (main, second) = (p.accent, p.accent2);
    match v % 3 {
        0 => {
            let heart: Vec<(i32, i32)> = (0..=48)
                .map(|i| {
                    let t = i as f64 / 48.0 * std::f64::consts::TAU;
                    let hx = 16.0 * t.sin().powi(3);
                    let hy = 13.0 * t.cos()
                        - 5.0 * (2.0 * t).cos()
                        - 2.0 * (3.0 * t).cos()
                        - (4.0 * t).cos();
                    (
                        (15.5 + hx * 0.55).round() as i32,
                        (6.5 + (12.0 - hy) * 0.5).round() as i32,
                    )
                })
                .collect();
            tube(c, &heart, main, plate);
            tube(c, &[(24, 5), (24, 7)], second, plate);
            tube(c, &[(23, 6), (25, 6)], second, plate);
        }
        1 => {
            tube(
                c,
                &[
                    (8, 10),
                    (9, 18),
                    (10, 19),
                    (19, 19),
                    (20, 18),
                    (21, 10),
                    (8, 10),
                ],
                main,
                plate,
            );
            tube(c, &[(21, 12), (24, 12), (24, 15), (20, 16)], main, plate);
            tube(c, &[(11, 7), (12, 6), (11, 5), (12, 4)], second, plate);
            tube(c, &[(16, 7), (17, 6), (16, 5), (17, 4)], second, plate);
            tube(c, &[(6, 22), (23, 22)], second, plate);
        }
        _ => {
            tube(c, &[(19, 3), (11, 13), (19, 13), (11, 22)], main, plate);
            tube(c, &[(6, 6), (6, 8)], second, plate);
            tube(c, &[(5, 7), (7, 7)], second, plate);
            tube(c, &[(24, 17), (24, 19)], second, plate);
            tube(c, &[(23, 18), (25, 18)], second, plate);
        }
    }
}

// --- Porthole -----------------------------------------------------------------

fn porthole(c: &mut Canvas, p: &Pal, v: usize) {
    let (cx, cy) = (16.0, 13.0);
    let (outer, inner) = (12.0, 8.6);
    let ring = mix(METAL, p.t.trim, 0.25);
    let deep = 0x0d1430;
    let space = 0x24356a;
    let dist = |x: i32, y: i32, ox: f64, oy: f64| {
        let (dx, dy) = (x as f64 + 0.5 - ox, y as f64 + 0.5 - oy);
        ((dx * dx + dy * dy).sqrt(), dx, dy)
    };
    for y in 0..26 {
        for x in 0..32 {
            let (d, dx, dy) = dist(x, y, cx, cy);
            if d > outer {
                continue;
            }
            let color = if d <= inner - 1.0 {
                mix(deep, space, (y as f64 - 4.0) / 18.0)
            } else if d <= inner {
                // Gasket: shadowed above, catching light below.
                if dy > 2.0 {
                    dark(ring, 0.1)
                } else {
                    dark(ring, 0.62)
                }
            } else {
                ramp(
                    ring,
                    ring_light(dx, dy, (outer + inner) / 2.0, (outer - inner) / 2.0),
                )
            };
            c.set(x, y, color);
        }
    }
    let glass = |x: i32, y: i32| dist(x, y, cx, cy).0 <= inner - 1.0;
    for i in 0..18 {
        let (sx, sy) = (7 + hash(i) % 18, 4 + hash(i + 50) % 18);
        if glass(sx, sy) {
            c.set(sx, sy, if i % 4 == 0 { 0xfff3c4 } else { 0x8a9fd8 });
        }
    }
    // A planet filling the lower right of the view.
    let (px, py, pr) = (19.5, 18.0, 7.5);
    let (body, band) = match v % 3 {
        0 => (0x3f7fd0, 0x5fbf6a),
        1 => (p.accent, light(p.accent, 0.3)),
        _ => (0xb48ad8, 0xd9b8ee),
    };
    for y in 0..26 {
        for x in 0..32 {
            let (d, dx, dy) = dist(x, y, px, py);
            if !glass(x, y) || d > pr {
                continue;
            }
            let mut color = body;
            if v.is_multiple_of(3) {
                // Continents and a cloud streak.
                let land = hash((x / 2) * 7 + (y / 2) * 13) % 3 == 0;
                if land {
                    color = band;
                }
                if (x + 2 * y) % 11 == 0 {
                    color = 0xf2f6ff;
                }
            } else if (y as f64 - py + dx * 0.25).round() as i32 % 3 == 0 {
                color = band;
            }
            let lit = -(dx * 0.6 + dy * 0.8) / pr;
            if d > pr - 1.2 && lit > 0.2 {
                color = light(color, 0.5);
            } else if lit < -0.25 {
                color = dark(color, 0.35);
            }
            c.set(x, y, color);
        }
    }
    if v % 3 == 1 {
        // Rings crossing in front.
        for x in 10..26 {
            let y = (18.0 - (x as f64 - 18.0) * 0.45).round() as i32;
            if glass(x, y) {
                c.set(x, y, light(p.accent2, 0.35));
            }
        }
    } else {
        // A small moon.
        c.set(11, 8, 0xe8e2d0);
        c.set(12, 8, 0xc9c2b0);
        c.set(11, 9, 0xc9c2b0);
    }
    // Glare across the glass.
    c.line((9, 11), (13, 7), mix(space, 0xffffff, 0.4));
    c.line((10, 13), (11, 12), mix(space, 0xffffff, 0.28));
    // Rivets round the ring.
    for k in 0..8 {
        let t = (k as f64 + 0.5) / 8.0 * std::f64::consts::TAU;
        let rx = (cx - 1.0 + 10.3 * t.cos()).round() as i32;
        let ry = (cy - 1.0 + 10.3 * t.sin()).round() as i32;
        c.set(rx, ry, light(ring, 0.7));
        c.set(rx + 1, ry, ring);
        c.set(rx, ry + 1, ring);
        c.set(rx + 1, ry + 1, dark(ring, 0.5));
    }
}

// --- Skis ---------------------------------------------------------------------

/// One ski as a plank from `tail` to `tip`, lit along its left edge, with a
/// racing stripe, a steel binding and a bright upturned tip. The ski in front
/// gets a dark rim where it crosses the other.
fn ski(c: &mut Canvas, tail: (f64, f64), tip: (f64, f64), color: u32, band: u32, front: bool) {
    let (dx, dy) = (tip.0 - tail.0, tip.1 - tail.1);
    let len2 = dx * dx + dy * dy;
    let len = len2.sqrt();
    let half = 1.55;
    let source = c.clone();
    for y in 0..c.h {
        for x in 0..c.w {
            let (px, py) = (x as f64 + 0.5 - tail.0, y as f64 + 0.5 - tail.1);
            let t = ((px * dx + py * dy) / len2).clamp(0.0, 1.0);
            let (qx, qy) = (px - t * dx, py - t * dy);
            let dist = (qx * qx + qy * qy).sqrt();
            // Signed offset across the ski: negative on the upper-left side.
            let side = (dx * py - dy * px) / len;
            if dist > half {
                if front && dist <= half + 1.0 && source.get(x, y) != CLEAR && t > 0.0 && t < 1.0 {
                    c.set(x, y, dark(source.get(x, y), 0.55));
                }
                continue;
            }
            let along = t * len;
            let mut shade = if side < -0.55 {
                light(color, 0.35)
            } else if side > 0.55 {
                dark(color, 0.3)
            } else if (1.0..len - 3.0).contains(&along) {
                band
            } else {
                color
            };
            if along > len - 2.5 {
                shade = light(color, if side > 0.55 { 0.15 } else { 0.5 });
            } else if (7.0..8.0).contains(&along) {
                shade = light(METAL, 0.3);
            } else if (8.0..9.0).contains(&along) {
                shade = dark(METAL, 0.25);
            }
            c.set(x, y, shade);
        }
    }
}

fn pole(c: &mut Canvas, x: i32, accent: u32) {
    let grip = 0x3a3542;
    // Wrist strap looped over a hook.
    c.set(x, 0, BRASS);
    c.set(x - 1, 1, accent);
    c.set(x + 1, 1, dark(accent, 0.3));
    c.set(x - 1, 2, accent);
    c.set(x + 1, 2, dark(accent, 0.3));
    c.rect(x - 1, 3, 3, 4, grip);
    c.vline(x - 1, 3, 4, light(grip, 0.35));
    c.hline(x - 1, 3, 3, light(grip, 0.2));
    c.set(x - 1, 3, light(grip, 0.45));
    c.vline(x, 7, 17, METAL);
    c.vline(x, 7, 10, light(METAL, 0.3));
    c.hline(x - 2, 20, 5, grip);
    c.set(x - 2, 19, light(grip, 0.4));
    c.set(x + 2, 19, grip);
    c.set(x, 24, dark(METAL, 0.35));
}

fn skis(c: &mut Canvas, p: &Pal) {
    // Skis in the room fabric with an accent stripe, swapped when the fabric
    // would sink into the wall.
    let (color, stripe) = if (luma(p.fabric) - luma(p.t.wall_colors.0)).abs() < 0.14 {
        (p.accent, p.fabric)
    } else {
        (p.fabric, p.accent)
    };
    pole(c, 3, p.accent2);
    pole(c, 28, p.accent2);
    ski(c, (21.5, 24.5), (11.0, 1.0), color, stripe, false);
    ski(c, (10.5, 24.5), (21.0, 1.0), color, stripe, true);
    // A strap binds the pair where they cross.
    c.rect(13, 11, 6, 2, p.accent2);
    c.hline(13, 11, 6, light(p.accent2, 0.4));
    c.hline(13, 12, 6, dark(p.accent2, 0.2));
    c.set(15, 11, BRASS);
}

// --- Lifebuoy -----------------------------------------------------------------

fn lifebuoy(c: &mut Canvas, p: &Pal) {
    let (cx, cy) = (16.0, 13.5);
    let (outer, inner) = (10.5, 4.8);
    let red = mix(0xe0463c, p.accent, 0.2);
    let white = 0xf7f3ea;
    let polar = |r: f64, deg: f64| {
        let t = deg.to_radians();
        (
            (cx - 0.5 + r * t.cos()).round() as i32,
            (cy - 0.5 + r * t.sin()).round() as i32,
        )
    };
    // Grab line first: it loops outside the ring between four lashings.
    let rope = ROPE;
    let twist = dark(ROPE, 0.32);
    for k in 0..4 {
        let start = 45.0 + 90.0 * k as f64;
        for s in 0..=36 {
            let f = s as f64 / 36.0;
            let r = outer + 0.6 + (f * std::f64::consts::PI).sin() * 1.1;
            let (x, y) = polar(r, start + f * 90.0);
            let (sx, sy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
            c.set(x, y, if sx + sy > 4.0 { twist } else { rope });
        }
    }
    for y in 0..26 {
        for x in 0..32 {
            let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
            let d = (dx * dx + dy * dy).sqrt();
            if d > outer || d < inner {
                continue;
            }
            let angle = dy.atan2(dx).to_degrees().rem_euclid(360.0);
            let base = if ((angle + 22.5) / 45.0) as i32 % 2 == 0 {
                red
            } else {
                white
            };
            let lit = ring_light(dx, dy, (outer + inner) / 2.0, (outer - inner) / 2.0);
            c.set(x, y, ramp(base, lit));
        }
    }
    // Lashings wrap the white bands.
    for k in 0..4 {
        let deg = 45.0 + 90.0 * k as f64;
        for r in [5.2, 6.2, 7.2, 8.2, 9.2, 10.2, 11.0] {
            let (x, y) = polar(r, deg);
            c.set(x, y, dark(ROPE, 0.12));
            let (x, y) = polar(r, deg + 8.0);
            c.set(x, y, dark(ROPE, 0.4));
        }
    }
    // Hook.
    c.rect(15, 0, 2, 2, METAL);
    c.set(15, 0, light(METAL, 0.5));
}

// --- Hanging scroll -----------------------------------------------------------

fn scroll(c: &mut Canvas, p: &Pal, v: usize) {
    let mount = against_wall(p, mix(p.accent2, 0x6d7f5c, 0.35), dark(p.accent2, 0.2));
    let rod = 0x4a3426;
    let paper = 0xf4ecd6;
    let ink = 0x2b2630;
    // Cord from a nail.
    c.line((16, 1), (9, 4), dark(ROPE, 0.25));
    c.line((16, 1), (22, 4), dark(ROPE, 0.25));
    c.set(16, 0, METAL);
    c.rect(7, 4, 18, 2, rod);
    c.hline(7, 4, 18, light(rod, 0.3));
    // Mounting cloth with brocade bands above and below the painting.
    c.rect(8, 6, 16, 16, mount);
    c.vline(8, 6, 16, light(mount, 0.2));
    c.vline(23, 6, 16, dark(mount, 0.3));
    for y in [7, 20] {
        for x in (9..23).step_by(2) {
            c.set(x, y, light(mount, 0.3));
        }
    }
    // Two hanging strips from the top rod.
    c.vline(12, 6, 3, dark(mount, 0.35));
    c.vline(19, 6, 3, dark(mount, 0.35));
    let area = (10, 9, 12, 11);
    c.rect(area.0, area.1, area.2, area.3, paper);
    inset_shadow(c, area, 0.12);
    match v % 3 {
        0 => {
            // A single brush circle, heavy on the left, open at the top right.
            for k in 0..44 {
                let t = (k as f64 / 44.0) * 330.0 + 300.0;
                let r = t.to_radians();
                let (ex, ey) = (15.5 + 4.2 * r.cos(), 14.2 + 4.0 * r.sin());
                c.set(ex.round() as i32, ey.round() as i32, ink);
                if (120.0..300.0).contains(&(t % 360.0)) {
                    c.set(
                        ex.round() as i32 + 1,
                        ey.round() as i32,
                        mix(ink, paper, 0.25),
                    );
                }
            }
        }
        1 => {
            // Two columns of brush strokes.
            for (col, strokes) in [(17, [3, 2, 3]), (13, [2, 3, 2])] {
                let mut yy = 10;
                for len in strokes {
                    c.vline(col, yy, len, ink);
                    c.set(col + 1, yy, ink);
                    c.set(col - 1, yy + len - 1, mix(ink, paper, 0.4));
                    yy += len + 1;
                }
            }
            c.hline(12, 12, 3, ink);
            c.line((16, 16), (18, 14), ink);
        }
        _ => {
            // Ink bamboo.
            let wash = mix(ink, paper, 0.45);
            c.vline(13, 9, 11, ink);
            c.vline(17, 11, 9, wash);
            for y in [12, 16] {
                c.set(13, y, paper);
            }
            c.line((14, 11), (18, 9), ink);
            c.line((14, 13), (19, 12), ink);
            c.line((12, 15), (10, 13), ink);
            c.line((18, 13), (20, 11), wash);
        }
    }
    // Red seal.
    c.rect(19, 17, 2, 2, mix(p.accent, 0xc7473f, 0.5));
    c.set(19, 17, light(p.accent, 0.3));
    // Weighted bottom roller with end knobs.
    c.rect(7, 22, 18, 2, rod);
    c.hline(7, 22, 18, light(rod, 0.3));
    c.rect(6, 21, 2, 4, dark(rod, 0.2));
    c.rect(24, 21, 2, 4, dark(rod, 0.2));
    c.set(6, 21, light(rod, 0.4));
    c.set(24, 21, light(rod, 0.4));
}
