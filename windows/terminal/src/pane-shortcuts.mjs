// SPDX-License-Identifier: GPL-3.0-or-later
// Rust owns defaults, overrides and conflict resolution. The renderer only matches
// the acknowledged table; xterm/Windows continue to own text composition.
const token = chord => `${chord.code}:${Number(chord.ctrl)}${Number(chord.alt)}${Number(chord.shift)}`;
const reserved = chord => !chord.alt && ((chord.ctrl && chord.shift && ['KeyC', 'KeyV'].includes(chord.code)) ||
  (chord.code === 'Insert' && chord.ctrl !== chord.shift));
const copyChord = chord => ({ code: chord.code, ctrl: chord.ctrl, alt: chord.alt, shift: chord.shift });
export class PaneShortcuts {
  constructor() { this.rightAlt = false; this.revision = null; this.bindings = []; this.lookup = new Map(); }
  reset() { this.rightAlt = false; }
  configure(revision, bindings) {
    if (typeof revision !== 'string' || !revision || revision.length > 128 || !Array.isArray(bindings) || bindings.length > 256) {
      throw new Error('Invalid resolved keybindings');
    }
    const lookup = new Map(), next = [];
    for (const binding of bindings) {
      const chord = binding?.chord;
      if (typeof binding?.action !== 'string' || !/^[a-z][a-z0-9-]{0,79}$/.test(binding.action) ||
          !chord || typeof chord.code !== 'string' || !chord.code || chord.code.length > 40 ||
          !['ctrl', 'alt', 'shift'].every(key => typeof chord[key] === 'boolean') || reserved(chord)) {
        throw new Error('Invalid or reserved resolved keybinding');
      }
      const key = token(chord);
      if (lookup.has(key)) throw new Error('Conflicting resolved keybindings');
      const item = { action: binding.action, chord: copyChord(chord) };
      next.push(item); lookup.set(key, item);
    }
    // Validate the complete replacement before removing the previous table.
    this.revision = revision; this.bindings = next; this.lookup = lookup;
  }
  snapshot() { return this.bindings.map(binding => ({ action: binding.action, chord: copyChord(binding.chord) })); }
  event(event, blocked) {
    if (event.code === 'AltRight') this.rightAlt = event.type !== 'keyup';
    if (event.type !== 'keydown' || blocked || event.isComposing || event.keyCode === 229 ||
        ['Dead', 'Process', 'Unidentified'].includes(event.key) || this.rightAlt ||
        event.getModifierState?.('AltGraph') || event.metaKey || !this.revision) return;
    const chord = { code: event.code, ctrl: !!event.ctrlKey, alt: !!event.altKey, shift: !!event.shiftKey };
    const binding = this.lookup.get(token(chord));
    if (!binding) return;
    return event.repeat ? 'consume' : { type: 'shortcut', action: binding.action, chord: copyChord(binding.chord), revision: this.revision };
  }
}
