// SPDX-License-Identifier: GPL-3.0-or-later
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SearchAddon } from '@xterm/addon-search';
import { SerializeAddon } from '@xterm/addon-serialize';
import { Output } from './output.mjs';
import { Input } from './input.mjs';
import { Paste } from './paste.mjs';
import { keyMode } from './named-key.mjs';
import { Selection } from './selection.mjs';
import { Clipboard } from './clipboard.mjs';
import { TerminalMenu } from './terminal-menu.mjs';
import { snapshot, restore } from './history.mjs';
import { observeCwd } from './cwd.mjs';
import { SearchUi } from './search.mjs';
import { OutputSearch } from './output-search.mjs';
import { PaneShortcuts } from './pane-shortcuts.mjs';
import { Settings, options } from './settings.mjs';
import { Minimap } from './minimap.mjs';

const identity = Object.freeze(window.__flowmuxIdentity);
delete window.__flowmuxIdentity;
const send = message => window.ipc.postMessage(JSON.stringify({ ...identity, message }));
const initialSettings = window.__flowmuxSettings;
delete window.__flowmuxSettings;
const backgroundTesting = window.__flowmuxBackgroundTesting === true;
delete window.__flowmuxBackgroundTesting;
const terminal = new Terminal({
  ...options(initialSettings.terminal), allowProposedApi: false,
  linkHandler: { activate: (_event, url) => send({ type: 'link', url }) },
});
const fit = new FitAddon(), serialize = new SerializeAddon();
terminal.loadAddon(fit); terminal.loadAddon(serialize);
terminal.open(document.getElementById('terminal'));
const selection = new Selection(terminal);
const find = new SearchUi(terminal, () => new SearchAddon(), document, selection);
const outputSearch = new OutputSearch(terminal, send, undefined, selection);
let restoring = false, composing = false, surfaceVisible = false;
const minimap = new Minimap(terminal, document, () => composing || restoring);
const paste = new Paste(terminal, () => composing ? 'Finish composing text before pasting.'
  : restoring ? 'Terminal history is being restored.' : null);
const output = new Output(terminal, send, () => snapshot(terminal, serialize), message => find.run(message),
  () => { find.invalidate(); outputSearch.changed(); minimap.changed(); }, outputSearch, paste, selection, minimap, () => keyMode(terminal, () => {
    if (composing || paste.settling) return 'Finish composing text before sending a named key.';
    if (restoring) return 'Terminal history is being restored.';
    return null;
  }));
const clipboard = new Clipboard(selection, paste, navigator.clipboard,
  outcome => send({ type: 'pasted', request: null, sequence: output.parsed, outcome }),
  message => { document.getElementById('input-status').textContent = message; },
  () => !backgroundTesting && surfaceVisible && document.hasFocus() && !document.hidden,
  () => !composing && !restoring && !paste.settling && !terminal.options.disableStdin);
const menu = new TerminalMenu(terminal, selection, clipboard, document, () => composing || restoring || paste.settling);
const terminalElement = document.getElementById('terminal');
terminalElement.addEventListener('contextmenu', event => menu.open(event), true);
terminalElement.addEventListener('pointerdown', event => { if (event.button === 0) { clipboard.cancel(); selection.primaryDown(); } }, true);
document.addEventListener('pointerup', event => { if (event.button === 0) selection.primaryUp(event.altKey); });
document.addEventListener('pointercancel', () => selection.primaryUp());
terminalElement.addEventListener('copy', event => selection.copyEvent(event, clipboard.report, !backgroundTesting), true);
const input = new Input(data => send({ type: 'input', data }));
observeCwd(terminal, send, () => restoring);
terminal.onData(data => { if (!paste.data(data) && !restoring) input.data(data); });
terminalElement.addEventListener('paste', event => { clipboard.cancel(); paste.event(event, outcome => {
  if (outcome.status === 'ok' && outcome.data) selection.forget();
  send({ type: 'pasted', request: null, sequence: output.parsed, outcome });
}); }, true);
terminal.onBinary(data => { if (!restoring) send({ type: 'binary_input', data }); });
terminal.onResize(({ cols, rows }) => send({ type: 'resize', cols, rows }));
terminal.onTitleChange(title => { if (!restoring) send({ type: 'title', title }); });
terminal.textarea.addEventListener('focus', () => send({ type: 'focus' }));
terminal.textarea.addEventListener('beforeinput', () => { clipboard.cancel(); selection.forget(); }, true);

const settings = new Settings(terminal, () => {
  if (document.body.clientWidth > 20 && document.body.clientHeight > 20) fit.fit();
}, send, () => composing || restoring, () => { find.invalidate(); outputSearch.changed(); }, document, minimap);
const paneShortcuts = new PaneShortcuts();
window.addEventListener('blur', () => { paneShortcuts.reset(); clipboard.cancel(); menu.close(false); selection.primaryUp(); });
document.addEventListener('visibilitychange', () => {
  if (document.hidden) { clipboard.cancel(); menu.close(false); }
  minimap.visibility(surfaceVisible && (!document.hidden || backgroundTesting));
});
window.addEventListener('resize', () => menu.close(false));
terminal.textarea.addEventListener('blur', () => { paneShortcuts.reset(); clipboard.cancel(); });
terminal.textarea.addEventListener('compositionstart', () => { composing = true; clipboard.cancel(); selection.forget(); });
terminal.textarea.addEventListener('compositionend', () => {
  paste.settleComposition(); composing = false; queueMicrotask(() => settings.flush());
});
terminal.attachCustomKeyEventHandler(event => {
  if (clipboard.key(event, composing || restoring || !!paste.settling || paneShortcuts.rightAlt)) return false;
  input.keyEvent(event);
  if (event.type === 'keydown' && event.keyCode === 229) paste.settleComposition();
  const paneAction = paneShortcuts.event(event, composing);
  if (paneAction) {
    event.preventDefault();
    if (paneAction !== 'consume') send(paneAction);
    return false;
  }
  // Composition belongs to xterm/Windows IME. Never replay commit keys using timers.
  if (composing || event.isComposing || event.keyCode === 229) {
    if (event.type === 'keydown') { clipboard.cancel(); selection.forget(); }
    return true;
  }
  if (event.type === 'keydown' && event.ctrlKey && event.shiftKey && event.code === 'KeyF') {
    event.preventDefault();
    find.open(true);
    return false;
  }
  if (event.type === 'keydown' && !['Shift', 'Control', 'Alt', 'Meta'].includes(event.key)) {
    clipboard.cancel(); selection.forget();
  }
  return true;
});
new ResizeObserver(() => {
  if (!restoring && document.body.clientWidth > 20 && document.body.clientHeight > 20) fit.fit();
  minimap.changed();
})
  .observe(document.getElementById('terminal'));
window.flowmuxHost = message => {
  try {
    if (message.type === 'restore') {
      clipboard.cancel(); selection.clear(); menu.close(false);
      restoring = true;
      terminal.options.disableStdin = true;
      restore(terminal, message.screen, () => {
        fit.fit();
        restoring = false;
        terminal.options.disableStdin = false;
        settings.flush();
        minimap.changed();
        send({ type: 'restored' });
      });
    } else if (message.type === 'visibility') {
      surfaceVisible = message.visible;
      minimap.visibility(surfaceVisible && (!document.hidden || backgroundTesting));
      if (!surfaceVisible) { clipboard.cancel(); menu.close(false); selection.primaryUp(); }
    } else if (message.type === 'settings') settings.receive(message.document);
    else if (message.type === 'focus') { if (!restoring) { fit.fit(); find.focus(); } }
    else if (message.type === 'open_find') { if (!restoring) find.open(true); }
    else if (message.type === 'open_search_hit') {
      if (message.commit) { find.close(false); fit.fit(); }
      output.receive(message);
    }
    else if (message.type === 'paste_result') {
      document.getElementById('input-status').textContent = message.error ? `Paste failed: ${message.error}` : '';
    }
    else if (message.type === 'shell_status') {
      const status=document.getElementById('status');
      status.textContent=message.error ? `Shell could not start: ${message.error} ` : '';
      terminal.options.disableStdin=!!message.error;
      if (message.error) {
        const retry=document.createElement('button'); retry.textContent='Start Command Prompt';
        retry.addEventListener('click',()=>send({type:'retry_command_prompt'})); status.append(retry);
      }
    }
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
