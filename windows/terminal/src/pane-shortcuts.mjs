// SPDX-License-Identifier: GPL-3.0-or-later
// Only navigation keys are intercepted. xterm/Windows remain the composition owner.
export class PaneShortcuts {
  constructor() { this.rightAlt = false; }
  reset() { this.rightAlt = false; }
  event(event, composing) {
    if (event.code === 'AltRight') this.rightAlt = event.type !== 'keyup';
    if (event.type !== 'keydown' || composing || event.isComposing || event.keyCode === 229 ||
        this.rightAlt || event.getModifierState?.('AltGraph') || event.metaKey || event.shiftKey) return;
    if (!event.altKey) return;
    if (event.ctrlKey) {
      if (event.code === 'KeyM') return event.repeat ? 'consume' : { type: 'toggle_pane_zoom' };
      if (event.code === 'KeyK') return event.repeat ? 'consume' : { type: 'toggle_overview' };
      return;
    }
    const direction = { ArrowLeft: 'left', ArrowRight: 'right', ArrowUp: 'up', ArrowDown: 'down' }[event.key];
    if (direction) return event.repeat ? 'consume' : { type: 'focus_direction', direction };
  }
}
