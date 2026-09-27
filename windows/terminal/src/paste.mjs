// SPDX-License-Identifier: GPL-3.0-or-later
export const MAX_PASTE_BYTES = 128 * 1024;

function validate(text) {
  if (typeof text !== 'string') throw new Error('Paste requires text.');
  if (text.length > MAX_PASTE_BYTES) throw new Error('Paste exceeds 128 KiB of UTF-8 text.');
  if (text.includes('\0')) throw new Error('Paste text contains NUL.');
  // Do not replace an unpaired surrogate or normalize decomposed Hangul.
  for (const character of text) {
    const code = character.codePointAt(0);
    if (code >= 0xd800 && code <= 0xdfff) throw new Error('Paste text contains invalid Unicode.');
  }
  if (new TextEncoder().encode(text).length > MAX_PASTE_BYTES)
    throw new Error('Paste exceeds 128 KiB of UTF-8 text.');
}

export class Paste {
  constructor(terminal, unavailable) {
    this.terminal = terminal;
    this.unavailable = unavailable;
    this.captured = null;
    this.settling = null;
  }
  settleComposition() {
    const token = this.settling = {};
    // xterm 6 finalizes compositionend/keyCode 229 from a zero-delay task.
    // Register after its event handler, including when our key hook runs first.
    // This timer only releases a paste guard; it never emits/replays text.
    queueMicrotask(() => setTimeout(() => {
      if (this.settling === token) this.settling = null;
    }, 0));
  }
  // Called before the ordinary keyboard/Shift+Enter path. xterm.paste emits
  // synchronously; the captured data travels in one acknowledged paste reply.
  data(data) {
    if (this.captured === null) return false;
    this.captured.push(data);
    return true;
  }
  run(text) {
    try {
      const reason = this.unavailable();
      if (reason) throw new Error(reason);
      if (this.settling) throw new Error('Finish composing text before pasting.');
      if (this.terminal.options.disableStdin) throw new Error('Terminal input is unavailable.');
      if (this.captured !== null) throw new Error('Another paste is in progress.');
      validate(text);
      // Empty clipboard text must not insert an empty bracketed-paste block.
      if (text === '') return { status: 'ok', data: '', bracketed: false };
      this.captured = [];
      const bracketed = this.terminal.modes.bracketedPasteMode && !this.terminal.options.ignoreBracketedPasteMode;
      try {
        this.terminal.paste(text);
        if (this.captured.length !== 1) throw new Error('Terminal did not produce one paste input.');
        return { status: 'ok', data: this.captured[0], bracketed };
      } finally { this.captured = null; }
    } catch (error) { return { status: 'error', message: String(error.message).slice(0, 256) }; }
  }
  event(event, submit) {
    if (event.defaultPrevented) return;
    // Capture on the terminal ancestor before xterm's two bubbling handlers.
    // Never install on the document: the Find field owns its normal paste.
    event.preventDefault();
    event.stopImmediatePropagation();
    if (!event.clipboardData) {
      submit({ status: 'error', message: 'Clipboard text is unavailable.' });
      return;
    }
    submit(this.run(event.clipboardData.getData('text/plain')));
  }
}
