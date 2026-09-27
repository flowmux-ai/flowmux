// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows host services. The GTK workspace does not depend on this crate.
pub mod command;
pub mod model;
#[cfg(windows)]
pub mod native;
pub mod protocol;
