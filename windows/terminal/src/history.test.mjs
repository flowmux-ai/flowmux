// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { snapshot, restore, MAX_SCREEN_BYTES } from './history.mjs';
import { Output } from './output.mjs';

test('history truncates complete styled rows by UTF-8 size and excludes process modes', () => {
  const terminal = { cols: 80, rows: 24, buffer: { normal: { length: 1000 } } };
  const screen = snapshot(terminal, { serialize: options => {
    assert.equal(options.excludeModes, true);
    assert.equal(options.excludeAltBuffer, true);
    return '\x1b[32m한글\x1b[0m\r\n'.repeat((options.range.end - options.range.start + 1) * 100);
  } });
  assert.equal(screen.truncated, true);
  assert.ok(Buffer.byteLength(screen.data) <= MAX_SCREEN_BYTES);
  assert.ok(screen.data.endsWith('\x1b[0m\r\n'));
});

test('restore finishes only after both parser callbacks and moves history out of the viewport', () => {
  const writes = [], callbacks = [], sizes = [];
  let done = false;
  restore({ resize: (...args) => sizes.push(args), write: (data, callback) => { writes.push(data); callbacks.push(callback); } },
    { format: 'xterm-ansi-v1', cols: 80, rows: 24, data: '한글' }, () => { done = true; });
  assert.deepEqual(sizes, [[80, 24]]);
  assert.deepEqual(writes, ['한글']);
  assert.equal(done, false);
  callbacks.shift()();
  assert.equal(writes[1], `\x1b[0m\x1b[r\x1b[24;1H\r\n[Restored history]\r\n${'\n'.repeat(23)}`);
  assert.equal(done, false);
  callbacks.shift()();
  assert.equal(done, true);
});

test('snapshots wait for parsed output and errors are attached to the request', () => {
  let parsed;
  const replies = [];
  const output = new Output({ write: (_, callback) => { parsed = callback; } }, m => replies.push(m), () => { throw new Error('too large'); });
  output.receive({ type: 'snapshot', request: 'test', after: 1 });
  output.receive({ type: 'output', sequence: 1, data: '' });
  assert.equal(replies.length, 0);
  parsed();
  assert.deepEqual(replies.at(-1), { type: 'snapshot_error', request: 'test', message: 'Error: too large' });
});
