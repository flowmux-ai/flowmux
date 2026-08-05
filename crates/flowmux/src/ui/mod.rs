// SPDX-License-Identifier: GPL-3.0-or-later
pub mod agent_bar;
mod browser_bookmarks;
mod browser_downloads;
pub mod browser_pane;
pub mod editor_pane;
pub mod file_browser;
pub mod ghostty_pane;
pub mod image_viewer;
pub mod keybindings_panel;
pub mod options_dialog;
pub mod overlay_menu;
pub mod pane_terminal;
pub mod popover_pos;
pub mod show_in_folder;
pub mod sidebar;
pub mod theme_tab;
pub mod thorvg;
pub mod update_banner;
pub mod usage_popover;
pub mod window;
pub mod workspace_view;
pub mod worktree_panel;

use flowmux_core::AgentStatus;
use gtk::prelude::*;

pub use window::{spawn_dispatch_loop, WindowController};

pub(crate) fn agent_status_icon_name(status: AgentStatus, seen: bool) -> &'static str {
    match status {
        AgentStatus::Blocked => "dialog-warning-symbolic",
        AgentStatus::Working => "process-working-symbolic",
        AgentStatus::Done if !seen => "emblem-ok-symbolic",
        AgentStatus::Done | AgentStatus::Idle => "media-playback-pause-symbolic",
        AgentStatus::Unknown => "dialog-question-symbolic",
    }
}

pub(crate) fn agent_status_css_class(status: AgentStatus, seen: bool) -> &'static str {
    match status {
        AgentStatus::Blocked if !seen => "flowmux-sidebar-agent-blocked",
        AgentStatus::Blocked => "flowmux-sidebar-agent-idle",
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
