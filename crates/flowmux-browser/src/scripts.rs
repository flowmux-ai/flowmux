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
  const options = /*FLOWMUX_OPTIONS*/ {};
  // Declared before the scope lookup below, which can already need cssPath.
  const pathIndexes = new Map();
  // Dense output omits Markdown and selectors, so the budget counts only printed data.
  const dense = !!options.dense;
  const maxNodes = Math.max(1, Math.min(options.max_nodes || 2000, 2000));
  const maxChars = Math.max(1024, Math.min(options.max_chars || 262144, 262144));
  let truncated = false, usedChars = 512;
  const matches = (actual, expected) => expected == null || (options.exact ? flat(actual) === flat(expected) : flat(actual).includes(flat(expected)));
  let scope = document, scopeRecord;
  if (options.within) {
    scopeRecord = globalThis.__flowmuxRefs?.get(options.within);
    scope = scopeRecord?.el;
    if (!scope || !scope.isConnected || scope.getRootNode() !== scopeRecord.root ||
        scopeRecord.frames.some(frame => !frame.el.isConnected || frame.el.contentDocument !== frame.document))
      throw new Error('stale scope ref: take a fresh snapshot');
  }
  if (scope.matches?.('iframe,frame')) {
    const frame = scope;
    if (!frame.contentDocument) throw new Error('scope frame is not accessible');
    scope = frame.contentDocument;
    scopeRecord = {
      shadowPath: [],
      framePath: [...scopeRecord.framePath, {selector: cssPath(frame), shadow_path: scopeRecord.shadowPath}],
      frames: [...scopeRecord.frames, {el: frame, document: scope}]
    };
  }
  const INTERACTIVE_ROLES = new Set([
    'button','link','textbox','checkbox','radio','combobox','listbox',
    'menuitem','menuitemcheckbox','menuitemradio','option','searchbox',
    'slider','spinbutton','switch','tab','treeitem'
  ]);
  const CONTENT_ROLES = new Set([
    'heading','cell','listitem','article','region','main','navigation','audio','video','iframe','frame','status','alert','dialog','form','table','row','columnheader','rowheader'
  ]);

  function implicitRole(el) {
    const aria = el.getAttribute('role');
    if (aria) return aria;
    const t = el.tagName.toLowerCase();
    if (t === 'a' && el.hasAttribute('href')) return 'link';
    if (t === 'button') return 'button';
    if (t === 'select') return 'combobox';
    if (t === 'textarea' || el.isContentEditable) return 'textbox';
    if (t === 'nav') return 'navigation';
    if (t === 'summary') return 'button';
    if (t === 'output') return 'status';
    if (t === 'input') {
      const ty = (el.getAttribute('type') || 'text').toLowerCase();
      if (ty === 'checkbox') return 'checkbox';
      if (ty === 'radio') return 'radio';
      if (ty === 'submit' || ty === 'button' || ty === 'reset') return 'button';
      if (ty === 'search') return 'searchbox';
      if (ty === 'range') return 'slider';
      if (ty === 'number') return 'spinbutton';
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

  function inViewport(el) {
    let {left, top, right, bottom} = el.getBoundingClientRect();
    for (let view = el.ownerDocument.defaultView;;) {
      left = Math.max(left, 0); top = Math.max(top, 0);
      right = Math.min(right, view.innerWidth); bottom = Math.min(bottom, view.innerHeight);
      if (right <= left || bottom <= top) return false;
      if (!view.frameElement) return true;
      try {
        const frame = frameTransform(view.frameElement);
        left = frame.x + left * frame.sx; right = frame.x + right * frame.sx;
        top = frame.y + top * frame.sy; bottom = frame.y + bottom * frame.sy;
      } catch (_) { return false; }
      view = view.frameElement.ownerDocument.defaultView;
    }
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

  // One pass per document or shadow root: id counts, each element's
  // nth-of-type index and the elements under each `tag:nth-of-type(n)` step.
  // Querying the whole root for every step made large pages quadratic or worse.
  // A form control named `id`, `tagName` or `parentNode` shadows that property on
  // its form, so read them through the prototype accessors.
  const accessor = (proto, name) => {
    const get = Object.getOwnPropertyDescriptor(proto, name).get;
    return node => get.call(node);
  };
  const idOf = el => Element.prototype.getAttribute.call(el, 'id');
  const tagOf = accessor(Element.prototype, 'tagName'), localNameOf = accessor(Element.prototype, 'localName');
  const namespaceOf = accessor(Element.prototype, 'namespaceURI');
  const parentOf = accessor(Node.prototype, 'parentNode'), parentElementOf = accessor(Node.prototype, 'parentElement');

  function pathIndex(root) {
    let index = pathIndexes.get(root);
    if (index) return index;
    // Quirks-mode id selectors match ASCII case-insensitively.
    const quirks = (root.ownerDocument || root).compatMode === 'BackCompat';
    index = { ids: new Map(), nth: new Map(), steps: new Map(), idKey: id => quirks ? id.toLowerCase() : id };
    const counters = new Map();
    for (const el of root.querySelectorAll('*')) {
      const id = idOf(el), tag = tagOf(el), parent = parentOf(el);
      if (id) index.ids.set(index.idKey(id), (index.ids.get(index.idKey(id)) || 0) + 1);
      let siblings = counters.get(parent);
      if (!siblings) counters.set(parent, siblings = new Map());
      const nth = (siblings.get(tag) || 0) + 1;
      siblings.set(tag, nth);
      index.nth.set(el, nth);
      // A lowercased type selector does not match mixed-case foreign elements such as SVG linearGradient.
      if (namespaceOf(el) !== 'http://www.w3.org/1999/xhtml' && localNameOf(el) !== localNameOf(el).toLowerCase()) continue;
      const step = tag.toLowerCase() + ':nth-of-type(' + nth + ')';
      const list = index.steps.get(step);
      if (list) list.push(el); else index.steps.set(step, [el]);
    }
    pathIndexes.set(root, index);
    return index;
  }

  // Build a CSS selector that matches only `el`. Prefer `#id` when the id is
  // unique; otherwise prepend `tag:nth-of-type(n)` steps until the path
  // matches one element. Stopping early keeps the selector short; repeated
  // markup without ids may need a path anchored at <html>. Matches are
  // counted from the index; `with_ref` still verifies the selector on use.
  function cssPath(el) {
    const index = pathIndex(el.getRootNode());
    const unique = node => idOf(node) && index.ids.get(index.idKey(idOf(node))) === 1;
    if (unique(el)) return '#' + CSS.escape(idOf(el));
    const parts = [];
    // Elements the selector built so far would match, and their ancestor at the current step.
    let matches = null;
    let node = el;
    while (node) {
      if (unique(node)) {
        parts.unshift('#' + CSS.escape(idOf(node)));
        return parts.join(' > ');
      }
      const step = tagOf(node).toLowerCase() + ':nth-of-type(' + index.nth.get(node) + ')';
      parts.unshift(step);
      const stepElements = index.steps.get(step) || [];
      if (!matches) matches = stepElements.map(match => ({ match, at: match }));
      else {
        const allowed = new Set(stepElements);
        matches = matches.filter(entry => (entry.at = parentElementOf(entry.at)) && allowed.has(entry.at));
      }
      if (matches.length === 1) return parts.join(' > ');
      node = parentElementOf(node);
    }
    return parts.join(' > ');
  }

  // Values an agent would otherwise read with separate calls. Passwords, card details and passcodes are never read.
  function controlDetails(el, shownName) {
    if (!el.matches('input,textarea,select')) return null;
    const control = {};
    const type = el.matches('input') ? el.type : el.tagName.toLowerCase();
    if (type !== 'text') control.type = type;
    if (type === 'password' || type === 'file') return control;
    if (el.matches('select')) {
      control.option_count = el.options.length;
      control.options = Array.from(el.options).slice(0, 4).map(option => flat(option.label).slice(0, 40));
      const chosen = Array.from(el.selectedOptions, option => flat(option.label).slice(0, 40));
      if (chosen.length) control.selected = el.multiple ? chosen : chosen[0];
    } else if (!['checkbox','radio','submit','button','reset','image','color','hidden'].includes(type) && el.value) {
      // Card details and passcodes are often plain text fields; their purpose is declared here.
      if (/(^|\s)(cc-(number|csc|exp|exp-month|exp-year)|one-time-code|current-password|new-password)(\s|$)/i
          .test(el.getAttribute('autocomplete') || '')) control.redacted = true;
      else control.value = el.value.slice(0, 80);
    }
    const placeholder = flat(el.getAttribute('placeholder'));
    if (placeholder && placeholder !== shownName) control.placeholder = placeholder.slice(0, 80);
    for (const key of ['min', 'max', 'step']) if (el.getAttribute(key)) control[key] = el.getAttribute(key);
    return Object.keys(control).length ? control : null;
  }

  const refs = {};
  const nodes = new Map();
  // Dense tokens are shorter: a process-unique snapshot serial plus 32 random bits.
  const nonce = (dense ? Number(options.serial || 0).toString(36) : '') +
    Array.from(crypto.getRandomValues(new Uint32Array(dense ? 1 : 4)), n => n.toString(16).padStart(8, '0')).join('');
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
      if ((options.text == null && !el.matches('a,button,input,textarea,select,[role],h1,h2,h3,h4,h5,h6,label,summary,li,article,nav,main,audio,video,iframe,frame,dialog,form,output,[contenteditable]')) || !visible(el)) return;
      const role = implicitRole(el);
      if (options.text == null && options.role == null && !INTERACTIVE_ROLES.has(role) && !CONTENT_ROLES.has(role)) return;
      if (options.viewport && !inViewport(el)) return;
      const fullName = name(el);
      const label = Array.from(el.labels || [], label => name(label, true)).join(' ');
      if ((options.interactive && !INTERACTIVE_ROLES.has(role)) ||
          (options.role != null && role !== options.role) || !matches(fullName, options.name) ||
          !matches(label, options.label) || !matches(el.innerText, options.text) ||
          !matches(el.getAttribute('placeholder'), options.placeholder)) return;
      if (counter >= maxNodes || usedChars >= maxChars) { truncated = true; return; }
      counter += 1;
      const token = 'e' + counter + '-' + nonce;
      const sel = cssPath(el);
      const nm = fullName.slice(0, 120);
      refs[token] = dense ? { role: role, name: nm } : { role: role, name: nm, selector: sel };
      if (shadowPath.length) refs[token].shadow_path = shadowPath;
      if (framePath.length) refs[token].frame_path = framePath;
      if (el.matches('iframe,frame')) refs[token].frame_accessible = !!el.contentDocument;
      let enabled = !el.matches(':disabled');
      for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {
        if (parent.hasAttribute('inert') || parent.getAttribute('aria-disabled') === 'true') enabled = false;
      }
      const description = flat(el.getAttribute('aria-description')) || flat(
        flat(el.getAttribute('aria-describedby')).split(' ').map(id => el.getRootNode().getElementById(id)?.textContent || '').join(' '));
      if (description) refs[token].description = description.slice(0, 240);
      const states = {
        visible: true, disabled: !enabled, readonly: !!el.readOnly, enabled,
        checked: !!el.checked || el.getAttribute('aria-checked') === 'true',
        selected: !!el.selected || el.getAttribute('aria-selected') === 'true',
        expanded: el.getAttribute('aria-expanded') === 'true',
        editable: enabled && ((el.matches('input,textarea') && !['button','checkbox','color','file','hidden','image','radio','range','reset','submit'].includes(el.type) && !el.readOnly) || el.isContentEditable),
        focused: el.getRootNode().activeElement === el,
        invalid: el.getAttribute('aria-invalid') === 'true' || el.matches(':invalid'),
      };
      if (!dense) refs[token].states = states;
      else {
        // Defaults (visible, enabled, unchecked, collapsed, unfocused, valid) are omitted.
        const set = ['disabled', 'readonly', 'checked', 'selected', 'expanded', 'focused', 'invalid'].filter(key => states[key]);
        if (set.length) refs[token].states = set;
        const control = controlDetails(el, nm);
        if (control) refs[token].control = control;
      }
      const cost = dense ? JSON.stringify(refs[token]).length + token.length + 8
        : JSON.stringify(refs[token]).length + nm.length + token.length * 2 + 64;
      if (usedChars + cost > maxChars) { delete refs[token]; truncated = true; return; }
      usedChars += cost;
      nodes.set(token, { el, root: el.getRootNode(), frames, shadowPath, framePath, selector: sel });
      if (dense) return;
      const safe = nm.split(String.fromCharCode(34)).join('\\"');
      lines.push('- ' + role + ' "' + safe + '" [ref=' + token + ']');
    });
  }
  if (scope.shadowRoot) walk(scope.shadowRoot, [...(scopeRecord?.shadowPath || []), cssPath(scope)], scopeRecord?.framePath || [], scopeRecord?.frames || []);
  walk(scope, scopeRecord?.shadowPath || [], scopeRecord?.framePath || [], scopeRecord?.frames || []);

  const filtered = options.viewport || options.interactive ||
    ['role', 'name', 'label', 'text', 'placeholder'].some(key => options[key] != null);
  const sourceText = filtered ? Object.values(refs).map(meta => meta.name).join('\n') : (scope.body || scope)?.innerText || '';
  const text = sourceText.slice(0, Math.min(4000, Math.max(0, maxChars - usedChars)));
  if (text.length < sourceText.length) truncated = true;
  const page = {
    url: location.href,
    title: document.title,
    ready_state: document.readyState,
    text: text
  };
  if (dense) {
    // A hidden tab has a 0x0 viewport; page counts would be meaningless there.
    const view = document.scrollingElement || document.documentElement;
    page.scroll = innerHeight > 0 ? {
      x: Math.round(scrollX), y: Math.round(scrollY), viewport: [innerWidth, innerHeight],
      document: [view.scrollWidth, view.scrollHeight],
      pages_above: +(scrollY / innerHeight).toFixed(1),
      pages_below: +(Math.max(0, view.scrollHeight - scrollY - innerHeight) / innerHeight).toFixed(1)
    } : { hidden_viewport: true };
  }

  const result = {truncated, markdown: lines.join('\n') + (lines.length ? '\n' : ''), refs, page};
  let json = JSON.stringify(result);
  // Budget the serialized payload, including escaped labels and selector paths.
  while (json.length > maxChars && nodes.size) {
    const token = Array.from(nodes.keys()).pop();
    nodes.delete(token); delete refs[token]; lines.pop();
    result.truncated = true;
    result.markdown = lines.join('\n') + (lines.length ? '\n' : '');
    json = JSON.stringify(result);
  }
  if (json.length > maxChars) {
    page.text = ''; page.url = page.url.slice(0, 96); page.title = page.title.slice(0, 32);
    result.truncated = true;
    json = JSON.stringify(result);
  }
  if (dense) {
    // The server needs selectors for its ref registry; the CLI removes them before printing.
    for (const [token, record] of nodes) refs[token].selector = record.selector;
    json = JSON.stringify(result);
  }
  // Kept in the application script world, never in page attributes.
  globalThis.__flowmuxRefs = nodes;
  return json;
})()
"#;

/// Options are encoded as data, never interpolated as executable selectors.
pub fn snapshot_with_options(options: &serde_json::Value) -> String {
    let script = SNAPSHOT_JS.replacen("/*FLOWMUX_OPTIONS*/ {}", &options.to_string(), 1);
    format!("(function() {{ {FRAME_TRANSFORM_JS} return ({script}); }})()")
}

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

const FRAME_TRANSFORM_JS: &str = r#"function frameTransform(frame) {
for (let node = frame; node; node = node.parentElement || node.getRootNode().host) {
    const style = node.ownerDocument.defaultView.getComputedStyle(node);
    if (style.transform !== 'none') {
        const matrix = new DOMMatrixReadOnly(style.transform);
        if (!matrix.is2D || matrix.b !== 0 || matrix.c !== 0 || matrix.a <= 0 || matrix.d <= 0)
            throw new Error('rotated or skewed frames are not supported');
    }
    if ((style.rotate !== 'none' && style.rotate !== '0deg') || style.perspective !== 'none' || style.offsetPath !== 'none' ||
        (style.scale !== 'none' && style.scale.split(/\s+/).some(value => !Number.isFinite(Number(value)) || Number(value) <= 0)))
        throw new Error('rotated or perspective frames are not supported');
}
const bounds = frame.getBoundingClientRect();
const box = frame.ownerDocument.defaultView.getComputedStyle(frame);
const leftInset = parseFloat(box.borderLeftWidth) + parseFloat(box.paddingLeft);
const topInset = parseFloat(box.borderTopWidth) + parseFloat(box.paddingTop);
const width = parseFloat(box.width) + (box.boxSizing === 'border-box' ? 0 :
    leftInset + parseFloat(box.borderRightWidth) + parseFloat(box.paddingRight));
const height = parseFloat(box.height) + (box.boxSizing === 'border-box' ? 0 :
    topInset + parseFloat(box.borderBottomWidth) + parseFloat(box.paddingBottom));
if (!(width > 0 && height > 0)) throw new Error('frame is not visible');
return {x: bounds.left + leftInset * bounds.width / width, y: bounds.top + topInset * bounds.height / height, sx: bounds.width / width, sy: bounds.height / height};
}"#;

fn element_action(selector: &str, pointer: bool, enabled: bool, action: &str) -> String {
    format!(
        r#"(function() {{
            {FRAME_TRANSFORM_JS}
            const el = document.querySelector("{s}");
            if (!el) return "error: not found";
            if ((({pointer} || {enabled}) && el.closest('[inert]')) || ({enabled} && (el.matches(':disabled') || el.closest('[aria-disabled="true"]'))))
                return "error: element is disabled or inert";
            const style = getComputedStyle(el);
            if (![...el.getClientRects()].some(r => r.width > 0 && r.height > 0) || style.visibility !== 'visible')
                return "error: element is not visible";
            for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {{
                if ((({pointer} || {enabled}) && parent.hasAttribute('inert')) || ({enabled} && parent.getAttribute('aria-disabled') === 'true'))
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
                    const cover = hit;
                    // Activating a label reaches its own control, so the label does not cover it.
                    while (hit && hit !== target && hit.control !== target)
                        hit = hit.assignedSlot || hit.parentElement || hit.getRootNode().host;
                    if (!hit) {{
                        // Name the cover so the agent can dismiss it. Only a modal dialog that offers
                        // a control fails fast: it waits for an answer, while a progress modal or a
                        // non-modal panel can go away on its own and keeps the full wait.
                        const dialog = cover?.closest('dialog,[aria-modal="true"]');
                        const isModal = node => {{
                            if (node.getAttribute('aria-modal') === 'true') return true;
                            try {{ return node.matches(':modal'); }} catch (_) {{ return false; }}
                        }};
                        const modal = dialog && !dialog.contains(target) && isModal(dialog) && dialog.querySelector(
                            'button:not([disabled]),a[href],input:not([type=hidden]):not([disabled]),select:not([disabled]),textarea:not([disabled]),[role=button],[role=link]') ? dialog : null;
                        const shown = modal || cover;
                        const label = shown && (shown.getAttribute('aria-label') || shown.innerText || '').replace(/\s+/g, ' ').trim().slice(0, 60);
                        return "error: element is covered or cannot receive pointer input" + (shown ?
                            ` (covered by ${{modal ? 'modal ' : ''}}<${{shown.localName}}${{shown.id ? '#' + shown.id : ''}}${{shown.getAttribute('role') ? ' role=' + shown.getAttribute('role') : ''}}> "${{label}}")` : '');
                    }}
                    const frame = view.frameElement;
                    if (!frame) break;
                    const transform = frameTransform(frame);
                    x = transform.x + x * transform.sx;
                    y = transform.y + y * transform.sy;
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

/// Coordinates are CSS pixels in the top-level viewport; no implicit scrolling.
pub fn coordinate_target(x: f64, y: f64, hover: bool) -> String {
    format!(
        r#"(() => {{
        {FRAME_TRANSFORM_JS}
        let x = {x}, y = {y}, view = window, el;
        if (![x,y].every(Number.isFinite) || x < 0 || y < 0 || x >= innerWidth || y >= innerHeight)
            return 'error: pointer is outside the browser viewport';
        for (;;) {{
            el = view.document.elementFromPoint(x,y);
            while (el?.shadowRoot) {{
                const next = el.shadowRoot.elementFromPoint(x,y);
                if (!next || next === el) break;
                el = next;
            }}
            if (!el) return 'error: no element at pointer coordinates';
            if (!el.matches('iframe,frame')) break;
            if (!el.contentDocument) return 'error: cross-origin frame pointer completion is unavailable';
            const transform = frameTransform(el);
            x = (x-transform.x)/transform.sx;
            y = (y-transform.y)/transform.sy;
            view = el.contentWindow;
            if (x < 0 || y < 0 || x >= view.innerWidth || y >= view.innerHeight)
                return 'error: pointer is outside the frame content';
        }}
        if (!{hover} && el.matches(':disabled')) return 'error: element is disabled';
        for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {{
            if (parent.hasAttribute('inert') || (!{hover} && parent.getAttribute('aria-disabled') === 'true'))
                return 'error: element is disabled or inert';
            const style = parent.ownerDocument.defaultView.getComputedStyle(parent);
            if (Number(style.opacity) === 0 || style.display === 'none' || style.visibility !== 'visible')
                return 'error: element is not visible';
        }}
        globalThis.__flowmuxInput.el = el;
        return JSON.stringify({{x:{x},y:{y},width:innerWidth,height:innerHeight}});
    }})()"#
    )
}

pub fn drag_target(from: [f64; 2], to: [f64; 2]) -> String {
    let start = coordinate_target(from[0], from[1], false);
    let end = coordinate_target(to[0], to[1], false);
    format!(
        r#"(() => {{
        const start = {start};
        if (start.startsWith('error:')) return start;
        const source = globalThis.__flowmuxInput.el;
        const end = {end};
        if (end.startsWith('error:')) return end;
        globalThis.__flowmuxInput.dragSource = source;
        return JSON.stringify({{...JSON.parse(start), end:JSON.parse(end)}});
    }})()"#
    )
}

pub fn element_capture(selector: &str) -> String {
    element_action(
        selector,
        false,
        false,
        r#"
        const rect = el.getBoundingClientRect();
        let {x, y, width, height} = rect;
        let viewportOnly = false;
        for (let parent = el; parent; parent = parent.assignedSlot || parent.parentElement || parent.getRootNode().host || parent.ownerDocument.defaultView.frameElement) {
            const position = parent.ownerDocument.defaultView.getComputedStyle(parent).position;
            if (position === 'fixed' || position === 'sticky') viewportOnly = true;
        }
        let view = el.ownerDocument.defaultView;
        while (view.frameElement) {
            if (x < 0 || y < 0 || x + width > view.innerWidth || y + height > view.innerHeight)
                return 'error: element is clipped by its frame; scroll it into view first';
            const transform = frameTransform(view.frameElement);
            x = transform.x + x * transform.sx;
            y = transform.y + y * transform.sy;
            width *= transform.sx;
            height *= transform.sy;
            view = view.frameElement.ownerDocument.defaultView;
        }
        // Document snapshots can lay out fixed/sticky content at a different scroll position.
        if (x >= 0 && y >= 0 && x + width <= view.innerWidth && y + height <= view.innerHeight)
            return JSON.stringify({width:view.innerWidth, height:view.innerHeight, viewport:true,
                clip:{x, y, width, height}});
        if (viewportOnly) return 'error: fixed or sticky element is clipped; scroll it fully into view first';
        return JSON.stringify({
            width: Math.max(view.document.documentElement.clientWidth,view.document.documentElement.scrollWidth,view.document.body?.scrollWidth||0),
            height: Math.max(view.document.documentElement.clientHeight,view.document.documentElement.scrollHeight,view.document.body?.scrollHeight||0),
            clip: {x: x + view.scrollX, y: y + view.scrollY, width, height}
        });
    "#,
    )
}

pub fn prepare_pointer_input(geometry: &str, action: crate::input::PointerAction) -> String {
    use crate::input::PointerAction;
    let event = match action {
        PointerAction::Click => "click",
        PointerAction::DoubleClick => "dblclick",
        PointerAction::Hover => "pointermove",
        PointerAction::Drag { .. } => "pointerup",
    };
    format!(
        r#"
        if (globalThis.__flowmuxInput) throw new Error('browser input is already pending');
        const state = {{ received: false, event: '{event}' }};
        globalThis.__flowmuxInput = state;
        try {{
            const started = performance.now(), deadline = started + 5000;
            let before, targetElement, targetDragSource;
            while (true) {{
                try {{
                    before = {geometry};
                    if (before.startsWith('error:')) throw new Error(before);
                    if (targetElement && (targetElement !== state.el || targetDragSource !== state.dragSource))
                        throw new Error('stale pointer target moved or was replaced; inspect the page before retrying');
                    targetElement = state.el; targetDragSource = state.dragSource;
                    for (let frame = 0; frame < 2; frame++) {{
                        await new Promise((resolve, reject) => {{
                            const frame = requestAnimationFrame(() => {{clearTimeout(timer); resolve();}});
                            const timer = setTimeout(() => {{cancelAnimationFrame(frame); reject(new Error('browser did not render a stable pointer target'));}}, Math.max(1, deadline - performance.now()));
                        }});
                        const after = {geometry};
                        if (targetElement !== state.el || targetDragSource !== state.dragSource)
                            throw new Error('stale pointer target moved or was replaced; inspect the page before retrying');
                        if (before !== after)
                            throw new Error('pointer target moved; inspect the page before retrying');
                    }}
                    break;
                }} catch (error) {{
                    // A modal dialog asking for an answer does not close on its own; allow only a closing animation.
                    if (performance.now() >= deadline || String(error).includes('stale') ||
                        (String(error).includes('covered by modal') && performance.now() >= started + 750)) throw error;
                    await new Promise(resolve => setTimeout(resolve, 50));
                }}
            }}
            state.listener = event => {{
                if (event.isTrusted && (event.composedPath().includes(state.el) || event.composedPath().includes(state.dragSource))) {{
                    state.received = true;
                    if (state.done) queueMicrotask(state.done);
                }}
            }};
            state.windows = new Set([state.el.ownerDocument.defaultView]);
            if (state.dragSource) state.windows.add(state.dragSource.ownerDocument.defaultView);
            for (const view of state.windows) view.addEventListener(state.event, state.listener, true);
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
    for (const view of state?.windows || []) view.removeEventListener(state.event, state.listener, true);
    delete globalThis.__flowmuxInput;
    return 'ok';
})()"#;

pub const FINISH_POINTER_INPUT_JS: &str = r#"
const state = globalThis.__flowmuxInput;
if (!state) throw new Error('page changed during native pointer input');
return await new Promise(resolve => {
    let completionTimer;
    const finish = result => {
        clearTimeout(timer);
        clearTimeout(completionTimer);
        clearInterval(hoverPoll);
        for (const view of state.windows) view.removeEventListener(state.event, state.listener, true);
        if (globalThis.__flowmuxInput === state) delete globalThis.__flowmuxInput;
        resolve(result);
    };
    const timer = setTimeout(() => finish('error: native pointer input completion timed out'), 10000);
    // Disabled controls and page handlers can suppress the pointer event while CSS still changes.
    // :scope avoids the bare :hover quirk that excludes non-links in legacy pages.
    const hoverPoll = state.event === 'pointermove' ? setInterval(() => {
        if (state.el.isConnected && state.el.matches(':scope:hover')) finish('ok');
    }, 25) : null;
    // Capture listeners run before target handlers and native default actions.
    state.done = () => {
        if (state.event !== 'pointermove' || state.el.matches(':scope:hover'))
            completionTimer ??= setTimeout(() => finish('ok'), 0);
    };
    state.finishAfterDialog = () => finish('ok');
    if (state.dialogObserved) state.finishAfterDialog();
    else if (state.received || (state.event === 'pointermove' && state.el.matches(':scope:hover'))) state.done();
});
"#;

/// Check readiness without delivering input.
pub fn input_readiness(selector: &str, editable: bool) -> String {
    element_action(
        selector,
        false,
        true,
        if editable {
            "if (el.readOnly) return 'error: element is readonly'; return 'ok';"
        } else {
            "return 'ok';"
        },
    )
}

/// Set a text field value and dispatch input/change events.
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

/// Scroll by viewport heights: the document, or the nearest scroll container of
/// `el` (the element itself or an ancestor). Reports the resulting position.
pub fn scroll_pages(pages: f64, within: bool) -> String {
    format!(
        r#"(function(el, pages) {{
            const scrollable = node => node.scrollHeight > node.clientHeight + 1 &&
                /(auto|scroll|overlay)/.test(node.ownerDocument.defaultView.getComputedStyle(node).overflowY);
            let box = el;
            while (box && !scrollable(box)) box = box.parentElement;
            if (el && !box) throw new Error('ref has no scrollable container; scroll the page without --within');
            const page = !box;
            const target = box || document.scrollingElement || document.documentElement;
            const height = page ? innerHeight : target.clientHeight;
            if (!height) throw new Error('viewport is hidden: show the browser tab, or use read for content');
            const before = target.scrollTop;
            target.scrollBy({{top: pages * height, behavior: 'instant'}});
            const y = target.scrollTop, total = target.scrollHeight;
            const result = {{moved: y !== before, container: page ? 'document' : target.localName,
                y: Math.round(y), pages_above: +(y / height).toFixed(1),
                pages_below: +(Math.max(0, total - y - height) / height).toFixed(1)}};
            // An application shell keeps the document fixed and scrolls an inner area.
            if (page && total <= height + 1) {{
                let inner = null, area = 0;
                for (const node of document.querySelectorAll('*')) {{
                    const size = node.clientWidth * node.clientHeight;
                    if (size > area && scrollable(node)) {{ inner = node; area = size; }}
                }}
                if (inner) result.hint = 'the document does not scroll; pass --within with a ref inside <' +
                    inner.localName + (inner.id ? '#' + inner.id : '') + '>';
            }}
            return JSON.stringify(result);
        }})({root}, {pages})"#,
        root = if within { "el" } else { "null" },
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
            if (el.matches('input[type=password]')) throw new Error('password values are redacted');
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
            if (el.matches('input[type=password]') && "{n}".toLowerCase() === 'value') throw new Error('password values are redacted');
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

/// Queued while a native page dialog is open; runs after its page handler resumes.
pub const DIALOG_INPUT_COMPLETE_JS: &str = r#"(() => {
    const state = globalThis.__flowmuxInput;
    if (state) {
        state.dialogObserved = true;
        state.finishAfterDialog?.();
    }
    return 'ok';
})()"#;

pub const FINISH_KEY_INPUT_JS: &str = r#"
const state = globalThis.__flowmuxInput;
if (!state) throw new Error('page changed during native key input');
return await new Promise(resolve => {
    let completionTimer;
    const finish = result => {
        clearTimeout(timer);
        clearTimeout(completionTimer);
        for (const view of state.windows) {
            view.removeEventListener('keyup', state.listener, true);
            view.removeEventListener('keydown', state.keydown, true);
        }
        if (globalThis.__flowmuxInput === state) delete globalThis.__flowmuxInput;
        resolve(result);
    };
    const timer = setTimeout(() => finish('error: native key input completion timed out'), 10000);
    state.done = () => {
        completionTimer ??= setTimeout(() => finish(state.down?.defaultPrevented ? 'prevented' : 'ok'), 0);
    };
    // A modal dialog can receive keyup instead of the page; its presentation proves delivery.
    state.finishAfterDialog = () => finish('prevented');
    if (state.dialogObserved) state.finishAfterDialog();
    else if (state.received) state.done();
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

/// Body of the read-only page queries. `root` is a validated ref element or
/// null; `opts` is a serialized `BrowserReadQuery`. It never replaces
/// `__flowmuxRefs`, so the latest snapshot's refs stay valid.
const PAGE_READ_JS: &str = r#"
const flat = text => (text || '').replace(/\s+/g, ' ').trim();
const style = el => el.ownerDocument.defaultView.getComputedStyle(el);
const doc = root ? (root.ownerDocument || root) : document;
const SKIP = new Set(['script', 'style', 'noscript', 'template', 'head', 'svg', 'canvas', 'object', 'embed', 'textarea']);
// Rendered text in layout order: open shadow roots, slots and same-origin frames
// included; display:none subtrees and visibility-hidden text excluded.
function pageText(start, withLinks) {
  const lines = [];
  let line = '', marker = '', flushes = 0, lists = 0;
  const flush = () => {
    const text = line.replace(/[ \t ]+/g, ' ').trim();
    if (text) { lines.push(marker + text); marker = ''; }
    line = ''; flushes++;
  };
  const children = node => node.shadowRoot ? node.shadowRoot.childNodes :
    node.localName === 'slot' && node.assignedNodes().length ? node.assignedNodes() : node.childNodes;
  function walk(node, shown, pre) {
    if (node.nodeType === 3) {
      if (!shown) return;
      if (!pre) { line += node.data.replace(/\s+/g, ' '); return; }
      node.data.split('\n').forEach((part, index) => { if (index) flush(); line += part; });
      return;
    }
    if (node.nodeType === 9 || node.nodeType === 11) { for (const child of node.childNodes) walk(child, shown, pre); return; }
    if (node.nodeType !== 1 || SKIP.has(node.localName)) return;
    const el = node, cs = style(el), tag = el.localName;
    if (cs.display === 'none') return;
    const visible = cs.visibility === 'visible';
    if (tag === 'br') { flush(); return; }
    if (tag === 'iframe' || tag === 'frame') {
      let inner = null;
      try { inner = el.contentDocument; } catch (_) {}
      flush();
      if (inner?.body) walk(inner.body, true, false);
      else if (visible) lines.push('[frame not accessible: ' + flat(el.title || el.name || el.src).slice(0, 80) + ']');
      flush();
      return;
    }
    if (tag === 'img') { const alt = flat(el.getAttribute('alt')); if (visible && alt) line += ' [image: ' + alt + '] '; return; }
    if (tag === 'input') { if (visible && ['submit', 'button', 'reset'].includes(el.type) && el.value) line += ' ' + el.value + ' '; return; }
    if (tag === 'select') { if (visible) line += ' ' + Array.from(el.selectedOptions, option => option.label).join(', ') + ' '; return; }
    const display = cs.display;
    const block = display !== 'contents' && !display.startsWith('inline') && display !== 'table-cell';
    if (block) flush();
    if (display === 'table-cell' && line.trim()) line += ' | ';
    const list = tag === 'ul' || tag === 'ol';
    if (list) lists++;
    if (/^h[1-6]$/.test(tag)) marker = '#'.repeat(Number(tag[1])) + ' ';
    else if (display === 'list-item') marker = '  '.repeat(Math.max(0, lists - 1)) + '- ';
    const start = line.length, startFlushes = flushes;
    for (const child of children(el)) walk(child, visible, pre || cs.whiteSpace.startsWith('pre'));
    if (withLinks && tag === 'a' && el.href && !el.href.startsWith('javascript:')) {
      const label = line.slice(start).trim();
      if (flushes === startFlushes && label) line = line.slice(0, start) + ' [' + label + '](' + el.href + ') ';
      else line += ' (' + el.href + ')';
    }
    if (list) lists--;
    if (block) flush();
  }
  walk(start, true, false);
  flush();
  return lines.join('\n');
}
// Matches under `start`, then those in its open shadow roots and same-origin frames,
// the same reach as the text walk above.
function deepAll(start, selector) {
  const found = Array.from(start.querySelectorAll(selector));
  for (const host of start.querySelectorAll('*')) {
    let inner = host.shadowRoot;
    if (!inner && (host.localName === 'iframe' || host.localName === 'frame')) try { inner = host.contentDocument; } catch (_) {}
    if (inner) for (const match of deepAll(inner, selector)) found.push(match);
  }
  return found;
}
const textRoot = root || doc.body || doc.documentElement;
const page = {url: doc.URL, title: doc.title};
if (opts.op === 'read') {
  const full = pageText(textRoot, opts.links);
  const offset = Math.min(opts.offset, full.length);
  let end = Math.min(full.length, offset + opts.max_chars);
  // End on a line boundary unless that would drop more than half of the chunk.
  if (end < full.length) { const cut = full.lastIndexOf('\n', end); if (cut > offset + opts.max_chars / 2) end = cut + 1; }
  return JSON.stringify({...page, offset, text: full.slice(offset, end),
    next_offset: end < full.length ? end : null, total_chars: full.length});
}
if (opts.op === 'grep') {
  const lines = pageText(textRoot, false).split('\n');
  let test;
  if (opts.regex) { const re = new RegExp(opts.pattern, opts.ignore_case ? 'i' : ''); test = text => re.test(text); }
  else { const needle = opts.ignore_case ? opts.pattern.toLowerCase() : opts.pattern; test = text => (opts.ignore_case ? text.toLowerCase() : text).includes(needle); }
  const matches = [];
  let offset = 0, total = 0;
  lines.forEach((text, index) => {
    if (test(text) && ++total <= opts.max)
      matches.push({offset, line: index + 1, text: lines.slice(Math.max(0, index - opts.context), index + opts.context + 1).join('\n')});
    offset += text.length + 1;
  });
  return JSON.stringify({...page, matches, total, truncated: total > matches.length});
}
if (opts.op === 'links') {
  const needle = (opts.matching || '').toLowerCase(), seen = new Set(), links = [];
  let total = 0;
  for (const a of deepAll(textRoot, 'a[href]')) {
    if (!a.getClientRects().length || style(a).visibility !== 'visible' || a.href.startsWith('javascript:')) continue;
    const text = flat(a.innerText || a.getAttribute('aria-label') || a.title || a.querySelector('img[alt]')?.alt).slice(0, 160);
    const key = text + '\n' + a.href;
    if (seen.has(key) || (needle && !(text + ' ' + a.href).toLowerCase().includes(needle))) continue;
    seen.add(key);
    if (++total <= opts.max) links.push({text, href: a.href});
  }
  return JSON.stringify({...page, links, total, truncated: total > links.length});
}
if (opts.op === 'query') {
  const all = deepAll(root || doc, opts.selector);
  const items = all.slice(0, opts.max).map(el => {
    const item = {tag: el.localName, text: flat(el.innerText || el.textContent).slice(0, 200), visible: el.getClientRects().length > 0};
    const attrs = {};
    for (const name of opts.attrs) {
      if (name === 'value' && el.matches('input[type=password]')) continue;
      const value = name === 'value' && 'value' in el ? el.value : name === 'href' && el.href ? el.href : el.getAttribute(name);
      if (value != null) attrs[name] = String(value).slice(0, 500);
    }
    if (opts.attrs.length) item.attrs = attrs;
    return item;
  });
  return JSON.stringify({...page, count: all.length, items, truncated: all.length > items.length});
}
if (opts.op === 'options') {
  let options;
  if (root.matches('select')) {
    options = Array.from(root.options, o => ({value: o.value, label: flat(o.label), selected: o.selected, disabled: o.disabled}));
  } else if (root.list) {
    options = Array.from(root.list.options, o => ({value: o.value, label: flat(o.label || o.value), disabled: o.disabled}));
  } else {
    const owned = flat(root.getAttribute('aria-controls') || root.getAttribute('aria-owns')).split(' ')
      .map(id => id && root.getRootNode().getElementById(id)).find(Boolean);
    const box = root.matches('[role=listbox]') ? root : owned;
    if (!box) throw new Error('target is not a select, a datalist input or a listbox');
    options = Array.from(box.querySelectorAll('[role=option]'), o => ({label: flat(o.innerText || o.textContent),
      selected: o.getAttribute('aria-selected') === 'true', disabled: o.getAttribute('aria-disabled') === 'true'}));
  }
  return JSON.stringify({...page, options: options.slice(0, 500), total: options.length, truncated: options.length > 500});
}
throw new Error('unknown page read');
"#;

/// Read-only page query. With `within`, the caller wraps this in [`with_ref`],
/// which binds `el` to the validated ref element.
pub fn page_read(options: &serde_json::Value, within: bool) -> String {
    format!(
        "(function(root, opts) {{ {PAGE_READ_JS} }})({}, {options})",
        if within { "el" } else { "null" }
    )
}

/// Mark the current document in the automation world and return its URL. A
/// replacing document starts with a fresh world, so the mark distinguishes old
/// from new without touching page-visible state.
pub fn load_mark(marker: &str) -> String {
    format!(
        r#"(function() {{ globalThis.__flowmuxLoadMark = "{m}"; return location.href; }})()"#,
        m = js_string(marker)
    )
}

/// Report whether the marked document is still current and its readiness.
/// A blank-URL document also reports its text: engine error pages are blank-URL.
pub fn load_probe(marker: &str) -> String {
    format!(
        r#"(function() {{
            return JSON.stringify({{replaced: globalThis.__flowmuxLoadMark !== "{m}",
                ready_state: document.readyState, url: location.href, title: document.title,
                text: location.href === 'about:blank' ? (document.body?.innerText || '').trim().slice(0, 200) : undefined}});
        }})()"#,
        m = js_string(marker)
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
