// SPDX-License-Identifier: GPL-3.0-or-later
//! Pixel canvases authored in code and baked once into cached cairo surfaces.
//!
//! Colors are `0xRRGGBB`; `CLEAR` marks transparent pixels. Shading ramps shift
//! shadows toward violet and highlights toward warm cream instead of mixing
//! with grey, and outlines take a darkened tone of the shape they surround.

use gtk::cairo::{Context, Filter, Format, ImageSurface};
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    time::{Duration, Instant},
};

pub(super) const CLEAR: u32 = u32::MAX;
pub(super) const DENSITY: i32 = 2;
const SHADOW: u32 = 0x1d1530;
const LIGHT: u32 = 0xfff4dc;
const CACHE_BYTES_LIMIT: usize = 32 * 1024 * 1024;
const SCALED_BYTES_LIMIT: usize = 8 * 1024 * 1024;
const UNUSED_TTL: Duration = Duration::from_secs(2);

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
            px: vec![CLEAR; (w * h * DENSITY * DENSITY).max(0) as usize],
        }
    }

    pub fn get(&self, x: i32, y: i32) -> u32 {
        self.pixel(x * DENSITY, y * DENSITY)
    }

    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.w * DENSITY || y >= self.h * DENSITY {
            CLEAR
        } else {
            self.px[(y * self.w * DENSITY + x) as usize]
        }
    }

    pub fn set(&mut self, x: i32, y: i32, color: u32) {
        self.rect(x, y, 1, 1, color);
    }

    pub fn rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        self.fine_rect(x * DENSITY, y * DENSITY, w * DENSITY, h * DENSITY, color);
    }

    /// Native pixels (half an art unit) for seams, highlights and facial details.
    pub fn fine_rect(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let stride = self.w * DENSITY;
        let left = x.clamp(0, stride);
        let right = (x + w).clamp(left, stride);
        for yy in y.max(0)..(y + h).min(self.h * DENSITY) {
            self.px[(yy * stride + left) as usize..(yy * stride + right) as usize].fill(color);
        }
    }

    /// A rectangle with `r` pixels trimmed diagonally from each corner.
    pub fn round(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, color: u32) {
        let (x, y, w, h, r) = (
            x * DENSITY,
            y * DENSITY,
            w * DENSITY,
            h * DENSITY,
            r * DENSITY,
        );
        for yy in 0..h {
            let inset = (r - yy).max(r - (h - 1 - yy)).max(0);
            self.fine_rect(x + inset, y + yy, w - inset * 2, 1, color);
        }
    }

    /// Filled ellipse whose bounding box is `x, y, w, h`.
    pub fn oval(&mut self, x: i32, y: i32, w: i32, h: i32, color: u32) {
        let (x, y, w, h) = (x * DENSITY, y * DENSITY, w * DENSITY, h * DENSITY);
        let (rx, ry) = (w as f64 / 2.0, h as f64 / 2.0);
        for yy in 0..h {
            for xx in 0..w {
                let dx = (xx as f64 + 0.5 - rx) / rx;
                let dy = (yy as f64 + 0.5 - ry) / ry;
                if dx * dx + dy * dy <= 1.0 {
                    self.fine_rect(x + xx, y + yy, 1, 1, color);
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
        let (x0, y0, x1, y1) = (x0 * DENSITY, y0 * DENSITY, x1 * DENSITY, y1 * DENSITY);
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).max(1);
        for i in 0..=steps {
            let t = i as f64 / steps as f64;
            self.fine_rect(
                (x0 as f64 + (x1 - x0) as f64 * t).round() as i32,
                (y0 as f64 + (y1 - y0) as f64 * t).round() as i32,
                1,
                1,
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
        for yy in 0..other.h * DENSITY {
            for xx in 0..other.w * DENSITY {
                let color = other.pixel(xx, yy);
                if color != CLEAR {
                    self.fine_rect(x * DENSITY + xx, y * DENSITY + yy, 1, 1, color);
                }
            }
        }
    }

    pub fn flipped(&self) -> Self {
        let mut out = Self::new(self.w, self.h);
        for y in 0..self.h * DENSITY {
            for x in 0..self.w * DENSITY {
                out.fine_rect(self.w * DENSITY - 1 - x, y, 1, 1, self.pixel(x, y));
            }
        }
        out
    }

    /// Selective outline: transparent pixels next to the shape take a deep
    /// tone of the neighbor they border, which reads crisper than flat black.
    pub fn outline(&mut self, strength: f64) {
        let source = self.clone();
        for y in 0..self.h * DENSITY {
            for x in 0..self.w * DENSITY {
                if source.pixel(x, y) != CLEAR {
                    continue;
                }
                let neighbor = [(0, 1), (0, -1), (1, 0), (-1, 0)]
                    .into_iter()
                    .map(|(dx, dy)| source.pixel(x + dx, y + dy))
                    .find(|color| *color != CLEAR);
                if let Some(color) = neighbor {
                    self.fine_rect(x, y, 1, 1, dark(color, strength));
                }
            }
        }
    }

    pub fn bake(&self) -> ImageSurface {
        let (w, h) = (self.w * DENSITY, self.h * DENSITY);
        let mut surface =
            ImageSurface::create(Format::ARgb32, w.max(1), h.max(1)).expect("sprite surface");
        let stride = surface.stride() as usize;
        {
            let mut data = surface.data().expect("sprite data");
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let color = self.px[y * w as usize + x];
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
        surface.set_device_scale(DENSITY as f64, DENSITY as f64);
        surface
    }
}

struct CachedSprite {
    surface: ImageSurface,
    used: Instant,
}

impl CachedSprite {
    fn new(surface: &ImageSurface) -> Self {
        Self {
            surface: surface.clone(),
            used: Instant::now(),
        }
    }

    fn touch(&mut self) -> ImageSurface {
        self.used = Instant::now();
        self.surface.clone()
    }

    fn bytes(&self) -> usize {
        self.surface.stride() as usize * self.surface.height() as usize
    }
}

thread_local! {
    static USERS: Cell<usize> = const { Cell::new(0) };
    static LAST_PRUNE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Shared by drawing areas; closing one window must not evict another's sprites.
pub(super) struct CacheLease;

impl CacheLease {
    pub fn new() -> Self {
        USERS.set(USERS.get() + 1);
        Self
    }

    pub fn prune(&self) {
        let now = Instant::now();
        if LAST_PRUNE
            .get()
            .is_some_and(|last| now.duration_since(last) < Duration::from_secs(1))
        {
            return;
        }
        LAST_PRUNE.set(Some(now));
        prune_unused(now);
    }
}

impl Drop for CacheLease {
    fn drop(&mut self) {
        USERS.set(USERS.get() - 1);
        if USERS.get() == 0 {
            SCALED.with(|c| *c.borrow_mut() = HashMap::new());
            CACHE.with(|c| *c.borrow_mut() = HashMap::new());
            SCALED_BYTES.set(0);
            CACHE_BYTES.set(0);
            LAST_PRUNE.set(None);
        }
    }
}

fn prune_unused(now: Instant) {
    let sources: HashSet<_> = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|_, sprite| now.saturating_duration_since(sprite.used) < UNUSED_TTL);
        CACHE_BYTES.set(cache.values().map(CachedSprite::bytes).sum());
        cache
            .values()
            .map(|sprite| sprite.surface.to_raw_none() as usize)
            .collect()
    });
    SCALED.with(|cache| {
        let mut cache = cache.borrow_mut();
        // Address-keyed copies cannot survive eviction of their original surface.
        cache.retain(|(source, _), sprite| {
            sources.contains(source) && now.saturating_duration_since(sprite.used) < UNUSED_TTL
        });
        SCALED_BYTES.set(cache.values().map(CachedSprite::bytes).sum());
    });
}

thread_local! {
    static CACHE: RefCell<HashMap<u64, CachedSprite>> = RefCell::new(HashMap::new());
    static CACHE_BYTES: Cell<usize> = const { Cell::new(0) };
}

/// Returns the baked surface for `key`, building it on first use.
pub(super) fn cached(key: impl Hash, build: impl FnOnce() -> Canvas) -> ImageSurface {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let id = hasher.finish();
    if let Some(surface) =
        CACHE.with(|cache| cache.borrow_mut().get_mut(&id).map(CachedSprite::touch))
    {
        return surface;
    }
    let surface = build().bake();
    let bytes = surface.stride() as usize * surface.height() as usize;
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        // Bound pixel storage rather than sprite count as art density increases.
        if CACHE_BYTES.get() + bytes > CACHE_BYTES_LIMIT {
            cache.clear();
            CACHE_BYTES.set(0);
            // Enlarged copies are keyed by surface address and must not outlive it.
            SCALED.with(|enlarged| enlarged.borrow_mut().clear());
            SCALED_BYTES.set(0);
        }
        if bytes <= CACHE_BYTES_LIMIT {
            cache.insert(id, CachedSprite::new(&surface));
            CACHE_BYTES.set(CACHE_BYTES.get() + bytes);
        }
    });
    surface
}

/// Paints a baked sprite with its top-left corner at `x, y` in the current space.
pub(super) fn paint(cr: &Context, surface: &ImageSurface, x: f64, y: f64) {
    let _ = cr.set_source_surface(surface, x, y);
    cr.source().set_filter(Filter::Nearest);
    let _ = cr.paint();
}

fn whole(scale: f64) -> bool {
    scale >= 1.0 && (scale - scale.round()).abs() < 1e-6
}

/// `surface` enlarged `k` times, each art pixel becoming a `k` x `k` block.
pub(super) fn enlarged(surface: &ImageSurface, k: u32) -> ImageSurface {
    let (w, h) = (surface.width(), surface.height());
    let big = ImageSurface::create(Format::ARgb32, w * k as i32, h * k as i32)
        .expect("enlarged sprite surface");
    let (sx, sy) = surface.device_scale();
    big.set_device_scale(sx, sy);
    {
        let cr = Context::new(&big).expect("enlarged sprite context");
        cr.scale(k as f64, k as f64);
        paint(&cr, surface, 0.0, 0.0);
    }
    big
}

thread_local! {
    static SCALED: RefCell<HashMap<(usize, u64), CachedSprite>> = RefCell::new(HashMap::new());
    static SCALED_BYTES: Cell<usize> = const { Cell::new(0) };
}

/// Resamples once at the display density, preserving the logical art bounds.
pub(super) fn scaled(
    surface: &ImageSurface,
    scale: f64,
) -> Result<ImageSurface, gtk::cairo::Error> {
    let density = surface.device_scale().0;
    let output = ImageSurface::create(
        Format::ARgb32,
        (surface.width() as f64 / density * scale).ceil() as i32,
        (surface.height() as f64 / density * scale).ceil() as i32,
    )?;
    output.set_device_scale(scale, scale);
    let cr = Context::new(&output)?;
    let pixel_scale = scale / density;
    let k = pixel_scale.ceil().max(1.0) as u32;
    let source = if k > 1 {
        enlarged(surface, k)
    } else {
        surface.clone()
    };
    cr.scale(1.0 / k as f64, 1.0 / k as f64);
    cr.set_source_surface(&source, 0.0, 0.0)?;
    cr.source().set_filter(if whole(pixel_scale) {
        Filter::Nearest
    } else {
        Filter::Good
    });
    cr.paint()?;
    Ok(output)
}

/// Cached sprites only: temporary room surfaces must use `scaled` directly.
pub(super) fn paint_scaled(
    cr: &Context,
    surface: &ImageSurface,
    x: f64,
    y: f64,
    scale: f64,
    alpha: f64,
) {
    let ready = if whole(scale / surface.device_scale().0) {
        surface.clone()
    } else {
        let key = (surface.to_raw_none() as usize, scale.to_bits());
        SCALED.with(|cache| {
            let mut cache = cache.borrow_mut();
            if let Some(ready) = cache.get_mut(&key) {
                return ready.touch();
            }
            let ready = scaled(surface, scale).expect("scaled sprite surface");
            let bytes = ready.stride() as usize * ready.height() as usize;
            if SCALED_BYTES.get() + bytes > SCALED_BYTES_LIMIT {
                cache.clear();
                SCALED_BYTES.set(0);
            }
            if bytes <= SCALED_BYTES_LIMIT {
                cache.insert(key, CachedSprite::new(&ready));
                SCALED_BYTES.set(SCALED_BYTES.get() + bytes);
            }
            ready
        })
    };
    let _ = cr.save();
    let _ = cr.set_source_surface(&ready, x, y);
    cr.source().set_filter(Filter::Nearest);
    if alpha < 1.0 {
        let _ = cr.paint_with_alpha(alpha);
    } else {
        let _ = cr.paint();
    }
    let _ = cr.restore();
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
        assert_ne!(canvas.pixel(1, 6), CLEAR);
        assert!(luma(canvas.pixel(1, 6)) < luma(0x80a0c0));
        assert!(luma(light(0x80a0c0, 0.3)) > luma(0x80a0c0));
        let a = cached(("test", 1), || canvas.clone());
        let b = cached(("test", 1), || unreachable!("cached"));
        assert_eq!(a.to_raw_none(), b.to_raw_none());
        assert_eq!(canvas.flipped().flipped().px, canvas.px);
    }

    #[test]
    fn fine_detail_survives_copy_flip_and_scaled_paint() {
        let mut canvas = Canvas::new(4, 4);
        canvas.rect(0, 0, 4, 4, 0x123456);
        canvas.fine_rect(2, 2, 1, 1, 0xffffff);
        let mut copy = Canvas::new(4, 4);
        copy.blit(&canvas.flipped().flipped(), 0, 0);
        assert_eq!(copy.px, canvas.px);
        let source = copy.bake();
        assert_eq!((source.width(), source.height()), (8, 8));
        assert_eq!(source.device_scale(), (2.0, 2.0));
        for scale in [0.75, 1.0, 2.0, 3.0, 4.0] {
            let mut target = ImageSurface::create(Format::ARgb32, 32, 32).unwrap();
            {
                let cr = Context::new(&target).unwrap();
                cr.scale(scale, scale);
                paint_scaled(&cr, &source, 2.0, 2.0, scale, 1.0);
            }
            let stride = target.stride() as usize;
            let data = target.data().unwrap();
            let alpha = |x: usize, y: usize| {
                u32::from_ne_bytes(
                    data[y * stride + x * 4..y * stride + x * 4 + 4]
                        .try_into()
                        .unwrap(),
                ) >> 24
            };
            assert_eq!(alpha((4.0 * scale) as usize, (4.0 * scale) as usize), 255);
            assert_eq!(
                alpha((7.0 * scale).ceil() as usize, (4.0 * scale) as usize),
                0,
                "density must not change logical sprite bounds at scale {scale}"
            );
            if scale == 2.0 {
                assert_eq!(
                    &data[6 * stride + 6 * 4..6 * stride + 6 * 4 + 4],
                    &0xffff_ffffu32.to_ne_bytes()
                );
                assert_eq!(
                    &data[6 * stride + 7 * 4..6 * stride + 7 * 4 + 4],
                    &0xff12_3456u32.to_ne_bytes()
                );
            }
        }
    }

    #[test]
    fn cache_releases_unused_sprites_and_waits_for_last_view() {
        let first = CacheLease::new();
        let second = CacheLease::new();
        assert_eq!(
            CACHE_BYTES.get(),
            0,
            "opening an office must not preload art"
        );
        let old = cached("old", || Canvas::new(8, 8));
        let active = cached("active", || Canvas::new(8, 8));
        let target = ImageSurface::create(Format::ARgb32, 32, 32).unwrap();
        let cr = Context::new(&target).unwrap();
        for surface in [&old, &active] {
            paint_scaled(&cr, surface, 0.0, 0.0, 1.5, 1.0);
        }
        let now = Instant::now();
        CACHE.with(|cache| {
            for entry in cache.borrow_mut().values_mut() {
                if entry.surface.to_raw_none() == old.to_raw_none() {
                    entry.used = now - UNUSED_TTL;
                }
            }
        });
        prune_unused(now);
        assert_eq!(CACHE_BYTES.get(), 8 * 8 * 4 * 4);
        assert_eq!(
            SCALED.with(|cache| cache.borrow().len()),
            1,
            "scaled copies of an evicted original must also be released"
        );
        cached("active", || panic!("visible sprites must be reused"));
        drop(first);
        assert!(CACHE_BYTES.get() > 0, "another office is still open");
        drop(second);
        assert_eq!(CACHE_BYTES.get(), 0);
        assert_eq!(SCALED_BYTES.get(), 0);
        assert!(CACHE.with(|cache| cache.borrow().is_empty()));
        assert!(SCALED.with(|cache| cache.borrow().is_empty()));
    }

    #[test]
    fn obsolete_scales_expire_without_discarding_the_original() {
        let _view = CacheLease::new();
        let source = cached("resized", || Canvas::new(8, 8));
        let target = ImageSurface::create(Format::ARgb32, 32, 32).unwrap();
        let cr = Context::new(&target).unwrap();
        for scale in [0.5, 0.75, 1.5] {
            paint_scaled(&cr, &source, 0.0, 0.0, scale, 1.0);
        }
        let now = Instant::now();
        SCALED.with(|cache| {
            for ((_, scale), entry) in cache.borrow_mut().iter_mut() {
                if *scale != 1.5_f64.to_bits() {
                    entry.used = now - UNUSED_TTL;
                }
            }
        });
        prune_unused(now);
        assert_eq!(SCALED.with(|cache| cache.borrow().len()), 1);
        assert_eq!(SCALED_BYTES.get(), 12 * 12 * 4);
        cached("resized", || panic!("resizing must preserve the original"));
    }

    #[test]
    fn density_cache_stays_within_pixel_budget() {
        for key in 0..10 {
            let source = cached(("budget", key), || Canvas::new(512, 512));
            let target = ImageSurface::create(Format::ARgb32, 8, 8).unwrap();
            paint_scaled(&Context::new(&target).unwrap(), &source, 0.0, 0.0, 1.5, 1.0);
            assert!(CACHE_BYTES.get() <= CACHE_BYTES_LIMIT);
            assert!(SCALED_BYTES.get() <= SCALED_BYTES_LIMIT);
        }
        let mut rebuilt = false;
        cached(("budget", 0), || {
            rebuilt = true;
            Canvas::new(512, 512)
        });
        assert!(
            rebuilt,
            "old sprites are evicted when their pixel budget is exceeded"
        );
    }
}
