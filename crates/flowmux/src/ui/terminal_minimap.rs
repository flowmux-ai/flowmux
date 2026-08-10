// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded-memory overview of VTE scrollback.

use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use vte::prelude::*;

const MAX_SAMPLES: usize = 128;
const REFRESH_INTERVAL: Duration = Duration::from_millis(200);
const MIN_VIEWPORT_HEIGHT: f64 = 6.0;

#[derive(Default)]
struct MinimapState {
    enabled: Cell<bool>,
    refresh_pending: Cell<bool>,
    densities: RefCell<Vec<f32>>,
}

/// A transparent overlay that stores only a fixed-size row-density sample.
/// VTE remains the sole owner of terminal text and scrollback.
#[derive(Clone)]
pub(crate) struct TerminalMinimap {
    area: gtk::DrawingArea,
    terminal: glib::WeakRef<vte::Terminal>,
    state: Rc<MinimapState>,
}

impl TerminalMinimap {
    pub(crate) fn new(term: &vte::Terminal) -> Self {
        let area = gtk::DrawingArea::builder()
            .accessible_role(gtk::AccessibleRole::Scrollbar)
            .can_focus(false)
            .focusable(false)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Fill)
            .visible(false)
            .build();
        area.set_cursor_from_name(Some("pointer"));
        area.set_tooltip_text(Some("Terminal scrollback minimap"));
        area.update_property(&[gtk::accessible::Property::Label(
            "Terminal scrollback minimap",
        )]);

        let state = Rc::new(MinimapState::default());
        install_drawing(term, &area, state.clone());
        install_pointer_navigation(term, &area);
        install_refresh(term, &area, state.clone());

        Self {
            area,
            terminal: term.downgrade(),
            state,
        }
    }

    pub(crate) fn widget(&self) -> &gtk::DrawingArea {
        &self.area
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        if self.state.enabled.replace(enabled) == enabled {
            return;
        }
        if !enabled {
            self.state.densities.borrow_mut().clear();
            self.area.set_visible(false);
            return;
        }

        if let Some(term) = self.terminal.upgrade() {
            sync_visibility(&self.area, &self.state, term.vadjustment().as_ref());
            refresh_now(&term, &self.area, &self.state);
        }
    }

    pub(crate) fn set_width(&self, width: u16) {
        self.area.set_width_request(i32::from(width));
    }
}

fn install_drawing(term: &vte::Terminal, area: &gtk::DrawingArea, state: Rc<MinimapState>) {
    let term = term.downgrade();
    area.set_draw_func(move |area, cr, width, height| {
        let Some(term) = term.upgrade() else {
            return;
        };
        if width <= 0 || height <= 0 {
            return;
        }

        let background = term.color_background_for_draw();
        cr.set_source_rgba(
            background.red() as f64,
            background.green() as f64,
            background.blue() as f64,
            0.92,
        );
        let _ = cr.paint();

        let color = area.color();
        let densities = state.densities.borrow();
        if !densities.is_empty() {
            let band = height as f64 / densities.len() as f64;
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                0.55,
            );
            for (index, density) in densities.iter().enumerate() {
                let bar_width = (f64::from(width - 4) * f64::from(*density)).max(0.0);
                if bar_width > 0.0 {
                    cr.rectangle(2.0, index as f64 * band, bar_width, band.max(1.0));
                }
            }
            let _ = cr.fill();
        }

        let Some(adj) = term.vadjustment() else {
            return;
        };
        let Some((top, viewport_height)) = viewport_geometry(
            adj.lower(),
            adj.upper(),
            adj.page_size(),
            adj.value(),
            f64::from(height),
        ) else {
            return;
        };
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.18,
        );
        cr.rectangle(0.5, top + 0.5, f64::from(width - 1), viewport_height - 1.0);
        let _ = cr.fill_preserve();
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            0.9,
        );
        cr.set_line_width(1.0);
        let _ = cr.stroke();
    });
}

fn install_pointer_navigation(term: &vte::Terminal, area: &gtk::DrawingArea) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    {
        let term = term.downgrade();
        let area = area.downgrade();
        click.connect_pressed(move |_, _, _, y| {
            if let (Some(term), Some(area)) = (term.upgrade(), area.upgrade()) {
                scroll_to_pointer(&term, y, f64::from(area.height()));
                term.grab_focus();
            }
        });
    }
    area.add_controller(click);

    let drag = gtk::GestureDrag::new();
    drag.set_button(gtk::gdk::BUTTON_PRIMARY);
    let origin_y = Rc::new(Cell::new(0.0));
    {
        let origin_y = origin_y.clone();
        drag.connect_drag_begin(move |_, _, y| origin_y.set(y));
    }
    {
        let term = term.downgrade();
        let area = area.downgrade();
        drag.connect_drag_update(move |_, _, offset_y| {
            if let (Some(term), Some(area)) = (term.upgrade(), area.upgrade()) {
                scroll_to_pointer(&term, origin_y.get() + offset_y, f64::from(area.height()));
            }
        });
    }
    area.add_controller(drag);
}

fn install_refresh(term: &vte::Terminal, area: &gtk::DrawingArea, state: Rc<MinimapState>) {
    {
        let area = area.downgrade();
        let state = state.clone();
        term.connect_contents_changed(move |term| {
            if let Some(area) = area.upgrade() {
                schedule_refresh(term, &area, state.clone());
            }
        });
    }

    let watched_adjustment = Rc::new(RefCell::new(None::<gtk::Adjustment>));
    sync_adjustment(term, area, &state, &watched_adjustment);

    {
        let area = area.downgrade();
        let state = state.clone();
        let watched = watched_adjustment.clone();
        term.connect_vadjustment_notify(move |term| {
            if let Some(area) = area.upgrade() {
                sync_adjustment(term, &area, &state, &watched);
            }
        });
    }
    {
        let area = area.downgrade();
        let state = state.clone();
        let watched = watched_adjustment.clone();
        term.connect_realize(move |term| {
            if let Some(area) = area.upgrade() {
                sync_adjustment(term, &area, &state, &watched);
            }
        });
    }
}

fn sync_adjustment(
    term: &vte::Terminal,
    area: &gtk::DrawingArea,
    state: &Rc<MinimapState>,
    watched: &Rc<RefCell<Option<gtk::Adjustment>>>,
) {
    let Some(adj) = term.vadjustment() else {
        area.set_visible(false);
        return;
    };

    let already_watching = watched
        .borrow()
        .as_ref()
        .is_some_and(|current| current.as_ptr() == adj.as_ptr());
    if !already_watching {
        {
            let term = term.downgrade();
            let area = area.downgrade();
            let state = state.clone();
            adj.connect_changed(move |adj| {
                let Some(area) = area.upgrade() else {
                    return;
                };
                sync_visibility(&area, &state, Some(adj));
                if let Some(term) = term.upgrade() {
                    schedule_refresh(&term, &area, state.clone());
                }
            });
        }
        {
            let area = area.downgrade();
            adj.connect_value_changed(move |_| {
                if let Some(area) = area.upgrade() {
                    area.queue_draw();
                }
            });
        }
        *watched.borrow_mut() = Some(adj.clone());
    }

    sync_visibility(area, state, Some(&adj));
}

fn sync_visibility(
    area: &gtk::DrawingArea,
    state: &MinimapState,
    adjustment: Option<&gtk::Adjustment>,
) {
    let visible = state.enabled.get()
        && adjustment
            .is_some_and(|adj| has_scrollable_range(adj.lower(), adj.upper(), adj.page_size()));
    area.set_visible(visible);
}

fn schedule_refresh(term: &vte::Terminal, area: &gtk::DrawingArea, state: Rc<MinimapState>) {
    if !state.enabled.get() || state.refresh_pending.replace(true) {
        return;
    }

    let term = term.downgrade();
    let area = area.downgrade();
    glib::timeout_add_local_once(REFRESH_INTERVAL, move || {
        state.refresh_pending.set(false);
        if let (Some(term), Some(area)) = (term.upgrade(), area.upgrade()) {
            refresh_now(&term, &area, &state);
        }
    });
}

fn refresh_now(term: &vte::Terminal, area: &gtk::DrawingArea, state: &MinimapState) {
    if !state.enabled.get() {
        return;
    }
    let Some(adj) = term.vadjustment() else {
        return;
    };
    if !has_scrollable_range(adj.lower(), adj.upper(), adj.page_size()) {
        state.densities.borrow_mut().clear();
        area.set_visible(false);
        return;
    }

    let columns = term.column_count().max(1) as usize;
    let last_column = columns.saturating_sub(1) as i64;
    let rows = sampled_rows(adj.lower(), adj.upper(), MAX_SAMPLES);
    let mut densities = state.densities.borrow_mut();
    densities.clear();
    for row in rows {
        let (text, _) = term.text_range_format(vte::Format::Text, row, 0, row, last_column);
        densities.push(
            text.as_deref()
                .map_or(0.0, |text| row_density(text, columns)),
        );
    }
    drop(densities);
    area.queue_draw();
}

fn scroll_to_pointer(term: &vte::Terminal, y: f64, height: f64) {
    let Some(adj) = term.vadjustment() else {
        return;
    };
    if let Some(target) = pointer_target(adj.lower(), adj.upper(), adj.page_size(), y, height) {
        adj.set_value(target);
    }
}

fn has_scrollable_range(lower: f64, upper: f64, page_size: f64) -> bool {
    upper > lower + page_size.max(1.0)
}

fn sampled_rows(lower: f64, upper: f64, limit: usize) -> Vec<i64> {
    if !lower.is_finite() || !upper.is_finite() || upper <= lower || limit == 0 {
        return Vec::new();
    }
    let first = lower.floor() as i64;
    let last = upper.ceil() as i64 - 1;
    let count = last.saturating_sub(first).saturating_add(1) as usize;
    if count <= limit {
        return (first..=last).collect();
    }
    if limit == 1 {
        return vec![first];
    }

    (0..limit)
        .map(|index| {
            let offset = i64::try_from((count - 1) as u128 * index as u128 / (limit - 1) as u128)
                .unwrap_or(i64::MAX);
            first.saturating_add(offset)
        })
        .collect()
}

fn row_density(text: &str, columns: usize) -> f32 {
    if columns == 0 {
        return 0.0;
    }
    (text.chars().filter(|ch| !ch.is_whitespace()).count() as f32 / columns as f32).clamp(0.0, 1.0)
}

fn viewport_geometry(
    lower: f64,
    upper: f64,
    page_size: f64,
    value: f64,
    height: f64,
) -> Option<(f64, f64)> {
    let total = upper - lower;
    if total <= 0.0 || height <= 0.0 {
        return None;
    }
    let viewport_height =
        (height * page_size.max(0.0) / total).clamp(MIN_VIEWPORT_HEIGHT.min(height), height);
    let available = height - viewport_height;
    let max_value = (upper - page_size).max(lower);
    let progress = if max_value > lower {
        ((value - lower) / (max_value - lower)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    Some((available * progress, viewport_height))
}

fn pointer_target(lower: f64, upper: f64, page_size: f64, y: f64, height: f64) -> Option<f64> {
    let total = upper - lower;
    if total <= page_size.max(0.0) || height <= 0.0 {
        return None;
    }
    let max_value = (upper - page_size).max(lower);
    Some((lower + y.clamp(0.0, height) / height * total - page_size / 2.0).clamp(lower, max_value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_is_bounded_and_covers_history_endpoints() {
        let rows = sampled_rows(-999_976.0, 24.0, MAX_SAMPLES);
        assert_eq!(rows.len(), MAX_SAMPLES);
        assert_eq!(rows.first(), Some(&-999_976));
        assert_eq!(rows.last(), Some(&23));
        assert!(rows.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn row_density_ignores_whitespace_and_clamps() {
        assert_eq!(row_density("ab  \n", 4), 0.5);
        assert_eq!(row_density("abcdefgh", 4), 1.0);
        assert_eq!(row_density("anything", 0), 0.0);
    }

    #[test]
    fn viewport_and_pointer_mapping_are_clamped() {
        assert_eq!(
            viewport_geometry(0.0, 100.0, 20.0, 0.0, 200.0),
            Some((0.0, 40.0))
        );
        assert_eq!(
            viewport_geometry(0.0, 100.0, 20.0, 80.0, 200.0),
            Some((160.0, 40.0))
        );
        assert_eq!(pointer_target(0.0, 100.0, 20.0, -1.0, 200.0), Some(0.0));
        assert_eq!(pointer_target(0.0, 100.0, 20.0, 201.0, 200.0), Some(80.0));
        assert_eq!(pointer_target(0.0, 20.0, 20.0, 10.0, 200.0), None);
    }
}
