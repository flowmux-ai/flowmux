// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows host services. The GTK workspace does not depend on this crate.
pub mod command;
pub mod cwd;
pub mod model;
#[cfg(windows)]
pub mod native;
pub mod output_search;
pub mod paste;
pub mod protocol;
pub mod settings;
pub mod shell;
pub mod state;
pub mod window_launch;
