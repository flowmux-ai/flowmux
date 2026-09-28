// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { TerminalMenu } from './terminal-menu.mjs';

function fixture() {
  const actions = [], clipboardActions = [];
  let unavailable = false, focused = true, terminalFocus = 0;
  class Element {
    constructor(action, label = '', tagName = 'BUTTON') {
      this.dataset = { action }; this.textContent = label; this.tagName = tagName;
      this.listeners = {}; this.disabled = false;
    }
    addEventListener(type, fn) { this.listeners[type] = fn; }
    closest() { return this; }
    focus() { document.activeElement = this; }
    click() { if (!this.disabled) menu.listeners.click({ target: this }); }
  }
  const menu = new Element(undefined, '', 'DIV');
  menu.hidden = true; menu.style = {}; menu.offsetWidth = 220; menu.offsetHeight = 200;
  const rows = [['copy', 'Copy'], ['paste', 'Paste'], [], ['split_right', 'Split Right'],
    ['split_down', 'Split Down'], [], ['copy_path', 'Copy path'], [], ['close_pane', 'Close Pane']];
  menu.children = rows.map(([action, label]) => new Element(action, label, action ? 'BUTTON' : 'DIV'));
  const buttons = menu.children.filter(row => row.tagName === 'BUTTON');
  menu.querySelectorAll = () => buttons; menu.contains = target => target === menu || menu.children.includes(target);
  menu.getBoundingClientRect = () => ({ left: parseFloat(menu.style.left) || 0,
    top: parseFloat(menu.style.top) || 0, width: 220, height: 200 });
  const document = { activeElement: null, documentElement: { clientWidth: 400, clientHeight: 240 },
    getElementById: () => menu, hasFocus: () => focused, addEventListener() {} };
  const terminal = { modes: { mouseTrackingMode: 'none' }, options: {}, focus: () => terminalFocus++ };
  const selection = { read: () => ({ text: '한글', error: null }) };
  const clipboard = { cancel() {}, run: action => clipboardActions.push(action) };
  const controller = new TerminalMenu(terminal, selection, clipboard, document, () => unavailable, action => actions.push(action));
  const event = overrides => ({ clientX: 390, clientY: 230, preventDefault() {}, stopImmediatePropagation() {}, ...overrides });
  const configure = (state = {}) => controller.configure({ pane: 'pane-한글', split: true, close: true, ...state });
  return { controller, configure, terminal, selection, menu, buttons, document, event, actions, clipboardActions,
    unavailable: () => { unavailable = true; }, blur: () => { focused = false; }, terminalFocus: () => terminalFocus };
}

test('native state gates pane actions initially and while the menu remains open', () => {
  const f = fixture();
  assert.equal(f.buttons[2].disabled, true); assert.equal(f.buttons[3].disabled, true); assert.equal(f.buttons[5].disabled, true);
  f.configure(); f.controller.open(f.event());
  f.configure({ split: false, close: false });
  f.buttons[2].click(); f.buttons[5].click();
  assert.deepEqual(f.actions, []); assert.equal(f.menu.hidden, false);
  f.configure(); f.buttons[3].click();
  assert.deepEqual(f.actions, [{ type: 'terminal_menu_action', action: 'split_down', pane: 'pane-한글' }]);
});

test('menu fits the pane and keyboard navigation skips unavailable Copy/Paste', () => {
  const f = fixture(); f.configure(); f.selection.read = () => ({ text: '', error: null }); f.terminal.options.disableStdin = true;
  f.controller.open(f.event());
  assert.equal(f.menu.hidden, false); assert.equal(f.menu.style.left, '180px'); assert.equal(f.menu.style.top, '40px');
  assert.equal(f.document.activeElement, f.buttons[2]);
  f.menu.listeners.keydown(f.event({ key: 'ArrowUp' }));
  assert.equal(f.document.activeElement, f.buttons[5]);
  f.menu.listeners.keydown(f.event({ key: 'Home' }));
  assert.equal(f.document.activeElement, f.buttons[2]);
  f.menu.listeners.keydown(f.event({ key: 'Enter' }));
  assert.deepEqual(f.actions, [{ type: 'terminal_menu_action', action: 'split_right', pane: 'pane-한글' }]);
  assert.equal(f.menu.hidden, true);
});

test('copy retains the existing clipboard controller and pane actions carry the opening pane', () => {
  const f = fixture(); f.configure(); f.controller.open(f.event());
  f.buttons[0].click();
  assert.deepEqual(f.clipboardActions, ['copy']); assert.equal(f.menu.hidden, true);
  f.controller.open(f.event()); f.buttons[4].click();
  assert.deepEqual(f.actions, [{ type: 'terminal_menu_action', action: 'copy_path', pane: 'pane-한글' }]);
  f.controller.open(f.event()); f.blur(); f.controller.close(true);
  assert.equal(f.terminalFocus(), 2); // No restoration after the document loses focus.
});

test('a changed pane invalidates the open menu instead of targeting its new location', () => {
  const f = fixture(); f.configure(); f.controller.open(f.event());
  f.configure({ pane: 'new-pane' });
  assert.equal(f.menu.hidden, true);
  f.buttons[2].click(); assert.deepEqual(f.actions, []);
  f.controller.open(f.event()); f.buttons[2].click();
  assert.equal(f.actions[0].pane, 'new-pane');
});

test('composition and PROCESSKEY suppress menu keys; unavailable is rechecked before a click', () => {
  const f = fixture(); f.configure(); f.controller.open(f.event()); f.buttons[5].focus();
  for (const key of ['Escape', 'Enter', 'ArrowUp']) {
    for (const composition of [{ isComposing: true }, { keyCode: 229 }]) {
      let prevented = false;
      f.menu.listeners.keydown(f.event({ key, ...composition, preventDefault() { prevented = true; } }));
      assert.equal(prevented, true); // Prevent native button activation too.
    }
  }
  assert.equal(f.menu.hidden, false); assert.equal(f.document.activeElement, f.buttons[5]); assert.deepEqual(f.actions, []);
  f.unavailable(); f.buttons[5].click();
  assert.deepEqual(f.actions, []);
  f.controller.close(false); f.buttons[5].click();
  assert.deepEqual(f.actions, []); assert.equal(f.terminalFocus(), 0);
});

test('application mouse tracking retains right click; Shift opens unless unavailable', () => {
  const f = fixture(); f.terminal.modes.mouseTrackingMode = 'vt200';
  f.controller.open(f.event()); assert.equal(f.menu.hidden, true);
  f.controller.open(f.event({ shiftKey: true })); assert.equal(f.menu.hidden, false);
  f.controller.close(false); f.unavailable();
  f.controller.open(f.event({ shiftKey: true })); assert.equal(f.menu.hidden, true);
});

test('diagnostics expose grouped Linux actions, separators, bounds and native enabled state', () => {
  const f = fixture(); f.configure({ close: false }); f.controller.open(f.event());
  const state = f.controller.diagnostics();
  assert.deepEqual(state.rows.map(row => row.action), ['copy', 'paste', 'separator', 'split_right', 'split_down', 'separator', 'copy_path', 'separator', 'close_pane']);
  assert.equal(state.rows[2].enabled, false); assert.equal(state.rows[2].label, '');
  assert.equal(state.rows[8].enabled, false); assert.equal(state.rows[8].label, 'Close Pane');
  assert.deepEqual(state.viewport, { width: 400, height: 240 });
  assert.deepEqual(state.rect, { left: 180, top: 40, width: 220, height: 200 });
});
