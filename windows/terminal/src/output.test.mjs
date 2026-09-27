// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Output } from './output.mjs';

test('split Korean UTF-8 stays bytes and screen waits for parser completion', () => {
  const callbacks = [], writes = [], replies = [];
  const terminal = { rows: 1, buffer: { active: { viewportY: 0, getLine: () => ({ translateToString: () => '한글' }) } },
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
  assert.deepEqual(replies.at(-1), { type: 'screen', request: 'read', sequence: 2, text: '한글' });
  assert.throws(() => output.receive({ type: 'output', sequence: 2, data: '' }), /Out-of-order/);
});
