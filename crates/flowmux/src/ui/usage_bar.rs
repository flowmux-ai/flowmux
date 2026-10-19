// SPDX-License-Identifier: GPL-3.0-or-later

use crate::usage::{Provider, ProviderState, UsagePanelState, UsageWindow};
use flowmux_config::options::{UsageBarPin, UsageBarPins};
use gtk::prelude::*;
use std::cell::RefCell;

/// Persistent widgets: refreshing changes values without rebuilding the footer.
pub(crate) struct UsageBar {
    pub(crate) root: gtk::Box,
    icons: [gtk::Image; 2],
    separator: gtk::Box,
    providers: [gtk::Box; 2],
    meters: [RefCell<Vec<Meter>>; 2],
    pins: RefCell<UsageBarPins>,
}

struct Meter {
    root: gtk::Box,
    progress: gtk::ProgressBar,
    percent: gtk::Label,
}

impl Meter {
    fn new(provider: usize, period: usize) -> Self {
        let meter = Self {
            root: gtk::Box::new(gtk::Orientation::Horizontal, 4),
            progress: gtk::ProgressBar::new(),
            percent: gtk::Label::new(None),
        };
        meter.progress.add_css_class(["claude", "codex"][provider]);
        meter.progress.set_valign(gtk::Align::Center);
        meter.progress.set_width_request(95);
        meter.percent.add_css_class("numeric");
        meter
            .percent
            .set_width_chars(if provider == 0 && period == 0 { 8 } else { 9 });
        meter.percent.set_xalign(0.0);
        meter.root.append(&meter.progress);
        meter.root.append(&meter.percent);
        meter
    }
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
            RefCell::new(
                (0..2)
                    .map(|period| {
                        let meter = Meter::new(provider, period);
                        providers[provider].append(&meter.root);
                        meter
                    })
                    .collect(),
            )
        });
        Self {
            root,
            icons,
            separator,
            providers,
            meters,
            pins: RefCell::new(UsageBarPins::default()),
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
        for meters in &self.meters {
            for meter in meters.borrow().iter() {
                meter.percent.set_attributes(Some(&attributes));
            }
        }
    }

    pub(crate) fn pins(&self) -> UsageBarPins {
        self.pins.borrow().clone()
    }

    pub(crate) fn set_pins(&self, pins: UsageBarPins) {
        *self.pins.borrow_mut() = pins;
    }

    pub(crate) fn is_pinned(&self, state: &ProviderState, window: &UsageWindow) -> bool {
        effective_pins(state, &self.pins.borrow()).contains(&window_pin(window))
    }

    pub(crate) fn toggle_pin(&self, state: &ProviderState, window: &UsageWindow, pinned: bool) {
        let mut pins = self.pins.borrow_mut();
        let mut selected = effective_pins(state, &pins);
        let key = window_pin(window);
        selected.retain(|entry| entry != &key);
        if pinned {
            selected.push(key);
        }
        match state.provider {
            Provider::Claude => pins.claude = Some(selected),
            Provider::Codex => pins.codex = Some(selected),
        }
    }

    pub(crate) fn render(&self, state: &UsagePanelState, enabled: bool) {
        let mut visible = [false; 2];
        for (provider, state) in [&state.claude, &state.codex].into_iter().enumerate() {
            let mut slots = [(300, "5h"), (10_080, "1W")].map(|(duration, label)| {
                window_percent(state, duration).map(|value| (label, label.to_owned(), value))
            });
            // Credit-based and enterprise plans report no 5h/1W window at all:
            // Claude may send "Extra usage", a seat key, or only "Spend", and
            // Codex its seat allowance under the limit's own name. Show the
            // provider's credit balance, or else its highest limit of any kind,
            // in the first slot so the bar still reports usage on such plans.
            if slots.iter().all(Option::is_none) {
                slots[0] = scope_percent(state, CREDIT_SCOPES[provider])
                    .map(|value| {
                        (
                            CREDIT_LABELS[provider],
                            CREDIT_LABELS[provider].to_owned(),
                            value,
                        )
                    })
                    .or_else(|| any_limit(state).map(|(name, value)| ("Limit", name, value)));
            }
            let pins = self.pins.borrow();
            let slots: Vec<_> = match provider_pins(state.provider, &pins) {
                None => slots.into_iter().collect(),
                Some(selected) => state
                    .limits
                    .iter()
                    .flat_map(|limits| &limits.value)
                    .filter(|window| {
                        valid_percent(window) && selected.contains(&window_pin(window))
                    })
                    .map(|window| {
                        let label = match window.duration_minutes {
                            Some(300) => "5h",
                            Some(10_080) => "1W",
                            _ if window.scope.as_deref() == Some(CREDIT_SCOPES[provider]) => {
                                CREDIT_LABELS[provider]
                            }
                            _ => "Limit",
                        };
                        let name = match &window.scope {
                            Some(scope) if label == "5h" || label == "1W" => {
                                format!("{scope} {label}")
                            }
                            Some(scope) if label == "Limit" => match window.duration_minutes {
                                Some(_) => format!("{scope} {}", window.label),
                                None => scope.clone(),
                            },
                            _ if label == "Limit" => window.label.clone(),
                            _ => label.to_owned(),
                        };
                        Some((label, name, window.used_percent))
                    })
                    .collect(),
            };
            let mut meters = self.meters[provider].borrow_mut();
            while meters.len() < slots.len() {
                let meter = Meter::new(provider, meters.len());
                meter
                    .percent
                    .set_attributes(meters[0].percent.attributes().as_ref());
                self.providers[provider].append(&meter.root);
                meters.push(meter);
            }
            for (period, meter) in meters.iter().enumerate() {
                let slot = slots.get(period).and_then(Option::as_ref);
                if let Some((label, long_label, value)) = &slot {
                    let (label, value) = (*label, *value);
                    let name = ["Claude", "Codex"][provider];
                    // Values kept from an earlier collection say so on hover,
                    // since the bar itself has no room for a staleness marker.
                    let stale = if state.limits_error.is_some() {
                        " (last known)"
                    } else {
                        ""
                    };
                    let description = format!("{name} {long_label} usage{stale}");
                    meter.root.set_tooltip_text(Some(&description));
                    meter
                        .progress
                        .update_property(&[gtk::accessible::Property::Label(&description)]);
                    meter.progress.set_fraction((value / 100.0).clamp(0.0, 1.0));
                    meter.percent.set_text(&format!("{value:.0}%({label})"));
                }
                meter.root.set_visible(slot.is_some());
                visible[provider] |= slot.is_some();
            }
            self.providers[provider].set_visible(visible[provider]);
        }
        self.separator.set_visible(visible[0] && visible[1]);
        self.root
            .set_visible(enabled && visible.into_iter().any(|value| value));
    }
}

fn window_pin(window: &UsageWindow) -> UsageBarPin {
    UsageBarPin {
        label: window.label.clone(),
        scope: window.scope.clone(),
        duration_minutes: window.duration_minutes,
    }
}

fn provider_pins(provider: Provider, pins: &UsageBarPins) -> Option<&Vec<UsageBarPin>> {
    match provider {
        Provider::Claude => pins.claude.as_ref(),
        Provider::Codex => pins.codex.as_ref(),
    }
}

fn valid_percent(window: &UsageWindow) -> bool {
    window.used_percent.is_finite() && window.used_percent >= 0.0
}

fn effective_pins(state: &ProviderState, pins: &UsageBarPins) -> Vec<UsageBarPin> {
    if let Some(selected) = provider_pins(state.provider, pins) {
        return selected.clone();
    }
    let Some(limits) = &state.limits else {
        return Vec::new();
    };
    let highest = |matches: &dyn Fn(&UsageWindow) -> bool| {
        let value = max_percent(state, matches)?;
        limits
            .value
            .iter()
            .find(|window| matches(window) && window.used_percent == value)
    };
    let mut selected: Vec<_> = [300, 10_080]
        .into_iter()
        .filter_map(|duration| highest(&|window| window.duration_minutes == Some(duration)))
        .map(window_pin)
        .collect();
    if selected.is_empty() {
        let scope = match state.provider {
            Provider::Claude => CREDIT_SCOPES[0],
            Provider::Codex => CREDIT_SCOPES[1],
        };
        if let Some(window) =
            highest(&|window| window.scope.as_deref() == Some(scope)).or_else(|| highest(&|_| true))
        {
            selected.push(window_pin(window));
        }
    }
    selected
}

/// The only limit a credit-based plan reports, per provider, and the short
/// label the bar shows for it.
const CREDIT_SCOPES: [&str; 2] = ["Extra usage", "Individual"];
const CREDIT_LABELS: [&str; 2] = ["Extra", "Individual"];

fn window_percent(state: &ProviderState, duration: u64) -> Option<f64> {
    max_percent(state, |window| window.duration_minutes == Some(duration))
}

fn scope_percent(state: &ProviderState, scope: &str) -> Option<f64> {
    max_percent(state, |window| window.scope.as_deref() == Some(scope))
}

/// The highest limit of any scope or period, with the name the popover uses.
/// The bar only has room for a short fixed label, so the name goes in the
/// tooltip.
fn any_limit(state: &ProviderState) -> Option<(String, f64)> {
    let value = max_percent(state, |_| true)?;
    let window = state
        .limits
        .as_ref()?
        .value
        .iter()
        .find(|window| window.used_percent == value)?;
    let name = match (&window.scope, window.duration_minutes) {
        (Some(scope), Some(_)) => format!("{scope} {}", window.label),
        (Some(scope), None) => scope.clone(),
        (None, _) => window.label.clone(),
    };
    Some((name, value))
}

/// A failed refresh keeps whatever was collected last — the endpoint rate
/// limits often enough that hiding the bar on a single failure would make it
/// blink out while the numbers are still fresh enough to act on. Only a
/// provider that never reported anything has no slots to show.
fn max_percent(state: &ProviderState, matches: impl Fn(&UsageWindow) -> bool) -> Option<f64> {
    // Some plans return several scoped limits. Show the highest utilization
    // for each period so the compact bar doesn't understate a reached limit.
    state
        .limits
        .as_ref()?
        .value
        .iter()
        .filter(|window| matches(window))
        .map(|window| window.used_percent)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .max_by(f64::total_cmp)
}

#[cfg(test)]
pub(crate) mod tests {
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

    fn credit_refresh(provider: Provider, scope: &str, percent: f64) -> ProviderRefresh {
        ProviderRefresh {
            provider,
            tokens: FieldRefresh::Failure(UsageError::network()),
            limits: FieldRefresh::Success(vec![UsageWindow {
                label: String::new(),
                scope: Some(scope.to_owned()),
                used_percent: percent,
                duration_minutes: None,
                resets_at: None,
            }]),
            collected_at: Utc::now(),
        }
    }

    #[test]
    fn automatic_pins_follow_existing_period_and_credit_fallbacks() {
        let mut state = UsagePanelState::default();
        let pins = UsageBarPins::default();
        assert!(effective_pins(&state.claude, &pins).is_empty());
        state.apply(refresh(
            Provider::Claude,
            &[(300, 20.0), (300, 80.0), (10_080, 30.0), (60, 99.0)],
        ));
        state.claude.limits.as_mut().unwrap().value[1].scope = Some("Team".into());
        let selected = effective_pins(&state.claude, &pins);
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].scope.as_deref(), Some("Team"));
        assert_eq!(selected[1].duration_minutes, Some(10_080));
        state.apply(credit_refresh(Provider::Claude, "Extra usage", 42.0));
        assert_eq!(
            effective_pins(&state.claude, &pins)[0].scope.as_deref(),
            Some("Extra usage")
        );
        state.apply(credit_refresh(Provider::Codex, "Team pool", 90.0));
        assert_eq!(
            effective_pins(&state.codex, &pins)[0].scope.as_deref(),
            Some("Team pool")
        );
    }

    #[cfg_attr(not(target_os = "macos"), gtk::test)]
    pub(crate) fn pins_preserve_scopes_values_fonts_and_survive_missing_data() {
        let bar = UsageBar::new();
        bar.set_font(&gtk::pango::FontDescription::from_string("Monospace 18"));
        let mut state = UsagePanelState::default();
        state.apply(refresh(
            Provider::Claude,
            &[(300, 22.0), (10_080, 35.0), (10_080, 15.0)],
        ));
        state.claude.limits.as_mut().unwrap().value[2].scope = Some("Sonnet".into());
        state.apply(refresh(Provider::Codex, &[(300, 44.0), (10_080, 65.0)]));
        let windows = state.claude.limits.as_ref().unwrap().value.clone();
        bar.render(&state, true);
        let first = bar.meters[0].borrow()[0].root.clone();
        assert!(!bar.is_pinned(&state.claude, &windows[2]));
        bar.toggle_pin(&state.claude, &windows[2], true);
        let saved = bar.pins();
        assert!(
            saved.codex.is_none(),
            "untouched providers keep automatic selection"
        );
        bar.render(&state, true);
        {
            let meters = bar.meters[0].borrow();
            assert_eq!(meters.len(), 3);
            assert_eq!(meters[0].root, first);
            assert_eq!(meters[0].percent.text(), "22%(5h)");
            assert_eq!(meters[1].percent.text(), "35%(1W)");
            assert_eq!(meters[2].percent.text(), "15%(1W)");
            assert_eq!(
                meters[2].percent.attributes(),
                meters[0].percent.attributes()
            );
        }
        assert_eq!(bar.meters[1].borrow()[1].percent.text(), "65%(1W)");
        let updated = &mut state.claude.limits.as_mut().unwrap().value[2];
        updated.used_percent = 46.0;
        updated.resets_at = Some(Utc::now());
        state.claude.limits_error = Some(UsageError::network());
        bar.render(&state, true);
        assert_eq!(bar.meters[0].borrow()[2].percent.text(), "46%(1W)");
        assert_eq!(
            bar.meters[0].borrow()[2].root.tooltip_text().as_deref(),
            Some("Claude Sonnet 1W usage (last known)")
        );
        state.claude.limits = None;
        bar.render(&state, true);
        assert!(!bar.providers[0].is_visible());
        assert!(bar.providers[1].is_visible());
        assert_eq!(bar.pins(), saved);
        state.apply(refresh(Provider::Claude, &[(300, 0.0), (10_080, 120.0)]));
        bar.render(&state, true);
        assert_eq!(bar.meters[0].borrow()[0].percent.text(), "0%(5h)");
        assert_eq!(bar.meters[0].borrow()[1].percent.text(), "120%(1W)");
        assert_eq!(bar.meters[0].borrow()[1].progress.fraction(), 1.0);
        assert!(!bar.meters[0].borrow()[2].root.is_visible());
        for window in &windows {
            bar.toggle_pin(&state.claude, window, false);
        }
        bar.render(&state, true);
        assert_eq!(bar.pins().claude, Some(vec![]));
        assert!(!bar.providers[0].is_visible());
        assert!(!bar.separator.is_visible());
        bar.set_pins(saved);
        bar.render(&state, false);
        assert!(!bar.root.is_visible());
        bar.render(&state, true);
        assert!(bar.root.is_visible());
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
        assert_eq!(
            window_percent(&state.claude, 300),
            Some(120.0),
            "a failed refresh keeps the last known value"
        );
        state.claude.limits = None;
        assert_eq!(window_percent(&state.claude, 300), None);
    }

    #[cfg_attr(not(target_os = "macos"), gtk::test)]
    pub(crate) fn footer_updates_in_place_and_hides_only_unavailable_periods() {
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
        assert!(
            bar.meters[0].borrow()[0].percent.width_chars()
                < bar.meters[1].borrow()[0].percent.width_chars()
        );
        assert!(bar.meters[0].borrow()[0].progress.has_css_class("claude"));
        assert!(bar.meters[1].borrow()[0].progress.has_css_class("codex"));
        assert_eq!(bar.meters[1].borrow()[1].percent.text(), "65%(1W)");
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
                Some(bar.meters[provider].borrow()[0].root.upcast_ref())
            );
        }
        let first = bar.meters[0].borrow()[0].root.clone();
        let visibility_changes = std::rc::Rc::new(std::cell::Cell::new(0));
        let changes = visibility_changes.clone();
        bar.root
            .connect_visible_notify(move |_| changes.set(changes.get() + 1));
        state.begin_forced_refresh();
        bar.render(&state, true);
        assert_eq!(bar.meters[0].borrow()[0].progress.fraction(), 0.22);
        state.apply(refresh(Provider::Claude, &[(300, 51.0), (10_080, 35.0)]));
        state.finish_refresh(Utc::now());
        bar.render(&state, true);
        assert_eq!(
            bar.icons[0].next_sibling().as_ref(),
            Some(first.upcast_ref())
        );
        assert_eq!(bar.meters[0].borrow()[0].progress.fraction(), 0.51);
        assert_eq!(
            visibility_changes.get(),
            0,
            "successful refresh must not hide the footer"
        );
        // A failed refresh keeps the last known values instead of blinking out,
        // and says so on hover.
        state.claude.limits_error = Some(UsageError::network());
        bar.render(&state, true);
        assert!(bar.providers[0].is_visible());
        assert_eq!(bar.meters[0].borrow()[0].percent.text(), "51%(5h)");
        assert_eq!(
            bar.meters[0].borrow()[0].root.tooltip_text().as_deref(),
            Some("Claude 5h usage (last known)")
        );
        assert!(bar.providers[1].is_visible());
        assert!(bar.separator.is_visible());
        state.apply(refresh(Provider::Codex, &[(300, 0.0), (10_080, 65.0)]));
        bar.render(&state, true);
        assert!(bar.meters[1].borrow()[0].root.is_visible());
        assert_eq!(bar.meters[1].borrow()[0].progress.fraction(), 0.0);
        assert_eq!(bar.meters[1].borrow()[0].percent.text(), "0%(5h)");
        assert_eq!(
            bar.meters[1].borrow()[0].root.tooltip_text().as_deref(),
            Some("Codex 5h usage")
        );
        assert!(bar.meters[1].borrow()[1].root.is_visible());
        assert!(bar.icons[1].is_visible());
        bar.render(&state, false);
        assert!(!bar.root.is_visible());
        // A provider that never collected anything has nothing to keep.
        state.claude.limits = None;
        state.codex.limits = None;
        state.codex.limits_error = Some(UsageError::network());
        bar.render(&state, true);
        assert!(!bar.root.is_visible());
        state.apply(refresh(Provider::Claude, &[(300, 120.0)]));
        bar.render(&state, true);
        assert!(bar.root.is_visible());
        assert_eq!(bar.meters[0].borrow()[0].progress.fraction(), 1.0);
        assert_eq!(bar.meters[0].borrow()[0].percent.text(), "120%(5h)");
    }

    #[cfg_attr(not(target_os = "macos"), gtk::test)]
    pub(crate) fn credit_based_plans_show_their_single_balance_in_the_first_slot() {
        let bar = UsageBar::new();
        let mut state = UsagePanelState::default();
        state.apply(credit_refresh(Provider::Claude, "Extra usage", 42.0));
        state.apply(credit_refresh(Provider::Codex, "Individual", 7.0));
        bar.render(&state, true);
        assert!(bar.root.is_visible());
        assert!(bar.separator.is_visible());
        assert_eq!(bar.meters[0].borrow()[0].percent.text(), "42%(Extra)");
        assert_eq!(bar.meters[0].borrow()[0].progress.fraction(), 0.42);
        assert_eq!(
            bar.meters[0].borrow()[0].root.tooltip_text().as_deref(),
            Some("Claude Extra usage")
        );
        assert_eq!(bar.meters[1].borrow()[0].percent.text(), "7%(Individual)");
        for provider in 0..2 {
            assert!(bar.providers[provider].is_visible());
            assert!(bar.meters[provider].borrow()[0].root.is_visible());
            assert!(
                !bar.meters[provider].borrow()[1].root.is_visible(),
                "a credit plan has no second window"
            );
        }
        // A plan that reports real periods keeps them; the balance is a fallback.
        state.apply(refresh(Provider::Claude, &[(300, 22.0)]));
        bar.render(&state, true);
        assert_eq!(bar.meters[0].borrow()[0].percent.text(), "22%(5h)");
        // Any other limit still shows, under a generic label, when a plan
        // reports neither periods nor a known credit balance.
        state.apply(credit_refresh(Provider::Codex, "Team pool", 90.0));
        bar.render(&state, true);
        assert!(bar.providers[1].is_visible());
        assert_eq!(bar.meters[1].borrow()[0].percent.text(), "90%(Limit)");
        assert_eq!(
            bar.meters[1].borrow()[0].root.tooltip_text().as_deref(),
            Some("Codex Team pool usage")
        );
        state.apply(credit_refresh(Provider::Claude, "Cinder cove", 100.0));
        bar.render(&state, true);
        assert_eq!(bar.meters[0].borrow()[0].percent.text(), "100%(Limit)");
    }

    #[cfg_attr(not(target_os = "macos"), gtk::test)]
    pub(crate) fn footer_font_tracks_global_terminal_font() {
        let bar = UsageBar::new();
        bar.set_font(&gtk::pango::FontDescription::from_string("Monospace 12"));
        let small_icon = bar.icons[0].pixel_size();
        let font = gtk::pango::FontDescription::from_string("Monospace 18");
        bar.set_font(&font);
        assert!(bar.icons[0].pixel_size() > small_icon);
        assert_eq!(bar.icons[0].pixel_size(), bar.icons[1].pixel_size());
        // Icons scale from font metrics; a label's layout height can round differently.
        let metrics = bar.root.pango_context().metrics(Some(&font), None);
        let original_height = gtk::pango::units_to_double(metrics.height()).ceil();
        assert_eq!(
            bar.icons[0].pixel_size(),
            (original_height * 0.8).round() as i32
        );
        assert_eq!(
            bar.meters[0].borrow()[0]
                .percent
                .attributes()
                .unwrap()
                .to_string(),
            bar.meters[1].borrow()[1]
                .percent
                .attributes()
                .unwrap()
                .to_string()
        );
        let rendered_font = bar.meters[0].borrow()[0]
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
