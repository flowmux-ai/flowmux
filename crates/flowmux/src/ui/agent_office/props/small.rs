// SPDX-License-Identifier: GPL-3.0-or-later
//! Potted plants (22x34) and floor clutter (18x24).
//!
//! Foliage is rasterized from curved blades: a quadratic midrib, a width
//! profile and optional periodic cuts for leaflets or splits. Each blade side
//! is lit or shaded by whether it faces the upper-left light, so leaves keep
//! clean silhouettes at any angle. Pots and clutter use flat shaded shapes like
//! the rest of the furniture.

use super::{mug, Pal, INK, METAL, PAPER};
use crate::ui::agent_office::{
    sprite::{dark, light, mix, Canvas, CLEAR},
    theme::{Clutter, Plant},
};

type Pt = (f64, f64);

const SOIL: u32 = 0x4a3428;
const TERRACOTTA: u32 = 0xc8774f;
const PORCELAIN: u32 = 0xeeeae0;
const WICKER: u32 = 0xc99a5b;
const CLAY: u32 = 0xd6b68f;
const HOOP: u32 = 0x55596a;
const GLOW: u32 = 0xfff1c6;
const BRASS: u32 = 0xc9a25a;
const PINK: u32 = 0xf27aa0;
const FUR: u32 = 0xeaa65a;
/// Returned by a blade painter for a gap: foliage behind it darkens, and
/// empty space stays transparent so the outline closes the notch.
const CUT: u32 = 0x0100_0000;

/// Five-step foliage ramp: deep, dark, mid, light, highlight.
fn greens(mid: u32) -> [u32; 5] {
    [
        dark(mid, 0.55),
        dark(mid, 0.28),
        mid,
        light(mix(mid, 0xb8d860, 0.35), 0.12),
        light(mix(mid, 0xd6ea8a, 0.55), 0.3),
    ]
}

/// A leaf, frond or blade along a quadratic curve from its base to its tip.
struct Blade {
    from: Pt,
    ctrl: Pt,
    to: Pt,
    width: f64,
}

/// One pixel of a blade: position along it (`t` 0..1, `s` in pixels), signed
/// distance from the midrib, local half width, and whether it faces the light.
struct Px {
    t: f64,
    s: f64,
    side: f64,
    hw: f64,
    lit: bool,
}

impl Px {
    fn edge(&self) -> bool {
        self.side.abs() > self.hw - 1.0
    }

    /// Periodic cut lines that lean toward the tip away from the midrib.
    fn cut(&self, period: f64, lean: f64, gap: f64) -> bool {
        let phase = (self.s - lean * self.side.abs()) / period;
        phase - phase.floor() < gap / period
    }
}

fn bez(b: &Blade, t: f64) -> Pt {
    let u = 1.0 - t;
    (
        u * u * b.from.0 + 2.0 * u * t * b.ctrl.0 + t * t * b.to.0,
        u * u * b.from.1 + 2.0 * u * t * b.ctrl.1 + t * t * b.to.1,
    )
}

/// Rasterizes `b`. With `rim`, foliage already painted just outside the
/// silhouette takes that color so overlapping leaves stay separate.
fn blade(
    c: &mut Canvas,
    b: &Blade,
    rim: Option<u32>,
    profile: impl Fn(f64) -> f64,
    paint: impl Fn(&Px) -> Option<u32>,
) {
    const N: usize = 48;
    let pts: Vec<Pt> = (0..=N).map(|i| bez(b, i as f64 / N as f64)).collect();
    let mut arc = vec![0.0; N + 1];
    for i in 1..=N {
        arc[i] = arc[i - 1] + (pts[i].0 - pts[i - 1].0).hypot(pts[i].1 - pts[i - 1].1);
    }
    let total = arc[N].max(0.01);
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in &pts {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let pad = b.width + 1.0;
    for y in (y0 - pad).floor() as i32..=(y1 + pad).ceil() as i32 {
        for x in (x0 - pad).floor() as i32..=(x1 + pad).ceil() as i32 {
            // Stay inside the one-pixel margin the outline needs.
            if x < 1 || y < 1 || x > c.w - 2 || y > c.h - 2 {
                continue;
            }
            let q = (x as f64 + 0.5, y as f64 + 0.5);
            let mut best = 0;
            let mut best_d = f64::MAX;
            for (i, pt) in pts.iter().enumerate() {
                let d = (q.0 - pt.0).powi(2) + (q.1 - pt.1).powi(2);
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            let a = pts[best.saturating_sub(1)];
            let z = pts[(best + 1).min(N)];
            let len = (z.0 - a.0).hypot(z.1 - a.1).max(1e-6);
            let tan = ((z.0 - a.0) / len, (z.1 - a.1) / len);
            let d = (q.0 - pts[best].0, q.1 - pts[best].1);
            let s = arc[best] + d.0 * tan.0 + d.1 * tan.1;
            if s < -1.0 || s > total + 1.0 {
                continue;
            }
            let side = tan.0 * d.1 - tan.1 * d.0;
            let t = (s / total).clamp(0.0, 1.0);
            let hw = b.width * profile(t);
            let outside = s < 0.0 || s > total || side.abs() > hw;
            if outside {
                let near = s >= 0.0 && side.abs() <= hw + 1.0;
                if let Some(color) = rim.filter(|_| near && c.get(x, y) != CLEAR) {
                    c.set(x, y, color);
                }
                continue;
            }
            let sign = if side < 0.0 { -1.0 } else { 1.0 };
            let normal = (-tan.1 * sign, tan.0 * sign);
            let lit = -0.6 * normal.0 - 0.8 * normal.1 > 0.05;
            match paint(&Px {
                t,
                s,
                side,
                hw,
                lit,
            }) {
                Some(CUT) => {
                    let behind = c.get(x, y);
                    if behind != CLEAR {
                        c.set(x, y, rim.unwrap_or_else(|| dark(behind, 0.5)));
                    }
                }
                Some(color) => c.set(x, y, color),
                None => {}
            }
        }
    }
}

/// Default leaf shading: lit half light with a highlight near the midrib,
/// shaded half mid with a darker rim, darker toward the base.
fn leaf_tone(px: &Px, g: &[u32; 5], depth: i32) -> u32 {
    let edge = px.edge();
    let mut tone = match (px.lit, edge) {
        (true, false)
            if px.hw > 2.0 && px.t > 0.25 && px.t < 0.8 && px.side.abs() < px.hw * 0.6 =>
        {
            4
        }
        (true, _) => 3,
        (false, false) => 2,
        (false, true) => 1,
    };
    if px.t < 0.12 {
        tone -= 1;
    }
    g[(tone - depth).clamp(0, 4) as usize]
}

/// Paints the opaque pixels of `mask` in `color`, lit from the upper left:
/// pixels near the upper-left silhouette lighten, those near the lower right deepen.
fn soft(c: &mut Canvas, mask: &Canvas, color: u32, ox: i32, oy: i32) {
    for y in 0..mask.h {
        for x in 0..mask.w {
            if mask.get(x, y) == CLEAR {
                continue;
            }
            let out = |dx: i32, dy: i32| mask.get(x + dx, y + dy) == CLEAR;
            let tone = if out(0, 1) || out(1, 1) && out(1, 0) {
                dark(color, 0.32)
            } else if out(1, 2) || out(0, 2) || out(2, 0) {
                dark(color, 0.14)
            } else if out(0, -1) || out(-1, 0) {
                light(color, 0.32)
            } else if out(-1, -1) || out(0, -2) {
                light(color, 0.14)
            } else {
                color
            };
            c.set(ox + x, oy + y, tone);
        }
    }
}

// ---------------------------------------------------------------------------
// Pots
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Pot {
    Terracotta,
    Glazed(u32),
    Porcelain,
    Basket,
    Bucket,
}

/// Planter with a lip, soil opening and body standing on row 32.
fn pot(c: &mut Canvas, p: &Pal, kind: Pot, x: i32, w: i32, top: i32, taper: bool) {
    const BOTTOM: i32 = 32;
    let base = match kind {
        Pot::Terracotta => TERRACOTTA,
        Pot::Glazed(color) => color,
        Pot::Porcelain => PORCELAIN,
        Pot::Basket => WICKER,
        Pot::Bucket => mix(p.wood, 0xa9784a, 0.5),
    };
    let right = x + w - 1;
    let body = top + 4;
    let rows = BOTTOM - body + 1;
    for y in body..=BOTTOM {
        let row = y - body;
        let inset = if taper { 1 + row * 2 / rows } else { 1 };
        let (l, r) = (x + inset, right - inset);
        for xx in l..=r {
            let mut color = match kind {
                Pot::Basket if (xx + (row / 2) * 2) % 4 < 2 => dark(base, 0.16),
                Pot::Bucket if row == 1 || y == BOTTOM - 1 => HOOP,
                Pot::Bucket if (xx - x) % 3 == 0 => dark(base, 0.2),
                Pot::Glazed(_) if y >= BOTTOM - 1 && xx != x + 4 && xx != r - 3 => CLAY,
                Pot::Porcelain if row == 2 => p.accent2,
                _ => base,
            };
            if row == 0 {
                color = dark(color, 0.35);
            } else if y == BOTTOM {
                color = dark(color, 0.38);
            } else if xx == l {
                color = light(color, 0.22);
            } else if xx == r {
                color = dark(color, 0.32);
            } else if xx == r - 1 {
                color = dark(color, 0.12);
            }
            c.set(xx, y, color);
        }
    }
    if matches!(kind, Pot::Glazed(_) | Pot::Porcelain) {
        c.vline(x + 3, body + 1, 3, light(base, 0.55));
    }
    // Back lip and soil, then the front lip band.
    c.hline(x + 1, top, w - 2, light(base, 0.4));
    c.hline(x + 1, top + 1, w - 2, SOIL);
    c.set(x, top + 1, light(base, 0.3));
    c.set(right, top + 1, dark(base, 0.15));
    for xx in x..=right {
        for (row, amount) in [(2, 0.3), (3, 0.08)] {
            let mut color = light(base, amount);
            if kind == Pot::Basket && ((xx - x) / 2 + row) % 2 == 0 {
                color = dark(base, 0.08);
            }
            if xx == right {
                color = dark(base, if row == 2 { 0.05 } else { 0.3 });
            }
            c.set(xx, top + row, color);
        }
    }
}

// ---------------------------------------------------------------------------
// Plants
// ---------------------------------------------------------------------------

pub(in crate::ui::agent_office) fn plant(c: &mut Canvas, p: &Pal, plant: Plant, v: usize) {
    match plant {
        Plant::Monstera => monstera(c, p, v),
        Plant::Fern => fern(c, p, v),
        Plant::Palm => palm(c, p, v),
        Plant::Snake => snake(c, p, v),
        Plant::Cactus => cactus(c, p, v),
        Plant::Fir => fir(c, p, v),
        Plant::Bamboo => bamboo(c, p, v),
    }
}

type Leaf = (Pt, Pt, Pt, f64, i32);

fn monstera(c: &mut Canvas, p: &Pal, v: usize) {
    let pots = [
        Pot::Glazed(p.accent2),
        Pot::Porcelain,
        Pot::Basket,
        Pot::Terracotta,
        Pot::Glazed(p.accent),
    ];
    pot(c, p, pots[v % pots.len()], 4, 14, 22, true);
    let g = greens(0x3a8a57);
    let stem = 0x6f9f4c;
    let k = (v % 2) as f64;
    let leaves: [Leaf; 5] = [
        ((9.5, 12.5), (6.0, 8.0), (1.5, 4.0 + k), 4.6, 1),
        ((12.5, 12.0), (16.0, 7.5), (20.5, 3.5 + k), 4.6, 1),
        ((11.0, 15.0), (10.5, 9.5), (11.0 - k, 2.0), 4.4, 0),
        ((10.0, 19.0), (5.5, 15.0), (1.0, 18.0), 4.2, 0),
        ((12.0, 18.5), (16.5, 14.0), (21.0, 16.0 + k), 4.2, 0),
    ];
    for &(from, _, _, _, depth) in &leaves {
        let color = if depth == 1 { dark(stem, 0.25) } else { stem };
        c.line((11, 23), (from.0 as i32, from.1 as i32), color);
    }
    let ovate = |t: f64| {
        if t < 0.45 {
            (t / 0.45).powf(0.45)
        } else {
            (1.0 - ((t - 0.45) / 0.55).powi(2)).max(0.0).sqrt()
        }
    };
    for &(from, ctrl, to, width, depth) in &leaves {
        blade(
            c,
            &Blade {
                from,
                ctrl,
                to,
                width,
            },
            Some(g[0]),
            ovate,
            |px| {
                if px.t < 0.1 && px.side.abs() < 1.0 {
                    return None;
                }
                if px.side.abs() > 1.1 && px.t > 0.2 && px.t < 0.88 && px.cut(3.0, 0.4, 1.1) {
                    return Some(CUT);
                }
                if px.side.abs() < 0.5 && px.t > 0.1 && px.t < 0.8 {
                    return Some(g[3 - depth as usize]);
                }
                Some(leaf_tone(px, &g, depth))
            },
        );
    }
}

fn fern(c: &mut Canvas, p: &Pal, v: usize) {
    let pots = [
        Pot::Terracotta,
        Pot::Basket,
        Pot::Porcelain,
        Pot::Glazed(p.accent),
        Pot::Bucket,
    ];
    pot(c, p, pots[v % pots.len()], 4, 14, 22, true);
    let g = greens(0x5a9e45);
    let k = (v % 3) as f64 - 1.0;
    let fronds: [Leaf; 8] = [
        ((10.5, 21.0), (9.0, 9.0), (6.0 + k, 2.0), 2.3, 1),
        ((11.5, 21.0), (13.0, 9.0), (16.0 + k, 2.5), 2.3, 1),
        ((10.0, 21.0), (3.0, 6.0), (1.0, 13.0), 2.3, 1),
        ((12.0, 21.0), (19.0, 6.0), (21.0, 13.0), 2.3, 1),
        ((10.0, 21.5), (2.5, 13.0), (1.0, 24.0), 2.3, 0),
        ((12.0, 21.5), (19.5, 13.0), (21.0, 24.0), 2.3, 0),
        ((11.0, 21.5), (10.0, 12.0), (11.0 - k, 4.0), 2.3, 0),
        ((11.0, 22.0), (6.0, 17.0), (4.0, 27.0), 2.0, 0),
    ];
    let profile = |t: f64| {
        if t < 0.1 {
            0.3
        } else {
            (4.0 * t * (1.0 - t)).powf(0.5).max(0.3)
        }
    };
    for &(from, ctrl, to, width, depth) in &fronds {
        blade(
            c,
            &Blade {
                from,
                ctrl,
                to,
                width,
            },
            Some(g[0]),
            profile,
            |px| {
                if px.side.abs() > 0.7 && px.cut(2.0, 1.1, 0.8) {
                    return Some(CUT);
                }
                Some(leaf_tone(px, &g, depth))
            },
        );
    }
}

fn palm(c: &mut Canvas, p: &Pal, v: usize) {
    let pots = [
        Pot::Basket,
        Pot::Porcelain,
        Pot::Glazed(p.accent),
        Pot::Terracotta,
        Pot::Glazed(p.accent2),
    ];
    pot(c, p, pots[v % pots.len()], 4, 14, 22, true);
    let g = greens(0x4c9a50);
    let cane = 0x8fae58;
    for (a, b) in [
        ((10, 23), (7, 12)),
        ((11, 23), (11, 9)),
        ((12, 23), (15, 11)),
    ] {
        c.line(a, b, cane);
    }
    let k = (v % 2) as f64;
    let fronds: [Leaf; 7] = [
        ((11.0, 9.0), (8.0, 1.0), (3.0, 2.5 + k), 3.0, 1),
        ((11.0, 9.0), (14.0, 1.0), (19.0, 2.5 + k), 3.0, 1),
        ((7.0, 12.0), (1.0, 5.0), (1.0, 14.0), 3.0, 0),
        ((15.0, 11.0), (21.0, 4.0), (21.0, 13.0), 3.0, 0),
        ((11.0, 9.0), (11.0, 3.0), (11.5 - k, 1.0), 2.6, 0),
        ((10.0, 18.0), (4.0, 12.0), (1.5, 21.0), 2.6, 0),
        ((12.0, 18.0), (18.0, 12.0), (20.5, 21.0), 2.6, 0),
    ];
    let profile = |t: f64| (t * 5.0).min(1.0) * (1.0 - t * t).max(0.0);
    for &(from, ctrl, to, width, depth) in &fronds {
        let rim = (depth == 0).then_some(g[0]);
        blade(
            c,
            &Blade {
                from,
                ctrl,
                to,
                width,
            },
            rim,
            profile,
            |px| {
                if px.side.abs() > 0.7 && px.cut(2.0, 1.4, 0.8) {
                    return None;
                }
                Some(leaf_tone(px, &g, depth))
            },
        );
    }
}

fn snake(c: &mut Canvas, p: &Pal, v: usize) {
    let pots = [
        Pot::Porcelain,
        Pot::Glazed(p.accent2),
        Pot::Terracotta,
        Pot::Glazed(p.accent),
        Pot::Basket,
    ];
    pot(c, p, pots[v % pots.len()], 4, 14, 22, true);
    let deep = 0x2f6143;
    let band = 0x6c9a64;
    let margin = 0xd8cc62;
    let edged = v.is_multiple_of(2);
    let blades: [Leaf; 6] = [
        ((9.0, 23.0), (6.5, 14.0), (4.5, 5.0), 2.3, 1),
        ((13.0, 23.0), (15.5, 13.0), (17.5, 3.0), 2.3, 1),
        ((11.0, 23.0), (11.0, 12.0), (10.0, 1.0), 2.6, 0),
        ((8.5, 23.0), (4.0, 18.0), (2.5, 10.0), 2.2, 0),
        ((13.5, 23.0), (18.0, 18.0), (19.5, 9.0), 2.2, 0),
        ((12.0, 23.0), (14.0, 18.0), (14.5, 12.5), 2.0, 0),
    ];
    let profile = |t: f64| {
        if t < 0.6 {
            0.7 + 0.3 * t / 0.6
        } else {
            ((1.0 - t) / 0.4).powf(0.7)
        }
    };
    for &(from, ctrl, to, width, depth) in &blades {
        blade(
            c,
            &Blade {
                from,
                ctrl,
                to,
                width,
            },
            Some(dark(deep, 0.4)),
            profile,
            |px| {
                let edge = px.edge();
                let color = if edged && edge && px.t > 0.1 {
                    if px.lit {
                        margin
                    } else {
                        dark(margin, 0.3)
                    }
                } else {
                    let stripe = (px.s + px.side.abs() * 0.6).rem_euclid(3.5) < 1.0;
                    let col = if stripe { band } else { deep };
                    if px.lit {
                        light(col, 0.18)
                    } else if edge {
                        dark(col, 0.25)
                    } else {
                        col
                    }
                };
                Some(if depth == 1 { dark(color, 0.22) } else { color })
            },
        );
    }
}

fn cactus(c: &mut Canvas, p: &Pal, v: usize) {
    let pots = [
        Pot::Terracotta,
        Pot::Glazed(p.accent2),
        Pot::Porcelain,
        Pot::Glazed(p.accent),
    ];
    pot(c, p, pots[v % pots.len()], 4, 14, 22, true);
    let g = greens(0x4f9a5c);
    let ribs = [g[3], g[4], g[3], g[2], g[2], g[1]];
    let arm = [g[3], g[4], g[2]];
    // Arms as (elbow row, tip row), left then right.
    let arms = if v.is_multiple_of(2) {
        [(15, 8), (12, 5)]
    } else {
        [(12, 6), (15, 9)]
    };
    for (side, (elbow, top)) in arms.into_iter().enumerate() {
        let (ax, from, to) = if side == 0 { (3, 4, 7) } else { (16, 14, 17) };
        for (row, tone) in [g[3], g[2], g[1]].into_iter().enumerate() {
            c.hline(from, elbow + row as i32, to - from + 1, tone);
        }
        for y in top..elbow + 2 {
            for (i, &tone) in arm.iter().enumerate() {
                c.set(ax + i as i32, y, tone);
            }
        }
        c.set(ax, top, CLEAR);
        c.set(ax + 2, top, CLEAR);
        c.set(ax + 1, top, light(g[4], 0.2));
        c.set(if side == 0 { ax } else { ax + 2 }, elbow + 2, CLEAR);
        c.set(ax + 1, elbow + 2, g[1]);
    }
    for y in 5..24 {
        for (i, &tone) in ribs.iter().enumerate() {
            c.set(8 + i as i32, y, tone);
        }
    }
    c.hline(9, 4, 4, g[4]);
    c.hline(10, 3, 2, g[3]);
    if v % 3 == 1 {
        // A pup at the base.
        c.round(13, 19, 4, 4, 1, g[2]);
        c.vline(14, 19, 3, g[4]);
        c.vline(16, 20, 2, g[1]);
    }
    // Spines in short pairs along the ridges.
    let spine = 0xf2ecd2;
    for (x, y) in [
        (9, 8),
        (12, 11),
        (9, 14),
        (12, 17),
        (9, 20),
        (4, 11),
        (17, 9),
    ] {
        c.set(x, y, spine);
    }
    let petal = match v % 3 {
        0 => PINK,
        1 => 0xf6d04d,
        _ => p.accent,
    };
    c.stamp(
        9,
        1,
        &[".pp.", "pyyp", ".qq."],
        &[(b'p', petal), (b'q', dark(petal, 0.25)), (b'y', 0xfff0a0)],
    );
}

fn fir(c: &mut Canvas, p: &Pal, v: usize) {
    let pots = [Pot::Bucket, Pot::Basket, Pot::Bucket, Pot::Terracotta];
    pot(c, p, pots[v % pots.len()], 5, 13, 23, true);
    let g = greens(0x2f6e58);
    c.rect(10, 20, 3, 5, 0x6b4a32);
    c.vline(10, 20, 5, 0x8a6444);
    let tiers = [(12, 21, 9.0), (8, 17, 7.0), (4, 12, 5.0), (1, 7, 3.0)];
    for (i, &(apex, bottom, half)) in tiers.iter().enumerate() {
        for y in apex..=bottom {
            let k = (y - apex + 1) as f64 / (bottom - apex + 1) as f64;
            let hw = (half * k).max(0.6);
            for x in 1..21 {
                let dx = x as f64 - 11.0;
                if dx.abs() > hw {
                    continue;
                }
                if y == bottom && (x + i as i32) % 3 == 0 {
                    continue;
                }
                let tone = if dx < -hw + 1.0 && y > apex + 1 {
                    4
                } else if dx < -hw * 0.4 {
                    3
                } else if dx < hw * 0.25 {
                    2
                } else {
                    1
                };
                c.set(x, y, g[tone]);
            }
        }
        if let Some(&(_, upper, upper_half)) = tiers.get(i + 1) {
            for x in 1..21 {
                let dx = (x as f64 - 11.0).abs();
                if dx <= upper_half + 0.5 && c.get(x, upper + 1) != CLEAR {
                    c.set(x, upper + 1, g[0]);
                }
            }
        }
    }
    match v % 3 {
        1 => {
            // Red ribbon bow on the planter.
            let red = 0xc8423c;
            c.hline(6, 28, 11, red);
            c.stamp(
                8,
                26,
                &["rr.rr", "rrkrr", "r.r.r"],
                &[(b'r', light(red, 0.15)), (b'k', dark(red, 0.3))],
            );
        }
        2 => {
            // Warm string lights and a star.
            for (x, y, color) in [
                (8, 7, 0xffd56a),
                (13, 9, p.accent),
                (7, 12, 0xff8f6a),
                (12, 14, 0xffd56a),
                (16, 16, p.accent),
                (5, 18, 0xffd56a),
                (10, 19, 0xff8f6a),
                (15, 20, 0xffd56a),
            ] {
                c.set(x, y, color);
            }
            c.stamp(
                10,
                1,
                &[".y.", "yYy", ".y."],
                &[(b'y', 0xf2c14e), (b'Y', 0xfff0a0)],
            );
        }
        _ => {}
    }
}

fn bamboo(c: &mut Canvas, p: &Pal, v: usize) {
    let pots = [
        Pot::Glazed(p.accent2),
        Pot::Porcelain,
        Pot::Glazed(dark(p.fabric, 0.1)),
        Pot::Glazed(p.accent),
    ];
    pot(c, p, pots[v % pots.len()], 5, 12, 21, false);
    for (x, color) in [
        (6, 0xd9d3c4),
        (8, 0x9b978d),
        (11, 0xe8e2d2),
        (13, 0xa9a49a),
        (15, 0xd9d3c4),
    ] {
        c.set(x, 22, color);
    }
    let g = greens(0x4f9e48);
    let stalk = 0x9cbf55;
    let tops = match v % 3 {
        0 => [7, 1, 4],
        1 => [3, 6, 1],
        _ => [5, 2, 8],
    };
    let mut leaves = Vec::new();
    for (i, &x) in [7, 10, 13].iter().enumerate() {
        let top = tops[i];
        c.vline(x, top, 23 - top, light(stalk, 0.3));
        c.vline(x + 1, top, 23 - top, dark(stalk, 0.15));
        c.hline(x, top, 2, 0xd6c27a);
        let mut y = 21 - (i as i32 * 2) % 4;
        while y > top + 2 {
            c.hline(x, y, 2, dark(stalk, 0.35));
            c.set(x, y - 1, light(stalk, 0.55));
            y -= 5;
        }
        let (fx, fy) = (x as f64 + 1.0, top as f64 + 3.0);
        let reach = if i == 1 { 1.0 } else { 0.0 };
        leaves.push(((fx - 1.0, fy + 1.0), (fx - 6.5 - reach, fy - 2.5)));
        leaves.push(((fx, fy + 4.0), (fx + 6.0 + reach, fy + 0.5)));
    }
    for (from, to) in leaves {
        let ctrl = ((from.0 + to.0) / 2.0, to.1 - 1.5);
        blade(
            c,
            &Blade {
                from,
                ctrl,
                to,
                width: 1.7,
            },
            Some(g[0]),
            |t| (4.0 * t * (1.0 - t)).powf(0.5),
            |px| Some(leaf_tone(px, &g, 0)),
        );
    }
    if v.is_multiple_of(2) {
        let ribbon = 0xe0b54f;
        c.hline(7, 16, 8, ribbon);
        c.stamp(
            9,
            15,
            &["r..r", "rkkr", "r..r"],
            &[(b'r', light(ribbon, 0.2)), (b'k', dark(ribbon, 0.3))],
        );
    }
}

// ---------------------------------------------------------------------------
// Clutter
// ---------------------------------------------------------------------------

pub(in crate::ui::agent_office) fn clutter(c: &mut Canvas, p: &Pal, clutter: Clutter, v: usize) {
    match clutter {
        Clutter::FloorLamp => floor_lamp(c, p, v),
        Clutter::Crates => crates(c, p, v),
        Clutter::WateringCan => watering_can(c, p, v),
        Clutter::SmallPot => small_pots(c, p, v),
        Clutter::Bin => bin(c, p, v),
        Clutter::BookPile => book_pile(c, p, v),
        Clutter::Stool => stool(c, p, v),
        Clutter::LavaLamp => lava_lamp(c, p, v),
        Clutter::BeanBag => bean_bag(c, p, v),
        Clutter::Cushion => cushions(c, p, v),
        Clutter::PaperLantern => paper_lantern(c, p, v),
    }
}

/// Glowing drum shade, slightly narrower at the top.
fn drum(c: &mut Canvas, shade: u32, x: i32, y: i32, w: i32, h: i32) {
    for row in 0..h {
        let inset = i32::from(row < h / 2);
        let (l, r) = (x + inset, x + w - 1 - inset);
        c.hline(l, y + row, r - l + 1, shade);
        for px in (l + 3..r - 1).step_by(3) {
            c.set(px, y + row, dark(shade, 0.07));
        }
        c.set(l, y + row, light(shade, 0.35));
        c.set(r - 1, y + row, dark(shade, 0.1));
        c.set(r, y + row, dark(shade, 0.24));
    }
    c.hline(x + 2, y, w - 4, light(shade, 0.5));
    c.hline(x, y + h - 1, w, GLOW);
}

fn floor_lamp(c: &mut Canvas, p: &Pal, v: usize) {
    let shade = mix(0xf5dcae, p.accent, 0.16);
    match v % 3 {
        0 => {
            let foot = 0x3f3b4a;
            c.oval(4, 19, 10, 4, foot);
            c.hline(6, 19, 6, light(foot, 0.35));
            c.hline(5, 22, 8, dark(foot, 0.3));
            c.vline(8, 9, 11, BRASS);
            c.vline(9, 9, 11, dark(BRASS, 0.35));
            drum(c, shade, 2, 1, 14, 9);
            c.vline(9, 10, 1, GLOW);
            c.vline(13, 10, 3, METAL);
            c.set(13, 13, BRASS);
        }
        1 => {
            // Arc lamp: weighted base on the right, dome hanging over the left.
            let stone = 0xd9d4ca;
            c.round(10, 18, 7, 5, 1, stone);
            c.hline(11, 18, 5, light(stone, 0.4));
            c.vline(16, 19, 3, dark(stone, 0.25));
            c.hline(11, 22, 5, dark(stone, 0.35));
            let rod = 0x45414f;
            c.vline(13, 5, 13, rod);
            c.vline(14, 6, 12, dark(rod, 0.3));
            for (x, y) in [(13, 4), (12, 3), (11, 2), (10, 2), (9, 2), (8, 2), (7, 3)] {
                c.set(x, y, rod);
            }
            let dome = p.accent2;
            c.round(2, 3, 10, 6, 2, dome);
            c.hline(4, 3, 6, light(dome, 0.45));
            c.hline(3, 4, 2, light(dome, 0.3));
            c.vline(11, 5, 3, dark(dome, 0.3));
            c.hline(2, 8, 10, dark(dome, 0.25));
            c.hline(4, 9, 6, GLOW);
            c.hline(5, 10, 4, light(GLOW, 0.4));
        }
        _ => {
            // Tripod with wooden legs.
            let wood = p.wood;
            c.line((9, 10), (9, 21), dark(wood, 0.4));
            c.line((8, 10), (3, 22), wood);
            c.line((7, 10), (2, 22), light(wood, 0.25));
            c.line((10, 10), (14, 22), dark(wood, 0.2));
            c.line((11, 10), (15, 22), dark(wood, 0.35));
            c.rect(7, 9, 4, 2, BRASS);
            c.set(7, 9, light(BRASS, 0.4));
            drum(c, shade, 2, 1, 14, 8);
        }
    }
}

/// Top face of a crate: a lit lid, or the back rim and dark inside when open.
fn crate_top(c: &mut Canvas, wood: u32, x: i32, y: i32, w: i32, open: bool) {
    let r = x + w - 1;
    if open {
        c.hline(x, y, w, light(wood, 0.3));
        c.hline(x, y + 1, w, dark(wood, 0.6));
        c.set(x, y + 1, light(wood, 0.2));
        c.set(r, y + 1, dark(wood, 0.2));
    } else {
        c.hline(x, y, w, light(wood, 0.42));
        c.hline(x, y + 1, w, light(wood, 0.28));
        for sx in (x + 5..r).step_by(5) {
            c.vline(sx, y, 2, light(wood, 0.12));
        }
    }
}

/// Slatted front of a crate whose top face starts at `y`.
fn crate_front(c: &mut Canvas, wood: u32, x: i32, y: i32, w: i32, h: i32) {
    let r = x + w - 1;
    let fy = y + 2;
    c.rect(x, fy, w, h - 2, wood);
    for gy in (fy + 3..y + h - 1).step_by(3) {
        c.hline(x + 2, gy, w - 4, dark(wood, 0.42));
    }
    c.rect(x, fy, 2, h - 2, light(wood, 0.12));
    c.vline(x, fy, h - 2, light(wood, 0.25));
    c.rect(r - 1, fy, 2, h - 2, dark(wood, 0.15));
    c.vline(r, fy, h - 2, dark(wood, 0.32));
    c.hline(x, fy, w, light(wood, 0.2));
    c.hline(x, y + h - 1, w, dark(wood, 0.38));
    for (nx, ny) in [
        (x + 1, fy + 1),
        (r - 1, fy + 1),
        (x + 1, y + h - 2),
        (r - 1, y + h - 2),
    ] {
        c.set(nx, ny, dark(wood, 0.5));
    }
}

fn crates(c: &mut Canvas, p: &Pal, v: usize) {
    let wood = mix(0xc8965c, p.wood, 0.35);
    crate_top(c, wood, 1, 11, 16, false);
    crate_front(c, wood, 1, 11, 16, 12);
    // Shipping label on the lower crate.
    c.rect(9, 16, 5, 4, PAPER);
    c.hline(10, 17, 3, 0xb9ae99);
    c.hline(10, 18, 2, p.accent);
    // Contents of the open crate poke above its rim.
    let upper = light(wood, 0.06);
    crate_top(c, upper, 2, 5, 11, true);
    match v % 4 {
        0 | 2 => {
            for (x, top, color) in [(4, 1, PAPER), (7, 3, p.accent), (10, 2, p.accent2)] {
                c.rect(x, top, 2, 6, color);
                c.vline(x + 1, top + 1, 5, dark(color, 0.2));
                c.hline(x, top, 2, light(color, 0.4));
            }
            if v % 4 == 2 {
                c.line((12, 2), (9, 6), 0x8a5a3c);
                c.rect(12, 1, 2, 2, p.accent2);
            }
        }
        1 => {
            // Tools: a hammer, a wrench and a screwdriver.
            c.vline(5, 2, 5, 0xb07a4a);
            c.rect(3, 1, 5, 2, METAL);
            c.hline(3, 1, 5, light(METAL, 0.4));
            c.vline(8, 3, 4, METAL);
            c.rect(7, 2, 3, 2, METAL);
            c.set(8, 2, CLEAR);
            c.vline(11, 1, 3, METAL);
            c.rect(10, 4, 3, 3, p.accent);
            c.set(10, 4, light(p.accent, 0.35));
        }
        _ => {
            // A cat napping in the crate.
            sleeping_cat(c, 2, 1, FUR);
        }
    }
    crate_front(c, upper, 2, 5, 11, 7);
    // Something small on the lower lid beside the open crate.
    if v.is_multiple_of(2) {
        // Paint tin with a drip down the side.
        c.rect(13, 7, 3, 4, METAL);
        c.hline(13, 7, 3, light(METAL, 0.45));
        c.rect(13, 8, 3, 2, p.accent);
        c.set(13, 8, light(p.accent, 0.3));
        c.set(15, 8, dark(p.accent, 0.25));
        c.set(15, 9, dark(p.accent, 0.25));
        c.set(15, 10, dark(METAL, 0.3));
        c.set(14, 7, p.accent);
    } else {
        c.rect(13, 8, 3, 3, p.accent2);
        c.hline(13, 8, 3, light(p.accent2, 0.35));
        c.set(15, 9, dark(p.accent2, 0.25));
        c.set(15, 10, dark(p.accent2, 0.25));
    }
}

/// Small pot: back lip, soil, front lip and a tapered body down to `bottom`.
fn minipot(c: &mut Canvas, x: i32, y: i32, w: i32, bottom: i32, base: u32) {
    let r = x + w - 1;
    for yy in y + 3..=bottom {
        let inset = 1 + i32::from(yy > (y + 3 + bottom) / 2 + 1 && w > 5);
        let (l, rr) = (x + inset, r - inset);
        c.hline(l, yy, rr - l + 1, base);
        c.set(l, yy, light(base, 0.2));
        c.set(rr, yy, dark(base, 0.3));
        if yy == y + 3 {
            c.hline(l, yy, rr - l + 1, dark(base, 0.3));
        }
        if yy == bottom {
            c.hline(l, yy, rr - l + 1, dark(base, 0.38));
        }
    }
    c.hline(x + 1, y, w - 2, light(base, 0.4));
    c.hline(x + 1, y + 1, w - 2, SOIL);
    c.set(x, y + 1, light(base, 0.3));
    c.set(r, y + 1, dark(base, 0.2));
    c.hline(x, y + 2, w, light(base, 0.18));
    c.set(r, y + 2, dark(base, 0.25));
}

fn small_pots(c: &mut Canvas, p: &Pal, v: usize) {
    let flip = !v.is_multiple_of(2);
    let at = |x: i32, w: i32| if flip { 18 - x - w } else { x };
    let g = greens(0x58a052);
    // Tall back pot with a coin-leaf plant.
    let back = if v % 3 == 1 { PORCELAIN } else { p.accent2 };
    minipot(c, at(7, 7), 9, 7, 18, back);
    let stem_x = if flip { 7 } else { 10 };
    for (lx, ly) in [(5, 2), (9, 1), (13, 3), (3, 6), (12, 7), (7, 5)] {
        let lx = if flip { 14 - lx } else { lx };
        c.line((stem_x, 10), (lx + 1, ly + 2), dark(g[1], 0.1));
    }
    for (lx, ly) in [(5, 2), (9, 1), (13, 3), (3, 6), (12, 7), (7, 5)] {
        let lx = if flip { 14 - lx } else { lx };
        c.round(lx, ly, 4, 3, 1, g[2]);
        c.hline(lx + 1, ly, 2, g[3]);
        c.set(lx, ly + 1, g[3]);
        c.set(lx + 1, ly + 1, g[4]);
        c.hline(lx + 1, ly + 2, 2, g[1]);
        c.set(lx + 3, ly + 1, g[1]);
    }
    // Front pot with a rosette succulent.
    let fx = at(1, 7);
    minipot(c, fx, 16, 7, 22, TERRACOTTA);
    let sage = 0x8fb8a0;
    c.stamp(
        fx,
        12,
        &["..p.p..", ".pcdcp.", "pcdhdcp", ".bcdcb.", "..bbb.."],
        &[
            (b'p', 0xd88a9a),
            (b'b', dark(sage, 0.25)),
            (b'c', sage),
            (b'd', light(sage, 0.25)),
            (b'h', light(sage, 0.5)),
        ],
    );
    // Little round cactus with a bloom.
    let sx = at(11, 6);
    let small = if v % 3 == 2 { p.accent } else { PORCELAIN };
    minipot(c, sx, 17, 6, 22, small);
    let cg = greens(0x4f9a5c);
    c.round(sx + 1, 13, 4, 5, 1, cg[2]);
    c.vline(sx + 1, 14, 3, cg[3]);
    c.vline(sx + 2, 13, 4, cg[4]);
    c.vline(sx + 4, 14, 3, cg[1]);
    c.hline(sx + 2, 12, 2, PINK);
}

fn watering_can(c: &mut Canvas, p: &Pal, v: usize) {
    let can = match v % 3 {
        0 => p.accent2,
        1 => 0xa7b2c0,
        _ => p.accent,
    };
    // Little wooden step stool.
    let wood = p.wood;
    c.round(1, 13, 11, 3, 1, wood);
    c.hline(2, 13, 9, light(wood, 0.35));
    c.hline(1, 15, 11, dark(wood, 0.25));
    c.rect(2, 16, 2, 7, dark(wood, 0.15));
    c.rect(9, 16, 2, 7, dark(wood, 0.3));
    c.hline(4, 19, 5, dark(wood, 0.4));
    // Can body, spout and handle.
    c.line((9, 11), (14, 4), dark(can, 0.2));
    c.line((9, 10), (13, 4), can);
    c.rect(13, 2, 3, 3, light(can, 0.1));
    c.set(15, 3, dark(can, 0.2));
    c.set(13, 2, light(can, 0.4));
    c.round(2, 6, 8, 7, 1, can);
    c.hline(3, 6, 6, light(can, 0.4));
    c.vline(2, 7, 4, light(can, 0.2));
    c.vline(9, 7, 5, dark(can, 0.3));
    c.hline(3, 12, 6, dark(can, 0.3));
    c.hline(2, 9, 8, dark(can, 0.12));
    c.stamp(
        3,
        2,
        &[".hhh.", "h...h", "h...h", "....."],
        &[(b'h', dark(can, 0.35))],
    );
    // Seedling in a small pot on the floor.
    let pot_x = 11;
    minipot(c, pot_x, 17, 6, 22, TERRACOTTA);
    let g = greens(0x6aa84f);
    c.vline(14, 13, 5, g[1]);
    c.stamp(
        pot_x,
        11,
        &["ll....", "lmm.d.", ".mm.dd", "...dd."],
        &[(b'l', g[4]), (b'm', g[3]), (b'd', g[2])],
    );
}

fn bin(c: &mut Canvas, p: &Pal, v: usize) {
    let solid = !v.is_multiple_of(2);
    let body = if solid { p.accent2 } else { 0x5a6070 };
    // Back rim and the dark inside, then paper sitting in the opening.
    c.hline(4, 6, 10, light(body, 0.35));
    c.hline(3, 7, 12, dark(body, 0.55));
    c.hline(3, 8, 12, dark(body, 0.55));
    c.set(3, 7, light(body, 0.2));
    c.rect(10, 1, 3, 6, light(PAPER, 0.2));
    c.vline(12, 1, 6, 0xc9c2b2);
    c.hline(10, 2, 2, 0xb9b0a0);
    c.hline(10, 4, 2, 0xb9b0a0);
    for (x, y) in [(4, 3), (8, 4)] {
        c.round(x, y, 5, 5, 1, PAPER);
        c.hline(x + 1, y, 3, 0xffffff);
        c.set(x + 3, y + 2, 0xc9c2b2);
        c.set(x + 1, y + 3, 0xc9c2b2);
        c.vline(x + 4, y + 1, 3, 0xd9d2c2);
    }
    c.hline(3, 9, 12, light(body, 0.4));
    c.set(14, 9, light(body, 0.1));
    for y in 10..23 {
        let inset = i32::from(y > 15);
        let (l, r) = (3 + inset, 14 - inset);
        for x in l..=r {
            let mut color = body;
            if !solid && x % 2 == 0 && y != 12 && y != 18 {
                color = dark(body, 0.35);
            }
            if x == l {
                color = light(color, 0.25);
            } else if x >= r - 1 {
                color = dark(color, if x == r { 0.35 } else { 0.15 });
            }
            c.set(x, y, color);
        }
    }
    if solid {
        c.vline(5, 11, 9, light(body, 0.3));
        c.rect(8, 13, 4, 3, PAPER);
        c.hline(9, 14, 2, 0x5fa05a);
    } else {
        c.hline(3, 12, 12, light(body, 0.2));
        c.hline(4, 18, 10, light(body, 0.1));
    }
    c.hline(4, 22, 10, dark(body, 0.45));
    // A ball that missed.
    let mut ball = Canvas::new(5, 4);
    ball.round(0, 0, 5, 4, 1, PAPER);
    soft(c, &ball, PAPER, 12, 19);
    c.set(14, 20, 0xc9c2b2);
}

fn book(c: &mut Canvas, x: i32, y: i32, w: i32, color: u32, pages: bool) {
    let r = x + w - 1;
    c.hline(x, y, w, light(color, 0.3));
    c.hline(x, y + 1, w, color);
    c.hline(x, y + 2, w, dark(color, 0.3));
    if pages {
        c.hline(x, y + 1, w - 1, PAPER);
        c.set(x, y + 1, color);
        for px in (x + 3..r).step_by(4) {
            c.set(px, y + 1, 0xd9d0bc);
        }
    } else {
        c.hline(x + 2, y + 1, 2, 0xe8c56a);
        c.hline(r - 3, y + 1, 2, 0xe8c56a);
    }
    c.set(r, y + 1, dark(color, 0.2));
}

fn book_pile(c: &mut Canvas, p: &Pal, v: usize) {
    let palette = [
        p.accent2,
        p.fabric,
        p.accent,
        0x3f6b5a,
        0xb5473f,
        mix(p.wood, INK, 0.3),
    ];
    let stack = [
        (1, 16, false),
        (2, 13, true),
        (1, 14, false),
        (3, 12, false),
        (2, 13, true),
    ];
    let mut y = 20;
    for (i, &(x, w, pages)) in stack.iter().enumerate() {
        let shift = i32::from((v + i).is_multiple_of(3));
        book(
            c,
            x + shift,
            y,
            w - shift,
            palette[(i + v) % palette.len()],
            pages,
        );
        y -= 3;
    }
    // Bookmark ribbon.
    c.vline(6, 15, 3, 0xe2506f);
    // Something on top of the pile.
    match v % 3 {
        0 => {
            mug(c, 6, 4, if p.t.dark { PAPER } else { p.accent });
            c.set(7, 2, 0xe8e2d6);
            c.set(8, 1, 0xe8e2d6);
        }
        1 => {
            minipot(c, 5, 4, 6, 7, TERRACOTTA);
            let g = greens(0x5a9e45);
            c.stamp(
                4,
                1,
                &[".m..l.", "mmdlll", ".dd.l."],
                &[(b'l', g[3]), (b'm', g[2]), (b'd', g[1])],
            );
        }
        _ => {
            // Reading glasses on a sticky note.
            c.rect(4, 6, 5, 2, 0xf6d55c);
            c.stamp(
                4,
                3,
                &["kk.kk", "ggkgg", "kk.kk"],
                &[(b'k', INK), (b'g', 0xbfe4f2)],
            );
        }
    }
}

fn stool(c: &mut Canvas, p: &Pal, v: usize) {
    let metal = !v.is_multiple_of(2);
    let frame = if metal { 0x7a8192 } else { p.wood };
    let cushion = if v % 3 == 2 { p.accent2 } else { p.accent };
    // Back leg, then the splayed front legs and a footrest.
    c.vline(8, 12, 9, dark(frame, 0.45));
    c.vline(9, 12, 9, dark(frame, 0.55));
    c.line((4, 12), (2, 22), light(frame, 0.15));
    c.line((5, 12), (3, 22), dark(frame, 0.1));
    c.line((12, 12), (14, 22), dark(frame, 0.2));
    c.line((13, 12), (15, 22), dark(frame, 0.38));
    c.hline(
        4,
        18,
        10,
        if metal {
            light(frame, 0.2)
        } else {
            dark(frame, 0.25)
        },
    );
    // Seat disc and cushion.
    c.oval(2, 8, 14, 5, dark(frame, 0.28));
    c.oval(2, 6, 14, 5, light(frame, 0.18));
    let mut pad = Canvas::new(12, 6);
    pad.oval(0, 0, 12, 6, cushion);
    soft(c, &pad, cushion, 3, 3);
    c.hline(8, 5, 2, dark(cushion, 0.3));
    c.set(8, 4, light(cushion, 0.2));
}

fn lava_lamp(c: &mut Canvas, p: &Pal, v: usize) {
    // Low side table.
    let wood = p.wood;
    c.rect(1, 17, 16, 3, wood);
    c.hline(1, 17, 16, light(wood, 0.35));
    c.hline(1, 19, 16, dark(wood, 0.3));
    c.vline(16, 17, 3, dark(wood, 0.2));
    c.rect(2, 20, 2, 3, dark(wood, 0.25));
    c.rect(14, 20, 2, 3, dark(wood, 0.4));
    // Lamp: chrome base, glass with wax and a cap. The renderer animates a
    // wax blob at x 8..9, y 8..12, inside the glass.
    let chrome = 0x9aa2b5;
    for (y, l, r) in [(13, 6, 11), (14, 5, 12), (15, 4, 13), (16, 4, 13)] {
        c.hline(l, y, r - l + 1, chrome);
        c.set(l, y, light(chrome, 0.45));
        c.set(l + 1, y, light(chrome, 0.2));
        c.set(r - 1, y, dark(chrome, 0.15));
        c.set(r, y, dark(chrome, 0.35));
    }
    c.hline(5, 16, 8, dark(chrome, 0.28));
    let liquid = mix(p.accent2, INK, 0.5);
    let wax = light(p.accent, 0.3);
    let glass = [
        (3, 7, 10),
        (4, 7, 10),
        (5, 6, 11),
        (6, 6, 11),
        (7, 6, 11),
        (8, 5, 12),
        (9, 5, 12),
        (10, 5, 12),
        (11, 5, 12),
        (12, 5, 12),
    ];
    for (y, l, r) in glass {
        c.hline(l, y, r - l + 1, liquid);
        c.set(r, y, dark(liquid, 0.35));
    }
    c.round(8, 4, 3, 3, 1, wax);
    c.set(8, 4, wax);
    c.round(6, 6, 2, 2, 0, wax);
    c.round(6, 10, 6, 3, 1, wax);
    c.hline(6, 12, 6, wax);
    c.set(6, 10, light(wax, 0.3));
    c.hline(6, 12, 6, dark(wax, 0.12));
    for (y, l, _) in glass {
        c.set(l + 1, y, light(c.get(l + 1, y), 0.3));
    }
    c.hline(7, 2, 4, chrome);
    c.set(7, 2, light(chrome, 0.4));
    c.set(10, 2, dark(chrome, 0.3));
    c.hline(8, 1, 2, light(chrome, 0.35));
    // A tiny companion on the table.
    if v.is_multiple_of(2) {
        c.stamp(
            14,
            14,
            &["k.k", "kek", "kkk"],
            &[(b'k', PAPER), (b'e', INK)],
        );
    } else {
        c.rect(14, 14, 2, 3, p.accent);
        c.hline(14, 14, 2, METAL);
        c.set(15, 15, dark(p.accent, 0.3));
    }
}

fn bean_bag(c: &mut Canvas, p: &Pal, v: usize) {
    let f = if v.is_multiple_of(2) {
        p.accent
    } else {
        p.accent2
    };
    let pillow = if v.is_multiple_of(2) {
        p.accent2
    } else {
        p.accent
    };
    let mut bag = Canvas::new(18, 24);
    bag.oval(1, 10, 16, 13, f);
    bag.oval(3, 3, 12, 12, f);
    soft(c, &bag, f, 0, 0);
    // Seat dent, a seam and a fold near the floor.
    c.oval(5, 12, 9, 4, dark(f, 0.18));
    c.hline(6, 12, 7, dark(f, 0.3));
    c.hline(6, 16, 7, light(f, 0.18));
    c.line((4, 6), (3, 12), light(f, 0.22));
    c.line((13, 18), (15, 15), dark(f, 0.2));
    // Square pillow with pointed corners propped against the back.
    let mut pad = Canvas::new(9, 7);
    pad.stamp(
        0,
        0,
        &[
            "x.xxxxx.x",
            ".xxxxxxx.",
            "xxxxxxxxx",
            "xxxxxxxxx",
            "xxxxxxxxx",
            ".xxxxxxx.",
            "x.xxxxx.x",
        ],
        &[(b'x', pillow)],
    );
    soft(c, &pad, pillow, 5, 6);
    // Center tuft pulling the fabric in.
    c.set(9, 9, dark(pillow, 0.35));
    c.set(8, 8, light(pillow, 0.2));
}

fn sleeping_cat(c: &mut Canvas, x: i32, y: i32, fur: u32) {
    let mut cat = Canvas::new(12, 7);
    cat.stamp(
        0,
        0,
        &[
            ".x.x........",
            "xxxxx.xxxx..",
            "xxxxxxxxxxx.",
            "xxxxxxxxxxxx",
            "xxxxxxxxxxxx",
            ".xxxxxxxxxx.",
            "..xxxxxxxx..",
        ],
        &[(b'x', fur)],
    );
    soft(c, &cat, fur, x, y);
    let stripe = dark(fur, 0.28);
    c.set(x + 1, y, light(fur, 0.2));
    c.set(x + 3, y, fur);
    c.set(x + 1, y + 3, INK);
    c.set(x + 3, y + 3, INK);
    c.set(x + 2, y + 4, 0xf28a8f);
    c.vline(x + 7, y + 1, 2, stripe);
    c.vline(x + 9, y + 1, 2, stripe);
    c.hline(x + 4, y + 5, 6, stripe);
    c.set(x + 4, y + 5, light(fur, 0.35));
}

fn cushions(c: &mut Canvas, p: &Pal, v: usize) {
    let colors = [p.fabric, p.accent2, p.accent];
    // Three zabuton, bottom to top: lit top face, puffy front and a tufted center.
    for (i, y) in [17, 13, 9].into_iter().enumerate() {
        let color = colors[(i + v) % 3];
        let x = 1 + (i as i32 + v as i32) % 2;
        c.round(x, y, 15, 6, 1, color);
        c.hline(x + 1, y, 13, light(color, 0.35));
        c.hline(x, y + 1, 15, light(color, 0.2));
        c.hline(x + 1, y + 5, 13, dark(color, 0.32));
        c.vline(x + 14, y + 2, 3, dark(color, 0.25));
        c.vline(x, y + 2, 3, light(color, 0.08));
        c.set(x + 7, y + 3, dark(color, 0.28));
        c.set(x + 7, y + 1, dark(color, 0.12));
        // Corner ties in the next color.
        let tie = colors[(i + v + 1) % 3];
        c.set(x, y + 1, light(tie, 0.1));
        c.set(x + 14, y + 1, tie);
    }
    if v % 3 == 2 {
        sleeping_cat(c, 3, 3, FUR);
    } else {
        // Round zafu: pleated side band under a domed top with a center tuft.
        let zafu = if p.t.dark {
            light(p.accent2, 0.1)
        } else {
            dark(p.accent2, 0.15)
        };
        let mut band = Canvas::new(12, 8);
        band.round(0, 0, 12, 8, 2, zafu);
        soft(c, &band, dark(zafu, 0.12), 3, 2);
        for x in (5..14).step_by(2) {
            c.vline(x, 7, 2, dark(zafu, 0.4));
        }
        c.oval(4, 2, 10, 5, light(zafu, 0.12));
        c.hline(6, 2, 6, light(zafu, 0.35));
        c.hline(8, 4, 2, dark(zafu, 0.35));
        c.hline(8, 3, 2, light(zafu, 0.2));
    }
}

fn paper_lantern(c: &mut Canvas, p: &Pal, v: usize) {
    let frame = mix(p.wood, 0x3a2a20, 0.45);
    let paper = 0xfbe6b8;
    if v.is_multiple_of(2) {
        // Andon: a paper box on legs.
        c.rect(4, 18, 11, 2, frame);
        c.rect(4, 20, 2, 3, frame);
        c.rect(13, 20, 2, 3, dark(frame, 0.2));
        c.hline(6, 21, 7, dark(frame, 0.3));
        c.hline(4, 2, 11, light(frame, 0.3));
        c.hline(4, 3, 11, frame);
        c.hline(5, 3, 9, GLOW);
        c.rect(4, 4, 11, 14, frame);
        c.rect(5, 5, 9, 12, paper);
        c.oval(6, 7, 7, 8, light(paper, 0.45));
        c.vline(9, 5, 12, mix(frame, paper, 0.4));
        c.hline(5, 9, 9, mix(frame, paper, 0.5));
        c.hline(5, 13, 9, mix(frame, paper, 0.5));
        c.vline(13, 5, 12, dark(paper, 0.12));
        c.vline(4, 4, 14, light(frame, 0.2));
        c.hline(4, 18, 11, light(frame, 0.15));
    } else {
        // Round lantern hanging from a wooden stand.
        c.rect(1, 21, 7, 2, frame);
        c.hline(1, 21, 7, light(frame, 0.3));
        c.rect(3, 3, 2, 18, frame);
        c.vline(3, 3, 18, light(frame, 0.25));
        c.rect(3, 2, 10, 2, frame);
        c.hline(3, 2, 10, light(frame, 0.3));
        c.vline(11, 4, 2, INK);
        let shell = mix(p.accent, 0xffd9a0, 0.3);
        let mut ball = Canvas::new(10, 13);
        ball.oval(0, 0, 10, 13, shell);
        soft(c, &ball, shell, 7, 7);
        c.oval(9, 10, 5, 6, light(shell, 0.4));
        for y in (9..19).step_by(2) {
            for x in 7..17 {
                if c.get(x, y) != CLEAR {
                    c.set(x, y, dark(c.get(x, y), 0.16));
                }
            }
        }
        c.rect(9, 6, 6, 2, INK);
        c.hline(9, 6, 6, light(INK, 0.25));
        c.rect(9, 19, 6, 2, INK);
        c.stamp(10, 11, &[".k.", "k.k", ".k."], &[(b'k', dark(shell, 0.5))]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::agent_office::theme::{DESIGNS, LAYOUTS};

    const PLANTS: [Plant; 7] = [
        Plant::Monstera,
        Plant::Fern,
        Plant::Palm,
        Plant::Snake,
        Plant::Cactus,
        Plant::Fir,
        Plant::Bamboo,
    ];
    const CLUTTER: [Clutter; 11] = [
        Clutter::FloorLamp,
        Clutter::Crates,
        Clutter::WateringCan,
        Clutter::SmallPot,
        Clutter::Bin,
        Clutter::BookPile,
        Clutter::Stool,
        Clutter::LavaLamp,
        Clutter::BeanBag,
        Clutter::Cushion,
        Clutter::PaperLantern,
    ];

    fn check(c: &Canvas, what: &str) {
        for x in 0..c.w {
            assert_eq!(c.get(x, 0), CLEAR, "{what}: top margin at x {x}");
            assert_eq!(c.get(x, c.h - 1), CLEAR, "{what}: bottom margin at x {x}");
        }
        for y in 0..c.h {
            assert_eq!(c.get(0, y), CLEAR, "{what}: left margin at y {y}");
            assert_eq!(c.get(c.w - 1, y), CLEAR, "{what}: right margin at y {y}");
        }
        let footed = (0..c.w).any(|x| c.get(x, c.h - 2) != CLEAR);
        assert!(footed, "{what}: stands on the bottom row");
        let opaque: Vec<(i32, i32)> = (0..c.h)
            .flat_map(|y| (0..c.w).map(move |x| (x, y)))
            .filter(|&(x, y)| c.get(x, y) != CLEAR)
            .collect();
        let span = |f: fn(&(i32, i32)) -> i32| {
            opaque.iter().map(f).max().unwrap_or(0) - opaque.iter().map(f).min().unwrap_or(0) + 1
        };
        assert!(
            span(|p| p.0) * 5 >= c.w * 3,
            "{what}: fills its frame width"
        );
        assert!(
            span(|p| p.1) * 4 >= c.h * 3,
            "{what}: fills its frame height"
        );
    }

    #[test]
    fn small_props_keep_margins_and_fill_frames() {
        for design in (0..DESIGNS).step_by(LAYOUTS) {
            let p = Pal::new(design);
            for v in 0..8 {
                for kind in PLANTS {
                    let mut c = Canvas::new(22, 34);
                    plant(&mut c, &p, kind, v);
                    check(&c, &format!("{kind:?} design {design} v {v}"));
                }
                for kind in CLUTTER {
                    let mut c = Canvas::new(18, 24);
                    clutter(&mut c, &p, kind, v);
                    check(&c, &format!("{kind:?} design {design} v {v}"));
                    if kind == Clutter::LavaLamp {
                        for y in 8..13 {
                            for x in 8..10 {
                                assert_ne!(c.get(x, y), CLEAR, "lava glass at {x},{y}");
                            }
                        }
                    }
                }
            }
        }
    }
}
