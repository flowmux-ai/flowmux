// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Input } from './input.mjs';

test('Shift+Enter preserves xterm commit order and never invents an Enter for an IME confirmation', async () => {
  const sent = [], input = new Input(data => sent.push(data));
  input.keyEvent({ type: 'keydown', key: 'Enter', shiftKey: true, keyCode: 13 });
  input.data('한'); // xterm finalizes the existing composition synchronously.
  input.data('\r');
  assert.deepEqual(sent, ['한', '\x1b\r']);
  await Promise.resolve();
  input.data('\r'); // A subsequent paste/data callback must not retain the modifier.
  input.keyEvent({ type: 'keydown', key: 'Enter', shiftKey: true, keyCode: 229 });
  input.data('글'); // IME consumes the key; xterm emits no CR, so neither do we.
  assert.deepEqual(sent, ['한', '\x1b\r', '\r', '글']);
});

test('rapid repeated Enter preserves the exact event count', () => {
  const sent = [], input = new Input(data => sent.push(data));
  for (let i = 0; i < 3; i++) {
    input.keyEvent({ type: 'keydown', key: 'Enter', shiftKey: false, keyCode: 13 });
    input.data('\r');
  }
  assert.equal(sent.join(''), '\r\r\r');
});
