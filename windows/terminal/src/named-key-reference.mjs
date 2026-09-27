// SPDX-License-Identifier: GPL-3.0-or-later
// Test-only oracle from the exact pinned dependency; never bundled into the app.
import { build } from 'esbuild';
import { fileURLToPath } from 'node:url';
import { Input } from './input.mjs';
export async function keyboardReference() {
  const { outputFiles } = await build({
    entryPoints: [fileURLToPath(new URL('../node_modules/@xterm/xterm/src/common/input/Keyboard.ts', import.meta.url))],
    nodePaths: [fileURLToPath(new URL('../node_modules/@xterm/xterm/src', import.meta.url))],
    write: false, bundle: true, format: 'esm', platform: 'node',
  });
  const { evaluateKeyboardEvent } = await import(`data:text/javascript;base64,${Buffer.from(outputFiles[0].text).toString('base64')}`);
  return (event, application) => {
    const result = evaluateKeyboardEvent(event, application, false, false);
    if (typeof result.key !== 'string') return null;
    let sent;
    const input = new Input(data => { sent = data; });
    input.keyEvent({ type: 'keydown', ...event }); input.data(result.key);
    return sent;
  };
}
