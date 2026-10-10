// SPDX-License-Identifier: GPL-3.0-or-later
//! Pixel canvases authored in code and baked once into cached cairo surfaces.
//!
//! Colors are `0xRRGGBB`; `CLEAR` marks transparent pixels. Shading ramps shift
//! shadows toward violet and highlights toward warm cream instead of mixing
//! with grey, and outlines take a darkened tone of the shape they surround.

use gtk::cairo::{Context, Filter, Format, ImageSurface};
use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
};

pub(super) const CLEAR: u32 = u32::MAX;
const SHADOW: u32 = 0x1d1530;
const LIGHT: u32 = 0xfff4dc;

pub(super) fn mix(a: u32, b: u32, t: f64) -> u32 {
    let channel = |shift: u32| {
        let x = ((a >> shift) & 255) as f64;
        let y = ((b >> shift) & 255) as f64;
        ((x + (y - x) * t).round().clamp(0.0, 255.0) as u32) << shift
    };
    channel(16) | channel(8) | channel(0)
}

/// Darker, cooler tone of `color`.
pub(super) fn dark(color: u32, amount: f64) -> u32 {
    mix(color, SHADOW, amount)
}

/// Lighter, warmer tone of `color`.
pub(super) fn light(color: u32, amount: f64) -> u32 {
    mix(color, LIGHT, amount)
}

pub(super) fn luma(color: u32) -> f64 {
    let c = |shift: u32| ((color >> shift) & 255) as f64 / 255.0;
    0.2126 * c(16) + 0.7152 * c(8) + 0.0722 * c(0)
}

#[derive(Clone)]
pub(super) struct Canvas {
    pub w: i32,
    pub h: i32,
    px: Vec<u32>,
}

impl Canvas {
    pub fn new(w: i32, h: i32) -> Self {
        Self {
            w,
            h,
            px: vec![CLEAR; (w * h).max(0) as usize],
        }
    }

    pub fn get(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            CLEAR
        } else {
            self.px[(y * self.w + x) as usize]
        }
    }

    pub fn set(&mut self, x: i32, y: i32, color: u32) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h {
            self.px[(y * self.w + x) as usize] = color;
        }
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        for yy in y..y + h {
            for xx in x..x + w {
                self.set(xx, yy, color);
            }
        }
    }

    /// A rectangle with `r` pixels trimmed diagonally from each corner.
    pub fn round(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, color: u32) {
        for yy in 0..h {
            let inset = (r - yy).max(r - (h - 1 - yy)).max(0);
            self.rect(x + inset, y + yy, w - inset * 2, 1, color);
        }
    }

    /// Filled ellipse whose bounding box is `x, y, w, h`.
    pub fn oval(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let (rx, ry) = (w as f64 / 2.0, h as f64 / 2.0);
        for yy in 0..h {
            for xx in 0..w {
                let dx = (xx as f64 + 0.5 - rx) / rx;
                let dy = (yy as f64 + 0.5 - ry) / ry;
                if dx * dx + dy * dy <= 1.0 {
                    self.set(x + xx, y + yy, color);
                }
            }
        }
    }

    pub fn hline(&mut self, x: i32, y: i32, w: i32, color: u32) {
        self.rect(x, y, w, 1, color);
    }

    pub fn vline(&mut self, x: i32, y: i32, h: i32, color: u32) {
        self.rect(x, y, 1, h, color);
    }

    pub fn line(&mut self, (x0, y0): (i32, i32), (x1, y1): (i32, i32), color: u32) {
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            self.set(
                (x0 as f64 + (x1 - x0) as f64 * t).round() as i32,
                (y0 as f64 + (y1 - y0) as f64 * t).round() as i32,
                color,
            );
        }
    }

    /// Paint `pattern` rows; `.` and space are skipped, other bytes index `colors`.
    pub fn stamp(&mut self, x: i32, y: i32, pattern: &[&str], colors: &[(u8, u32)]) {
        for (row, line) in pattern.iter().enumerate() {
            for (col, byte) in line.bytes().enumerate() {
                if let Some(&(_, color)) = colors.iter().find(|(key, _)| *key == byte) {
                    self.set(x + col as i32, y + row as i32, color);
                }
            }
        }
    }

    /// Copies the opaque pixels of `other` at `x, y`.
    pub fn blit(&mut self, other: &Canvas, x: i32, y: i32) {
        for yy in 0..other.h {
            for xx in 0..other.w {
                let color = other.get(xx, yy);
                if color != CLEAR {
                    self.set(x + xx, y + yy, color);
                }
            }
        }
    }

    pub fn flipped(&self) -> Self {
        let mut out = Self::new(self.w, self.h);
        for y in 0..self.h {
            for x in 0..self.w {
                out.set(self.w - 1 - x, y, self.get(x, y));
            }
        }
        out
    }

    /// Selective outline: transparent pixels next to the shape take a deep
    /// tone of the neighbor they border, which reads crisper than flat black.
    pub fn outline(&mut self, strength: f64) {
        let source = self.clone();
        for y in 0..self.h {
            for x in 0..self.w {
                if source.get(x, y) != CLEAR {
                    continue;
                }
                let neighbor = [(0, 1), (0, -1), (1, 0), (-1, 0)]
                    .into_iter()
                    .map(|(dx, dy)| source.get(x + dx, y + dy))
                    .find(|color| *color != CLEAR);
                if let Some(color) = neighbor {
                    self.set(x, y, dark(color, strength));
                }
            }
        }
    }

    pub fn bake(&self) -> ImageSurface {
        let mut surface = ImageSurface::create(Format::ARgb32, self.w.max(1), self.h.max(1))
            .expect("sprite surface");
        let stride = surface.stride() as usize;
        {
            let mut data = surface.data().expect("sprite data");
            for y in 0..self.h as usize {
                for x in 0..self.w as usize {
                    let color = self.px[y * self.w as usize + x];
                    let value = if color == CLEAR {
                        0
                    } else {
                        0xff00_0000 | color
                    };
                    data[y * stride + x * 4..y * stride + x * 4 + 4]
                        .copy_from_slice(&value.to_ne_bytes());
                }
            }
        }
        surface.mark_dirty();
        surface
    }
}

thread_local! {
    static CACHE: RefCell<HashMap<u64, ImageSurface>> = RefCell::new(HashMap::new());
}

/// Returns the baked surface for `key`, building it on first use.
pub(super) fn cached(key: impl Hash, build: impl FnOnce() -> Canvas) -> ImageSurface {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let id = hasher.finish();
    if let Some(surface) = CACHE.with(|cache| cache.borrow().get(&id).cloned()) {
        return surface;
    }
    let surface = build().bake();
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        // Designs cycle rarely; a full reset bounds memory without bookkeeping.
        if cache.len() > 6000 {
            cache.clear();
        }
        cache.insert(id, surface.clone());
    });
    surface
}

/// Paints a baked sprite with its top-left corner at `x, y` in the current space.
pub(super) fn paint(cr: &Context, surface: &ImageSurface, x: f64, y: f64) {
    let _ = cr.set_source_surface(surface, x, y);
    cr.source().set_filter(Filter::Nearest);
    let _ = cr.paint();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outline_shading_and_cache_are_stable() {
        let mut canvas = Canvas::new(8, 8);
        canvas.round(1, 1, 6, 6, 1, 0x80a0c0);
        assert_eq!(canvas.get(1, 1), CLEAR, "rounded corner is trimmed");
        canvas.outline(0.6);
        assert_ne!(canvas.get(0, 3), CLEAR);
        assert!(luma(canvas.get(0, 3)) < luma(0x80a0c0));
        assert!(luma(light(0x80a0c0, 0.3)) > luma(0x80a0c0));
        let a = cached(("test", 1), || canvas.clone());
        let b = cached(("test", 1), || unreachable!("cached"));
        assert_eq!(a.to_raw_none(), b.to_raw_none());
        assert_eq!(canvas.flipped().flipped().px, canvas.px);
    }
}
