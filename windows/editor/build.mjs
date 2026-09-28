// SPDX-License-Identifier: GPL-3.0-or-later
// Bundle the shared frontend without changing its sources or checked-in dist.
import { cp, mkdir, readFile, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const directory = dirname(fileURLToPath(import.meta.url));
const repository = resolve(directory, "../..");
const source = resolve(repository, "editor/flowmux-editor-web");
const output = resolve(directory, "../assets/editor");
const entry = resolve(source, "src/main.ts");
const adapter = await readFile(resolve(directory, "adapter.js"), "utf8");
await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });
await build({
  absWorkingDir: source,
  bundle: true,
  entryPoints: {
    main: "src/main.ts",
    "editor.worker": "src/workers/editor.worker.ts",
    "json.worker": "src/workers/json.worker.ts",
    "css.worker": "src/workers/css.worker.ts",
    "html.worker": "src/workers/html.worker.ts",
    "ts.worker": "src/workers/ts.worker.ts",
  },
  nodePaths: [resolve(directory, "node_modules")],
  plugins: [{
    name: "windows-editor-command-adapter",
    setup(builder) {
      // Resolve Monaco from this Windows package even when the shared frontend
      // happens to have its own node_modules directory on a developer machine.
      builder.onResolve({ filter: /^monaco-editor\// }, ({ path }) => ({
        path: resolve(directory, "node_modules", path),
      }));
      builder.onLoad({ filter: /[/\\]main\.ts$/ }, async ({ path }) => {
        if (path !== entry) return;
        return {
          contents: `${await readFile(path, "utf8")}\n${adapter}`,
          loader: "ts",
          resolveDir: dirname(path),
        };
      });
    },
  }],
  entryNames: "[name]",
  format: "esm",
  legalComments: "eof",
  loader: { ".ttf": "dataurl" },
  minify: true,
  outdir: output,
  platform: "browser",
  sourcemap: false,
  target: ["es2022", "safari15"],
});
await cp(resolve(source, "index.html"), resolve(output, "index.html"));
await cp(resolve(repository, "docs/legal/THIRD_PARTY_NOTICES.md"), resolve(output, "THIRD_PARTY_NOTICES.md"));
await cp(resolve(source, "MONACO_THIRD_PARTY_NOTICES.txt"), resolve(output, "MONACO_THIRD_PARTY_NOTICES.txt"));
console.log("Built offline Windows Monaco assets from unchanged shared frontend sources.");
