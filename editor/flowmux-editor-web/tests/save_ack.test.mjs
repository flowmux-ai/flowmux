// SPDX-License-Identifier: GPL-3.0-or-later

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { advanceDocumentEdit } from "../.test-build/protocol.js";

// Run the production handlers without booting the DOM or Monaco in Node.
const source = ts.createSourceFile(
  "main.ts",
  readFileSync(new URL("../src/main.ts", import.meta.url), "utf8"),
  ts.ScriptTarget.Latest,
  true,
);
const names = new Set([
  "handleHostMessage", "addOrReplaceDocument", "clearChangeTimer", "syncDocument",
  "flushChangesForHost", "completeFlushRequests", "requestSave", "submitSaveAs",
  "requestClose", "requestCloseActiveDocument",
]);
const functions = source.statements.filter(
  (node) => ts.isFunctionDeclaration(node) && names.has(node.name?.text),
);
assert.equal(functions.length, names.size);
const script = ts.transpileModule(functions.map((node) => node.getText(source)).join("\n"), {
  compilerOptions: { target: ts.ScriptTarget.ES2022 },
}).outputText;

function editorFixture(saveAs = false) {
  const sent = [];
  const timers = new Set();
  let text = "original";
  let onChange;
  const model = {
    getValue: () => text,
    onDidChangeContent: (callback) => { onChange = callback; },
    getFullModelRange: () => ({}),
    pushEditOperations: (_selection, edits) => { text = edits[0].text; onChange(); },
    pushStackElement() {},
  };
  const context = vm.createContext({
    documents: new Map(), surfaceId: "surface", activeDocumentId: "doc",
    diffDocumentId: null, closeAfterSaveDocumentId: null,
    pendingFlushRequests: new Set(), PROTOCOL_VERSION: 1,
    CHANGE_SYNC_DELAY_MS: 150, maxDocumentBytes: 16 * 1024 * 1024,
    advanceDocumentEdit, utf8ByteLength: (value) => Buffer.byteLength(value),
    postToHost: (message) => sent.push(message), renderState() {},
    setTimeout: (callback) => { timers.add(callback); return callback; },
    clearTimeout: (timer) => timers.delete(timer),
    monaco: {
      Uri: { parse: (uri) => uri },
      editor: { createModel: () => model, setModelLanguage: (_model, language) => { model.language = language; } },
    },
    languageForPath: () => "plaintext",
    editor: { saveViewState: () => null },
    saveAsDocumentId: "doc", saveAsPath: { value: "renamed.ts" },
    saveAsError: {}, saveAsOverwrite: false, closeSaveAsDialog() {},
    showCloseDialog: () => { context.closePrompted = true; },
  });
  vm.runInContext(script, context);
  context.addOrReplaceDocument({
    id: "doc", uri: "flowmux://doc", name: "original.txt", relativePath: "original.txt",
    content: text, version: 1, dirty: false, readOnly: false, externalChange: false,
    encoding: "UTF-8", eol: "LF", cursorLine: 0, cursorColumn: 0, scrollTop: 0,
  });
  const document = context.documents.get("doc");
  const edit = (value) => { text = value; onChange(); };
  const receive = (message) => context.handleHostMessage({ surfaceId: "surface", ...message });
  edit("saved earlier");
  if (saveAs) context.submitSaveAs();
  else context.requestSave();
  receive({ type: "document_change_applied", documentId: "doc", documentVersion: 2, changeSequence: 1 });
  const saved = { ...document.payload, content: text, dirty: false,
    relativePath: "renamed.ts", name: "renamed.ts", language: "typescript" };
  const acknowledge = () => receive(saveAs
    ? { type: "save_as_completed", document: saved, changeSequence: 1 }
    : { type: "save_completed", documentId: "doc", documentVersion: 2, changeSequence: 1 });
  sent.length = 0;
  return { context, document, model, sent, timers, edit, receive, acknowledge };
}

for (const saveAs of [false, true]) {
  const label = saveAs ? "Save As" : "Save";
  for (const synchronized of [false, true]) {
    test(`${label} acknowledgement preserves newer ${synchronized ? "sent" : "throttled"} edits`, () => {
      const f = editorFixture(saveAs);
      f.context.closeAfterSaveDocumentId = "doc";
      f.edit("newer edit");
      assert.equal(f.context.closeAfterSaveDocumentId, null);
      if (synchronized) f.context.syncDocument(f.document);
      const timer = f.document.changeTimer;
      f.acknowledge();

      assert.equal(f.document.payload.dirty, true);
      assert.equal(f.model.getValue(), "newer edit");
      assert.equal(f.document.pendingChanges, !synchronized);
      assert.equal(f.document.changeTimer, timer);
      if (timer !== null) assert.equal(f.timers.has(timer), true);
      assert.equal(f.document.payload.version, synchronized ? 3 : 2);
      assert.equal(f.document.changeSequence, synchronized ? 2 : 1);
      assert.equal(f.document.outstandingChanges.size, synchronized ? 1 : 0);
      assert.equal(f.sent.some((message) => message.type === "close_requested"), false);
      if (saveAs) {
        assert.equal(f.document.payload.relativePath, "renamed.ts");
        assert.equal(f.document.payload.name, "renamed.ts");
        assert.equal(f.model.language, "typescript");
      }

      // Native close flushes must still wait for the newer edit's host acknowledgement.
      f.receive({ type: "flush_changes", requestId: 7 });
      assert.equal(f.sent.some((message) => message.type === "flush_completed"), false);
      const changed = f.sent.find((message) => message.type === "document_changed");
      assert.equal(changed.content, "newer edit");
      assert.equal(changed.documentVersion, 2);
      assert.equal(changed.changeSequence, 2);
      f.receive({ type: "document_change_applied", documentId: "doc", documentVersion: 3, changeSequence: 2 });
      assert.equal(f.sent.at(-1).type, "flush_completed");
      assert.equal(f.sent.at(-1).requestId, 7);
      assert.equal(f.document.payload.dirty, true);
      f.context.requestCloseActiveDocument();
      assert.equal(f.context.closePrompted, true);
      f.context.requestSave();
      assert.equal(f.sent.at(-1).type, "save_requested");
      assert.equal(f.sent.at(-1).content, "newer edit");
      assert.equal(f.sent.at(-1).documentVersion, 3);
      assert.equal(f.sent.at(-1).changeSequence, 2);
      f.receive({ type: "save_completed", documentId: "doc", documentVersion: 3, changeSequence: 2 });
      assert.equal(f.document.payload.dirty, false);
    });
  }

  test(`${label} acknowledgement marks an unchanged buffer clean`, () => {
    const f = editorFixture(saveAs);
    if (!saveAs) f.context.closeAfterSaveDocumentId = "doc";
    f.acknowledge();
    assert.equal(f.document.payload.dirty, false);
    assert.equal(f.model.getValue(), "saved earlier");
    assert.equal(f.document.pendingChanges, false);
    assert.equal(f.document.payload.version, 2);
    if (saveAs) assert.equal(f.document.payload.relativePath, "renamed.ts");
    else {
      assert.equal(f.sent.at(-1).type, "close_requested");
      assert.equal(f.sent.at(-1).dirty, false);
      assert.equal(f.context.closeAfterSaveDocumentId, null);
    }
  });
}
