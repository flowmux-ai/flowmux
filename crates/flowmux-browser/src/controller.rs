// SPDX-License-Identifier: GPL-3.0-or-later
//! Async trait every concrete browser controller (WebKit pane,
//! headless mock, future libcef binding, …) implements.
//!
//! The methods are deliberately fine-grained — `click`, `fill`,
//! `type_keys`, `press`, `select`, `scroll`, `text_of`, `value_of`,
//! `attr_of` — so the IPC and CLI layers can map one flowmux verb to
//! exactly one trait call.
//!
//! `async_trait` is used so the trait object stays usable
//! (`Box<dyn BrowserController>`) for the IPC dispatcher.

use crate::DomSnapshot;
use async_trait::async_trait;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum BrowserError {
    /// The ref token from the most recent snapshot is no longer
    /// resolvable — either the server's [`crate::refs::RefStore`]
    /// does not have it, or the cssSelector it maps to no longer
    /// matches a live DOM element (page navigated, element removed).
    #[error("element ref not found: {0}")]
    RefNotFound(String),
    /// JS evaluation threw or returned an unexpected shape.
    #[error("eval failed: {0}")]
    Eval(String),
    /// Navigation rejected (bad URL, network failure surfaced
    /// synchronously by WebKit).
    #[error("navigation failed: {0}")]
    Nav(String),
    /// Snapshot JSON couldn't be decoded into [`DomSnapshot`].
    #[error("snapshot decode: {0}")]
    Decode(String),
    /// Backend transport (IPC channel closed, etc.).
    #[error("transport: {0}")]
    Transport(String),
}

#[async_trait(?Send)]
pub trait BrowserController {
    // ---- navigation ----------------------------------------------------
    async fn navigate(&self, url: &str) -> Result<(), BrowserError>;
    async fn back(&self) -> Result<bool, BrowserError>;
    async fn forward(&self) -> Result<bool, BrowserError>;
    async fn reload(&self) -> Result<(), BrowserError>;

    // ---- introspection ------------------------------------------------
    async fn url(&self) -> Result<String, BrowserError>;
    async fn title(&self) -> Result<String, BrowserError>;
    async fn snapshot(&self) -> Result<DomSnapshot, BrowserError>;

    // ---- low-level eval ----------------------------------------------
    /// Run arbitrary JavaScript in the page context and return the
    /// stringified result (whatever the JS expression evaluates to).
    async fn eval(&self, source: &str) -> Result<String, BrowserError>;

    // ---- element interactions ----------------------------------------
    async fn click(&self, ref_id: &str) -> Result<(), BrowserError>;
    async fn fill(&self, ref_id: &str, value: &str) -> Result<(), BrowserError>;
    async fn select_option(&self, ref_id: &str, value: &str) -> Result<(), BrowserError>;
    async fn scroll(&self, ref_id: &str, x: i32, y: i32) -> Result<(), BrowserError>;

    // ---- keyboard input ----------------------------------------------
    async fn type_keys(&self, text: &str) -> Result<(), BrowserError>;
    async fn press(&self, key: &str) -> Result<(), BrowserError>;

    // ---- read element state ------------------------------------------
    async fn text_of(&self, ref_id: &str) -> Result<String, BrowserError>;
    async fn value_of(&self, ref_id: &str) -> Result<String, BrowserError>;
    async fn attr_of(&self, ref_id: &str, name: &str) -> Result<String, BrowserError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_is_actionable() {
        assert_eq!(
            BrowserError::RefNotFound("e1".into()).to_string(),
            "element ref not found: e1"
        );
        assert_eq!(
            BrowserError::Eval("syntax".into()).to_string(),
            "eval failed: syntax"
        );
    }
}
