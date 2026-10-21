// SPDX-License-Identifier: GPL-3.0-or-later
// The registry lives in this document; navigation invalidates every old reference.
const key = 'element-6066-11e4-a52e-4f735466cecf';
const fail = (error, message) => { throw {error, message}; };
const registry = window[registryName] ?? (() => {
    const state = {nodes: new Map(), ids: new WeakMap()};
    Object.defineProperty(window, registryName, {value: state});
    return state;
})();
const element = id => {
    const node = registry.nodes.get(id);
    if (!node || !node.isConnected || node.ownerDocument !== document)
        fail('stale element reference', 'Element is no longer in this document');
    return node;
};
const encode = (value, seen = new Set()) => {
    if (value instanceof Element) {
        let id = registry.ids.get(value);
        if (!id) {
            id = Array.from(crypto.getRandomValues(new Uint32Array(4)), n => n.toString(16)).join('-');
            registry.ids.set(value, id);
            registry.nodes.set(id, value);
        }
        return {[key]: id};
    }
    if (value == null) return null;
    if (typeof value !== 'object') return value;
    if (seen.has(value)) fail('javascript error', 'Cyclic script result');
    seen.add(value);
    const result = Array.isArray(value) || value instanceof NodeList || value instanceof HTMLCollection
        ? Array.from(value, item => encode(item, seen))
        : Object.fromEntries(Object.entries(value).map(([name, item]) => [name, encode(item, seen)]));
    seen.delete(value);
    return result;
};
const decode = value => {
    if (value && typeof value === 'object') {
        if (Object.hasOwn(value, key)) return element(value[key]);
        return Array.isArray(value) ? value.map(decode)
            : Object.fromEntries(Object.entries(value).map(([name, item]) => [name, decode(item)]));
    }
    return value;
};
const shown = node => {
    const style = getComputedStyle(node);
    return node.getClientRects().length > 0 && style.visibility !== 'hidden' && style.display !== 'none';
};
const center = node => {
    node.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'});
    const r = node.getBoundingClientRect();
    const x = (Math.max(0, r.left) + Math.min(innerWidth, r.right)) / 2;
    const y = (Math.max(0, r.top) + Math.min(innerHeight, r.bottom)) / 2;
    if (!shown(node) || r.width <= 0 || r.height <= 0)
        fail('element not interactable', 'Element has no visible area');
    const hit = document.elementFromPoint(x, y);
    if (!hit || (hit !== node && !node.contains(hit)))
        fail('element click intercepted', 'Another element covers the click target');
    return {x, y};
};
const find = (root, using, value) => {
    try {
        switch (using) {
        case 'css selector': return Array.from(root.querySelectorAll(value));
        case 'tag name': return Array.from(root.getElementsByTagName(value));
        case 'link text': return Array.from(root.querySelectorAll('a')).filter(a => a.innerText.trim() === value);
        case 'partial link text': return Array.from(root.querySelectorAll('a')).filter(a => a.innerText.includes(value));
        case 'xpath': {
            const result = document.evaluate(value, root, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null);
            const nodes = Array.from({length: result.snapshotLength}, (_, i) => result.snapshotItem(i));
            if (nodes.some(n => !(n instanceof Element))) throw Error('XPath must select elements');
            return nodes;
        }
        }
    } catch (error) { fail('invalid selector', String(error)); }
    fail('invalid selector', 'Unsupported locator');
};
