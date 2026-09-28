// SPDX-License-Identifier: GPL-3.0-or-later
// Read barriers resolve only after xterm's parser callback, including hidden tabs.
import { readScreen } from './screen.mjs';
export class Output {
  constructor(terminal, send, serialize, find, changed, outputSearch, paste, selection, minimap, keyMode) {
    this.terminal = terminal;
    this.send = send;
    this.serialize = serialize;
    this.find = find;
    this.changed = changed;
    this.outputSearch = outputSearch;
    this.paste = paste;
    this.selection = selection;
    this.minimap = minimap;
    this.keyMode = keyMode;
    this.received = 0;
    this.parsed = 0;
    this.pending = [];
  }
  cancelPending() {
    // A disconnected session may never deliver its outstanding barriers. Drop
    // those requests while retaining the grid and already submitted writes.
    this.pending = [];
  }
  receive(message) {
    if (message.type === 'output') {
      if (message.sequence !== this.received + 1) throw new Error('Out-of-order terminal output');
      this.received = message.sequence;
      // Never decode each transport chunk as UTF-8: a Korean syllable can span chunks.
      const bytes = Uint8Array.from(atob(message.data), c => c.charCodeAt(0));
      this.terminal.write(bytes, () => {
        this.parsed = message.sequence;
        this.changed?.();
        this.send({ type: 'ack', sequence: this.parsed });
        this.flush();
      });
    } else if (message.type === 'cancel_search') {
      this.outputSearch.cancel(message.search);
      this.pending = this.pending.filter(p => p.search !== message.search);
    } else if (['read_screen', 'snapshot', 'find', 'search_buffer', 'open_search_hit', 'paste', 'selection', 'minimap', 'key_mode'].includes(message.type)) {
      this.pending.push(message);
      this.flush();
    }
  }
  flush() {
    const waiting = [];
    for (const message of this.pending) {
      if (message.after > this.parsed) { waiting.push(message); continue; }
      if (message.type === 'read_screen') {
        let outcome;
        try { outcome = { status: 'ok', snapshot: readScreen(this.terminal, message.recent) }; }
        catch (error) { outcome = { status: 'error', message: String(error) }; }
        this.send({ type: 'screen', request: message.request, sequence: this.parsed, outcome });
      } else if (message.type === 'minimap') {
        this.send({ type: 'minimap', request: message.request, sequence: this.parsed, outcome: this.minimap.run(message.action) });
      } else if (message.type === 'key_mode') {
        const outcome = this.keyMode();
        if (outcome.status === 'ok') this.selection?.forget();
        this.send({ type: 'key_mode', request: message.request, sequence: this.parsed, outcome });
      } else if (message.type === 'paste') {
        const outcome = this.paste.run(message.text);
        if (outcome.status === 'ok' && message.text) this.selection?.forget();
        this.send({ type: 'pasted', request: message.request, sequence: this.parsed, outcome });
      } else if (message.type === 'selection') {
        this.send({ type: 'selected', request: message.request, sequence: this.parsed, result: this.selection.run(message.action) });
      } else if (message.type === 'find') {
        this.send({ type: 'found', request: message.request, sequence: this.parsed, result: this.find(message) });
      } else if (message.type === 'search_buffer') {
        void this.outputSearch.scan(message, this.parsed);
      } else if (message.type === 'open_search_hit') {
        void this.outputSearch.open(message, this.parsed);
      } else {
        try {
          this.send({ type: 'snapshot', request: message.request, sequence: this.parsed, screen: this.serialize() });
        } catch (error) {
          this.send({ type: 'snapshot_error', request: message.request, message: String(error) });
        }
      }
    }
    this.pending = waiting;
  }
}
