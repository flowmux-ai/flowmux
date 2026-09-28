// SPDX-License-Identifier: GPL-3.0-or-later
// Static bridge/adapter tests only. Native verification exercises real Monaco.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const initializer = await readFile(new URL("./initialize.js", import.meta.url), "utf8");
const adapter = await readFile(new URL("./adapter.js", import.meta.url), "utf8");

function initialization(background, actualUrl = "http://127.0.0.1:1234/token/index.html?surface=s") {
  const sent = [];
  const handlers = [];
  class Element { focus() { this.focused = true; } }
  class Dialog extends Element {
    show() { this.open = true; this.nonmodal = true; }
    showModal() { this.open = true; this.modal = true; }
  }
  const window = { location: { href: actualUrl }, ipc: { postMessage: (raw) => sent.push(JSON.parse(raw)) }, addEventListener: (name, handler) => handlers.push([name, handler]) };
  window.top = window;
  const context = vm.createContext({ window, document: { addEventListener() {} }, HTMLElement: Element, HTMLDialogElement: Dialog });
  const init = vm.runInContext(`(${initializer})`, context);
  init({ url: "http://127.0.0.1:1234/token/index.html?surface=s", signal_id: "s", credential: "private", background });
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
  assert.deepEqual(state.sent[0], { kind: "editor_message", message: { type: "editor_ready" }, signal_id: "s", credential: "private" });
  const foreign = initialization(true, "http://127.0.0.1:1234/other/index.html?surface=s");
  assert.equal(foreign.window.webkit, undefined);
  const normal = initialization(false); const normalElement = new normal.Element(); normalElement.focus();
  assert.equal(normalElement.focused, true);
  state.window.__flowmuxWindowsEditorSealed = true;
  let prevented = false, stopped = false;
  state.handlers.find(([name]) => name === "beforeinput")[1]({ preventDefault() { prevented = true; }, stopImmediatePropagation() { stopped = true; } });
  assert.equal(prevented && stopped, true);
});

function harness(content = "hello", empty = false) {
  const sent = [], calls = [], composition = {}, models = {}, pending = new Set();
  const document = {
    payload: { id: "document-1", relativePath: "한글.txt", encoding: "UTF-8", eol: "LF", dirty: false, readOnly: false, externalChange: false, version: 1 },
    model: { getValue: () => content, getLanguageId: () => "plaintext", getFullModelRange: () => "full-model", undo() {}, redo() {} },
    pendingChanges: false, outstandingChanges: new Set(),
    changeSequence: 0, changeTimer: null,
  };
  const editor = {
    readOnly: false,
    onDidCompositionStart(fn) { composition.start = fn; }, onDidCompositionEnd(fn) { composition.end = fn; }, onDidDispose() {},
    updateOptions(value) { this.readOnly = value.readOnly; calls.push(["options", value.readOnly]); },
    pushUndoStop() { calls.push(["undo-stop", this.readOnly]); return !this.readOnly; },
    executeEdits(source, edits) {
      calls.push(["edit", source, this.readOnly, context.window.__flowmuxWindowsEditorSealed]);
      if (this.readOnly) return false;
      content = edits[0].text;
      return true;
    },
  };
  const context = vm.createContext({
    window: { document: { hasFocus: () => false }, flowmuxEditorHost: { receive(message) {
      const target = context.documents.get(message.documentId);
      if (target !== undefined && message.surfaceId === "s") {
        if (message.type === "document_change_applied") target.outstandingChanges.delete(message.changeSequence);
        if (message.type === "save_completed" && message.changeSequence === target.changeSequence) target.payload.dirty = false;
      }
    } }, __flowmuxWindowsEditorBridge: (message) => sent.push(message) },
    editor, diffEditor: null, documents: new Map(empty ? [] : [[document.payload.id, document]]),
    monaco: { editor: { EndOfLineSequence: { LF: 0 }, onDidCreateModel(listener) { models.created = listener; } } },
    activeDocumentId: empty ? null : document.payload.id, surfaceId: "s", maxDocumentBytes: 16 * 1024 * 1024,
    diffDocumentId: null, closeDialog: { open: false }, recoveryDialogDocumentId: null,
    pendingFlushRequests: pending, flushChangesForHost() { calls.push(["flush", ...pending]); },
    completeFlushRequests(error) { calls.push(["flush-completed", error]); },
    reportActiveViewState() { calls.push(["view-state", document.payload.version]); },
    postToHost(message) { calls.push(["posted", message.type]); },
    isHostMessage: (message) => message?.surfaceId === "s", utf8ByteLength: (value) => Buffer.byteLength(value),
    setTimeout, clearTimeout, Promise,
    syncDocument: () => true,
    clearChangeTimer(document) { clearTimeout(document.changeTimer); document.changeTimer = null; },
    requestSaveAll() {}, renderState() { calls.push(["render"]); },
    requestCloseActiveDocument() { context.closeDialog.open = true; calls.push(["close"]); },
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
