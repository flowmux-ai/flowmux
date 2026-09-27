// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Clipboard } from './clipboard.mjs';

function fixture() {
  const writes = [], inputs = [], messages = [];
  let resolve, reads = 0, available = true, canPaste = true;
  const selection = { read: () => ({ text: '한글 한 😀', error: null }), forget() {} };
  const clipboard = { writeText: async text => { writes.push(text); }, readText: () => { reads++; return new Promise(r => { resolve = r; }); } };
  const controller = new Clipboard(selection, { run: text => ({ status: 'ok', data: text, bracketed: false }) }, clipboard,
    outcome => inputs.push(outcome.data), text => messages.push(text), () => available, () => canPaste);
  return { controller, selection, clipboard, writes, inputs, messages, reads: () => reads, resolve: text => resolve(text),
    unavailable: () => { available = false; }, compose: () => { canPaste = false; } };
}

test('async clipboard read is discarded across tab/focus changes, composition, or intervening input', async () => {
  for (const invalidate of [f => f.controller.cancel(), f => f.unavailable(), f => f.compose()]) {
    const f = fixture(), result = f.controller.run('paste');
    invalidate(f); f.resolve('한글'); await result;
    assert.deepEqual(f.inputs, []);
  }
  const f = fixture(), result = f.controller.run('paste');
  f.resolve('한글'); await result;
  assert.deepEqual(f.inputs, ['한글']);
});

test('empty or oversized selections never overwrite clipboard; permission failure never retries', async () => {
  const f = fixture();
  f.selection.read = () => ({ text: '', error: null }); await f.controller.run('copy');
  f.selection.read = () => ({ text: '', error: 'Selection exceeds limit' }); await f.controller.run('copy');
  assert.deepEqual(f.writes, []);
  f.selection.read = () => ({ text: '한글', error: null });
  let attempts = 0;
  f.clipboard.writeText = async () => { attempts++; throw new Error('Permission denied'); };
  await f.controller.run('copy');
  assert.equal(attempts, 1); assert.equal(f.messages.at(-1), 'Permission denied');
});

test('only copy/paste shortcuts are consumed; composition, AltGr, Ctrl+C and repeats preserve ownership', async () => {
  const f = fixture(); let prevented = 0;
  const event = { type: 'keydown', ctrlKey: true, shiftKey: true, code: 'KeyC', key: 'C', preventDefault: () => prevented++ };
  for (const extra of [{ ctrlKey: true, shiftKey: false, key: 'c' }, { altKey: true }, { isComposing: true }, { keyCode: 229 },
    { getModifierState: () => true }, { type: 'keyup' }])
    assert.equal(f.controller.key({ ...event, ...extra }, false), false);
  assert.equal(f.controller.key(event, true), false);
  assert.equal(f.controller.key(event, false), true);
  await Promise.resolve();
  assert.deepEqual(f.writes, ['한글 한 😀']);
  assert.equal(f.controller.key({ ...event, repeat: true }, false), true);
  assert.equal(f.writes.length, 1); assert.equal(prevented, 2);
});

test('background/hidden document and active composition refuse clipboard APIs before access', async () => {
  const f = fixture(); f.unavailable();
  await f.controller.run('copy'); await f.controller.run('paste');
  assert.deepEqual(f.writes, []); assert.equal(f.reads(), 0);
  const g = fixture(); g.compose(); await g.controller.run('paste');
  assert.equal(g.reads(), 0);
});

test('a pending clipboard operation is bounded and cancellation permits a fresh request after completion', async () => {
  const f = fixture(), first = f.controller.run('paste');
  await f.controller.run('paste'); assert.equal(f.reads(), 1);
  f.controller.cancel(); f.resolve('stale'); await first;
  const second = f.controller.run('paste'); f.resolve('current'); await second;
  assert.deepEqual(f.inputs, ['current']);
});
