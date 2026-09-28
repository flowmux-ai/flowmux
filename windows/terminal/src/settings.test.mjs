// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Settings } from './settings.mjs';
import { PaneShortcuts } from './pane-shortcuts.mjs';
const documentFor = (revision, font_size, theme = 'dark') => ({ revision, terminal: { font_size, theme,
  font_family: '"한글 한 😀", monospace', scrollback: 1000, cursor_blink: false, cursor_style: 'bar',
  minimap_enabled: true, minimap_width: 40, minimap_opacity: 50 } });
const bindingsFor = code => [{ action: 'terminal-search', chord: { code, ctrl: true, alt: false, shift: true } }];
const key = code => ({ type: 'keydown', key: code, code, ctrlKey: true, shiftKey: true });
function fixture() {
  const state = { blocked: false, fits: 0, changes: 0, replies: [], maps: [] };
  const terminal = { options: { fontSize: 14 }, focus() { throw Error('focus stolen'); }, write() { throw Error('input/output changed'); } };
  const document = { body: { style: {setProperty(name,value) {this[name]=value;}} } }, shortcuts = new PaneShortcuts();
  const controller = new Settings(terminal, () => { assert.equal(state.maps.at(-1).minimap_width, 40); state.fits++; },
    message => state.replies.push(message), () => state.blocked, () => state.changes++, document,
    { configure: settings => state.maps.push(settings) }, shortcuts);
  return { state, terminal, document, shortcuts, controller };
}

test('settings and bindings defer and coalesce across composition/restore without focus or input', () => {
  const { state, terminal, document, shortcuts, controller } = fixture(); state.blocked = true;
  controller.receive(documentFor('old', 20), bindingsFor('KeyF'));
  controller.receive(documentFor('new', 24, 'light'), bindingsFor('KeyG'));
  assert.equal(terminal.options.fontSize, 14); assert.equal(state.replies.length, 0);
  assert.equal(state.maps.length, 0); assert.deepEqual(shortcuts.snapshot(), []);
  state.blocked = false; controller.flush(); controller.flush();
  assert.equal(terminal.options.fontSize, 24); assert.equal(state.replies.length, 1);
  assert.equal(state.replies[0].revision, 'new'); assert.deepEqual(state.replies[0].bindings, bindingsFor('KeyG'));
  assert.equal(state.replies[0].terminal.font_family, '"한글 한 😀", monospace');
  assert.equal(state.replies[0].background, '#ffffff'); assert.equal(document.body.style.color, '#202124');
  assert.equal(state.fits, 1); assert.equal(state.changes, 1);
  assert.equal(state.maps.length, 1); assert.equal(state.replies[0].terminal.minimap_opacity, 50);
  assert.equal(shortcuts.event(key('KeyF'), false), undefined);
  assert.equal(shortcuts.event(key('KeyG'), false).revision, 'new');
});

test('shortcut messages retain the actually applied revision until the pending table is applied', () => {
  const { state, shortcuts, controller } = fixture();
  controller.receive(documentFor('revision-a', 16), bindingsFor('KeyF'));
  state.blocked = true;
  controller.receive(documentFor('revision-b', 18), bindingsFor('KeyG'));
  // Host compares this revision against its current document and rejects stale requests.
  assert.equal(shortcuts.event(key('KeyF'), false).revision, 'revision-a');
  assert.equal(shortcuts.event(key('KeyG'), false), undefined);
  assert.equal(state.replies.length, 1);
  state.blocked = false; controller.flush();
  assert.equal(shortcuts.event(key('KeyF'), false), undefined);
  assert.equal(shortcuts.event(key('KeyG'), false).revision, 'revision-b');
  assert.deepEqual(state.replies.map(reply => reply.revision), ['revision-a', 'revision-b']);
  state.replies[1].bindings[0].chord.code = 'KeyX';
  assert.equal(shortcuts.event(key('KeyG'), false).action, 'terminal-search');
});

test('invalid resolved tables cannot partially apply settings or acknowledge an unapplied revision', () => {
  const { state, terminal, shortcuts, controller } = fixture();
  controller.receive(documentFor('revision-a', 16), bindingsFor('KeyF'));
  const conflicting = [...bindingsFor('KeyG'), ...bindingsFor('KeyG')];
  assert.throws(() => controller.receive(documentFor('rejected', 20), conflicting), /Conflicting/);
  assert.equal(terminal.options.fontSize, 16); assert.equal(state.replies.length, 1);
  assert.equal(shortcuts.event(key('KeyF'), false).revision, 'revision-a');
  controller.receive(documentFor('unbound', 18), []);
  assert.equal(terminal.options.fontSize, 18); assert.deepEqual(state.replies.at(-1).bindings, []);
  assert.equal(shortcuts.event(key('KeyF'), false), undefined);
});

test('resolved palette and overrides apply together after composition and ACK actual terminal colors', () => {
  const {state,terminal,document,controller}=fixture();
  const colors={background:'#112233',foreground:'#ddeeff',cursor:'#123456',
    selection_background:'#445566',selection_foreground:'#abcdef',
    palette:Array.from({length:16},(_,i)=>'#'+i.toString(16).padStart(6,'0')),dark:true};
  state.blocked=true;
  controller.receive(documentFor('old',16),[],colors);
  const newest={...colors,background:'#223344',selection_foreground:null};
  controller.receive(documentFor('new',18),[],newest);
  assert.equal(state.replies.length,0);
  state.blocked=false;controller.flush();
  assert.deepEqual(state.replies[0].colors,newest);
  assert.equal(terminal.options.theme.brightWhite,'#00000f');
  assert.equal(terminal.options.theme.selectionForeground,undefined);
  assert.equal(document.body.style.backgroundColor,'#223344');
  assert.equal(document.body.style['--theme-accent'],'#123456');
  assert.equal(document.body.style.colorScheme,'dark');
  const before=structuredClone(terminal.options);
  assert.throws(()=>controller.receive(documentFor('invalid',22),[],{...colors,palette:[]}),/Invalid/);
  assert.throws(()=>controller.receive(documentFor('null',22),[],{...colors,background:null}),/Invalid/);
  assert.deepEqual(terminal.options,before);
  assert.equal(state.replies.length,1);
});
