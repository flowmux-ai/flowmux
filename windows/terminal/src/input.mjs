// SPDX-License-Identifier: GPL-3.0-or-later
// xterm remains the sole owner of composition and text production. A shortcut
// changes only a CR xterm actually emits during that key event, after its commit.
export class Input {
  constructor(send) { this.send = send; this.key = undefined; }
  keyEvent(event) {
    const key = this.key = event;
    queueMicrotask(() => { if (this.key === key) this.key = undefined; });
  }
  data(data) {
    const key = this.key;
    if (data === '\r' && key?.type === 'keydown' && key.key === 'Enter' && key.shiftKey && key.keyCode !== 229) {
      this.send('\x1b\r');
    } else this.send(data);
  }
}
