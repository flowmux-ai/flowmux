// SPDX-License-Identifier: GPL-3.0-or-later
// Read barriers resolve only after xterm's parser callback, including hidden tabs.
export class Output {
  constructor(terminal, send, serialize) {
    this.terminal = terminal;
    this.send = send;
    this.serialize = serialize;
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
        this.send({ type: 'ack', sequence: this.parsed });
        this.flush();
      });
    } else if (message.type === 'read_screen' || message.type === 'snapshot') {
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
      } else {
        this.send({ type: 'snapshot', request: message.request, sequence: this.parsed, data: this.serialize() });
      }
    }
    this.pending = waiting;
  }
}
