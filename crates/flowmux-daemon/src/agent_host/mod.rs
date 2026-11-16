// SPDX-License-Identifier: GPL-3.0-or-later
//! GUI-independent ownership and retirement of managed agent instances.

mod host;
pub mod retirement;
mod terminal;

pub use host::run;
