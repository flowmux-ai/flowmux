// SPDX-License-Identifier: GPL-3.0-or-later
//! JavaScript snippets the controller injects into the page to
//! implement snapshots, refs, clicks, fills, etc.
//!
//! Snapshot policy (cmux-equivalent): walk the DOM looking for nodes
//! with an interactive role / content role, compute a CSS path for
//! each, allocate a server-side `eN` token, and return a Markdown
//! tree + a `refs` map. **The DOM is not mutated** — we never stamp
//! `data-flowmux-ref` on the page. Subsequent action scripts take a
//! CSS selector, not a token; the server's [`crate::refs::RefStore`]
//! does the token→selector mapping before calling these.
//!
//! Each builder returns a string ready to hand to
//! `WebView::evaluate_javascript`. Action helpers always evaluate to
//! either the literal string `"ok"` on success or `"error: <reason>"`
//! on a soft failure (e.g. selector matches no element).

/// Walk the document for everything an agent might want to act on
/// — links, buttons, inputs, headings, anything with an explicit
/// role — and emit a JSON snapshot in cmux's shape:
///
/// ```text
/// {
///   "markdown": "- button \"OK\" [ref=e1]\n  - text \"Click me\"\n",
///   "refs": { "e1": { "role": "...", "name": "...", "selector": "..." } },
///   "page": { "url": "...", "title": "...", "ready_state": "...",
///             "text": "...", "html": null }
/// }
/// ```
///
/// `selector` is a CSS path (`#id` when present, otherwise
/// `tag:nth-of-type(n)` chains up to 6 ancestors deep). The page is
/// never modified.
pub const SNAPSHOT_JS: &str = r#"
(function() {
  const INTERACTIVE_ROLES = new Set([
    'button','link','textbox','checkbox','radio','combobox','listbox',
    'menuitem','menuitemcheckbox','menuitemradio','option','searchbox',
    'slider','spinbutton','switch','tab','treeitem'
  ]);
  const CONTENT_ROLES = new Set([
    'heading','cell','listitem','article','region','main','navigation'
  ]);

  function implicitRole(el) {
    const aria = el.getAttribute('role');
    if (aria) return aria;
    const t = el.tagName.toLowerCase();
    if (t === 'a' && el.hasAttribute('href')) return 'link';
    if (t === 'button') return 'button';
    if (t === 'select') return 'combobox';
    if (t === 'textarea') return 'textbox';
    if (t === 'input') {
      const ty = (el.getAttribute('type') || 'text').toLowerCase();
      if (ty === 'checkbox') return 'checkbox';
      if (ty === 'radio') return 'radio';
      if (ty === 'submit' || ty === 'button') return 'button';
      if (ty === 'search') return 'searchbox';
      return 'textbox';
    }
    if (/^h[1-6]$/.test(t)) return 'heading';
    if (t === 'li') return 'listitem';
    return t;
  }

  function visible(el) {
    const r = el.getBoundingClientRect();
    if (r.width < 4 || r.height < 4) return false;
    const cs = window.getComputedStyle(el);
    if (cs.visibility === 'hidden' || cs.display === 'none') return false;
    if (Number(cs.opacity) === 0) return false;
    return true;
  }

  function name(el) {
    return (
      el.getAttribute('aria-label') ||
      el.getAttribute('alt') ||
      el.getAttribute('title') ||
      el.getAttribute('placeholder') ||
      (el.innerText || '').trim().slice(0, 120)
    );
  }

  // Build a stable CSS selector for `el`. Prefer `#id` when the id is
  // unique; otherwise walk up to 6 ancestors using
  // `tag:nth-of-type(n)`. Bounded depth keeps the selector short and
  // resilient to small DOM changes higher up the tree.
  function cssPath(el) {
    if (el.id && document.querySelectorAll('#' + CSS.escape(el.id)).length === 1) {
      return '#' + CSS.escape(el.id);
    }
    const parts = [];
    let node = el;
    let depth = 0;
    while (node && node.nodeType === 1 && depth < 6) {
      let tag = node.tagName.toLowerCase();
      if (node.id && document.querySelectorAll('#' + CSS.escape(node.id)).length === 1) {
        parts.unshift('#' + CSS.escape(node.id));
        return parts.join(' > ');
      }
      let nth = 1;
      let sib = node.previousElementSibling;
      while (sib) {
        if (sib.tagName === node.tagName) nth += 1;
        sib = sib.previousElementSibling;
      }
      parts.unshift(tag + ':nth-of-type(' + nth + ')');
      node = node.parentElement;
      depth += 1;
    }
    return parts.join(' > ');
  }

  const refs = {};
  const lines = [];
  let counter = 0;

  document.querySelectorAll(
    'a,button,input,textarea,select,[role],h1,h2,h3,h4,h5,h6,label,summary,li,article,nav,main'
  ).forEach((el) => {
    if (!visible(el)) return;
    const role = implicitRole(el);
    if (!INTERACTIVE_ROLES.has(role) && !CONTENT_ROLES.has(role)) return;
    counter += 1;
    const token = 'e' + counter;
    const sel = cssPath(el);
    const nm = name(el).replace(/\n+/g, ' ').slice(0, 120);
    refs[token] = { role: role, name: nm, selector: sel };
    const safe = nm.split(String.fromCharCode(34)).join('\\"');
    lines.push('- ' + role + ' "' + safe + '" [ref=' + token + ']');
  });

  const text = (document.body && document.body.innerText
    ? document.body.innerText : '').slice(0, 4000);
  const page = {
    url: location.href,
    title: document.title,
    ready_state: document.readyState,
    text: text
  };

  return JSON.stringify({
    markdown: lines.join('\n') + (lines.length ? '\n' : ''),
    refs: refs,
    page: page
  });
})()
"#;

/// Click the element matched by `selector`. Returns `"ok"` on success,
/// `"error: not found"` if `querySelector` returns nothing.
pub fn click_by_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            el.click();
            return "ok";
        }})()"#,
        s = js_string(selector)
    )
}

/// Set `value` on an input/textarea (`<select>` should use
/// [`select_option_by_selector`] instead) and dispatch the standard
/// `input` + `change` events so framework listeners fire.
pub fn fill_by_selector(selector: &str, value: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            const setter = Object.getOwnPropertyDescriptor(el.__proto__, 'value');
            if (setter && setter.set) {{
                setter.set.call(el, "{v}");
            }} else {{
                el.value = "{v}";
            }}
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return "ok";
        }})()"#,
        s = js_string(selector),
        v = js_string(value)
    )
}

/// `<select>` value picker — looks up an `<option>` by its `value`
/// or, failing that, by its visible text.
pub fn select_option_by_selector(selector: &str, value: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            const want = "{v}";
            for (const opt of el.options) {{
                if (opt.value === want || opt.textContent.trim() === want) {{
                    opt.selected = true;
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return "ok";
                }}
            }}
            return "error: option not found";
        }})()"#,
        s = js_string(selector),
        v = js_string(value)
    )
}

/// Scroll the element matched by `selector` into view, with a
/// sub-pixel offset applied to the body afterwards.
pub fn scroll_by_selector(selector: &str, x: i32, y: i32) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            el.scrollIntoView({{ block: "center", inline: "nearest" }});
            window.scrollBy({x}, {y});
            return "ok";
        }})()"#,
        s = js_string(selector),
        x = x,
        y = y
    )
}

/// Read element's `innerText`.
pub fn text_of_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            return (el.innerText || "").toString();
        }})()"#,
        s = js_string(selector)
    )
}

/// Read an input/textarea/select's `value`.
pub fn value_of_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            return (el.value || "").toString();
        }})()"#,
        s = js_string(selector)
    )
}

/// Read an arbitrary attribute. Returns the empty string if the
/// element exists but the attribute does not (matches DOM behavior).
pub fn attr_of_selector(selector: &str, name: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            return (el.getAttribute("{n}") || "").toString();
        }})()"#,
        s = js_string(selector),
        n = js_string(name)
    )
}

/// Send each character of `text` as a `keydown`+`input`+`keyup`
/// triple to the active element. Mirrors what a user typing into a
/// focused input would produce.
pub fn type_keys(text: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.activeElement;
            if (!el) return "error: no focus";
            const text = "{t}";
            for (const ch of text) {{
                el.dispatchEvent(new KeyboardEvent('keydown', {{ key: ch, bubbles: true }}));
                if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {{
                    el.value += ch;
                    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
                }}
                el.dispatchEvent(new KeyboardEvent('keyup', {{ key: ch, bubbles: true }}));
            }}
            if (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA') {{
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            }}
            return "ok";
        }})()"#,
        t = js_string(text)
    )
}

/// Double-click the element matched by `selector`.
pub fn dblclick_by_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            el.dispatchEvent(new MouseEvent('dblclick', {{ bubbles: true, cancelable: true }}));
            return "ok";
        }})()"#,
        s = js_string(selector)
    )
}

/// Hover (mouseenter + mouseover) over the element matched by `selector`.
pub fn hover_by_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            el.dispatchEvent(new MouseEvent('mouseenter', {{ bubbles: false }}));
            el.dispatchEvent(new MouseEvent('mouseover', {{ bubbles: true }}));
            return "ok";
        }})()"#,
        s = js_string(selector)
    )
}

/// Focus the element matched by `selector` (uses `HTMLElement.focus()`).
pub fn focus_by_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            if (typeof el.focus !== 'function') return "error: not focusable";
            el.focus();
            return "ok";
        }})()"#,
        s = js_string(selector)
    )
}

/// Blur the element matched by `selector` (uses `HTMLElement.blur()`).
pub fn blur_by_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            if (typeof el.blur !== 'function') return "error: not blurrable";
            el.blur();
            return "ok";
        }})()"#,
        s = js_string(selector)
    )
}

/// Set a checkbox / radio's `checked` to true and dispatch `change`.
pub fn check_by_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            if (el.type !== 'checkbox' && el.type !== 'radio') return "error: not checkable";
            if (!el.checked) {{
                el.checked = true;
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            }}
            return "ok";
        }})()"#,
        s = js_string(selector)
    )
}

/// Set a checkbox's `checked` to false and dispatch `change`.
/// (Radio buttons can only be deselected by selecting another radio in
/// the same group, so this is a no-op for `<input type="radio">`.)
pub fn uncheck_by_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            if (el.type !== 'checkbox') return "error: not a checkbox";
            if (el.checked) {{
                el.checked = false;
                el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            }}
            return "ok";
        }})()"#,
        s = js_string(selector)
    )
}

/// Return `"true"` / `"false"` depending on whether the element matched
/// by `selector` is visible (size > 0, not display:none / visibility:
/// hidden / opacity:0). Returns `"error: not found"` when the selector
/// matches nothing.
pub fn is_visible_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            const r = el.getBoundingClientRect();
            if (r.width < 1 || r.height < 1) return "false";
            const cs = window.getComputedStyle(el);
            if (cs.visibility === 'hidden' || cs.display === 'none') return "false";
            if (Number(cs.opacity) === 0) return "false";
            return "true";
        }})()"#,
        s = js_string(selector)
    )
}

/// Return `"true"` / `"false"` depending on whether the element matched
/// by `selector` is *not* `disabled`.
pub fn is_enabled_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            return (el.disabled === true) ? "false" : "true";
        }})()"#,
        s = js_string(selector)
    )
}

/// Return `"true"` / `"false"` depending on whether the element matched
/// by `selector` (a checkbox or radio) is checked.
pub fn is_checked_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            return (el.checked === true) ? "true" : "false";
        }})()"#,
        s = js_string(selector)
    )
}

/// Return the integer count of elements matching `selector`. The CLI
/// stringifies the result (e.g. `"3"`) so a single string ack channel
/// can carry both int and bool results uniformly.
pub fn count_selector(selector: &str) -> String {
    format!(
        r#"(function() {{
            return String(document.querySelectorAll("{s}").length);
        }})()"#,
        s = js_string(selector)
    )
}

/// Send a single named key (`Enter`, `Tab`, `ArrowDown`, …) as a
/// `keydown`+`keyup` pair to the active element.
pub fn press_key(key: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.activeElement || document.body;
            const k = "{k}";
            el.dispatchEvent(new KeyboardEvent('keydown', {{ key: k, bubbles: true }}));
            el.dispatchEvent(new KeyboardEvent('keyup', {{ key: k, bubbles: true }}));
            return "ok";
        }})()"#,
        k = js_string(key)
    )
}

/// Conservative JS string escaper — covers the cases the agent
/// surfaces actually pass us (URLs, names, free text). Doesn't try
/// to be a general-purpose JS escaper, but is safe for what we use.
fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_string_escapes_known_specials() {
        assert_eq!(js_string(r#"a\b"#), "a\\\\b");
        assert_eq!(js_string("a\nb"), "a\\nb");
        assert_eq!(js_string("a\tb"), "a\\tb");
        assert_eq!(js_string("a\rb"), "a\\rb");
        assert_eq!(js_string("\"hi\""), "\\\"hi\\\"");
    }

    #[test]
    fn js_string_passes_safe_ascii_through() {
        assert_eq!(js_string("hello world"), "hello world");
        assert_eq!(js_string("a-b_c.d"), "a-b_c.d");
    }

    #[test]
    fn js_string_escapes_low_control_chars() {
        let s = js_string("\u{0001}");
        assert_eq!(s, "\\u0001");
    }

    #[test]
    fn js_string_escapes_line_separators() {
        // U+2028 / U+2029 break JS string literals if not escaped.
        assert_eq!(js_string("\u{2028}"), "\\u2028");
        assert_eq!(js_string("\u{2029}"), "\\u2029");
    }
}
