// SPDX-License-Identifier: GPL-3.0-or-later
//! Browser operations, profile identifiers, DOM snapshots, and JavaScript helpers.
//! Native WebView integration lives in the GUI crate; these types are headless.

pub mod bookmarks;
pub mod controller;
pub mod profile;
pub mod refs;
pub mod scripts;
pub mod snapshot;

pub use bookmarks::{Bookmark, BookmarkError, BookmarkRepository};
pub use controller::{BrowserController, BrowserError};
pub use profile::{BrowserProfile, ProfileError};
pub use refs::{RefScope, RefStore};
pub use snapshot::{DomSnapshot, PageMeta, RefMeta};
