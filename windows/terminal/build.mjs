// SPDX-License-Identifier: GPL-3.0-or-later
import { build } from 'esbuild';
import { copyFile, mkdir, readFile, writeFile } from 'node:fs/promises';

await mkdir('../assets', { recursive: true });
await build({ entryPoints: ['src/terminal.mjs'], bundle: true, outfile: '../assets/terminal.js',
  format: 'iife', target: ['chrome109'], minify: true, legalComments: 'eof',
  banner: { js: '/* SPDX-License-Identifier: GPL-3.0-or-later; bundled dependencies: see THIRD_PARTY.txt */' } });
await copyFile('node_modules/@xterm/xterm/css/xterm.css', '../assets/xterm.css');
let notices = 'flowmux terminal bundle: GPL-3.0-or-later\n\n';
for (const name of ['xterm', 'addon-fit', 'addon-search', 'addon-serialize']) {
  // addon-serialize's npm archive omits LICENSE; all four belong to the same MIT project.
  const license = await readFile(`node_modules/@xterm/${name}/LICENSE`, 'utf8')
    .catch(() => readFile('node_modules/@xterm/xterm/LICENSE', 'utf8'));
  notices += `@xterm/${name}\n${license}\n\n`;
}
await writeFile('../assets/THIRD_PARTY.txt', notices.replace(/[ \t]+$/gm, '').trimEnd() + '\n');
