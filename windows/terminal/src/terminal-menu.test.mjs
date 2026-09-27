// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { TerminalMenu } from './terminal-menu.mjs';

function fixture() {
  const actions = [], clipboardActions = [];
  let unavailable = false, focused = true, terminalFocus = 0;
  class Element {
    constructor(action) { this.dataset = { action }; this.listeners = {}; this.disabled = false; }
    addEventListener(type, fn) { this.listeners[type] = fn; }
    closest() { return this; }
    focus() { document.activeElement = this; }
  }
  const menu = new Element(); menu.hidden = true; menu.style = {}; menu.offsetWidth = 220; menu.offsetHeight = 140;
  const buttons = ['copy', 'paste', 'all', 'clear'].map(action => new Element(action));
  menu.querySelectorAll = () => buttons; menu.contains = target => target === menu || buttons.includes(target);
  const document = { activeElement: null, documentElement: { clientWidth: 400, clientHeight: 200 },
    getElementById: () => menu, hasFocus: () => focused, addEventListener() {} };
  const terminal = { modes: { mouseTrackingMode: 'none' }, options: {}, focus: () => terminalFocus++ };
  const selection = { read: () => ({ text: '한글', error: null }), run: action => actions.push(action) };
  const clipboard = { cancel() {}, run: action => clipboardActions.push(action) };
  const controller = new TerminalMenu(terminal, selection, clipboard, document, () => unavailable);
  const event = overrides => ({ clientX: 390, clientY: 190, preventDefault() {}, stopImmediatePropagation() {}, ...overrides });
  return { controller, terminal, selection, menu, buttons, document, event, actions, clipboardActions,
    unavailable: () => { unavailable = true; }, blur: () => { focused = false; }, terminalFocus: () => terminalFocus };
}

test('menu fits the pane and keyboard navigation skips unavailable Copy/Paste', () => {
  const f = fixture(); f.selection.read = () => ({ text: '', error: null }); f.terminal.options.disableStdin = true;
  f.controller.open(f.event());
  assert.equal(f.menu.hidden, false); assert.equal(f.menu.style.left, '180px'); assert.equal(f.menu.style.top, '60px');
  assert.equal(f.document.activeElement, f.buttons[2]);
  f.menu.listeners.keydown(f.event({ key: 'ArrowDown' }));
  assert.equal(f.document.activeElement, f.buttons[3]);
  f.menu.listeners.keydown(f.event({ key: 'Home' }));
  assert.equal(f.document.activeElement, f.buttons[2]);
});

test('menu actions share clipboard/selection controllers; Escape during IME leaves the menu alone', () => {
  const f = fixture(); f.controller.open(f.event());
  f.menu.listeners.keydown(f.event({ key: 'Escape', isComposing: true }));
  assert.equal(f.menu.hidden, false);
  f.menu.listeners.click({ target: f.buttons[0] });
  assert.deepEqual(f.clipboardActions, ['copy']); assert.equal(f.menu.hidden, true);
  f.controller.open(f.event()); f.menu.listeners.click({ target: f.buttons[3] });
  assert.deepEqual(f.actions, [{ kind: 'clear' }]);
  f.controller.open(f.event()); f.blur(); f.controller.close(true);
  assert.equal(f.terminalFocus(), 2); // No focus restoration after the document lost focus.
});

test('composition and application mouse mode retain their normal handling; Shift can open the menu', () => {
  const f = fixture(); f.terminal.modes.mouseTrackingMode = 'vt200';
  f.controller.open(f.event()); assert.equal(f.menu.hidden, true);
  f.controller.open(f.event({ shiftKey: true })); assert.equal(f.menu.hidden, false);
  f.controller.close(false); f.unavailable();
  f.controller.open(f.event({ shiftKey: true })); assert.equal(f.menu.hidden, true);
});
