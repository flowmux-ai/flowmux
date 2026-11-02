// SPDX-License-Identifier: GPL-3.0-or-later
//! Scale wheel deltas before VTE interprets scrollback, mouse protocols or alternate screens.
use gtk::{glib, prelude::*};
use std::{cell::Cell, rc::Rc};

pub(super) fn install(term: &vte::Terminal) -> Rc<Cell<f64>> {
    use glib::translate::{from_glib, IntoGlib};
    let multiplier = Rc::new(Cell::new(1.0));
    let controllers = term.observe_controllers();
    let controller = (0..controllers.n_items()).find_map(|index| {
        controllers
            .item(index)?
            .downcast::<gtk::EventControllerScroll>()
            .ok()
    });
    let Some(controller) = controller else {
        tracing::warn!("VTE has no scroll controller; terminal wheel scaling unavailable");
        return multiplier;
    };
    let signal = glib::subclass::SignalId::lookup("scroll", controller.type_()).unwrap();
    let handler = unsafe {
        glib::gobject_ffi::g_signal_handler_find(
            controller.as_ptr().cast(),
            glib::gobject_ffi::G_SIGNAL_MATCH_ID,
            signal.into_glib(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if handler == 0 {
        tracing::warn!("VTE has no scroll handler; terminal wheel scaling unavailable");
        return multiplier;
    }
    let handler: glib::SignalHandlerId = unsafe { from_glib(handler) };
    controller.block_signal(&handler);
    let scale = multiplier.clone();
    let forwarding = Cell::new(false);
    controller.connect_scroll(move |controller, dx, dy| {
        if forwarding.get() {
            return glib::Propagation::Proceed;
        }
        let factor = if controller
            .current_event()
            .and_then(|event| event.downcast::<gtk::gdk::ScrollEvent>().ok())
            .is_some_and(|event| event.unit() == gtk::gdk::ScrollUnit::Wheel)
        {
            scale.get()
        } else {
            1.0
        };
        // shortcut: wrap VTE's GTK scroll handler until VTE exposes a speed setting.
        // Re-emitting on this controller retains the original event and VTE's input modes.
        forwarding.set(true);
        controller.unblock_signal(&handler);
        let handled = controller.emit_by_name::<bool>("scroll", &[&(dx * factor), &(dy * factor)]);
        controller.block_signal(&handler);
        forwarding.set(false);
        handled.into()
    });
    multiplier
}
