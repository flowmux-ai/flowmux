// SPDX-License-Identifier: GPL-3.0-or-later
// Snapshot public xterm mode state only after the output parser barrier. No DOM
// keyboard event, composition commit, terminal focus or onData replay is involved.
export function keyMode(terminal, unavailable) {
  const reason = unavailable();
  if (reason || terminal.options.disableStdin)
    return { status: 'error', message: reason || 'Terminal input is unavailable.' };
  return { status: 'ok', application_cursor: terminal.modes.applicationCursorKeysMode };
}
