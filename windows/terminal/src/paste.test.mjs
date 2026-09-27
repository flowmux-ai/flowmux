// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Paste, MAX_PASTE_BYTES } from './paste.mjs';
import { Input } from './input.mjs';

function fixture() {
  const normal = [], calls = [];
  const input = new Input(data => normal.push(data));
  const terminal = { options: {}, modes: { bracketedPasteMode: false }, paste(text) {
    calls.push(text);
    // Stub transport only; real xterm transformation is checked in WebView2.
    if (!paste.data(text)) input.data(text);
  } };
  let unavailable = null;
  const paste = new Paste(terminal, () => unavailable);
  return { terminal, paste, input, normal, calls, block: reason => { unavailable = reason; } };
}

test('paste bypasses Shift+Enter and is captured once without ordinary input duplication', () => {
  const f = fixture();
  f.input.keyEvent({ type: 'keydown', key: 'Enter', shiftKey: true, keyCode: 13 });
  assert.deepEqual(f.paste.run('\r'), { status: 'ok', data: '\r', bracketed: false });
  assert.deepEqual(f.normal, []);
  assert.deepEqual(f.calls, ['\r']);
  assert.equal(f.paste.data('한'), false); // Composition/keyboard still belongs to xterm.
});

test('composition, restoration and disabled input reject without invoking xterm or replaying later', () => {
  const f = fixture();
  for (const reason of ['Finish composing text before pasting.', 'Terminal history is being restored.']) {
    f.block(reason);
    assert.deepEqual(f.paste.run('한글'), { status: 'error', message: reason });
  }
  f.block(null); f.terminal.options.disableStdin = true;
  assert.match(f.paste.run('한글').message, /unavailable/);
  f.terminal.options.disableStdin = false;
  assert.deepEqual(f.calls, []);
  assert.equal(f.paste.run('다음').data, '다음');
  assert.deepEqual(f.calls, ['다음']);
});

test('UTF-8 bounds, invalid surrogates and NUL fail atomically; decomposition is preserved', () => {
  const f = fixture();
  for (const text of ['a'.repeat(MAX_PASTE_BYTES + 1), '한'.repeat(MAX_PASTE_BYTES / 3 + 1), '\ud800', '\udc00', '한\0글'])
    assert.equal(f.paste.run(text).status, 'error');
  assert.deepEqual(f.calls, []);
  for (const text of ['a'.repeat(MAX_PASTE_BYTES), '한글 한 😀\t\r\n'])
    assert.equal(f.paste.run(text).data, text);
  f.terminal.modes.bracketedPasteMode = true;
  const before = f.calls.length;
  assert.deepEqual(f.paste.run(''), { status: 'ok', data: '', bracketed: false });
  assert.equal(f.calls.length, before);
});

test('DOM handler consumes the default paste once; missing or cancelled clipboard events never paste', () => {
  const f = fixture(), outcomes = [], events = [];
  const event = { defaultPrevented: false, clipboardData: { getData: type => {
    assert.equal(type, 'text/plain'); return '한글';
  } }, preventDefault: () => events.push('prevent'), stopImmediatePropagation: () => events.push('stop') };
  f.paste.event(event, value => outcomes.push(value));
  assert.deepEqual(events, ['prevent', 'stop']);
  assert.deepEqual(f.calls, ['한글']);
  f.paste.event({ ...event, defaultPrevented: true }, value => outcomes.push(value));
  assert.equal(outcomes.length, 1);
  f.paste.event({ ...event, clipboardData: null }, value => outcomes.push(value));
  assert.equal(outcomes[1].status, 'error');
  assert.deepEqual(f.calls, ['한글']);
});

test('failed or multiple xterm emissions do not leak partial input and next paste can recover', () => {
  const f = fixture();
  f.terminal.paste = () => { f.paste.data('partial'); throw new Error('parser failure'); };
  assert.equal(f.paste.run('한글').status, 'error');
  f.terminal.paste = () => { f.paste.data('one'); f.paste.data('two'); };
  assert.equal(f.paste.run('한글').status, 'error');
  assert.deepEqual(f.normal, []);
  assert.equal(f.paste.data('ordinary'), false);
});

test('paste stays blocked through xterm deferred composition finalization without replaying text', async () => {
  const f = fixture(), commits = [];
  // The custom key hook runs before xterm schedules its keyCode 229 finalizer.
  f.paste.settleComposition();
  setTimeout(() => { commits.push('한'); assert.equal(f.paste.run('글').status, 'error'); }, 0);
  assert.equal(f.paste.run('글').status, 'error');
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.deepEqual(f.calls, []);
  assert.deepEqual(commits, ['한']);
  assert.equal(f.paste.run('글').data, '글');
  // A new composition ending while the first guard settles must keep its guard.
  f.paste.settleComposition();
  setTimeout(() => f.paste.settleComposition(), 0);
  await new Promise(resolve => setTimeout(resolve, 0));
  assert.equal(f.paste.run('다음').status, 'error');
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.equal(f.paste.run('다음').data, '다음');
});
