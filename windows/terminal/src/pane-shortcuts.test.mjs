// SPDX-License-Identifier: GPL-3.0-or-later
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { PaneShortcuts } from './pane-shortcuts.mjs';

const key = overrides => ({ type: 'keydown', key: 'ArrowLeft', code: 'ArrowLeft', altKey: true, ...overrides });
test('pane navigation selects directions and prevents repeated zoom toggles', () => {
  const shortcuts = new PaneShortcuts();
  for (const direction of ['Left', 'Right', 'Up', 'Down']) {
    assert.deepEqual(shortcuts.event(key({ key: `Arrow${direction}` }), false), { type: 'focus_direction', direction: direction.toLowerCase() });
  }
  assert.deepEqual(shortcuts.event(key({ key: 'm', code: 'KeyM', ctrlKey: true }), false), { type: 'toggle_pane_zoom' });
  assert.equal(shortcuts.event(key({ key: 'm', code: 'KeyM', ctrlKey: true, repeat: true }), false), 'consume');
  assert.deepEqual(shortcuts.event(key({ key: 'k', code: 'KeyK', ctrlKey: true }), false), { type: 'toggle_overview' });
  assert.equal(shortcuts.event(key({ key: 'k', code: 'KeyK', ctrlKey: true, repeat: true }), false), 'consume');
  for (const override of [{ isComposing: true }, { keyCode: 229 }, { getModifierState: () => true }]) {
    assert.equal(shortcuts.event(key({ code: 'KeyK', ctrlKey: true, ...override }), false), undefined);
  }
  assert.equal(shortcuts.event(key({ repeat: true }), false), 'consume');
  assert.equal(shortcuts.event(key({ type: 'keyup' }), false), undefined);
});

test('composition, AltGr and non-navigation keys retain normal terminal handling', () => {
  const shortcuts = new PaneShortcuts();
  assert.equal(shortcuts.event(key({}), true), undefined);
  for (const overrides of [{ isComposing: true }, { keyCode: 229 }, { shiftKey: true }, { metaKey: true },
    { ctrlKey: true }, { getModifierState: name => name === 'AltGraph' }, { altKey: false }, { key: 'Enter' }, { key: '한', code: 'KeyG' }]) {
    assert.equal(shortcuts.event(key(overrides), false), undefined);
  }
  shortcuts.event(key({ key: 'Alt', code: 'AltRight' }), false);
  assert.equal(shortcuts.event(key({ code: 'KeyM', ctrlKey: true }), false), undefined);
  shortcuts.event(key({ type: 'keyup', key: 'Alt', code: 'AltRight' }), false);
  assert.deepEqual(shortcuts.event(key({}), false), { type: 'focus_direction', direction: 'left' });
  shortcuts.event(key({ key: 'Alt', code: 'AltRight' }), false);
  shortcuts.reset();
  assert.deepEqual(shortcuts.event(key({}), false), { type: 'focus_direction', direction: 'left' });
});
