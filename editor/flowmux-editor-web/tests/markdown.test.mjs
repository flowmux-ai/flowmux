// SPDX-License-Identifier: GPL-3.0-or-later

import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { runInNewContext } from "node:vm";

test("plain Markdown completes without initializing Mermaid or its browser dependencies", async () => {
  const source = await readFile(new URL("../dist/markdown.js", import.meta.url), "utf8");
  const window = {};
  runInNewContext(source, {
    window,
    document: {
      querySelectorAll(selector) {
        assert.equal(selector, "main.markdown-body pre > code.language-mermaid");
        return [];
      },
    },
  });
  assert.equal(typeof window.flowmuxMarkdownReady?.then, "function");
  await window.flowmuxMarkdownReady;
});
