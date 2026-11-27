// SPDX-License-Identifier: GPL-3.0-or-later

//! The full-size view a chat image or video opens in.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use super::media::{decode_pixels, show_errors, texture, Pixels};

/// Large enough to zoom into, small enough for any GPU texture.
pub(super) const FULL: (u32, u32) = (4096, 4096);
const STEP: f64 = 1.25;
const MAX_ZOOM: f64 = 8.0;

pub(super) enum Content {
    Image(Pixels),
    Video(PathBuf),
}

/// Opens `content` over the window holding `origin`.
pub(super) fn show(origin: &gtk::Widget, title: &str, content: Content) {
    let dialog = adw::Dialog::builder().title(title).build();
    if let Some(window) = origin.root().and_downcast::<gtk::Window>() {
        dialog.set_content_width((window.width() * 9 / 10).max(360));
        dialog.set_content_height((window.height() * 9 / 10).max(280));
    }
    let header = adw::HeaderBar::new();
    let path = match &content {
        Content::Image(Pixels::File(path)) | Content::Video(path) => Some(path.clone()),
        Content::Image(Pixels::Data(_)) => None,
    };
    if let Some(path) = path {
        let open = button("document-open-symbolic", "Open file");
        // The dialog sits outside the transcript, so the file opens from where it was shown.
        let origin = origin.downgrade();
        let shown = dialog.downgrade();
        open.connect_clicked(move |_| {
            if let Some(dialog) = shown.upgrade() {
                dialog.close();
            }
            if let Some(origin) = origin.upgrade() {
                let path = path.display().to_string();
                let _ = origin.activate_action("chat.open-file", Some(&path.to_variant()));
            }
        });
        header.pack_end(&open);
    }
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&match content {
        Content::Image(pixels) => image(&dialog, &header, title, pixels),
        Content::Video(path) => video(&dialog, &path),
    }));
    dialog.set_child(Some(&toolbar));
    dialog.present(Some(origin));
}

fn button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name(icon);
    button.set_tooltip_text(Some(label));
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

fn status(text: &str) -> gtk::Label {
    let status = gtk::Label::new(Some(text));
    status.add_css_class("dim-label");
    status.set_wrap(true);
    status.set_halign(gtk::Align::Center);
    status.set_valign(gtk::Align::Center);
    status
}

struct Zoom {
    picture: gtk::Picture,
    scroller: gtk::ScrolledWindow,
    level: gtk::Button,
    out: gtk::Button,
    into: gtk::Button,
    size: Cell<(f64, f64)>,
    /// `None` fits the image to the window.
    scale: Cell<Option<f64>>,
}

impl Zoom {
    /// The fitted scale; small images stay at their own size.
    fn fit(&self) -> f64 {
        let (width, height) = self.size.get();
        let view = (
            f64::from(self.scroller.width().max(1)),
            f64::from(self.scroller.height().max(1)),
        );
        (view.0 / width).min(view.1 / height).min(1.0)
    }

    fn current(&self) -> f64 {
        self.scale.get().unwrap_or_else(|| self.fit())
    }

    fn set(&self, scale: Option<f64>) {
        let (width, height) = self.size.get();
        if width <= 0.0 {
            return;
        }
        let scale = scale
            .map(|scale| scale.min(MAX_ZOOM))
            .filter(|scale| *scale > self.fit() + 1e-3);
        let centre = [self.scroller.hadjustment(), self.scroller.vadjustment()].map(|adjustment| {
            (adjustment.value() + adjustment.page_size() / 2.0) / adjustment.upper().max(1.0)
        });
        match scale {
            None => {
                self.picture.set_size_request(-1, -1);
                self.picture.set_halign(gtk::Align::Fill);
                self.picture.set_valign(gtk::Align::Fill);
                self.picture.set_content_fit(gtk::ContentFit::ScaleDown);
                self.level.set_label("Fit");
            }
            Some(scale) => {
                // At or above 100% the picture keeps its zoomed size. Below it, the fill
                // allocation and Contain still draw it at `scale`, since the scale exceeds the fit.
                let align = if scale >= 1.0 {
                    gtk::Align::Center
                } else {
                    gtk::Align::Fill
                };
                self.picture
                    .set_size_request((width * scale) as i32, (height * scale) as i32);
                self.picture.set_halign(align);
                self.picture.set_valign(align);
                self.picture.set_content_fit(gtk::ContentFit::Contain);
                self.level.set_label(&format!("{:.0}%", scale * 100.0));
            }
        }
        self.scale.set(scale);
        self.out.set_sensitive(scale.is_some());
        self.into.set_sensitive(self.current() < MAX_ZOOM);
        // Keep the same part of the image in the middle once the new size is laid out.
        let scroller = self.scroller.downgrade();
        glib::idle_add_local_once(move || {
            let Some(scroller) = scroller.upgrade() else {
                return;
            };
            for (adjustment, centre) in [scroller.hadjustment(), scroller.vadjustment()]
                .into_iter()
                .zip(centre)
            {
                adjustment.set_value(centre * adjustment.upper() - adjustment.page_size() / 2.0);
            }
        });
    }

    fn step(&self, factor: f64) {
        self.set(Some(self.current() * factor));
    }

    /// Fit and 100% are the two views a double click or the level button switches between.
    fn toggle(&self) {
        self.set(match self.scale.get() {
            None => Some(1.0),
            Some(_) => None,
        });
    }
}

fn image(
    dialog: &adw::Dialog,
    header: &adw::HeaderBar,
    title: &str,
    pixels: Pixels,
) -> gtk::Widget {
    let picture = gtk::Picture::builder()
        .can_shrink(true)
        .content_fit(gtk::ContentFit::ScaleDown)
        .build();
    picture.set_alternative_text(Some(title));
    picture.update_property(&[gtk::accessible::Property::Label(title)]);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&picture)
        .hexpand(true)
        .vexpand(true)
        .build();
    let level = gtk::Button::with_label("Fit");
    level.set_tooltip_text(Some("Switch between fit and actual size"));
    let zoom = Rc::new(Zoom {
        picture: picture.clone(),
        scroller: scroller.clone(),
        level: level.clone(),
        out: button("zoom-out-symbolic", "Zoom out"),
        into: button("zoom-in-symbolic", "Zoom in"),
        size: Cell::new((0.0, 0.0)),
        scale: Cell::new(None),
    });
    let copy = button("edit-copy-symbolic", "Copy image");
    for widget in [&zoom.out, &level, &zoom.into, &copy] {
        widget.set_sensitive(false);
    }
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    controls.add_css_class("linked");
    controls.append(&zoom.out);
    controls.append(&level);
    controls.append(&zoom.into);
    header.pack_start(&controls);
    header.pack_end(&copy);

    let weak = Rc::downgrade(&zoom);
    zoom.out.connect_clicked(move |_| {
        if let Some(zoom) = weak.upgrade() {
            zoom.step(1.0 / STEP);
        }
    });
    let weak = Rc::downgrade(&zoom);
    zoom.into.connect_clicked(move |_| {
        if let Some(zoom) = weak.upgrade() {
            zoom.step(STEP);
        }
    });
    let weak = Rc::downgrade(&zoom);
    level.connect_clicked(move |_| {
        if let Some(zoom) = weak.upgrade() {
            zoom.toggle();
        }
    });
    let picture_ref = picture.downgrade();
    copy.connect_clicked(move |copy| {
        if let Some(texture) = picture_ref
            .upgrade()
            .and_then(|picture| picture.paintable())
            .and_downcast::<gdk::Texture>()
        {
            copy.clipboard().set_texture(&texture);
        }
    });

    // Ctrl+wheel zooms, a drag pans and a double click switches between fit and 100%.
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    wheel.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(&zoom);
    wheel.connect_scroll(move |wheel, _, dy| {
        let Some(zoom) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if !wheel
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
            || dy == 0.0
        {
            return glib::Propagation::Proceed;
        }
        zoom.step(if dy < 0.0 { STEP } else { 1.0 / STEP });
        glib::Propagation::Stop
    });
    scroller.add_controller(wheel);
    let drag = gtk::GestureDrag::new();
    let start = Rc::new(Cell::new((0.0, 0.0)));
    let (hadjustment, vadjustment) = (scroller.hadjustment(), scroller.vadjustment());
    drag.connect_drag_begin({
        let start = start.clone();
        let (h, v) = (hadjustment.clone(), vadjustment.clone());
        move |_, _, _| start.set((h.value(), v.value()))
    });
    drag.connect_drag_update(move |_, dx, dy| {
        let (x, y) = start.get();
        hadjustment.set_value(x - dx);
        vadjustment.set_value(y - dy);
    });
    scroller.add_controller(drag);
    let click = gtk::GestureClick::new();
    let weak = Rc::downgrade(&zoom);
    click.connect_pressed(move |_, presses, _, _| {
        if let (2, Some(zoom)) = (presses, weak.upgrade()) {
            zoom.toggle();
        }
    });
    scroller.add_controller(click);
    let keys = gtk::EventControllerKey::new();
    let weak = Rc::downgrade(&zoom);
    keys.connect_key_pressed(move |_, key, _, _| {
        let Some(zoom) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if zoom.size.get().0 <= 0.0 {
            return glib::Propagation::Proceed;
        }
        match key {
            gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => zoom.step(STEP),
            gdk::Key::minus | gdk::Key::KP_Subtract => zoom.step(1.0 / STEP),
            gdk::Key::_0 | gdk::Key::KP_0 => zoom.set(None),
            gdk::Key::_1 | gdk::Key::KP_1 => zoom.set(Some(1.0)),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    dialog.add_controller(keys);
    // The controls hold the zoom weakly; the dialog keeps it until it goes away.
    let owner = zoom.clone();
    dialog.connect_closed(move |_| {
        let _ = &owner;
    });

    let loading = status("Loading image…");
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&scroller));
    overlay.add_overlay(&loading);
    glib::spawn_future_local(async move {
        // The inline preview is a thumbnail; the view decodes the original again.
        let decoded = gio::spawn_blocking(move || decode_pixels(&pixels, FULL))
            .await
            .unwrap_or_else(|_| Err("The image could not be decoded.".into()));
        match decoded {
            Ok(rgba) => {
                let (width, height) = rgba.dimensions();
                zoom.size.set((f64::from(width), f64::from(height)));
                zoom.picture.set_paintable(Some(&texture(rgba)));
                for widget in [&zoom.level, &zoom.into, &copy] {
                    widget.set_sensitive(true);
                }
                zoom.set(None);
                loading.set_visible(false);
            }
            Err(error) => loading.set_label(&error),
        }
    });
    overlay.upcast()
}

fn video(dialog: &adw::Dialog, path: &std::path::Path) -> gtk::Widget {
    let video = gtk::Video::builder()
        .autoplay(true)
        .file(&gio::File::for_path(path))
        .hexpand(true)
        .vexpand(true)
        .build();
    let error = status("");
    error.set_visible(false);
    show_errors(&video, &error);
    // Closing the view stops playback instead of leaving the stream running.
    let playing = video.downgrade();
    dialog.connect_closed(move |_| {
        if let Some(video) = playing.upgrade() {
            if let Some(stream) = video.media_stream() {
                stream.pause();
            }
            video.set_file(None::<&gio::File>);
        }
    });
    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
    box_.append(&video);
    box_.append(&error);
    error.set_vexpand(true);
    box_.upcast()
}
