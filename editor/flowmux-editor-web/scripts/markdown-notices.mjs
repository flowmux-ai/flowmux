// SPDX-License-Identifier: GPL-3.0-or-later

import { readFile, readdir } from "node:fs/promises";
import { resolve } from "node:path";

export async function markdownNotices(root) {
  const lock = JSON.parse(await readFile(resolve(root, "package-lock.json"), "utf8"));
  const sections = ["Mermaid and its production dependencies. DOMPurify is used under Apache-2.0."];
  for (const [path, entry] of Object.entries(lock.packages).sort(([a], [b]) => a.localeCompare(b, "en"))) {
    if (!path || entry.dev || path === "node_modules/monaco-editor") continue;
    const dir = resolve(root, path);
    const pkg = JSON.parse(await readFile(resolve(dir, "package.json"), "utf8"));
    // Khroma omits package metadata but ships the MIT license text.
    const license = pkg.license ?? (pkg.name === "khroma" ? "MIT" : undefined);
    if (!["MIT", "ISC", "BSD-3-Clause", "Apache-2.0", "Unlicense", "(MPL-2.0 OR Apache-2.0)"].includes(license)) {
      throw new Error(`Review the Markdown dependency license: ${pkg.name}: ${license}`);
    }
    const files = (await readdir(dir)).filter((name) => /^(licen[cs]e|copying|notice)(\.|$)/i.test(name)).sort();
    let text;
    if (files.length) {
      text = (await Promise.all(files.map((file) => readFile(resolve(dir, file), "utf8")))).join("\n\n");
    } else {
      const readme = await readFile(resolve(dir, "README.md"), "utf8");
      const start = readme.indexOf("## License\n");
      if (start < 0) throw new Error(`Missing license text: ${pkg.name}`);
      text = readme.slice(start);
    }
    sections.push(`${pkg.name} ${pkg.version} — ${license}\n\n${text.trim()}`);
  }
  return sections.join("\n\n----------------------------------------\n\n") + "\n";
}
