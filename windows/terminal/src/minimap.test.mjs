// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Minimap, paletteColor, cellPaint, previewWindow, followViewport, pointerRow } from './minimap.mjs';
const theme = { foreground: '#abcdef', background: '#010203' };
function cell(chars = '한', width = 2, overrides = {}) {
  return { getChars: () => chars, getWidth: () => width, isFgRGB: () => false, isBgRGB: () => false,
    isFgPalette: () => false, isBgPalette: () => false, isBold: () => false, isDim: () => false,
    isInverse: () => false, isInvisible: () => false, ...overrides };
}
function fixture() {
  const events = {}, listeners = {}, attributes = {}, tasks = new Map(), fills = [], props = {};
  let next = 0, focused = 0, reads = 0, blocked = false;
  const ctx = { setTransform() {}, fillRect(...coords) { fills.push({ color: this.fillStyle, alpha: this.globalAlpha, coords }); },
    getImageData: () => ({ data: new Uint8Array([fills.length % 256]) }) };
  const canvas = { style: {}, getContext: () => ctx }, thumb = { style: {} };
  const area = { style: {}, height: 100, getBoundingClientRect() { return { top: 5, height: this.height, width: 40 }; },
    querySelector: name => name === 'canvas' ? canvas : thumb,
    addEventListener: (name, fn) => { listeners[name] = fn; }, setAttribute: (key, value) => { attributes[key] = value; },
    setPointerCapture() {},
  };
  const document = { body: { clientWidth: 200, classList: { toggle() {} }, style: { setProperty: (key, value) => { props[key] = value; } } },
    defaultView: { devicePixelRatio: 1 }, getElementById: id => id === 'minimap' ? area : { getBoundingClientRect: () => ({ right: 195 - parseFloat(props['--minimap-width']) }) } };
  const row = [cell('A', 1), cell('한', 2, { isFgRGB: () => true, getFgColor: () => 0x123456 }), cell('', 0), cell(' ', 1)];
  const buffer = { type: 'normal', length: 500, viewportY: 490, baseY: 490, getNullCell: () => ({}),
    getLine: () => { reads++; return { getCell: x => row[x] }; } };
  const terminal = { cols: 4, rows: 10, options: { theme }, buffer: { active: buffer, onBufferChange: fn => { events.buffer = fn; } },
    onScroll: fn => { events.scroll = fn; }, onResize: fn => { events.resize = fn; },
    scrollToLine: n => { buffer.viewportY = n; events.scroll(); }, focus: () => focused++ };
  const clock = { set: fn => { tasks.set(++next, fn); return next; }, clear: id => tasks.delete(id) };
  const minimap = new Minimap(terminal, document, () => blocked, clock);
  minimap.configure({ minimap_enabled: true, minimap_width: 40, minimap_opacity: 50 }); minimap.visibility(true);
  return { minimap, terminal, buffer, area, canvas, attributes, listeners, fills, props, tasks, events,
    get focused() { return focused; }, get reads() { return reads; }, block: b => { blocked = b; },
    tick() { const due = [...tasks.values()]; tasks.clear(); due.forEach(fn => fn()); } };
}
test('minimap resolves ANSI, RGB, inverse, dim and invisible cell attributes', () => {
  assert.equal(paletteColor(196, theme), '#ff0000'); assert.equal(paletteColor(255, theme), '#eeeeee');
  assert.equal(paletteColor(1, { red: '#123456' }), '#123456');
  const value = cell('한', 2, { isFgPalette: () => true, getFgColor: () => 1, isBold: () => true,
    isBgRGB: () => true, getBgColor: () => 0x010203, isDim: () => true, isInverse: () => true });
  assert.deepEqual(cellPaint(value, theme), { fg: '#010203', bg: '#ef2929', ink: true, alpha: .5 });
  assert.equal(cellPaint(cell('한', 2, { isInvisible: () => true }), theme).ink, false);
  assert.equal(cellPaint(cell(' '), theme).ink, false);
});
test('preview geometry remains bounded and navigation uses physical rows', () => {
  assert.deepEqual(previewWindow(5000, 6000, 0), { top: 2952, rows: 2048, offset: 0 });
  const w = previewWindow(5000, 100, 250);
  assert.equal(pointerRow(w, 50, 24, 4976), 4688);
  assert.equal(pointerRow(w, -500, 24, 4976), 4638);
  assert.equal(followViewport(w, 10, 24, 5000), 4890);
  assert.equal(previewWindow(5, 100, 1000).top, 0);
});
test('raster reads only preview cells and counts wide characters once', () => {
  const f = fixture(), result = f.minimap.run({ kind: 'read' }).snapshot;
  assert.equal(f.reads, 100); assert.equal(result.cells, 400); assert.equal(result.ink_cells, 200);
  assert.equal(result.wide_cells, 100); assert.equal(result.gutter, 40);
  assert.ok(f.fills.some(p => p.color === '#123456' && p.coords[2] === 20));
  assert.equal(result.raster_hash.length, 8); assert.equal(f.focused, 0);
  f.minimap.run({ kind: 'read' }); assert.equal(f.reads, 100);
});
test('wheel preview leaves terminal viewport alone; seek follows without focus or input', () => {
  const f = fixture(); f.minimap.run({ kind: 'preview', rows: -25 });
  assert.equal(f.buffer.viewportY, 490); assert.equal(f.minimap.offset, 25);
  const result = f.minimap.run({ kind: 'seek', row: 10 }).snapshot;
  assert.equal(result.viewport_row, 5); assert.equal(result.preview_top, 5); assert.equal(f.focused, 0);
  assert.equal(f.minimap.run({ kind: 'seek', row: 500 }).status, 'error'); assert.equal(f.buffer.viewportY, 5);
});
test('refresh keeps its first deadline and hidden or alternate views release raster work', () => {
  const f = fixture(); f.minimap.changed(); f.minimap.changed();
  assert.equal(f.tasks.size, 1); f.tick(); assert.equal(f.minimap.renders, 1);
  f.minimap.changed(); f.minimap.changed(); f.tick(); assert.equal(f.minimap.renders, 2);
  f.minimap.changed(); f.minimap.visibility(false);
  assert.equal(f.tasks.size, 0); assert.equal(f.canvas.width, 1);
  f.minimap.changed(); assert.equal(f.tasks.size, 0);
  assert.equal(f.minimap.run({ kind: 'read' }).snapshot.cells, 0);
  assert.equal(f.minimap.run({ kind: 'preview', rows: -25 }).status, 'error');
  f.minimap.visibility(true); f.tick(); assert.equal(f.minimap.renders, 3);
  f.buffer.type = 'alternate'; f.events.buffer();
  assert.equal(f.area.hidden, true); assert.equal(f.props['--minimap-width'], '40px'); assert.equal(f.tasks.size, 0);
  f.buffer.type = 'normal'; f.events.buffer(); f.tick(); assert.equal(f.minimap.renders, 4);
  f.minimap.preview(-50);
  f.minimap.configure({ minimap_enabled: false, minimap_width: 12, minimap_opacity: 0 });
  assert.equal(f.minimap.offset, 0); assert.equal(f.props['--minimap-width'], '0px');
  assert.equal(f.tasks.size, 0); assert.equal(f.minimap.run({ kind: 'read' }).snapshot.raster_hash, null);
});
test('composition leaves navigation keys and pointer untouched; ordinary keyboard scrolls', () => {
  const f = fixture(); let prevented = 0;
  const event = { key: 'Home', preventDefault: () => prevented++, stopPropagation() {} };
  f.listeners.keydown({ ...event, isComposing: true }); assert.equal(prevented, 0);
  f.block(true); f.listeners.pointerdown({ button: 0, pointerId: 1, clientY: 30, ...event }); assert.equal(f.focused, 0);
  assert.equal(f.minimap.run({ kind: 'seek', row: 0 }).status, 'error');
  f.block(false); f.listeners.keydown(event); assert.equal(prevented, 1); assert.equal(f.buffer.viewportY, 0);
  assert.equal(f.focused, 0);
});
