// SPDX-License-Identifier: GPL-3.0-or-later

use crate::usage::{
    collect_all, collect_antigravity, collect_gemini, format_token_count, reset_countdown,
    FieldRefresh, Provider, ProviderRefresh, ProviderState, UsageError, UsageErrorKind,
    UsagePanelState, UsageWindow,
};
use chrono::{DateTime, Local, Utc};
use flowmux_config::options::UsageBarPins;
use flowmux_core::WorkspaceId;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;

#[cfg(all(test, feature = "native-smoke", target_os = "macos"))]
pub(crate) mod native_tests;

#[derive(Clone)]
pub(crate) struct UsagePopover {
    button: gtk::MenuButton,
    show_bar: gtk::ToggleButton,
    pub(crate) bar: Rc<super::usage_bar::UsageBar>,
    pins: Rc<PinControls>,
    cards: gtk::Box,
    refresh_button: gtk::Button,
    spinner: gtk::Spinner,
    team_workspaces: Rc<RefCell<HashSet<WorkspaceId>>>,
}

type PinsChanged = Box<dyn Fn(UsageBarPins)>;

struct PinControls {
    state: Rc<RefCell<UsagePanelState>>,
    bar: Rc<super::usage_bar::UsageBar>,
    show_bar: gtk::glib::WeakRef<gtk::ToggleButton>,
    changed: RefCell<Option<PinsChanged>>,
}

impl PinControls {
    fn button(self: &Rc<Self>, state: &ProviderState, window: &UsageWindow) -> gtk::ToggleButton {
        let button = gtk::ToggleButton::new();
        button.add_css_class("flat");
        button.set_widget_name("flowmux-usage-pin");
        button.set_active(self.bar.is_pinned(state, window));
        let update_appearance = |button: &gtk::ToggleButton| {
            let (icon, tooltip) = if button.is_active() {
                ("flowmux-pin-symbolic", "Unpin from usage bar")
            } else {
                ("flowmux-pin-outline-symbolic", "Pin to usage bar")
            };
            button.set_icon_name(icon);
            button.set_tooltip_text(Some(tooltip));
        };
        update_appearance(&button);
        let description = format!(
            "Pin {} {}{} to usage bar",
            state.provider.name(),
            window
                .scope
                .as_ref()
                .map(|scope| format!("{scope} "))
                .unwrap_or_default(),
            window.label
        );
        button.update_property(&[gtk::accessible::Property::Label(&description)]);
        let controls = self.clone();
        let provider = state.provider;
        let window = window.clone();
        button.connect_toggled(move |button| {
            update_appearance(button);
            {
                let state = controls.state.borrow();
                let provider_state = state.provider(provider);
                controls
                    .bar
                    .toggle_pin(provider_state, &window, button.is_active());
                if let Some(toggle) = controls.show_bar.upgrade() {
                    controls.bar.render(&state, toggle.is_active());
                }
            }
            if let Some(changed) = controls.changed.borrow().as_ref() {
                changed(controls.bar.pins());
            }
        });
        button
    }
}

impl UsagePopover {
    pub(crate) fn new(tokio_handle: Option<tokio::runtime::Handle>) -> Self {
        let button = gtk::MenuButton::new();
        button.set_icon_name("utilities-system-monitor-symbolic");
        button.add_css_class("flat");
        button.add_css_class("flowmux-sidebar-options");
        button.set_tooltip_text(Some("AI usage"));
        button.set_focus_on_click(false);
        button.set_widget_name("flowmux-usage-button");

        let popover = gtk::Popover::new();
        popover.set_size_request(360, -1);
        crate::ui::popover_pos::set_menu_popover(&button, &popover);
        popover.set_position(gtk::PositionType::Top);
        popover.set_halign(gtk::Align::Start);

        let state = Rc::new(RefCell::new(UsagePanelState::default()));
        let bar = Rc::new(super::usage_bar::UsageBar::new());
        // Set while a manual (refresh-button) refresh is in flight so its
        // result render does not replay the bar animation.
        let manual_refresh = Rc::new(Cell::new(false));

        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.set_margin_top(12);
        root.set_margin_bottom(12);
        root.set_margin_start(12);
        root.set_margin_end(12);

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("flowmux-panel-header");
        let title = gtk::Label::new(Some("AI Usage"));
        title.add_css_class("heading");
        title.set_halign(gtk::Align::Start);
        title.set_hexpand(true);
        header.append(&title);

        let show_bar = gtk::ToggleButton::new();
        show_bar.set_icon_name("view-bottom-pane-symbolic");
        show_bar.add_css_class("flat");
        show_bar.set_focus_on_click(false);
        show_bar.set_widget_name("flowmux-usage-show-bar");
        show_bar.set_tooltip_text(Some("Show bar"));
        show_bar.update_property(&[gtk::accessible::Property::Label("Show bar")]);
        header.append(&show_bar);
        let pins = Rc::new(PinControls {
            state: state.clone(),
            bar: bar.clone(),
            show_bar: show_bar.downgrade(),
            changed: RefCell::new(None),
        });

        let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
        refresh_button.add_css_class("flat");
        refresh_button.set_tooltip_text(Some("Refresh usage"));
        refresh_button.set_focus_on_click(false);
        refresh_button.set_widget_name("flowmux-usage-refresh-button");
        header.append(&refresh_button);

        let spinner = gtk::Spinner::new();
        spinner.set_tooltip_text(Some("Refreshing usage"));
        spinner.set_widget_name("flowmux-usage-refresh-spinner");
        header.append(&spinner);
        root.append(&header);

        let cards = gtk::Box::new(gtk::Orientation::Vertical, 8);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroll.set_min_content_height(180);
        scroll.set_max_content_height(520);
        scroll.set_propagate_natural_height(true);
        scroll.set_child(Some(&cards));
        root.append(&scroll);
        popover.set_child(Some(&root));
        render_usage(
            &cards,
            &refresh_button,
            &spinner,
            &state.borrow(),
            false,
            &pins,
        );

        let (result_tx, result_rx) = async_channel::bounded(4);
        let bar_weak = Rc::downgrade(&bar);
        let show_bar_weak = show_bar.downgrade();
        let state_for_results = state.clone();
        let manual_for_results = manual_refresh.clone();
        let popover_weak = popover.downgrade();
        let cards_weak = cards.downgrade();
        let refresh_weak = refresh_button.downgrade();
        let spinner_weak = spinner.downgrade();
        let handle_for_retry = tokio_handle.clone();
        let tx_for_retry = result_tx.clone();
        let pins_for_results = Rc::downgrade(&pins);
        gtk::glib::MainContext::default().spawn_local(async move {
            let mut attempt = 0;
            while let Ok(event) = result_rx.recv().await {
                let finished = matches!(&event, RefreshEvent::Finished);
                {
                    let mut state = state_for_results.borrow_mut();
                    match event {
                        RefreshEvent::Core(refreshes) => {
                            for refresh in refreshes {
                                state.apply(refresh);
                            }
                        }
                        RefreshEvent::Provider(refresh) => state.apply(refresh),
                        RefreshEvent::Finished => state.finish_refresh(Utc::now()),
                    }
                }
                if let (Some(bar), Some(toggle)) = (bar_weak.upgrade(), show_bar_weak.upgrade()) {
                    bar.render(&state_for_results.borrow(), toggle.is_active());
                }
                // A failed fetch leaves the last known values on screen, so
                // retry it without waiting out the periodic refresh.
                if finished {
                    let failed = {
                        let state = state_for_results.borrow();
                        state.claude.limits_error.is_some() || state.codex.limits_error.is_some()
                    };
                    let (delay, next_attempt) = next_retry(failed, attempt);
                    attempt = next_attempt;
                    if let Some(delay) = delay {
                        let state = state_for_results.clone();
                        let handle = handle_for_retry.clone();
                        let result_tx = tx_for_retry.clone();
                        gtk::glib::timeout_add_seconds_local_once(delay, move || {
                            request_refresh(&state, RefreshMode::CoreRetry, &handle, &result_tx);
                        });
                    }
                }
                let animate = finished && !manual_for_results.replace(false);
                let Some(popover) = popover_weak.upgrade() else {
                    break;
                };
                if popover.is_visible() {
                    let (Some(cards), Some(refresh), Some(spinner), Some(pins)) = (
                        cards_weak.upgrade(),
                        refresh_weak.upgrade(),
                        spinner_weak.upgrade(),
                        pins_for_results.upgrade(),
                    ) else {
                        break;
                    };
                    render_usage(
                        &cards,
                        &refresh,
                        &spinner,
                        &state_for_results.borrow(),
                        animate,
                        &pins,
                    );
                }
            }
        });

        let state_for_show = state.clone();
        let result_tx_for_show = result_tx.clone();
        let handle_for_show = tokio_handle.clone();
        let cards_for_show = cards.clone();
        let refresh_for_show = refresh_button.clone();
        let spinner_for_show = spinner.clone();
        let pins_for_show = pins.clone();
        popover.connect_show(move |_| {
            request_refresh(
                &state_for_show,
                RefreshMode::Cached,
                &handle_for_show,
                &result_tx_for_show,
            );
            render_usage(
                &cards_for_show,
                &refresh_for_show,
                &spinner_for_show,
                &state_for_show.borrow(),
                true,
                &pins_for_show,
            );
        });

        let state_for_refresh = state.clone();
        let manual_for_refresh = manual_refresh.clone();
        let result_tx_for_refresh = result_tx.clone();
        let handle_for_refresh = tokio_handle.clone();
        let cards_for_refresh = cards.clone();
        let spinner_for_refresh = spinner.clone();
        let pins_for_refresh = pins.clone();
        refresh_button.connect_clicked(move |button| {
            manual_for_refresh.set(true);
            request_refresh(
                &state_for_refresh,
                RefreshMode::Manual,
                &handle_for_refresh,
                &result_tx_for_refresh,
            );
            render_usage(
                &cards_for_refresh,
                button,
                &spinner_for_refresh,
                &state_for_refresh.borrow(),
                false,
                &pins_for_refresh,
            );
        });

        // Warm the cache before the first open; the on-show throttle prevents a duplicate fetch.
        request_refresh(&state, RefreshMode::Cached, &tokio_handle, &result_tx);

        let state_for_toggle = state.clone();
        let bar_for_toggle = bar.clone();
        let handle_for_toggle = tokio_handle.clone();
        let tx_for_toggle = result_tx.clone();
        show_bar.connect_toggled(move |toggle| {
            if toggle.is_active() {
                request_refresh(
                    &state_for_toggle,
                    RefreshMode::Cached,
                    &handle_for_toggle,
                    &tx_for_toggle,
                );
            }
            bar_for_toggle.render(&state_for_toggle.borrow(), toggle.is_active());
        });

        let countdown_toggle = show_bar.downgrade();
        let countdown_bar = Rc::downgrade(&bar);
        let countdown_state = state.clone();
        gtk::glib::timeout_add_seconds_local(15, move || {
            let (Some(toggle), Some(bar)) = (countdown_toggle.upgrade(), countdown_bar.upgrade())
            else {
                return gtk::glib::ControlFlow::Break;
            };
            if bar.root.is_mapped() && toggle.is_active() {
                bar.render(&countdown_state.borrow(), true);
            }
            gtk::glib::ControlFlow::Continue
        });

        let toggle_weak = show_bar.downgrade();
        let bar_weak = Rc::downgrade(&bar);
        let team_workspaces = Rc::new(RefCell::new(HashSet::new()));
        let teams_for_refresh = team_workspaces.clone();
        gtk::glib::timeout_add_seconds_local(BAR_REFRESH_SECONDS, move || {
            let (Some(toggle), Some(bar)) = (toggle_weak.upgrade(), bar_weak.upgrade()) else {
                return gtk::glib::ControlFlow::Break;
            };
            // Team routing consumes usage observations even when the bar is hidden.
            if toggle.is_active() || !teams_for_refresh.borrow().is_empty() {
                // The periodic refresh must not be postponed by the popover
                // cache throttle; the in-flight guard still prevents overlap.
                let mode = if toggle.is_active() {
                    RefreshMode::Periodic
                } else {
                    RefreshMode::CoreRetry
                };
                request_refresh(&state, mode, &tokio_handle, &result_tx);
                // Render retained values while the refresh is in flight.
                bar.render(&state.borrow(), toggle.is_active());
            }
            gtk::glib::ControlFlow::Continue
        });

        Self {
            button,
            show_bar,
            bar,
            pins,
            cards,
            refresh_button,
            spinner,
            team_workspaces,
        }
    }

    pub(crate) fn set_team_workspace(&self, id: WorkspaceId, team: bool) {
        let mut workspaces = self.team_workspaces.borrow_mut();
        if team {
            workspaces.insert(id);
        } else {
            workspaces.remove(&id);
        }
    }

    pub(crate) fn set_bar_enabled(&self, enabled: bool) {
        self.show_bar.set_active(enabled);
    }

    pub(crate) fn connect_bar_clicked(&self, callback: impl Fn(bool) + 'static) {
        self.show_bar
            .connect_clicked(move |toggle| callback(toggle.is_active()));
    }

    pub(crate) fn set_pins(&self, pins: UsageBarPins) {
        if self.bar.pins() == pins {
            return;
        }
        self.bar.set_pins(pins);
        let state = self.pins.state.borrow();
        self.bar.render(&state, self.show_bar.is_active());
        if self
            .button
            .popover()
            .is_some_and(|popover| popover.is_visible())
        {
            render_usage(
                &self.cards,
                &self.refresh_button,
                &self.spinner,
                &state,
                false,
                &self.pins,
            );
        }
    }

    pub(crate) fn connect_pins_changed(&self, callback: impl Fn(UsageBarPins) + 'static) {
        *self.pins.changed.borrow_mut() = Some(Box::new(callback));
    }

    pub(crate) fn button(&self) -> &gtk::MenuButton {
        &self.button
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefreshMode {
    Cached,
    Periodic,
    Manual,
    CoreRetry,
}

enum RefreshEvent {
    Core([ProviderRefresh; 2]),
    Provider(ProviderRefresh),
    Finished,
}

fn request_refresh(
    state: &Rc<RefCell<UsagePanelState>>,
    mode: RefreshMode,
    tokio_handle: &Option<tokio::runtime::Handle>,
    result_tx: &async_channel::Sender<RefreshEvent>,
) {
    let started = if mode == RefreshMode::Cached {
        state.borrow_mut().begin_refresh(Utc::now())
    } else {
        state.borrow_mut().begin_forced_refresh()
    };
    if !started {
        return;
    }

    let Some(handle) = tokio_handle.clone() else {
        apply_local_failure(
            state,
            UsageError::new(UsageErrorKind::Io, "Could not start the usage collector."),
        );
        return;
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        apply_local_failure(
            state,
            UsageError::new(
                UsageErrorKind::NotLoggedIn,
                "The local home directory was not found.",
            ),
        );
        return;
    };
    let now = Utc::now();
    let check_antigravity = mode == RefreshMode::Manual
        || state
            .borrow()
            .provider(Provider::Antigravity)
            .limits_error
            .as_ref()
            .is_none_or(|error| {
                !matches!(
                    error.kind,
                    UsageErrorKind::NotLoggedIn | UsageErrorKind::Unavailable
                )
            });
    let check_gemini = {
        let state = state.borrow();
        let gemini = state.provider(Provider::Gemini);
        mode == RefreshMode::Manual
            || (gemini.limits_error.is_none()
                && gemini
                    .limits
                    .as_ref()
                    .is_none_or(|limits| now - limits.updated_at >= chrono::Duration::minutes(10)))
    };
    let result_tx = result_tx.clone();
    handle.spawn(async move {
        let core = async {
            let result =
                match tokio::time::timeout(REFRESH_DEADLINE, collect_all(home.clone())).await {
                    Ok(refreshes) => refreshes,
                    Err(_) => refresh_timeout_failure(),
                };
            if let Err(error) = crate::usage::team::publish(&home, &result) {
                tracing::warn!(%error, "could not publish Team usage observations");
            }
            send_refresh(&result_tx, RefreshEvent::Core(result)).await;
        };
        let antigravity = async {
            if mode != RefreshMode::CoreRetry && check_antigravity {
                let update = collect_antigravity(home.clone()).await;
                send_refresh(&result_tx, RefreshEvent::Provider(update)).await;
            }
        };
        let gemini = async {
            if mode != RefreshMode::CoreRetry {
                let update = tokio::time::timeout(
                    REFRESH_DEADLINE,
                    collect_gemini(home.clone(), check_gemini),
                )
                .await
                .unwrap_or_else(|_| {
                    refresh_failure(Provider::Gemini, refresh_timeout_error(), Utc::now())
                });
                send_refresh(&result_tx, RefreshEvent::Provider(update)).await;
            }
        };
        tokio::join!(core, antigravity, gemini);
        send_refresh(&result_tx, RefreshEvent::Finished).await;
    });
}

async fn send_refresh(result_tx: &async_channel::Sender<RefreshEvent>, event: RefreshEvent) {
    let _ = result_tx.send(event).await;
    // Channel readiness alone does not wake the GTK main loop on macOS.
    gtk::glib::MainContext::default().wakeup();
}

const BAR_REFRESH_SECONDS: u32 = 150;

/// Delay before each retry of a failed fetch, in seconds. The spacing grows so
/// a rate-limited endpoint (`HTTP 429` is what the Claude usage service returns
/// under frequent polling) is not hammered, and once the list runs out the
/// periodic refresh takes over again.
const RETRY_DELAYS: [u32; 3] = [5, 20, 60];

/// How long to wait before retrying, plus the attempt counter to keep. A
/// successful refresh resets the counter so the next failure retries promptly.
fn next_retry(failed: bool, attempt: usize) -> (Option<u32>, usize) {
    if !failed {
        return (None, 0);
    }
    (RETRY_DELAYS.get(attempt).copied(), attempt + 1)
}

/// Backstop for providers with shorter internal deadlines. Antigravity has its own deadline.
const REFRESH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

fn refresh_timeout_error() -> UsageError {
    UsageError::new(UsageErrorKind::Timeout, "The usage request timed out.")
}

fn refresh_failure(provider: Provider, error: UsageError, now: DateTime<Utc>) -> ProviderRefresh {
    ProviderRefresh {
        provider,
        tokens: FieldRefresh::Failure(error.clone()),
        limits: FieldRefresh::Failure(error),
        collected_at: now,
    }
}

fn refresh_timeout_failure() -> [ProviderRefresh; 2] {
    let now = Utc::now();
    [Provider::Claude, Provider::Codex]
        .map(|provider| refresh_failure(provider, refresh_timeout_error(), now))
}

fn apply_local_failure(state: &Rc<RefCell<UsagePanelState>>, error: UsageError) {
    let now = Utc::now();
    let mut state = state.borrow_mut();
    for provider in Provider::ALL {
        state.apply(refresh_failure(provider, error.clone(), now));
    }
    state.finish_refresh(now);
}

fn render_usage(
    cards: &gtk::Box,
    refresh_button: &gtk::Button,
    spinner: &gtk::Spinner,
    state: &UsagePanelState,
    animate: bool,
    pins: &Rc<PinControls>,
) {
    set_refresh_controls(refresh_button, spinner, state.refreshing);
    while let Some(child) = cards.first_child() {
        cards.remove(&child);
    }
    for provider in Provider::ALL {
        cards.append(&provider_card(
            state.provider(provider),
            state.refreshing,
            animate,
            pins,
        ));
    }
}

fn set_refresh_controls(refresh_button: &gtk::Button, spinner: &gtk::Spinner, refreshing: bool) {
    refresh_button.set_sensitive(!refreshing);
    spinner.set_visible(refreshing);
    if refreshing {
        spinner.start();
    } else {
        spinner.stop();
    }
}

fn provider_card(
    state: &ProviderState,
    refreshing: bool,
    animate: bool,
    pins: &Rc<PinControls>,
) -> gtk::Widget {
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");

    let row = gtk::ListBoxRow::new();
    row.set_activatable(false);
    row.set_selectable(false);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    let provider_header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let name = gtk::Label::new(Some(state.provider.name()));
    name.add_css_class("heading");
    name.set_halign(gtk::Align::Start);
    name.set_hexpand(true);
    provider_header.append(&name);
    if let Some(updated_at) = latest_update(state) {
        let updated = gtk::Label::new(Some(&format_updated_at(updated_at)));
        updated.add_css_class("caption");
        updated.add_css_class("dim-label");
        provider_header.append(&updated);
    }
    content.append(&provider_header);

    let error_messages: HashSet<&str> = [state.token_error.as_ref(), state.limits_error.as_ref()]
        .into_iter()
        .flatten()
        .map(|error| error.message.as_str())
        .collect();
    let text_rows = provider_text_rows(state);
    for text in &text_rows {
        let label = gtk::Label::new(Some(text));
        label.set_halign(gtk::Align::Start);
        label.set_wrap(true);
        if error_messages.contains(text.as_str())
            || text.starts_with("Token data last updated ")
            || text.starts_with("Rate limits last updated ")
        {
            label.add_css_class("caption");
            label.add_css_class("dim-label");
        }
        content.append(&label);
    }

    if let Some(limits) = &state.limits {
        if limits.value.is_empty() {
            let empty = gtk::Label::new(Some("No rate limit data"));
            empty.set_halign(gtk::Align::Start);
            empty.add_css_class("caption");
            empty.add_css_class("dim-label");
            content.append(&empty);
        } else {
            for window in &limits.value {
                content.append(&limit_row(
                    state.provider,
                    window,
                    animate,
                    &pins.button(state, window),
                ));
            }
        }
    }

    if text_rows.is_empty() && state.limits.is_none() {
        let empty = gtk::Label::new(Some(if refreshing {
            "Loading usage…"
        } else {
            "No usage data"
        }));
        empty.set_halign(gtk::Align::Start);
        empty.add_css_class("dim-label");
        content.append(&empty);
    }

    row.set_child(Some(&content));
    list.append(&row);
    list.upcast()
}

fn limit_row(
    provider: Provider,
    window: &UsageWindow,
    animate: bool,
    pin: &gtk::ToggleButton,
) -> gtk::Widget {
    let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
    root.set_margin_top(2);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let mut title_text = window.label.clone();
    if let Some(scope) = &window.scope {
        title_text.push_str(" · ");
        title_text.push_str(scope);
    } else if provider == Provider::Claude && window.duration_minutes == Some(10_080) {
        title_text.push_str(" · All models");
    }
    let title = gtk::Label::new(Some(&title_text));
    title.set_halign(gtk::Align::Start);
    title.set_hexpand(true);
    header.append(&title);
    let percent = gtk::Label::new(Some(&percent_label(window.used_percent)));
    percent.add_css_class("numeric");
    header.append(&percent);
    header.append(pin);
    root.append(&header);

    let progress = gtk::ProgressBar::new();
    if animate {
        animate_fraction(&progress, progress_fraction(window.used_percent));
    } else {
        progress.set_fraction(progress_fraction(window.used_percent));
    }
    progress.set_hexpand(true);
    root.append(&progress);

    if let Some(resets_at) = window.resets_at {
        let reset = gtk::Label::new(Some(&reset_detail(resets_at)));
        reset.set_halign(gtk::Align::Start);
        reset.add_css_class("caption");
        reset.add_css_class("dim-label");
        let reset_weak = reset.downgrade();
        gtk::glib::timeout_add_seconds_local(15, move || {
            let Some(reset) = reset_weak.upgrade() else {
                return gtk::glib::ControlFlow::Break;
            };
            if reset.is_mapped() {
                reset.set_text(&reset_detail(resets_at));
            }
            gtk::glib::ControlFlow::Continue
        });
        root.append(&reset);
    }
    root.upcast()
}

/// How long a limit bar takes to fill from empty to its value, in
/// microseconds (frame clock time unit).
const BAR_ANIMATION_US: f64 = 500_000.0;

/// Animate a mapped bar from zero to its target when animations are enabled.
fn animate_fraction(progress: &gtk::ProgressBar, target: f64) {
    progress.set_fraction(0.0);
    progress.connect_map(move |progress| {
        let animations_enabled =
            gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations());
        if target <= 0.0 || !animations_enabled {
            progress.set_fraction(target);
            return;
        }
        let start_time = std::cell::Cell::new(None);
        progress.add_tick_callback(move |progress, clock| {
            let now = clock.frame_time();
            let start = start_time.get().unwrap_or_else(|| {
                start_time.set(Some(now));
                now
            });
            let t = ((now - start) as f64 / BAR_ANIMATION_US).clamp(0.0, 1.0);
            progress.set_fraction(target * t);
            if t < 1.0 {
                gtk::glib::ControlFlow::Continue
            } else {
                gtk::glib::ControlFlow::Break
            }
        });
    });
}

fn provider_text_rows(state: &ProviderState) -> Vec<String> {
    let mut rows = Vec::new();
    if let Some(tokens) = &state.tokens {
        if let Some(today) = tokens.value.today {
            rows.push(format!("Tokens today {}", format_token_count(today)));
        }
        if let Some(lifetime) = tokens.value.lifetime {
            rows.push(format!("Lifetime tokens {}", format_token_count(lifetime)));
        }
        for (model, amount) in &tokens.value.by_model {
            rows.push(format!(
                "{model} · {} tokens today",
                format_token_count(*amount)
            ));
        }
        if state.token_error.is_some() {
            rows.push(format_field_updated_at("Token data", tokens.updated_at));
        }
    }
    if let Some(limits) = &state.limits {
        if state.limits_error.is_some() {
            rows.push(format_field_updated_at("Rate limits", limits.updated_at));
        }
    }
    let mut seen_errors = HashSet::new();
    for error in [state.token_error.as_ref(), state.limits_error.as_ref()]
        .into_iter()
        .flatten()
    {
        if seen_errors.insert(error.message.as_str()) {
            rows.push(error.message.clone());
        }
    }
    rows
}

fn format_field_updated_at(label: &str, value: DateTime<Utc>) -> String {
    let local: DateTime<Local> = value.into();
    format!("{label} last updated {}", local.format("%H:%M"))
}

fn latest_update(state: &ProviderState) -> Option<DateTime<Utc>> {
    state
        .tokens
        .as_ref()
        .map(|value| value.updated_at)
        .into_iter()
        .chain(state.limits.as_ref().map(|value| value.updated_at))
        .max()
}

fn format_updated_at(value: DateTime<Utc>) -> String {
    let local: DateTime<Local> = value.into();
    format!("Updated {}", local.format("%H:%M"))
}

fn reset_detail(value: DateTime<Utc>) -> String {
    format!(
        "{} · {}",
        reset_countdown(value, Utc::now()),
        format_reset_at(value)
    )
}

pub(super) fn format_reset_at(value: DateTime<Utc>) -> String {
    let local: DateTime<Local> = value.into();
    if local.date_naive() == Local::now().date_naive() {
        format!("Resets today at {}", local.format("%H:%M"))
    } else {
        format!("Resets at {}", local.format("%m/%d %H:%M"))
    }
}

fn progress_fraction(percent: f64) -> f64 {
    if percent.is_finite() {
        (percent / 100.0).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

fn percent_label(percent: f64) -> String {
    if !percent.is_finite() {
        return "—".into();
    }
    if (percent - percent.round()).abs() < 0.05 {
        format!("{percent:.0}%")
    } else {
        format!("{percent:.1}%")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::usage::{
        Provider, ProviderState, Timestamped, TokenTotals, UsageError, UsageErrorKind,
    };
    use chrono::Utc;

    #[test]
    fn progress_fraction_clamps_without_changing_numeric_label() {
        assert_eq!(progress_fraction(-5.0), 0.0);
        assert_eq!(progress_fraction(42.0), 0.42);
        assert_eq!(progress_fraction(120.0), 1.0);
        assert_eq!(percent_label(120.0), "120%");
        assert_eq!(percent_label(42.5), "42.5%");
    }

    #[test]
    fn failed_fetches_retry_with_growing_spacing_and_reset_on_success() {
        let mut attempt = 0;
        let mut delays = Vec::new();
        for _ in 0..5 {
            let (delay, next) = next_retry(true, attempt);
            attempt = next;
            delays.push(delay);
        }
        assert_eq!(delays, [Some(5), Some(20), Some(60), None, None]);
        assert_eq!(next_retry(false, attempt), (None, 0));
        assert_eq!(next_retry(true, 0), (Some(5), 1));
    }

    #[cfg_attr(not(target_os = "macos"), gtk::test)]
    pub(crate) fn team_refresh_tracks_all_workspaces_without_enabling_the_bar() {
        let usage = UsagePopover::new(None);
        let first = WorkspaceId::new();
        let second = WorkspaceId::new();
        usage.set_team_workspace(first, true);
        usage.set_team_workspace(first, true);
        usage.set_team_workspace(second, true);
        assert_eq!(usage.team_workspaces.borrow().len(), 2);
        usage.set_team_workspace(first, false);
        assert!(!usage.team_workspaces.borrow().is_empty());
        usage.set_team_workspace(second, false);
        assert!(usage.team_workspaces.borrow().is_empty());
        assert!(!usage.show_bar.is_active());
        assert!(!usage.bar.root.is_visible());
    }

    #[test]
    fn stale_error_keeps_value_and_explains_refresh_failure() {
        let state = ProviderState {
            provider: Provider::Claude,
            tokens: Some(Timestamped {
                value: TokenTotals {
                    today: Some(12_500),
                    lifetime: None,
                    by_model: Vec::new(),
                },
                updated_at: Utc::now(),
            }),
            limits: None,
            token_error: Some(UsageError::new(
                UsageErrorKind::Unauthorized,
                "Run Claude once to refresh the local login.",
            )),
            limits_error: None,
        };

        let rows = provider_text_rows(&state);

        assert!(rows.iter().any(|row| row.contains("Tokens today 12.5K")));
        assert!(rows.iter().any(|row| row.contains("Run Claude once")));
    }

    #[test]
    fn partial_failure_marks_only_the_stale_field_timestamp() {
        let old = Utc::now() - chrono::Duration::minutes(5);
        let fresh = Utc::now();
        let mut panel = UsagePanelState::default();
        panel.apply(ProviderRefresh {
            provider: Provider::Claude,
            tokens: FieldRefresh::Success(TokenTotals {
                today: Some(12_500),
                lifetime: None,
                by_model: Vec::new(),
            }),
            limits: FieldRefresh::Success(Vec::new()),
            collected_at: old,
        });
        panel.apply(ProviderRefresh {
            provider: Provider::Claude,
            tokens: FieldRefresh::Failure(UsageError::network()),
            limits: FieldRefresh::Success(Vec::new()),
            collected_at: fresh,
        });

        let rows = provider_text_rows(&panel.claude);

        assert!(rows
            .iter()
            .any(|row| row.starts_with("Token data last updated ")));
        assert!(!rows
            .iter()
            .any(|row| row.starts_with("Rate limits last updated ")));
    }

    #[cfg_attr(not(target_os = "macos"), gtk::test)]
    pub(crate) fn menu_button_owns_a_wide_upward_popover() {
        let usage = UsagePopover::new(None);
        let popover = usage
            .button()
            .popover()
            .unwrap()
            .downcast::<gtk::Popover>()
            .unwrap();

        assert_eq!(
            usage.button().icon_name().as_deref(),
            Some("utilities-system-monitor-symbolic")
        );
        assert_eq!(popover.position(), gtk::PositionType::Top);
        assert_eq!(popover.width_request(), 360);

        let root = popover.child().unwrap();
        let toggle = find_named_widget(&root, "flowmux-usage-show-bar")
            .unwrap()
            .downcast::<gtk::ToggleButton>()
            .unwrap();
        assert_eq!(
            toggle.icon_name().as_deref(),
            Some("view-bottom-pane-symbolic")
        );
        assert!(toggle.label().is_none());
        assert_eq!(toggle.tooltip_text().as_deref(), Some("Show bar"));
        assert!(!toggle.is_active());
        assert_eq!(
            toggle.next_sibling().unwrap().widget_name(),
            "flowmux-usage-refresh-button"
        );
        let changed = Rc::new(Cell::new(None));
        let changed_cb = changed.clone();
        usage.connect_bar_clicked(move |enabled| changed_cb.set(Some(enabled)));
        usage.set_bar_enabled(true);
        assert!(toggle.is_active());
        assert_eq!(
            changed.get(),
            None,
            "options synchronization must not echo a click"
        );
        toggle.emit_clicked();
        assert_eq!(changed.get(), Some(false));
        toggle.emit_clicked();
        assert_eq!(changed.get(), Some(true));
        assert!(
            !usage.bar.root.is_visible(),
            "unavailable limits must stay hidden"
        );
        let refresh = find_named_widget(&root, "flowmux-usage-refresh-button")
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        let spinner = find_named_widget(&root, "flowmux-usage-refresh-spinner")
            .unwrap()
            .downcast::<gtk::Spinner>()
            .unwrap();
        assert_eq!(
            refresh.icon_name().as_deref(),
            Some("view-refresh-symbolic")
        );
        assert_eq!(refresh.tooltip_text().as_deref(), Some("Refresh usage"));
        set_refresh_controls(&refresh, &spinner, true);
        assert!(!refresh.is_sensitive());
        assert!(spinner.is_spinning());
    }

    #[cfg_attr(not(target_os = "macos"), gtk::test)]
    pub(crate) fn dropping_usage_popover_releases_refresh_widgets() {
        let usage = UsagePopover::new(None);
        let bar_weak = usage.bar.root.downgrade();
        let refresh_weak = {
            let popover = usage.button().popover().unwrap();
            let root = popover.child().unwrap();
            find_named_widget(&root, "flowmux-usage-refresh-button")
                .unwrap()
                .downgrade()
        };

        drop(usage);
        #[cfg(not(target_os = "macos"))]
        {
            let context = gtk::glib::MainContext::default();
            while context.pending() {
                context.iteration(false);
            }
        }

        assert!(
            refresh_weak.upgrade().is_none(),
            "the refresh receiver must not retain the widget graph"
        );
        assert!(
            bar_weak.upgrade().is_none(),
            "pin callbacks must not retain the usage bar"
        );
    }

    fn find_named_widget(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if root.widget_name() == name {
            return Some(root.clone());
        }
        let mut child = root.first_child();
        while let Some(widget) = child {
            if let Some(found) = find_named_widget(&widget, name) {
                return Some(found);
            }
            child = widget.next_sibling();
        }
        None
    }
}
