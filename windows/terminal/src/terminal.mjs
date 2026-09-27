// SPDX-License-Identifier: GPL-3.0-or-later
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { SerializeAddon } from '@xterm/addon-serialize';
import { Output } from './output.mjs';
import { Input } from './input.mjs';
import { snapshot, restore } from './history.mjs';
import { observeCwd } from './cwd.mjs';
import { SearchUi } from './search.mjs';
import { OutputSearch } from './output-search.mjs';
import { PaneShortcuts } from './pane-shortcuts.mjs';
import { Settings, options } from './settings.mjs';

const identity = Object.freeze(window.__flowmuxIdentity);
delete window.__flowmuxIdentity;
const send = message => window.ipc.postMessage(JSON.stringify({ ...identity, message }));
const initialSettings = window.__flowmuxSettings;
delete window.__flowmuxSettings;
const terminal = new Terminal({
  ...options(initialSettings.terminal), allowProposedApi: false,
  linkHandler: { activate: (_event, url) => send({ type: 'link', url }) },
});
const fit = new FitAddon(), serialize = new SerializeAddon();
terminal.loadAddon(fit); terminal.loadAddon(serialize);
terminal.open(document.getElementById('terminal'));
const find = new SearchUi(terminal, () => new SearchAddon(), document);
const outputSearch = new OutputSearch(terminal, send);
const output = new Output(terminal, send, () => snapshot(terminal, serialize), message => find.run(message),
  () => { find.invalidate(); outputSearch.changed(); }, outputSearch);
const input = new Input(data => send({ type: 'input', data }));
let restoring = false;
observeCwd(terminal, send, () => restoring);
terminal.onData(data => { if (!restoring) input.data(data); });
terminal.onBinary(data => { if (!restoring) send({ type: 'binary_input', data }); });
terminal.onResize(({ cols, rows }) => send({ type: 'resize', cols, rows }));
terminal.onTitleChange(title => { if (!restoring) send({ type: 'title', title }); });
terminal.textarea.addEventListener('focus', () => send({ type: 'focus' }));

let composing = false;
const settings = new Settings(terminal, () => {
  if (document.body.clientWidth > 20 && document.body.clientHeight > 20) fit.fit();
}, send, () => composing || restoring, () => { find.invalidate(); outputSearch.changed(); }, document);
const paneShortcuts = new PaneShortcuts();
window.addEventListener('blur', () => paneShortcuts.reset());
terminal.textarea.addEventListener('blur', () => paneShortcuts.reset());
terminal.textarea.addEventListener('compositionstart', () => { composing = true; });
terminal.textarea.addEventListener('compositionend', () => { composing = false; queueMicrotask(() => settings.flush()); });
terminal.attachCustomKeyEventHandler(event => {
  input.keyEvent(event);
  const paneAction = paneShortcuts.event(event, composing);
  if (paneAction) {
    event.preventDefault();
    if (paneAction !== 'consume') send(paneAction);
    return false;
  }
  // Composition belongs to xterm/Windows IME. Never replay commit keys using timers.
  if (composing || event.isComposing || event.keyCode === 229) return true;
  if (event.type === 'keydown' && event.ctrlKey && event.shiftKey && event.code === 'KeyF') {
    event.preventDefault();
    find.open(true);
    return false;
  }
  return true;
});
new ResizeObserver(() => { if (!restoring && document.body.clientWidth > 20 && document.body.clientHeight > 20) fit.fit(); })
  .observe(document.getElementById('terminal'));
window.flowmuxHost = message => {
  try {
    if (message.type === 'restore') {
      restoring = true;
      terminal.options.disableStdin = true;
      restore(terminal, message.screen, () => {
        fit.fit();
        restoring = false;
        terminal.options.disableStdin = false;
        settings.flush();
        send({ type: 'restored' });
      });
    } else if (message.type === 'settings') settings.receive(message.document);
    else if (message.type === 'focus') { if (!restoring) { fit.fit(); find.focus(); } }
    else if (message.type === 'open_find') { if (!restoring) find.open(true); }
    else if (message.type === 'open_search_hit') {
      if (message.commit) { find.close(false); fit.fit(); }
      output.receive(message);
    }
    else if (message.type === 'paste') terminal.paste(message.text);
    else if (message.type === 'exit') {
      document.getElementById('status').textContent = `Process exited (${message.code})`;
      terminal.options.disableStdin = true;
    } else output.receive(message);
  } catch (error) { send({ type: 'fault', message: String(error) }); }
};
fit.fit();
settings.receive(initialSettings);
send({ type: 'ready' });
// The native host grants focus after readiness. Hidden tabs must not steal it.
