// SPDX-License-Identifier: GPL-3.0-or-later
//! Position a context-menu Popover at the click point, shifting its anchor
//! up or left when the measured size would overflow the window.
//!
//! The pointing rectangle encodes horizontal placement. `set_position(Bottom)`
//! requests placement below it, and `set_halign(Fill)` resets prior alignment.

use gtk::graphene;
use gtk::prelude::*;

/// Detach a one-shot menu after GTK finishes hiding it. Clear its focus
/// first: GTK 4.14 defers focus movement until after paint, and hiding then
/// unparenting a focused widget can overwrite (and leak) that pending reference.
pub fn unparent_after_close(popover: &gtk::Popover) {
    if let Some(root) = popover.root() {
        if root.focus().is_some_and(|focus| focus.is_ancestor(popover)) {
            root.set_focus(gtk::Widget::NONE);
        }
    }
    let popover = popover.clone();
    gtk::glib::idle_add_local_once(move || {
        popover.unparent();
    });
}

pub fn anchor_at_click(popover: &gtk::Popover, parent: &impl IsA<gtk::Widget>, x: f64, y: f64) {
    let parent_widget: &gtk::Widget = parent.upcast_ref();

    let toplevel = parent_widget
        .root()
        .and_then(|r| r.dynamic_cast::<gtk::Window>().ok());
    let (ww, wh) = toplevel
        .as_ref()
        .map(|w| (w.width().max(1) as f32, w.height().max(1) as f32))
        .unwrap_or((1280.0, 800.0));

    let click_in_win = toplevel
        .as_ref()
        .and_then(|w| {
            let widget: &gtk::Widget = w.upcast_ref();
            parent_widget.compute_point(widget, &graphene::Point::new(x as f32, y as f32))
        })
        .unwrap_or_else(|| graphene::Point::new(x as f32, y as f32));

    let (_, nat_w, _, _) = popover.measure(gtk::Orientation::Horizontal, -1);
    let (_, nat_h, _, _) = popover.measure(gtk::Orientation::Vertical, -1);
    // Conservative floor — measure may report 0 for popovers that
    // haven't been allocated yet; small enough to be a no-op clamp
    // when the popover is in fact larger.
    let mw = (nat_w as f32).max(160.0);
    let mh = (nat_h as f32).max(96.0);

    let cx = click_in_win.x();
    let cy = click_in_win.y();
    let mut ax = cx;
    let mut ay = cy;
    if ax + mw > ww {
        ax = (ww - mw).max(0.0);
    }
    if ay + mh > wh {
        ay = (wh - mh).max(0.0);
    }

    let anchor_in_parent = toplevel
        .as_ref()
        .and_then(|w| {
            let widget: &gtk::Widget = w.upcast_ref();
            widget.compute_point(parent_widget, &graphene::Point::new(ax, ay))
        })
        .unwrap_or_else(|| graphene::Point::new(ax, ay));

    // 1×1 rect whose center is mw/2 right of the desired anchor —
    // the popover, centered horizontally on the rect, then sits with
    // its left edge exactly at the anchor.
    let rect_cx = (anchor_in_parent.x() + mw / 2.0) as i32;
    let rect_y = anchor_in_parent.y() as i32 - 1;
    let rect = gtk::gdk::Rectangle::new(rect_cx, rect_y, 1, 1);
    popover.set_pointing_to(Some(&rect));
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_halign(gtk::Align::Fill); // reset any prior halign
}
