// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows page-find scripts using the WebView's native text search engine.
use anyhow::ensure;
use serde_json::json;

pub const MAX_QUERY_BYTES: usize = 4096;

// Separate clap builders avoid growing the Windows debug main-thread stack.
#[derive(Debug, Clone, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneArgs {
    #[arg(value_parser = crate::command::parse_id)]
    pub pane: uuid::Uuid,
}
#[derive(Debug, Clone, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Args {
    #[arg(value_parser = crate::command::parse_id)]
    pub pane: uuid::Uuid,
    pub query: String,
    #[arg(long)]
    #[serde(default)]
    pub backward: bool,
    #[arg(long)]
    #[serde(default)]
    pub case_sensitive: bool,
    #[arg(long)]
    #[serde(default)]
    pub no_wrap: bool,
}

// Keep the engine responsible for rendered text, Unicode matching, and scrolling.
// The key is a random, per-browser property name supplied by the native host.
// Range identity and immutable boundary/direction snapshots prevent close from
// clearing a selection that the page or user has since replaced or changed.
const BRIDGE: &str = r#"function(args) {
    const tag = 'flowmux-page-find-v1';
    const descriptor = Object.getOwnPropertyDescriptor(window, args.key);
    const state = descriptor && descriptor.value;
    const owned = state && state.tag === tag && state.document === document;
    const selection = () => {
        try { return window.getSelection(); } catch (_) { return null; }
    };
    const text = () => {
        try {
            const current = selection();
            if (!current) return '';
            let value = current.toString().slice(0, 8192);
            const last = value.charCodeAt(value.length - 1);
            if (last >= 0xD800 && last <= 0xDBFF) value = value.slice(0, -1);
            return value;
        } catch (_) { return ''; }
    };
    const matches = (saved) => {
        try {
            const current = selection();
            if (!saved || saved.document !== document || !current ||
                current.rangeCount !== 1 || current.isCollapsed) return false;
            const range = current.getRangeAt(0);
            return range === saved.range &&
                saved.startContainer.isConnected && saved.endContainer.isConnected &&
                range.startContainer === saved.startContainer &&
                range.startOffset === saved.startOffset &&
                range.endContainer === saved.endContainer &&
                range.endOffset === saved.endOffset &&
                current.anchorNode === saved.anchorNode &&
                current.anchorOffset === saved.anchorOffset &&
                current.focusNode === saved.focusNode &&
                current.focusOffset === saved.focusOffset &&
                text() === saved.text;
        } catch (_) { return false; }
    };
    const discard = (saved) => {
        if (!saved) return;
        try { document.removeEventListener('selectionchange', saved.onChange); } catch (_) {}
        try {
            const current = Object.getOwnPropertyDescriptor(window, args.key);
            if (current && current.value === saved) delete window[args.key];
        } catch (_) {}
    };
    const release = () => {
        const clear = owned && matches(state);
        if (owned) discard(state);
        let cleared = false;
        if (clear) {
            try {
                selection().removeAllRanges();
                cleared = true;
            } catch (_) {}
        }
        return cleared;
    };
    if (args.close) {
        const cleared = release();
        return {found: false, selection: text(), cleared};
    }
    if (typeof window.find !== 'function') {
        throw new Error('Page find is unsupported: window.find is unavailable');
    }
    if (descriptor && !owned) {
        throw new Error('Page find state is unavailable');
    }
    if (owned && !matches(state)) discard(state);
    const found = window.find(
        args.query, args.case_sensitive, args.backward, args.wrap,
        false, false, false
    ) === true;
    if (found) {
        if (owned) discard(state);
        const current = selection();
        if (current && current.rangeCount === 1 && !current.isCollapsed) {
            const range = current.getRangeAt(0);
            const saved = {
                tag, document, range,
                query: args.query, case_sensitive: args.case_sensitive,
                startContainer: range.startContainer, startOffset: range.startOffset,
                endContainer: range.endContainer, endOffset: range.endOffset,
                anchorNode: current.anchorNode, anchorOffset: current.anchorOffset,
                focusNode: current.focusNode, focusOffset: current.focusOffset,
                text: text(), onChange: null
            };
            saved.onChange = () => {
                if (!matches(saved)) discard(saved);
            };
            Object.defineProperty(window, args.key, {
                value: saved, configurable: true, writable: true
            });
            document.addEventListener('selectionchange', saved.onChange);
        }
    } else if (owned &&
        (state.query !== args.query || state.case_sensitive !== args.case_sensitive)) {
        release();
    } else if (owned && !matches(state)) {
        discard(state);
    }
    return {found, selection: text()};
}"#;

/// Find the next/previous occurrence, leaving query codepoints unchanged.
pub fn script(
    key: &str,
    query: &str,
    backward: bool,
    case_sensitive: bool,
    wrap: bool,
) -> anyhow::Result<String> {
    ensure!(!query.is_empty(), "page find query must not be empty");
    ensure!(
        query.len() <= MAX_QUERY_BYTES,
        "page find query exceeds {MAX_QUERY_BYTES} UTF-8 bytes"
    );
    ensure!(
        !query.contains('\0'),
        "page find query must not contain NUL"
    );
    let args = json!({
        "key": key,
        "query": query,
        "backward": backward,
        "case_sensitive": case_sensitive,
        "wrap": wrap,
        "close": false,
    });
    Ok(format!("({BRIDGE})({args})"))
}

/// Release this document's find state without changing an unrelated selection.
pub fn close_script(key: &str) -> String {
    let args = json!({"key": key, "close": true});
    format!("({BRIDGE})({args})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn arguments(source: &str) -> Value {
        let prefix = format!("({BRIDGE})(");
        serde_json::from_str(
            source
                .strip_prefix(&prefix)
                .unwrap()
                .strip_suffix(')')
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn query_validation_bounds_utf8_bytes_and_rejects_nul() {
        for query in ["".into(), "\0".into(), "a\0b".into(), "a".repeat(4097)] {
            assert!(script("key", &query, false, false, true).is_err());
        }
        assert!(script("key", &"a".repeat(MAX_QUERY_BYTES), false, false, true).is_ok());
        let korean = "한".repeat(MAX_QUERY_BYTES / 3);
        assert!(script("key", &korean, false, false, true).is_ok());
        assert!(script("key", &(korean + "글"), false, false, true).is_err());
        assert!(script("key", " \t\n", false, false, true).is_ok());
    }

    #[test]
    fn query_and_key_are_json_data_with_exact_unicode_and_options() {
        let query = "한글 한 e\u{301} é 😀 \" \\ \n');window.injected=true;//\u{2028}\u{2029}";
        let key = "key\"\\');window.injected=true;//";
        for backward in [false, true] {
            for case_sensitive in [false, true] {
                for wrap in [false, true] {
                    let source = script(key, query, backward, case_sensitive, wrap).unwrap();
                    let args = arguments(&source);
                    assert_eq!(args["key"], key);
                    assert_eq!(args["query"].as_str().unwrap().as_bytes(), query.as_bytes());
                    assert_eq!(args["backward"], backward);
                    assert_eq!(args["case_sensitive"], case_sensitive);
                    assert_eq!(args["wrap"], wrap);
                    assert_eq!(args["close"], false);
                }
            }
        }
    }

    #[test]
    fn close_encodes_the_same_key_without_a_search_query() {
        let key = "한글\"\\\n\0');window.injected=true;//";
        assert_eq!(
            arguments(&close_script(key)),
            json!({"key": key, "close": true})
        );
    }
}
