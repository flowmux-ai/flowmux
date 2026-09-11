// SPDX-License-Identifier: GPL-3.0-or-later
//! Context menu drawn inside the window's own widget tree.
//!
//! GTK popovers are popup surfaces. On X11 hosts (observed on Ubuntu
//! 22.04 Xorg, with both the host toolkit and the GNOME 48 Flatpak
//! runtime) their input grab is unreliable: the menu renders but
//! pointer input reaches its items only intermittently, varying within
//! a single process run. This module sidesteps that whole class of
//! problems by drawing the menu as an ordinary widget inside the
//! toplevel's content `gtk::Overlay`: a full-window transparent scrim
//! catches outside clicks (and Escape) to dismiss, and a `gtk::Fixed`
//! places the menu at the click point. In-window input routing
//! involves no grabs and no popup surfaces, so it behaves identically
//! on X11 and Wayland, sandboxed or not.

use gtk::prelude::*;
use std::rc::Rc;

pub enum MenuItem {
    Action {
        label: &'static str,
        activate: Box<dyn Fn() + 'static>,
    },
    Separator,
}

/// Show a context menu at `(x, y)` in `anchor`'s coordinate space.
///
/// The menu is hosted by the outermost `gtk::Overlay` above `anchor`
/// (the window's content overlay, which also hosts the clipboard
/// toast). If no such overlay exists the menu is dropped with a
/// warning — better no menu than a popover that eats clicks.
pub fn show_at(anchor: &impl IsA<gtk::Widget>, x: f64, y: f64, items: Vec<MenuItem>) {
    let anchor: &gtk::Widget = anchor.upcast_ref();
    let Some(overlay) = host_overlay(anchor) else {
        tracing::warn!("overlay-menu: no host gtk::Overlay ancestor; menu dropped");
        return;
    };

    let menu = gtk::Box::new(gtk::Orientation::Vertical, 0);
    menu.add_css_class("flowmux-overlay-menu");

    let scrim = gtk::Fixed::new();
    scrim.set_halign(gtk::Align::Fill);
    scrim.set_valign(gtk::Align::Fill);
    scrim.set_hexpand(true);
    scrim.set_vexpand(true);

    let dismiss: Rc<dyn Fn()> = {
        let overlay = overlay.downgrade();
        let scrim = scrim.downgrade();
        Rc::new(move || {
            if let (Some(overlay), Some(scrim)) = (overlay.upgrade(), scrim.upgrade()) {
                if scrim.parent().is_some() {
                    overlay.remove_overlay(&scrim);
                }
            }
        })
    };

    let mut first_button: Option<gtk::Button> = None;
    for item in items {
        match item {
            MenuItem::Separator => {
                menu.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            MenuItem::Action { label, activate } => {
                let b = gtk::Button::with_label(label);
                b.add_css_class("flat");
                b.set_halign(gtk::Align::Fill);
                b.set_hexpand(true);
                if let Some(l) = b.child().and_downcast::<gtk::Label>() {
                    l.set_xalign(0.0);
                }
                let dismiss = dismiss.clone();
                b.connect_clicked(move |_| {
                    dismiss();
                    activate();
                });
                if first_button.is_none() {
                    first_button = Some(b.clone());
                }
                menu.append(&b);
            }
        }
    }

    // Translate the click point into overlay coordinates, then clamp
    // the menu fully inside the window — same convention as
    // `popover_pos`: shift up/left by the overflow, never flip.
    let pt = anchor
        .compute_point(&overlay, &gtk::graphene::Point::new(x as f32, y as f32))
        .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
    let (_, nat_w, _, _) = menu.measure(gtk::Orientation::Horizontal, -1);
    let (_, nat_h, _, _) = menu.measure(gtk::Orientation::Vertical, -1);
    // Conservative floor — measure may report 0 before allocation.
    let mw = (nat_w as f32).max(160.0);
    let mh = (nat_h as f32).max(96.0);
    let ow = overlay.width().max(1) as f32;
    let oh = overlay.height().max(1) as f32;
    let mx = pt.x().min(ow - mw).max(0.0);
    let my = pt.y().min(oh - mh).max(0.0);
    scrim.put(&menu, mx as f64, my as f64);

    // Any press outside the menu dismisses it without activating
    // whatever sits underneath — the same modality an autohide
    // popover provides, minus the popup surface.
    let click = gtk::GestureClick::new();
    click.set_button(0);
    {
        let dismiss = dismiss.clone();
        let menu = menu.clone();
        click.connect_pressed(move |gesture, _n_press, px, py| {
            let Some(scrim) = gesture.widget() else {
                return;
            };
            let on_menu = scrim
                .pick(px, py, gtk::PickFlags::DEFAULT)
                .map(|t| t == *menu.upcast_ref::<gtk::Widget>() || t.is_ancestor(&menu))
                .unwrap_or(false);
            if !on_menu {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                dismiss();
            }
        });
    }
    scrim.add_controller(click);

    let key = gtk::EventControllerKey::new();
    {
        let dismiss = dismiss.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Escape {
                dismiss();
                return gtk::glib::Propagation::Stop;
            }
            gtk::glib::Propagation::Proceed
        });
    }
    menu.add_controller(key);

    overlay.add_overlay(&scrim);
    // Focus the first item so Escape and arrow-key navigation work
    // immediately; in-window focus is reliable everywhere.
    if let Some(b) = first_button {
        b.grab_focus();
    }
}

/// The outermost `gtk::Overlay` above `widget` — the window content
/// overlay, not any intermediate one (workspace rows are themselves
/// `gtk::Overlay`s for the hover close button).
fn host_overlay(widget: &gtk::Widget) -> Option<gtk::Overlay> {
    let mut found = None;
    let mut cur = widget.parent();
    while let Some(w) = cur {
        if let Ok(o) = w.clone().downcast::<gtk::Overlay>() {
            found = Some(o);
        }
        cur = w.parent();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gtk::test]
    async fn dismissed_overlay_menu_releases_widgets_and_action() {
        let window = gtk::Window::new();
        let overlay = gtk::Overlay::new();
        let anchor = gtk::Button::with_label("Menu");
        overlay.set_child(Some(&anchor));
        window.set_child(Some(&overlay));
        window.present();
        gtk::glib::timeout_future(std::time::Duration::from_millis(30)).await;
        for dismiss_kind in 0..3 {
            let sentinel = Rc::new(std::cell::Cell::new(false));
            let weak_action = Rc::downgrade(&sentinel);
            show_at(
                &anchor,
                0.0,
                0.0,
                vec![MenuItem::Action {
                    label: "Run",
                    activate: Box::new(move || sentinel.set(true)),
                }],
            );
            let scrim = overlay
                .last_child()
                .unwrap()
                .downcast::<gtk::Fixed>()
                .unwrap();
            let weak_scrim = scrim.downgrade();
            let menu = scrim.first_child().unwrap();
            let button = menu
                .first_child()
                .unwrap()
                .downcast::<gtk::Button>()
                .unwrap();
            match dismiss_kind {
                0 => button.emit_clicked(),
                1 => {
                    let controllers = menu.observe_controllers();
                    let key = (0..controllers.n_items())
                        .find_map(|i| {
                            controllers
                                .item(i)
                                .and_downcast::<gtk::EventControllerKey>()
                        })
                        .unwrap();
                    assert!(key.emit_by_name::<bool>(
                        "key-pressed",
                        &[
                            &gtk::gdk::Key::Escape,
                            &0u32,
                            &gtk::gdk::ModifierType::empty(),
                        ]
                    ));
                }
                _ => {
                    let controllers = scrim.observe_controllers();
                    let click = (0..controllers.n_items())
                        .find_map(|i| controllers.item(i).and_downcast::<gtk::GestureClick>())
                        .unwrap();
                    click.emit_by_name::<()>("pressed", &[&1i32, &-10.0f64, &-10.0f64]);
                }
            }
            assert_eq!(weak_action.upgrade().unwrap().get(), dismiss_kind == 0);
            assert!(scrim.parent().is_none());
            drop(button);
            drop(menu);
            drop(scrim);
            for _ in 0..40 {
                if weak_scrim.upgrade().is_none() {
                    break;
                }
                gtk::glib::timeout_future(std::time::Duration::from_millis(25)).await;
            }
            assert!(weak_scrim.upgrade().is_none(), "dismissed scrim leaked");
            assert!(weak_action.upgrade().is_none(), "dismissed action leaked");
        }
        window.close();
    }
}
