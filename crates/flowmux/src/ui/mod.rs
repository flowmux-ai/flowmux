// SPDX-License-Identifier: GPL-3.0-or-later
pub mod agent_bar;
mod agent_connection;
pub(crate) mod agent_office;
mod browser_bookmarks;
mod browser_capture;
mod browser_chrome;
#[cfg(not(target_os = "macos"))]
pub(crate) mod browser_color_scheme;
mod browser_control;
mod browser_dialogs;
mod browser_downloads;
pub(crate) mod browser_feedback;
mod browser_file_chooser;
mod browser_import;
pub mod browser_pane;
mod chat_pane;
mod chat_runtime;
pub mod editor_pane;
pub mod file_browser;
pub mod ghostty_pane;
pub mod image_viewer;
mod integration_status;
pub mod keybindings_panel;
#[cfg(target_os = "macos")]
pub mod macos_ime;
#[cfg(target_os = "macos")]
pub mod macos_layer;
mod managed_agents;
mod notification_popover;
pub mod options_dialog;
pub mod overlay_menu;
pub mod pane_terminal;
pub mod popover_pos;
pub(crate) mod review_window;
pub(crate) mod session_flow;
pub mod session_panel;
pub mod show_in_folder;
pub mod sidebar;
pub(crate) mod skills_tab;
mod terminal_minimap;
mod terminal_scroll;
mod terminal_scrollback;
pub mod theme_tab;
pub mod thorvg;
pub mod update_banner;
pub(crate) mod usage_bar;
pub mod usage_popover;
pub(crate) mod welcome;
pub mod window;
pub mod workspace_view;
pub mod worktree_panel;

use flowmux_core::AgentStatus;
use gtk::prelude::*;

pub use window::{spawn_dispatch_loop, WindowController};

#[cfg(test)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
async fn wait_for_agent_scroll_range(
    scroll: &gtk::ScrolledWindow,
    adjustment: &gtk::Adjustment,
    offset: f64,
) {
    use std::time::Duration;

    gtk::glib::future_with_timeout(Duration::from_secs(5), async {
        while !scroll.is_mapped()
            || adjustment.page_size() <= 0.0
            || adjustment.upper() - adjustment.page_size() <= offset
        {
            gtk::glib::timeout_future(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!(
            "agent scroll range did not reach {offset}: mapped={}, viewport={}x{}, upper={}, page={}",
            scroll.is_mapped(),
            scroll.width(),
            scroll.height(),
            adjustment.upper(),
            adjustment.page_size(),
        )
    });
}

pub(crate) const TAB_BOUND_AGENT_TOOLTIP: &str = "This agent is tied to its terminal tab. Closing the tab or Flowmux may stop it. Start a local persistent execution with New Agent or flowmux agent run.";

pub(crate) const HOSTED_AGENT_TOOLTIP: &str =
    "This agent keeps working in Background when its terminal tab or Flowmux closes.";

thread_local! {
    /// Terminal tabs whose shell runs in the Agent Host, as last reported by it.
    static HOSTED_TERMINALS: std::cell::RefCell<std::collections::HashSet<flowmux_core::SurfaceId>> =
        Default::default();
}

pub(crate) fn set_hosted_terminals(surfaces: std::collections::HashSet<flowmux_core::SurfaceId>) {
    HOSTED_TERMINALS.with(|hosted| *hosted.borrow_mut() = surfaces);
}

thread_local! {
    /// Running agents attached to the main window when its views were last verified.
    static RUNNING_AGENTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn set_running_agents(count: usize) {
    RUNNING_AGENTS.with(|agents| agents.set(count));
}

thread_local! {
    /// A termination signal is closing the window; nobody is there to answer a question.
    static UNATTENDED_CLOSE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub(crate) fn set_unattended_close() {
    UNATTENDED_CLOSE.with(|unattended| unattended.set(true));
}

pub(crate) fn is_unattended_close() -> bool {
    UNATTENDED_CLOSE.with(std::cell::Cell::get)
}

/// Whether closing the window will ask about running agents.
pub(crate) fn has_running_agents() -> bool {
    RUNNING_AGENTS.with(|agents| agents.get() > 0)
}

/// What closing its tab means for the agent in this terminal.
pub(crate) fn agent_lifetime_tooltip(surface: flowmux_core::SurfaceId) -> &'static str {
    if HOSTED_TERMINALS.with(|hosted| hosted.borrow().contains(&surface)) {
        HOSTED_AGENT_TOOLTIP
    } else {
        TAB_BOUND_AGENT_TOOLTIP
    }
}

/// Shown only for an agent whose terminal runs its shell itself.
pub(crate) fn tab_bound_agent_label(surface: flowmux_core::SurfaceId) -> gtk::Label {
    let label = gtk::Label::new(Some("Tab-bound"));
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    label.set_tooltip_text(Some(TAB_BOUND_AGENT_TOOLTIP));
    label.set_visible(agent_lifetime_tooltip(surface) == TAB_BOUND_AGENT_TOOLTIP);
    label
}

pub(crate) fn agent_attention_summary_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_widget_name("flowmux-agent-attention-summary");
    label.set_xalign(0.0);
    label.set_halign(gtk::Align::Start);
    label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    label.set_max_width_chars(32);
    label.add_css_class("caption");
    label.set_visible(false);
    label
}

pub(crate) fn update_agent_attention_summary(
    label: &gtk::Label,
    summary: flowmux_core::AgentAttentionSummary,
) {
    let text = summary.label();
    label.set_label(&text);
    label.set_visible(!text.is_empty());
    label.set_tooltip_text(Some(&summary.tooltip()));
}

pub(crate) fn agent_status_icon_name(status: AgentStatus, seen: bool) -> &'static str {
    match status {
        AgentStatus::Blocked => "dialog-warning-symbolic",
        AgentStatus::Working => "process-working-symbolic",
        // Done is an unread response boundary, not verification of successful work.
        AgentStatus::Done if !seen => "process-stop-symbolic",
        AgentStatus::Done | AgentStatus::Idle => "media-playback-pause-symbolic",
        AgentStatus::Unknown => "dialog-question-symbolic",
    }
}

/// Presentation only: retry waiting must not look like a user input request.
pub(crate) fn agent_attention_visual_status(
    status: AgentStatus,
    attention: flowmux_core::AgentAttention,
) -> AgentStatus {
    match attention {
        flowmux_core::AgentAttention::InputRequired
        | flowmux_core::AgentAttention::FailureReview => AgentStatus::Blocked,
        flowmux_core::AgentAttention::Waiting => AgentStatus::Idle,
        _ => status,
    }
}

pub(crate) fn agent_status_css_class(status: AgentStatus, seen: bool) -> &'static str {
    match status {
        AgentStatus::Blocked => "flowmux-sidebar-agent-blocked",
        AgentStatus::Working => "flowmux-sidebar-agent-working",
        AgentStatus::Done if !seen => "flowmux-sidebar-agent-done",
        AgentStatus::Done | AgentStatus::Idle => "flowmux-sidebar-agent-idle",
        AgentStatus::Unknown => "flowmux-sidebar-agent-unknown",
    }
}

pub(crate) fn agent_status_indicator(status: AgentStatus, seen: bool) -> gtk::Widget {
    if status == AgentStatus::Working {
        let spinner = gtk::Spinner::new();
        spinner.set_spinning(true);
        spinner.set_size_request(12, 12);
        spinner.add_css_class(agent_status_css_class(status, seen));
        spinner.upcast()
    } else {
        let icon = gtk::Image::from_icon_name(agent_status_icon_name(status, seen));
        icon.set_pixel_size(12);
        icon.add_css_class(agent_status_css_class(status, seen));
        icon.upcast()
    }
}

pub(crate) fn agent_icon(agent: &str) -> gtk::Image {
    let icon = gtk::Image::from_icon_name(crate::builtin_icons::agent_icon_name(agent));
    icon.set_pixel_size(14);
    icon.set_valign(gtk::Align::Center);
    icon
}
