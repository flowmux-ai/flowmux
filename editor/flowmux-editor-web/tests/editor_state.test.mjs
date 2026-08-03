// SPDX-License-Identifier: GPL-3.0-or-later

import assert from "node:assert/strict";
import test from "node:test";
import {
  adjustedZoomPercent,
  conflictUiState,
  editorZoomDirectionForKey,
  utf8ByteLength,
  visibleDocumentState,
} from "../.test-build/editor_state.js";

test("document byte limits use UTF-8 bytes", () => {
  assert.equal(utf8ByteLength("abc"), 3);
  assert.equal(utf8ByteLength("한🙂"), 7);
});

test("editor zoom moves in ten percent steps inside its supported range", () => {
  assert.equal(adjustedZoomPercent(100, 1), 110);
  assert.equal(adjustedZoomPercent(200, 1), 200);
  assert.equal(adjustedZoomPercent(50, -1), 50);
});

test("editor zoom recognizes Ctrl plus and minus before Monaco handles them", () => {
  const key = (overrides) => ({
    altKey: false,
    ctrlKey: true,
    metaKey: false,
    shiftKey: false,
    key: "",
    code: "",
    ...overrides,
  });
  assert.equal(
    editorZoomDirectionForKey(key({ shiftKey: true, key: "+", code: "Equal" })),
    1,
  );
  assert.equal(editorZoomDirectionForKey(key({ key: "-", code: "Minus" })), -1);
  assert.equal(editorZoomDirectionForKey(key({ key: "=", code: "Equal" })), null);
  assert.equal(editorZoomDirectionForKey(key({ altKey: true, key: "-", code: "Minus" })), null);
});

test("conflict actions distinguish changed deleted and compare states", () => {
  assert.deepEqual(conflictUiState("modified", true, false), {
    hidden: false,
    message: "This file changed on disk while you were editing it.",
    compareDisabled: false,
    reloadDisabled: false,
    keepLabel: "Keep Mine",
    closeCompareHidden: true,
  });
  assert.deepEqual(conflictUiState("deleted", true, false), {
    hidden: false,
    message: "This file was deleted on disk.",
    compareDisabled: true,
    reloadDisabled: true,
    keepLabel: "Recreate on Save",
    closeCompareHidden: true,
  });
  assert.equal(conflictUiState("modified", true, true).compareDisabled, true);
  assert.equal(conflictUiState("unchanged", false, false).hidden, true);
});

test("document state distinguishes external changes and deletion", () => {
  assert.deepEqual(visibleDocumentState("modified", false, true), {
    text: "Changed on disk",
    kind: "conflict",
    hidden: false,
  });
  assert.deepEqual(visibleDocumentState("deleted", false, false), {
    text: "Deleted on disk",
    kind: "conflict",
    hidden: false,
  });
  assert.deepEqual(visibleDocumentState("unchanged", false, false), {
    text: "Saved",
    kind: "normal",
    hidden: true,
  });
});
