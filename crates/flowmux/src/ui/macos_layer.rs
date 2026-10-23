// SPDX-License-Identifier: GPL-3.0-or-later
//! Keep GTK's macOS window layer opaque while it re-tiles.
//!
//! GDK's macOS backend draws a toplevel through `GdkMacosLayer`, a `CALayer`
//! split into tiles that each show one rectangle of the frame's `IOSurface`.
//! `swapBuffer:withDamage:` only refreshes the tiles the frame damaged. When
//! the surface's opaque region changes, the layer rebuilds its tiles first
//! (`gdk/macos/GdkMacosLayer.c`, `_applyLayout:`), and the rebuilt tiles that
//! lie outside that frame's damage are left with no contents at all. The
//! window is non-opaque, so those tiles show whatever is behind the window.
//!
//! GSK derives the opaque region from the render tree each frame. While an
//! in-window `AdwDialog` (or toast) animates over a maximized window, that
//! rectangle picks up fractional edges and shrinks by a pixel on alternating
//! sides, so the region changes on frames whose damage covers only the dialog.
//! Every pane then flashes the desktop for a frame.
//!
//! Wrap `swapBuffer:withDamage:` and, whenever the tile set changed or any
//! tile ended up without contents, replay the swap with full damage. The
//! current buffer is complete (GDK repaints the buffer-age region), so
//! refreshing every tile from it is always correct.

use std::ffi::c_void;
use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
use objc2::{msg_send, sel};
use objc2_foundation::NSRect;

type SwapBufferImp = unsafe extern "C-unwind" fn(
    &AnyObject,
    Sel,
    *mut c_void,
    *const gtk::cairo::ffi::cairo_region_t,
);

static ORIGINAL_SWAP_BUFFER: OnceLock<SwapBufferImp> = OnceLock::new();

/// Install the tile refresh override. Idempotent; safe to call before GTK is
/// initialised (the class is registered when libgtk loads).
pub fn install_tile_refresh() {
    if ORIGINAL_SWAP_BUFFER.get().is_some() {
        return;
    }
    let Some(class) = AnyClass::get(c"GdkMacosLayer") else {
        tracing::warn!("GdkMacosLayer not found; leaving window layer tiling as is");
        return;
    };
    let sel = sel!(swapBuffer:withDamage:);
    let Some(method) = class.instance_method(sel) else {
        tracing::warn!("GdkMacosLayer lacks swapBuffer:withDamage:; not patching");
        return;
    };
    // SAFETY: the method's real signature is
    // `void (id, SEL, GdkMacosBuffer *, const cairo_region_t *)`;
    // `SwapBufferImp` matches it, and the replacement forwards every call to
    // the original with the same arguments.
    unsafe {
        let original = std::mem::transmute::<Imp, SwapBufferImp>(method.implementation());
        if ORIGINAL_SWAP_BUFFER.set(original).is_err() {
            return;
        }
        let replacement: SwapBufferImp = swap_buffer_refreshing_tiles;
        method.set_implementation(std::mem::transmute::<SwapBufferImp, Imp>(replacement));
    }
    tracing::debug!("installed tile-refreshing swapBuffer:withDamage: on GdkMacosLayer");
}

unsafe extern "C-unwind" fn swap_buffer_refreshing_tiles(
    layer: &AnyObject,
    sel: Sel,
    buffer: *mut c_void,
    damage: *const gtk::cairo::ffi::cairo_region_t,
) {
    let Some(original) = ORIGINAL_SWAP_BUFFER.get() else {
        return;
    };
    let before = tile_signature(layer);
    original(layer, sel, buffer, damage);
    if tile_signature(layer) == before && !any_tile_lacks_contents(layer) {
        return;
    }
    // Damage is compared against device-pixel tile rectangles; cover them all.
    let mut everything = gtk::cairo::ffi::cairo_rectangle_int_t {
        x: 0,
        y: 0,
        width: 1 << 24,
        height: 1 << 24,
    };
    let full = gtk::cairo::ffi::cairo_region_create_rectangle(&mut everything);
    original(layer, sel, buffer, full);
    gtk::cairo::ffi::cairo_region_destroy(full);
}

fn tile_signature(layer: &AnyObject) -> (usize, Option<(NSRect, NSRect)>) {
    unsafe {
        let tiles: *mut AnyObject = msg_send![layer, sublayers];
        if tiles.is_null() {
            return (0, None);
        }
        let count: usize = msg_send![tiles, count];
        if count == 0 {
            return (0, None);
        }
        let first: *mut AnyObject = msg_send![tiles, objectAtIndex: 0usize];
        let last: *mut AnyObject = msg_send![tiles, objectAtIndex: count - 1];
        let first: NSRect = msg_send![first, frame];
        let last: NSRect = msg_send![last, frame];
        (count, Some((first, last)))
    }
}

fn any_tile_lacks_contents(layer: &AnyObject) -> bool {
    unsafe {
        let tiles: *mut AnyObject = msg_send![layer, sublayers];
        if tiles.is_null() {
            return false;
        }
        let count: usize = msg_send![tiles, count];
        (0..count).any(|index| {
            let tile: *mut AnyObject = msg_send![tiles, objectAtIndex: index];
            let contents: *mut AnyObject = msg_send![tile, contents];
            contents.is_null()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_replaces_swap_buffer_once() {
        let class = AnyClass::get(c"GdkMacosLayer").expect("GDK macOS backend linked");
        let method = class
            .instance_method(sel!(swapBuffer:withDamage:))
            .expect("swapBuffer:withDamage: exists");
        install_tile_refresh();
        let patched = method.implementation() as *const ();
        assert_eq!(patched, swap_buffer_refreshing_tiles as *const ());
        let original = *ORIGINAL_SWAP_BUFFER.get().expect("original kept") as *const ();
        assert_ne!(original, patched);
        install_tile_refresh();
        assert_eq!(method.implementation() as *const (), patched);
        assert_eq!(*ORIGINAL_SWAP_BUFFER.get().unwrap() as *const (), original);
    }
}
