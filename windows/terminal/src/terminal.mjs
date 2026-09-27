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

const identity = Object.freeze(window.__flowmuxIdentity);
delete window.__flowmuxIdentity;
const send = message => window.ipc.postMessage(JSON.stringify({ ...identity, message }));
const terminal = new Terminal({
  cursorBlink: true, scrollback: 10000, fontFamily: 'Cascadia Mono, Consolas, "Malgun Gothic", monospace',
  fontSize: 14, allowProposedApi: false,
  theme: { background: '#17191f', foreground: '#e2e5ed', cursor: '#b9c6ff', selectionBackground: '#455483' },
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
terminal.textarea.addEventListener('compositionstart', () => { composing = true; });
terminal.textarea.addEventListener('compositionend', () => { composing = false; });
terminal.attachCustomKeyEventHandler(event => {
  input.keyEvent(event);
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
        send({ type: 'restored' });
      });
    } else if (message.type === 'focus') { if (!restoring) { fit.fit(); find.focus(); } }
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
send({ type: 'ready' });
// The native host grants focus after readiness. Hidden tabs must not steal it.
