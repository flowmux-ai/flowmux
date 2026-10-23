// SPDX-License-Identifier: GPL-3.0-or-later
//! JavaScript for DOM snapshots and browser actions. The snapshot allocates
//! opaque, snapshot-specific tokens without modifying the DOM. Rust stores
//! the returned selector map and resolves tokens before evaluating actions.

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
/// `selector` is a CSS path that matches only its element: `tag:nth-of-type(n)`
/// steps, anchored at the nearest unique `#id` or as far up as needed. The page
/// is never modified.
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

  // Build a CSS selector that matches only `el`. Prefer `#id` when the id is
  // unique; otherwise prepend `tag:nth-of-type(n)` steps until the path
  // matches one element. Stopping early keeps the selector short; repeated
  // markup without ids may need a path anchored at <html>.
  function cssPath(el) {
    if (el.id && document.querySelectorAll('#' + CSS.escape(el.id)).length === 1) {
      return '#' + CSS.escape(el.id);
    }
    const parts = [];
    let node = el;
    while (node && node.nodeType === 1) {
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
      if (document.querySelectorAll(parts.join(' > ')).length === 1) {
        return parts.join(' > ');
      }
      node = node.parentElement;
    }
    return parts.join(' > ');
  }

  const refs = {};
  const nodes = new Map();
  const nonce = Array.from(crypto.getRandomValues(new Uint32Array(4)), n => n.toString(16).padStart(8, '0')).join('');
  const lines = [];
  let counter = 0;

  document.querySelectorAll(
    'a,button,input,textarea,select,[role],h1,h2,h3,h4,h5,h6,label,summary,li,article,nav,main'
  ).forEach((el) => {
    if (!visible(el)) return;
    const role = implicitRole(el);
    if (!INTERACTIVE_ROLES.has(role) && !CONTENT_ROLES.has(role)) return;
    counter += 1;
    const token = 'e' + counter + '-' + nonce;
    const sel = cssPath(el);
    const nm = name(el).replace(/\n+/g, ' ').slice(0, 120);
    refs[token] = { role: role, name: nm, selector: sel };
    nodes.set(token, el);
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

  // Kept in the application script world, never in page attributes.
  globalThis.__flowmuxRefs = nodes;
  return JSON.stringify({
    markdown: lines.join('\n') + (lines.length ? '\n' : ''),
    refs: refs,
    page: page
  });
})()
"#;

/// Validate node identity and execute the action in the same script turn.
/// The caller must use the same isolated script world as the snapshot.
pub fn with_ref(ref_id: &str, selector: &str, script: &str) -> String {
    format!(
        r#"(function() {{
            const el = globalThis.__flowmuxRefs?.get("{reference}");
            if (!el || !el.isConnected || document.querySelector("{selector}") !== el)
                throw new Error("stale element ref: take a fresh snapshot");
            return {script};
        }})()"#,
        reference = js_string(ref_id.strip_prefix('@').unwrap_or(ref_id)),
        selector = js_string(selector),
    )
}

fn element_action(selector: &str, pointer: bool, enabled: bool, action: &str) -> String {
    format!(
        r#"(function() {{
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            if (el.closest('[inert]') || ({enabled} && (el.matches(':disabled') || el.closest('[aria-disabled="true"]'))))
                return "error: element is disabled or inert";
            const style = getComputedStyle(el);
            if (![...el.getClientRects()].some(r => r.width > 0 && r.height > 0) || style.visibility !== 'visible')
                return "error: element is not visible";
            for (let parent = el; parent; parent = parent.parentElement) {{
                if (Number(getComputedStyle(parent).opacity) === 0)
                    return "error: element is not visible";
            }}
            if ({pointer}) {{
                el.scrollIntoView({{block:'center', inline:'nearest', behavior:'instant'}});
                const rect = el.getClientRects()[0];
                const left = Math.max(0, rect.left), right = Math.min(innerWidth, rect.right);
                const top = Math.max(0, rect.top), bottom = Math.min(innerHeight, rect.bottom);
                if (right <= left || bottom <= top) return "error: element is outside the viewport";
                const hit = document.elementFromPoint((left + right) / 2, (top + bottom) / 2);
                if (!hit || !el.contains(hit)) return "error: element is covered or cannot receive pointer input";
            }}
            {action}
        }})()"#,
        s = js_string(selector)
    )
}

/// Click only when the element can receive pointer input.
pub fn click_by_selector(selector: &str) -> String {
    element_action(selector, true, true, "el.click(); return 'ok';")
}

/// Set `value` on an input/textarea (`<select>` should use
/// [`select_option_by_selector`] instead) and dispatch the standard
/// `input` + `change` events so framework listeners fire.
pub fn fill_by_selector(selector: &str, value: &str) -> String {
    element_action(
        selector,
        false,
        true,
        &format!(
            r#"
            if (el.tagName !== 'TEXTAREA' && (el.tagName !== 'INPUT' ||
                ['button','checkbox','color','file','hidden','image','radio','range','reset','submit'].includes(el.type)))
                return "error: element is not a text field";
            if (el.readOnly) return "error: element is readonly";
            const setter = Object.getOwnPropertyDescriptor(el.__proto__, 'value');
            if (setter && setter.set) {{
                setter.set.call(el, "{v}");
            }} else {{
                el.value = "{v}";
            }}
            el.dispatchEvent(new Event('input', {{ bubbles: true }}));
            el.dispatchEvent(new Event('change', {{ bubbles: true }}));
            return el.value === "{v}" ? "ok" : "error: fill value was not retained";
        "#,
            v = js_string(value)
        ),
    )
}

/// `<select>` value picker — looks up an `<option>` by its `value`
/// or, failing that, by its visible text.
pub fn select_option_by_selector(selector: &str, value: &str) -> String {
    element_action(
        selector,
        false,
        true,
        &format!(
            r#"
            if (el.tagName !== 'SELECT') return "error: element is not a select";
            const want = "{v}";
            for (const opt of el.options) {{
                if (opt.value === want || opt.textContent.trim() === want) {{
                    if (opt.matches(':disabled')) return "error: option is disabled";
                    opt.selected = true;
                    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
                    return opt.selected ? "ok" : "error: selection was not retained";
                }}
            }}
            return "error: option not found";
        "#,
            v = js_string(value)
        ),
    )
}

/// Center the element, then scroll the window by the given pixel offsets.
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
/// element exists but the attribute does not.
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

/// Observe the focused editor before a native insertion; serialize pending input.
pub fn prepare_text_input(text: &str) -> String {
    format!(
        r#"(function() {{
        if (globalThis.__flowmuxInput) return "error: browser input is already pending";
        const el = document.activeElement;
        const field = el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA');
        if (!el || (!field && !el.isContentEditable)) return "error: focused element is not editable";
        if (el.matches(':disabled') || el.readOnly || el.closest('[inert],[aria-disabled="true"]'))
            return "error: focused element is disabled or readonly";
        if (![...el.getClientRects()].some(r => r.width > 0 && r.height > 0) || getComputedStyle(el).visibility !== 'visible')
            return "error: focused element is not visible";
        for (let parent = el; parent; parent = parent.parentElement) {{
            if (Number(getComputedStyle(parent).opacity) === 0)
                return "error: focused element is not visible";
        }}
        const text = "{text}";
        const expected = field && el.selectionStart !== null
            ? el.value.slice(0, el.selectionStart) + text + el.value.slice(el.selectionEnd) : null;
        const state = {{ el, expected, received: false }};
        state.listener = event => {{ if (event.isTrusted) state.received = true; }};
        el.addEventListener('input', state.listener);
        globalThis.__flowmuxInput = state;
        return "ok";
    }})()"#,
        text = js_string(text)
    )
}

/// Read the native edit result and always release the pending-input observation.
pub const FINISH_TEXT_INPUT_JS: &str = r#"(function() {
    const state = globalThis.__flowmuxInput;
    delete globalThis.__flowmuxInput;
    if (!state) return "error: page changed during native input";
    state.el.removeEventListener('input', state.listener);
    if (!state.el.isConnected || document.activeElement !== state.el)
        return "error: input target changed";
    if (!state.received) return "error: native input was not applied";
    if (state.expected !== null && state.el.value !== state.expected)
        return "error: input value was not retained";
    return "ok";
})()"#;

pub fn prepare_key_input(stroke: &crate::input::KeyStroke) -> String {
    format!(
        "(function() {{ const expectedKey={}; return {}; }})()",
        serde_json::to_string(&stroke.event_key()).unwrap(),
        PREPARE_KEY_INPUT_JS
    )
}

const PREPARE_KEY_INPUT_JS: &str = r#"(function() {
    if (globalThis.__flowmuxInput) return "error: browser input is already pending";
    const state = { received: false, down: null };
    // WebKitGTK reports ISO_Left_Tab as Unidentified but preserves its physical code.
    const matches = event => event.key === expectedKey ||
        (expectedKey === 'Tab' && event.key === 'Unidentified' && event.code === 'Tab');
    state.keydown = event => {
        if (event.isTrusted && matches(event)) state.down = event;
    };
    state.listener = event => {
        if (event.isTrusted && matches(event) && state.down) {
            state.received = true;
            if (state.done) queueMicrotask(state.done);
        }
    };
    window.addEventListener('keyup', state.listener, true);
    window.addEventListener('keydown', state.keydown, true);
    globalThis.__flowmuxInput = state;
    return "ok";
})()"#;

pub const FINISH_KEY_INPUT_JS: &str = r#"
const state = globalThis.__flowmuxInput;
if (!state) throw new Error('page changed during native key input');
return await new Promise(resolve => {
    const finish = result => {
        clearTimeout(timer);
        window.removeEventListener('keyup', state.listener, true);
        window.removeEventListener('keydown', state.keydown, true);
        delete globalThis.__flowmuxInput;
        resolve(result);
    };
    const timer = setTimeout(() => finish('error: native key input completion timed out'), 10000);
    state.done = () => finish(state.down?.defaultPrevented ? 'prevented' : 'ok');
    if (state.received) state.done();
});
"#;

/// Double-click the element matched by `selector`.
pub fn dblclick_by_selector(selector: &str) -> String {
    element_action(
        selector,
        true,
        true,
        r#"
            el.dispatchEvent(new MouseEvent('dblclick', { bubbles: true, cancelable: true }));
            return "ok";
        "#,
    )
}

/// Hover (mouseenter + mouseover) over the element matched by `selector`.
pub fn hover_by_selector(selector: &str) -> String {
    element_action(
        selector,
        true,
        false,
        r#"
            el.dispatchEvent(new MouseEvent('mouseenter', { bubbles: false }));
            el.dispatchEvent(new MouseEvent('mouseover', { bubbles: true }));
            return "ok";
        "#,
    )
}

/// Focus the element matched by `selector` (uses `HTMLElement.focus()`).
pub fn focus_by_selector(selector: &str) -> String {
    element_action(
        selector,
        false,
        true,
        r#"
            if (typeof el.focus !== 'function') return "error: not focusable";
            el.focus();
            return document.activeElement === el ? "ok" : "error: focus was not applied";
        "#,
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

/// Click an unchecked checkbox / radio so the page sees click, input and
/// change events, as frameworks that track clicks require.
pub fn check_by_selector(selector: &str) -> String {
    element_action(
        selector,
        true,
        true,
        r#"
            if (el.type !== 'checkbox' && el.type !== 'radio') return "error: not checkable";
            if (!el.checked) {
                el.click();
                if (!el.checked) return "error: check was prevented";
            }
            return "ok";
        "#,
    )
}

/// Click a checked checkbox so the page sees click, input and change events.
/// Returns `"error: not a checkbox"` for radios and other element types.
pub fn uncheck_by_selector(selector: &str) -> String {
    element_action(
        selector,
        true,
        true,
        r#"
            if (el.type !== 'checkbox') return "error: not a checkbox";
            if (el.checked) {
                el.click();
                if (el.checked) return "error: uncheck was prevented";
            }
            return "ok";
        "#,
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

/// Escape text embedded inside a double-quoted JavaScript string literal.
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
