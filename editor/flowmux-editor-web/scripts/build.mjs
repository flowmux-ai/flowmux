// SPDX-License-Identifier: GPL-3.0-or-later

import { cp, mkdir, rm } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { build } from "esbuild";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const output = resolve(root, "dist");

await rm(output, { recursive: true, force: true });
await mkdir(output, { recursive: true });

await build({
  absWorkingDir: root,
  bundle: true,
  entryPoints: {
    main: "src/main.ts",
    "editor.worker": "src/workers/editor.worker.ts",
    "json.worker": "src/workers/json.worker.ts",
    "css.worker": "src/workers/css.worker.ts",
    "html.worker": "src/workers/html.worker.ts",
    "ts.worker": "src/workers/ts.worker.ts",
  },
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

await cp(resolve(root, "index.html"), resolve(output, "index.html"));
await cp(
  resolve(root, "THIRD_PARTY_NOTICES.md"),
  resolve(output, "THIRD_PARTY_NOTICES.md"),
);
await cp(
  resolve(root, "MONACO_THIRD_PARTY_NOTICES.txt"),
  resolve(output, "MONACO_THIRD_PARTY_NOTICES.txt"),
);
