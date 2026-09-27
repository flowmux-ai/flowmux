// SPDX-License-Identifier: GPL-3.0-or-later
// Clipboard permissions and focus belong to the trusted terminal document.
// No textarea replacement or IME commit is used to copy/paste.
export class Clipboard {
  constructor(selection, paste, clipboard, sendPaste, report, available, canPaste) {
    Object.assign(this, { selection, paste, clipboard, sendPaste, report, available, canPaste });
    this.epoch = 0;
    this.busy = false;
  }
  cancel() { this.epoch++; }
  key(event, composing) {
    if (event.type !== 'keydown' || composing || event.isComposing || event.keyCode === 229 ||
        event.altKey || event.metaKey || event.getModifierState?.('AltGraph')) return false;
    const copy = (event.ctrlKey && event.shiftKey && event.code === 'KeyC') ||
      (event.ctrlKey && !event.shiftKey && event.key === 'Insert');
    const paste = (event.ctrlKey && event.shiftKey && event.code === 'KeyV') ||
      (!event.ctrlKey && event.shiftKey && event.key === 'Insert');
    if (!copy && !paste) return false;
    event.preventDefault();
    if (!event.repeat) { this.cancel(); void this.run(copy ? 'copy' : 'paste'); }
    return true;
  }
  async run(action) {
    if (this.busy) { this.report('A clipboard request is still pending.'); return; }
    if (!this.available()) { this.report('Clipboard is unavailable in this terminal.'); return; }
    if (typeof this.clipboard?.[action === 'copy' ? 'writeText' : 'readText'] !== 'function') {
      this.report('Clipboard is unavailable in this terminal.'); return;
    }
    const epoch = this.epoch;
    this.busy = true;
    try {
      if (action === 'copy') {
        const result = this.selection.read();
        if (result.error) throw new Error(result.error);
        if (!result.text) { this.report('No text selected.'); return; }
        await this.clipboard.writeText(result.text);
        if (epoch === this.epoch) this.report('');
      } else if (action === 'paste') {
        if (!this.canPaste()) throw new Error('Terminal input is unavailable or text is still being composed.');
        const text = await this.clipboard.readText();
        // A delayed clipboard read must not paste after focus/tab/composition
        // changed, even if the user has already returned to this same terminal.
        if (epoch !== this.epoch || !this.available() || !this.canPaste()) return;
        const outcome = this.paste.run(text);
        if (outcome.status === 'ok' && text) this.selection.forget();
        this.sendPaste(outcome);
      }
    } catch (error) {
      if (epoch === this.epoch) this.report(error?.message || 'Clipboard request failed.');
    } finally { this.busy = false; }
  }
}
