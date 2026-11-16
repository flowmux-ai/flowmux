// SPDX-License-Identifier: GPL-3.0-or-later
(() => {
  globalThis.__flowmuxFeedback?.stop();
  const state = { result: null, stop, read };
  globalThis.__flowmuxFeedback = state;
  const overlay = document.createElement('div');
  overlay.style.cssText = 'all:initial!important;position:fixed!important;pointer-events:none!important;border:2px solid #5b9dff!important;background:#5b9dff22!important;box-sizing:border-box!important;z-index:2147483647!important;display:none!important';
  // Intercept pointer input before it enters embedded frames or page controls.
  const shield = document.createElement('div');
  shield.style.cssText = 'all:initial!important;position:fixed!important;inset:0!important;z-index:2147483646!important;cursor:crosshair!important';
  document.documentElement.append(shield);
  document.documentElement.append(overlay);
  let hovered = null;
  let selected = null;
  const handlers = [];
  function listen(type, fn) {
    window.addEventListener(type, fn, true);
    handlers.push([type, fn]);
  }
  function stop() {
    for (const [type, fn] of handlers) window.removeEventListener(type, fn, true);
    overlay.remove();
    shield.remove();
  }
  function block(event) { event.preventDefault(); event.stopImmediatePropagation(); }
  function read() {
    if (selected) {
      const { element, rect, viewport, scroll } = selected;
      const current = element.getBoundingClientRect();
      if (!element.isConnected || innerWidth !== viewport.width || innerHeight !== viewport.height ||
          scrollX !== scroll.x || scrollY !== scroll.y ||
          ['x', 'y', 'width', 'height'].some(key => current[key] !== rect[key]))
        state.result = { error: 'The selected element or viewport changed. Select the element again.' };
    }
    return state.result;
  }
  function target(event) {
    if (event.type === 'focusin') return event.composedPath().find(node => node instanceof Element);
    shield.style.setProperty('pointer-events', 'none', 'important');
    let element = document.elementFromPoint(event.clientX, event.clientY);
    while (element?.shadowRoot) {
      const inner = element.shadowRoot.elementFromPoint(event.clientX, event.clientY);
      if (!inner || inner === element) break;
      element = inner;
    }
    shield.style.removeProperty('pointer-events');
    return element;
  }
  function highlight(element) {
    hovered = element;
    if (!element) return;
    const r = element.getBoundingClientRect();
    for (const [key, value] of Object.entries({ left: r.left, top: r.top, width: r.width, height: r.height }))
      overlay.style.setProperty(key, `${value}px`, 'important');
    overlay.style.setProperty('display', 'block', 'important');
  }
  function select(element) {
    if (!element) return;
    // shortcut: frame contents need separate pickers; add them when cross-frame selection is required.
    if (element.matches('iframe,frame,object,embed')) {
      state.result = { error: 'Embedded content cannot be selected. Choose an element in the main page.' };
      stop(); return;
    }
    // Never serialize form values, arbitrary attributes or whole subtrees.
    if (element.closest('input,textarea,select,[contenteditable]:not([contenteditable="false"])')) {
      state.result = { error: 'Choose a surrounding label or container instead of an input field.' };
      stop(); return;
    }
    const r = element.getBoundingClientRect();
    const x = Math.max(0, r.x), y = Math.max(0, r.y);
    const width = Math.min(innerWidth, r.right) - x, height = Math.min(innerHeight, r.bottom) - y;
    if (width <= 0 || height <= 0) return;
    let selector = '', current = element;
    for (let depth = 0; current && depth < 6; depth++, current = current.parentElement) {
      let part = current.localName;
      if (current.id) { part += '#' + CSS.escape(current.id); selector = part + (selector ? ' > ' + selector : ''); break; }
      if (current.parentElement) {
        const siblings = [...current.parentElement.children].filter(node => node.localName === current.localName);
        if (siblings.length > 1) part += `:nth-of-type(${siblings.indexOf(current) + 1})`;
      }
      selector = part + (selector ? ' > ' + selector : '');
    }
    const styles = {}, computed = getComputedStyle(element);
    for (const name of ['display','position','width','height','color','background-color','font-family','font-size','font-weight','line-height','margin','padding','gap','border','border-radius','align-items','justify-content'])
      styles[name] = computed.getPropertyValue(name).slice(0, 300);
    const url = new URL(location.href);
    url.username = ''; url.password = ''; url.search = ''; url.hash = '';
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
    let text = '', node, count = 0;
    while ((node = walker.nextNode()) && count++ < 200 && text.length < 500) {
      if (node.parentElement.getClientRects().length && getComputedStyle(node.parentElement).visibility === 'visible' &&
          !node.parentElement.closest('script,style,input,textarea,select,[contenteditable]:not([contenteditable="false"]),[hidden],[aria-hidden="true"]'))
        text += node.textContent.slice(0, 500 - text.length) + ' ';
    }
    state.result = {
      url: url.href, tag: element.localName, selector: selector.slice(0, 1000),
      text: text.replace(/\s+/g, ' ').trim().slice(0, 500), styles,
      viewport: { width: innerWidth, height: innerHeight },
      rect: { x, y, width, height },
      context: element.getRootNode() instanceof ShadowRoot ? 'Inside an open shadow root; selector is relative to that root.' : 'Top-level document.'
    };
    selected = { element, rect: r, viewport: state.result.viewport, scroll: { x: scrollX, y: scrollY } };
    stop();
  }
  listen('pointermove', event => { event.stopImmediatePropagation(); highlight(target(event)); });
  listen('focusin', event => highlight(target(event)));
  for (const type of ['pointerdown','pointerup','mousedown','mouseup','dblclick','contextmenu','touchstart','touchend']) listen(type, block);
  listen('click', event => { block(event); select(target(event)); });
  listen('beforeinput', block);
  listen('keydown', event => {
    if (event.key === 'Tab') event.stopImmediatePropagation();
    else block(event);
  });
  // Finish on key release so the selection key cannot reach page keyup handlers.
  listen('keyup', event => {
    block(event);
    if (event.key === 'Escape') { state.result = { cancelled: true }; stop(); }
    else if (event.key === 'Enter' || event.key === ' ') select(hovered || document.activeElement);
  });
  return 'ready';
})()
