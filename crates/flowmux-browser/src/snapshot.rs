// SPDX-License-Identifier: GPL-3.0-or-later
//! Serializable shape of the page snapshot the controller produces.
//!
//! The snapshot is built by [`crate::scripts::SNAPSHOT_JS`] running
//! inside the WebView and decoded by `serde_json` on the Rust side.
//!
//! Shape mirrors cmux's `v2BrowserSnapshot` response (Markdown-style
//! tree + ref→meta map + page metadata). Importantly, **the snapshot
//! does not stamp any `data-flowmux-ref` attribute on the page**: the
//! server keeps a `(surface_id, ref_token) → cssSelector` map (see
//! [`crate::refs::RefStore`]) so subsequent action calls can resolve
//! a ref to a CSS selector without needing the page to remember it.
//! This matches cmux's policy of leaving the live DOM untouched.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct CaptureSize {
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub viewport: bool,
    #[serde(default)]
    pub clip: Option<CaptureRect>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
pub struct CaptureRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl CaptureSize {
    pub fn check(self, scale: f64) -> Result<(), String> {
        if let Some(rect) = self.clip {
            if ![rect.x, rect.y, rect.width, rect.height]
                .iter()
                .all(|v| v.is_finite())
                || rect.x < 0.0
                || rect.y < 0.0
                || rect.width <= 0.0
                || rect.height <= 0.0
                || rect.x + rect.width > self.width
                || rect.y + rect.height > self.height
            {
                return Err("element capture is outside the document".into());
            }
        }
        // Bound the uncompressed bitmap to 64 MiB before asking WebKit to allocate it.
        if ![self.width, self.height, scale]
            .iter()
            .all(|v| v.is_finite() && *v > 0.0)
            || (self.width * scale).ceil() * (self.height * scale).ceil() > 16_777_216.0
        {
            return Err("document capture exceeds the 64 MiB bitmap limit".into());
        }
        Ok(())
    }
}

/// Top-level snapshot returned by the WebView script.
///
/// Wire shape (JSON):
/// ```text
/// {
///   "markdown": "- button \"OK\" [ref=e1]\n  - text \"Click me\"\n",
///   "refs": { "e1": { "role": "button", "name": "OK", "selector": "..." } },
///   "page": { "url": "...", "title": "...", "ready_state": "complete",
///             "text": "...", "html": null }
/// }
/// ```
///
/// `refs` is a `BTreeMap` so JSON output is key-sorted and stable
/// across runs — useful for snapshot test goldens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DomSnapshot {
    #[serde(default)]
    pub truncated: bool,
    /// Tree-shaped Markdown the agent reads to plan its next click.
    pub markdown: String,
    /// `ref_token → meta`. Each ref's `selector` is what the server
    /// remembers in [`crate::refs::RefStore`].
    pub refs: BTreeMap<String, RefMeta>,
    /// Page-level metadata.
    pub page: PageMeta,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefMeta {
    /// ARIA role (or implicit role inferred from the tag).
    pub role: String,
    /// Best-effort accessible name from ARIA references, labels and content.
    pub name: String,
    /// CSS path relative to the element's document or shadow root.
    pub selector: String,
    /// Outer-to-inner CSS selectors for open shadow hosts; absent for light DOM.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_path: Vec<String>,
    /// Outer-to-inner frame locators, each relative to its parent document.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frame_path: Vec<FrameLocator>,
    /// Present on frame elements; false when DOM access is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_accessible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub states: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FrameLocator {
    pub selector: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shadow_path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageMeta {
    /// Present when the GUI binds the snapshot to a concrete browser tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    pub url: String,
    pub title: String,
    /// `document.readyState`: `"loading" | "interactive" | "complete"`.
    pub ready_state: String,
    /// Truncated `body.innerText` so agents have a quick summary
    /// without needing a separate read call.
    pub text: String,
    /// Optional HTML payload; the current snapshot script leaves this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html: Option<String>,
}

impl DomSnapshot {
    pub fn empty(url: impl Into<String>) -> Self {
        Self {
            truncated: false,
            markdown: String::new(),
            refs: BTreeMap::new(),
            page: PageMeta {
                surface_id: None,
                url: url.into(),
                title: String::new(),
                ready_state: "loading".into(),
                text: String::new(),
                html: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_size_bounds_physical_pixels_before_allocation() {
        let size = CaptureSize {
            width: 2048.0,
            height: 2048.0,
            viewport: false,
            clip: None,
        };
        assert!(size.check(2.0).is_ok());
        assert!(size.check(2.01).is_err());
        assert!(size.check(f64::NAN).is_err());
        assert!(CaptureSize {
            width: -1.0,
            height: 5.0,
            viewport: false,
            clip: None,
        }
        .check(1.0)
        .is_err());
        assert!(CaptureSize {
            width: f64::INFINITY,
            height: 5.0,
            viewport: false,
            clip: None,
        }
        .check(1.0)
        .is_err());
    }

    fn sample() -> DomSnapshot {
        let mut refs = BTreeMap::new();
        refs.insert(
            "e1".into(),
            RefMeta {
                role: "textbox".into(),
                name: "Email".into(),
                selector: "form#login > input:nth-of-type(1)".into(),
                shadow_path: Vec::new(),
                frame_path: Vec::new(),
                frame_accessible: None,
                states: None,
                description: None,
            },
        );
        refs.insert(
            "e2".into(),
            RefMeta {
                role: "button".into(),
                name: "Sign in".into(),
                selector: "form#login > button:nth-of-type(1)".into(),
                shadow_path: Vec::new(),
                frame_path: Vec::new(),
                frame_accessible: None,
                states: None,
                description: None,
            },
        );
        DomSnapshot {
            truncated: false,
            markdown: "- textbox \"Email\" [ref=e1]\n- button \"Sign in\" [ref=e2]\n".into(),
            refs,
            page: PageMeta {
                surface_id: None,
                url: "https://example.com/login".into(),
                title: "Sign in".into(),
                ready_state: "complete".into(),
                text: "Email Password Sign in".into(),
                html: None,
            },
        }
    }

    #[test]
    fn snapshot_roundtrips_through_json() {
        let s = sample();
        let json = serde_json::to_string(&s).unwrap();
        let back: DomSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn refs_map_is_key_sorted_in_json() {
        let s = sample();
        let json = serde_json::to_string(&s).unwrap();
        // BTreeMap serializes keys in sorted order. e1 must appear
        // before e2 in the JSON text.
        let i1 = json.find("\"e1\"").expect("e1 in json");
        let i2 = json.find("\"e2\"").expect("e2 in json");
        assert!(i1 < i2, "ref keys should be sorted: {json}");
    }

    #[test]
    fn empty_constructor_uses_loading_ready_state() {
        let s = DomSnapshot::empty("about:blank");
        assert_eq!(s.page.url, "about:blank");
        assert_eq!(s.page.title, "");
        assert!(s.refs.is_empty());
        assert_eq!(s.markdown, "");
        assert_eq!(s.page.ready_state, "loading");
    }

    #[test]
    fn page_html_is_omitted_when_none() {
        let s = DomSnapshot::empty("about:blank");
        let json = serde_json::to_string(&s).unwrap();
        // `html: None` should not produce a `"html":null` key.
        assert!(
            !json.contains("\"html\""),
            "html must be omitted when None: {json}"
        );
    }

    #[test]
    fn snapshot_parses_real_world_payload() {
        let raw = r#"{
            "markdown": "- button \"OK\" [ref=e1]\n",
            "refs": {
                "e1": {
                    "role": "button",
                    "name": "OK",
                    "selector": "body > button:nth-of-type(1)"
                }
            },
            "page": {
                "url": "https://x.test/",
                "title": "X",
                "ready_state": "complete",
                "text": "OK"
            }
        }"#;
        let s: DomSnapshot = serde_json::from_str(raw).unwrap();
        assert_eq!(s.refs.len(), 1);
        let meta = s.refs.get("e1").unwrap();
        assert_eq!(meta.role, "button");
        assert_eq!(meta.selector, "body > button:nth-of-type(1)");
        assert_eq!(s.page.html, None);
    }
}
