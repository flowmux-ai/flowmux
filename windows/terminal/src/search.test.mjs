// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { SearchUi } from './search.mjs';
import { Output } from './output.mjs';

function fixture() {
  const elements = new Map(), calls = [];
  const document = { getElementById(id) {
    if (!elements.has(id)) {
      const handlers = new Map();
      elements.set(id, { value: '', checked: false, hidden: true, textContent: '',
        addEventListener: (type, handler) => handlers.set(type, handler),
        fire: (type, event = {}) => handlers.get(type)?.(event),
        focus: () => calls.push('query-focus'), setAttribute() {}, removeAttribute() {} });
    }
    return elements.get(id);
  } };
  const terminal = { focus: () => calls.push('terminal-focus'), clearSelection: () => calls.push('clear'),
    loadAddon() {}, onResize() {},
    getSelection: () => '한글', getSelectionPosition: () => ({ start: {x:2,y:1}, end:{x:6,y:1} }),
    buffer: { active: { viewportY: 0, type: 'normal' } } };
  const addon = { clearDecorations() {}, dispose: () => calls.push('dispose'),
    findNext: (query, options) => { calls.push({query, options, direction:'next'}); return true; },
    findPrevious: (query, options) => { calls.push({query, options, direction:'previous'}); return true; } };
  return { ui: new SearchUi(terminal, () => addon, document), calls, elements, terminal };
}

test('IME confirmation and cancellation never navigate or close the find bar', () => {
  const {ui, calls} = fixture();
  ui.open(false); ui.query.value = '한글';
  const forbidden = () => { throw new Error('IME event was intercepted'); };
  ui.query.fire('compositionstart');
  ui.query.fire('input', {isComposing:true});
  ui.query.fire('keydown', {key:'Enter', preventDefault:forbidden});
  ui.query.fire('keydown', {key:'Escape', preventDefault:forbidden});
  assert.equal(ui.bar.hidden, false);
  assert.deepEqual(calls, []);
  ui.query.fire('compositionend');
  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.incremental, true);
  ui.query.fire('keydown', {key:'Enter', keyCode:229, preventDefault:forbidden});
  ui.query.fire('keydown', {key:'Enter', isComposing:true, preventDefault:forbidden});
  assert.equal(calls.length, 1);
  ui.query.fire('keydown', {key:'Enter', shiftKey:true, preventDefault() {}});
  assert.equal(calls.at(-1).direction, 'previous');
  ui.query.fire('keydown', {key:'Escape', preventDefault() {}});
  assert.equal(ui.bar.hidden, true);
  assert.equal(calls.at(-1), 'terminal-focus');
});

test('invalid regex clears stale selection; remote finds preserve focus and options', () => {
  const {ui, calls} = fixture();
  const request = { query: '한글', previous:false, match_case:true, regex:false, close:false, focus:false };
  const valid = ui.run(request);
  assert.equal(valid.found, true);
  assert.equal(valid.match_case, true);
  assert.equal(calls.includes('query-focus'), false);
  const invalid = ui.run({...request, query:'[', regex:true});
  assert.equal(invalid.error, 'Invalid regular expression');
  assert.equal(calls.at(-1), 'clear');
  assert.equal(ui.run({...request, query:''}).status, '');
  assert.equal(ui.run({...request, close:true}).visible, false);
  assert.equal(calls.includes('terminal-focus'), false);
});

test('find waits for completed terminal parsing and returns that sequence', () => {
  const replies = [], callbacks = [];
  const {ui, terminal, calls} = fixture();
  terminal.write = (_bytes, callback) => callbacks.push(callback);
  const output = new Output(terminal, message => replies.push(message), () => {}, message => ui.run(message), () => ui.invalidate());
  output.receive({type:'output', sequence:1, data:Buffer.from('한글').toString('base64')});
  output.receive({type:'find', request:'find', after:1, query:'한글', focus:false});
  assert.deepEqual(calls, []);
  callbacks[0]();
  assert.equal(replies.at(-1).type, 'found');
  assert.equal(replies.at(-1).sequence, 1);
  assert.equal(replies.at(-1).result.selection, '한글');
  assert.equal(calls[0], 'dispose', 'cached search lines must be discarded before a barrier find');
});

test('bounded search selection never cuts an emoji surrogate pair in the Rust bridge', () => {
  const {ui, terminal} = fixture();
  terminal.getSelection = () => 'a'.repeat(16383) + '🙂';
  const result = ui.result(true);
  assert.equal(result.selection, 'a'.repeat(16383));
  assert.equal(result.selection_truncated, true);
});
