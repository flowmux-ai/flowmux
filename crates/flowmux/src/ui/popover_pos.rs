// SPDX-License-Identifier: GPL-3.0-or-later
//! Position a context-menu Popover at the click point, shifting its anchor
//! up or left when the measured size would overflow the window.
//!
//! The pointing rectangle encodes horizontal placement. `set_position(Bottom)`
//! requests placement below it, and `set_halign(Fill)` resets prior alignment.

use gtk::graphene;
use gtk::prelude::*;

/// Button menus toggle on the button's own click. Native autohide grabs can
/// dismiss a popup before replaying that same click to its button, reopening
/// it. Keep input in the window and dismiss outside presses explicitly.
pub fn set_menu_popover(button: &gtk::MenuButton, popover: &gtk::Popover) {
    use std::{cell::RefCell, rc::Rc};
    popover.set_autohide(false);
    button.set_popover(Some(popover));
    let weak = button.downgrade();
    popover.connect_show(move |_| {
        if let Some(button) = weak.upgrade() {
            if let Some(root) = button.root() {
                close_other_menus(root.upcast_ref(), &button);
            }
        }
    });
    let installed = Rc::new(RefCell::new(
        None::<(
            gtk::glib::WeakRef<gtk::Window>,
            gtk::glib::WeakRef<gtk::Widget>,
            gtk::EventControllerLegacy,
            gtk::glib::SignalHandlerId,
        )>,
    ));
    let active = installed.clone();
    button.connect_map(move |button| {
        if active.borrow().is_some() {
            return;
        }
        let Some(window) = button.root().and_downcast::<gtk::Window>() else {
            return;
        };
        let Some(native) = button
            .native()
            .and_then(|n| n.dynamic_cast::<gtk::Widget>().ok())
        else {
            return;
        };
        let events = gtk::EventControllerLegacy::new();
        events.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = button.downgrade();
        events.connect_event(move |_, event| {
            let Some(button) = weak.upgrade() else {
                return gtk::glib::Propagation::Proceed;
            };
            let Some(popover) = button.popover().filter(|p| p.is_visible()) else {
                return gtk::glib::Propagation::Proceed;
            };
            if event.event_type() == gtk::gdk::EventType::KeyPress
                && event
                    .downcast_ref::<gtk::gdk::KeyEvent>()
                    .is_some_and(|e| e.keyval() == gtk::gdk::Key::Escape)
            {
                button.popdown();
                button.grab_focus();
                return gtk::glib::Propagation::Stop;
            }
            if matches!(
                event.event_type(),
                gtk::gdk::EventType::ButtonPress | gtk::gdk::EventType::TouchBegin
            ) {
                if let (Some(native), Some((x, y))) = (button.native(), event.position()) {
                    // A submenu button lives in its parent's popup surface.
                    // Never interpret another surface's coordinates as these.
                    if native.surface() == event.surface() {
                        if let Ok(root) = native.dynamic_cast::<gtk::Widget>() {
                            dismiss_outside_press(&button, &popover, &root, x, y);
                        }
                    }
                }
            }
            gtk::glib::Propagation::Proceed
        });
        native.add_controller(events.clone());
        let weak = button.downgrade();
        let deactivate = window.connect_is_active_notify(move |window| {
            if !window.is_active() {
                let window = window.downgrade();
                let button = weak.clone();
                gtk::glib::idle_add_local_once(move || {
                    if let (Some(window), Some(button)) = (window.upgrade(), button.upgrade()) {
                        if !window.is_active() {
                            button.popdown();
                        }
                    }
                });
            }
        });
        *active.borrow_mut() = Some((window.downgrade(), native.downgrade(), events, deactivate));
    });
    button.connect_unmap(move |button| {
        button.popdown();
        if let Some((window, native, events, deactivate)) = installed.borrow_mut().take() {
            if let Some(native) = native.upgrade() {
                native.remove_controller(&events);
            }
            if let Some(window) = window.upgrade() {
                window.disconnect(deactivate);
            }
        }
    });
}

// Keyboard opening has no outside press to dismiss the preceding menu.
// Preserve ancestors when opening a nested submenu.
fn close_other_menus(widget: &gtk::Widget, opening: &gtk::MenuButton) {
    if widget == opening.upcast_ref::<gtk::Widget>() {
        return;
    }
    if let Some(menu) = widget.downcast_ref::<gtk::MenuButton>() {
        if !opening.is_ancestor(menu) {
            menu.popdown();
            return;
        }
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        close_other_menus(&widget, opening);
    }
}

fn dismiss_outside_press(
    button: &gtk::MenuButton,
    popover: &gtk::Popover,
    root: &gtk::Widget,
    x: f64,
    y: f64,
) {
    let inside = root
        .pick(x, y, gtk::PickFlags::DEFAULT)
        .is_some_and(|target| {
            target == *button.upcast_ref::<gtk::Widget>()
                || target.is_ancestor(button)
                || target == *popover.upcast_ref::<gtk::Widget>()
                || target.is_ancestor(popover)
        });
    if !inside {
        button.popdown();
    }
}

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

#[cfg(test)]
pub(crate) async fn menu_toggle_smoke() {
    use std::{cell::Cell, rc::Rc, time::Duration};
    async fn ready(mut predicate: impl FnMut() -> bool) {
        gtk::glib::future_with_timeout(Duration::from_secs(10), async {
            while !predicate() {
                gtk::glib::timeout_future(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("menu state did not settle");
    }
    let window = gtk::Window::builder()
        .default_width(500)
        .default_height(300)
        .build();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let menu = gtk::MenuButton::builder().label("Toggle menu").build();
    let popover = gtk::Popover::new();
    let item = gtk::Button::with_label("Item");
    popover.set_child(Some(&item));
    set_menu_popover(&menu, &popover);
    let other_menu = gtk::MenuButton::builder().label("Other menu").build();
    let other_popup = gtk::Popover::new();
    other_popup.set_child(Some(&gtk::Label::new(Some("Other item"))));
    set_menu_popover(&other_menu, &other_popup);
    let entry = gtk::Entry::new();
    content.append(&other_menu);
    content.append(&menu);
    content.append(&entry);
    window.set_child(Some(&content));
    window.present();
    ready(|| menu.is_mapped() && menu.width() > 0).await;
    let count = window.observe_controllers().n_items();
    let opens = Rc::new(Cell::new(0));
    let observed = opens.clone();
    popover.connect_show(move |_| observed.set(observed.get() + 1));
    let button = menu
        .first_child()
        .unwrap()
        .downcast::<gtk::ToggleButton>()
        .unwrap();
    for expected in 1..=4 {
        button.emit_clicked();
        ready(|| popover.is_mapped() && popover.width() > 0 && popover.height() > 0).await;
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        assert!(menu.is_active());
        let at = button
            .compute_point(&window, &graphene::Point::new(4.0, 4.0))
            .unwrap();
        // The dismiss phase must leave the anchor press for GTK's own toggle.
        dismiss_outside_press(
            &menu,
            &popover,
            window.upcast_ref(),
            at.x() as f64,
            at.y() as f64,
        );
        assert!(menu.is_active());
        button.emit_clicked();
        ready(|| !popover.is_mapped()).await;
        // Synthetic signals bypass the normal event/frame cadence. Let the
        // native popup finish unmapping before opening its surface again.
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        assert!(!menu.is_active());
        assert_eq!(opens.get(), expected, "closing must not reopen the popup");
    }
    menu.popup();
    ready(|| popover.is_mapped() && popover.width() > 0 && popover.height() > 0).await;
    gtk::glib::timeout_future(Duration::from_millis(50)).await;
    let at = entry
        .compute_point(&window, &graphene::Point::new(4.0, 4.0))
        .unwrap();
    dismiss_outside_press(
        &menu,
        &popover,
        window.upcast_ref(),
        at.x() as f64,
        at.y() as f64,
    );
    ready(|| !popover.is_mapped()).await;
    // Synthetic signals bypass the normal event/frame cadence. Let the
    // native popup finish unmapping before opening its surface again.
    gtk::glib::timeout_future(Duration::from_millis(50)).await;
    assert!(!menu.is_active());
    assert!(entry.grab_focus());
    entry.set_text("input after dismissal");
    assert_eq!(entry.text(), "input after dismissal");
    // Unmap/remap must detach and install exactly one window controller.
    for _ in 0..3 {
        menu.popup();
        ready(|| popover.is_mapped() && popover.width() > 0 && popover.height() > 0).await;
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        menu.set_visible(false);
        assert!(!menu.is_active());
        assert_eq!(window.observe_controllers().n_items(), count - 1);
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        menu.set_visible(true);
        ready(|| menu.is_mapped()).await;
        assert_eq!(window.observe_controllers().n_items(), count);
    }
    menu.popup();
    ready(|| popover.is_mapped()).await;
    gtk::glib::timeout_future(Duration::from_millis(50)).await;
    other_menu.popup();
    ready(|| other_popup.is_mapped()).await;
    assert!(
        !menu.is_active(),
        "keyboard opening must dismiss the previous menu"
    );
    other_menu.popdown();
    gtk::glib::timeout_future(Duration::from_millis(50)).await;
    window.destroy();
    println!("MENU_BUTTON_TOGGLE_DISMISS_REMAP_OK");
}

#[cfg(all(test, not(target_os = "macos")))]
#[gtk::test]
async fn menu_buttons_toggle_without_reopening_or_retaining_controllers() {
    menu_toggle_smoke().await;
}
