// SPDX-License-Identifier: GPL-3.0-or-later
//! Theme tab for the options dialog.
//!
//! A list of built-in theme presets (from `flowmux-config`) with color
//! swatches, plus per-color override pickers. Every interaction updates
//! the shared [`ThemeSelection`] and calls `on_change` so the dialog can
//! persist and apply the look immediately.

use adw::prelude::*;
use flowmux_config::ghostty::GhosttyConfig;
use flowmux_config::options::ThemeOverrides;
use flowmux_config::presets::PRESETS;
use gtk::gdk;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use vte::prelude::*;

/// Theme selection shared with the options dialog.
#[derive(Clone, Default)]
pub struct ThemeSelection {
    /// Preset id, or `None` to follow the user's theme file (with built-in
    /// fallback colors when the file is absent).
    pub theme: Option<String>,
    pub overrides: ThemeOverrides,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Background,
    Foreground,
    Cursor,
    SelectionBackground,
    SelectionForeground,
}

const FIELDS: [(Field, &str); 5] = [
    (Field::Background, "Terminal background"),
    (Field::Foreground, "Terminal text"),
    (Field::Cursor, "Cursor"),
    (Field::SelectionBackground, "Selection background"),
    (Field::SelectionForeground, "Selection text"),
];

fn override_slot(overrides: &mut ThemeOverrides, field: Field) -> &mut Option<String> {
    match field {
        Field::Background => &mut overrides.background,
        Field::Foreground => &mut overrides.foreground,
        Field::Cursor => &mut overrides.cursor,
        Field::SelectionBackground => &mut overrides.selection_background,
        Field::SelectionForeground => &mut overrides.selection_foreground,
    }
}

fn override_value(overrides: &ThemeOverrides, field: Field) -> Option<String> {
    match field {
        Field::Background => overrides.background.clone(),
        Field::Foreground => overrides.foreground.clone(),
        Field::Cursor => overrides.cursor.clone(),
        Field::SelectionBackground => overrides.selection_background.clone(),
        Field::SelectionForeground => overrides.selection_foreground.clone(),
    }
}

/// The theme's own color for `field`, with the same fallbacks
/// `ResolvedTheme::from_ghostty` applies, so the override buttons open
/// showing what is actually on screen.
fn base_color(cfg: &GhosttyConfig, field: Field) -> String {
    let bg = cfg
        .background
        .clone()
        .unwrap_or_else(|| crate::theme::DEFAULT_BG.to_string());
    let fg = cfg
        .foreground
        .clone()
        .unwrap_or_else(|| crate::theme::DEFAULT_FG.to_string());
    match field {
        Field::Background => bg,
        Field::Foreground => fg,
        Field::Cursor => cfg.cursor_color.clone().unwrap_or(fg),
        Field::SelectionBackground => cfg.selection_background.clone().unwrap_or(bg),
        Field::SelectionForeground => cfg.selection_foreground.clone().unwrap_or(fg),
    }
}

/// Base config for the current selection: the preset when one is picked,
/// otherwise the user's theme file (the legacy source).
fn base_config(theme: Option<&str>) -> GhosttyConfig {
    match theme {
        Some(id) => flowmux_config::presets::config(id).unwrap_or_default(),
        None => flowmux_config::theme::load().unwrap_or_default(),
    }
}

fn parse_rgba(color: &str) -> gdk::RGBA {
    gdk::RGBA::parse(color).unwrap_or_else(|_| gdk::RGBA::new(0.0, 0.0, 0.0, 1.0))
}

/// Small rounded color chip used in the preset rows.
fn swatch(color: &str) -> gtk::Widget {
    let rgba = parse_rgba(color);
    let area = gtk::DrawingArea::new();
    area.set_content_width(14);
    area.set_content_height(14);
    area.set_valign(gtk::Align::Center);
    area.set_draw_func(move |_, cr, w, h| {
        cr.set_source_rgba(
            rgba.red() as f64,
            rgba.green() as f64,
            rgba.blue() as f64,
            1.0,
        );
        cr.rectangle(0.0, 0.0, w as f64, h as f64);
        let _ = cr.fill();
    });
    area.upcast()
}

/// bg, fg, and ANSI colors 1..=6 — enough to recognize a scheme at a glance.
fn swatch_colors(cfg: &GhosttyConfig) -> Vec<String> {
    let mut colors = vec![
        base_color(cfg, Field::Background),
        base_color(cfg, Field::Foreground),
    ];
    for i in 1..=6 {
        colors.push(
            cfg.palette[i]
                .clone()
                .unwrap_or_else(|| crate::theme::DEFAULT_PALETTE[i].to_string()),
        );
    }
    colors
}

fn update_preview(preview: &vte::Terminal, cfg: &GhosttyConfig) {
    let theme = crate::theme::ResolvedTheme::from_ghostty(cfg);
    preview.set_colors(
        Some(&theme.fg),
        Some(&theme.bg),
        &theme.palette.iter().collect::<Vec<_>>(),
    );
    let mut font = theme.font.clone();
    font.set_size(11 * gtk::pango::SCALE);
    preview.set_font(Some(&font));
    // The preview has no PTY and never executes commands. Use the real ANSI
    // renderer, including explicit selection colors, for representative output.
    let rgb = |color: gdk::RGBA| {
        format!(
            "{};{};{}",
            (color.red() * 255.0).round() as u8,
            (color.green() * 255.0).round() as u8,
            (color.blue() * 255.0).round() as u8
        )
    };
    let selection_bg = theme.selection_bg.unwrap_or(theme.fg);
    let selection_fg = theme
        .selection_fg
        .unwrap_or(if theme.selection_bg.is_some() {
            theme.fg
        } else {
            theme.bg
        });
    preview.feed(format!(
        "\x1b[0m\x1b[2J\x1b[H\x1b[?25l$ cargo test\r\n\x1b[32mPASS\x1b[0m  12 tests passed\r\n\x1b[31merror:\x1b[0m example diagnostic\r\n\x1b[48;2;{}m\x1b[38;2;{}m Selected text \x1b[0m  Cursor \x1b[48;2;{}m \x1b[0m\r\nfn main() {{ println!(\"Hello\"); }}",
        rgb(selection_bg), rgb(selection_fg), rgb(theme.cursor),
    ).as_bytes());
}

pub fn build(state: Rc<RefCell<ThemeSelection>>, on_change: Rc<dyn Fn()>) -> gtk::Widget {
    // Suppresses change handlers while widgets are being seeded
    // programmatically (initial selection, reseeding after a preset click).
    let syncing = Rc::new(Cell::new(false));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_top(16);
    body.set_margin_bottom(16);
    body.set_margin_start(20);
    body.set_margin_end(20);

    let preview_heading = gtk::Label::new(Some("Preview"));
    preview_heading.set_xalign(0.0);
    preview_heading.add_css_class("heading");
    body.append(&preview_heading);
    let preview = vte::Terminal::new();
    preview.set_widget_name("flowmux-theme-preview");
    preview.add_css_class("flowmux-theme-preview");
    preview.set_input_enabled(false);
    preview.set_focusable(false);
    preview.set_scrollback_lines(0);
    preview.set_size(44, 6);
    preview.set_height_request(120);
    preview.set_vexpand(false);
    let preview_frame = gtk::Frame::new(None);
    preview_frame.set_child(Some(&preview));
    body.append(&preview_frame);

    let list = gtk::ListBox::new();
    list.set_widget_name("flowmux-theme-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    let file_cfg = flowmux_config::theme::load();
    let file_label = if file_cfg.is_some() {
        "User theme file"
    } else {
        "User theme file (not found; using defaults)"
    };
    let choices = std::iter::once((file_label, file_cfg.unwrap_or_default())).chain(
        PRESETS.iter().map(|preset| {
            (
                preset.name,
                flowmux_config::presets::config(preset.id).unwrap_or_default(),
            )
        }),
    );
    for (name, cfg) in choices {
        let row_box = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        row_box.set_margin_top(8);
        row_box.set_margin_bottom(8);
        row_box.set_margin_start(10);
        row_box.set_margin_end(10);
        let name = gtk::Label::new(Some(name));
        name.set_xalign(0.0);
        name.set_hexpand(true);
        row_box.append(&name);
        let tone = gtk::Label::new(Some(
            if crate::theme::ResolvedTheme::from_ghostty(&cfg).is_dark() {
                "Dark"
            } else {
                "Light"
            },
        ));
        tone.add_css_class("dim-label");
        tone.set_margin_end(8);
        row_box.append(&tone);
        for color in swatch_colors(&cfg) {
            row_box.append(&swatch(&color));
        }
        let row = gtk::ListBoxRow::new();
        row.set_child(Some(&row_box));
        list.append(&row);
    }
    body.append(&list);

    let overrides_heading = gtk::Label::new(Some("Custom colors"));
    overrides_heading.set_xalign(0.0);
    overrides_heading.add_css_class("heading");
    overrides_heading.set_margin_top(8);
    body.append(&overrides_heading);

    let override_status = gtk::Label::new(None);
    override_status.set_widget_name("flowmux-theme-override-status");
    override_status.set_xalign(0.0);
    override_status.set_wrap(true);
    override_status.add_css_class("dim-label");
    body.append(&override_status);

    let override_buttons: Rc<Vec<(Field, gtk::ColorDialogButton, gtk::Button)>> = Rc::new(
        FIELDS
            .iter()
            .enumerate()
            .map(|(index, (field, label))| {
                let color_dialog = gtk::ColorDialog::new();
                color_dialog.set_with_alpha(false);
                let button = gtk::ColorDialogButton::new(Some(color_dialog));
                button.set_widget_name(&format!("flowmux-theme-color-{index}"));
                let reset = gtk::Button::with_label("Reset");
                reset.set_widget_name(&format!("flowmux-theme-reset-{index}"));
                reset.set_tooltip_text(Some(&format!(
                    "Use theme color for {}",
                    label.to_lowercase()
                )));
                let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                controls.append(&button);
                controls.append(&reset);
                body.append(&crate::ui::options_dialog::row(label, &controls));
                (*field, button, reset)
            })
            .collect(),
    );

    let reset_btn = gtk::Button::with_label("Reset custom colors");
    reset_btn.set_widget_name("flowmux-theme-reset-all");
    reset_btn.set_halign(gtk::Align::Start);
    body.append(&reset_btn);

    let seed_buttons = {
        let state = state.clone();
        // Callbacks own this closure, so it must not keep their buttons alive.
        let buttons: Vec<_> = override_buttons
            .iter()
            .map(|(field, button, reset)| (*field, button.downgrade(), reset.downgrade()))
            .collect();
        let syncing = syncing.clone();
        let reset_btn = reset_btn.downgrade();
        Rc::new(move || {
            let selection = state.borrow();
            let mut cfg = base_config(selection.theme.as_deref());
            cfg.merge(selection.overrides.to_ghostty());
            update_preview(&preview, &cfg);
            syncing.set(true);
            let mut custom_count = 0;
            for (field, button, reset) in buttons.iter() {
                let Some((button, reset)) = button.upgrade().zip(reset.upgrade()) else {
                    continue;
                };
                let custom = override_value(&selection.overrides, *field).is_some();
                custom_count += usize::from(custom);
                reset.set_sensitive(custom);
                button.set_tooltip_text(Some(if custom {
                    "Custom color"
                } else {
                    "Inherited from theme"
                }));
                let color = base_color(&cfg, *field);
                button.set_rgba(&parse_rgba(&color));
            }
            if let Some(reset_btn) = reset_btn.upgrade() {
                reset_btn.set_sensitive(custom_count > 0);
            }
            override_status.set_text(&if custom_count == 0 {
                "Using theme colors".into()
            } else {
                format!("Custom colors active: {custom_count}. Kept when switching themes.")
            });
            syncing.set(false);
        })
    };
    seed_buttons();

    // The first row follows the user file; Default is a distinct preset.
    // Seed before connecting signals so opening Options never changes the source.
    let initial_index = state
        .borrow()
        .theme
        .as_deref()
        .map(|id| {
            PRESETS
                .iter()
                .position(|preset| preset.id == id)
                .unwrap_or(0)
                + 1
        })
        .unwrap_or(0);
    syncing.set(true);
    list.select_row(list.row_at_index(initial_index as i32).as_ref());
    syncing.set(false);

    {
        let state = state.clone();
        let on_change = on_change.clone();
        let seed_buttons = seed_buttons.clone();
        let syncing = syncing.clone();
        list.connect_row_selected(move |_, row| {
            if syncing.get() {
                return;
            }
            let Some(row) = row else {
                return;
            };
            let theme = if row.index() == 0 {
                None
            } else {
                PRESETS
                    .get((row.index() - 1) as usize)
                    .map(|preset| preset.id.to_string())
            };
            if state.borrow().theme == theme {
                return;
            }
            state.borrow_mut().theme = theme;
            seed_buttons();
            on_change();
        });
    }

    for (field, button, reset) in override_buttons.iter() {
        let field = *field;
        {
            let state = state.clone();
            let on_change = on_change.clone();
            let seed_buttons = seed_buttons.clone();
            reset.connect_clicked(move |_| {
                *override_slot(&mut state.borrow_mut().overrides, field) = None;
                seed_buttons();
                on_change();
            });
        }
        let state = state.clone();
        let on_change = on_change.clone();
        let syncing = syncing.clone();
        let seed_buttons = seed_buttons.clone();
        button.connect_rgba_notify(move |button| {
            if syncing.get() {
                return;
            }
            let rgba = button.rgba();
            let hex = format!(
                "#{:02x}{:02x}{:02x}",
                (rgba.red().clamp(0.0, 1.0) * 255.0).round() as u8,
                (rgba.green().clamp(0.0, 1.0) * 255.0).round() as u8,
                (rgba.blue().clamp(0.0, 1.0) * 255.0).round() as u8,
            );
            *override_slot(&mut state.borrow_mut().overrides, field) = Some(hex);
            seed_buttons();
            on_change();
        });
    }

    {
        let state = state.clone();
        let on_change = on_change.clone();
        let seed_buttons = seed_buttons.clone();
        reset_btn.connect_clicked(move |_| {
            state.borrow_mut().overrides = ThemeOverrides::default();
            seed_buttons();
            on_change();
        });
    }

    let scroller = gtk::ScrolledWindow::new();
    scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
    scroller.set_vexpand(true);
    scroller.set_child(Some(&body));
    scroller.upcast()
}
