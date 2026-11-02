// SPDX-License-Identifier: GPL-3.0-or-later
//! Host-browser cookie import. Firefox reads plaintext SQLite cookies.
//! The interactive importer reads a selected profile after user consent.

pub mod chromium;
pub mod cookie;
mod crypto;
pub mod firefox;
pub mod import;
pub mod source;

pub use cookie::Cookie;
pub use source::{discover_sources, BrowserId, Source};
