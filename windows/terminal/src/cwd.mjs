// SPDX-License-Identifier: GPL-3.0-or-later
// Metadata from terminal output is untrusted. Never resolve or open it here.
export function osc9Cwd(data) {
  if (!data.startsWith('9;')) return null;
  let path = data.slice(2);
  if (path.startsWith('"') && path.endsWith('"')) path = path.slice(1, -1);
  return localPath(path);
}
export function osc7Cwd(data) {
  if (data.length > 32767 || !data.startsWith('file://')) return null;
  try {
    const url = new URL(data);
    if (url.protocol !== 'file:' || !['', 'localhost'].includes(url.hostname) || url.search || url.hash) return null;
    return localPath(decodeURIComponent(url.pathname.slice(1)));
  } catch { return null; }
}
function localPath(path) {
  if (path.length > 32767 || !/^[a-z]:[\\/]/i.test(path) || /[\x00-\x1f\x7f<>:"|?*]/.test(path.slice(3))) return null;
  return path;
}

export function observeCwd(terminal, send, isRestoring) {
  terminal.parser.registerOscHandler(9, data => {
    const path = osc9Cwd(data);
    if (path && !isRestoring()) send({ type: 'cwd', path });
    return data.startsWith('9;');
  });
  terminal.parser.registerOscHandler(7, data => {
    const path = osc7Cwd(data);
    if (path && !isRestoring()) send({ type: 'cwd', path });
    return true;
  });
}
