// SPDX-License-Identifier: GPL-3.0-or-later

import assert from "node:assert/strict";
import test from "node:test";
import { focusDirectionForKey } from "../.test-build/focus_navigation.js";

function key(key, overrides = {}) {
  return {
    key,
    altKey: false,
    ctrlKey: false,
    shiftKey: false,
    metaKey: false,
    isComposing: false,
    ...overrides,
  };
}

test("plain Alt+arrow requests pane focus navigation", () => {
  assert.equal(focusDirectionForKey(key("ArrowLeft", { altKey: true })), "left");
  assert.equal(focusDirectionForKey(key("ArrowRight", { altKey: true })), "right");
  assert.equal(focusDirectionForKey(key("ArrowUp", { altKey: true })), "up");
  assert.equal(focusDirectionForKey(key("ArrowDown", { altKey: true })), "down");
});

test("typing and editor navigation remain owned by Monaco", () => {
  assert.equal(focusDirectionForKey(key("ArrowLeft")), null);
  assert.equal(focusDirectionForKey(key("a")), null);
  assert.equal(focusDirectionForKey(key("ArrowRight", { altKey: true, shiftKey: true })), null);
  assert.equal(focusDirectionForKey(key("ArrowUp", { altKey: true, ctrlKey: true })), null);
  assert.equal(focusDirectionForKey(key("ArrowDown", { altKey: true, metaKey: true })), null);
  assert.equal(focusDirectionForKey(key("ArrowLeft", { altKey: true, isComposing: true })), null);
});
