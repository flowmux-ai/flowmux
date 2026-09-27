// SPDX-License-Identifier: GPL-3.0-or-later
export class TerminalMenu {
  constructor(terminal, selection, clipboard, document, unavailable) {
    Object.assign(this, { terminal, selection, clipboard, document, unavailable });
    this.menu = document.getElementById('terminal-menu');
    this.buttons = [...this.menu.querySelectorAll('button')];
    this.menu.addEventListener('click', event => {
      const button = event.target.closest('button');
      if (!button || button.disabled || !this.menu.contains(button)) return;
      const action = button.dataset.action;
      this.close(true);
      if (action === 'copy' || action === 'paste') void clipboard.run(action);
      else selection.run({ kind: action });
    });
    this.menu.addEventListener('keydown', event => {
      if (event.isComposing || event.keyCode === 229) return;
      if (event.key === 'Escape') { event.preventDefault(); this.close(true); return; }
      if (event.key === 'Tab') { this.close(false); return; }
      const enabled = this.buttons.filter(button => !button.disabled);
      const index = enabled.indexOf(document.activeElement);
      const next = { ArrowDown: (index + 1) % enabled.length,
        ArrowUp: (index + enabled.length - 1) % enabled.length, Home: 0, End: enabled.length - 1 }[event.key];
      if (next !== undefined) { event.preventDefault(); enabled[next]?.focus(); }
    });
    document.addEventListener('pointerdown', event => { if (!this.menu.contains(event.target)) this.close(false); }, true);
    this.menu.addEventListener('focusout', () => queueMicrotask(() => {
      if (!this.menu.contains(document.activeElement)) this.close(false);
    }));
  }
  open(event) {
    if (this.unavailable() || (this.terminal.modes.mouseTrackingMode !== 'none' && !event.shiftKey)) return;
    event.preventDefault(); event.stopImmediatePropagation();
    this.clipboard.cancel();
    const selection = this.selection.read();
    for (const button of this.buttons) {
      button.disabled = button.dataset.action === 'copy' ? !selection.text || !!selection.error
        : button.dataset.action === 'paste' ? this.terminal.options.disableStdin : false;
    }
    this.menu.hidden = false;
    const root = this.document.documentElement;
    this.menu.style.left = `${Math.max(0, Math.min(event.clientX, root.clientWidth - this.menu.offsetWidth))}px`;
    this.menu.style.top = `${Math.max(0, Math.min(event.clientY, root.clientHeight - this.menu.offsetHeight))}px`;
    this.buttons.find(button => !button.disabled)?.focus();
  }
  close(focus) {
    if (this.menu.hidden) return;
    this.menu.hidden = true;
    if (focus && this.document.hasFocus()) this.terminal.focus();
  }
}
