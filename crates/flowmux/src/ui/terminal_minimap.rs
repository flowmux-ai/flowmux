// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded-memory overview of VTE terminal content.

use crate::ui::terminal_scrollback::{terminal_cell_width, vte_html_pixel_rows, VtePixelRun};
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use vte::prelude::*;

const REFRESH_INTERVAL: Duration = Duration::from_millis(100);
const PREVIEW_SCROLL_ROWS: i64 = 25;

#[derive(Default)]
struct MinimapState {
    enabled: Cell<bool>,
    alternate_screen: Cell<bool>,
    refresh_source: RefCell<Option<glib::SourceId>>,
    preview_offset: Cell<i64>,
    preview_top: Cell<i64>,
    preview_rows: Cell<usize>,
    columns: Cell<usize>,
    text_row_offset: Cell<i64>,
    pixel_rows: RefCell<Vec<Vec<VtePixelRun>>>,
}

/// A cell-level preview of a movable VTE scrollback window.
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
            if let Some(source) = self.state.refresh_source.borrow_mut().take() {
                source.remove();
            }
            *self.state.pixel_rows.borrow_mut() = Vec::new();
            self.state.preview_offset.set(0);
            self.state.preview_rows.set(0);
            self.area.set_visible(false);
            return;
        }

        if let Some(term) = self.terminal.upgrade() {
            sync_visibility(&self.area, &self.state, term.vadjustment().as_ref());
            if self.area.is_mapped() {
                refresh_now(&term, &self.area, &self.state);
            }
        }
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.state.enabled.get()
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
        self.state.preview_offset.set(0);
        if active {
            if let Some(source) = self.state.refresh_source.borrow_mut().take() {
                source.remove();
            }
            self.area.set_visible(false);
            return;
        }
        if let Some(term) = self.terminal.upgrade() {
            sync_visibility(&self.area, &self.state, term.vadjustment().as_ref());
            if self.area.is_mapped() {
                refresh_now(&term, &self.area, &self.state);
            }
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
        let columns = state.columns.get().max(1);
        let cell_width = f64::from(width) / columns as f64;
        cr.set_antialias(gtk::cairo::Antialias::None);
        for (row, runs) in state
            .pixel_rows
            .borrow()
            .iter()
            .take(state.preview_rows.get())
            .enumerate()
        {
            for run in runs {
                let start = run.column.min(columns);
                let end = run.column.saturating_add(run.len).min(columns);
                if start == end {
                    continue;
                }
                let (red, green, blue) = run.color.map_or_else(
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
                cr.set_source_rgb(red, green, blue);
                cr.rectangle(
                    start as f64 * cell_width,
                    row as f64,
                    (end - start) as f64 * cell_width,
                    1.0,
                );
                let _ = cr.fill();
            }
        }

        let Some(adj) = term.vadjustment() else {
            return;
        };
        if !has_scrollable_range(adj.lower(), adj.upper(), adj.page_size()) {
            return;
        }
        let Some((top, viewport_height, out_of_bounds)) = viewport_geometry(
            state.preview_top.get(),
            state.preview_rows.get(),
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
            if out_of_bounds { 0.08 } else { 0.24 },
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
        let state = state.clone();
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

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let term = term.downgrade();
        let area = area.downgrade();
        scroll.connect_scroll(move |_, _, delta_y| {
            let (Some(term), Some(area)) = (term.upgrade(), area.upgrade()) else {
                return glib::Propagation::Proceed;
            };
            if scroll_preview(&term, &area, &state, delta_y) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
    }
    area.add_controller(scroll);
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
    {
        let term = term.downgrade();
        let state = state.clone();
        area.connect_resize(move |area, _, _| {
            if let Some(term) = term.upgrade() {
                schedule_refresh(&term, area, state.clone());
            }
        });
    }
    {
        let term = term.downgrade();
        let state = state.clone();
        area.connect_map(move |area| {
            if let Some(term) = term.upgrade() {
                refresh_now(&term, area, &state);
            }
        });
    }
    {
        let state = state.clone();
        area.connect_unmap(move |_| {
            if let Some(source) = state.refresh_source.borrow_mut().take() {
                source.remove();
            }
            *state.pixel_rows.borrow_mut() = Vec::new();
            state.preview_rows.set(0);
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
            let term = term.downgrade();
            let state = state.clone();
            adj.connect_value_changed(move |_| {
                if let (Some(term), Some(area)) = (term.upgrade(), area.upgrade()) {
                    if sync_preview_to_viewport(&term, &state, area.height().max(1) as usize) {
                        schedule_refresh(&term, &area, state.clone());
                    } else {
                        area.queue_draw();
                    }
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
    let visible = state.enabled.get() && !state.alternate_screen.get() && adjustment.is_some();
    area.set_visible(visible);
}

fn schedule_refresh(term: &vte::Terminal, area: &gtk::DrawingArea, state: Rc<MinimapState>) {
    if !state.enabled.get() || state.alternate_screen.get() || !area.is_mapped() {
        return;
    }
    if let Some(source) = state.refresh_source.borrow_mut().take() {
        source.remove();
    }

    let term = term.downgrade();
    let area = area.downgrade();
    let refresh_state = state.clone();
    let source = glib::timeout_add_local_once(REFRESH_INTERVAL, move || {
        refresh_state.refresh_source.borrow_mut().take();
        if let (Some(term), Some(area)) = (term.upgrade(), area.upgrade()) {
            refresh_now(&term, &area, &refresh_state);
        }
    });
    *state.refresh_source.borrow_mut() = Some(source);
}

fn refresh_now(term: &vte::Terminal, area: &gtk::DrawingArea, state: &MinimapState) {
    if !state.enabled.get() || state.alternate_screen.get() {
        return;
    }
    let Some(adj) = term.vadjustment() else {
        return;
    };
    let columns = term.column_count().max(1) as usize;
    if !has_scrollable_range(adj.lower(), adj.upper(), adj.page_size()) {
        refresh_visible_screen(term, area, state, columns);
        return;
    }

    let Some(window) = preview_window(
        adj.lower(),
        adj.upper(),
        area.height().max(1) as usize,
        state.preview_offset.get(),
    ) else {
        return;
    };
    state.preview_offset.set(window.offset);
    state.preview_top.set(window.top);
    state.preview_rows.set(window.rows);
    state.columns.set(columns);

    let last_column = columns.saturating_sub(1) as i64;
    // CSI 3 J rebases the adjustment, while VTE text-range rows remain absolute.
    // Keep the translation monotonic so normal cursor-up repaint sequences do
    // not shift the entire minimap back and forth.
    let offset = advance_text_row_offset(
        state.text_row_offset.get(),
        term.cursor_position().1,
        window.upper,
    );
    state.text_row_offset.set(offset);
    let first = window.top.saturating_add(offset);
    let last = window
        .top
        .saturating_add(window.rows as i64 - 1)
        .saturating_add(offset);
    let (html, _) = term.text_range_format(vte::Format::Html, first, 0, last, last_column);
    let rows = html
        .as_deref()
        .and_then(|html| vte_html_pixel_rows(html, columns).ok())
        .or_else(|| {
            let (text, _) = term.text_range_format(vte::Format::Text, first, 0, last, last_column);
            text.as_deref()
                .map(|text| plain_text_pixel_rows(text, columns))
        })
        .unwrap_or_default();
    replace_pixel_rows(state, rows, window.rows);
    area.queue_draw();
}

fn refresh_visible_screen(
    term: &vte::Terminal,
    area: &gtk::DrawingArea,
    state: &MinimapState,
    columns: usize,
) {
    state.preview_offset.set(0);
    state.preview_top.set(
        term.vadjustment()
            .as_ref()
            .map_or(0, |adj| adj.lower().floor() as i64),
    );
    state.columns.set(columns);
    let limit = (term.row_count().max(1) as usize).min(area.height().max(1) as usize);
    state.preview_rows.set(limit);
    let rows = term
        .text_format(vte::Format::Html)
        .as_deref()
        .and_then(|html| vte_html_pixel_rows(html, columns).ok())
        .or_else(|| {
            term.text_format(vte::Format::Text)
                .as_deref()
                .map(|text| plain_text_pixel_rows(text, columns))
        })
        .unwrap_or_default();
    replace_pixel_rows(state, rows, limit);
    area.queue_draw();
}

fn replace_pixel_rows(state: &MinimapState, mut rows: Vec<Vec<VtePixelRun>>, limit: usize) {
    rows.truncate(limit);
    *state.pixel_rows.borrow_mut() = rows;
}

fn scroll_to_pointer(term: &vte::Terminal, state: &MinimapState, y: f64, height: f64) -> bool {
    if state.alternate_screen.get() {
        return false;
    }
    let Some(adj) = term.vadjustment() else {
        return true;
    };
    if let Some(target) = pointer_target(
        adj.lower(),
        adj.upper(),
        adj.page_size(),
        state.preview_top.get(),
        state.preview_rows.get(),
        y,
        height,
    ) {
        adj.set_value(target);
    }
    true
}

fn scroll_preview(
    term: &vte::Terminal,
    area: &gtk::DrawingArea,
    state: &MinimapState,
    delta_y: f64,
) -> bool {
    if state.alternate_screen.get() || delta_y == 0.0 {
        return false;
    }
    let Some(adj) = term.vadjustment() else {
        return true;
    };
    let Some(window) = preview_window(
        adj.lower(),
        adj.upper(),
        area.height().max(1) as usize,
        state.preview_offset.get(),
    ) else {
        return true;
    };
    let offset =
        (window.offset - delta_y.signum() as i64 * PREVIEW_SCROLL_ROWS).clamp(0, window.max_offset);
    if offset != state.preview_offset.replace(offset) {
        refresh_now(term, area, state);
    }
    true
}

fn has_scrollable_range(lower: f64, upper: f64, page_size: f64) -> bool {
    upper > lower + page_size.max(1.0)
}

fn advance_text_row_offset(previous: i64, cursor_row: i64, adjustment_upper: i64) -> i64 {
    previous.max(cursor_row.saturating_sub(adjustment_upper.saturating_sub(1)))
}

fn sync_preview_to_viewport(term: &vte::Terminal, state: &MinimapState, limit: usize) -> bool {
    if !state.enabled.get() || state.alternate_screen.get() {
        return false;
    }
    let Some(adj) = term.vadjustment() else {
        return false;
    };
    let Some(offset) = preview_offset_for_viewport(
        adj.lower(),
        adj.upper(),
        adj.page_size(),
        adj.value(),
        limit,
        state.preview_offset.get(),
    ) else {
        return false;
    };
    offset != state.preview_offset.replace(offset)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PreviewWindow {
    top: i64,
    upper: i64,
    rows: usize,
    offset: i64,
    max_offset: i64,
}

fn preview_window(lower: f64, upper: f64, limit: usize, offset: i64) -> Option<PreviewWindow> {
    if !lower.is_finite() || !upper.is_finite() || upper <= lower || limit == 0 {
        return None;
    }
    let lower = lower.floor() as i64;
    let upper = upper.ceil() as i64;
    let total = upper.saturating_sub(lower);
    let rows = total.min(limit as i64).max(1) as usize;
    let max_offset = total.saturating_sub(rows as i64);
    let offset = offset.clamp(0, max_offset);
    Some(PreviewWindow {
        top: upper.saturating_sub(rows as i64).saturating_sub(offset),
        upper,
        rows,
        offset,
        max_offset,
    })
}

fn preview_offset_for_viewport(
    lower: f64,
    upper: f64,
    page_size: f64,
    value: f64,
    limit: usize,
    offset: i64,
) -> Option<i64> {
    if !page_size.is_finite() || !value.is_finite() {
        return None;
    }
    let window = preview_window(lower, upper, limit, offset)?;
    let viewport_top = value.floor() as i64;
    let viewport_bottom = (value + page_size.max(1.0)).ceil() as i64;
    let preview_bottom = window.top.saturating_add(window.rows as i64);
    let top = if viewport_top < window.top {
        viewport_top
    } else if viewport_bottom > preview_bottom {
        viewport_bottom.saturating_sub(window.rows as i64)
    } else {
        window.top
    };
    let max_top = window.upper.saturating_sub(window.rows as i64);
    let top = top.clamp(lower.floor() as i64, max_top);
    Some(max_top.saturating_sub(top))
}

fn plain_text_pixel_rows(text: &str, columns: usize) -> Vec<Vec<VtePixelRun>> {
    let columns = columns.max(1);
    let mut rows: Vec<Vec<VtePixelRun>> = vec![Vec::new()];
    let mut column: usize = 0;
    for ch in text.chars() {
        if ch == '\n' {
            rows.push(Vec::new());
            column = 0;
            continue;
        }
        let width = terminal_cell_width(ch);
        if width == 0 {
            continue;
        }
        if column.saturating_add(width) > columns {
            rows.push(Vec::new());
            column = 0;
        }
        if !ch.is_whitespace() {
            let row = rows.last_mut().expect("rows always contains one entry");
            if row
                .last()
                .is_some_and(|previous| previous.column + previous.len == column)
            {
                row.last_mut().unwrap().len += width;
            } else {
                row.push(VtePixelRun {
                    column,
                    len: width,
                    color: None,
                });
            }
        }
        column += width;
    }
    rows
}

fn viewport_geometry(
    preview_top: i64,
    preview_rows: usize,
    page_size: f64,
    value: f64,
    height: f64,
) -> Option<(f64, f64, bool)> {
    if preview_rows == 0 || !page_size.is_finite() || !value.is_finite() || height <= 0.0 {
        return None;
    }
    let preview_height = (preview_rows as f64).min(height);
    let viewport_height = page_size.max(1.0).min(preview_height);
    let raw_top = value - preview_top as f64;
    let out_of_bounds = raw_top < 0.0 || raw_top + viewport_height > preview_height;
    Some((
        raw_top.clamp(0.0, preview_height - viewport_height),
        viewport_height,
        out_of_bounds,
    ))
}

fn pointer_target(
    lower: f64,
    upper: f64,
    page_size: f64,
    preview_top: i64,
    preview_rows: usize,
    y: f64,
    height: f64,
) -> Option<f64> {
    let total = upper - lower;
    if total <= page_size.max(0.0) || preview_rows == 0 || height <= 0.0 {
        return None;
    }
    let max_value = (upper - page_size).max(lower);
    let row = y.clamp(0.0, (preview_rows as f64).min(height));
    Some((preview_top as f64 + row - page_size / 2.0).clamp(lower, max_value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gtk::test]
    async fn refresh_waits_for_a_quiet_period() {
        let term = vte::Terminal::new();
        let area = gtk::DrawingArea::new();
        let state = Rc::new(MinimapState::default());
        state.enabled.set(true);
        let window = gtk::Window::new();
        window.set_child(Some(&area));
        window.present();
        gtk::glib::timeout_future(Duration::from_millis(50)).await;

        schedule_refresh(&term, &area, state.clone());
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        schedule_refresh(&term, &area, state.clone());
        gtk::glib::timeout_future(Duration::from_millis(75)).await;
        assert!(state.refresh_source.borrow().is_some());
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        assert!(state.refresh_source.borrow().is_none());
        window.close();
    }

    #[gtk::test]
    async fn viewport_shift_uses_debounced_refresh() {
        let term = vte::Terminal::new();
        let adjustment = gtk::Adjustment::new(0.0, -1_000.0, 24.0, 1.0, 24.0, 24.0);
        term.set_property("vadjustment", &adjustment);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&term));
        let minimap = TerminalMinimap::new(&term);
        overlay.add_overlay(minimap.widget());
        minimap.set_enabled(true);
        let window = gtk::Window::new();
        window.set_child(Some(&overlay));
        window.present();
        gtk::glib::timeout_future(Duration::from_millis(50)).await;

        adjustment.set_value(-400.0);
        assert!(minimap.state.refresh_source.borrow().is_some());
        minimap.set_enabled(false);
        window.close();
    }

    #[gtk::test]
    async fn hidden_minimap_defers_work_until_mapped() {
        let term = vte::Terminal::new();
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&term));
        let minimap = TerminalMinimap::new(&term);
        overlay.add_overlay(minimap.widget());
        minimap.set_enabled(true);

        term.feed(b"hidden output\n");
        gtk::glib::timeout_future(Duration::from_millis(150)).await;
        assert!(minimap.state.refresh_source.borrow().is_none());
        assert!(minimap.state.pixel_rows.borrow().is_empty());

        let window = gtk::Window::new();
        window.set_default_size(800, 600);
        window.set_child(Some(&overlay));
        window.present();
        gtk::glib::timeout_future(Duration::from_millis(50)).await;

        assert!(minimap.widget().is_mapped());
        assert!(!minimap.state.pixel_rows.borrow().is_empty());
        window.close();
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        assert!(minimap.state.pixel_rows.borrow().is_empty());
    }

    #[gtk::test]
    async fn wrapped_output_uses_one_minimap_row_per_terminal_row() {
        let term = vte::Terminal::new();
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

        let columns = term.column_count().max(1) as usize;
        term.feed("x".repeat(columns * 3 + 1).as_bytes());
        gtk::glib::timeout_future(Duration::from_millis(50)).await;
        refresh_now(&term, minimap.widget(), &minimap.state);

        assert_eq!(
            minimap
                .state
                .pixel_rows
                .borrow()
                .iter()
                .filter(|row| !row.is_empty())
                .count(),
            4
        );
        window.close();
    }

    #[gtk::test]
    async fn pixel_preview_survives_clear_scrollback_and_resize() {
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
        for index in 0..1_200 {
            output.push_str(&"x".repeat(index % 60 + 1));
            output.push_str("\r\n");
        }
        term.feed(output.as_bytes());
        window.set_default_size(400, 600);
        gtk::glib::timeout_future(Duration::from_millis(250)).await;
        refresh_now(&term, minimap.widget(), &minimap.state);

        let nonempty = minimap
            .state
            .pixel_rows
            .borrow()
            .iter()
            .filter(|row| !row.is_empty())
            .count();
        assert!(
            nonempty > 64,
            "scrollback preview returned {nonempty} nonempty rows after clearing history",
        );
        assert!(minimap.state.pixel_rows.borrow().len() <= minimap.widget().height() as usize);
        let adjustment = term.vadjustment().unwrap();
        let terminal_position = adjustment.value();
        assert!(scroll_preview(
            &term,
            minimap.widget(),
            &minimap.state,
            -1.0
        ));
        assert_eq!(adjustment.value(), terminal_position);
        assert_eq!(minimap.state.preview_offset.get(), PREVIEW_SCROLL_ROWS);
        window.close();
    }

    #[gtk::test]
    async fn alternate_screen_hides_minimap_and_blocks_navigation() {
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
        assert!(!minimap.widget().is_visible());
        scroll_to_pointer(&term, &minimap.state, 600.0, 600.0);
        assert_eq!(adjustment.value(), before);
        minimap.set_alternate_screen(false);
        assert!(minimap.widget().is_visible());
        window.close();
    }

    #[test]
    fn preview_window_follows_the_bottom_and_scrolls_locally() {
        assert_eq!(
            preview_window(-1_000.0, 24.0, 600, 0),
            Some(PreviewWindow {
                top: -576,
                upper: 24,
                rows: 600,
                offset: 0,
                max_offset: 424,
            })
        );
        assert_eq!(preview_window(-1_000.0, 24.0, 600, 25).unwrap().top, -601);
        assert_eq!(
            preview_window(-1_000.0, 24.0, 600, 999).unwrap().top,
            -1_000
        );
    }

    #[test]
    fn terminal_viewport_moves_the_preview_only_at_its_bounds() {
        assert_eq!(
            preview_offset_for_viewport(-1_000.0, 24.0, 24.0, -80.0, 600, 0),
            Some(0)
        );
        assert_eq!(
            preview_offset_for_viewport(-1_000.0, 24.0, 24.0, -800.0, 600, 0),
            Some(224)
        );
        assert_eq!(
            preview_offset_for_viewport(-1_000.0, 24.0, 24.0, 0.0, 600, 796,),
            Some(0)
        );
    }

    #[test]
    fn cursor_repaints_do_not_shift_the_text_range() {
        assert_eq!(advance_text_row_offset(0, 1_023, 24), 1_000);
        assert_eq!(advance_text_row_offset(100, 115, 24), 100);
        assert_eq!(advance_text_row_offset(100, 123, 24), 100);
        assert_eq!(advance_text_row_offset(100, 124, 24), 101);
    }

    #[test]
    fn plain_text_pixels_keep_spacing_and_wide_cells() {
        assert_eq!(
            plain_text_pixel_rows("  ab 한\n x", 80),
            vec![
                vec![
                    VtePixelRun {
                        column: 2,
                        len: 2,
                        color: None,
                    },
                    VtePixelRun {
                        column: 5,
                        len: 2,
                        color: None,
                    },
                ],
                vec![VtePixelRun {
                    column: 1,
                    len: 1,
                    color: None,
                }],
            ]
        );
    }

    #[test]
    fn viewport_and_pointer_mapping_are_clamped() {
        assert_eq!(
            viewport_geometry(-104, 128, 24.0, -104.0, 600.0),
            Some((0.0, 24.0, false))
        );
        assert_eq!(
            viewport_geometry(-104, 128, 24.0, 0.0, 600.0),
            Some((104.0, 24.0, false))
        );
        assert_eq!(
            viewport_geometry(-129, 128, 24.0, 0.0, 600.0),
            Some((104.0, 24.0, true))
        );
        assert_eq!(
            pointer_target(-1_000.0, 24.0, 24.0, -129, 128, 0.0, 600.0),
            Some(-141.0)
        );
        assert_eq!(
            pointer_target(-1_000.0, 24.0, 24.0, -129, 128, 600.0, 600.0),
            Some(-13.0)
        );
        assert_eq!(pointer_target(0.0, 20.0, 20.0, 0, 128, 10.0, 200.0), None);
    }
}
