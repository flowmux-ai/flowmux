// SPDX-License-Identifier: GPL-3.0-or-later
export const MAX_TEXT_BYTES = 128 * 1024;
export const RECENT_ROWS = 80;
const encoder = new TextEncoder();

// Read parsed cells synchronously: do not scroll, select, normalize Unicode or
// confuse a PTY transcript with the current grid. Recent normal rows ignore the
// user's scroll position; alternate-screen footers can be below the cursor.
export function readScreen(terminal, recent = false) {
  const buffer = terminal.buffer.active;
  const rowCount = recent && buffer.type === 'normal'
    ? Math.min(RECENT_ROWS, buffer.length) : terminal.rows;
  const first = recent ? buffer.length - rowCount : buffer.viewportY;
  const lines = [];
  let bytes = 0;
  for (let row = first; row < first + rowCount; row++) {
    const line = buffer.getLine(row);
    if (!line) throw new Error('Terminal screen range is unavailable');
    const text = line.translateToString(true);
    bytes += encoder.encode(text).length + (lines.length ? 1 : 0);
    if (bytes > MAX_TEXT_BYTES) throw new Error('Terminal screen exceeds the 128 KiB UTF-8 limit');
    lines.push(text);
  }
  return { text: lines.join('\n'), metadata: {
    mode: recent ? 'recent' : 'viewport', buffer: buffer.type,
    cols: terminal.cols, rows: terminal.rows, buffer_rows: buffer.length,
    first_row: first, row_count: rowCount, viewport_row: buffer.viewportY,
    base_row: buffer.baseY, cursor: { row: buffer.baseY + buffer.cursorY, column: buffer.cursorX },
  } };
}
