// SPDX-License-Identifier: GPL-3.0-or-later
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { PaneShortcuts } from './pane-shortcuts.mjs';
import { Clipboard } from './clipboard.mjs';

const binding = (action, code, ctrl = false, alt = false, shift = false) => ({ action, chord: { code, ctrl, alt, shift } });
const key = (chord, overrides = {}) => ({ type: 'keydown', key: chord.code, code: chord.code,
  ctrlKey: chord.ctrl, altKey: chord.alt, shiftKey: chord.shift, ...overrides });
// Windows' 28 supported Rust defaults, as wire fixtures, not runtime fallbacks.
const defaults = [
  ...['Left', 'Right', 'Up', 'Down'].map(direction => binding(`focus-${direction.toLowerCase()}`, `Arrow${direction}`, false, true)),
  binding('toggle-pane-zoom', 'KeyM', true, true),
  binding('toggle-workspace-overview', 'KeyK', true, true),
  binding('terminal-search', 'KeyF', true, false, true),
  binding('new-surface', 'KeyT', true, false, true),
  binding('split-right', 'PageUp', true, false, true),
  binding('split-down', 'PageDown', true, false, true),
  binding('close-surface', 'KeyW', false, true),
  binding('quit-app', 'KeyW', true, false, true),
  binding('next-surface', 'ArrowRight', true, false, true),
  binding('prev-surface', 'ArrowLeft', true, false, true),
  binding('next-workspace', 'Tab', true),
  binding('prev-workspace', 'Tab', true, false, true),
  ...Array.from({ length: 8 }, (_, index) => binding(`workspace-${index + 1}`, `Digit${index + 1}`, false, true)),
  binding('new-browser-surface', 'KeyB', true, false, true),
  binding('new-workspace', 'KeyN', true),
  binding('search-all-terminals', 'KeyF', true, true, true),
  binding('toggle-file-browser', 'KeyF', true, true),
];
const configured = (bindings = defaults, revision = 'revision-a') => {
  const shortcuts = new PaneShortcuts(); shortcuts.configure(revision, bindings); return shortcuts;
};

test('resolved defaults dispatch only one revision-tagged host shortcut', () => {
  const shortcuts = configured();
  assert.equal(defaults.length, 28);
  for (const item of defaults) {
    assert.deepEqual(shortcuts.event(key(item.chord), false), { type: 'shortcut', ...item, revision: 'revision-a' });
    assert.equal(shortcuts.event(key(item.chord, { repeat: true }), false), 'consume');
    assert.equal(shortcuts.event(key(item.chord, { type: 'keyup' }), false), undefined);
  }
  assert.equal(shortcuts.event(key(defaults[0].chord, { shiftKey: true }), false), undefined);
  assert.equal(shortcuts.event(key(defaults[0].chord, { code: 'KeyQ', repeat: true }), false), undefined);
  // Physical code, rather than the layout-dependent character, identifies a chord.
  assert.equal(shortcuts.event(key(defaults[5].chord, { key: '한' }), false).action, 'toggle-workspace-overview');
});

test('a complete replacement rebinds and unbinds without retaining old defaults', () => {
  const shortcuts = configured(), changed = binding('toggle-workspace-overview', 'KeyO', true, true);
  shortcuts.configure('revision-b', [changed]);
  assert.equal(shortcuts.event(key(defaults[5].chord), false), undefined);
  assert.equal(shortcuts.event(key(defaults[0].chord), false), undefined);
  assert.deepEqual(shortcuts.event(key(changed.chord), false), { type: 'shortcut', ...changed, revision: 'revision-b' });
  shortcuts.configure('revision-c', []);
  assert.equal(shortcuts.event(key(changed.chord), false), undefined);
  assert.deepEqual(shortcuts.snapshot(), []);
  assert.equal(new PaneShortcuts().event(key(defaults[0].chord), false), undefined);
});

test('conflicts, reserved clipboard chords and malformed tables cannot replace an applied table', () => {
  const shortcuts = configured();
  const invalid = [
    [defaults[0], { ...defaults[0], action: 'focus-right' }],
    [binding('new-surface', 'KeyC', true, false, true)],
    [binding('new-surface', 'KeyV', true, false, true)],
    [binding('new-surface', 'Insert', true)],
    [binding('new-surface', 'Insert', false, false, true)],
    [{ action: 'new-surface', chord: { code: 'KeyT', ctrl: 1, alt: false, shift: false } }],
    [{ action: '', chord: defaults[0].chord }], null,
  ];
  for (const table of invalid) {
    assert.throws(() => shortcuts.configure('rejected-revision', table), /keybinding/i);
    assert.deepEqual(shortcuts.snapshot(), defaults);
    assert.equal(shortcuts.event(key(defaults[0].chord), false).revision, 'revision-a');
  }
});

test('composition, restoration and paste guards do not consume terminal input', () => {
  const shortcuts = configured(), chord = defaults[5].chord;
  for (const overrides of [{ isComposing: true }, { keyCode: 229 }, { key: 'Dead' }, { key: 'Process' },
    { key: 'Unidentified' }, { metaKey: true }, { getModifierState: name => name === 'AltGraph' }]) {
    assert.equal(shortcuts.event(key(chord, overrides), false), undefined);
  }
  // The caller combines composition, history restore, paste settling and disableStdin.
  assert.equal(shortcuts.event(key(chord), true), undefined);
  shortcuts.event(key(chord, { code: 'AltRight', key: 'Alt' }), true);
  assert.equal(shortcuts.event(key(chord), false), undefined);
  shortcuts.event(key(chord, { code: 'AltRight', key: 'Alt', type: 'keyup' }), true);
  assert.equal(shortcuts.event(key(chord), false).action, 'toggle-workspace-overview');
  shortcuts.event(key(chord, { code: 'AltRight', key: 'Alt' }), false);
  shortcuts.reset();
  assert.equal(shortcuts.event(key(chord), false).action, 'toggle-workspace-overview');
});

test('bindings, acknowledgements and dispatched chords cannot mutate the applied lookup', () => {
  const table = [binding('terminal-search', 'KeyF', true, false, true)], shortcuts = configured(table);
  table[0].action = 'new-surface'; table[0].chord.code = 'KeyN';
  const snapshot = shortcuts.snapshot(); snapshot[0].chord.code = 'KeyX';
  const chord = binding('terminal-search', 'KeyF', true, false, true).chord;
  const sent = shortcuts.event(key(chord), false); sent.chord.code = 'KeyY';
  assert.equal(shortcuts.event(key(chord), false).action, 'terminal-search');
  assert.equal(shortcuts.snapshot()[0].chord.code, 'KeyF');
});

test('fixed clipboard shortcuts keep priority without accessing a real clipboard', () => {
  const shortcuts = configured(); let prevented = 0;
  const clipboard = new Clipboard(null, null, null, () => {}, () => {}, () => false, () => false);
  for (const chord of [binding('', 'KeyC', true, false, true).chord, binding('', 'KeyV', true, false, true).chord,
    binding('', 'Insert', true).chord, binding('', 'Insert', false, false, true).chord]) {
    const event = key(chord, { preventDefault: () => prevented++ });
    assert.equal(clipboard.key(event, false), true);
    assert.equal(shortcuts.event(event, false), undefined);
  }
  assert.equal(prevented, 4);
});
