// SPDX-License-Identifier: GPL-3.0-or-later
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { SerializeAddon } from '@xterm/addon-serialize';
import { Output } from './output.mjs';
import { Input } from './input.mjs';

const identity = Object.freeze(window.__flowmuxIdentity);
delete window.__flowmuxIdentity;
const send = message => window.ipc.postMessage(JSON.stringify({ ...identity, message }));
const terminal = new Terminal({
  cursorBlink: true, scrollback: 10000, fontFamily: 'Cascadia Mono, Consolas, "Malgun Gothic", monospace',
  fontSize: 14, allowProposedApi: false,
  theme: { background: '#17191f', foreground: '#e2e5ed', cursor: '#b9c6ff', selectionBackground: '#455483' },
  linkHandler: { activate: (_event, url) => send({ type: 'link', url }) },
});
const fit = new FitAddon(), search = new SearchAddon(), serialize = new SerializeAddon();
terminal.loadAddon(fit); terminal.loadAddon(search); terminal.loadAddon(serialize);
terminal.open(document.getElementById('terminal'));
const output = new Output(terminal, send, () => serialize.serialize());
const input = new Input(data => send({ type: 'input', data }));
terminal.onData(data => input.data(data));
terminal.onBinary(data => send({ type: 'binary_input', data }));
terminal.onResize(({ cols, rows }) => send({ type: 'resize', cols, rows }));
terminal.onTitleChange(title => send({ type: 'title', title }));
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
    document.getElementById('search').hidden = false;
    document.getElementById('query').focus();
    return false;
  }
  return true;
});
const query = document.getElementById('query');
document.getElementById('search').addEventListener('submit', event => event.preventDefault());
query.addEventListener('input', () => search.findNext(query.value, { incremental: true }));
query.addEventListener('keydown', event => {
  if (event.key === 'Escape') { document.getElementById('search').hidden = true; terminal.focus(); }
  if (event.key === 'Enter') { event.preventDefault(); search[event.shiftKey ? 'findPrevious' : 'findNext'](query.value); }
});
new ResizeObserver(() => { if (document.body.clientWidth > 20 && document.body.clientHeight > 20) fit.fit(); })
  .observe(document.getElementById('terminal'));
window.flowmuxHost = message => {
  try {
    if (message.type === 'focus') { fit.fit(); terminal.focus(); }
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
