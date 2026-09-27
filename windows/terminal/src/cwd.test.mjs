// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { osc7Cwd, osc9Cwd, observeCwd } from './cwd.mjs';

test('local Windows OSC paths preserve Korean, combining characters and delimiters', () => {
  assert.equal(osc9Cwd('9;"C:\\한글 e\u0301\\space %;#\'folder"'), 'C:\\한글 e\u0301\\space %;#\'folder');
  assert.equal(osc7Cwd('file:///C:/%ED%95%9C%EA%B8%80/space%20%25%23%3B'), 'C:/한글/space %#;');
  for (const url of ['https://example.test/C:/x', 'file://remote-host/C:/x', 'file:///tmp/x', 'file:///C:/x?query', 'file:///C:/x#fragment', 'file:///C:/bad%00name']) assert.equal(osc7Cwd(url), null);
  for (const data of ['hello', '9;C:relative', '9;\\\\remote\\share', '9;C:\\bad\nname']) assert.equal(osc9Cwd(data), null);
});

test('history replay cannot change cwd; unrelated OSC 9 remains available to other handlers', () => {
  const handlers = new Map(), messages = [];
  let restoring = true;
  observeCwd({ parser: { registerOscHandler: (code, callback) => handlers.set(code, callback) } }, m => messages.push(m), () => restoring);
  assert.equal(handlers.get(9)('notification'), false);
  handlers.get(9)('9;C:\\history');
  handlers.get(7)('file:///C:/history');
  assert.deepEqual(messages, []);
  restoring = false;
  handlers.get(9)('9;C:\\live');
  assert.deepEqual(messages, [{ type: 'cwd', path: 'C:\\live' }]);
});
