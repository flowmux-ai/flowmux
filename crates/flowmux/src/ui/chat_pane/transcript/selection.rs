// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::{gdk, glib, graphene, pango, prelude::*};

type Position = (usize, i32);

struct Selection {
    leaves: Vec<gtk::Widget>,
    anchor: Position,
    extent: Position,
    colors: (gdk::RGBA, gdk::RGBA),
}

pub(super) fn attach(scroller: &gtk::ScrolledWindow, history: &gtk::Box) {
    let selection = Rc::new(RefCell::new(None::<Selection>));
    let pending = Rc::new(RefCell::new(None::<Selection>));
    // A press inside the selection may drag its text elsewhere instead of selecting again.
    let carry = Rc::new(Cell::new(Carry::None));
    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);
    scroller.set_focusable(true);
    let selected = selection.clone();
    let start = pending.clone();
    let carrying = carry.clone();
    let history = history.downgrade();
    drag.connect_drag_begin(move |drag, x, y| {
        start.borrow_mut().take();
        carrying.set(Carry::None);
        let touch = drag
            .current_event_device()
            .is_some_and(|device| device.source() == gdk::InputSource::Touchscreen);
        if let Some(scroller) = drag.widget().filter(|_| !touch) {
            if selected
                .borrow()
                .as_ref()
                .is_some_and(|selection| selection.contains(&scroller, (x, y)))
            {
                // Claim the press so the label underneath neither selects nor takes focus;
                // the grouped drag source still starts the drag.
                carrying.set(Carry::Pressed);
                drag.set_state(gtk::EventSequenceState::Claimed);
                return;
            }
        }
        if let Some(selection) = selected.borrow_mut().take() {
            selection.clear();
        }
        let (Some(scroller), Some(history)) = (drag.widget(), history.upgrade()) else {
            return;
        };
        // A selectable child can receive focus and scroll into view before the first motion.
        let mut leaves = Vec::new();
        collect(history.upcast_ref(), &mut leaves);
        // A label keeps its own selection (double-click, short drag) after another is pressed.
        clear_native(&leaves);
        // A focused label would keep showing its caret; a pressed label takes focus back.
        if scroller
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| leaves.contains(&focus))
        {
            scroller.grab_focus();
        }
        let touch = drag
            .current_event_device()
            .is_some_and(|device| device.source() == gdk::InputSource::Touchscreen);
        if touch || !can_start(&scroller, x, y) {
            drag.set_state(gtk::EventSequenceState::Denied);
            return;
        }
        if let Some(anchor) = position(&leaves, &scroller, (x, y)) {
            *start.borrow_mut() = Some(Selection {
                leaves,
                anchor,
                extent: anchor,
                colors: selection_colors(&scroller),
            });
        }
    });

    let scroll_point = Rc::new(Cell::new((0.0, 0.0)));
    let start = pending.clone();
    let selected = selection.clone();
    let carrying = carry.clone();
    drag.connect_drag_end(move |_, _, _| {
        start.borrow_mut().take();
        // A click inside the selection, without dragging it away, deselects.
        if carrying.replace(Carry::None) == Carry::Pressed {
            if let Some(selection) = selected.borrow_mut().take() {
                selection.clear();
            }
        }
    });
    let current_point = scroll_point.clone();
    let selected = selection.clone();
    let carrying = carry.clone();
    drag.connect_drag_update(move |drag, dx, dy| {
        if carrying.get() != Carry::None {
            return;
        }
        let (Some(scroller), Some((x, y))) = (drag.widget(), drag.start_point()) else {
            return;
        };
        let point = (x + dx, y + dy);
        current_point.set(point);
        let mut selected = selected.borrow_mut();
        if selected.is_none() {
            if !scroller.drag_check_threshold(x as i32, y as i32, point.0 as i32, point.1 as i32) {
                return;
            }
            let Some(start) = pending.borrow_mut().take() else {
                return;
            };
            if !drag.set_state(gtk::EventSequenceState::Claimed) {
                return;
            }
            scroller.grab_focus();
            // The pressed label may have started its own selection before the drag was claimed.
            clear_native(&start.leaves);
            *selected = Some(start);
        }
        if let Some(selection) = selected.as_mut() {
            selection.update(&scroller, point);
        }
    });

    let selected = selection.clone();
    let weak_drag = drag.downgrade();
    // This runs only during a claimed drag so holding the pointer at an edge keeps selecting.
    let carrying = carry.clone();
    drag.connect_sequence_state_changed(move |drag, _, state| {
        // A press that may carry the selection away never extends it at the edges.
        if state != gtk::EventSequenceState::Claimed || carrying.get() != Carry::None {
            return;
        }
        let Some(scroller) = drag.widget().and_downcast::<gtk::ScrolledWindow>() else {
            return;
        };
        let selected = selected.clone();
        let scroll_point = scroll_point.clone();
        let weak_drag = weak_drag.clone();
        scroller.add_tick_callback(move |scroller, _| {
            if !weak_drag.upgrade().is_some_and(|drag| drag.is_active()) {
                return glib::ControlFlow::Break;
            }
            let point = scroll_point.get();
            let adjustment = scroller.vadjustment();
            let height = f64::from(scroller.height());
            let step = (point.1 - 18.0).min(0.0) + (point.1 - height + 18.0).max(0.0);
            if step != 0.0 {
                adjustment.set_value(adjustment.value() + step.clamp(-32.0, 32.0));
                if let Some(selection) = selected.borrow_mut().as_mut() {
                    selection.update(scroller.upcast_ref(), (point.0, point.1.clamp(0.0, height)));
                }
            }
            glib::ControlFlow::Continue
        });
    });
    scroller.add_controller(drag.clone());

    // Dragging the selection offers its text, so a terminal or editor can take a copy.
    let source = gtk::DragSource::new();
    source.set_button(gdk::BUTTON_PRIMARY);
    source.set_actions(gdk::DragAction::COPY);
    source.set_propagation_phase(gtk::PropagationPhase::Capture);
    let dragged = Rc::new(RefCell::new(String::new()));
    let selected = selection.clone();
    let carrying = carry.clone();
    let text = dragged.clone();
    source.connect_prepare(move |_, _, _| {
        if carrying.get() != Carry::Pressed {
            return None;
        }
        let selected = selected.borrow();
        let selection = selected.as_ref()?;
        let value = selection.text();
        if value.is_empty() {
            return None;
        }
        carrying.set(Carry::Dragged);
        // The pressed label may have started its own selection meanwhile.
        clear_native(&selection.leaves);
        *text.borrow_mut() = excerpt(&value);
        Some(gdk::ContentProvider::for_value(&value.to_value()))
    });
    source.connect_drag_begin(move |_, drag| {
        let label = gtk::Label::new(Some(&dragged.borrow()));
        label.set_margin_top(6);
        label.set_margin_bottom(6);
        label.set_margin_start(10);
        label.set_margin_end(10);
        let card = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        card.add_css_class("card");
        card.append(&label);
        gtk::DragIcon::for_drag(drag).set_child(Some(&card));
    });
    scroller.add_controller(source.clone());
    // Grouping needs both on the widget; a claimed press then still reaches the source.
    source.group_with(&drag);

    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(move |keys, key, _, modifiers| {
        let copy_modifier = if cfg!(target_os = "macos") {
            gdk::ModifierType::META_MASK
        } else {
            gdk::ModifierType::CONTROL_MASK
        };
        if key != gdk::Key::c
            || modifiers & gtk::accelerator_get_default_mod_mask() != copy_modifier
        {
            return glib::Propagation::Proceed;
        }
        let Some(widget) = keys.widget() else {
            return glib::Propagation::Proceed;
        };
        if widget
            .root()
            .and_then(|root| root.focus())
            .is_some_and(|focus| {
                focus.is::<gtk::Editable>()
                    || focus
                        .downcast_ref::<gtk::TextView>()
                        .is_some_and(gtk::TextView::is_editable)
            })
        {
            return glib::Propagation::Proceed;
        }
        let text = selection.borrow().as_ref().map(Selection::text);
        if let Some(text) = text.filter(|text| !text.is_empty()) {
            widget.clipboard().set_text(&text);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    scroller.add_controller(keys);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Carry {
    None,
    /// Pressed inside the selection; a click deselects and a drag carries the text.
    Pressed,
    Dragged,
}

/// The first line of dragged text, short enough for the drag icon.
fn excerpt(text: &str) -> String {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut excerpt: String = line.trim().chars().take(60).collect();
    if excerpt.len() < line.trim().len() || text.trim().lines().count() > 1 {
        excerpt.push('…');
    }
    excerpt
}

#[allow(deprecated)]
fn selection_colors(widget: &gtk::Widget) -> (gdk::RGBA, gdk::RGBA) {
    let style = widget.style_context();
    (
        style
            .lookup_color("accent_bg_color")
            .unwrap_or(gdk::RGBA::BLUE),
        style
            .lookup_color("accent_fg_color")
            .unwrap_or(gdk::RGBA::WHITE),
    )
}

fn can_start(scroller: &gtk::Widget, x: f64, y: f64) -> bool {
    let mut target = scroller.pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(widget) = target {
        if is_text(&widget) {
            return true;
        }
        if widget.is::<gtk::Button>()
            || widget.is::<gtk::Expander>()
            || widget.is::<gtk::DropDown>()
            || widget.is::<gtk::Scrollbar>()
            || widget.is::<gtk::Entry>()
            || widget.is::<gtk::TextView>()
        {
            return false;
        }
        if widget == *scroller {
            return true;
        }
        target = widget.parent();
    }
    false
}

fn is_text(widget: &gtk::Widget) -> bool {
    widget
        .downcast_ref::<gtk::Label>()
        .is_some_and(gtk::Label::is_selectable)
        || widget
            .downcast_ref::<gtk::TextView>()
            .is_some_and(|view| !view.is_editable())
}

fn collect(widget: &gtk::Widget, leaves: &mut Vec<gtk::Widget>) {
    if !widget.is_mapped() {
        return;
    }
    if is_text(widget) {
        leaves.push(widget.clone());
        return;
    }
    let mut child = widget.first_child();
    while let Some(widget) = child {
        collect(&widget, leaves);
        child = widget.next_sibling();
    }
}

fn position(
    leaves: &[gtk::Widget],
    scroller: &gtk::Widget,
    (x, y): (f64, f64),
) -> Option<Position> {
    let point = graphene::Point::new(x as f32, y as f32);
    let mut target = scroller.pick(x, y, gtk::PickFlags::DEFAULT);
    while let Some(widget) = target {
        if let Some(index) = leaves.iter().position(|leaf| *leaf == widget) {
            let local = scroller.compute_point(&widget, &point)?;
            return Some((index, offset(&widget, local)));
        }
        if widget == *scroller {
            break;
        }
        target = widget.parent();
    }
    let mut previous = None;
    'leaf: for (index, widget) in leaves
        .iter()
        .enumerate()
        .filter(|(_, leaf)| leaf.is_mapped())
    {
        let Some(mut bounds) = widget.compute_bounds(scroller) else {
            continue;
        };
        // A nested scrollable label can extend below the block that contains it.
        let mut parent = widget.parent();
        while let Some(ancestor) = parent.filter(|ancestor| *ancestor != *scroller) {
            if ancestor.is::<gtk::ScrolledWindow>() {
                if let Some(frame) = ancestor.compute_bounds(scroller) {
                    let Some(clipped) = bounds.intersection(&frame) else {
                        continue 'leaf;
                    };
                    bounds = clipped;
                }
            }
            parent = ancestor.parent();
        }
        if point.y() < bounds.y() {
            return previous.or(Some((index, 0)));
        }
        if point.y() < bounds.y() + bounds.height() {
            let local = scroller.compute_point(widget, &point)?;
            return Some((index, offset(widget, local)));
        }
        previous = Some((index, length(widget)));
    }
    previous
}

fn offset(widget: &gtk::Widget, point: graphene::Point) -> i32 {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        let (x, y) = label.layout_offsets();
        let (_, byte, trailing) = label.layout().xy_to_index(
            ((point.x() - x as f32) * pango::SCALE as f32) as i32,
            ((point.y() - y as f32) * pango::SCALE as f32) as i32,
        );
        // Pango indexes bytes and returns a character count within the final cluster.
        return label
            .text()
            .get(..byte as usize)
            .map_or(0, |text| text.chars().count() as i32)
            + trailing;
    }
    let view = widget.downcast_ref::<gtk::TextView>().unwrap();
    let (x, y) = view.window_to_buffer_coords(
        gtk::TextWindowType::Widget,
        point.x() as i32,
        point.y() as i32,
    );
    if let Some((iter, trailing)) = view.iter_at_position(x, y) {
        return iter.offset() + trailing;
    }
    let (mut iter, _) = view.line_at_y(y);
    if x > view.iter_location(&iter).x() && !iter.ends_line() {
        iter.forward_to_line_end();
    }
    iter.offset()
}

fn length(widget: &gtk::Widget) -> i32 {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.text().chars().count() as i32
    } else {
        widget
            .downcast_ref::<gtk::TextView>()
            .unwrap()
            .buffer()
            .char_count()
    }
}

fn clear_native(leaves: &[gtk::Widget]) {
    for label in leaves
        .iter()
        .filter_map(|leaf| leaf.downcast_ref::<gtk::Label>())
    {
        if label.selection_bounds().is_some() {
            label.select_region(0, 0);
        }
    }
}

// Native selections take exclusive ownership of the primary clipboard.
fn paint(widget: &gtk::Widget, start: i32, end: i32, (bg, fg): (gdk::RGBA, gdk::RGBA)) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        label.set_attributes(None);
        // GTK 4.14 merges these attributes into the parsed markup attributes in place, so
        // clearing them leaves the old highlight. Reparsing the markup drops it; setting the
        // same markup again is a no-op.
        if label.uses_markup() {
            let markup = label.label();
            label.set_markup("");
            label.set_markup(&markup);
        }
        if start >= end {
            return;
        }
        let text = label.text();
        let mut offsets = text.char_indices().map(|(byte, _)| byte);
        let first = offsets.nth(start as usize).unwrap_or(text.len()) as u32;
        let last = offsets
            .nth((end - start - 1) as usize)
            .unwrap_or(text.len()) as u32;
        let [r, g, b] = color_components(bg);
        let background = pango::AttrColor::new_background(r, g, b);
        let [r, g, b] = color_components(fg);
        let foreground = pango::AttrColor::new_foreground(r, g, b);
        let attributes = pango::AttrList::new();
        for mut color in [background.upcast(), foreground.upcast()] {
            color.set_start_index(first);
            color.set_end_index(last);
            attributes.insert(color);
        }
        label.set_attributes(Some(&attributes));
    } else {
        let buffer = widget.downcast_ref::<gtk::TextView>().unwrap().buffer();
        const TAG: &str = "flowmux-chat-selection";
        let tag = buffer.tag_table().lookup(TAG);
        if let Some(tag) = &tag {
            buffer.remove_tag(tag, &buffer.start_iter(), &buffer.end_iter());
        }
        if start < end {
            let tag = tag.or_else(|| {
                buffer.create_tag(
                    Some(TAG),
                    &[("background-rgba", &bg), ("foreground-rgba", &fg)],
                )
            });
            if let Some(tag) = tag {
                buffer.apply_tag(
                    &tag,
                    &buffer.iter_at_offset(start),
                    &buffer.iter_at_offset(end),
                );
            }
        }
    }
}

fn color_components(color: gdk::RGBA) -> [u16; 3] {
    [color.red(), color.green(), color.blue()].map(|component| (component * u16::MAX as f32) as u16)
}

impl Selection {
    /// Whether `point` in `scroller` falls on selected text.
    fn contains(&self, scroller: &gtk::Widget, (x, y): (f64, f64)) -> bool {
        let mut target = scroller.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(widget) = target {
            if let Some(index) = self.leaves.iter().position(|leaf| *leaf == widget) {
                let point = graphene::Point::new(x as f32, y as f32);
                let Some(local) = scroller.compute_point(&widget, &point) else {
                    return false;
                };
                let offset = offset(&widget, local);
                let (first, last) = self.span(index, self.extent);
                return first < last && (first..=last).contains(&offset);
            }
            if widget == *scroller {
                break;
            }
            target = widget.parent();
        }
        false
    }

    fn span(&self, index: usize, extent: Position) -> (i32, i32) {
        let (start, end) = (self.anchor.min(extent), self.anchor.max(extent));
        if index < start.0 || index > end.0 {
            return (0, 0);
        }
        let first = if index == start.0 { start.1 } else { 0 };
        let last = if index == end.0 {
            end.1
        } else {
            length(&self.leaves[index])
        };
        (first, last)
    }

    fn clear(self) {
        let (start, end) = (self.anchor.min(self.extent), self.anchor.max(self.extent));
        for widget in &self.leaves[start.0..=end.0] {
            paint(widget, 0, 0, self.colors);
        }
    }

    fn update(&mut self, scroller: &gtk::Widget, point: (f64, f64)) {
        let Some(extent) = position(&self.leaves, scroller, point) else {
            return;
        };
        if extent == self.extent {
            return;
        }
        let (start, end) = (self.anchor.min(extent), self.anchor.max(extent));
        for index in start.0.min(self.extent.0)..=end.0.max(self.extent.0) {
            let (first, last) = self.span(index, extent);
            if (first, last) != self.span(index, self.extent) {
                paint(&self.leaves[index], first, last, self.colors);
            }
        }
        self.extent = extent;
    }

    fn text(&self) -> String {
        let (start, end) = (self.anchor.min(self.extent), self.anchor.max(self.extent));
        let mut parts = Vec::new();
        for index in start.0..=end.0 {
            let widget = &self.leaves[index];
            let (first, last) = self.span(index, self.extent);
            if first >= last {
                continue;
            }
            parts.push(if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                label
                    .text()
                    .chars()
                    .skip(first as usize)
                    .take((last - first) as usize)
                    .collect()
            } else {
                let buffer = widget.downcast_ref::<gtk::TextView>().unwrap().buffer();
                buffer
                    .text(
                        &buffer.iter_at_offset(first),
                        &buffer.iter_at_offset(last),
                        false,
                    )
                    .to_string()
            });
        }
        parts.join("\n")
    }
}
