// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { osc7Cwd, osc7RemoteCwd, osc9Cwd, observeCwd } from './cwd.mjs';

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
  handlers.get(7)('file://remote-host/srv/history');
  assert.deepEqual(messages, []);
  restoring = false;
  handlers.get(9)('9;C:\\live');
  assert.deepEqual(messages, [{ type: 'cwd', path: 'C:\\live' }]);
});

test('remote OSC 7 preserves decoded POSIX Unicode and punctuation without treating its host as local', () => {
  const path = "/srv/한글 한 e\u0301 😀/'quoted' & %?#;\\folder";
  const uri = `file://remote-host${path.split('/').map(encodeURIComponent).join('/')}`;
  assert.equal(osc7RemoteCwd(uri), path);
  assert.equal(osc7RemoteCwd('file://localhost/home/user'), '/home/user');
  assert.equal(osc7RemoteCwd('file://[::1]/srv/project'), '/srv/project');
  assert.equal(osc7Cwd(uri), null);
  for (const url of ['https://host/srv', 'file:/srv', 'file://[bad]/srv',
    'file://host:22/srv', 'file://user@host/srv', 'file://host/srv?query',
    'file://host/srv?', 'file://host/srv#', 'file://host/srv%00x',
    'file://host/srv%0Ax', 'file://host/srv%C2%85x', 'file://host/srv%FF',
    'file://host/srv%ZZ', 'file://host/srv\nx', 'file://host/srv\\x',
    `file://host/${'x'.repeat(32767)}`]) {
    assert.equal(osc7RemoteCwd(url), null, url.slice(0, 100));
  }
});

test('OSC 7 observation prefers Windows paths and otherwise forwards remote metadata for native validation', () => {
  const handlers = new Map(), messages = [];
  observeCwd({ parser: { registerOscHandler: (code, callback) => handlers.set(code, callback) } },
    message => messages.push(message), () => false);
  handlers.get(7)('file:///C:/Windows%20path');
  handlers.get(7)('file://remote/srv/%ED%95%9C%EA%B8%80');
  handlers.get(7)('file://remote/srv%00bad');
  assert.deepEqual(messages, [
    { type: 'cwd', path: 'C:/Windows path' },
    { type: 'cwd', path: '/srv/한글' },
  ]);
});
