// SPDX-License-Identifier: GPL-3.0-or-later
pub mod agent_bar;
mod agent_details;
pub(crate) mod agent_office;
mod browser_bookmarks;
mod browser_capture;
mod browser_chrome;
mod browser_control;
mod browser_dialogs;
mod browser_downloads;
mod browser_file_chooser;
pub mod browser_pane;
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
mod notification_popover;
pub mod options_dialog;
pub mod overlay_menu;
pub mod pane_terminal;
pub mod popover_pos;
pub(crate) mod review_window;
pub mod session_panel;
pub mod show_in_folder;
pub mod sidebar;
pub(crate) mod skills_tab;
mod terminal_minimap;
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
