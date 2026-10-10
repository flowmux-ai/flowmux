// SPDX-License-Identifier: GPL-3.0-or-later
//! Large focal pieces, 48x48 art pixels, standing on a 44x16 footprint at the bottom.
//!
//! The scene paints moving parts over some of these sprites, so those spots keep
//! dark slots here: status lights at `(10 + 12 * i, 14 | 24)`, a light strip at
//! `(20, 10)` and flames rising from `(16..30, 36)`.

use super::{Pal, INK, METAL, PAPER, SCREEN};
use crate::ui::agent_office::{
    sprite::{dark, light, mix, Canvas},
    theme::Feature,
};

const LEAF: u32 = 0x5f9a58;
const CLAY: u32 = 0xc8774f;
const SOIL: u32 = 0x4b3427;
const BRASS: u32 = 0xd6a446;
const BARK: u32 = 0x6e4c34;
const SLOT: u32 = 0x1b2130;
const SUN: u32 = 0xf6d55c;
const GLOW: u32 = 0xffe2a0;
const GLASS: u32 = 0xd6ecf0;

pub(in crate::ui::agent_office) fn draw(c: &mut Canvas, p: &Pal, feature: Feature, v: usize) {
    match feature {
        Feature::Easel => easel(c, p, v),
        Feature::Bookcase => bookcase(c, p, v),
        Feature::Planter => planter(c, p, v),
        Feature::PottingBench => potting_bench(c, p, v),
        Feature::ServerRack => server_rack(c, p, v),
        Feature::Telescope => telescope(c, p, v),
        Feature::Globe => globe(c, p, v),
        Feature::Workbench => workbench(c, p, v),
        Feature::Printer3d => printer3d(c, p, v),
        Feature::EspressoBar => espresso_bar(c, p, v),
        Feature::PastryCase => pastry_case(c, p, v),
        Feature::Arcade => arcade(c, p, v),
        Feature::Vending => vending(c, p, v),
        Feature::HydroPod => hydro_pod(c, p, v),
        Feature::Console => console(c, p, v),
        Feature::Fireplace => fireplace(c, p, v),
        Feature::WoodPile => wood_pile(c, p, v),
        Feature::SurfRack => surf_rack(c, p, v),
        Feature::DeckChair => deck_chair(c, p, v),
        Feature::Bonsai => bonsai(c, p, v),
        Feature::StoneLantern => stone_lantern(c, p, v),
        Feature::Gumball => gumball(c, p, v),
    }
}

fn hash(i: i32) -> i32 {
    ((i as u32).wrapping_mul(2_654_435_761) >> 9) as i32
}

/// A box seen from the front and a little above: `top` rows of lit top face, then the front.
fn block(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, top: i32, color: u32) {
    c.rect(x, y, w, h, color);
    c.rect(x, y, w, top, light(color, 0.25));
    c.hline(x, y, w, light(color, 0.42));
    c.vline(x + w - 1, y + top, h - top, dark(color, 0.18));
    c.hline(x, y + h - 1, w, dark(color, 0.35));
}

/// Shaded cylinder from `a` to `b` with flat ends; its upper side is lit.
fn rod(c: &mut Canvas, a: (f64, f64), b: (f64, f64), r0: f64, r1: f64, color: u32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = (dx * dx + dy * dy).max(1e-6);
    let len = len2.sqrt();
    let (mut nx, mut ny) = (-dy / len, dx / len);
    if nx + ny > 0.0 {
        (nx, ny) = (-nx, -ny);
    }
    let pad = r0.max(r1) + 1.0;
    let (x0, x1) = ((a.0.min(b.0) - pad) as i32, (a.0.max(b.0) + pad) as i32);
    let (y0, y1) = ((a.1.min(b.1) - pad) as i32, (a.1.max(b.1) + pad) as i32);
    for y in y0..=y1 {
        for x in x0..=x1 {
            let (qx, qy) = (x as f64 + 0.5 - a.0, y as f64 + 0.5 - a.1);
            let t = (qx * dx + qy * dy) / len2;
            if !(0.0..=1.0).contains(&t) {
                continue;
            }
            let (ex, ey) = (qx - t * dx, qy - t * dy);
            let r = r0 + (r1 - r0) * t;
            if ex * ex + ey * ey > r * r {
                continue;
            }
            let side = (ex * nx + ey * ny) / r.max(0.5);
            let color = if side > 0.4 {
                light(color, 0.3)
            } else if side < -0.45 {
                dark(color, 0.3)
            } else {
                color
            };
            c.set(x, y, color);
        }
    }
}

/// Shaded sphere in a `d`-wide box; `tex` gives the base color at each offset.
fn ball(c: &mut Canvas, x: i32, y: i32, d: i32, tex: impl Fn(i32, i32) -> u32) {
    let r = d as f64 / 2.0;
    for yy in 0..d {
        for xx in 0..d {
            let nx = (xx as f64 + 0.5 - r) / r;
            let ny = (yy as f64 + 0.5 - r) / r;
            let q = nx * nx + ny * ny;
            if q > 1.0 {
                continue;
            }
            let lit = -0.45 * nx - 0.55 * ny + 0.7 * (1.0 - q).sqrt();
            let base = tex(xx, yy);
            let color = if lit > 0.92 {
                light(base, 0.5)
            } else if lit > 0.62 {
                light(base, 0.16)
            } else if lit > 0.2 {
                base
            } else if lit > -0.15 {
                dark(base, 0.2)
            } else {
                dark(base, 0.36)
            };
            c.set(x + xx, y + yy, color);
        }
    }
}

/// A leafy mound: shaded body with a lit crown.
fn clump(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, leaf: u32) {
    c.oval(x, y, w, h, dark(leaf, 0.3));
    c.oval(x, y, w - 1, h - 1, leaf);
    c.oval(x + 1, y, w * 3 / 5, h / 2, light(leaf, 0.2));
    c.set(x + w / 4 + 1, y + 1, light(leaf, 0.45));
}

/// Flower pot with soil showing inside the rim.
fn pot(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, color: u32) {
    c.hline(x, y, w, light(color, 0.35));
    c.hline(x + 1, y, w - 2, SOIL);
    c.hline(x, y + 1, w, light(color, 0.12));
    for r in 2..h {
        let inset = if w >= 7 && r * 2 >= h { 2 } else { 1 };
        c.hline(x + inset, y + r, w - inset * 2, color);
        c.set(x + w - inset - 1, y + r, dark(color, 0.22));
    }
    c.hline(x + 2, y + h - 1, w - 4, dark(color, 0.3));
}

/// Small blossom: four petals around a center.
fn bloom(c: &mut Canvas, x: i32, y: i32, petal: u32, heart: u32) {
    for (dx, dy) in [(0, -1), (-1, 0), (1, 0), (0, 1)] {
        c.set(x + dx, y + dy, petal);
    }
    c.set(x - 1, y - 1, light(petal, 0.3));
    c.set(x, y, heart);
}

fn cup(c: &mut Canvas, x: i32, y: i32, color: u32, drink: u32) {
    c.rect(x, y, 4, 3, color);
    c.hline(x, y, 4, light(color, 0.4));
    c.hline(x + 1, y, 2, drink);
    c.set(x + 3, y + 1, dark(color, 0.2));
    c.set(x + 4, y + 1, color);
    c.hline(x, y + 3, 5, dark(color, 0.15));
}

// ---------------------------------------------------------------- atelier

fn easel(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    // Paint-spattered drop cloth under the easel.
    let cloth = mix(PAPER, wood, 0.18);
    c.round(2, 40, 32, 6, 2, cloth);
    c.hline(4, 40, 28, light(cloth, 0.35));
    c.hline(4, 45, 28, dark(cloth, 0.25));
    c.line((14, 41), (18, 45), dark(cloth, 0.1));
    c.line((25, 41), (23, 45), dark(cloth, 0.1));
    for (x, y, color) in [
        (5, 42, p.accent),
        (10, 44, p.accent2),
        (21, 42, SUN),
        (28, 43, p.accent),
        (17, 43, p.accent2),
    ] {
        c.set(x, y, color);
        c.set(x + 1, y, dark(color, 0.15));
    }
    // A spare canvas leaning behind the taboret, stretcher side out.
    c.rect(35, 11, 11, 23, light(wood, 0.1));
    c.rect(36, 12, 9, 21, mix(PAPER, wood, 0.35));
    c.hline(36, 21, 9, light(wood, 0.1));
    c.vline(40, 12, 21, light(wood, 0.1));
    c.vline(45, 11, 23, dark(wood, 0.3));
    c.hline(35, 11, 11, light(wood, 0.4));
    // Taboret with a brush jar and paint tubes.
    let cab = mix(p.accent2, PAPER, 0.3);
    block(c, 33, 30, 13, 15, 3, cab);
    for y in [36, 40] {
        c.hline(34, y, 11, dark(cab, 0.3));
    }
    for y in [34, 38, 42] {
        c.rect(38, y, 3, 1, METAL);
    }
    c.set(34, 45, INK);
    c.set(44, 45, INK);
    for (x0, x1, top, tip) in [
        (35, 33, 20, p.accent),
        (36, 36, 18, p.accent2),
        (37, 39, 20, SUN),
    ] {
        c.line((x0, 26), (x1, top + 1), dark(wood, 0.4));
        c.set(x1, top, tip);
        c.set(x1, top + 1, dark(tip, 0.2));
    }
    c.rect(34, 25, 5, 5, GLASS);
    c.hline(34, 25, 5, light(GLASS, 0.4));
    c.rect(34, 27, 5, 3, mix(GLASS, p.accent2, 0.4));
    c.vline(38, 26, 4, dark(GLASS, 0.25));
    for (y, color) in [(28, p.accent), (29, SUN)] {
        c.hline(40, y, 4, color);
        c.set(44, y, METAL);
        c.set(40, y, light(color, 0.35));
    }
    // Back leg, splayed front legs and the mast.
    c.line((18, 28), (23, 41), dark(wood, 0.45));
    for (top, foot) in [((16, 3), (5, 44)), ((20, 3), (31, 44))] {
        c.line(top, foot, dark(wood, 0.05));
        c.line((top.0 + 1, top.1), (foot.0 + 1, foot.1), dark(wood, 0.3));
        c.rect(foot.0, foot.1, 3, 1, dark(wood, 0.45));
    }
    c.rect(17, 1, 3, 36, wood);
    c.vline(17, 1, 36, light(wood, 0.3));
    c.vline(19, 1, 36, dark(wood, 0.25));
    c.rect(16, 33, 5, 2, dark(wood, 0.2));
    c.set(21, 33, METAL);
    // The canvas, its painting and the ledge with a palette.
    c.rect(4, 5, 28, 21, PAPER);
    c.vline(31, 5, 21, dark(PAPER, 0.3));
    c.hline(4, 25, 28, dark(PAPER, 0.2));
    c.blit(&painting(p, v), 5, 6);
    c.rect(3, 26, 31, 3, wood);
    c.hline(3, 26, 31, light(wood, 0.35));
    c.hline(3, 28, 31, dark(wood, 0.35));
    let board = 0xe6c995;
    c.oval(1, 24, 11, 6, board);
    c.hline(2, 24, 8, light(board, 0.35));
    c.hline(3, 29, 7, dark(board, 0.3));
    for (x, y, color) in [
        (3, 25, p.accent),
        (5, 25, p.accent2),
        (7, 25, SUN),
        (3, 27, PAPER),
        (6, 27, LEAF),
    ] {
        c.set(x, y, color);
        c.set(x + 1, y, dark(color, 0.15));
    }
    c.set(9, 27, dark(board, 0.4));
    c.rect(15, 3, 7, 3, dark(wood, 0.2));
    c.hline(15, 3, 7, light(wood, 0.15));
    c.set(22, 4, METAL);
}

fn painting(p: &Pal, v: usize) -> Canvas {
    let mut a = Canvas::new(26, 19);
    match v % 3 {
        0 => {
            let (high, low) = (mix(p.accent2, PAPER, 0.3), mix(p.accent2, PAPER, 0.72));
            for y in 0..12 {
                a.hline(0, y, 26, mix(high, low, y as f64 / 11.0));
            }
            a.hline(2, 4, 6, light(low, 0.5));
            a.hline(4, 3, 3, light(low, 0.5));
            a.oval(18, 2, 5, 5, SUN);
            a.set(19, 3, 0xfff4c8);
            a.oval(-6, 8, 22, 14, 0x8cbf72);
            a.oval(10, 10, 24, 14, LEAF);
            a.rect(0, 15, 26, 4, dark(LEAF, 0.12));
            a.rect(5, 10, 6, 4, PAPER);
            for (i, w) in [8, 6, 4].into_iter().enumerate() {
                a.hline(4 + i as i32, 9 - i as i32, w, p.accent);
            }
            a.vline(7, 12, 2, INK);
            a.set(9, 11, 0x8fc1e3);
            a.rect(20, 11, 1, 4, 0x6b4630);
            a.oval(18, 5, 5, 7, dark(LEAF, 0.3));
            a.set(19, 6, LEAF);
            a.line((8, 14), (12, 18), 0xe8d6a8);
        }
        1 => {
            let wall = mix(p.accent2, PAPER, 0.62);
            a.rect(0, 0, 26, 19, wall);
            for x in (2..26).step_by(5) {
                a.vline(x, 0, 13, light(wall, 0.25));
            }
            let table = mix(p.wood, p.accent, 0.25);
            a.rect(0, 13, 26, 6, table);
            a.hline(0, 13, 26, light(table, 0.35));
            let stem = dark(LEAF, 0.1);
            a.line((12, 9), (8, 4), stem);
            a.line((13, 9), (14, 3), stem);
            a.line((14, 9), (19, 5), stem);
            a.set(10, 7, LEAF);
            a.set(17, 7, LEAF);
            a.oval(5, 1, 6, 5, p.accent);
            a.set(7, 3, SUN);
            a.oval(12, 0, 5, 5, SUN);
            a.set(14, 2, p.accent);
            a.oval(17, 3, 6, 5, PAPER);
            a.set(19, 5, SUN);
            let vase = dark(p.accent2, 0.15);
            a.rect(11, 8, 4, 2, vase);
            a.oval(9, 9, 8, 7, vase);
            a.vline(10, 11, 3, light(vase, 0.35));
            a.oval(18, 12, 4, 4, 0xf0a040);
            a.set(19, 12, light(0xf0a040, 0.45));
            a.oval(2, 13, 5, 4, 0xd9484f);
            a.set(3, 13, light(0xd9484f, 0.4));
        }
        _ => {
            let bg = mix(p.accent, PAPER, 0.55);
            a.rect(0, 0, 26, 19, bg);
            a.oval(3, 1, 20, 18, light(bg, 0.25));
            let fur = 0xe39b52;
            a.oval(5, 13, 16, 10, dark(fur, 0.12));
            a.oval(7, 4, 12, 10, fur);
            a.stamp(8, 2, &["k..", "kk.", "kkk"], &[(b'k', fur)]);
            a.stamp(15, 2, &["..k", ".kk", "kkk"], &[(b'k', fur)]);
            a.set(9, 4, 0xf2a0a0);
            a.set(16, 4, 0xf2a0a0);
            for x in [11, 13, 15] {
                a.vline(x, 5, 2, dark(fur, 0.25));
            }
            a.rect(10, 8, 1, 2, INK);
            a.rect(15, 8, 1, 2, INK);
            a.hline(11, 11, 4, PAPER);
            a.hline(12, 10, 2, 0xf28a8f);
            a.hline(9, 14, 8, p.accent2);
            a.set(13, 15, SUN);
        }
    }
    a
}

fn bookcase(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    let back = dark(wood, 0.55);
    c.rect(3, 4, 42, 40, wood);
    c.vline(3, 5, 38, light(wood, 0.22));
    c.vline(44, 5, 38, dark(wood, 0.3));
    // Crown and plinth.
    c.rect(2, 1, 44, 4, light(wood, 0.08));
    c.hline(2, 1, 44, light(wood, 0.45));
    c.hline(2, 2, 44, light(wood, 0.25));
    c.hline(2, 4, 44, dark(wood, 0.25));
    c.rect(2, 43, 44, 3, dark(wood, 0.2));
    c.hline(2, 43, 44, light(wood, 0.1));
    c.hline(2, 45, 44, dark(wood, 0.45));
    // Three open shelves split by a divider.
    let shelves = [6, 16, 26];
    for y in shelves {
        for x in [6, 25] {
            c.rect(x, y, 17, 8, back);
            c.hline(x, y, 17, dark(back, 0.35));
            c.vline(x, y, 8, dark(back, 0.2));
        }
        c.rect(5, y + 8, 38, 2, wood);
        c.hline(5, y + 8, 38, light(wood, 0.3));
        c.hline(5, y + 9, 38, dark(wood, 0.12));
    }
    // Cabinet doors.
    for x in [6, 25] {
        c.rect(x, 36, 17, 7, dark(wood, 0.06));
        c.rect(x + 2, 37, 13, 5, dark(wood, 0.16));
        c.hline(x + 2, 37, 13, dark(wood, 0.35));
        c.hline(x + 2, 41, 13, light(wood, 0.2));
    }
    c.vline(23, 36, 7, dark(wood, 0.35));
    c.set(21, 39, BRASS);
    c.set(27, 39, BRASS);
    for (i, y) in shelves.into_iter().enumerate() {
        for (j, x) in [6, 25].into_iter().enumerate() {
            let bay = (i * 2 + j + v * 2) % 6;
            shelf(c, p, x, y + 7, bay, (i * 2 + j) as i32 + v as i32 * 7);
        }
    }
}

fn book_colors(p: &Pal) -> [u32; 7] {
    [
        p.accent,
        p.accent2,
        p.fabric,
        0xe9dcb8,
        mix(p.accent, INK, 0.4),
        0x5b7f6a,
        0xd9a647,
    ]
}

/// Upright spines from `x` across `w` pixels, standing on `floor`.
fn spines(c: &mut Canvas, p: &Pal, x: i32, floor: i32, w: i32, tall: i32, seed: i32) {
    let colors = book_colors(p);
    let mut bx = x;
    let mut i = 0;
    let mut last = usize::MAX;
    while bx < x + w {
        let h = hash(seed * 31 + i);
        let bw = (2 + h % 2).min(x + w - bx);
        let bh = tall - (h >> 3) % 3;
        let mut k = (h >> 5) as usize % colors.len();
        if k == last {
            k = (k + 1) % colors.len();
        }
        last = k;
        let color = colors[k];
        let top = floor - bh + 1;
        c.rect(bx, top, bw, bh, color);
        c.hline(bx, top, bw, light(color, 0.3));
        c.vline(bx + bw - 1, top + 1, bh - 1, dark(color, 0.2));
        if (h >> 7) % 3 == 0 {
            c.hline(bx, top + 2, bw - 1, mix(color, 0xf3d68a, 0.7));
        }
        bx += bw;
        i += 1;
    }
}

fn shelf(c: &mut Canvas, p: &Pal, x: i32, floor: i32, bay: usize, seed: i32) {
    let colors = book_colors(p);
    match bay {
        0 => spines(c, p, x + 1, floor, 16, 7, seed),
        1 => {
            spines(c, p, x + 1, floor, 10, 7, seed);
            // A leaning book and a bookend.
            let color = colors[(seed as usize + 2) % colors.len()];
            for r in 0..6 {
                c.rect(
                    x + 11 + r / 2,
                    floor - r,
                    2,
                    1,
                    if r == 5 { light(color, 0.3) } else { color },
                );
            }
            c.rect(x + 15, floor - 3, 2, 4, METAL);
            c.hline(x + 15, floor - 3, 2, light(METAL, 0.4));
        }
        2 => {
            // Trailing plant, then books.
            pot(c, x + 1, floor - 3, 6, 4, CLAY);
            clump(c, x, floor - 9, 8, 7, LEAF);
            for (dx, len) in [(1, 6), (5, 4)] {
                c.vline(x + dx, floor - 2, len, dark(LEAF, 0.1));
                c.set(x + dx + 1, floor + len - 3, LEAF);
            }
            spines(c, p, x + 8, floor, 9, 7, seed);
        }
        3 => {
            // A flat stack crowned by a porcelain cat.
            for (i, w) in [9, 8, 9].into_iter().enumerate() {
                let color = colors[(seed as usize + i * 3) % colors.len()];
                let y = floor - 1 - i as i32 * 2;
                c.rect(x + 1 + i as i32 % 2, y, w, 2, color);
                c.hline(x + 1 + i as i32 % 2, y, w, light(color, 0.3));
            }
            c.stamp(
                x + 3,
                floor - 13,
                &[
                    "k...k", "kkkkk", "kekek", "kknkk", ".ccc.", "kkkkk", "kkkkk",
                ],
                &[
                    (b'k', PAPER),
                    (b'e', INK),
                    (b'n', 0xf28a8f),
                    (b'c', p.accent),
                ],
            );
            spines(c, p, x + 11, floor, 6, 7, seed);
        }
        4 => {
            spines(c, p, x + 1, floor, 9, 7, seed);
            // A framed photo.
            c.rect(x + 11, floor - 6, 6, 7, p.t.trim);
            c.rect(x + 12, floor - 5, 4, 3, 0x9fd0ee);
            c.rect(x + 12, floor - 2, 4, 2, LEAF);
            c.set(x + 13, floor - 3, SUN);
            c.hline(x + 11, floor - 6, 6, light(p.t.trim, 0.35));
        }
        _ => {
            // A small globe on a stand, then books.
            c.vline(x + 4, floor - 1, 2, BRASS);
            c.hline(x + 2, floor, 5, dark(BRASS, 0.2));
            ball(c, x + 1, floor - 7, 7, |xx, yy| {
                if (xx + yy * 2) % 5 < 2 && yy > 1 {
                    0x8fb36a
                } else {
                    0x4f8fbf
                }
            });
            spines(c, p, x + 9, floor, 8, 7, seed);
        }
    }
}

// ---------------------------------------------------------------- conservatory

fn planter(c: &mut Canvas, p: &Pal, v: usize) {
    let timber = mix(p.wood, 0x9a6a42, 0.35);
    // Back board and soil.
    c.rect(3, 26, 42, 2, light(timber, 0.3));
    c.rect(5, 28, 38, 5, SOIL);
    for i in 0..14 {
        c.set(6 + hash(i) % 36, 28 + hash(i + 50) % 5, light(SOIL, 0.2));
    }
    // Tall plants at the back.
    match v % 3 {
        0 => {
            sunflower(c, 9, 3);
            lavender(c, 34);
        }
        1 => {
            tomato(c, 10);
            sunflower(c, 37, 5);
        }
        _ => {
            lavender(c, 6);
            sunflower(c, 22, 2);
            tomato(c, 36);
        }
    }
    // Leafy mounds and blossoms.
    let greens = [
        LEAF,
        mix(LEAF, 0x9ccc65, 0.45),
        dark(LEAF, 0.12),
        mix(LEAF, 0x6fb08a, 0.4),
    ];
    for (i, (x, y, w, h)) in [
        (3, 20, 12, 11),
        (13, 17, 11, 12),
        (22, 19, 12, 11),
        (32, 18, 12, 12),
        (6, 24, 10, 9),
        (17, 23, 12, 10),
        (28, 24, 11, 9),
        (36, 23, 9, 9),
    ]
    .into_iter()
    .enumerate()
    {
        clump(c, x, y, w, h, greens[(i + v) % greens.len()]);
    }
    let petals = [p.accent2, PAPER, 0xb98be0, mix(p.accent2, SUN, 0.5)];
    for (i, (x, y)) in [
        (8, 22),
        (18, 19),
        (27, 22),
        (37, 20),
        (13, 27),
        (23, 26),
        (33, 27),
        (41, 25),
    ]
    .into_iter()
    .enumerate()
    {
        bloom(c, x, y, petals[(i + v) % petals.len()], SUN);
    }
    // A plant label.
    c.vline(30, 27, 6, light(timber, 0.2));
    c.rect(29, 25, 3, 2, PAPER);
    // Front board on corner posts, with trailing vines.
    c.rect(3, 33, 42, 2, light(timber, 0.3));
    c.hline(3, 33, 42, light(timber, 0.45));
    c.rect(3, 35, 42, 11, timber);
    for y in [38, 42] {
        c.hline(5, y, 38, dark(timber, 0.25));
    }
    c.hline(3, 45, 42, dark(timber, 0.4));
    for x in [2, 42] {
        c.rect(x, 31, 4, 15, light(timber, 0.06));
        c.hline(x, 31, 4, light(timber, 0.4));
        c.vline(x + 3, 32, 14, dark(timber, 0.22));
        c.set(x + 1, 36, dark(timber, 0.4));
        c.set(x + 1, 42, dark(timber, 0.4));
    }
    for (x, len) in [(9, 5), (21, 3), (31, 6)] {
        c.vline(x, 34, len, dark(LEAF, 0.05));
        c.set(x + 1, 34 + len - 2, light(LEAF, 0.2));
        c.set(x - 1, 34 + len / 2, LEAF);
    }
}

fn sunflower(c: &mut Canvas, x: i32, top: i32) {
    c.vline(x, top + 6, 26 - top, dark(LEAF, 0.15));
    for (dy, dx) in [(10, -2), (15, 1), (20, -2)] {
        c.rect(x + dx, top + dy, 2, 2, LEAF);
        c.set(x + dx, top + dy, light(LEAF, 0.3));
    }
    c.oval(x - 4, top, 9, 9, SUN);
    c.oval(x - 3, top + 1, 4, 3, light(SUN, 0.4));
    c.oval(x - 2, top + 2, 5, 5, 0x7a4a2a);
    c.set(x - 1, top + 3, 0xa0703f);
}

fn lavender(c: &mut Canvas, x: i32) {
    let purple = 0x9b7fd1;
    for (i, dx) in [0, 3, 6, 9].into_iter().enumerate() {
        let top = 10 + (i as i32 * 5) % 7;
        c.vline(x + dx, top + 5, 26 - top, dark(LEAF, 0.1));
        for k in 0..6 {
            let color = if k % 2 == 0 {
                light(purple, 0.25)
            } else {
                purple
            };
            c.set(x + dx + k % 2, top + k, color);
        }
    }
}

fn tomato(c: &mut Canvas, x: i32) {
    c.vline(x, 6, 24, 0xb88a55);
    c.set(x, 6, light(0xb88a55, 0.4));
    for (dx, dy) in [(-4, 9), (1, 13), (-4, 17)] {
        clump(c, x + dx, dy, 7, 6, mix(LEAF, 0x3f7a45, 0.4));
    }
    for (dx, dy) in [(-3, 12), (3, 16), (-2, 20), (2, 10)] {
        c.rect(x + dx, dy, 2, 2, 0xe0483f);
        c.set(x + dx, dy, light(0xe0483f, 0.45));
    }
}

fn potting_bench(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    // Backboard with a top shelf.
    c.rect(4, 11, 40, 14, dark(wood, 0.1));
    for x in (9..44).step_by(6) {
        c.vline(x, 11, 13, dark(wood, 0.25));
    }
    c.rect(3, 9, 42, 2, light(wood, 0.2));
    c.hline(3, 9, 42, light(wood, 0.45));
    c.hline(4, 11, 40, dark(wood, 0.35));
    for (i, x) in [5, 12, 33, 39].into_iter().enumerate() {
        let color = if i % 2 == 0 {
            CLAY
        } else {
            mix(CLAY, p.accent2, 0.4)
        };
        pot(c, x, 6, 5, 3, color);
        let green = if i % 2 == 0 { LEAF } else { light(LEAF, 0.2) };
        c.rect(x + 1, 3, 3, 3, green);
        c.set(x + 2, 2, light(green, 0.3));
    }
    // Hanging tools: twine, a trowel, a hand fork and gardening gloves.
    let twine = 0xd8c08a;
    c.oval(17, 13, 5, 5, twine);
    c.hline(17, 15, 5, dark(twine, 0.2));
    c.set(18, 14, light(twine, 0.4));
    c.set(19, 12, METAL);
    c.rect(24, 12, 2, 4, p.accent);
    c.set(24, 12, light(p.accent, 0.4));
    c.stamp(23, 16, &["mmmm", "mmmm", ".mm.", ".mm."], &[(b'm', METAL)]);
    c.vline(23, 16, 2, light(METAL, 0.4));
    c.rect(29, 12, 2, 4, p.accent2);
    c.set(29, 12, light(p.accent2, 0.4));
    c.stamp(
        27,
        16,
        &["mmmmmm", "m.m.m.", "m.m.m.", "m.m.m."],
        &[(b'm', METAL)],
    );
    c.set(28, 16, light(METAL, 0.4));
    let glove = mix(p.accent, PAPER, 0.25);
    c.stamp(
        33,
        13,
        &["wwww.", "kkkk.", "kkkkk", "kkkkk", "kkkkk", "k.k.k"],
        &[(b'k', glove), (b'w', PAPER)],
    );
    c.vline(37, 15, 3, dark(glove, 0.25));
    c.set(34, 14, light(glove, 0.35));
    // Work top.
    c.rect(2, 24, 44, 3, light(wood, 0.18));
    c.hline(2, 24, 44, light(wood, 0.45));
    c.rect(2, 27, 44, 3, wood);
    c.hline(2, 29, 44, dark(wood, 0.35));
    // Legs and the lower shelf.
    for x in [3, 42] {
        c.rect(x, 30, 3, 16, dark(wood, 0.12));
        c.vline(x + 2, 30, 16, dark(wood, 0.3));
    }
    c.rect(3, 39, 42, 2, light(wood, 0.1));
    c.hline(3, 40, 42, dark(wood, 0.3));
    // Nested pots and a soil sack below.
    for (i, y) in [33, 32, 31].into_iter().enumerate() {
        let rim = if i == 2 {
            light(CLAY, 0.35)
        } else {
            dark(CLAY, 0.1)
        };
        c.hline(8, y, 9, rim);
    }
    pot(c, 8, 32, 9, 7, CLAY);
    let sack = 0xcaa86c;
    c.round(22, 31, 14, 8, 2, sack);
    c.hline(23, 31, 12, light(sack, 0.3));
    c.hline(23, 32, 12, dark(sack, 0.25));
    c.rect(26, 34, 6, 3, PAPER);
    c.set(28, 35, LEAF);
    c.set(29, 34, LEAF);
    c.rect(37, 35, 4, 4, mix(CLAY, p.accent, 0.3));
    // Things on the top: a leafy pot, a seedling, a seed tray, a trowel.
    pot(c, 5, 18, 10, 8, CLAY);
    let big = if v.is_multiple_of(2) {
        LEAF
    } else {
        mix(LEAF, 0x3f7a45, 0.5)
    };
    clump(c, 3, 8, 9, 9, dark(big, 0.05));
    clump(c, 9, 7, 8, 9, big);
    clump(c, 6, 12, 8, 7, light(big, 0.08));
    if v % 3 == 1 {
        bloom(c, 7, 10, p.accent2, SUN);
        bloom(c, 13, 13, PAPER, SUN);
    }
    pot(c, 18, 20, 6, 6, mix(CLAY, PAPER, 0.25));
    c.vline(20, 16, 4, LEAF);
    c.rect(18, 15, 2, 2, light(LEAF, 0.2));
    c.rect(21, 14, 2, 2, LEAF);
    c.rect(26, 22, 12, 4, 0x3b3f4c);
    c.hline(26, 22, 12, light(0x3b3f4c, 0.3));
    for x in (27..37).step_by(2) {
        c.set(x, 21, light(LEAF, 0.3));
        c.set(x, 22, LEAF);
        c.set(x + 1, 23, dark(0x3b3f4c, 0.3));
    }
    c.rect(38, 24, 4, 2, METAL);
    c.set(38, 24, light(METAL, 0.4));
    c.rect(42, 25, 3, 1, p.accent);
    for i in 0..5 {
        c.set(16 + hash(i) % 22, 25 + hash(i + 9) % 2, SOIL);
    }
}

// ---------------------------------------------------------------- observatory

fn server_rack(c: &mut Canvas, p: &Pal, v: usize) {
    let body = mix(0x3d4559, p.t.trim, 0.25);
    let face = mix(body, 0x77819a, 0.4);
    // Patch tower between the racks with a router on top.
    c.line((22, 11), (21, 5), METAL);
    c.line((26, 11), (27, 6), METAL);
    rack(
        c,
        20,
        11,
        8,
        body,
        face,
        &[
            (14, 3, 2),
            (18, 3, 3),
            (22, 4, 2),
            (28, 3, 3),
            (33, 4, 2),
            (38, 3, 3),
        ],
    );
    rack(
        c,
        2,
        6,
        17,
        body,
        face,
        &[
            (9, 2, 0),
            (12, 4, 2),
            (17, 4, 1),
            (22, 4, 3),
            (27, 3, 0),
            (31, 5, 1),
            (37, 4, 4),
        ],
    );
    rack(
        c,
        29,
        8,
        17,
        body,
        face,
        &[
            (11, 5, 1),
            (17, 3, 2),
            (21, 5, 3),
            (27, 4, 0),
            (32, 4, 1),
            (37, 4, 4),
        ],
    );
    // Glass door on the right rack.
    for y in 11..41 {
        for x in 31..44 {
            let color = c.get(x, y);
            c.set(x, y, mix(color, 0x2a3f66, 0.32));
        }
    }
    for i in 0..6 {
        c.set(
            33 + i,
            39 - i * 3,
            mix(c.get(33 + i, 39 - i * 3), PAPER, 0.35),
        );
    }
    c.vline(42, 20, 8, light(METAL, 0.2));
    // Patch cables.
    let cables = [p.accent, p.accent2, 0x7be08a, 0xf28fb0];
    for (i, color) in cables.into_iter().enumerate() {
        let i = i as i32;
        c.line((19, 15 + i * 2), (20, 16 + i * 2), color);
        c.line((28, 19 + i * 2), (29, 20 + i * 2), color);
    }
    // Things on top: a succulent and a box of tapes.
    pot(c, 6, 3, 6, 3, CLAY);
    c.rect(7, 1, 4, 2, LEAF);
    c.set(8, 1, light(LEAF, 0.3));
    let box_ = 0xc9a26a;
    block(c, 33, 4, 9, 4, 1, box_);
    c.rect(35, 5, 4, 2, PAPER);
    if v % 2 == 1 {
        c.rect(30, 30, 3, 3, 0xf6d55c);
    }
    // Slots for the animated status lights.
    for i in 0..6 {
        c.rect(10 + (i % 3) * 12, 14 + (i / 3) * 10, 2, 1, SLOT);
    }
}

/// One rack: a frame with a lit top, server units and a plinth.
fn rack(c: &mut Canvas, x: i32, y: i32, w: i32, body: u32, face: u32, units: &[(i32, i32, u8)]) {
    c.rect(x, y, w, 43 - y, body);
    c.rect(x, y, w, 2, light(body, 0.3));
    c.hline(x, y, w, light(body, 0.45));
    c.vline(x, y + 2, 41 - y, light(body, 0.12));
    c.vline(x + w - 1, y + 2, 41 - y, dark(body, 0.35));
    for &(uy, uh, kind) in units {
        unit(c, x + 2, uy, w - 4, uh, face, kind);
    }
    c.rect(x, 42, w, 3, dark(body, 0.3));
    c.hline(x, 42, w, dark(body, 0.1));
    for cx in [x + 1, x + w - 2] {
        c.set(cx, 45, INK);
    }
}

fn unit(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, face: u32, kind: u8) {
    c.rect(x, y, w, h, face);
    c.hline(x, y, w, light(face, 0.25));
    if h >= 3 {
        c.hline(x, y + h - 1, w, dark(face, 0.25));
    }
    let mid = y + h / 2;
    match kind {
        1 => {
            for k in 0..(w - 2) / 3 {
                c.rect(x + 1 + k * 3, y + 1, 2, h - 2, light(face, 0.35));
                c.set(x + 2 + k * 3, y + h - 2, 0x7be08a);
            }
        }
        2 => {
            for xx in (x + 1..x + w - 1).step_by(2) {
                c.set(xx, mid, SLOT);
            }
            c.set(x + w - 2, y + 1, 0x7be08a);
        }
        3 => {
            c.rect(x + 1, y + 1, 5.min(w - 2), h - 2, SCREEN);
            c.set(x + 2, y + 1, 0x7be08a);
            c.set(x + w - 2, mid, 0xf2b450);
        }
        4 => {
            c.vline(x, y + 1, h - 2, METAL);
            c.vline(x + w - 1, y + 1, h - 2, METAL);
            c.hline(x + 2, mid, w - 4, dark(face, 0.3));
        }
        _ => {
            for xx in (x + 1..x + w - 1).step_by(2) {
                c.set(xx, mid, dark(face, 0.35));
            }
        }
    }
}

fn telescope(c: &mut Canvas, p: &Pal, v: usize) {
    let legs = mix(0xa8784c, p.wood, 0.3);
    // Star chart on a lectern stand.
    let chart = 0x22305a;
    c.rect(40, 30, 2, 15, dark(legs, 0.2));
    c.hline(36, 45, 10, dark(legs, 0.25));
    c.rect(35, 19, 12, 12, legs);
    c.hline(35, 19, 12, light(legs, 0.35));
    c.rect(36, 20, 10, 10, chart);
    let stars = [(37, 22), (40, 21), (43, 23), (42, 26), (38, 27), (44, 28)];
    for pair in stars.windows(2).take(4) {
        c.line(pair[0], pair[1], mix(chart, p.accent, 0.5));
    }
    for (x, y) in stars {
        c.set(x, y, PAPER);
    }
    c.set(39, 25, SUN);
    c.hline(35, 30, 12, dark(legs, 0.3));
    // Back leg.
    c.line((21, 24), (23, 42), dark(legs, 0.45));
    // Tube aimed at the upper left, with its finder and brass rings.
    let tube = light(p.t.trim, 0.12);
    let brass = mix(BRASS, p.accent2, 0.25);
    let (a, b) = ((6.0, 8.0), (29.0, 22.5));
    let at = |t: f64| (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
    rod(c, at(0.42), at(0.62), 1.2, 1.2, METAL);
    rod(c, a, b, 3.5, 2.6, tube);
    rod(c, (3.6, 6.5), at(0.2), 4.0, 3.9, dark(tube, 0.12));
    rod(c, at(0.2), at(0.24), 4.2, 4.2, brass);
    rod(c, at(0.52), at(0.56), 3.6, 3.6, brass);
    rod(c, at(0.94), b, 3.0, 3.0, brass);
    rod(c, (5.4, 2.6), (1.8, 8.6), 1.0, 1.0, 0x1d2a4a);
    c.set(4, 4, 0x9fc4ff);
    let off = (2.6, -4.2);
    let (f0, f1) = (at(0.32), at(0.55));
    rod(
        c,
        (f0.0 + off.0, f0.1 + off.1),
        (f1.0 + off.0, f1.1 + off.1),
        1.3,
        1.3,
        dark(tube, 0.25),
    );
    // Focuser and eyepiece.
    rod(c, b, (32.5, 24.6), 1.6, 1.6, 0x4a4f5e);
    rod(c, (32.0, 24.0), (33.5, 28.5), 1.3, 1.3, 0x2c3040);
    c.set(33, 29, METAL);
    // Mount and tripod.
    c.rect(18, 19, 6, 5, 0x4a4f5e);
    c.hline(18, 19, 6, light(0x4a4f5e, 0.3));
    c.set(24, 21, brass);
    for foot in [(9, 45), (31, 45)] {
        c.line((20, 24), foot, legs);
        c.line((21, 24), (foot.0 + 1, foot.1), dark(legs, 0.3));
        c.rect(foot.0, foot.1, 2, 1, METAL);
    }
    c.hline(15, 35, 12, dark(legs, 0.2));
    for x in [17, 21, 24] {
        c.set(x, 34, if v.is_multiple_of(2) { brass } else { METAL });
    }
}

// ---------------------------------------------------------------- library

fn globe(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    let brass = mix(BRASS, p.accent, 0.15);
    let (cx, cy) = (17, 17);
    // Back half of the horizon ring.
    c.oval(cx - 15, cy - 3, 30, 7, dark(wood, 0.2));
    // Legs and stretcher.
    for (top, foot) in [
        ((5, 19), (5, 45)),
        ((29, 19), (29, 45)),
        ((17, 24), (17, 45)),
    ] {
        c.line(top, foot, wood);
        c.line((top.0 + 1, top.1), (foot.0 + 1, foot.1), dark(wood, 0.3));
        c.rect(foot.0 - 1, foot.1, 4, 1, dark(wood, 0.4));
    }
    c.hline(6, 38, 23, dark(wood, 0.15));
    c.rect(16, 36, 4, 4, light(wood, 0.1));
    // Meridian ring behind the globe.
    for i in 0..48 {
        let t = std::f64::consts::TAU * i as f64 / 48.0;
        let (x, y) = (cx as f64 + 12.6 * t.cos(), cy as f64 - 12.6 * t.sin());
        if t.cos() > -0.35 {
            c.rect(x as i32, y as i32, 2, 1, dark(brass, 0.1));
        }
    }
    // The globe, an antique sea with parchment lands.
    let sea = 0x4f8bb0;
    let land = 0xd6c38b;
    let lands = [
        (2, 4, 7, 5),
        (4, 8, 5, 7),
        (6, 14, 4, 5),
        (12, 3, 6, 4),
        (13, 7, 5, 8),
        (17, 5, 4, 3),
    ];
    ball(c, cx - 11, cy - 11, 22, |x, y| {
        let x = (x + v as i32 * 3) % 22;
        let inside = lands.iter().any(|&(lx, ly, lw, lh)| {
            let dx = (x - lx) as f64 / lw as f64 - 0.5;
            let dy = (y - ly) as f64 / lh as f64 - 0.5;
            dx * dx + dy * dy < 0.25
        });
        if inside {
            land
        } else {
            sea
        }
    });
    // Front half of the horizon ring and the meridian pins.
    for x in cx - 15..cx + 15 {
        let dx = (x as f64 + 0.5 - cx as f64) / 15.0;
        let dy = (1.0 - dx * dx).max(0.0).sqrt() * 3.0;
        let y = cy + dy.round() as i32;
        c.rect(x, y, 1, 2, wood);
        c.set(x, y, light(wood, 0.35));
    }
    c.rect(cx - 1, cy - 14, 3, 2, brass);
    c.set(cx + 7, cy + 11, brass);
    // Reading table with a book stack and a candle.
    let top = light(wood, 0.08);
    c.oval(32, 26, 15, 5, top);
    c.oval(33, 26, 12, 3, light(top, 0.25));
    c.hline(33, 31, 13, dark(wood, 0.3));
    c.rect(38, 31, 3, 12, dark(wood, 0.1));
    c.vline(40, 31, 12, dark(wood, 0.35));
    c.oval(34, 42, 11, 4, dark(wood, 0.15));
    c.hline(35, 42, 9, light(wood, 0.1));
    for (i, (x, w, color)) in [(34, 10, p.accent2), (35, 9, p.fabric), (34, 9, p.accent)]
        .into_iter()
        .enumerate()
    {
        let y = 26 - i as i32 * 2;
        c.rect(x, y, w, 2, color);
        c.hline(x, y, w, light(color, 0.3));
        c.set(x + w - 1, y + 1, PAPER);
    }
    c.rect(38, 15, 2, 6, PAPER);
    c.vline(39, 15, 6, dark(PAPER, 0.2));
    c.hline(37, 20, 4, brass);
    c.set(38, 14, GLOW);
    c.set(38, 13, 0xff9a4a);
}

// ---------------------------------------------------------------- workshop

fn workbench(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    // Clamp lamp at the back right.
    c.line((42, 21), (40, 12), METAL);
    c.line((40, 12), (34, 7), METAL);
    c.round(28, 5, 9, 5, 1, p.accent);
    c.hline(29, 5, 7, light(p.accent, 0.35));
    c.hline(29, 10, 7, GLOW);
    // Butcher-block top.
    c.rect(2, 20, 44, 5, light(wood, 0.15));
    for y in [21, 23] {
        c.hline(2, y, 44, light(wood, 0.28));
    }
    c.hline(2, 20, 44, light(wood, 0.45));
    c.rect(2, 25, 44, 4, wood);
    for x in (5..46).step_by(5) {
        c.vline(x, 25, 4, dark(wood, 0.15));
    }
    c.hline(2, 28, 44, dark(wood, 0.35));
    // Legs, stretcher and lower shelf.
    for x in [4, 40] {
        c.rect(x, 29, 4, 17, dark(wood, 0.1));
        c.vline(x + 3, 29, 17, dark(wood, 0.32));
        c.vline(x, 29, 17, light(wood, 0.05));
    }
    c.rect(4, 38, 40, 2, light(wood, 0.05));
    c.hline(4, 39, 40, dark(wood, 0.3));
    let toolbox = p.accent;
    block(c, 9, 31, 12, 7, 2, toolbox);
    c.rect(13, 30, 4, 1, dark(toolbox, 0.3));
    c.hline(10, 34, 10, dark(toolbox, 0.3));
    c.set(15, 35, METAL);
    for (i, (x, w, h)) in [(24, 7, 4), (27, 5, 3), (32, 6, 3)].into_iter().enumerate() {
        let y = 38 - h - i as i32 % 2;
        block(c, x, y, w, h, 1, light(wood, 0.1 * i as f64));
    }
    c.rect(36, 33, 3, 5, p.accent2);
    c.hline(36, 33, 3, METAL);
    // Bench vise on the front left corner, clamping a board.
    let iron = mix(0x5d6578, p.accent2, 0.15);
    c.rect(8, 5, 2, 8, light(wood, 0.25));
    c.vline(9, 5, 8, light(wood, 0.05));
    c.hline(8, 5, 2, light(wood, 0.5));
    for x in [4, 10] {
        c.rect(x, 10, 4, 4, iron);
        c.hline(x, 10, 4, light(iron, 0.4));
        c.vline(x + 3, 11, 3, dark(iron, 0.2));
    }
    c.vline(7, 10, 4, METAL);
    c.vline(10, 10, 4, METAL);
    c.round(5, 14, 8, 5, 1, iron);
    c.hline(6, 14, 6, light(iron, 0.2));
    c.vline(12, 15, 3, dark(iron, 0.3));
    c.rect(3, 19, 12, 1, dark(iron, 0.35));
    c.line((2, 18), (15, 15), light(METAL, 0.1));
    c.oval(7, 15, 4, 3, dark(iron, 0.3));
    c.set(8, 16, METAL);
    for (x, y) in [(1, 18), (15, 14)] {
        c.rect(x, y, 2, 2, dark(METAL, 0.25));
        c.set(x, y, light(METAL, 0.3));
    }
    // Tools on the top.
    c.rect(17, 21, 8, 1, dark(wood, 0.3));
    c.rect(24, 19, 2, 4, METAL);
    c.set(24, 19, light(METAL, 0.4));
    if v.is_multiple_of(2) {
        let plane = p.accent2;
        c.round(28, 17, 8, 4, 1, plane);
        c.hline(29, 17, 6, light(plane, 0.4));
        c.rect(30, 15, 2, 2, dark(wood, 0.2));
        c.hline(28, 21, 8, METAL);
    } else {
        c.rect(27, 19, 9, 2, METAL);
        c.hline(27, 19, 9, light(METAL, 0.4));
        c.rect(35, 18, 3, 3, p.accent2);
    }
    // Offcuts, shavings and a mug.
    block(c, 37, 18, 4, 3, 1, light(wood, 0.2));
    for (x, y) in [(15, 23), (21, 24), (33, 23), (27, 22)] {
        c.set(x, y, light(wood, 0.55));
        c.set(x + 1, y - 1, light(wood, 0.45));
    }
    cup(c, 41, 18, PAPER, 0x6b4630);
}

fn printer3d(c: &mut Canvas, p: &Pal, v: usize) {
    let frame = mix(p.t.trim, 0x30343f, 0.55);
    let fil = [p.accent, p.accent2, PAPER, 0x6fc6e8];
    // Stand with spool cubbies.
    let stand = p.wood;
    block(c, 3, 33, 42, 13, 2, stand);
    for x in [6, 25] {
        c.rect(x, 36, 17, 8, dark(stand, 0.5));
        c.hline(x, 36, 17, dark(stand, 0.65));
        for k in 0..2 {
            let color = fil[((x / 19) as usize * 2 + k as usize + v) % fil.len()];
            let sx = x + 1 + k * 8;
            c.oval(sx, 37, 7, 7, dark(color, 0.35));
            c.oval(sx + 1, 38, 5, 5, color);
            c.set(sx + 1, 39, light(color, 0.4));
            c.rect(sx + 3, 40, 1, 1, INK);
        }
    }
    // Enclosure.
    c.round(5, 6, 38, 28, 1, frame);
    c.rect(5, 6, 38, 2, light(frame, 0.3));
    c.hline(6, 6, 36, light(frame, 0.5));
    c.vline(42, 8, 25, dark(frame, 0.3));
    // Chamber interior, lit.
    let chamber = mix(SCREEN, 0x8fa8c8, 0.4);
    c.rect(8, 9, 32, 20, chamber);
    c.hline(8, 9, 32, light(chamber, 0.3));
    for x in [9, 38] {
        c.vline(x, 9, 20, METAL);
    }
    // Build plate and the print in progress.
    c.rect(9, 24, 30, 2, light(METAL, 0.2));
    c.hline(9, 25, 30, dark(METAL, 0.3));
    let print = fil[(1 + v) % fil.len()];
    c.stamp(
        18,
        19,
        &["..aaaa..", ".bbbbbb.", "aaaaaaaa", "bbbbbbbb", ".aaaaaa."],
        &[(b'a', print), (b'b', dark(print, 0.15))],
    );
    c.vline(25, 21, 2, dark(print, 0.3));
    c.hline(20, 19, 4, light(print, 0.35));
    // Gantry rail and print head.
    c.rect(8, 13, 32, 3, METAL);
    c.hline(8, 13, 32, light(METAL, 0.4));
    c.hline(8, 15, 32, dark(METAL, 0.3));
    c.round(18, 11, 9, 7, 1, p.accent);
    c.hline(19, 11, 7, light(p.accent, 0.35));
    c.oval(23, 12, 3, 3, dark(p.accent, 0.4));
    c.rect(21, 18, 2, 1, METAL);
    // Glass reflections.
    for (x0, y0) in [(11, 26), (30, 27)] {
        for i in 0..6 {
            let (x, y) = (x0 + i, y0 - i * 2);
            if !(13..=15).contains(&y) && y >= 9 {
                c.set(x, y, mix(c.get(x, y), PAPER, 0.4));
            }
        }
    }
    // Control strip with a progress screen.
    c.rect(29, 30, 10, 3, SCREEN);
    c.hline(30, 31, 5, 0x7be08a);
    c.hline(35, 31, 3, dark(SCREEN, 0.2));
    c.oval(25, 30, 3, 3, METAL);
    // Spool on top, feeding the head.
    let spool = fil[v % fil.len()];
    c.rect(15, 2, 12, 4, spool);
    for x in (16..26).step_by(2) {
        c.vline(x, 2, 4, dark(spool, 0.15));
    }
    c.hline(15, 2, 12, light(spool, 0.3));
    c.rect(14, 1, 2, 6, METAL);
    c.rect(26, 1, 2, 6, METAL);
    c.vline(14, 1, 6, light(METAL, 0.3));
    c.line((27, 4), (31, 6), PAPER);
    // Slots for the animated status lights.
    for i in 0..6 {
        c.rect(10 + (i % 3) * 12, 14 + (i / 3) * 10, 2, 1, SLOT);
    }
}

// ---------------------------------------------------------------- tea commons

fn espresso_bar(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    // Counter.
    let top = mix(PAPER, wood, 0.15);
    c.rect(2, 26, 44, 4, top);
    c.hline(2, 26, 44, light(top, 0.5));
    c.hline(2, 29, 44, dark(top, 0.25));
    c.rect(2, 30, 44, 16, wood);
    let tile = mix(p.accent, PAPER, 0.25);
    for x in (2..46).step_by(3) {
        let color = if (x / 3) % 2 == 0 {
            tile
        } else {
            light(tile, 0.3)
        };
        c.rect(x, 32, 3, 4, color);
    }
    c.hline(2, 36, 44, dark(wood, 0.3));
    for x in [14, 33] {
        c.vline(x, 37, 7, dark(wood, 0.2));
    }
    c.hline(2, 43, 44, dark(wood, 0.45));
    c.rect(2, 44, 44, 2, dark(wood, 0.55));
    // Espresso machine.
    let body = p.accent;
    let chrome = 0xc9ced8;
    c.rect(12, 5, 24, 3, chrome);
    c.hline(12, 5, 24, light(chrome, 0.4));
    for (i, x) in [14, 20, 27].into_iter().enumerate() {
        c.rect(x, 2, 4, 3, if i == 1 { p.accent2 } else { PAPER });
        c.hline(x, 2, 4, light(PAPER, 0.3));
    }
    c.round(12, 8, 24, 11, 1, body);
    c.hline(13, 8, 22, light(body, 0.35));
    c.vline(35, 9, 9, dark(body, 0.25));
    c.rect(19, 9, 10, 4, chrome);
    c.rect(20, 10, 8, 2, SLOT);
    for gx in [13, 30] {
        c.oval(gx, 11, 5, 5, chrome);
        c.oval(gx + 1, 12, 3, 3, PAPER);
        c.set(gx + 2, 13, p.accent2);
    }
    c.rect(12, 18, 24, 9, dark(body, 0.38));
    c.hline(12, 18, 24, chrome);
    for gx in [15, 28] {
        c.rect(gx, 19, 5, 2, chrome);
        c.rect(gx - 2, 21, 9, 1, INK);
        c.rect(gx + 1, 21, 3, 1, chrome);
        cup(c, gx, 23, PAPER, 0x6b4630);
    }
    c.line((12, 17), (10, 24), METAL);
    c.hline(12, 26, 24, METAL);
    // Grinder.
    let hopper = mix(GLASS, 0x6b4630, 0.15);
    for r in 0..8 {
        c.hline(37 + r / 3, 7 + r, 9 - r / 3 * 2, hopper);
    }
    c.rect(38, 11, 7, 4, 0x6b4630);
    for (x, y) in [(39, 12), (42, 11), (41, 13), (43, 13)] {
        c.set(x, y, 0x9a6a42);
    }
    c.rect(36, 5, 10, 2, INK);
    c.hline(36, 5, 10, light(INK, 0.3));
    let grinder = mix(INK, wood, 0.25);
    c.round(37, 15, 9, 11, 1, grinder);
    c.hline(38, 15, 7, light(grinder, 0.3));
    c.rect(40, 21, 3, 3, dark(INK, 0.2));
    // Cup stack and a milk pitcher.
    for i in 0..4 {
        c.rect(3, 23 - i * 3, 5, 3, PAPER);
        c.hline(3, 23 - i * 3, 5, light(PAPER, 0.5));
        c.set(7, 24 - i * 3, dark(PAPER, 0.2));
    }
    c.rect(4, 10, 3, 1, dark(PAPER, 0.25));
    c.stamp(
        7,
        19,
        &["mmmm.", ".mmmk", ".mmmk", ".mmm.", ".mmm."],
        &[(b'm', chrome), (b'k', dark(chrome, 0.3))],
    );
    c.vline(8, 20, 4, light(chrome, 0.45));
    c.hline(7, 19, 4, light(chrome, 0.3));
    if v % 2 == 1 {
        c.rect(38, 23, 4, 3, p.accent2);
    }
}

fn pastry_case(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    // Cabinet base.
    block(c, 2, 31, 44, 15, 2, wood);
    for x in [16, 31] {
        c.vline(x, 34, 10, dark(wood, 0.25));
    }
    c.rect(2, 44, 44, 2, dark(wood, 0.45));
    // Glass case.
    let inside = 0xfbf3e2;
    c.round(3, 9, 42, 23, 2, METAL);
    c.rect(5, 11, 38, 19, inside);
    c.hline(4, 9, 40, light(METAL, 0.45));
    c.rect(5, 19, 38, 1, light(GLASS, 0.3));
    c.hline(5, 20, 38, dark(inside, 0.15));
    c.rect(4, 29, 40, 2, dark(METAL, 0.2));
    // Upper shelf: croissants, cupcakes, macarons.
    for x in [6, 11] {
        c.oval(x, 15, 6, 4, 0xe2a24e);
        c.oval(x + 1, 15, 3, 2, light(0xe2a24e, 0.4));
        c.set(x + 2, 17, dark(0xe2a24e, 0.3));
    }
    for (x, frost) in [
        (18, mix(p.fabric, PAPER, 0.2)),
        (24, mix(p.accent, PAPER, 0.35)),
    ] {
        c.rect(x + 1, 16, 4, 3, 0xd9a05e);
        c.vline(x + 2, 16, 3, dark(0xd9a05e, 0.2));
        c.oval(x, 13, 6, 4, frost);
        c.set(x + 1, 13, light(frost, 0.4));
        c.set(x + 3, 12, 0xd9484f);
    }
    let macaron = [p.accent, p.accent2, 0xf2b8c6, 0xb9e3c6];
    for (i, color) in macaron.into_iter().enumerate() {
        let x = 31 + i as i32 * 3;
        c.rect(x, 16, 3, 3, light(color, 0.15));
        c.hline(x, 17, 3, PAPER);
        c.set(x, 16, light(color, 0.45));
    }
    // Lower shelf: a cake with a slice out, donuts and a fruit tart.
    let sponge = 0xf3dcb0;
    let cream = mix(p.fabric, PAPER, 0.45);
    c.oval(6, 21, 12, 4, cream);
    c.rect(6, 23, 12, 5, sponge);
    c.hline(6, 25, 12, cream);
    c.rect(14, 23, 4, 5, inside);
    c.oval(6, 21, 8, 4, light(cream, 0.3));
    for x in [8, 11] {
        c.set(x, 22, 0xd9484f);
    }
    c.rect(14, 24, 1, 4, dark(sponge, 0.15));
    for x in [20, 25] {
        let icing = if x == 20 {
            mix(p.accent, PAPER, 0.3)
        } else {
            0x7a4a2a
        };
        c.oval(x, 24, 6, 4, 0xd9a05e);
        c.oval(x, 23, 6, 3, icing);
        c.set(x + 2, 24, inside);
        c.set(x + 3, 24, dark(0xd9a05e, 0.3));
        c.set(x + 1, 23, SUN);
        c.set(x + 4, 24, p.accent2);
    }
    c.rect(32, 25, 10, 3, 0xd9a05e);
    c.hline(32, 25, 10, light(0xd9a05e, 0.3));
    for (i, x) in (33..41).step_by(2).enumerate() {
        c.set(x, 24, [0xd9484f, 0x5a5fc4, SUN, 0x7bc86a][i % 4]);
    }
    // Price tags.
    for x in [9, 22, 35] {
        c.rect(x, 18, 3, 1, PAPER);
    }
    // Glass reflections.
    for (x0, len) in [(8, 9), (12, 6), (30, 8)] {
        for i in 0..len {
            let (x, y) = (x0 + i, 28 - i * 2);
            if y > 10 {
                c.set(x, y, mix(c.get(x, y), 0xffffff, 0.45));
            }
        }
    }
    // A covered cake stand and a bell on top.
    c.hline(8, 8, 13, METAL);
    c.vline(14, 6, 2, METAL);
    c.oval(9, 1, 11, 9, mix(GLASS, PAPER, 0.4));
    c.rect(10, 3, 9, 4, cream);
    c.hline(10, 3, 9, light(cream, 0.35));
    c.set(14, 2, 0xd9484f);
    c.vline(10, 2, 4, 0xffffff);
    c.hline(9, 8, 11, dark(METAL, 0.2));
    c.oval(32, 5, 7, 5, BRASS);
    c.set(33, 6, light(BRASS, 0.5));
    c.set(35, 4, dark(BRASS, 0.2));
    c.hline(31, 8, 9, dark(BRASS, 0.3));
    if v.is_multiple_of(2) {
        c.rect(40, 5, 4, 4, PAPER);
        c.rect(41, 6, 2, 2, p.accent);
    }
}

// ---------------------------------------------------------------- neon night, pop arcade

fn arcade(c: &mut Canvas, p: &Pal, v: usize) {
    let body = p.wood;
    let bezel = 0x15121f;
    // Stool at the left.
    let seat = p.accent2;
    c.rect(6, 37, 2, 7, METAL);
    c.oval(3, 43, 8, 3, dark(METAL, 0.2));
    c.oval(2, 32, 10, 5, seat);
    c.oval(3, 32, 7, 3, light(seat, 0.3));
    c.hline(3, 36, 8, dark(seat, 0.35));
    // Change machine at the right.
    let change = mix(METAL, p.accent, 0.2);
    block(c, 38, 18, 8, 28, 2, change);
    c.rect(40, 22, 4, 3, SCREEN);
    c.hline(41, 23, 2, p.accent2);
    c.rect(40, 27, 4, 1, INK);
    c.rect(39, 36, 6, 3, dark(change, 0.4));
    c.set(41, 37, BRASS);
    // Cabinet sides and body.
    c.rect(12, 3, 25, 43, body);
    c.vline(12, 3, 43, light(body, 0.2));
    c.vline(36, 3, 43, dark(body, 0.35));
    c.vline(13, 13, 15, p.accent);
    c.vline(35, 13, 15, p.accent2);
    // Marquee.
    let glow = light(p.accent2, 0.55);
    c.rect(13, 3, 23, 10, bezel);
    c.rect(14, 4, 21, 8, glow);
    c.rect(14, 4, 21, 2, light(glow, 0.3));
    c.stamp(
        22,
        5,
        &["..k..", ".kkk.", "kkkkk", ".k.k."],
        &[(b'k', p.accent)],
    );
    for x in [16, 32] {
        c.rect(x, 6, 2, 3, p.accent2);
    }
    c.rect(20, 10, 8, 2, SLOT);
    // Screen with a little shooter game.
    c.rect(13, 13, 23, 12, bezel);
    let game = 0x10183a;
    c.rect(15, 14, 19, 10, game);
    for y in (15..24).step_by(2) {
        c.hline(15, y, 19, light(game, 0.06));
    }
    for (i, x) in (17..32).step_by(4).enumerate() {
        let color = if i % 2 == 0 { p.accent } else { 0x7be08a };
        c.stamp(x, 16, &["k.k", "kkk"], &[(b'k', color)]);
    }
    c.stamp(23, 21, &[".k.", "kkk"], &[(b'k', p.accent2)]);
    c.set(24, 19, SUN);
    c.set(16, 22, PAPER);
    c.set(32, 15, PAPER);
    c.hline(15, 14, 4, light(game, 0.4));
    // Control panel.
    c.rect(11, 25, 27, 3, light(body, 0.3));
    c.hline(11, 25, 27, light(body, 0.5));
    c.rect(11, 28, 27, 2, dark(body, 0.3));
    c.vline(17, 23, 3, METAL);
    c.rect(16, 22, 3, 2, 0xe0483f);
    c.set(16, 22, light(0xe0483f, 0.5));
    for (x, color) in [(23, p.accent), (26, SUN), (29, p.accent2), (32, 0x7be08a)] {
        c.rect(x, 26, 2, 1, color);
        c.set(x, 27, dark(color, 0.4));
    }
    // Coin door and kick plate.
    c.rect(19, 32, 11, 9, dark(body, 0.25));
    c.rect(20, 33, 9, 7, METAL);
    c.hline(20, 33, 9, light(METAL, 0.4));
    for x in [22, 26] {
        c.rect(x, 35, 2, 3, 0xff7a3d);
        c.vline(x, 35, 3, GLOW);
    }
    c.rect(12, 43, 25, 3, bezel);
    if v % 2 == 1 {
        c.rect(14, 41, 4, 2, p.accent);
    }
}

fn vending(c: &mut Canvas, p: &Pal, v: usize) {
    let body = p.accent;
    // Recycling bin at the right.
    let bin = p.accent2;
    c.rect(38, 33, 8, 13, bin);
    c.vline(38, 33, 13, light(bin, 0.25));
    c.vline(45, 33, 13, dark(bin, 0.3));
    c.oval(38, 31, 8, 4, light(bin, 0.3));
    c.oval(40, 32, 4, 2, dark(bin, 0.6));
    c.hline(39, 39, 6, light(bin, 0.4));
    c.rect(41, 27, 3, 4, METAL);
    c.hline(41, 27, 3, light(METAL, 0.4));
    // Body.
    c.round(4, 2, 34, 44, 1, body);
    c.vline(4, 3, 42, light(body, 0.25));
    c.vline(36, 3, 42, dark(body, 0.25));
    c.vline(37, 3, 42, dark(body, 0.4));
    c.rect(5, 2, 31, 2, light(body, 0.35));
    // Lit header.
    let sign = light(p.accent2, 0.5);
    c.rect(7, 5, 28, 4, sign);
    for x in (8..34).step_by(4) {
        c.set(x, 6, PAPER);
        c.set(x + 1, 7, p.accent2);
    }
    // Display window with rows of drinks.
    let lit = 0xeaf6ff;
    c.rect(7, 10, 21, 27, dark(body, 0.45));
    c.rect(8, 11, 19, 25, lit);
    let drinks = [
        p.accent2, SUN, 0x7be08a, p.accent, PAPER, 0x8a63b8, 0xff8a3d,
    ];
    for row in 0..4 {
        let y = 12 + row * 6;
        for col in 0..5 {
            let color = drinks[((row * 2 + col + v as i32) % drinks.len() as i32) as usize];
            let x = 9 + col * 4;
            c.rect(x, y, 3, 4, color);
            c.set(x, y, light(color, 0.45));
            c.set(x + 2, y + 1, dark(color, 0.25));
            c.hline(x, y, 3, METAL);
        }
        c.hline(8, y + 4, 19, dark(lit, 0.3));
        for col in 0..5 {
            let tag = if col % 2 == 0 { p.accent } else { 0x7be08a };
            c.set(10 + col * 4, y + 5, tag);
        }
    }
    for i in 0..10 {
        let (x, y) = (10 + i, 34 - i * 2);
        c.set(x, y, mix(c.get(x, y), 0xffffff, 0.5));
    }
    // Selection panel.
    c.rect(29, 10, 6, 26, dark(body, 0.2));
    c.rect(30, 12, 4, 3, SCREEN);
    c.hline(30, 13, 3, 0x7be08a);
    for y in (17..27).step_by(2) {
        c.rect(30, y, 1, 1, PAPER);
        c.rect(32, y, 1, 1, PAPER);
    }
    c.rect(31, 28, 2, 4, INK);
    c.set(31, 28, p.accent2);
    // Dispensing bay and kick plate.
    c.rect(8, 38, 20, 5, dark(body, 0.55));
    c.hline(8, 38, 20, dark(body, 0.7));
    c.rect(9, 40, 18, 2, dark(body, 0.4));
    c.rect(29, 38, 6, 3, dark(body, 0.4));
    c.rect(4, 44, 34, 2, INK);
}

// ---------------------------------------------------------------- orbital

/// A leafy crop growing out of a tower pocket: lettuce, herb, berries or kale.
fn sprout(c: &mut Canvas, x: i32, y: i32, kind: usize, leaf: u32) {
    let (l, m, d) = (light(leaf, 0.3), leaf, dark(leaf, 0.25));
    let pattern: &[&str] = match kind % 4 {
        0 => &[
            "...l.l.l...",
            "..lmlmlml..",
            ".lmmmmmmmd.",
            "lmmmmlmmmmd",
            ".dmmmmmmdd.",
            "..ddmmmdd..",
            "....ddd....",
        ],
        1 => &[
            "l....l....l",
            "ml..lml..lm",
            ".mm.mmm.mm.",
            "..mmmmmmm..",
            "..dmmdmmd..",
            "...ddddd...",
        ],
        2 => &[
            "..l.....l..",
            ".lml.l.lml.",
            "lmmmmmmmmmd",
            ".mrmmmmmrm.",
            "..rr.d.rr..",
            "...r...r...",
        ],
        _ => &[
            ".l.l.l.l.l.",
            "lmlmlmlmlmd",
            "mmmmmmmmmmd",
            ".dmdmdmdmd.",
            "..d.d.d.d..",
        ],
    };
    c.stamp(
        x,
        y,
        pattern,
        &[(b'l', l), (b'm', m), (b'd', d), (b'r', 0xe0483f)],
    );
}

fn hydro_pod(c: &mut Canvas, p: &Pal, v: usize) {
    let shell = p.wood;
    let trim = p.t.trim;
    // Base platform.
    block(c, 3, 39, 42, 7, 2, shell);
    c.hline(3, 42, 42, trim);
    c.hline(5, 44, 38, dark(shell, 0.2));
    // Nutrient tank at the left.
    let water = 0x6fc6e8;
    c.rect(4, 24, 8, 15, mix(GLASS, shell, 0.3));
    c.rect(5, 30, 6, 9, water);
    c.hline(5, 30, 6, light(water, 0.4));
    for (x, y) in [(7, 33), (9, 35), (6, 36)] {
        c.set(x, y, light(water, 0.55));
    }
    c.vline(5, 25, 13, light(GLASS, 0.5));
    c.rect(4, 22, 8, 2, trim);
    c.hline(4, 22, 8, light(trim, 0.35));
    c.line((11, 36), (14, 36), METAL);
    // Control column at the right.
    block(c, 37, 20, 8, 19, 2, shell);
    c.rect(38, 23, 6, 5, SCREEN);
    c.line((38, 27), (43, 24), p.accent2);
    c.set(40, 25, light(p.accent2, 0.4));
    c.rect(38, 30, 2, 2, p.accent);
    c.rect(41, 30, 2, 2, p.accent2);
    for y in [34, 36] {
        c.hline(38, y, 6, dark(shell, 0.2));
    }
    // Glass capsule.
    let air = mix(0xe4f6ee, p.accent2, 0.12);
    c.round(13, 3, 23, 37, 4, trim);
    c.round(14, 4, 21, 35, 4, air);
    c.rect(13, 37, 23, 3, METAL);
    c.hline(13, 37, 23, light(METAL, 0.4));
    // Grow light hood, its glow fading down the capsule.
    c.rect(16, 8, 17, 4, 0x3a3f4c);
    c.hline(16, 8, 17, light(0x3a3f4c, 0.3));
    c.rect(20, 10, 8, 2, SLOT);
    for y in 12..17 {
        c.hline(16, y, 17, mix(air, 0xf2b8e8, 0.4 - (y - 12) as f64 * 0.08));
    }
    c.vline(24, 5, 3, METAL);
    // Tower with planted pockets.
    c.rect(22, 12, 5, 25, PAPER);
    c.vline(22, 12, 25, light(PAPER, 0.4));
    c.vline(26, 12, 25, dark(PAPER, 0.2));
    for y in [17, 22, 27, 32] {
        c.hline(22, y, 5, dark(PAPER, 0.12));
    }
    let greens = [
        light(LEAF, 0.08),
        mix(LEAF, 0x9ccc65, 0.55),
        mix(LEAF, 0x4f9a7a, 0.5),
        mix(LEAF, 0x7fb84a, 0.4),
    ];
    for (i, y) in [13, 19, 24, 30].into_iter().enumerate() {
        let left = i % 2 == 0;
        let x = if left { 14 } else { 24 };
        let pocket = if left { 21 } else { 26 };
        c.rect(pocket, y + 4, 2, 2, dark(PAPER, 0.3));
        sprout(c, x, y, i + v, greens[(i + v) % greens.len()]);
    }
    // Glass shine.
    c.vline(15, 18, 16, light(air, 0.6));
    c.vline(16, 20, 6, light(air, 0.4));
    c.vline(34, 14, 22, dark(air, 0.12));
}

fn console(c: &mut Canvas, p: &Pal, v: usize) {
    let shell = p.wood;
    let trim = p.t.trim;
    let bezel = 0x2a3140;
    // Screen bank.
    c.round(3, 2, 42, 15, 1, shell);
    c.hline(4, 2, 40, light(shell, 0.4));
    c.vline(44, 3, 13, dark(shell, 0.25));
    for (i, x) in [5, 17, 29].into_iter().enumerate() {
        c.rect(x, 3, 12, 13, bezel);
        c.rect(x + 1, 4, 10, 8, SCREEN);
        c.hline(x + 1, 4, 10, light(SCREEN, 0.15));
        let ink = if i == 1 { p.accent } else { p.accent2 };
        match (i + v) % 3 {
            0 => {
                c.oval(x + 2, 4, 8, 8, dark(ink, 0.55));
                c.oval(x + 3, 5, 6, 6, SCREEN);
                c.line((x + 6, 8), (x + 9, 6), ink);
                c.set(x + 4, 6, PAPER);
                c.set(x + 7, 10, ink);
            }
            1 => {
                c.oval(x + 4, 6, 4, 4, ink);
                c.set(x + 4, 6, light(ink, 0.5));
                c.hline(x + 2, 8, 8, dark(ink, 0.4));
                c.set(x + 9, 5, PAPER);
            }
            _ => {
                for k in 0..4 {
                    let h = 2 + hash(k + x) % 5;
                    c.rect(x + 2 + k * 2, 11 - h, 1, h, ink);
                }
            }
        }
    }
    c.hline(3, 16, 42, dark(shell, 0.3));
    // Sloped control desk.
    let desk = light(shell, 0.05);
    c.rect(2, 17, 44, 11, desk);
    c.hline(2, 17, 44, light(desk, 0.45));
    c.hline(2, 18, 44, light(desk, 0.25));
    c.hline(2, 27, 44, dark(desk, 0.3));
    for (i, x) in [5, 17, 29].into_iter().enumerate() {
        c.rect(x, 20, 12, 7, dark(desk, 0.12));
        c.hline(x, 20, 12, dark(desk, 0.25));
        let keys = [p.accent, p.accent2, PAPER, SUN];
        for k in 0..4 {
            c.set(x + 1 + k * 2, 22, keys[(k as usize + i) % 4]);
        }
        for k in 0..3 {
            c.vline(x + 8 + k, 21, 5, dark(desk, 0.35));
            c.set(x + 8 + k, 22 + (k + i as i32) % 3, METAL);
        }
    }
    c.rect(41, 20, 3, 4, p.accent);
    c.hline(41, 20, 3, light(p.accent, 0.4));
    // Pedestal with service hatches and a vent.
    block(c, 4, 28, 40, 18, 0, shell);
    c.hline(4, 28, 40, dark(shell, 0.4));
    c.rect(4, 30, 40, 2, trim);
    c.hline(4, 30, 40, light(trim, 0.3));
    for x in [7, 26] {
        c.round(x, 34, 15, 9, 1, dark(shell, 0.2));
        c.round(x + 1, 35, 13, 7, 1, shell);
        c.hline(x + 1, 35, 13, light(shell, 0.3));
        for (dx, dy) in [(1, 1), (11, 1), (1, 5), (11, 5)] {
            c.set(x + 1 + dx, 35 + dy, dark(shell, 0.3));
        }
    }
    for x in (10..19).step_by(2) {
        c.vline(x, 37, 3, dark(shell, 0.35));
    }
    c.rect(29, 37, 7, 3, p.accent);
    c.hline(29, 37, 7, light(p.accent, 0.35));
    c.rect(37, 37, 2, 3, dark(shell, 0.3));
    c.rect(4, 44, 40, 2, dark(trim, 0.3));
    // A mug and a sticky note.
    cup(c, 39, 14, p.accent2, 0x6b4630);
    c.rect(26, 3, 3, 3, SUN);
    // Slots for the animated status lights.
    for i in 0..6 {
        c.rect(10 + (i % 3) * 12, 14 + (i / 3) * 10, 2, 1, SLOT);
    }
}

// ---------------------------------------------------------------- alpine

fn stones(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, base: u32, seed: i32) {
    let mortar = dark(base, 0.45);
    c.rect(x, y, w, h, mortar);
    let mut row = 0;
    let mut sy = y;
    while sy < y + h {
        let sh = 3 + hash(seed + row * 7) % 2;
        let mut sx = x - (hash(seed + row) % 4);
        let mut k = 0;
        while sx < x + w {
            let sw = 4 + hash(seed + row * 13 + k) % 4;
            let tone = match hash(seed + row * 5 + k * 3) % 3 {
                0 => light(base, 0.12),
                1 => base,
                _ => dark(base, 0.1),
            };
            let (x0, x1) = (sx.max(x), (sx + sw - 1).min(x + w));
            let bottom = (sy + sh - 1).min(y + h);
            if x1 > x0 && bottom > sy {
                c.round(x0, sy, x1 - x0, bottom - sy, 1, tone);
                c.hline(x0 + 1, sy, x1 - x0 - 2, light(tone, 0.3));
                c.hline(x0 + 1, bottom - 1, x1 - x0 - 2, dark(tone, 0.2));
            }
            sx += sw;
            k += 1;
        }
        sy += sh;
        row += 1;
    }
}

fn fireplace(c: &mut Canvas, p: &Pal, v: usize) {
    let stone = mix(0x9b958c, p.t.trim, 0.12);
    let wood = dark(p.wood, 0.1);
    // Chimney breast and surround.
    stones(c, 9, 1, 30, 17, stone, 3 + v as i32);
    c.vline(38, 1, 17, dark(stone, 0.4));
    stones(c, 5, 21, 38, 22, stone, 11 + v as i32);
    c.vline(42, 21, 22, dark(stone, 0.4));
    // Firebox: dark arch with brick sides, logs and embers.
    let soot = 0x1f1418;
    c.round(13, 24, 22, 19, 3, dark(stone, 0.5));
    c.round(14, 25, 20, 18, 3, soot);
    let brick = 0x5a2e26;
    for y in (28..41).step_by(3) {
        c.hline(14, y, 2, brick);
        c.hline(32, y, 2, brick);
    }
    c.rect(16, 39, 16, 3, 0x3a2420);
    for x in (17..31).step_by(2) {
        c.set(x, 40, if x % 4 == 1 { 0xff7a3d } else { 0xb8402f });
    }
    rod(c, (15.0, 38.0), (32.0, 36.5), 1.6, 1.6, BARK);
    rod(c, (17.0, 37.0), (31.0, 39.0), 1.5, 1.5, dark(BARK, 0.1));
    for (x, y) in [(15, 37), (31, 38)] {
        c.set(x, y, 0xd9b27c);
    }
    c.hline(18, 36, 12, 0xff9a4a);
    c.rect(14, 42, 20, 1, METAL);
    // Hearth slab.
    c.rect(3, 42, 42, 4, light(stone, 0.12));
    c.hline(3, 42, 42, light(stone, 0.4));
    c.hline(3, 45, 42, dark(stone, 0.35));
    for x in [13, 26, 37] {
        c.vline(x, 43, 2, dark(stone, 0.2));
    }
    // Mantel beam with corbels.
    c.rect(3, 17, 42, 2, light(wood, 0.25));
    c.hline(3, 17, 42, light(wood, 0.45));
    c.rect(3, 19, 42, 3, wood);
    c.hline(3, 21, 42, dark(wood, 0.35));
    for x in [7, 11, 33, 38] {
        c.set(x, 20, dark(wood, 0.2));
    }
    for x in [6, 39] {
        c.rect(x, 22, 3, 3, dark(wood, 0.15));
        c.set(x + 2, 24, dark(wood, 0.4));
    }
    // Garland with berries.
    for x in 4..44 {
        let sag = ((x - 4) % 10 - 5_i32).abs();
        let y = 22 + (5 - sag) / 2;
        c.set(
            x,
            y,
            if x % 2 == 0 {
                0x3f6f58
            } else {
                light(0x3f6f58, 0.2)
            },
        );
        if x % 5 == 2 {
            c.set(x, y + 1, p.fabric);
        }
    }
    // Candles, a framed picture and a sprig on the mantel.
    for (x, h) in [(5, 6), (8, 4)] {
        c.rect(x, 17 - h, 2, h, PAPER);
        c.vline(x + 1, 17 - h, h, dark(PAPER, 0.15));
        c.set(x, 16 - h, GLOW);
        c.set(x, 15 - h, 0xff9a4a);
    }
    c.rect(17, 8, 11, 9, p.t.trim);
    c.rect(18, 9, 9, 7, 0x9fc4e8);
    c.stamp(
        18,
        11,
        &["...k.....", "..kkk..k.", ".kkkkkkkk", "kkkkkkkkk"],
        &[(b'k', 0xe8eef8)],
    );
    c.hline(18, 15, 9, dark(0x3f6f58, 0.1));
    c.hline(17, 8, 11, light(p.t.trim, 0.3));
    c.rect(33, 12, 4, 5, p.accent);
    c.hline(33, 12, 4, light(p.accent, 0.35));
    for (x, y) in [(33, 9), (35, 8), (34, 10), (36, 10)] {
        c.set(x, y, 0x3f6f58);
    }
    // Slot for the animated flames: keep the firebox dark there.
    c.rect(16, 29, 15, 7, soot);
}

/// A sawn log seen end on, seven pixels across.
fn log_end(c: &mut Canvas, x: i32, y: i32, seed: i32) {
    let face = match hash(seed) % 3 {
        0 => 0xe0b984,
        1 => 0xd4a46c,
        _ => 0xe8c592,
    };
    let bark = if hash(seed + 3) % 2 == 0 {
        BARK
    } else {
        mix(BARK, 0x8a6a4a, 0.4)
    };
    c.stamp(
        x,
        y,
        &[
            "..bbb..", ".bwwwd.", "bwwrwwd", "bwrcrwd", "bwwrwwd", ".dwwwd.", "..ddd..",
        ],
        &[
            (b'b', light(bark, 0.2)),
            (b'd', dark(bark, 0.25)),
            (b'w', face),
            (b'r', dark(face, 0.16)),
            (b'c', dark(face, 0.32)),
        ],
    );
    c.set(x + 2, y + 1, light(face, 0.4));
    if hash(seed + 7) % 3 == 0 {
        c.set(x + 4, y + 2, dark(face, 0.25));
    }
}

fn wood_pile(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    // Log rows, packed and staggered; the end posts hide the cut edges.
    let rows: [(i32, &[i32]); 4] = [
        (37, &[6, 12, 18, 24]),
        (31, &[3, 9, 15, 21, 27]),
        (25, &[6, 12, 18, 24]),
        (19, &[9, 15, 21]),
    ];
    for (row, (y, xs)) in rows.into_iter().enumerate() {
        for (k, &x) in xs.iter().enumerate() {
            log_end(c, x, y, row as i32 * 11 + k as i32 + v as i32 * 5);
        }
    }
    // Two split logs lying across the top.
    for (x0, x1, y) in [(8.0, 22.0, 16.5), (14.0, 27.0, 14.0)] {
        rod(c, (x0, y), (x1, y), 2.2, 2.2, BARK);
        c.oval(x1 as i32 - 1, y as i32 - 2, 3, 5, 0xe0b984);
        c.set(x1 as i32, y as i32 - 1, light(0xe0b984, 0.4));
    }
    // Rack posts and base beam.
    for x in [3, 30] {
        c.rect(x, 17, 3, 29, dark(wood, 0.1));
        c.vline(x, 17, 29, light(wood, 0.2));
        c.vline(x + 2, 17, 29, dark(wood, 0.35));
        c.hline(x, 17, 3, light(wood, 0.45));
    }
    c.rect(3, 43, 30, 3, dark(wood, 0.2));
    c.hline(3, 43, 30, light(wood, 0.1));
    c.hline(3, 45, 30, dark(wood, 0.45));
    // Chopping block with an axe and chips.
    let stump = 0x8a5e3c;
    c.rect(33, 36, 12, 10, stump);
    for x in [35, 38, 42] {
        c.vline(x, 38, 7, dark(stump, 0.3));
    }
    c.vline(33, 36, 10, light(stump, 0.15));
    c.vline(44, 36, 10, dark(stump, 0.35));
    c.oval(33, 33, 12, 5, 0xd9b27c);
    c.oval(35, 34, 8, 3, dark(0xd9b27c, 0.12));
    c.oval(37, 35, 4, 1, 0xd9b27c);
    rod(c, (38.5, 34.5), (45.0, 21.0), 1.0, 1.0, light(wood, 0.3));
    c.set(45, 21, dark(wood, 0.2));
    c.stamp(34, 31, &["mmmmm.", "mmmmmm", ".mmmm."], &[(b'm', METAL)]);
    c.hline(34, 31, 5, light(METAL, 0.45));
    c.set(34, 32, 0xe0e4ec);
    for (x, y) in [(31, 45), (46, 44), (29, 44)] {
        c.set(x, y, 0xe0b984);
    }
    // Basket of kindling.
    for (x, top) in [(5, 30), (8, 28), (11, 31), (7, 32)] {
        c.line((x, 37), (x + 1, top), 0xa0703f);
        c.set(x + 1, top, light(0xa0703f, 0.4));
    }
    let wicker = 0xc79a5a;
    c.rect(2, 37, 13, 9, wicker);
    for y in (38..46).step_by(2) {
        for x in (2..15).step_by(2) {
            c.set(x + (y / 2) % 2, y, dark(wicker, 0.22));
        }
    }
    c.hline(2, 36, 13, light(wicker, 0.35));
    c.hline(2, 37, 13, dark(wicker, 0.1));
    c.hline(2, 45, 13, dark(wicker, 0.4));
    c.rect(1, 40, 2, 3, dark(wicker, 0.15));
    c.rect(14, 40, 2, 3, dark(wicker, 0.25));
    // A knitted mitten left on the basket.
    c.stamp(
        8,
        33,
        &[".kk..", "kkkk.", "kkkkk", "kkkk.", "wwww."],
        &[(b'k', p.fabric), (b'w', PAPER)],
    );
    c.set(9, 34, light(p.fabric, 0.35));
}

// ---------------------------------------------------------------- seaside

/// Surfboard standing on its tail, leaning by `lean` pixels at the nose.
fn board(
    c: &mut Canvas,
    cx: i32,
    top: i32,
    bottom: i32,
    half: f64,
    lean: f64,
    color: u32,
    stripe: u32,
    style: usize,
) {
    let len = (bottom - top) as f64;
    for y in top..=bottom {
        let t = (y - top) as f64 / len;
        let s = if t < 0.45 {
            (t / 0.45).powf(0.55)
        } else {
            (1.0 - ((t - 0.45) / 0.55).powi(4)).sqrt()
        };
        let w = half * s;
        let mid = cx as f64 + lean * (1.0 - t);
        let (x0, x1) = ((mid - w).round() as i32, (mid + w).round() as i32);
        if x1 < x0 {
            continue;
        }
        let fill = match style {
            0 if (0.25..0.31).contains(&t) || (0.34..0.37).contains(&t) => stripe,
            1 if t > 0.55 => stripe,
            2 if (mid - (x0 + x1) as f64 / 2.0).abs() < 1.0 && (x1 - x0) > 2 => color,
            _ => color,
        };
        c.hline(x0, y, x1 - x0 + 1, fill);
        if style == 2 && x1 - x0 > 4 {
            c.set(x0 + 1, y, stripe);
            c.set(x1 - 1, y, stripe);
        }
        c.set(x0, y, light(fill, 0.3));
        c.set(x1, y, dark(fill, 0.3));
        if x1 - x0 > 3 {
            c.set(mid.round() as i32, y, dark(fill, 0.12));
        }
    }
}

fn surf_rack(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    // Back rail the boards rest against.
    c.rect(3, 15, 42, 2, dark(wood, 0.2));
    c.hline(3, 15, 42, light(wood, 0.1));
    let boards = [
        (10, 3, 4.5, -1.5, p.accent, PAPER, 0),
        (19, 9, 4.0, -1.0, PAPER, p.accent2, 2),
        (28, 1, 4.5, 1.0, p.fabric, mix(p.fabric, PAPER, 0.5), 1),
        (37, 7, 4.0, 1.5, p.accent2, p.accent, 0),
    ];
    for (i, &(cx, top, half, lean, color, stripe, style)) in boards.iter().enumerate() {
        board(
            c,
            cx,
            top,
            43,
            half,
            lean,
            color,
            stripe,
            (style + v + i) % 3,
        );
    }
    // A hibiscus on the second board.
    bloom(c, 19, 24, p.accent, SUN);
    // Front rail with pegs, end posts and feet.
    c.rect(3, 40, 42, 3, wood);
    c.hline(3, 40, 42, light(wood, 0.4));
    c.hline(3, 42, 42, dark(wood, 0.35));
    for x in [14, 23, 32] {
        c.rect(x, 37, 2, 4, light(wood, 0.15));
        c.set(x, 37, light(wood, 0.45));
    }
    for x in [2, 43] {
        c.rect(x, 13, 3, 33, light(wood, 0.05));
        c.vline(x + 2, 14, 32, dark(wood, 0.3));
        c.hline(x, 13, 3, light(wood, 0.45));
    }
    c.hline(1, 45, 6, dark(wood, 0.3));
    c.hline(41, 45, 6, dark(wood, 0.3));
    // Striped towel over the right post and a starfish on the rail.
    for y in 14..28 {
        let color = if (y / 2) % 2 == 0 { p.accent2 } else { PAPER };
        c.hline(41, y, 5, color);
        c.set(45, y, dark(color, 0.25));
    }
    c.hline(41, 28, 5, dark(PAPER, 0.2));
    c.stamp(
        7,
        37,
        &["..k..", "kkkkk", ".kkk.", "k...k"],
        &[(b'k', p.accent)],
    );
    c.set(9, 38, light(p.accent, 0.4));
}

fn deck_chair(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = p.wood;
    // Umbrella pole and stand.
    c.rect(27, 10, 2, 34, dark(wood, 0.1));
    c.vline(27, 10, 34, light(wood, 0.25));
    c.oval(23, 42, 10, 4, dark(METAL, 0.1));
    c.hline(24, 42, 8, light(METAL, 0.2));
    // Canopy: striped dome with a scalloped hem.
    let (cx, base) = (27.5, 12.0);
    for y in 1..14 {
        for x in 7..47 {
            let dx = (x as f64 + 0.5 - cx) / 20.0;
            let dy = (y as f64 + 0.5 - base) / 11.0;
            let hem = y >= 11 && ((x as f64 - 7.0) % 5.0 - 2.5).abs() > (13 - y) as f64 + 0.5;
            if dx * dx + dy * dy > 1.0 && y < 12 || y >= 12 && hem || dx.abs() > 1.0 {
                continue;
            }
            let wedge = ((x as f64 + 0.5 - cx) / (y as f64 + 3.0) * 2.2).floor() as i32;
            let base_color = if wedge.rem_euclid(2) == 0 {
                p.accent
            } else {
                PAPER
            };
            let color = if dy < -0.6 && dx < 0.0 {
                light(base_color, 0.25)
            } else if dx > 0.55 || y >= 12 {
                dark(base_color, 0.2)
            } else {
                base_color
            };
            c.set(x, y, color);
        }
    }
    c.rect(27, 0, 2, 2, p.accent2);
    // Chair: back legs, striped sling, front rails.
    let frame = wood;
    for (top, foot) in [((8, 26), (10, 45)), ((20, 26), (18, 45))] {
        c.line(top, foot, dark(frame, 0.35));
    }
    let (a, b) = (p.fabric, PAPER);
    for y in 19..38 {
        let t = (y - 19) as f64 / 18.0;
        let inset = (t * 2.0) as i32;
        for x in 6 + inset..23 - inset {
            let base_color = if ((x - 6) / 3) % 2 == 0 { a } else { b };
            let color = if y > 32 {
                dark(base_color, 0.18)
            } else if y == 19 {
                light(base_color, 0.3)
            } else {
                base_color
            };
            c.set(x, y, color);
        }
    }
    c.hline(5, 18, 19, light(frame, 0.2));
    c.hline(5, 18, 19, light(frame, 0.35));
    for (top, foot) in [((5, 18), (3, 45)), ((23, 18), (25, 45))] {
        c.line(top, foot, frame);
        c.line((top.0 + 1, top.1), (foot.0 + 1, foot.1), dark(frame, 0.25));
    }
    c.rect(4, 37, 22, 2, frame);
    c.hline(4, 37, 22, light(frame, 0.35));
    for x in [2, 21] {
        c.rect(x, 29, 6, 2, light(frame, 0.1));
        c.hline(x, 29, 6, light(frame, 0.4));
    }
    // Straw hat on the top corner.
    let straw = 0xe8c77a;
    c.oval(17, 14, 10, 4, straw);
    c.oval(19, 11, 6, 5, light(straw, 0.15));
    c.hline(19, 14, 6, p.accent);
    c.set(20, 12, light(straw, 0.45));
    // Side table with a drink and sunglasses.
    let table = PAPER;
    c.oval(32, 30, 14, 5, table);
    c.oval(33, 30, 10, 3, light(table, 0.3));
    c.hline(33, 34, 12, dark(table, 0.3));
    c.rect(38, 35, 2, 9, dark(METAL, 0.1));
    c.oval(34, 43, 10, 3, dark(METAL, 0.2));
    let juice = if v.is_multiple_of(2) {
        0xffa64a
    } else {
        0xf26f8f
    };
    c.rect(35, 24, 4, 7, mix(GLASS, juice, 0.75));
    c.vline(35, 24, 7, light(juice, 0.4));
    c.hline(35, 24, 4, light(GLASS, 0.3));
    c.line((37, 24), (39, 20), p.accent2);
    c.oval(38, 22, 3, 3, 0xf6e05c);
    c.rect(40, 31, 2, 1, INK);
    c.rect(43, 31, 2, 1, INK);
    c.set(42, 31, METAL);
}

// ---------------------------------------------------------------- zen

/// A cloud-pruned foliage pad.
fn pad(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, leaf: u32, flower: Option<u32>) {
    c.oval(x, y + 1, w, h, dark(leaf, 0.3));
    c.oval(x, y, w, h - 1, leaf);
    c.oval(x + 1, y, w - 3, h / 2 + 1, light(leaf, 0.2));
    for i in 0..w / 3 {
        c.set(x + 2 + i * 3, y + h / 2, dark(leaf, 0.15));
        c.set(x + 1 + i * 3, y + 1, light(leaf, 0.4));
    }
    if let Some(f) = flower {
        for i in 0..w / 4 {
            c.set(x + 2 + i * 4, y + 1 + i % 2 * 2, f);
        }
    }
}

fn bonsai(c: &mut Canvas, p: &Pal, v: usize) {
    let stand = dark(p.wood, 0.2);
    // Low stand.
    c.rect(4, 34, 40, 3, light(stand, 0.2));
    c.hline(4, 34, 40, light(stand, 0.45));
    c.rect(4, 37, 40, 3, stand);
    c.hline(4, 39, 40, dark(stand, 0.35));
    for x in [5, 39] {
        c.rect(x, 40, 4, 6, dark(stand, 0.1));
        c.vline(x + 3, 40, 6, dark(stand, 0.4));
    }
    c.hline(9, 40, 30, dark(stand, 0.25));
    // Glazed pot.
    let glaze = p.accent2;
    c.rect(11, 28, 26, 2, light(glaze, 0.25));
    c.hline(11, 28, 26, light(glaze, 0.45));
    c.rect(12, 30, 24, 4, glaze);
    c.hline(12, 33, 24, dark(glaze, 0.35));
    c.vline(35, 30, 4, dark(glaze, 0.25));
    c.rect(13, 34, 3, 1, dark(glaze, 0.4));
    c.rect(32, 34, 3, 1, dark(glaze, 0.4));
    // Moss mound and roots.
    let moss = 0x6d8f4a;
    c.oval(13, 25, 22, 5, moss);
    c.hline(15, 25, 16, light(moss, 0.3));
    c.line((22, 27), (17, 28), BARK);
    c.line((26, 27), (31, 28), BARK);
    // Trunk and branches.
    let bark = BARK;
    rod(c, (24.0, 28.0), (20.0, 21.0), 2.6, 2.2, bark);
    rod(c, (20.0, 21.5), (23.5, 15.0), 2.2, 1.8, bark);
    rod(c, (23.5, 15.5), (26.0, 9.0), 1.8, 1.3, bark);
    rod(c, (20.0, 21.0), (11.0, 18.0), 1.2, 0.9, bark);
    rod(c, (23.0, 15.0), (34.0, 14.0), 1.2, 0.9, bark);
    // Foliage pads by variant: pine, blossom or autumn maple.
    let (leaf, flower) = match v % 3 {
        0 => (0x4f7a3f, None),
        1 => (0x5f8a48, Some(0xf6b6c8)),
        _ => (mix(p.accent, 0xe08a3a, 0.45), Some(SUN)),
    };
    pad(c, 3, 13, 16, 7, leaf, flower);
    pad(c, 29, 10, 16, 7, leaf, flower);
    pad(c, 15, 2, 18, 8, leaf, flower);
    pad(c, 12, 9, 9, 5, leaf, flower);
    // A tea cup and a smooth stone on the stand.
    cup(c, 5, 30, PAPER, 0x9cba6a);
    c.oval(38, 31, 5, 3, 0x8a8a84);
    c.set(39, 31, light(0x8a8a84, 0.4));
}

fn stone_lantern(c: &mut Canvas, p: &Pal, v: usize) {
    let stone = mix(0xa7a59a, p.t.trim, 0.08);
    let moss = 0x6d8f4a;
    // Raked gravel bed.
    let gravel = 0xd6d1c2;
    c.oval(1, 35, 46, 11, gravel);
    for (i, r) in [8, 14, 20].into_iter().enumerate() {
        for x in 24 - r..24 + r {
            let dx = (x - 24) as f64 / r as f64;
            let y = 41 + ((1.0 - dx * dx).max(0.0).sqrt() * (2 + i as i32) as f64) as i32;
            if y < 46 {
                c.set(x, y, dark(gravel, 0.15));
            }
        }
    }
    for i in 0..18 {
        c.set(3 + hash(i) % 42, 37 + hash(i + 40) % 8, light(gravel, 0.4));
    }
    // Rocks and a fern clump.
    c.oval(3, 36, 9, 6, dark(stone, 0.1));
    c.oval(4, 36, 6, 3, light(stone, 0.2));
    c.set(5, 37, moss);
    c.oval(38, 39, 6, 4, stone);
    clump(c, 37, 31, 9, 8, moss);
    clump(c, 40, 34, 6, 6, light(moss, 0.1));
    // Base.
    c.round(13, 37, 22, 7, 2, stone);
    c.hline(14, 37, 20, light(stone, 0.35));
    c.hline(14, 43, 20, dark(stone, 0.35));
    c.vline(34, 39, 3, dark(stone, 0.25));
    // Post with a ring.
    c.rect(20, 28, 8, 9, stone);
    c.vline(20, 28, 9, light(stone, 0.2));
    c.vline(27, 28, 9, dark(stone, 0.3));
    c.hline(20, 32, 8, dark(stone, 0.18));
    // Platform.
    c.rect(14, 24, 20, 4, stone);
    c.hline(14, 24, 20, light(stone, 0.4));
    c.hline(14, 27, 20, dark(stone, 0.35));
    c.rect(16, 28, 16, 1, dark(stone, 0.25));
    // Light box with a glowing window.
    c.rect(17, 15, 14, 9, stone);
    c.vline(17, 15, 9, light(stone, 0.18));
    c.vline(30, 15, 9, dark(stone, 0.3));
    c.rect(20, 16, 8, 7, 0x3a2a20);
    c.rect(21, 17, 6, 5, 0xffb35a);
    c.rect(22, 18, 4, 3, GLOW);
    c.vline(24, 17, 5, dark(stone, 0.2));
    // Roof with upturned corners.
    for (i, y) in (8..15).enumerate() {
        let hw = 4 + i as i32 * 2;
        c.hline(
            24 - hw,
            y,
            hw * 2,
            if y < 12 { light(stone, 0.15) } else { stone },
        );
        c.hline(
            24 - hw,
            y,
            hw,
            light(stone, if y < 12 { 0.3 } else { 0.15 }),
        );
    }
    c.hline(10, 14, 28, dark(stone, 0.35));
    c.rect(8, 12, 2, 2, stone);
    c.rect(38, 12, 2, 2, dark(stone, 0.15));
    c.set(8, 11, light(stone, 0.3));
    c.set(39, 11, stone);
    // Finial.
    c.rect(22, 6, 4, 2, stone);
    c.oval(21, 1, 6, 6, light(stone, 0.1));
    c.set(23, 0, light(stone, 0.2));
    c.set(22, 2, light(stone, 0.45));
    // Moss on the roof and base.
    for (x, y) in [
        (15, 11),
        (16, 11),
        (17, 10),
        (14, 12),
        (19, 9),
        (13, 38),
        (14, 38),
        (31, 24),
        (15, 24),
    ] {
        c.set(
            x,
            y,
            if (x + y) % 2 == 0 {
                moss
            } else {
                light(moss, 0.25)
            },
        );
    }
    if v % 2 == 1 {
        c.rect(30, 38, 3, 2, moss);
    }
}

// ---------------------------------------------------------------- pop arcade

fn candy_jar(c: &mut Canvas, x: i32, y: i32, w: i32, h: i32, lid: u32, candy: &[u32], seed: usize) {
    let glass = mix(GLASS, PAPER, 0.3);
    c.rect(x, y, w, h, glass);
    let mut k = seed;
    for yy in (y + 2..y + h - 1).step_by(2) {
        for xx in (x + 1..x + w - 1).step_by(2) {
            let color = candy[k % candy.len()];
            k += 3;
            c.rect(xx, yy, 2, 2, color);
            c.set(xx, yy, light(color, 0.45));
        }
        k += 1;
    }
    c.vline(x, y, h, light(glass, 0.5));
    c.vline(x + w - 1, y, h, dark(glass, 0.2));
    c.hline(x, y + h - 1, w, dark(glass, 0.3));
    c.rect(x - 1, y - 2, w + 2, 2, lid);
    c.hline(x - 1, y - 2, w + 2, light(lid, 0.4));
    c.rect(x + w / 2 - 1, y - 3, 2, 1, dark(lid, 0.1));
}

fn gumball(c: &mut Canvas, p: &Pal, v: usize) {
    let body = p.accent;
    let balls = [
        p.accent, p.accent2, 0xffd84d, 0x7a5bd1, 0x7fdc8a, PAPER, 0xff8a3d,
    ];
    let candy = [p.accent, 0xffd84d, p.accent2, 0x7fdc8a, 0xff8a3d, 0x7a5bd1];
    // Candy shelf at the right: an upper board and a striped cabinet.
    let shelf = mix(p.wood, p.accent2, 0.15);
    for x in [27, 44] {
        c.rect(x, 18, 2, 16, dark(shelf, 0.15));
    }
    c.rect(26, 21, 21, 2, light(shelf, 0.1));
    c.hline(26, 21, 21, light(shelf, 0.45));
    c.hline(26, 22, 21, dark(shelf, 0.3));
    block(c, 26, 33, 20, 13, 2, shelf);
    for x in (27..46).step_by(4) {
        c.line((x, 36), (x + 3, 43), mix(p.accent, PAPER, 0.4));
    }
    c.hline(26, 44, 20, dark(shelf, 0.45));
    candy_jar(c, 29, 13, 7, 8, p.accent2, &candy, v);
    candy_jar(c, 29, 26, 6, 7, 0x7a5bd1, &candy, v + 2);
    candy_jar(c, 37, 25, 7, 8, p.accent, &candy, v + 4);
    // Lollipops in a cup.
    for (x, top, color) in [(39, 9, p.accent), (41, 6, 0xffd84d), (43, 10, p.accent2)] {
        c.vline(x, top + 4, 17 - top - 3, PAPER);
        c.oval(x - 2, top, 5, 5, color);
        c.set(x - 1, top + 1, light(color, 0.6));
        c.set(x, top + 2, dark(color, 0.25));
        c.set(x + 1, top + 3, dark(color, 0.25));
    }
    c.rect(38, 16, 7, 5, p.accent2);
    c.hline(38, 16, 7, light(p.accent2, 0.4));
    c.vline(44, 17, 4, dark(p.accent2, 0.3));
    c.hline(39, 18, 5, PAPER);
    // Machine: cap, globe of gumballs, body and pedestal.
    c.oval(10, 1, 10, 4, body);
    c.hline(12, 1, 6, light(body, 0.4));
    c.rect(14, 0, 2, 2, METAL);
    c.oval(5, 3, 20, 20, dark(GLASS, 0.2));
    c.oval(6, 4, 18, 18, mix(GLASS, PAPER, 0.4));
    for row in 0..6 {
        for col in 0..6 {
            let (x, y) = (6 + col * 3 + row % 2, 9 + row * 2 + col % 2);
            let (dx, dy) = (x as f64 + 1.5 - 15.0, y as f64 + 1.5 - 13.0);
            if dx * dx + dy * dy > 64.0 {
                continue;
            }
            let color = balls[((row * 3 + col * 5 + v as i32) % balls.len() as i32) as usize];
            c.rect(x, y, 3, 3, color);
            c.set(x, y, light(color, 0.5));
            c.set(x + 2, y + 2, dark(color, 0.25));
        }
    }
    c.line((8, 9), (10, 6), 0xffffff);
    c.set(8, 10, 0xffffff);
    c.rect(8, 21, 14, 2, METAL);
    c.hline(8, 21, 14, light(METAL, 0.4));
    for r in 0..11 {
        let inset = r / 4;
        c.hline(8 + inset, 23 + r, 14 - inset * 2, body);
        c.set(8 + inset, 23 + r, light(body, 0.25));
        c.set(21 - inset, 23 + r, dark(body, 0.3));
    }
    c.rect(11, 25, 8, 6, METAL);
    c.hline(11, 25, 8, light(METAL, 0.4));
    c.oval(12, 26, 5, 4, light(METAL, 0.2));
    c.hline(13, 27, 3, dark(METAL, 0.4));
    c.rect(17, 26, 1, 2, INK);
    c.rect(13, 31, 4, 3, dark(METAL, 0.3));
    c.set(14, 32, balls[v % balls.len()]);
    c.rect(12, 34, 6, 8, body);
    c.vline(12, 34, 8, light(body, 0.25));
    c.vline(17, 34, 8, dark(body, 0.3));
    c.oval(6, 41, 18, 5, dark(body, 0.1));
    c.oval(7, 41, 15, 3, light(body, 0.2));
}
