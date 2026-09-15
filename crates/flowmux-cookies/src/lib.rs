// SPDX-License-Identifier: GPL-3.0-or-later
//! Host-browser cookie import. Firefox reads plaintext SQLite cookies.
//! Chromium-family sources detect profiles but reject encrypted extraction
//! with `Error::EncryptedValuesUnsupported`.

pub mod chromium;
pub mod cookie;
pub mod firefox;
pub mod source;

pub use cookie::Cookie;
pub use source::{discover_sources, BrowserId, Source};
