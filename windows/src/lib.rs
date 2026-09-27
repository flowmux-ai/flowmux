// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows host services. The GTK workspace does not depend on this crate.
pub mod browser;
pub mod browser_action;
pub mod browser_capture;
pub mod browser_dom;
pub mod browser_wait;
pub mod command;
pub mod cwd;
pub mod keys;
pub mod minimap;
pub mod model;
#[cfg(windows)]
pub mod native;
pub mod notifications;
pub mod output_search;
pub mod paste;
pub mod protocol;
pub mod screen;
pub mod selection;
pub mod settings;
pub mod shell;
pub mod state;
pub mod window_launch;
