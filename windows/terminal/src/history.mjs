// SPDX-License-Identifier: GPL-3.0-or-later
export const MAX_SCREEN_BYTES = 128 * 1024;
const encoder = new TextEncoder();

export function snapshot(terminal, serialize) {
  const buffer = terminal.buffer.normal;
  const end = buffer.length - 1;
  let count = Math.min(buffer.length, 10000 + terminal.rows);
  for (;;) {
    // Trim complete rows, never split an ANSI sequence, UTF-8 character or SGR.
    const start = end + 1 - count;
    const data = serialize.serialize({ range: { start, end }, excludeModes: true, excludeAltBuffer: true });
    if (encoder.encode(data).length <= MAX_SCREEN_BYTES) {
      return { format: 'xterm-ansi-v1', cols: terminal.cols, rows: terminal.rows, data, truncated: start > 0 };
    }
    if (count === 1) throw new Error('One terminal row exceeds the history limit');
    count = Math.max(1, Math.floor(count / 2));
  }
}

// The caller suppresses terminal onData/onBinary/title during this entire parser
// transaction. Historical VT replies must never become input to the fresh PTY.
export function restore(terminal, screen, done) {
  if (screen.format !== 'xterm-ansi-v1' || encoder.encode(screen.data).length > MAX_SCREEN_BYTES) {
    throw new Error('Unsupported terminal history');
  }
  terminal.resize(screen.cols, screen.rows);
  terminal.write(screen.data, () => {
    // Put the restored display into scrollback before ConPTY's initial clear.
    // A new process cannot resume the old process's alternate screen or modes.
    terminal.write(`\x1b[0m\x1b[r\x1b[${screen.rows};1H\r\n[Restored history — new PowerShell session]\r\n${'\n'.repeat(screen.rows - 1)}`, done);
  });
}
