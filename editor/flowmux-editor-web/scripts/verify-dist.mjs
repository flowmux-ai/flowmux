// SPDX-License-Identifier: GPL-3.0-or-later

import { readFile, stat } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const requiredFiles = [
  "MONACO_THIRD_PARTY_NOTICES.txt",
  "THIRD_PARTY_NOTICES.md",
  "index.html",
  "main.js",
  "main.css",
  "editor.worker.js",
  "json.worker.js",
  "css.worker.js",
  "html.worker.js",
  "ts.worker.js",
];

for (const file of requiredFiles) {
  const details = await stat(resolve(root, "dist", file));
  if (!details.isFile() || details.size === 0) {
    throw new Error(`Editor asset is missing or empty: ${file}`);
  }
}

const html = await readFile(resolve(root, "dist", "index.html"), "utf8");
if (
  !html.includes("Content-Security-Policy") ||
  !html.includes("style-src 'self' 'unsafe-inline'") ||
  !html.includes('src="./main.js"') ||
  !html.includes('id="close-dialog"') ||
  !html.includes('id="close-dialog-save"') ||
  !html.includes('id="close-dialog-discard"') ||
  !html.includes('id="close-dialog-cancel"') ||
  !html.includes('id="recovery-dialog"') ||
  !html.includes('id="recovery-dialog-restore"') ||
  !html.includes('id="recovery-dialog-discard"') ||
  !html.includes('id="search-dialog"') ||
  !html.includes('id="search-query"') ||
  !html.includes('id="conflict-banner"') ||
  !html.includes('id="zoom-toast"') ||
  !html.includes('id="save-as-dialog"') ||
  !html.includes('id="diff-editor"')
) {
  throw new Error("Editor entry point is missing its security or document safety controls");
}

const main = await readFile(resolve(root, "dist", "main.js"), "utf8");
const css = await readFile(resolve(root, "dist", "main.css"), "utf8");
const notice = await readFile(resolve(root, "THIRD_PARTY_NOTICES.md"), "utf8");
const distributedNotice = await readFile(
  resolve(root, "dist", "THIRD_PARTY_NOTICES.md"),
  "utf8",
);
const monacoNotice = await readFile(
  resolve(root, "MONACO_THIRD_PARTY_NOTICES.txt"),
  "utf8",
);
const upstreamMonacoNotice = await readFile(
  resolve(root, "node_modules", "monaco-editor", "ThirdPartyNotices.txt"),
  "utf8",
);
const distributedMonacoNotice = await readFile(
  resolve(root, "dist", "MONACO_THIRD_PARTY_NOTICES.txt"),
  "utf8",
);
if (distributedNotice !== notice) {
  throw new Error("The distributed editor notice is out of date");
}
if (monacoNotice !== upstreamMonacoNotice) {
  throw new Error("The tracked Monaco notice differs from the installed package");
}
if (distributedMonacoNotice !== monacoNotice) {
  throw new Error("The distributed Monaco notice is out of date");
}
if (
  !notice.includes("DOMPurify 3.1.7") ||
  !main.includes("@license DOMPurify 3.1.7") ||
  !main.includes("Apache license 2.0 and Mozilla Public License 2.0")
) {
  throw new Error("The editor bundle is missing the DOMPurify license notice");
}
if (
  html.includes('id="mode-switch"') ||
  html.includes('id="mode-edit"') ||
  html.includes('id="mode-diff"') ||
  css.includes(".mode-switch")
) {
  throw new Error("Editor bundle still exposes the unfinished edit and diff controls");
}
if (
  !main.includes("discard_close_requested") ||
  !main.includes("recovery_decision") ||
  !main.includes("view_state_changed") ||
  !main.includes("quick_open_requested") ||
  !main.includes("workspace_search_requested") ||
  !main.includes("search_result_open_requested") ||
  !main.includes("save_as_requested") ||
  !main.includes("conflict_action_requested") ||
  !main.includes("set_appearance") ||
  !main.includes("zoom_changed")
) {
  throw new Error("Editor bundle is missing an explicit document safety message");
}
if (!main.includes("actions.find") || !css.includes(".find-widget")) {
  throw new Error("Editor bundle is missing Monaco's built-in find contribution");
}

console.log(`Verified ${requiredFiles.length} editor assets.`);
