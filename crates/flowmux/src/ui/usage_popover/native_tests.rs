// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use flowmux_config::options;

fn descendants(root: &gtk::Widget) -> Vec<gtk::Widget> {
    let mut result = vec![root.clone()];
    let mut child = root.first_child();
    while let Some(widget) = child {
        result.extend(descendants(&widget));
        child = widget.next_sibling();
    }
    result
}

fn labels(root: &gtk::Widget) -> Vec<String> {
    descendants(root)
        .into_iter()
        .filter(|widget| {
            let mut current = Some(widget.clone());
            while let Some(widget) = current {
                if !widget.is_visible() {
                    return false;
                }
                if &widget == root {
                    break;
                }
                current = widget.parent();
            }
            true
        })
        .filter_map(|widget| {
            widget
                .downcast::<gtk::Label>()
                .ok()
                .map(|label| label.text().to_string())
        })
        .collect()
}

fn buttons(usage: &UsagePopover) -> Vec<gtk::ToggleButton> {
    descendants(usage.cards.upcast_ref())
        .into_iter()
        .filter(|widget| widget.widget_name() == "flowmux-usage-pin")
        .map(|widget| widget.downcast().unwrap())
        .collect()
}

async fn wait(mut ready: impl FnMut() -> bool) {
    gtk::glib::future_with_timeout(std::time::Duration::from_secs(10), async {
        while !ready() {
            gtk::glib::timeout_future(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("usage pin UI timed out");
}

pub(crate) async fn check(usage: &UsagePopover) {
    use crate::ui::usage_bar::tests as bar_tests;
    bar_tests::footer_updates_in_place_and_hides_only_unavailable_periods();
    bar_tests::credit_based_plans_show_their_single_balance_in_the_first_slot();
    bar_tests::footer_font_tracks_global_terminal_font();
    bar_tests::pins_preserve_scopes_values_fonts_and_survive_missing_data();
    tests::menu_button_owns_a_wide_upward_popover();
    tests::dropping_usage_popover_releases_refresh_widgets();

    {
        let mut state = usage.pins.state.borrow_mut();
        for provider in [Provider::Claude, Provider::Codex] {
            state.apply(ProviderRefresh {
                provider,
                tokens: FieldRefresh::Success(crate::usage::TokenTotals {
                    today: Some(12_500),
                    lifetime: Some(50_000),
                }),
                limits: FieldRefresh::Success(
                    [(300, "5 hours", 22.0), (10_080, "Weekly", 35.0)]
                        .into_iter()
                        .map(|(duration, label, value)| UsageWindow {
                            label: label.into(),
                            scope: None,
                            used_percent: value,
                            duration_minutes: Some(duration),
                            resets_at: Some(Utc::now() + chrono::Duration::hours(2)),
                        })
                        .collect(),
                ),
                collected_at: Utc::now(),
            });
        }
        state.finish_refresh(Utc::now());
    }
    usage.bar.render(&usage.pins.state.borrow(), true);
    usage.button.popup();
    wait(|| usage.button.popover().unwrap().is_mapped()).await;
    gtk::glib::timeout_future(std::time::Duration::from_millis(100)).await;
    let original_text = labels(usage.cards.upcast_ref());
    let original_footer = labels(usage.bar.root.upcast_ref());
    assert!(original_text
        .iter()
        .any(|text| text.contains("Tokens today 12.5K")));
    assert_eq!(
        original_footer,
        ["22%(5h)", "35%(1W)", "22%(5h)", "35%(1W)"]
    );
    let pins = buttons(usage);
    assert_eq!(pins.len(), 4);
    assert!(pins.iter().all(|pin| pin.is_active() && pin.is_mapped()));
    pins[0].emit_clicked();
    wait(|| {
        options::load()
            .usage_bar_pins
            .claude
            .as_ref()
            .is_some_and(|pins| pins.len() == 1)
    })
    .await;
    assert!(!pins[0].is_active());
    assert_eq!(
        labels(usage.cards.upcast_ref()),
        original_text,
        "pinning must not change card text"
    );
    let saved = options::load().usage_bar_pins;
    assert!(saved.codex.is_none());
    usage.button.popdown();
    wait(|| !usage.button.popover().unwrap().is_mapped()).await;
    gtk::glib::timeout_future(std::time::Duration::from_millis(100)).await;
    usage.button.popup();
    wait(|| usage.button.popover().unwrap().is_mapped()).await;
    gtk::glib::timeout_future(std::time::Duration::from_millis(100)).await;
    let reopened = buttons(usage);
    assert!(!reopened[0].is_active());
    assert!(reopened[1].is_active());
    assert_eq!(labels(usage.cards.upcast_ref()), original_text);
    let restored = UsagePopover::new(None);
    *restored.pins.state.borrow_mut() = usage.pins.state.borrow().clone();
    restored.set_pins(saved.clone());
    restored.set_bar_enabled(true);
    assert_eq!(restored.bar.pins(), saved);
    assert_eq!(
        labels(restored.bar.root.upcast_ref()),
        labels(usage.bar.root.upcast_ref())
    );
    reopened[0].emit_clicked();
    wait(|| {
        options::load()
            .usage_bar_pins
            .claude
            .as_ref()
            .is_some_and(|pins| pins.len() == 2)
    })
    .await;
    assert_eq!(labels(usage.bar.root.upcast_ref()), original_footer);
    assert_eq!(labels(usage.cards.upcast_ref()), original_text);
    usage.set_bar_enabled(false);
    assert!(!usage.bar.root.is_visible());
    usage.set_bar_enabled(true);
    assert!(usage.bar.root.is_visible());
    usage.button.popdown();
    wait(|| !usage.button.popover().unwrap().is_mapped()).await;
    gtk::glib::timeout_future(std::time::Duration::from_millis(100)).await;
    println!("MACOS_USAGE_PINS_OK: default rendering, toggle, persistence, reopen, restore, stale data, font and lifetime");
}
