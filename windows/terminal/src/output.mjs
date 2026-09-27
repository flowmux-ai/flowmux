// SPDX-License-Identifier: GPL-3.0-or-later
// Read barriers resolve only after xterm's parser callback, including hidden tabs.
export class Output {
  constructor(terminal, send, serialize, find, changed) {
    this.terminal = terminal;
    this.send = send;
    this.serialize = serialize;
    this.find = find;
    this.changed = changed;
    this.received = 0;
    this.parsed = 0;
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
    } else if (['read_screen', 'snapshot', 'find'].includes(message.type)) {
      this.pending.push(message);
      this.flush();
    }
  }
  flush() {
    const waiting = [];
    for (const message of this.pending) {
      if (message.after > this.parsed) { waiting.push(message); continue; }
      if (message.type === 'read_screen') {
        const buffer = this.terminal.buffer.active;
        const lines = [];
        for (let y = buffer.viewportY; y < buffer.viewportY + this.terminal.rows; y++) {
          lines.push(buffer.getLine(y)?.translateToString(true) ?? '');
        }
        this.send({ type: 'screen', request: message.request, sequence: this.parsed, text: lines.join('\n') });
      } else if (message.type === 'find') {
        this.send({ type: 'found', request: message.request, sequence: this.parsed, result: this.find(message) });
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
