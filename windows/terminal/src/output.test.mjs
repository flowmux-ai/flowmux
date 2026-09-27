// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Output } from './output.mjs';
import { Paste } from './paste.mjs';

test('split Korean UTF-8 stays bytes and screen waits for parser completion', () => {
  const callbacks = [], writes = [], replies = [];
  const terminal = { cols: 80, rows: 1, buffer: { active: { type: 'normal', length: 1,
    viewportY: 0, baseY: 0, cursorY: 0, cursorX: 0, getLine: () => ({ translateToString: () => '한글' }) } },
    write: (bytes, callback) => { writes.push(bytes); callbacks.push(callback); } };
  const output = new Output(terminal, message => replies.push(message), () => 'serialized');
  const bytes = new TextEncoder().encode('한글');
  output.receive({ type: 'output', sequence: 1, data: Buffer.from(bytes.slice(0, 2)).toString('base64') });
  output.receive({ type: 'read_screen', request: 'read', after: 2 });
  output.receive({ type: 'output', sequence: 2, data: Buffer.from(bytes.slice(2)).toString('base64') });
  assert.equal(replies.length, 0);
  callbacks[0]();
  assert.equal(replies.length, 1);
  callbacks[1]();
  assert.deepEqual(Buffer.concat(writes), Buffer.from(bytes));
  assert.equal(replies.at(-1).type, 'screen');
  assert.equal(replies.at(-1).request, 'read');
  assert.equal(replies.at(-1).sequence, 2);
  assert.equal(replies.at(-1).outcome.snapshot.text, '한글');
  assert.throws(() => output.receive({ type: 'output', sequence: 2, data: '' }), /Out-of-order/);
});

test('recent reads share parser barriers and screen errors do not strand later requests', () => {
  const replies = [], callbacks = [];
  let large = true;
  const terminal = { rows: 1, cols: 80, buffer: { active: { type: 'normal', length: 100,
    viewportY: 0, baseY: 99, cursorY: 0, cursorX: 0,
    getLine: row => ({ translateToString: () => large ? '한'.repeat(50000) : `ROW_${row}` }),
  } }, write: (_, done) => callbacks.push(done) };
  const output = new Output(terminal, result => replies.push(result));
  output.receive({ type: 'read_screen', request: 'oversize', after: 1, recent: true });
  output.receive({ type: 'output', sequence: 1, data: 'QQ==' });
  assert.deepEqual(replies, []);
  callbacks[0]();
  assert.match(replies.at(-1).outcome.message, /128 KiB/);
  large = false;
  output.receive({ type: 'read_screen', request: 'retry', after: 1, recent: true });
  assert.equal(replies.at(-1).outcome.snapshot.metadata.first_row, 20);
  assert.equal(replies.at(-1).outcome.snapshot.text.split('\n').length, 80);
  output.flush();
  assert.equal(replies.filter(r => r.type === 'screen').length, 2);
});

test('paste waits for pending mode output to finish parsing and emits only one outcome', () => {
  const callbacks = [], replies = [], modes = [], calls = [];
  const terminal = { options: {}, modes: { bracketedPasteMode: false },
    write: (_bytes, callback) => callbacks.push(callback),
    paste: text => { calls.push(text); modes.push(terminal.modes.bracketedPasteMode); paste.data('xterm-result'); } };
  const paste = new Paste(terminal, () => null);
  const output = new Output(terminal, message => replies.push(message), null, null, null, null, paste);
  output.receive({ type: 'output', sequence: 1, data: 'QQ==' });
  output.receive({ type: 'paste', after: 2, request: 'paste', text: '한글' });
  output.receive({ type: 'output', sequence: 2, data: 'Qg==' });
  callbacks[0]();
  assert.deepEqual(calls, []);
  terminal.modes.bracketedPasteMode = true;
  callbacks[1]();
  output.flush();
  assert.deepEqual(modes, [true]);
  assert.deepEqual(calls, ['한글']);
  assert.deepEqual(replies.at(-1), { type: 'pasted', request: 'paste', sequence: 2,
    outcome: { status: 'ok', data: 'xterm-result', bracketed: true } });
  assert.equal(replies.filter(r => r.type === 'pasted').length, 1);
});

test('selection actions wait for parsed output and return the exact completed sequence', () => {
  const replies = [], callbacks = [], actions = [];
  const terminal = { write: (_, done) => callbacks.push(done) };
  const selection = { run: action => { actions.push(action); return { text: '한글' }; } };
  const output = new Output(terminal, result => replies.push(result), null, null, null, null, null, selection);
  output.receive({ type: 'selection', request: 'select', after: 1, action: { kind: 'all' } });
  output.receive({ type: 'output', sequence: 1, data: 'QQ==' });
  assert.deepEqual(actions, []);
  callbacks[0]();
  assert.deepEqual(actions, [{ kind: 'all' }]);
  assert.deepEqual(replies.at(-1), { type: 'selected', request: 'select', sequence: 1, result: { text: '한글' } });
});
