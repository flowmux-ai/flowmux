// SPDX-License-Identifier: GPL-3.0-or-later
// The same controller serves the visible find bar and parser-barrier CLI requests.
export class SearchUi {
  constructor(terminal, createAddon, document, selection) {
    this.terminal = terminal;
    this.selection = selection;
    this.createAddon = createAddon;
    this.addon = createAddon();
    terminal.loadAddon(this.addon);
    this.dirty = false;
    terminal.onResize(() => this.invalidate());
    this.bar = document.getElementById('search');
    this.query = document.getElementById('query');
    this.matchCase = document.getElementById('match-case');
    this.regex = document.getElementById('regex');
    this.status = document.getElementById('search-status');
    this.composing = false;
    this.bar.addEventListener('submit', event => event.preventDefault());
    this.query.addEventListener('compositionstart', () => { this.composing = true; });
    this.query.addEventListener('compositionend', () => {
      this.composing = false;
      this.update(true);
    });
    this.query.addEventListener('input', event => {
      if (!this.composing && !event.isComposing) this.update(true);
    });
    this.query.addEventListener('keydown', event => {
      // Enter/Escape while composing belong to the IME, including legacy 229 events.
      if (this.composing || event.isComposing || event.keyCode === 229) return;
      if (event.key === 'Escape') { event.preventDefault(); this.close(true); }
      if (event.key === 'Enter') { event.preventDefault(); this.update(false, event.shiftKey); }
    });
    for (const toggle of [this.matchCase, this.regex]) toggle.addEventListener('change', () => this.update(true));
    document.getElementById('find-previous').addEventListener('click', () => this.update(false, true));
    document.getElementById('find-next').addEventListener('click', () => this.update());
    document.getElementById('find-close').addEventListener('click', () => this.close(true));
  }
  open(focus) {
    this.bar.hidden = false;
    if (focus) this.query.focus();
  }
  focus() {
    if (this.bar.hidden) this.terminal.focus();
    else this.query.focus();
  }
  clear() {
    this.selection?.forget();
    this.addon.clearDecorations();
    this.terminal.clearSelection();
    this.status.textContent = '';
    this.status.removeAttribute('data-error');
    this.query.removeAttribute('aria-invalid');
  }
  invalidate() { this.dirty = true; }
  close(focus) {
    this.clear();
    this.bar.hidden = true;
    if (focus) this.terminal.focus();
    return this.result(false);
  }
  update(incremental = false, previous = false) {
    if (this.composing) return this.result(false, 'Search query is being composed');
    const query = this.query.value;
    this.status.removeAttribute('data-error');
    this.query.removeAttribute('aria-invalid');
    if (!query) { this.clear(); return this.result(false); }
    try {
      if (query.length > 1024 || /[\r\n\0]/.test(query)) throw new Error('Use a single-line query of at most 1024 characters');
      if (this.regex.checked) {
        try { new RegExp(query); } catch { throw new Error('Invalid regular expression'); }
      }
      if (this.dirty) {
        // addon-search 0.16 caches lines on cursor/linefeed/resize events. A
        // rewrite ending at the same cursor position does not invalidate it.
        // Recreate through public APIs after output, without replacing xterm
        // or clearing its selection, history, focus or composition state.
        this.addon.dispose();
        this.addon = this.createAddon();
        this.terminal.loadAddon(this.addon);
        this.dirty = false;
      }
      this.selection?.forget();
      const found = this.addon[previous ? 'findPrevious' : 'findNext'](query, {
        incremental, caseSensitive: this.matchCase.checked, regex: this.regex.checked,
      });
      this.selection?.capture();
      this.status.textContent = found ? 'Match found' : 'No matches';
      return this.result(found);
    } catch (error) {
      this.clear();
      this.status.textContent = error.message;
      this.status.setAttribute('data-error', 'true');
      this.query.setAttribute('aria-invalid', 'true');
      return this.result(false, error.message);
    }
  }
  run(message) {
    if (this.composing) return this.result(false, 'Search query is being composed');
    if (message.close) return this.close(message.focus);
    this.query.value = message.query;
    this.matchCase.checked = message.match_case;
    this.regex.checked = message.regex;
    this.open(message.focus);
    return this.update(false, message.previous);
  }
  result(found, error = null) {
    const selection = this.terminal.getSelection();
    let excerpt = selection.slice(0, 16384);
    // JSON sent to Rust must never end with half of a surrogate pair.
    if (/[\uD800-\uDBFF]$/.test(excerpt)) excerpt = excerpt.slice(0, -1);
    return {
      found, error, visible: !this.bar.hidden, query: this.query.value,
      match_case: this.matchCase.checked, regex: this.regex.checked,
      selection: excerpt, selection_truncated: selection.length > excerpt.length,
      position: this.terminal.getSelectionPosition() ?? null,
      viewport: this.terminal.buffer.active.viewportY, buffer: this.terminal.buffer.active.type,
      status: this.status.textContent,
    };
  }
}
