// SPDX-License-Identifier: GPL-3.0-or-later
export const MAX_SELECTION_BYTES = 128 * 1024;

export class Selection {
  constructor(terminal) {
    this.terminal = terminal;
    this.saved = null;
    this.tracking = false;
    terminal.onSelectionChange(() => { if (this.tracking) this.capture(); });
    terminal.buffer.onBufferChange(() => this.forget());
    // RIS is an explicit terminal reset. Do not copy a previous session's text.
    terminal.parser.registerEscHandler({ final: 'c' }, () => { this.forget(); return false; });
  }
  forget() { this.saved = null; this.tracking = false; }
  capture() {
    const text = this.terminal.getSelection();
    if (!text) { if (this.tracking) this.saved = null; return; }
    const bytes = text.length <= MAX_SELECTION_BYTES ? new TextEncoder().encode(text).length : MAX_SELECTION_BYTES + 1;
    this.saved = {
      text: bytes <= MAX_SELECTION_BYTES ? text : '',
      error: bytes > MAX_SELECTION_BYTES ? 'Selection exceeds 128 KiB of UTF-8 text.' : null,
      position: this.terminal.getSelectionPosition() ?? null,
      buffer: this.terminal.buffer.active.type,
    };
  }
  primaryDown() {
    this.forget(); this.tracking = true;
    // Do not read the previous selection between pointerdown and xterm's
    // compatibility mousedown. Capture from selection changes/the release.
  }
  primaryUp(altClick = false) {
    if (!this.tracking) return;
    if (altClick && this.terminal.options?.altClickMovesCursor && this.terminal.getSelection().length <= 1) {
      this.forget(); return;
    }
    this.capture(); this.tracking = false;
  }
  replace(action) {
    this.forget();
    const result = action();
    this.capture();
    return result;
  }
  clear() { this.forget(); this.terminal.clearSelection(); }
  read() {
    const live = this.terminal.getSelection();
    // A first read also supports selection supplied by another xterm addon.
    if (!this.saved && live) this.capture();
    const saved = this.saved;
    const liveMatch = saved && live === saved.text;
    return {
      text: saved?.text ?? '', error: saved?.error ?? null,
      source: saved ? (liveMatch ? 'live' : 'retained') : 'none',
      position: liveMatch ? (this.terminal.getSelectionPosition() ?? null) : (saved?.position ?? null),
      buffer: saved?.buffer ?? this.terminal.buffer.active.type,
    };
  }
  run(action) {
    try {
      if (action.kind === 'clear') this.clear();
      else if (action.kind === 'all') this.replace(() => this.terminal.selectAll());
      else if (action.kind === 'range') {
        const { column, row, length } = action;
        const capacity = (this.terminal.buffer.active.length - row) * this.terminal.cols - column;
        if (![column, row, length].every(Number.isSafeInteger) || column < 0 || column >= this.terminal.cols ||
            row < 0 || row >= this.terminal.buffer.active.length || length < 1 || length > capacity)
          throw new Error('Selection range is outside the retained terminal buffer.');
        this.replace(() => this.terminal.select(column, row, length));
      } else if (action.kind !== 'read') throw new Error('Unknown selection operation.');
      return this.read();
    } catch (error) { return { text: '', source: 'none', error: error.message, position: null, buffer: this.terminal.buffer.active.type }; }
  }
  copyEvent(event, report, allowed = true) {
    if (event.defaultPrevented) return;
    event.preventDefault(); event.stopImmediatePropagation();
    if (!allowed) { report('Clipboard access is disabled in background tests.'); return; }
    const result = this.read();
    if (result.error) { report(result.error); return; }
    // An empty copy must never replace another application's clipboard text.
    if (!result.text) { report('No text selected.'); return; }
    if (!event.clipboardData) { report('Clipboard is unavailable.'); return; }
    try { event.clipboardData.setData('text/plain', result.text); report(''); }
    catch { report('Could not copy the selection.'); }
  }
}
