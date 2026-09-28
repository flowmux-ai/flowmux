// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Output } from './output.mjs';
import { Paste } from './paste.mjs';
import xterm from '@xterm/xterm';

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

test('minimap reads wait for preceding output before sampling cells', () => {
  const replies = [], callbacks = [], actions = [];
  const terminal = { write: (_, done) => callbacks.push(done) };
  const minimap = { run: action => { actions.push(action); return { status: 'ok' }; } };
  const output = new Output(terminal, reply => replies.push(reply), null, null, null, null, null, null, minimap);
  output.receive({ type: 'minimap', request: 'map', after: 1, action: { kind: 'read' } });
  output.receive({ type: 'output', sequence: 1, data: 'QQ==' });
  assert.equal(actions.length, 0); callbacks[0]();
  assert.deepEqual(replies.at(-1), { type: 'minimap', request: 'map', sequence: 1, outcome: { status: 'ok' } });
});

test('disconnect cancels old actions without losing in-flight output, history or reconnect sequence', () => {
  const callbacks = [], replies = [], actions = [];
  let grid = '기존 한 history ';
  const terminal = { write: (bytes, done) => callbacks.push(() => {
    grid += Buffer.from(bytes).toString('utf8'); done();
  }) };
  const output = new Output(terminal, reply => replies.push(reply), null,
    message => { actions.push(`find:${message.request}`); return {}; }, null, null,
    { run: text => { actions.push(`paste:${text}`); return {}; } },
    { run: action => { actions.push(`selection:${action.kind}`); return {}; } });
  output.receive({ type: 'output', sequence: 1, data: 'QQ==' });
  output.receive({ type: 'find', request: 'old-find', after: 3 });
  output.receive({ type: 'selection', request: 'old-select', after: 3, action: { kind: 'all' } });
  output.receive({ type: 'paste', request: 'old-paste', after: 3, text: '실행하면 안 됨' });
  output.cancelPending();
  assert.equal(output.received, 1); assert.equal(output.parsed, 0);
  callbacks[0]();
  assert.equal(grid, '기존 한 history A');
  output.receive({ type: 'output', sequence: 2, data: 'Qg==' });
  output.receive({ type: 'find', request: 'new-find', after: 3 });
  output.receive({ type: 'output', sequence: 3, data: 'Qw==' });
  callbacks[1](); callbacks[2](); output.flush();
  assert.equal(grid, '기존 한 history ABC');
  assert.equal(output.received, 3); assert.equal(output.parsed, 3);
  assert.deepEqual(actions, ['find:new-find']);
  assert.deepEqual(replies.filter(reply => reply.type === 'ack').map(reply => reply.sequence), [1, 2, 3]);
  assert.deepEqual(replies.filter(reply => reply.request).map(reply => reply.request), ['new-find']);
});

test('real xterm session reset preserves Korean history and sequence while abandoning old modes and parser state', async () => {
  const { Terminal } = xterm;
  const encoded = text => new TextEncoder().encode(text);
  const history = '이전 한글 한 😀';
  for (const unfinished of [encoded('\x1b[31;'), encoded('\x1b]2;unfinished'),
    encoded('\x1bP$qunfinished'), encoded('한').slice(0, 2)]) {
    const terminal = new Terminal({ cols: 60, rows: 4, scrollback: 20 });
    try {
      const replies = [];
      let tag = 'old';
      const output = new Output(terminal, message => replies.push({ tag, ...message }));
      const receive = (sequence, data) => output.receive({ type: 'output', sequence,
        data: Buffer.from(data).toString('base64') });
      const fence = () => new Promise(resolve => terminal.write(new Uint8Array(), resolve));
      receive(1, encoded(`${history}\r\n1\r\n2\r\n3\r\n4\r\n`));
      receive(2, encoded('\x1b[?1049h\x1b[?1;66;1003;1006;2004;1004hALT'));
      await fence();
      assert.equal(terminal.buffer.active.type, 'alternate');
      assert.equal(terminal.modes.applicationCursorKeysMode, true);
      assert.equal(terminal.modes.applicationKeypadMode, true);
      assert.equal(terminal.modes.bracketedPasteMode, true);
      assert.equal(terminal.modes.mouseTrackingMode, 'any');
      receive(3, unfinished);
      const reset = new Promise(resolve => output.startSession(() => { tag = 'new'; resolve(); }));
      const next = encoded('새 한글 가 😀');
      receive(4, next.slice(0, 2));
      receive(5, next.slice(2));
      await reset; await fence();
      assert.equal(terminal.buffer.active.type, 'normal');
      assert.deepEqual(terminal.modes, {
        applicationCursorKeysMode: false, applicationKeypadMode: false,
        bracketedPasteMode: false, insertMode: false, mouseTrackingMode: 'none',
        originMode: false, reverseWraparoundMode: false, sendFocusMode: false,
        synchronizedOutputMode: false, wraparoundMode: true,
      });
      const buffer = terminal.buffer.normal;
      const text = Array.from({ length: buffer.length }, (_, row) =>
        buffer.getLine(row).translateToString(true)).join('\n');
      assert.ok(text.includes(history), JSON.stringify(text));
      assert.ok(text.includes('새 한글 가 😀'), JSON.stringify(text));
      assert.equal(output.received, 5); assert.equal(output.parsed, 5);
      assert.deepEqual(replies.map(reply => [reply.type, reply.sequence, reply.tag]),
        [['ack', 1, 'old'], ['ack', 2, 'old'], ['ack', 3, 'old'], ['ack', 4, 'new'], ['ack', 5, 'new']]);
    } finally { terminal.dispose(); }
  }
});

test('real xterm reconnect in normal buffer preserves the current cursor and existing Korean cells', async () => {
  const terminal = new xterm.Terminal({ cols: 60, rows: 4 });
  try {
    const output = new Output(terminal, () => {});
    output.receive({ type: 'output', sequence: 1,
      data: Buffer.from('보존 한 😀\r\n현재 프롬프트').toString('base64') });
    await new Promise(resolve => terminal.write(new Uint8Array(), resolve));
    const before = [terminal.buffer.active.cursorX, terminal.buffer.active.cursorY];
    await new Promise(resolve => output.startSession(resolve));
    assert.deepEqual([terminal.buffer.active.cursorX, terminal.buffer.active.cursorY], before);
    output.receive({ type: 'output', sequence: 2, data: Buffer.from(' 새 연결').toString('base64') });
    await new Promise(resolve => terminal.write(new Uint8Array(), resolve));
    assert.equal(terminal.buffer.normal.getLine(0).translateToString(true), '보존 한 😀');
    assert.equal(terminal.buffer.normal.getLine(1).translateToString(true), '현재 프롬프트 새 연결');
    assert.equal(output.received, 2); assert.equal(output.parsed, 2);
  } finally { terminal.dispose(); }
});
