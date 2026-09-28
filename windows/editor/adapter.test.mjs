// SPDX-License-Identifier: GPL-3.0-or-later
// Static bridge/adapter tests only. Native verification exercises real Monaco.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";
import { SearchParams, TextModelSearch } from "monaco-editor/esm/vs/editor/common/model/textModelSearch.js";
import { Range } from "monaco-editor/esm/vs/editor/common/core/range.js";
import { Position } from "monaco-editor/esm/vs/editor/common/core/position.js";

const initializer = await readFile(new URL("./initialize.js", import.meta.url), "utf8");
const adapter = await readFile(new URL("./adapter.js", import.meta.url), "utf8");
const paneShortcuts = await readFile(new URL("../terminal/src/pane-shortcuts.mjs", import.meta.url), "utf8");

function initialization(background, actualUrl = "http://127.0.0.1:1234/token/index.html?surface=s") {
  const sent = [];
  const handlers = [];
  const dialogs = [];
  class Element { focus() { this.focused = true; } }
  class Dialog extends Element {
    constructor() { super(); dialogs.push(this); }
    show() { this.open = true; this.nonmodal = true; }
    showModal() { this.open = true; this.modal = true; }
  }
  class DomEvent {
    constructor(type, values = {}) { this.type = type; Object.assign(this, values); this.defaultPrevented = false; this.stopped = false; }
    preventDefault() { this.defaultPrevented = true; }
    stopImmediatePropagation() { this.stopped = true; }
    getModifierState(name) { return name === "AltGraph" && !!this.modifierAltGraph; }
  }
  const window = { location: { href: actualUrl }, ipc: { postMessage: (raw) => sent.push(JSON.parse(raw)) }, addEventListener: (name, handler) => handlers.push([name, handler]) };
  window.dispatchEvent = (event) => {
    for (const [name, handler] of handlers) {
      if (event.type === name) handler(event);
      if (event.stopped) break;
    }
    return !event.defaultPrevented;
  };
  window.top = window;
  const context = vm.createContext({ window, document: { addEventListener() {}, querySelector: () => dialogs.find(dialog => dialog.open) ?? null },
    HTMLElement: Element, HTMLDialogElement: Dialog, KeyboardEvent: DomEvent, CompositionEvent: DomEvent, Event: DomEvent });
  const init = vm.runInContext(`(() => { ${paneShortcuts.replace("export class PaneShortcuts", "class PaneShortcuts")}\nreturn (${initializer}); })()`, context);
  init({ url: "http://127.0.0.1:1234/token/index.html?surface=s", signal_id: "s", credential: "private", background, revision: "revision-1", bindings: [] });
  return { window, sent, handlers, Element, Dialog };
}

test("initialization authenticates the exact page and hidden focus guards precede bundle execution", () => {
  const state = initialization(true);
  const element = new state.Element(); element.focus();
  assert.equal(element.focused, undefined);
  const dialog = new state.Dialog(); dialog.showModal();
  assert.equal(dialog.nonmodal, true);
  assert.equal(dialog.modal, undefined);
  state.window.webkit.messageHandlers.flowmuxEditor.postMessage('{"type":"editor_ready"}');
  assert.deepEqual(state.sent[0], { kind: "keybindings_applied", revision: "revision-1", bindings: [], signal_id: "s", credential: "private" });
  assert.deepEqual(state.sent.at(-1), { kind: "editor_message", message: { type: "editor_ready" }, signal_id: "s", credential: "private" });
  const foreign = initialization(true, "http://127.0.0.1:1234/other/index.html?surface=s");
  assert.equal(foreign.window.webkit, undefined);
  const normal = initialization(false); const normalElement = new normal.Element(); normalElement.focus();
  assert.equal(normalElement.focused, true);
  state.window.__flowmuxWindowsEditorSealed = true;
  let prevented = false, stopped = false;
  state.handlers.find(([name]) => name === "beforeinput")[1]({ preventDefault() { prevented = true; }, stopImmediatePropagation() { stopped = true; } });
  assert.equal(prevented && stopped, true);
});

test("editor app shortcuts preserve composition, dialog and seal guards while replacing resolved bindings", () => {
  const state = initialization(true), api = state.window.__flowmuxWindowsEditorShortcuts;
  const binding = (code) => ({ action: "new-tab", chord: { code, ctrl: true, alt: false, shift: true } });
  const key = (extra = {}) => ({ code: "KeyT", key: "t", ctrlKey: true, shiftKey: true, ...extra });
  const dispatch = (extra = {}) => api.test(key(extra));
  const releases = () => dispatch({ type: "keyup" });
  api.configure("revision-2", [binding("KeyT")]);
  assert.deepEqual(state.sent.at(-1), { kind: "keybindings_applied", revision: "revision-2", bindings: [binding("KeyT")], signal_id: "s", credential: "private" });
  let localKeys = 0;
  state.window.addEventListener("keydown", () => { localKeys += 1; });
  assert.equal(dispatch(), true);
  assert.deepEqual(state.sent.at(-1), { kind: "shortcut", action: "new-tab", chord: binding("KeyT").chord,
    revision: "revision-2", signal_id: "s", credential: "private" });
  const sent = state.sent.length;
  assert.equal(dispatch({ repeat: true }), true);
  assert.equal(state.sent.length, sent, "repeat consumes without another native action");
  api.test({ type: "compositionstart", data: "한" });
  assert.equal(dispatch(), false);
  api.configure("revision-3", [binding("KeyY")]);
  api.test({ type: "compositionend", data: "한" });
  assert.equal(dispatch({ code: "KeyY" }), false);
  assert.equal(api.test({ type: "keyup", code: "ControlLeft", key: "Control" }), false);
  assert.equal(dispatch({ code: "KeyY" }), false, "modifier release must not complete composition settling");
  releases();
  assert.equal(dispatch(), false, "old binding was removed");
  assert.equal(dispatch({ code: "KeyY" }), true);
  assert.equal(state.sent.at(-1).revision, "revision-3");
  api.configure("revision-4", [binding("KeyT")]);
  for (const blocked of [{ isComposing: true }, { keyCode: 229 }, { key: "Dead" }, { key: "Process" }, { altGraph: true }, { metaKey: true }]) {
    assert.equal(dispatch(blocked), false);
    releases();
  }
  api.test({ code: "AltRight", key: "Alt", altKey: true });
  assert.equal(dispatch(), false);
  api.test({ type: "blur" });
  assert.equal(dispatch(), true, "blur releases stale right-Alt state");
  const dialog = new state.Dialog(); dialog.showModal();
  const beforeBlocked = state.sent.length;
  assert.equal(dispatch(), false);
  dialog.open = false;
  state.window.__flowmuxWindowsEditorSealed = true;
  assert.equal(dispatch(), false);
  api.test({ code: "AltRight", key: "Alt", altKey: true });
  api.test({ type: "keyup", code: "AltRight", key: "Alt" });
  assert.equal(state.sent.length, beforeBlocked);
  state.window.__flowmuxWindowsEditorSealed = false;
  assert.equal(dispatch(), true);
  const beforeInvalid = JSON.stringify(api.snapshot());
  assert.throws(() => api.configure("bad", [binding("KeyT"), binding("KeyT")]), /Conflicting/);
  assert.equal(JSON.stringify(api.snapshot()), beforeInvalid);
  api.configure("revision-5", []);
  assert.equal(dispatch(), false);
  assert.equal(localKeys, 0, "synthetic unhandled keys never reach Monaco's text handlers");
  state.window.dispatchEvent({ type: "keydown", code: "KeyQ", key: "q",
    preventDefault() { assert.fail("ordinary text must remain available to Monaco"); },
    stopImmediatePropagation() { assert.fail("ordinary text must remain available to Monaco"); } });
  assert.equal(localKeys, 1, "ordinary unmatched keys still reach local editor handlers");
  assert.equal(initialization(false).window.__flowmuxWindowsEditorShortcuts.test, undefined);
});

function harness(content = "hello", empty = false) {
  const sent = [], calls = [], composition = {}, models = {}, pending = new Set();
  const appearance = { dark: true, background: "#15171b", foreground: "#e6e9ef", cursor: "#72b7a8",
    selectionBackground: "#365c59a0", selectionForeground: "#e6e9ef", minimapEnabled: true,
    fontFamily: '"한글 한 é 😀", monospace', fontSize: 13 };
  const options = { fontFamily: appearance.fontFamily, fontSize: appearance.fontSize, minimap: { enabled: true } };
  const css = { "--ink": appearance.background, "--text": appearance.foreground };
  const root = {}, background = {};
  const nativeDocument = { hasFocus: () => false, documentElement: root,
    querySelector: (selector) => selector === ".monaco-editor-background" ? background : null };
  const input = () => ({ handlers: {}, addEventListener(name, listener) { this.handlers[name] = listener; } });
  const document = {
    payload: { id: "document-1", relativePath: "한글.txt", encoding: "UTF-8", eol: "LF", dirty: false, readOnly: false, externalChange: false, version: 1 },
    model: { getValue: () => content, getLanguageId: () => "plaintext", getFullModelRange: () => "full-model", undo() {}, redo() {} },
    pendingChanges: false, outstandingChanges: new Set(),
    changeSequence: 0, changeTimer: null,
  };
  const editor = {
    readOnly: false,
    getModel: () => document.model, getSelection: () => null,
    hasWidgetFocus: () => false, hasTextFocus: () => false,
    onDidCompositionStart(fn) { composition.start = fn; }, onDidCompositionEnd(fn) { composition.end = fn; }, onDidDispose(fn) { composition.dispose = fn; },
    getOption: (option) => options[option],
    updateOptions(value) { Object.assign(options, value); if (Object.hasOwn(value, "readOnly")) this.readOnly = value.readOnly; calls.push(["options", value.readOnly]); },
    pushUndoStop() { calls.push(["undo-stop", this.readOnly]); return !this.readOnly; },
    executeEdits(source, edits) {
      calls.push(["edit", source, this.readOnly, context.window.__flowmuxWindowsEditorSealed]);
      if (this.readOnly) return false;
      content = edits[0].text;
      return true;
    },
  };
  const context = vm.createContext({
    window: { document: nativeDocument, getComputedStyle: (element) => ({
      backgroundColor: element === background ? css["--ink"] : "",
      getPropertyValue: (property) => css[property] ?? "",
    }), flowmuxEditorHost: { receive(message) {
      const target = context.documents.get(message.documentId);
      if (target !== undefined && message.surfaceId === "s") {
        if (message.type === "document_change_applied") target.outstandingChanges.delete(message.changeSequence);
        if (message.type === "save_completed" && message.changeSequence === target.changeSequence) target.payload.dirty = false;
        if (message.type === "document_disk_status") target.payload.externalChange = message.status !== "unchanged";
      }
    } }, __flowmuxWindowsEditorBridge: (message) => sent.push(message) },
    editor, diffEditor: null, documents: new Map(empty ? [] : [[document.payload.id, document]]),
    monaco: { editor: { EditorOption: { fontFamily: "fontFamily", fontSize: "fontSize", minimap: "minimap" }, EndOfLineSequence: { LF: 0 }, onDidCreateModel(listener) { models.created = listener; } } },
    appliedAppearance: appearance, editorFontSize: appearance.fontSize, minimapEnabled: true,
    applyAppearance(value) {
      context.appliedAppearance = value;
      context.editorFontSize = value.fontSize; context.minimapEnabled = value.minimapEnabled;
      editor.updateOptions({ fontFamily: value.fontFamily, fontSize: value.fontSize, minimap: { enabled: value.minimapEnabled } });
      css["--ink"] = value.background; css["--text"] = value.foreground;
      calls.push(["appearance", value]);
    },
    activeDocumentId: empty ? null : document.payload.id, surfaceId: "s", maxDocumentBytes: 16 * 1024 * 1024,
    diffDocumentId: null, closeDialog: { open: false }, saveAsDialog: { open: false }, searchDialog: { open: false }, recoveryDialogDocumentId: null,
    pendingFlushRequests: pending, flushChangesForHost() { calls.push(["flush", ...pending]); },
    completeFlushRequests(error) { calls.push(["flush-completed", error]); },
    reportActiveViewState() { calls.push(["view-state", document.payload.version]); },
    postToHost(message) { calls.push(["posted", message.type]); },
    isHostMessage: (message) => message?.surfaceId === "s", utf8ByteLength: (value) => Buffer.byteLength(value),
    setTimeout, clearTimeout, Promise, queueMicrotask,
    syncDocument: () => true,
    clearChangeTimer(document) { clearTimeout(document.changeTimer); document.changeTimer = null; },
    requestSaveAll() {}, renderState() { calls.push(["render"]); },
    requestCloseActiveDocument() { context.closeDialog.open = true; calls.push(["close"]); },
    searchQuery: input(), searchInclude: input(), searchExclude: input(), searchStatus: { textContent: "" },
    searchMode: "workspace", activeSearchRequestId: null, renderedSearchResults: [], recentPaths: [],
    searchInputChanged() { calls.push(["search-input"]); },
    scheduleWorkspaceSearch() { calls.push(["search-schedule"]); },
    requestWorkspaceSearch() { calls.push(["search-request"]); },
    renderQuickOpen() { calls.push(["search-render-quick"]); },
    searchKeyDown(event) { calls.push(["search-key", event.key]); },
    closeSearchDialog() { context.searchDialog.open = false; calls.push(["search-close"]); },
    clearSearchTimer() { calls.push(["search-clear-timer"]); },
    cancelActiveSearch() { context.activeSearchRequestId = null; calls.push(["search-cancel"]); },
    clearSearchResultNodes() { context.renderedSearchResults = []; calls.push(["search-clear-results"]); },
    openSearchResult() { throw new Error("The legacy path-only open must not run"); },
  });
  vm.runInContext(adapter, context);
  async function command(action, extra = {}) {
    const before = sent.length;
    context.window.flowmuxWindowsEditor.command({ id: 1, action, ...extra });
    for (let i = 0; i < 30 && sent.length === before; i += 1) await new Promise((resolve) => setTimeout(resolve, 1));
    assert.ok(sent.length > before, "command replied");
    await Promise.resolve();
    return sent.at(-1);
  }
  return { context, command, sent, calls, composition, models, document };
}

function findHarness(initial) {
  const state = harness(initial);
  let content = initial, selection = new Range(1, 1, 1, 1);
  const undo = [], redo = [];
  const model = state.document.model;
  const lines = () => content.split("\n");
  Object.assign(model, {
    getValue: () => content, getEOL: () => "\n",
    getLineCount: () => lines().length,
    getLineContent: (line) => lines()[line - 1],
    getLineMaxColumn: (line) => lines()[line - 1].length + 1,
    getOffsetAt(position) { return lines().slice(0, position.lineNumber - 1).reduce((sum, line) => sum + line.length + 1, 0) + position.column - 1; },
    getPositionAt(offset) { const before = content.slice(0, offset).split("\n"); return new Position(before.length, before.at(-1).length + 1); },
    getFullModelRange() { return new Range(1, 1, this.getLineCount(), this.getLineMaxColumn(this.getLineCount())); },
    getValueInRange(range) { return content.slice(this.getOffsetAt(range.getStartPosition()), this.getOffsetAt(range.getEndPosition())); },
    findMatches(query, editable, regex, sensitive, separators, capture, limit) {
      state.calls.push(["find-matches", query, editable, regex, sensitive, separators, capture, limit]);
      return TextModelSearch.findMatches(this, new SearchParams(query, regex, sensitive, separators), this.getFullModelRange(), capture, limit);
    },
    findNextMatch(query, position, regex, sensitive, separators, capture) {
      return TextModelSearch.findNextMatch(this, new SearchParams(query, regex, sensitive, separators), position, capture);
    },
    findPreviousMatch(query, position, regex, sensitive, separators, capture) {
      return TextModelSearch.findPreviousMatch(this, new SearchParams(query, regex, sensitive, separators), position, capture);
    },
    undo() { if (undo.length) { redo.push(content); content = undo.pop(); state.document.pendingChanges = true; } },
    redo() { if (redo.length) { undo.push(content); content = redo.pop(); state.document.pendingChanges = true; } },
  });
  state.context.monaco.editor.EditorOption.wordSeparators = 1;
  const originalGetOption = state.context.editor.getOption;
  Object.assign(state.context.editor, {
    getModel: () => model, getSelection: () => selection,
    getOption: (option) => option === 1 ? "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?" : originalGetOption(option),
    setSelection(range) { selection = Range.lift(range); state.calls.push(["selection", range]); },
    focus() { throw new Error("Find must not request focus"); },
    executeEdits(source, edits) {
      state.calls.push(["literal-edits", source, edits.length, this.readOnly, state.context.window.__flowmuxWindowsEditorSealed]);
      if (this.readOnly) return false;
      undo.push(content); redo.length = 0;
      const offsets = edits.map((edit) => ({ start: model.getOffsetAt(edit.range.getStartPosition()), end: model.getOffsetAt(edit.range.getEndPosition()), text: edit.text }));
      for (const edit of offsets.sort((a, b) => b.start - a.start)) content = content.slice(0, edit.start) + edit.text + content.slice(edit.end);
      selection = new Range(1, 1, 1, 1);
      state.document.pendingChanges = true;
      return true;
    },
  });
  state.context.syncDocument = () => {
    state.calls.push(["synchronize"]);
    if (state.document.pendingChanges) {
      state.document.payload.version += 1; state.document.payload.dirty = true; state.document.pendingChanges = false;
    }
    return true;
  };
  state.release = () => state.context.window.flowmuxWindowsEditor.releaseBarrier(1);
  state.pin = () => ({ document_id: state.document.payload.id, version: state.document.payload.version });
  return state;
}

test("literal find uses Monaco ranges for Unicode, metacharacters, whole words and backward wrapping without focus", async () => {
  const state = findHarness("😀 a.b A.B aXb\n한글 한글말 한글");
  let reply = await state.command("find", { query: "a.b" });
  assert.equal(reply.error, null); assert.equal(reply.result.count, 2);
  assert.deepEqual(JSON.parse(JSON.stringify(reply.result.selected)), { index: 0, range: { start_line: 1, start_column: 4, end_line: 1, end_column: 7 } });
  assert.deepEqual(state.calls.find(([call]) => call === "find-matches").slice(2), [false, false, false, null, false, 501]);
  state.release();
  reply = await state.command("find", { query: "a.b", backward: true });
  assert.equal(reply.result.selected.index, 1);
  state.release();
  reply = await state.command("find", { query: "a.b", case_sensitive: true });
  assert.equal(reply.result.count, 1);
  state.release();
  reply = await state.command("find", { query: "한글", whole_word: true });
  assert.equal(reply.result.count, 2); assert.equal(reply.result.matches[1].start_column, 8);
  assert.equal(state.document.payload.version, 1); assert.equal(state.document.payload.dirty, false);
});

test("read observes the actual UTF-16 selection without changing model, selection or focus", async () => {
  const state = findHarness("😀 one\n한글 two");
  state.context.editor.setSelection(new Range(1, 4, 2, 3));
  const before = state.calls.length;
  const reply = await state.command("read");
  assert.equal(reply.error, null);
  assert.deepEqual(JSON.parse(JSON.stringify(reply.result.selection)), { start_line: 1, start_column: 4, end_line: 2, end_column: 3 });
  assert.equal(reply.result.content, "😀 one\n한글 two");
  assert.equal(reply.result.active_version, 1);
  assert.deepEqual(state.calls.slice(before), []);
  state.context.editor.getModel = () => null;
  assert.equal((await state.command("read")).result.selection, null);
  assert.equal((await harness("", true).command("read")).result.selection, null);
});

test("find retains 500 ranges but backward navigation reaches the actual last match; Replace All rejects truncation", async () => {
  const state = findHarness("x ".repeat(501));
  const found = await state.command("find", { query: "x", backward: true });
  assert.equal(found.result.count, 500); assert.equal(found.result.truncated, true);
  assert.equal(found.result.selected.index, null); assert.equal(found.result.selected.range.start_column, 1001);
  state.release();
  const rejected = await state.command("replace_all", { query: "x", text: "y", ...state.pin() });
  assert.match(rejected.error, /truncated/); assert.equal(state.document.model.getValue(), "x ".repeat(501));
  assert.equal(state.calls.some(([call]) => call === "literal-edits" || call === "undo-stop"), false);
});

test("literal Replace All is one undo transaction, preserves LF and returns the synchronized document version", async () => {
  const state = findHarness("한글.*\n한글.*\n");
  const reply = await state.command("replace_all", { query: "한글.*", text: "é😀\r\nnext", ...state.pin() });
  assert.equal(reply.error, null); assert.equal(reply.result.replaced, 2); assert.equal(reply.result.version, 2);
  assert.equal(reply.result.count, 0); assert.equal(reply.result.selected, null);
  assert.equal(state.document.model.getValue(), "é😀\nnext\né😀\nnext\n");
  assert.deepEqual(state.calls.filter(([call]) => call === "literal-edits"), [["literal-edits", "flowmux-windows-find", 2, false, true]]);
  assert.equal(state.calls.filter(([call]) => call === "undo-stop").length, 2);
  assert.equal(state.context.editor.readOnly, true);
  state.release(); await state.command("undo"); assert.equal(state.document.model.getValue(), "한글.*\n한글.*\n");
  state.release(); await state.command("redo"); assert.equal(state.document.model.getValue(), "é😀\nnext\né😀\nnext\n");
});

test("Replace Match replaces the selected literal match rather than skipping it and permits deletion", async () => {
  const state = findHarness("one one one");
  await state.command("find", { query: "one" }); state.release();
  const reply = await state.command("replace_match", { query: "one", text: "", ...state.pin() });
  assert.equal(reply.error, null); assert.equal(reply.result.replaced, 1);
  assert.equal(state.document.model.getValue(), " one one"); assert.equal(reply.result.count, 2);
});

test("replacement requires current document and synchronized version and never edits a changed active model", async () => {
  for (const fields of [{}, { document_id: "other", version: 1 }, { document_id: "document-1", version: 2 }, { document_id: "document-1" }]) {
    const state = findHarness("old");
    const reply = await state.command("replace_match", { query: "old", text: "new", ...fields });
    assert.ok(reply.error); assert.equal(state.document.model.getValue(), "old");
    assert.equal(state.calls.some(([call]) => call === "literal-edits"), false);
  }
  const state = findHarness("old"); state.document.pendingChanges = true;
  assert.match((await state.command("replace_all", { query: "old", text: "new", ...state.pin() })).error, /stale/);
  assert.equal(state.document.payload.version, 2); assert.equal(state.document.model.getValue(), "old");
  const changed = findHarness("old"); changed.context.editor.getModel = () => ({});
  assert.match((await changed.command("find", { query: "old" })).error, /active document changed/);
});

test("find and replacement reject regex, empty or oversized queries, malformed options and composition", async () => {
  for (const extra of [{ query: "" }, { query: "😀".repeat(1025) }, { query: "\ud800" }, { query: "old", regex: true }, { query: "old", backward: "true" }]) {
    const state = findHarness("old");
    assert.ok((await state.command("find", extra)).error);
    assert.equal(state.calls.some(([call]) => call === "selection" || call === "find-matches"), false);
  }
  const state = findHarness("old"); state.composition.start();
  assert.match((await state.command("find", { query: "old" })).error, /composition/);
  assert.equal(state.calls.length, 0);
  state.composition.end(); state.document.payload.readOnly = true;
  assert.equal((await state.command("find", { query: "old" })).error, null); state.release();
  assert.match((await state.command("replace_all", { query: "old", text: "new", ...state.pin() })).error, /read-only/);
});

test("replacement preflights full UTF-8 growth before undo or edits and zero matches do not mutate", async () => {
  const state = findHarness("x".repeat(500));
  const replacement = "😀".repeat(9000);
  assert.match((await state.command("replace_all", { query: "x", text: replacement, ...state.pin() })).error, /16 MiB/);
  assert.equal(state.document.model.getValue(), "x".repeat(500));
  assert.equal(state.calls.some(([call]) => call === "literal-edits" || call === "undo-stop"), false);
  state.release();
  const empty = await state.command("replace_all", { query: "absent", text: "", ...state.pin() });
  assert.equal(empty.error, null); assert.equal(empty.result.replaced, 0); assert.equal(empty.result.version, 1);
  assert.equal(state.calls.some(([call]) => call === "literal-edits" || call === "undo-stop"), false);
});

function searchHarness(kind = "quick") {
  const state = harness();
  state.context.searchMode = kind; state.context.searchDialog.open = true;
  state.context.activeSearchRequestId = "search-1";
  const receive = state.context.window.flowmuxEditorHost.receive;
  state.context.window.flowmuxEditorHost.receive = (message) => {
    receive(message);
    state.calls.push(["search-completion", message.type]);
    state.context.activeSearchRequestId = null;
    state.context.renderedSearchResults = kind === "quick" ? message.paths.map((path) => ({ path, line: 0, column: 0, length: 0 })) : message.result.matches;
    if (kind === "quick") state.context.renderQuickOpen();
  };
  state.complete = (entries, metadata = {}, messageFields = {}) => state.context.window.flowmuxWindowsEditor.completeSearch({
    surfaceId: "s", requestId: "search-1", type: kind === "quick" ? "quick_open_completed" : "workspace_search_completed",
    ...(kind === "quick" ? { paths: entries, truncated: false } : { result: { matches: entries, truncated: false, cancelled: false }, error: null }),
    ...messageFields,
  }, { request_id: "search-1", token: "native-token", kind, error: null, diagnostics: {}, ...metadata });
  return state;
}

test("Quick Open maps a reordered visible row to the exact retained index and waits for native completion", () => {
  const state = searchHarness();
  assert.equal(state.complete(["first.txt", "한글.txt"]), true);
  state.context.renderedSearchResults.reverse();
  state.context.openSearchResult(0); state.context.openSearchResult(1);
  assert.deepEqual(JSON.parse(JSON.stringify(state.sent)), [{ kind: "search_open", token: "native-token", index: 1 }]);
  assert.equal(state.context.searchDialog.open, true); assert.equal(state.context.recentPaths.length, 0);
  state.context.window.flowmuxWindowsEditor.searchOpenFinished("stale-token", null);
  assert.equal(state.context.searchDialog.open, true);
  state.context.window.flowmuxWindowsEditor.barrier(90, true);
  state.context.window.flowmuxWindowsEditor.searchOpenFinished("native-token", null);
  assert.equal(state.context.searchDialog.open, false); assert.deepEqual(state.context.recentPaths, ["한글.txt"]);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
});

test("search completion rejects stale or malformed identities and renders bounded Quick Open errors safely", () => {
  const stale = searchHarness();
  assert.equal(stale.complete(["old.txt"], { request_id: "search-old" }), false);
  assert.equal(stale.calls.length, 0);
  assert.equal(stale.complete(["old.txt"], {}, { surfaceId: "foreign" }), false);
  assert.match(stale.context.searchStatus.textContent, /Invalid/);
  const invalid = searchHarness();
  assert.equal(invalid.complete(Array(2001).fill("file.txt")), false);
  assert.match(invalid.context.searchStatus.textContent, /Too many/);
  const failed = searchHarness();
  assert.equal(failed.complete([], { token: null, error: "<script>한글😀</script>".repeat(400) }), true);
  assert.ok(Buffer.byteLength(failed.context.searchStatus.textContent) <= 4096);
  assert.ok(failed.context.searchStatus.textContent.startsWith("<script>"));
  assert.equal(failed.context.renderedSearchResults.length, 0);
  failed.context.openSearchResult(0); assert.equal(failed.sent.length, 0);
});

test("workspace open keeps zero-based UTF-16 selection identity and failure retains results without unsealing", () => {
  const state = searchHarness("workspace");
  const entry = { path: "한글.txt", line: 2, column: 3, length: 2, preview: "😀 한글", previewColumn: 3, previewLength: 2 };
  assert.equal(state.complete([entry]), true);
  state.context.renderedSearchResults = [{ ...entry, column: 4 }];
  state.context.openSearchResult(0); assert.equal(state.sent.length, 0);
  state.context.renderedSearchResults = [entry]; state.context.openSearchResult(0);
  assert.equal(state.sent[0].index, 0);
  state.context.window.flowmuxWindowsEditor.barrier(91, true);
  state.context.window.flowmuxWindowsEditor.searchOpenFinished("native-token", "The file changed; search again.");
  assert.equal(state.context.searchDialog.open, true); assert.equal(state.context.renderedSearchResults.length, 1);
  assert.match(state.context.searchStatus.textContent, /file changed/);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
});

test("a late open acknowledgment cannot close results belonging to a newer search token", () => {
  const state = searchHarness(); state.complete(["old.txt"]); state.context.openSearchResult(0);
  state.context.activeSearchRequestId = "search-2";
  state.complete(["new.txt"], { request_id: "search-2", token: "new-token" }, { requestId: "search-2" });
  state.context.window.flowmuxWindowsEditor.searchOpenFinished("native-token", null);
  assert.equal(state.context.searchDialog.open, true);
  assert.equal(state.context.renderedSearchResults[0].path, "new.txt");
  state.context.openSearchResult(0); assert.equal(state.sent.at(-1).token, "new-token");
});

test("query/include/exclude composition defers workspace work and Enter without cancelling native composition", () => {
  for (const name of ["searchQuery", "searchInclude", "searchExclude"]) {
    const state = searchHarness("workspace"), input = state.context[name];
    input.handlers.compositionstart();
    state.context.searchInputChanged(); state.context.scheduleWorkspaceSearch(); state.context.requestWorkspaceSearch();
    let prevented = false;
    state.context.searchKeyDown({ key: "Enter", preventDefault() { prevented = true; } });
    assert.equal(prevented, false);
    assert.equal(state.calls.some(([call]) => ["search-input", "search-schedule", "search-request", "search-key"].includes(call)), false);
    assert.equal(state.context.activeSearchRequestId, null);
    input.handlers.compositionend();
    assert.equal(state.calls.at(-1)[0], "search-input");
  }
});

test("Quick Open composition retains its query-independent index but defers ranking and opening", () => {
  const state = searchHarness(); state.context.searchQuery.handlers.compositionstart();
  assert.equal(state.context.activeSearchRequestId, "search-1");
  assert.equal(state.complete(["한글.txt"]), true);
  assert.equal(state.calls.some(([call]) => call === "search-render-quick"), false);
  state.context.openSearchResult(0); assert.equal(state.sent.length, 0);
  state.context.searchQuery.handlers.compositionend();
  state.context.openSearchResult(0); assert.equal(state.sent[0].kind, "search_open");
});

test("legacy path-only search opens cannot bypass the Windows token bridge", () => {
  const state = harness();
  state.context.postToHost({ type: "search_result_open_requested", path: "unexpected.txt", line: 0, column: 0, length: 0 });
  assert.equal(state.calls.some(([kind]) => kind === "posted"), false);
  assert.match(state.context.searchStatus.textContent, /expired/);
});

test("new Windows document and diff models use LF internally without changing disk metadata", () => {
  const state = harness();
  state.document.payload.eol = "CRLF";
  for (const initialText of ["", "one line", "one\ntwo\n"]) {
    let eol = 1;
    const model = { setEOL(value) { eol = value; }, initialText };
    state.models.created(model);
    assert.equal(eol, 0);
    assert.equal(model.initialText, initialText);
  }
  assert.equal(state.document.payload.eol, "CRLF");
  assert.equal(state.document.payload.dirty, false);
});

test("theme keeps only newest colors through composition and preserves Unicode, model, fonts and local minimap", async () => {
  const text = "한글 한 é 😀\nunchanged";
  const state = harness(text), api = state.context.window.flowmuxWindowsEditor;
  const model = state.document.model, family = state.context.appliedAppearance.fontFamily;
  state.context.editor.focus = () => { throw new Error("Theme must not focus the editor"); };
  state.context.editor.setModel = () => { throw new Error("Theme must not replace the model"); };
  state.context.minimapEnabled = false;
  state.context.editor.updateOptions({ fontFamily: family, fontSize: 19, minimap: { enabled: false } });
  const colors = { dark: false, background: "#fdf6e3", foreground: "#657b83", cursor: "#657b83",
    selectionBackground: "#eee8d5", selectionForeground: "#586e75" };
  state.composition.start();
  api.setTheme({ ...colors, background: "#123456" });
  api.setTheme(colors);
  let read = (await state.command("read")).result;
  assert.equal(read.appearance.pending, true);
  assert.equal(read.appearance.applied.background, "#15171b");
  assert.equal(state.calls.filter(([kind]) => kind === "appearance").length, 0);
  state.composition.end();
  assert.equal(state.calls.filter(([kind]) => kind === "appearance").length, 0, "composition end does not apply synchronously");
  state.composition.start();
  await Promise.resolve();
  assert.equal(state.calls.filter(([kind]) => kind === "appearance").length, 0, "new composition also blocks a queued flush");
  state.composition.end();
  await Promise.resolve();
  read = (await state.command("read")).result;
  assert.equal(read.appearance.pending, false);
  assert.equal(read.appearance.applied.background, colors.background);
  assert.equal(read.appearance.css_background, colors.background);
  assert.equal(read.appearance.css_foreground, colors.foreground);
  assert.equal(read.appearance.background, colors.background);
  assert.equal(read.appearance.font_family, family);
  assert.equal(read.appearance.font_size, 19);
  assert.equal(read.appearance.applied.minimapEnabled, false);
  assert.equal(read.content, text);
  assert.equal(read.dirty, false);
  assert.equal(read.active_version, 1);
  assert.equal(state.context.editor.getModel(), model);
  assert.equal(state.calls.filter(([kind]) => kind === "appearance").length, 1);

  const diffComposition = {}, modified = {
    onDidCompositionStart(fn) { diffComposition.start = fn; },
    onDidCompositionEnd(fn) { diffComposition.end = fn; },
    onDidDispose(fn) { diffComposition.dispose = fn; },
    getOption: state.context.editor.getOption,
  };
  state.context.diffEditor = { getModifiedEditor: () => modified };
  state.context.window.flowmuxEditorHost.receive({ surfaceId: "s", type: "noop" });
  diffComposition.start();
  api.setTheme({ ...colors, cursor: "#112233" });
  assert.equal((await state.command("read")).result.appearance.pending, true);
  diffComposition.dispose(); state.context.diffEditor = null;
  assert.equal(state.calls.filter(([kind]) => kind === "appearance").length, 1);
  await Promise.resolve();
  read = (await state.command("read")).result;
  assert.equal(read.appearance.pending, false);
  assert.equal(read.appearance.applied.cursor, "#112233");
  assert.equal(read.content, text);
  assert.equal(state.context.editor.getModel(), model);
  assert.equal(state.calls.some(([kind]) => kind === "edit" || kind === "undo-stop"), false);
});

test("bounded read preserves Unicode scalar boundaries and empty document state", async () => {
  const state = harness("가".repeat(50000) + "😀");
  const reply = await state.command("read");
  assert.equal(reply.error, null);
  assert.equal(reply.result.document_focused, false);
  assert.equal(reply.result.content_truncated, true);
  assert.ok(Buffer.byteLength(reply.result.content) <= 128 * 1024);
  assert.equal(reply.result.content, "가".repeat(Math.floor(128 * 1024 / 3)));
  const empty = await harness("", true).command("read");
  assert.equal(empty.result.content, null);
  assert.equal(empty.result.document_id, null);
});

test("barrier refuses composition without cancelling it and seals until the matching release", async () => {
  const state = harness();
  const api = state.context.window.flowmuxWindowsEditor;
  state.composition.start(); api.barrier(9, true);
  assert.equal(state.sent.at(-1).kind, "barrier_error");
  assert.equal(state.calls.length, 0);
  state.composition.end(); api.barrier(10, true);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  assert.equal(state.calls.at(-1)[0], "flush");
  assert.match((await state.command("undo")).error, /close decision/);
  api.releaseBarrier(9);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  api.releaseBarrier(10);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
  assert.deepEqual(state.calls.at(-1), ["options", false]);
  api.barrier(11, true); api.releaseBarrier(0);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
});

test("dirty close preserves the document and requires confirmation; malformed fields cannot mutate", async () => {
  const state = harness(); state.document.payload.dirty = true;
  const reply = await state.command("close_document");
  assert.equal(reply.result.closed, false);
  assert.equal(reply.result.confirmation_required, true);
  assert.equal(state.context.documents.size, 1);
  assert.match((await state.command("read", { text: "\ud800" })).error, /Invalid or oversized/);
  assert.match((await state.command("read", { unexpected: true })).error, /Unknown editor command field/);
});

test("barrier reports current view state after content acknowledgment and before completion", () => {
  const state = harness();
  state.document.outstandingChanges.add(1);
  state.context.flushChangesForHost = () => {
    if (state.document.outstandingChanges.size === 0) state.context.completeFlushRequests(null);
    else state.calls.push(["waiting-for-content"]);
  };
  state.context.window.flowmuxWindowsEditor.barrier(12, false);
  assert.deepEqual(state.calls, [["waiting-for-content"]]);
  state.document.payload.version = 2;
  state.document.outstandingChanges.clear();
  state.context.flushChangesForHost();
  assert.deepEqual(state.calls.slice(-2), [["view-state", 2], ["flush-completed", null]]);
  state.calls.length = 0;
  state.context.completeFlushRequests("content rejected");
  assert.deepEqual(state.calls, [["flush-completed", "content rejected"]]);
});

test("replace uses a synchronous writable window while input and subsequent waits stay sealed", async () => {
  const state = harness();
  const reply = await state.command("replace_text", { text: "changed 한글\n" });
  assert.equal(reply.error, null);
  assert.equal(reply.result.content, "changed 한글\n");
  assert.equal(reply.result.sealed, true);
  assert.deepEqual(state.calls, [
    ["options", true], ["options", false], ["undo-stop", false],
    ["edit", "flowmux-windows", false, true], ["undo-stop", false], ["options", true],
  ]);
  state.context.window.flowmuxWindowsEditor.releaseBarrier(2);
  assert.equal(state.context.editor.readOnly, true);
  state.context.window.flowmuxWindowsEditor.releaseBarrier(1);
  assert.equal(state.context.editor.readOnly, false);
  assert.equal((await state.command("read")).result.sealed, false);
});

test("a timed-out mutation retains its seal until the native owner releases the matching command", async () => {
  const state = harness();
  let timeout;
  state.context.setTimeout = (callback) => { timeout = callback; return 1; };
  state.context.clearTimeout = () => {};
  state.document.outstandingChanges.add(1);
  const response = state.command("undo");
  await new Promise((resolve) => setTimeout(resolve, 1));
  assert.equal(typeof timeout, "function");
  timeout();
  assert.match((await response).error, /timed out/);
  assert.equal(state.context.editor.readOnly, true);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  state.context.window.flowmuxWindowsEditor.releaseBarrier(1);
  assert.equal(state.context.editor.readOnly, false);
});

test("normal UI replacement seals before dispatch and cannot be unlocked by a barrier release", () => {
  const state = harness();
  const api = state.context.window.flowmuxWindowsEditor;
  state.context.postToHost({ type: "conflict_action_requested", action: "reload_from_disk" });
  assert.deepEqual(state.calls.slice(0, 2), [["options", true], ["posted", "conflict_action_requested"]]);
  api.releaseBarrier(0);
  assert.equal(state.context.editor.readOnly, true);
  api.barrier(10, false);
  assert.equal(state.sent.at(-1).kind, "barrier_error");
  assert.match(state.sent.at(-1).error, /replacement/);
  api.replacementCompleted();
  assert.equal(state.context.editor.readOnly, false);
  state.context.postToHost({ type: "recovery_decision", choice: "restore" });
  assert.equal(state.context.editor.readOnly, true);
  api.quarantine();
  api.replacementCompleted(); api.releaseBarrier(0);
  assert.equal(state.context.editor.readOnly, true);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
});

test("command completion waits for native replacement completion before exposing its result", async () => {
  const state = harness();
  state.document.payload.externalChange = true;
  state.context.requestConflictAction = (action) => state.context.postToHost({ type: "conflict_action_requested", action });
  const result = state.command("reload");
  await new Promise((resolve) => setTimeout(resolve, 1));
  state.context.window.flowmuxEditorHost.receive({ surfaceId: "s", type: "replace_document", document: { id: "document-1" } });
  await new Promise((resolve) => setTimeout(resolve, 1));
  assert.equal(state.sent.length, 0);
  state.context.window.flowmuxWindowsEditor.replacementCompleted();
  assert.equal((await result).error, null);
  assert.equal(state.context.editor.readOnly, true);
  state.context.window.flowmuxWindowsEditor.releaseBarrier(1);
  assert.equal(state.context.editor.readOnly, false);
});

test("overlapping Save All and close requests retain the input shield until every completion", () => {
  const state = harness();
  const api = state.context.window.flowmuxWindowsEditor;
  for (const type of ["save_requested", "save_requested", "save_as_requested", "close_requested", "discard_close_requested"]) {
    state.context.postToHost({ type });
  }
  assert.equal(state.calls.filter(([kind]) => kind === "posted").length, 5);
  for (let remaining = 4; remaining > 0; remaining -= 1) {
    api.replacementCompleted();
    assert.equal(state.context.editor.readOnly, true);
    api.releaseBarrier(0);
    assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  }
  api.replacementCompleted();
  assert.equal(state.context.editor.readOnly, false);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
  api.replacementCompleted();
  state.context.postToHost({ type: "save_requested" });
  assert.equal(state.context.editor.readOnly, true);
  api.quarantine();
  api.replacementCompleted();
  assert.equal(state.context.editor.readOnly, true);
});

test("a deferred command result waits for all overlapping response completions", async () => {
  const state = harness();
  state.document.payload.externalChange = true;
  state.context.requestConflictAction = (action) => {
    state.context.postToHost({ type: "conflict_action_requested", action });
    state.context.postToHost({ type: "save_requested" });
  };
  const result = state.command("reload");
  await new Promise((resolve) => setTimeout(resolve, 1));
  state.context.window.flowmuxEditorHost.receive({ surfaceId: "s", type: "replace_document", document: { id: "document-1" } });
  await new Promise((resolve) => setTimeout(resolve, 1));
  const api = state.context.window.flowmuxWindowsEditor;
  api.replacementCompleted();
  assert.equal(state.sent.length, 0);
  assert.equal(state.context.editor.readOnly, true);
  api.replacementCompleted();
  assert.equal((await result).error, null);
  assert.equal(state.context.editor.readOnly, true);
  api.releaseBarrier(1);
  assert.equal(state.context.editor.readOnly, false);
});

async function settle() {
  for (let count = 0; count < 12; count += 1) await Promise.resolve();
}

function saveAllHarness(count = 10) {
  const state = harness();
  state.context.documents.clear();
  const queued = [];
  for (let index = 0; index < count; index += 1) {
    const document = {
      ...state.document,
      payload: { ...state.document.payload, id: `document-${index + 1}`, name: `문서 ${index + 1}.txt`, dirty: true },
      pendingChanges: true,
      outstandingChanges: new Set(),
      changeTimer: setTimeout(() => assert.fail("queued debounce must be cancelled"), 1000),
    };
    state.context.documents.set(document.payload.id, document);
    queued.push(document);
  }
  state.context.syncDocument = (document) => {
    if (document.pendingChanges) {
      state.calls.push(["sync", document.payload.id]);
      document.pendingChanges = false;
      document.changeSequence += 1;
      document.outstandingChanges.add(document.changeSequence);
    }
    return true;
  };
  state.context.requestSave = (id) => {
    state.calls.push(["save", id]);
    state.context.postToHost({ type: "save_requested", documentId: id });
  };
  const receive = (type, document, extra = {}) => state.context.window.flowmuxEditorHost.receive({
    surfaceId: "s", type, documentId: document.payload.id, changeSequence: document.changeSequence, ...extra,
  });
  return { ...state, queued, receive };
}

for (const source of ["ui", "cli"]) {
  test(`${source} Save All serializes ten dirty documents through change, save and native completion`, async () => {
    const state = saveAllHarness();
    const api = state.context.window.flowmuxWindowsEditor;
    if (source === "ui") state.context.requestSaveAll();
    else api.command({ id: 42, action: "save_all" });
    await settle();
    assert.ok(state.queued.every((document) => document.changeTimer === null));
    assert.ok(state.queued.slice(1).every((document) => document.pendingChanges));
    assert.equal(state.context.editor.readOnly, true);
    assert.match((await state.command("read")).error, /pending/);
    for (let index = 0; index < state.queued.length; index += 1) {
      const document = state.queued[index];
      assert.equal(state.calls.filter(([kind]) => kind === "sync").length, index + 1);
      assert.equal(state.calls.filter(([kind]) => kind === "save").length, index);
      state.receive("document_change_applied", document);
      await settle();
      assert.equal(state.calls.filter(([kind]) => kind === "save").length, index + 1);
      state.receive("save_completed", document, { changeSequence: document.changeSequence - 1 });
      await settle();
      assert.equal(document.payload.dirty, true);
      state.receive("save_completed", document);
      await settle();
      assert.equal(state.calls.filter(([kind]) => kind === "sync").length, index + 1);
      assert.equal(state.context.editor.readOnly, true);
      api.replacementCompleted();
      await settle();
    }
    assert.ok(state.queued.every((document) => !document.payload.dirty));
    if (source === "cli") {
      const reply = state.sent.find((message) => message.id === 42);
      assert.equal(reply.error, null);
      assert.equal(reply.result.saved, 10);
      assert.equal(state.context.editor.readOnly, true);
      api.releaseBarrier(42);
    }
    assert.equal(state.context.editor.readOnly, false);
    assert.equal((await state.command("read")).error, null);
  });
}

test("UI Save All stops on save failure, preserves later edits, and holds its seal until completion", async () => {
  const state = saveAllHarness(3);
  const api = state.context.window.flowmuxWindowsEditor;
  state.context.requestSaveAll();
  state.receive("document_change_applied", state.queued[0]);
  await settle();
  state.receive("save_failed", state.queued[0], { reason: "disk is read-only" });
  await settle();
  assert.match(state.queued[0].saveError, /Save All stopped:.*disk is read-only/);
  assert.equal(state.calls.filter(([kind]) => kind === "save").length, 1);
  assert.ok(state.queued.slice(1).every((document) => document.pendingChanges && document.payload.dirty));
  api.releaseBarrier(0);
  assert.equal(state.context.editor.readOnly, true);
  api.replacementCompleted();
  await settle();
  assert.equal(state.context.editor.readOnly, false);
  assert.equal(state.calls.filter(([kind]) => kind === "sync").length, 1);
});

test("UI Save All timeout preserves the seal until its late content acknowledgment without saving", async () => {
  const state = saveAllHarness(2);
  let timeout;
  state.context.setTimeout = (callback) => { timeout = callback; return 1; };
  state.context.clearTimeout = () => {};
  state.context.requestSaveAll();
  timeout();
  await settle();
  assert.match(state.queued[0].saveError, /timed out/);
  assert.equal(state.context.editor.readOnly, true);
  state.receive("document_change_applied", state.queued[0]);
  await settle();
  assert.equal(state.context.editor.readOnly, false);
  assert.equal(state.calls.filter(([kind]) => kind === "save").length, 0);
  assert.ok(state.queued.every((document) => document.payload.dirty));
  assert.equal(state.queued[1].pendingChanges, true);
});

test("UI Save All refuses active composition without changing read-only state", () => {
  const state = harness();
  state.composition.start();
  state.context.requestSaveAll();
  assert.match(state.document.saveError, /Finish text composition/);
  assert.equal(state.context.editor.readOnly, false);
  assert.equal(state.calls.filter(([kind]) => kind === "options").length, 0);
});


// These tests exercise adapter state ordering with spies, not real Monaco/IME.
function refreshHarness() {
  const state = harness("active 한글 한 é 😀");
  const second = {
    payload: { ...state.document.payload, id: "document-2", relativePath: "inactive 한.txt" },
    model: { getValue: () => "old inactive", getLanguageId: () => "plaintext" },
    pendingChanges: false, outstandingChanges: new Set(), changeSequence: 0,
  };
  state.context.documents.set(second.payload.id, second);
  state.context.addOrReplaceDocument = (payload) => {
    state.calls.push(["replace-existing", payload.id]);
    const target = state.context.documents.get(payload.id);
    assert.ok(target, "refresh preserves an existing model");
    target.payload = { ...payload };
    target.model.getValue = () => payload.content;
  };
  state.context.activateDocument = () => state.calls.push(["activate"]);
  state.context.editor.focus = () => state.calls.push(["focus"]);
  const replacement = () => ({ surfaceId: "s", type: "replace_document",
    document: { ...second.payload, version: second.payload.version + 1,
      content: "new inactive 한글 한 é 😀", dirty: false } });
  return { ...state, second, replacement, api: state.context.window.flowmuxWindowsEditor };
}
function flushedRefresh(state, id) {
  state.api.beginDiskRefresh(id);
  state.context.completeFlushRequests(null);
}

test("automatic refresh updates inactive existing model without activation or focus and waits for native release", () => {
  const state = refreshHarness(); const originalModel = state.second.model;
  flushedRefresh(state, 71);
  state.api.applyDiskRefreshMessage(71, state.replacement());
  assert.equal(state.context.activeDocumentId, "document-1");
  assert.equal(state.second.model, originalModel);
  assert.equal(state.second.model.getValue(), "new inactive 한글 한 é 😀");
  assert.equal(state.calls.some(([call]) => ["activate", "focus"].includes(call)), false);
  state.api.completeDiskRefresh(71);
  assert.equal(state.sent.at(-1).kind, "refresh_applied");
  state.api.releaseBarrier(0);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  state.api.releaseDiskRefresh(70);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  state.api.releaseDiskRefresh(71);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
});

test("automatic refresh defers composition without ending it or changing editability", () => {
  const state = refreshHarness(); state.composition.start();
  state.api.beginDiskRefresh(72);
  assert.equal(state.sent.at(-1).kind, "refresh_deferred");
  assert.equal(state.calls.length, 0);
  state.api.beginDiskRefresh(73);
  assert.equal(state.sent.at(-1).kind, "refresh_deferred");
  state.composition.end(); flushedRefresh(state, 74);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  state.api.abortDiskRefreshBeforeWork(74);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
});

test("automatic refresh leaves the shared Quick Open and workspace search overlay editable", () => {
  const state = refreshHarness();
  state.context.searchDialog.open = true;
  const callsBefore = state.calls.length;
  state.api.beginDiskRefresh(82);
  assert.equal(state.sent.at(-1).kind, "refresh_deferred");
  assert.equal(state.sent.at(-1).id, 82);
  assert.equal(state.context.searchDialog.open, true);
  assert.equal(state.context.editor.readOnly, false);
  assert.notEqual(state.context.window.__flowmuxWindowsEditorSealed, true);
  assert.equal(state.calls.length, callsBefore, "no flush, focus, or model operation while the overlay is open");
  assert.equal(state.context.pendingFlushRequests.size, 0);
  state.context.searchDialog.open = false;
  flushedRefresh(state, 83);
  state.api.completeDiskRefresh(83);
  assert.equal(state.sent.at(-1).kind, "refresh_applied");
  state.api.releaseDiskRefresh(83);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
});

test("automatic refresh defers focused Monaco widgets but permits ordinary noncomposing text focus", () => {
  const state = refreshHarness();
  state.context.editor.hasWidgetFocus = () => true;
  state.context.editor.hasTextFocus = () => false;
  const callsBefore = state.calls.length;
  state.api.beginDiskRefresh(84);
  assert.equal(state.sent.at(-1).kind, "refresh_deferred");
  assert.equal(state.sent.at(-1).id, 84);
  assert.equal(state.context.editor.readOnly, false);
  assert.notEqual(state.context.window.__flowmuxWindowsEditorSealed, true);
  assert.equal(state.calls.length, callsBefore);
  assert.equal(state.context.pendingFlushRequests.size, 0);
  // Focus state is a mocked preflight condition, not physical IME verification.
  state.context.editor.hasTextFocus = () => true;
  flushedRefresh(state, 85);
  state.api.completeDiskRefresh(85);
  assert.equal(state.sent.at(-1).kind, "refresh_applied");
  state.api.releaseDiskRefresh(85);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
});

test("automatic refresh defers pending edits instead of consuming debounce state", () => {
  const state = refreshHarness(); state.second.pendingChanges = true;
  state.api.beginDiskRefresh(75);
  assert.equal(state.sent.at(-1).kind, "refresh_deferred");
  assert.equal(state.second.pendingChanges, true);
  assert.equal(state.calls.length, 0);
});

test("automatic refresh never replaces dirty content and quarantines a contradictory backend replacement", () => {
  const state = refreshHarness(); state.second.payload.dirty = true;
  flushedRefresh(state, 76);
  state.api.applyDiskRefreshMessage(76, state.replacement());
  assert.equal(state.sent.at(-1).kind, "refresh_error");
  assert.equal(state.second.model.getValue(), "old inactive");
  state.api.completeDiskRefresh(76); state.api.releaseDiskRefresh(76);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
});

test("a stale automatic refresh cannot replace a model or release a later guard", () => {
  const state = refreshHarness(); flushedRefresh(state, 77);
  state.api.applyDiskRefreshMessage(76, state.replacement());
  state.api.completeDiskRefresh(76); state.api.releaseDiskRefresh(76);
  assert.equal(state.second.model.getValue(), "old inactive");
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  state.api.abortDiskRefreshBeforeWork(77);
});

test("composition starting after the refresh guard preserves content and requires native failure handling", () => {
  const state = refreshHarness(); flushedRefresh(state, 78);
  state.composition.start(); state.api.applyDiskRefreshMessage(78, state.replacement());
  assert.equal(state.sent.at(-1).kind, "refresh_error");
  assert.equal(state.second.model.getValue(), "old inactive");
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
});

test("automatic disk status preserves dirty content and active document identity", () => {
  const state = refreshHarness(); state.second.payload.dirty = true;
  flushedRefresh(state, 79);
  state.api.applyDiskRefreshMessage(79, { surfaceId: "s", type: "document_disk_status",
    documentId: state.second.payload.id, documentVersion: state.second.payload.version, status: "deleted" });
  assert.equal(state.second.payload.dirty, true);
  assert.equal(state.second.payload.externalChange, true);
  assert.equal(state.second.model.getValue(), "old inactive");
  assert.equal(state.context.activeDocumentId, "document-1");
  assert.equal(state.calls.some(([call]) => ["replace-existing", "activate", "focus"].includes(call)), false);
  state.api.completeDiskRefresh(79); state.api.releaseDiskRefresh(79);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
});

test("automatic refresh rejects unknown documents and nonadvancing replacement versions", () => {
  for (const mutation of [
    (message) => { message.document.id = "not-open"; },
    (message) => { message.document.version = 1; },
    (message) => { message.surfaceId = "other-surface"; },
  ]) {
    const state = refreshHarness(); flushedRefresh(state, 80);
    const message = state.replacement(); mutation(message);
    state.api.applyDiskRefreshMessage(80, message);
    assert.equal(state.sent.at(-1).kind, "refresh_error");
    assert.equal(state.second.model.getValue(), "old inactive");
    assert.equal(state.calls.some(([call]) => call === "replace-existing"), false);
    assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  }
});

test("an empty automatic result still waits for completed content flush before acknowledgment", () => {
  const state = refreshHarness(); state.api.beginDiskRefresh(81);
  state.api.completeDiskRefresh(81); state.api.releaseDiskRefresh(81);
  assert.equal(state.sent.some((message) => message.kind === "refresh_applied"), false);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, true);
  state.context.completeFlushRequests(null);
  state.api.completeDiskRefresh(81);
  assert.equal(state.sent.at(-1).kind, "refresh_applied");
  state.api.releaseDiskRefresh(81);
  assert.equal(state.context.window.__flowmuxWindowsEditorSealed, false);
});
