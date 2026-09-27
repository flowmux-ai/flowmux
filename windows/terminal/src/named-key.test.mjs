// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { keyMode } from './named-key.mjs';
import { keyboardReference } from './named-key-reference.mjs';
import { Output } from './output.mjs';

test('frozen native key vectors match pinned xterm and flowmux Shift+Enter encoding', async () => {
  const { cases } = JSON.parse(await readFile(new URL('./named-key.cases.json', import.meta.url), 'utf8'));
  const reference = await keyboardReference();
  for (const value of cases) {
    assert.equal(reference(value.event, false), value.normal, `${value.name} normal`);
    assert.equal(reference(value.event, true), value.application, `${value.name} application`);
  }
});

test('mode snapshots wait for parser completion, never produce onData and reply once', () => {
  const replies = [], callbacks = [];
  let forget = 0, unavailable = null;
  const terminal = { options: {}, modes: { applicationCursorKeysMode: false },
    write: (_, callback) => callbacks.push(callback) };
  const output = new Output(terminal, value => replies.push(value), null, null, null, null, null,
    { forget: () => forget++ }, null, () => keyMode(terminal, () => unavailable));
  output.receive({ type: 'key_mode', request: 'key', after: 2 });
  output.receive({ type: 'output', sequence: 1, data: 'QQ==' });
  output.receive({ type: 'output', sequence: 2, data: 'Qg==' });
  callbacks[0]();
  assert.equal(forget, 0);
  terminal.modes.applicationCursorKeysMode = true; callbacks[1]();
  output.flush();
  assert.equal(forget, 1);
  assert.deepEqual(replies.filter(r => r.type === 'key_mode'), [
    { type: 'key_mode', request: 'key', sequence: 2, outcome: { status: 'ok', application_cursor: true } },
  ]);
  unavailable = 'Finish composing text.';
  output.receive({ type: 'key_mode', request: 'composing', after: 2 });
  assert.equal(replies.at(-1).outcome.status, 'error');
  assert.equal(forget, 1);
});

test('composition settling, restore and disabled input reject without querying mode', () => {
  const terminal = { options: {}, get modes() { throw new Error('Mode queried while unavailable'); } };
  for (const reason of ['composing', 'composition settling', 'restoring']) {
    assert.deepEqual(keyMode(terminal, () => reason), { status: 'error', message: reason });
  }
  terminal.options.disableStdin = true;
  assert.deepEqual(keyMode(terminal, () => null), { status: 'error', message: 'Terminal input is unavailable.' });
});
