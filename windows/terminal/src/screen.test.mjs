// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { readScreen, MAX_TEXT_BYTES } from './screen.mjs';

function fixture(lines, rows, viewport, type = 'normal') {
  const reads = [];
  const buffer = { type, length: lines.length, baseY: lines.length - rows,
    viewportY: viewport, cursorX: 0, cursorY: 0,
    getLine: row => { reads.push(row); return lines[row] === undefined ? undefined : {
      translateToString: trim => { assert.equal(trim, true); return lines[row]; },
    }; },
  };
  return { terminal: { cols: 80, rows, buffer: { active: buffer } }, reads };
}
test('viewport preserves physical row breaks, blank rows and exact Unicode', () => {
  const f = fixture(['old', '한글 한 é 😀', '', 'wrapped', 'latest'], 3, 1);
  const result = readScreen(f.terminal);
  assert.equal(result.text, '한글 한 é 😀\n\nwrapped');
  assert.deepEqual(f.reads, [1, 2, 3]);
  assert.equal(result.metadata.first_row, 1);
  assert.equal(result.metadata.mode, 'viewport');
  assert.equal(f.terminal.buffer.active.viewportY, 1);
});
test('recent reads only the newest 80 normal rows independent of viewport and cursor', () => {
  const f = fixture(Array.from({ length: 10000 }, (_, n) => `ROW_${n}`), 24, 17);
  const result = readScreen(f.terminal, true);
  assert.equal(result.text, Array.from({ length: 80 }, (_, n) => `ROW_${9920 + n}`).join('\n'));
  assert.deepEqual(f.reads, Array.from({ length: 80 }, (_, n) => 9920 + n));
  assert.equal(result.metadata.row_count, 80);
  assert.equal(result.metadata.cursor.row, 9976);
  assert.equal(result.metadata.viewport_row, 17);
});
test('recent includes blank bottom rows and all alternate rows below the cursor', () => {
  const normal = fixture(['한글', '', ''], 3, 0);
  assert.equal(readScreen(normal.terminal, true).text, '한글\n\n');
  const lines = Array.from({ length: 100 }, (_, n) => n === 99 ? 'FOOTER_한글' : '');
  const alternate = fixture(lines, 100, 0, 'alternate');
  const result = readScreen(alternate.terminal, true);
  assert.equal(result.metadata.row_count, 100);
  assert.equal(result.metadata.cursor.row, 0);
  assert.equal(result.text, lines.join('\n'));
});
test('screen byte limit counts Unicode and newlines and never returns truncated text', () => {
  const exact = '한'.repeat(Math.floor((MAX_TEXT_BYTES - 1) / 3)) + 'x';
  assert.equal(new TextEncoder().encode(exact + '\n').length, MAX_TEXT_BYTES);
  const f = fixture([exact, ''], 2, 0);
  assert.equal(readScreen(f.terminal).text, exact + '\n');
  const tooLarge = fixture([exact, 'x', 'not_read'], 3, 0);
  assert.throws(() => readScreen(tooLarge.terminal), /128 KiB/);
  assert.deepEqual(tooLarge.reads, [0, 1]);
  assert.throws(() => readScreen(fixture(['ok', undefined], 2, 0).terminal), /unavailable/);
});
