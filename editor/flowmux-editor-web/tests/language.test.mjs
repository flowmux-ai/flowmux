// SPDX-License-Identifier: GPL-3.0-or-later

import assert from "node:assert/strict";
import test from "node:test";
import { languageForPath } from "../.test-build/language.js";

test("recognizes exact file names and common extensions", () => {
  assert.equal(languageForPath("/workspace/Cargo.toml"), "toml");
  assert.equal(languageForPath("src/main.rs"), "rust");
  assert.equal(languageForPath("web/app.tsx"), "typescript");
  assert.equal(languageForPath("Dockerfile"), "dockerfile");
});

test("handles multilingual paths without changing their contents", () => {
  assert.equal(languageForPath("문서/인사말.py"), "python");
  assert.equal(languageForPath("日本語/設定.yaml"), "yaml");
  assert.equal(languageForPath("العربية/رسالة.md"), "markdown");
  assert.equal(languageForPath("emoji/메모🧭.txt"), "plaintext");
});

test("falls back to plain text for unknown or extensionless files", () => {
  assert.equal(languageForPath("README"), "plaintext");
  assert.equal(languageForPath("archive.unknown"), "plaintext");
  assert.equal(languageForPath("trailing."), "plaintext");
});
