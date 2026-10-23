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
    'heading','cell','listitem','article','region','main','navigation','audio','video','iframe','frame'
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
      if (ty === 'submit' || ty === 'button' || ty === 'reset') return 'button';
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
    const cs = el.ownerDocument.defaultView.getComputedStyle(el);
    if (cs.visibility === 'hidden' || cs.display === 'none') return false;
    for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {
      const style = parent.ownerDocument.defaultView.getComputedStyle(parent);
      if (Number(style.opacity) === 0 || style.display === 'none' ||
          (parent.matches('iframe,frame') && style.visibility !== 'visible')) return false;
    }
    return true;
  }

  function flat(text) {
    return (text || '').replace(/\s+/g, ' ').trim();
  }

  function name(el, referenced = false) {
    // IDREF traversal is not recursive, including self references and cycles.
    if (!referenced) {
      const labels = flat(el.getAttribute('aria-labelledby')).split(' ')
        .map(id => el.getRootNode().getElementById(id)).filter(Boolean);
      if (labels.length) return flat(labels.map(label => name(label, true)).join(' '));
    }
    const aria = flat(el.getAttribute('aria-label'));
    if (aria) return aria;
    if (el.labels?.length) {
      const label = flat(Array.from(el.labels, label => name(label, true)).join(' '));
      if (label) return label;
    }
    if (el.matches('input[type=button],input[type=submit],input[type=reset]') && flat(el.value))
      return flat(el.value);
    if (el.hasAttribute('alt')) return flat(el.getAttribute('alt'));
    // shortcut: CSS-generated content and embedded-control naming need a full accessibility-tree API.
    const content = el.matches('input,textarea,select') ? ''
      : flat(el.innerText || (referenced ? el.textContent : ''));
    return content || flat(el.getAttribute('title')) || flat(el.getAttribute('placeholder'));
  }

  // Build a CSS selector that matches only `el`. Prefer `#id` when the id is
  // unique; otherwise prepend `tag:nth-of-type(n)` steps until the path
  // matches one element. Stopping early keeps the selector short; repeated
  // markup without ids may need a path anchored at <html>.
  function cssPath(el) {
    const document = el.getRootNode();
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

  function walk(root, shadowPath, framePath, frames) {
    root.querySelectorAll('*').forEach((el) => {
      if (el.shadowRoot) walk(el.shadowRoot, [...shadowPath, cssPath(el)], framePath, frames);
      // shortcut: inaccessible frames need native frame contexts for cross-origin control.
      if (el.matches('iframe,frame') && el.contentDocument && visible(el)) {
        walk(el.contentDocument, [], [...framePath, {selector: cssPath(el), shadow_path: shadowPath}],
          [...frames, {el, document: el.contentDocument}]);
      }
      if (!el.matches('a,button,input,textarea,select,[role],h1,h2,h3,h4,h5,h6,label,summary,li,article,nav,main,audio,video,iframe,frame') || !visible(el)) return;
      const role = implicitRole(el);
      if (!INTERACTIVE_ROLES.has(role) && !CONTENT_ROLES.has(role)) return;
      counter += 1;
      const token = 'e' + counter + '-' + nonce;
      const sel = cssPath(el);
      const nm = name(el).slice(0, 120);
      refs[token] = { role: role, name: nm, selector: sel };
      if (shadowPath.length) refs[token].shadow_path = shadowPath;
      if (framePath.length) refs[token].frame_path = framePath;
      if (el.matches('iframe,frame')) refs[token].frame_accessible = !!el.contentDocument;
      nodes.set(token, { el, root, frames });
      const safe = nm.split(String.fromCharCode(34)).join('\\"');
      lines.push('- ' + role + ' "' + safe + '" [ref=' + token + ']');
    });
  }
  walk(document, [], [], []);

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
            const record = globalThis.__flowmuxRefs?.get("{reference}");
            const el = record?.el;
            const document = record?.root;
            if (!el || !el.isConnected || el.getRootNode() !== document || document.querySelector("{selector}") !== el ||
                record.frames.some(frame => !frame.el.isConnected || frame.el.contentDocument !== frame.document))
                throw new Error("stale element ref: take a fresh snapshot");
            const window = el.ownerDocument.defaultView;
            const {{innerWidth, innerHeight, Event, HTMLMediaElement}} = window;
            const getComputedStyle = window.getComputedStyle.bind(window);
            return {script};
        }})()"#,
        reference = js_string(ref_id.strip_prefix('@').unwrap_or(ref_id)),
        selector = js_string(selector),
    )
}

/// `action_json` is a serialized IPC BrowserMediaAction, not page-supplied JS.
pub fn media(selector: &str, action_json: &str) -> String {
    format!(
        r#"(async function() {{
        const el = document.querySelector("{selector}");
        if (!(el instanceof HTMLMediaElement)) throw new Error('target is not an audio/video element');
        const action = {action_json};
        const deadline = performance.now() + 10000;
        const timeoutMessage = 'media action timed out; playback may still complete';
        function check() {{
            if (!el.isConnected) throw new Error('media element was detached');
            if (el.error) throw new Error('media error ' + el.error.code + ': ' + el.error.message);
            if (performance.now() >= deadline) throw new Error(timeoutMessage);
        }}
        async function wait(predicate) {{
            for (;;) {{
                check();
                if (predicate()) return;
                await new Promise(resolve => setTimeout(resolve, 25));
            }}
        }}
        switch (action.op) {{
            case 'status': break;
            case 'play': {{
                check();
                let timer;
                try {{
                    await Promise.race([el.play(), new Promise((_, reject) => {{
                        timer = setTimeout(() => reject(new Error(timeoutMessage)), 10000);
                    }})]);
                }} finally {{ clearTimeout(timer); }}
                const start = el.currentTime;
                await wait(() => (!el.paused && el.currentTime > start) || el.ended);
                break;
            }}
            case 'pause':
                el.pause();
                await wait(() => el.paused);
                break;
            case 'seek': {{
                check();
                const seconds = action.seconds;
                if (!Number.isFinite(seconds) || seconds < 0 || el.readyState === 0 ||
                    !Array.from({{length: el.seekable.length}}, (_, i) =>
                        seconds >= el.seekable.start(i) && seconds <= el.seekable.end(i)).some(Boolean))
                    throw new Error('seek time is outside the available media range');
                el.currentTime = seconds;
                await wait(() => !el.seeking && Math.abs(el.currentTime - seconds) < 0.25);
                break;
            }}
            case 'mute': case 'unmute':
                el.muted = action.op === 'mute';
                await wait(() => el.muted === (action.op === 'mute'));
                break;
            default: throw new Error('unknown media action');
        }}
        return JSON.stringify({{
            paused: el.paused, ended: el.ended, seeking: el.seeking, muted: el.muted,
            current_time: el.currentTime, duration: Number.isFinite(el.duration) ? el.duration : null,
            ready_state: el.readyState, network_state: el.networkState,
            error: el.error ? {{code: el.error.code, message: el.error.message}} : null
        }});
    }})()"#,
        selector = js_string(selector)
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
            for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {{
                if (parent.hasAttribute('inert') || ({enabled} && parent.getAttribute('aria-disabled') === 'true'))
                    return "error: element is disabled or inert";
                const style = parent.ownerDocument.defaultView.getComputedStyle(parent);
                if (Number(style.opacity) === 0 || style.display === 'none' ||
                    (parent.matches('iframe,frame') && style.visibility !== 'visible'))
                    return "error: element is not visible";
            }}
            let pointerPosition;
            if ({pointer}) {{
                if (el.matches('iframe,frame')) return "error: target is a frame; use an element ref inside it";
                el.scrollIntoView({{block:'center', inline:'nearest', behavior:'instant'}});
                const rect = el.getClientRects()[0];
                const left = Math.max(0, rect.left), right = Math.min(innerWidth, rect.right);
                const top = Math.max(0, rect.top), bottom = Math.min(innerHeight, rect.bottom);
                if (right <= left || bottom <= top) return "error: element is outside the viewport";
                let x = (left + right) / 2, y = (top + bottom) / 2;
                let target = el, view = el.ownerDocument.defaultView;
                for (;;) {{
                    if (x < 0 || y < 0 || x >= view.innerWidth || y >= view.innerHeight)
                        return "error: element is outside the viewport";
                    let hit = view.document.elementFromPoint(x, y);
                    while (hit?.shadowRoot) {{
                        const next = hit.shadowRoot.elementFromPoint(x, y);
                        if (!next || next === hit) break;
                        hit = next;
                    }}
                    while (hit && hit !== target)
                        hit = hit.assignedSlot || hit.parentElement || hit.getRootNode().host;
                    if (!hit) return "error: element is covered or cannot receive pointer input";
                    const frame = view.frameElement;
                    if (!frame) break;
                    for (let node = frame; node; node = node.parentElement || node.getRootNode().host) {{
                        const style = node.ownerDocument.defaultView.getComputedStyle(node);
                        if (style.transform !== 'none') {{
                            const matrix = new DOMMatrixReadOnly(style.transform);
                            if (!matrix.is2D || matrix.b !== 0 || matrix.c !== 0 || matrix.a <= 0 || matrix.d <= 0)
                                return "error: rotated or skewed frame pointer targets are not supported";
                        }}
                        if ((style.rotate !== 'none' && style.rotate !== '0deg') || style.perspective !== 'none' || style.offsetPath !== 'none' ||
                            (style.scale !== 'none' && style.scale.split(/\s+/).some(value => !Number.isFinite(Number(value)) || Number(value) <= 0)))
                            return "error: rotated or perspective frame pointer targets are not supported";
                    }}
                    const bounds = frame.getBoundingClientRect();
                    const box = frame.ownerDocument.defaultView.getComputedStyle(frame);
                    const leftInset = parseFloat(box.borderLeftWidth) + parseFloat(box.paddingLeft);
                    const topInset = parseFloat(box.borderTopWidth) + parseFloat(box.paddingTop);
                    const width = parseFloat(box.width) + (box.boxSizing === 'border-box' ? 0 :
                        leftInset + parseFloat(box.borderRightWidth) + parseFloat(box.paddingRight));
                    const height = parseFloat(box.height) + (box.boxSizing === 'border-box' ? 0 :
                        topInset + parseFloat(box.borderBottomWidth) + parseFloat(box.paddingBottom));
                    if (!(width > 0 && height > 0)) return "error: frame is not visible";
                    x = bounds.left + (leftInset + x) * bounds.width / width;
                    y = bounds.top + (topInset + y) * bounds.height / height;
                    target = frame;
                    view = frame.ownerDocument.defaultView;
                }}
                pointerPosition = {{x, y, width: view.innerWidth, height: view.innerHeight}};
            }}
            {action}
        }})()"#,
        s = js_string(selector)
    )
}

pub fn pointer_target(selector: &str, hover: bool) -> String {
    element_action(
        selector,
        true,
        !hover,
        r#"
        globalThis.__flowmuxInput.el = el;
        return JSON.stringify(pointerPosition);
    "#,
    )
}

pub fn prepare_pointer_input(geometry: &str, action: crate::input::PointerAction) -> String {
    use crate::input::PointerAction;
    let event = match action {
        PointerAction::Click => "click",
        PointerAction::DoubleClick => "dblclick",
        PointerAction::Hover => "pointermove",
    };
    format!(
        r#"
        if (globalThis.__flowmuxInput) throw new Error('browser input is already pending');
        const state = {{ received: false, event: '{event}' }};
        globalThis.__flowmuxInput = state;
        try {{
            const before = {geometry};
            if (before.startsWith('error:')) throw new Error(before);
            for (let frame = 0; frame < 2; frame++) {{
                await new Promise((resolve, reject) => {{
                    const timeout = setTimeout(() => reject(new Error('browser did not render a stable pointer target')), 1000);
                    requestAnimationFrame(() => {{clearTimeout(timeout); resolve();}});
                }});
                const after = {geometry};
                if (before !== after) throw new Error('pointer target moved; inspect the page before retrying');
            }}
            state.listener = event => {{
                if (event.isTrusted && event.composedPath().includes(state.el)) {{
                    state.received = true;
                    if (state.done) queueMicrotask(state.done);
                }}
            }};
            state.window = state.el.ownerDocument.defaultView;
            state.window.addEventListener(state.event, state.listener, true);
            return before;
        }} catch (error) {{
            delete globalThis.__flowmuxInput;
            throw error;
        }}
    "#
    )
}

pub const CANCEL_POINTER_INPUT_JS: &str = r#"(function() {
    const state = globalThis.__flowmuxInput;
    if (state?.window) state.window.removeEventListener(state.event, state.listener, true);
    delete globalThis.__flowmuxInput;
    return 'ok';
})()"#;

pub const FINISH_POINTER_INPUT_JS: &str = r#"
const state = globalThis.__flowmuxInput;
if (!state) throw new Error('page changed during native pointer input');
return await new Promise(resolve => {
    const finish = result => {
        clearTimeout(timer);
        clearInterval(hoverPoll);
        state.window.removeEventListener(state.event, state.listener, true);
        delete globalThis.__flowmuxInput;
        resolve(result);
    };
    const timer = setTimeout(() => finish('error: native pointer input completion timed out'), 10000);
    // Disabled controls and page handlers can suppress the pointer event while CSS still changes.
    const hoverPoll = state.event === 'pointermove' ? setInterval(() => {
        if (state.el.isConnected && state.el.matches(':hover')) finish('ok');
    }, 25) : null;
    state.done = () => finish('ok');
    if (state.received || (state.event === 'pointermove' && state.el.matches(':hover'))) state.done();
});
"#;

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
            el.dispatchEvent(new Event('input', {{ bubbles: true, composed: true }}));
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
const ACTIVE_ELEMENT_JS: &str = r#"(function() {
    let el = document.activeElement;
    while (el) {
        if (el.shadowRoot?.activeElement) { el = el.shadowRoot.activeElement; continue; }
        if (el.matches('iframe,frame')) {
            if (!el.contentDocument) throw new Error('focused frame is not accessible from this document');
            el = el.contentDocument.activeElement;
            continue;
        }
        break;
    }
    return el;
})()"#;

pub fn prepare_text_input(text: &str) -> String {
    format!(
        r#"(function() {{
        if (globalThis.__flowmuxInput) return "error: browser input is already pending";
        const el = {ACTIVE_ELEMENT_JS};
        const field = el && (el.tagName === 'INPUT' || el.tagName === 'TEXTAREA');
        if (!el || (!field && !el.isContentEditable)) return "error: focused element is not editable";
        if (el.matches(':disabled') || el.readOnly || el.closest('[inert],[aria-disabled="true"]'))
            return "error: focused element is disabled or readonly";
        if (![...el.getClientRects()].some(r => r.width > 0 && r.height > 0) || getComputedStyle(el).visibility !== 'visible')
            return "error: focused element is not visible";
        for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {{
            if (parent.matches('[inert],[aria-disabled="true"]'))
                return "error: focused element is disabled or readonly";
            const style = parent.ownerDocument.defaultView.getComputedStyle(parent);
            if (Number(style.opacity) === 0 || style.display === 'none' ||
                (parent.matches('iframe,frame') && style.visibility !== 'visible'))
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
pub fn finish_text_input() -> String {
    format!("(function() {{ const activeElement = () => {ACTIVE_ELEMENT_JS}; return {FINISH_TEXT_INPUT_JS}; }})()")
}

const FINISH_TEXT_INPUT_JS: &str = r#"(function() {
    const state = globalThis.__flowmuxInput;
    delete globalThis.__flowmuxInput;
    if (!state) return "error: page changed during native input";
    state.el.removeEventListener('input', state.listener);
    const active = activeElement();
    if (!state.el.isConnected || active !== state.el)
        return "error: input target changed";
    if (!state.received) return "error: native input was not applied";
    if (state.expected !== null && state.el.value !== state.expected)
        return "error: input value was not retained";
    return "ok";
})()"#;

pub fn prepare_key_input(stroke: &crate::input::KeyStroke) -> String {
    format!(
        "(function() {{ const activeElement = {ACTIVE_ELEMENT_JS}; const expectedKey={}; return {}; }})()",
        serde_json::to_string(&stroke.event_key()).unwrap(),
        PREPARE_KEY_INPUT_JS
    )
}

const PREPARE_KEY_INPUT_JS: &str = r#"(function() {
    if (globalThis.__flowmuxInput) return "error: browser input is already pending";
    const state = { received: false, down: null, windows: new Set() };
    // Tab can send keyup to a different frame from keydown.
    function visit(root) {
        if (root.defaultView) state.windows.add(root.defaultView);
        for (const el of root.querySelectorAll('*')) {
            if (el.shadowRoot) visit(el.shadowRoot);
            if (el.matches('iframe,frame') && el.contentDocument) visit(el.contentDocument);
        }
    }
    visit(document);
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
    for (const view of state.windows) {
        view.addEventListener('keyup', state.listener, true);
        view.addEventListener('keydown', state.keydown, true);
    }
    globalThis.__flowmuxInput = state;
    return "ok";
})()"#;

pub const FINISH_KEY_INPUT_JS: &str = r#"
const state = globalThis.__flowmuxInput;
if (!state) throw new Error('page changed during native key input');
return await new Promise(resolve => {
    const finish = result => {
        clearTimeout(timer);
        for (const view of state.windows) {
            view.removeEventListener('keyup', state.listener, true);
            view.removeEventListener('keydown', state.keydown, true);
        }
        delete globalThis.__flowmuxInput;
        resolve(result);
    };
    const timer = setTimeout(() => finish('error: native key input completion timed out'), 10000);
    state.done = () => finish(state.down?.defaultPrevented ? 'prevented' : 'ok');
    if (state.received) state.done();
});
"#;

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
            for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {{
                const style = parent.ownerDocument.defaultView.getComputedStyle(parent);
                if (Number(style.opacity) === 0 || style.display === 'none' ||
                    (parent.matches('iframe,frame') && style.visibility !== 'visible')) return "false";
            }}
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
            if (el.matches(':disabled')) return "false";
            for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {{
                if (parent.matches('[inert],[aria-disabled="true"]')) return "false";
            }}
            return "true";
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
