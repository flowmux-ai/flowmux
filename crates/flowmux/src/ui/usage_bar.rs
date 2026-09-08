// SPDX-License-Identifier: GPL-3.0-or-later

use crate::usage::{ProviderState, UsagePanelState};
use gtk::prelude::*;

/// Persistent widgets: refreshing changes values without rebuilding the footer.
pub(crate) struct UsageBar {
    pub(crate) root: gtk::Box,
    icons: [gtk::Image; 2],
    separator: gtk::Box,
    providers: [gtk::Box; 2],
    meters: [[Meter; 2]; 2],
}

struct Meter {
    root: gtk::Box,
    progress: gtk::ProgressBar,
    percent: gtk::Label,
}

impl UsageBar {
    pub(crate) fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.set_widget_name("flowmux-usage-bar");
        root.add_css_class("flowmux-usage-bar");
        root.set_visible(false);
        let separator = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let providers = std::array::from_fn(|_| gtk::Box::new(gtk::Orientation::Horizontal, 8));
        let icons = ["claude", "codex"].map(super::agent_icon);
        let meters = std::array::from_fn(|provider| {
            if provider == 1 {
                root.append(&separator);
            }
            root.append(&providers[provider]);
            providers[provider].append(&icons[provider]);
            std::array::from_fn(|period| {
                let meter = Meter {
                    root: gtk::Box::new(gtk::Orientation::Horizontal, 4),
                    progress: gtk::ProgressBar::new(),
                    percent: gtk::Label::new(None),
                };
                let name = ["Claude", "Codex"][provider];
                let duration = ["5h", "1W"][period];
                meter
                    .root
                    .set_tooltip_text(Some(&format!("{name} {duration} usage")));
                meter
                    .progress
                    .update_property(&[gtk::accessible::Property::Label(&format!(
                        "{name} {duration} usage"
                    ))]);
                meter.progress.add_css_class(["claude", "codex"][provider]);
                meter.progress.set_valign(gtk::Align::Center);
                meter.progress.set_width_request(95);
                meter.percent.add_css_class("numeric");
                // Keep the two Claude meters closer without changing provider spacing.
                meter
                    .percent
                    .set_width_chars(if provider == 0 && period == 0 { 8 } else { 9 });
                meter.percent.set_xalign(0.0);
                meter.root.append(&meter.progress);
                meter.root.append(&meter.percent);
                providers[provider].append(&meter.root);
                meter
            })
        });
        Self {
            root,
            icons,
            separator,
            providers,
            meters,
        }
    }

    pub(crate) fn set_font(&self, font: &gtk::pango::FontDescription) {
        let metrics = self.root.pango_context().metrics(Some(font), None);
        let icon_size = (gtk::pango::units_to_double(metrics.height()).ceil() * 0.8).round() as i32;
        let mut font = font.clone();
        font.set_size((font.size() as f64 * 0.8).round() as i32);
        let attributes = gtk::pango::AttrList::new();
        attributes.insert(gtk::pango::AttrFontDesc::new(&font));
        for icon in &self.icons {
            icon.set_pixel_size(icon_size);
        }
        // Preserve the old separator's space without displaying a character.
        let separator_layout = self.root.create_pango_layout(Some("|"));
        separator_layout.set_font_description(Some(&font));
        self.separator
            .set_width_request(separator_layout.pixel_size().0);
        for meter in self.meters.iter().flatten() {
            meter.percent.set_attributes(Some(&attributes));
        }
    }

    pub(crate) fn render(&self, state: &UsagePanelState, enabled: bool) {
        let mut visible = [false; 2];
        for (provider, state) in [&state.claude, &state.codex].into_iter().enumerate() {
            for (period, duration) in [300, 10_080].into_iter().enumerate() {
                let value = window_percent(state, duration);
                let meter = &self.meters[provider][period];
                if let Some(value) = value {
                    meter.progress.set_fraction((value / 100.0).clamp(0.0, 1.0));
                    meter
                        .percent
                        .set_text(&format!("{value:.0}%({})", ["5h", "1W"][period]));
                }
                meter.root.set_visible(value.is_some());
                visible[provider] |= value.is_some();
            }
            self.providers[provider].set_visible(visible[provider]);
        }
        self.separator.set_visible(visible[0] && visible[1]);
        self.root
            .set_visible(enabled && visible.into_iter().any(|value| value));
    }
}

fn window_percent(state: &ProviderState, duration: u64) -> Option<f64> {
    if state.limits_error.is_some() {
        return None;
    }
    // Some plans return several scoped limits. Show the highest utilization
    // for each period so the compact bar doesn't understate a reached limit.
    state
        .limits
        .as_ref()?
        .value
        .iter()
        .filter(|window| window.duration_minutes == Some(duration))
        .map(|window| window.used_percent)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .max_by(f64::total_cmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{FieldRefresh, Provider, ProviderRefresh, UsageError, UsageWindow};
    use chrono::Utc;

    fn refresh(provider: Provider, values: &[(u64, f64)]) -> ProviderRefresh {
        ProviderRefresh {
            provider,
            tokens: FieldRefresh::Failure(UsageError::network()),
            limits: FieldRefresh::Success(
                values
                    .iter()
                    .map(|&(duration, percent)| UsageWindow {
                        label: String::new(),
                        scope: None,
                        used_percent: percent,
                        duration_minutes: Some(duration),
                        resets_at: None,
                    })
                    .collect(),
            ),
            collected_at: Utc::now(),
        }
    }

    #[test]
    fn periods_use_maximum_valid_usage_and_reject_unavailable_limits() {
        let mut state = UsagePanelState::default();
        assert_eq!(window_percent(&state.claude, 300), None);
        state.apply(refresh(
            Provider::Claude,
            &[
                (300, 0.0),
                (300, -1.0),
                (300, f64::NAN),
                (300, f64::INFINITY),
                (60, 90.0),
                (10_080, 23.0),
            ],
        ));
        assert_eq!(window_percent(&state.claude, 300), Some(0.0));
        assert_eq!(window_percent(&state.claude, 10_080), Some(23.0));
        state.apply(refresh(Provider::Claude, &[(300, -1.0), (300, f64::NAN)]));
        assert_eq!(window_percent(&state.claude, 300), None);
        state.apply(refresh(Provider::Claude, &[(300, 20.0), (300, 120.0)]));
        assert_eq!(window_percent(&state.claude, 300), Some(120.0));
        state.claude.limits_error = Some(UsageError::network());
        assert_eq!(window_percent(&state.claude, 300), None);
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn footer_updates_in_place_and_hides_only_unavailable_periods() {
        let bar = UsageBar::new();
        let mut state = UsagePanelState::default();
        bar.render(&state, true);
        assert!(!bar.root.is_visible());
        state.apply(refresh(Provider::Claude, &[(300, 22.0), (10_080, 35.0)]));
        state.apply(refresh(Provider::Codex, &[(300, 44.0), (10_080, 65.0)]));
        bar.render(&state, true);
        assert!(bar.root.is_visible());
        assert!(bar.separator.is_visible());
        assert!(
            bar.separator.first_child().is_none(),
            "provider spacing contains no text"
        );
        assert!(bar.meters[0][0].percent.width_chars() < bar.meters[1][0].percent.width_chars());
        assert!(bar.meters[0][0].progress.has_css_class("claude"));
        assert!(bar.meters[1][0].progress.has_css_class("codex"));
        assert_eq!(bar.meters[1][1].percent.text(), "65%(1W)");
        assert_eq!(
            bar.root.first_child().as_ref(),
            Some(bar.providers[0].upcast_ref())
        );
        for (provider, name) in ["claude", "codex"].into_iter().enumerate() {
            assert_eq!(
                bar.providers[provider].first_child().as_ref(),
                Some(bar.icons[provider].upcast_ref())
            );
            assert_eq!(
                bar.icons[provider].icon_name().as_deref(),
                Some(crate::builtin_icons::agent_icon_name(name))
            );
            assert_eq!(
                bar.icons[provider].next_sibling().as_ref(),
                Some(bar.meters[provider][0].root.upcast_ref())
            );
        }
        let first = bar.meters[0][0].root.clone();
        let visibility_changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let changes = visibility_changes.clone();
        bar.root
            .connect_visible_notify(move |_| changes.set(changes.get() + 1));
        state.begin_forced_refresh();
        bar.render(&state, true);
        assert_eq!(bar.meters[0][0].progress.fraction(), 0.22);
        state.apply(refresh(Provider::Claude, &[(300, 51.0), (10_080, 35.0)]));
        state.finish_refresh(Utc::now());
        bar.render(&state, true);
        assert_eq!(
            bar.icons[0].next_sibling().as_ref(),
            Some(first.upcast_ref())
        );
        assert_eq!(bar.meters[0][0].progress.fraction(), 0.51);
        assert_eq!(
            visibility_changes.get(),
            0,
            "successful refresh must not hide the footer"
        );
        state.claude.limits_error = Some(UsageError::network());
        bar.render(&state, true);
        assert!(!bar.providers[0].is_visible());
        assert!(bar.providers[1].is_visible());
        assert!(!bar.separator.is_visible());
        state.apply(refresh(Provider::Codex, &[(300, 0.0), (10_080, 65.0)]));
        bar.render(&state, true);
        assert!(bar.meters[1][0].root.is_visible());
        assert_eq!(bar.meters[1][0].progress.fraction(), 0.0);
        assert_eq!(bar.meters[1][0].percent.text(), "0%(5h)");
        assert!(bar.meters[1][1].root.is_visible());
        assert!(bar.icons[1].is_visible());
        bar.render(&state, false);
        assert!(!bar.root.is_visible());
        state.codex.limits_error = Some(UsageError::network());
        bar.render(&state, true);
        assert!(!bar.root.is_visible());
        state.apply(refresh(Provider::Claude, &[(300, 120.0)]));
        bar.render(&state, true);
        assert!(bar.root.is_visible());
        assert_eq!(bar.meters[0][0].progress.fraction(), 1.0);
        assert_eq!(bar.meters[0][0].percent.text(), "120%(5h)");
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn footer_font_tracks_global_terminal_font() {
        let bar = UsageBar::new();
        bar.set_font(&gtk::pango::FontDescription::from_string("Monospace 12"));
        let small_icon = bar.icons[0].pixel_size();
        let font = gtk::pango::FontDescription::from_string("Monospace 18");
        bar.set_font(&font);
        assert!(bar.icons[0].pixel_size() > small_icon);
        assert_eq!(bar.icons[0].pixel_size(), bar.icons[1].pixel_size());
        let original_label = gtk::Label::new(Some("100%(5h)"));
        let original_attributes = gtk::pango::AttrList::new();
        original_attributes.insert(gtk::pango::AttrFontDesc::new(&font));
        original_label.set_attributes(Some(&original_attributes));
        let original_height = original_label.measure(gtk::Orientation::Vertical, -1).1;
        assert_eq!(
            bar.icons[0].pixel_size(),
            (original_height as f64 * 0.8).round() as i32
        );
        assert_eq!(
            bar.meters[0][0].percent.attributes().unwrap().to_string(),
            bar.meters[1][1].percent.attributes().unwrap().to_string()
        );
        let rendered_font = bar.meters[0][0]
            .percent
            .attributes()
            .unwrap()
            .iterator()
            .font()
            .0;
        let rendered_points = gtk::pango::units_to_double(rendered_font.size());
        assert!((rendered_points - 14.4).abs() < 0.001);
        assert_eq!(rendered_font.family().as_deref(), Some("Monospace"));
        assert_eq!(
            font.size(),
            18 * gtk::pango::SCALE,
            "the terminal font stays unchanged"
        );
    }
}
