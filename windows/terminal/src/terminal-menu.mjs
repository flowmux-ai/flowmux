// SPDX-License-Identifier: GPL-3.0-or-later
export class TerminalMenu {
  constructor(terminal, selection, clipboard, document, unavailable, send) {
    Object.assign(this, { terminal, selection, clipboard, document, unavailable, send });
    this.menu = document.getElementById('terminal-menu');
    this.buttons = [...this.menu.querySelectorAll('button')];
    this.configure({ split: false, close: false, pane: null });
    this.menu.addEventListener('click', event => {
      const button = event.target.closest('button');
      if (this.menu.hidden || this.unavailable() || !button || button.disabled || !this.menu.contains(button)) return;
      const action = button.dataset.action;
      this.close(true);
      if (action === 'copy' || action === 'paste') void clipboard.run(action);
      else if (this.openedPane && ['split_right', 'split_down', 'copy_path', 'close_pane'].includes(action)) {
        send({ type: 'terminal_menu_action', action, pane: this.openedPane });
      }
    });
    this.menu.addEventListener('keydown', event => {
      if (this.menu.hidden || this.unavailable() || event.isComposing || event.keyCode === 229) {
        event.preventDefault();
        return;
      }
      if (event.key === 'Escape') { event.preventDefault(); this.close(true); return; }
      if (event.key === 'Tab') { this.close(false); return; }
      if (event.key === 'Enter' || event.key === ' ') {
        event.preventDefault();
        const button = this.buttons.find(button => button === document.activeElement && !button.disabled);
        button?.click();
        return;
      }
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
  configure({ split, close, pane }) {
    if (this.state?.pane !== pane) this.close(false);
    this.state = { split: split === true, close: close === true, pane };
    for (const button of this.buttons) {
      if (button.dataset.action.startsWith('split_')) button.disabled = !this.state.split;
      else if (button.dataset.action === 'close_pane') button.disabled = !this.state.close;
    }
  }
  open(event) {
    if (this.unavailable() || (this.terminal.modes.mouseTrackingMode !== 'none' && !event.shiftKey)) return;
    event.preventDefault(); event.stopImmediatePropagation();
    this.clipboard.cancel();
    this.openedPane = this.state.pane;
    const selection = this.selection.read();
    for (const button of this.buttons) {
      button.disabled = button.dataset.action === 'copy' ? !selection.text || !!selection.error
        : button.dataset.action === 'paste' ? !!this.terminal.options.disableStdin
        : button.dataset.action.startsWith('split_') ? !this.state.split
        : button.dataset.action === 'close_pane' ? !this.state.close : false;
    }
    this.menu.hidden = false;
    const root = this.document.documentElement;
    this.menu.style.left = `${Math.max(0, Math.min(event.clientX, root.clientWidth - this.menu.offsetWidth))}px`;
    this.menu.style.top = `${Math.max(0, Math.min(event.clientY, root.clientHeight - this.menu.offsetHeight))}px`;
    this.buttons.find(button => !button.disabled)?.focus();
  }
  diagnostics() {
    const rect = this.menu.getBoundingClientRect(), root = this.document.documentElement;
    return { hidden: this.menu.hidden,
      rows: [...this.menu.children].map(row => ({ action: row.dataset.action || 'separator',
        label: row.textContent.trim(), enabled: row.tagName === 'BUTTON' && !row.disabled })),
      rect: { left: rect.left, top: rect.top, width: rect.width, height: rect.height },
      viewport: { width: root.clientWidth, height: root.clientHeight } };
  }
  close(focus) {
    if (this.menu.hidden) return;
    this.menu.hidden = true;
    if (focus && this.document.hasFocus()) this.terminal.focus();
  }
}
