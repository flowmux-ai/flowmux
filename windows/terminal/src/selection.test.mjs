// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Selection, MAX_SELECTION_BYTES } from './selection.mjs';

function fixture() {
  let text = '', changed, bufferChanged, reset;
  const terminal = { cols: 80, buffer: { active: { type: 'normal', length: 10 }, onBufferChange: fn => { bufferChanged = fn; } },
    parser: { registerEscHandler: (_, fn) => { reset = fn; } },
    onSelectionChange: fn => { changed = fn; }, getSelection: () => text,
    getSelectionPosition: () => text ? { start: { x: 0, y: 1 }, end: { x: 4, y: 1 } } : undefined,
    clearSelection: () => { text = ''; changed(); }, select: () => { text = '한글'; changed(); },
    selectAll: () => { text = '한글\r\n한 😀'; changed(); } };
  const selection = new Selection(terminal);
  return { selection, terminal, change: value => { text = value; changed(); }, bufferChanged: () => bufferChanged(), reset: () => reset() };
}

test('copy retains the selected Unicode snapshot after redraw, trim and loss of live selection', () => {
  const f = fixture(), original = '한글 한 😀';
  f.selection.primaryDown(); f.change(original); f.selection.primaryUp();
  for (const text of ['different text at same cells', 'part', '']) {
    f.change(text);
    assert.equal(f.selection.read().text, original);
    assert.equal(f.selection.read().source, 'retained');
  }
});

test('new primary selection, explicit clear, alternate screen and RIS cannot resurrect an old snapshot', async () => {
  const f = fixture(); f.selection.run({ kind: 'all' });
  // Browser checkpoints can run between pointerdown and compatibility mousedown.
  f.selection.primaryDown(); await Promise.resolve(); f.change(''); f.selection.primaryUp();
  assert.equal(f.selection.read().text, '');
  f.selection.run({ kind: 'all' }); f.selection.run({ kind: 'clear' });
  assert.equal(f.selection.read().source, 'none');
  for (const end of [f.bufferChanged, f.reset]) {
    f.selection.run({ kind: 'all' }); f.change(''); end();
    assert.equal(f.selection.read().text, '');
  }
});

test('oversized selections replace the fallback with an error and never truncate or copy earlier text', () => {
  const f = fixture(); f.selection.run({ kind: 'all' });
  f.selection.primaryDown(); f.change('한'.repeat(MAX_SELECTION_BYTES / 3 + 1)); f.selection.primaryUp();
  let writes = 0; const messages = [];
  f.selection.copyEvent({ preventDefault() {}, stopImmediatePropagation() {}, clipboardData: { setData() { writes++; } } }, m => messages.push(m));
  assert.equal(writes, 0); assert.match(messages[0], /128 KiB/);
  assert.equal(f.selection.read().text, '');
});

test('copy event uses exact plain text once and leaves the clipboard alone when empty or blocked', () => {
  const f = fixture(), writes = [], events = [];
  const event = { preventDefault() { events.push('prevent'); }, stopImmediatePropagation() { events.push('stop'); },
    clipboardData: { setData: (type, text) => writes.push({ type, text }) } };
  f.selection.run({ kind: 'all' }); f.change('');
  f.selection.copyEvent(event, () => {});
  assert.deepEqual(writes, [{ type: 'text/plain', text: '한글\r\n한 😀' }]);
  assert.deepEqual(events, ['prevent', 'stop']);
  f.selection.copyEvent(event, () => {}, false);
  f.selection.run({ kind: 'clear' }); f.selection.copyEvent(event, () => {});
  assert.equal(writes.length, 1);
});

test('invalid range is atomic; valid ranges use cell coordinates without taking focus', () => {
  const f = fixture(); f.selection.run({ kind: 'all' }); const before = f.selection.read().text;
  for (const action of [{ row: -1, column: 0, length: 1 }, { row: 10, column: 0, length: 1 },
    { row: 0, column: 80, length: 1 }, { row: 0, column: 0, length: 801 }, { row: 0, column: 0, length: 0 }]) {
    assert.match(f.selection.run({ kind: 'range', ...action }).error, /outside/);
    assert.equal(f.selection.read().text, before);
  }
  assert.equal(f.selection.run({ kind: 'range', row: 1, column: 0, length: 4 }).text, '한글');
});
