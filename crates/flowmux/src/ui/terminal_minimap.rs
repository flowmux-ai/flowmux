// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded-memory overview of VTE terminal content.

use crate::ui::terminal_scrollback::{
    terminal_cell_width, vte_html_row_appearance, vte_html_row_appearances, VteRowAppearance,
};
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
    alternate_screen: Cell<bool>,
    refresh_pending: Cell<bool>,
    samples: RefCell<Vec<RowSample>>,
}

#[derive(Clone, Copy, Default)]
struct RowSample {
    density: f32,
    color: Option<[u8; 3]>,
}

/// A transparent overlay that stores only fixed-size row density/color samples.
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
        install_pointer_navigation(term, &area, state.clone());
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
            self.state.samples.borrow_mut().clear();
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

    pub(crate) fn set_opacity(&self, opacity: u8) {
        self.area.set_opacity(f64::from(opacity) / 100.0);
    }

    pub(crate) fn set_alternate_screen(&self, active: bool) {
        if self.state.alternate_screen.replace(active) == active {
            return;
        }
        let label = if active {
            "Alternate screen: minimap shows the current screen only"
        } else {
            "Terminal scrollback minimap"
        };
        self.area.set_tooltip_text(Some(label));
        self.area
            .update_property(&[gtk::accessible::Property::Label(label)]);
        if let Some(term) = self.terminal.upgrade() {
            refresh_now(&term, &self.area, &self.state);
        }
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
            0.72,
        );
        let _ = cr.paint();

        let color = area.color();
        let samples = state.samples.borrow();
        if !samples.is_empty() {
            let band = height as f64 / samples.len() as f64;
            for (index, sample) in samples.iter().enumerate() {
                let (red, green, blue) = sample.color.map_or_else(
                    || {
                        (
                            color.red() as f64,
                            color.green() as f64,
                            color.blue() as f64,
                        )
                    },
                    |[red, green, blue]| {
                        (
                            f64::from(red) / 255.0,
                            f64::from(green) / 255.0,
                            f64::from(blue) / 255.0,
                        )
                    },
                );
                cr.set_source_rgba(red, green, blue, 0.5);
                let bar_width = (f64::from(width - 4) * f64::from(sample.density)).max(0.0);
                if bar_width > 0.0 {
                    cr.rectangle(
                        2.0,
                        index as f64 * band,
                        bar_width,
                        (band - 1.0).max(1.0).min(band),
                    );
                    let _ = cr.fill();
                }
            }
        }

        if state.alternate_screen.get() {
            let badge_height = f64::from(height.min(14));
            cr.set_source_rgba(
                color.red() as f64,
                color.green() as f64,
                color.blue() as f64,
                0.9,
            );
            cr.rectangle(
                2.0,
                2.0,
                f64::from((width - 4).max(1)),
                (badge_height - 2.0).max(1.0),
            );
            let _ = cr.fill();
            if width >= 24 && height >= 14 {
                cr.set_source_rgb(
                    background.red() as f64,
                    background.green() as f64,
                    background.blue() as f64,
                );
                cr.select_font_face(
                    "Sans",
                    gtk::cairo::FontSlant::Normal,
                    gtk::cairo::FontWeight::Bold,
                );
                cr.set_font_size(8.0);
                cr.move_to(4.0, 11.0);
                let _ = cr.show_text("ALT");
            }
            return;
        }

        let Some(adj) = term.vadjustment() else {
            return;
        };
        if !has_scrollable_range(adj.lower(), adj.upper(), adj.page_size()) {
            return;
        }
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
        cr.rectangle(0.0, top, f64::from(width), viewport_height);
        let _ = cr.fill();
    });
}

fn install_pointer_navigation(
    term: &vte::Terminal,
    area: &gtk::DrawingArea,
    state: Rc<MinimapState>,
) {
    let click = gtk::GestureClick::new();
    click.set_button(gtk::gdk::BUTTON_PRIMARY);
    {
        let term = term.downgrade();
        let area = area.downgrade();
        let state = state.clone();
        click.connect_pressed(move |_, _, _, y| {
            if let (Some(term), Some(area)) = (term.upgrade(), area.upgrade()) {
                if scroll_to_pointer(&term, &state, y, f64::from(area.height())) {
                    term.grab_focus();
                }
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
                scroll_to_pointer(
                    &term,
                    &state,
                    origin_y.get() + offset_y,
                    f64::from(area.height()),
                );
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
    let visible = state.enabled.get() && adjustment.is_some();
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

    let columns = term.column_count().max(1) as usize;
    if state.alternate_screen.get()
        || !has_scrollable_range(adj.lower(), adj.upper(), adj.page_size())
    {
        refresh_visible_screen(term, area, state, columns);
        return;
    }

    let last_column = columns.saturating_sub(1) as i64;
    let mut rows = sampled_rows(adj.lower(), adj.upper(), MAX_SAMPLES);
    // CSI 3 J rebases the adjustment, while VTE text-range rows remain absolute.
    if let Some(last) = rows.last().copied() {
        let offset = term.cursor_position().1.saturating_sub(last);
        for row in &mut rows {
            *row = row.saturating_add(offset);
        }
    }
    let mut samples = state.samples.borrow_mut();
    samples.clear();
    for row in rows {
        let (html, _) = term.text_range_format(vte::Format::Html, row, 0, row, last_column);
        let sample = html
            .as_deref()
            .and_then(|html| vte_html_row_appearance(html).ok())
            .map_or_else(
                || {
                    let (text, _) =
                        term.text_range_format(vte::Format::Text, row, 0, row, last_column);
                    RowSample {
                        density: text
                            .as_deref()
                            .map_or(0.0, |text| row_density(text, columns)),
                        color: None,
                    }
                },
                |appearance| sample_from_appearance(appearance, columns),
            );
        samples.push(sample);
    }
    drop(samples);
    area.queue_draw();
}

fn refresh_visible_screen(
    term: &vte::Terminal,
    area: &gtk::DrawingArea,
    state: &MinimapState,
    columns: usize,
) {
    let appearances = term
        .text_format(vte::Format::Html)
        .as_deref()
        .and_then(|html| vte_html_row_appearances(html).ok());
    let mut samples = state.samples.borrow_mut();
    samples.clear();
    if let Some(appearances) = appearances {
        for index in sampled_rows(0.0, appearances.len() as f64, MAX_SAMPLES) {
            samples.push(sample_from_appearance(appearances[index as usize], columns));
        }
    } else if let Some(text) = term.text_format(vte::Format::Text) {
        let lines: Vec<_> = text.lines().collect();
        for index in sampled_rows(0.0, lines.len() as f64, MAX_SAMPLES) {
            samples.push(RowSample {
                density: row_density(lines[index as usize], columns),
                color: None,
            });
        }
    }
    drop(samples);
    area.queue_draw();
}

fn sample_from_appearance(appearance: VteRowAppearance, columns: usize) -> RowSample {
    RowSample {
        density: (appearance.occupied_cells as f32 / columns as f32).clamp(0.0, 1.0),
        color: appearance.color,
    }
}

fn scroll_to_pointer(term: &vte::Terminal, state: &MinimapState, y: f64, height: f64) -> bool {
    if state.alternate_screen.get() {
        return false;
    }
    let Some(adj) = term.vadjustment() else {
        return true;
    };
    if let Some(target) = pointer_target(adj.lower(), adj.upper(), adj.page_size(), y, height) {
        adj.set_value(target);
    }
    true
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
    (text.chars().map(terminal_cell_width).sum::<usize>() as f32 / columns as f32).clamp(0.0, 1.0)
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

    #[gtk::test]
    async fn sampling_survives_clear_scrollback_and_resize() {
        let term = vte::Terminal::new();
        term.set_scrollback_lines(5_000);
        let adjustment = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0);
        term.set_property("vadjustment", &adjustment);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&term));
        let minimap = TerminalMinimap::new(&term);
        overlay.add_overlay(minimap.widget());
        minimap.set_enabled(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&overlay));
        window.present();
        gtk::glib::timeout_future(Duration::from_millis(50)).await;

        let old_output = "old scrollback\r\n".repeat(600);
        term.feed(old_output.as_bytes());
        term.feed(b"\x1b[3J");
        let mut output = String::new();
        for index in 0..600 {
            output.push_str(&"x".repeat(index % 60 + 1));
            output.push_str("\r\n");
        }
        term.feed(output.as_bytes());
        window.set_default_size(400, 600);
        gtk::glib::timeout_future(Duration::from_millis(250)).await;
        refresh_now(&term, minimap.widget(), &minimap.state);

        let nonempty = minimap
            .state
            .samples
            .borrow()
            .iter()
            .filter(|sample| sample.density > 0.0)
            .count();
        assert!(
            nonempty > MAX_SAMPLES / 2,
            "scrollback sampling returned {nonempty} nonempty rows after clearing history",
        );
        window.close();
    }

    #[gtk::test]
    async fn alternate_screen_samples_only_the_screen_and_blocks_navigation() {
        let term = vte::Terminal::new();
        term.set_scrollback_lines(5_000);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&term));
        let minimap = TerminalMinimap::new(&term);
        overlay.add_overlay(minimap.widget());
        minimap.set_enabled(true);
        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&overlay));
        window.present();
        gtk::glib::timeout_future(Duration::from_millis(50)).await;

        term.feed("history\r\n".repeat(600).as_bytes());
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        let adjustment = term.vadjustment().unwrap();
        adjustment.set_value(adjustment.lower());
        let before = adjustment.value();

        minimap.set_alternate_screen(true);
        assert!(minimap.state.samples.borrow().len() < MAX_SAMPLES);
        scroll_to_pointer(&term, &minimap.state, 600.0, 600.0);
        assert_eq!(adjustment.value(), before);
        window.close();
    }

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
        assert_eq!(row_density("한글", 4), 1.0);
        assert_eq!(row_density("a\u{301}", 4), 0.25);
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
