// SPDX-License-Identifier: GPL-3.0-or-later
// Search the parsed grid on demand. Never copy raw PTY transcripts or normalize text.
const yieldTask = () => new Promise(resolve => {
  const channel = new MessageChannel();
  channel.port1.onmessage = () => { channel.port1.close(); channel.port2.close(); resolve(); };
  channel.port2.postMessage(0);
});
function excerpt(text, start, end) {
  let value=text.slice(start,end);
  if (/^[\uDC00-\uDFFF]/.test(value)) value=value.slice(1);
  if (/[\uD800-\uDBFF]$/.test(value)) value=value.slice(0,-1);
  return value;
}

export function matchRange(text, query, matchCase) {
  const offset = (matchCase ? text : text.toLowerCase()).indexOf(matchCase ? query : query.toLowerCase());
  if (offset < 0) return null;
  if (matchCase) return [offset, offset + query.length];
  // Lowercasing can expand a character (for example U+0130). Map offsets back
  // before converting the original UTF-16 text into terminal cell coordinates.
  const end = offset + query.toLowerCase().length;
  let folded = 0, original = 0, start = null, finish = text.length;
  for (const char of text) {
    const size = char.toLowerCase().length;
    if (start === null && folded + size > offset) start = original;
    original += char.length; folded += size;
    if (folded >= end) { finish = original; break; }
  }
  return [start ?? original, finish];
}

function rowText(buffer, row) {
  const line = buffer.getLine(row), next = buffer.getLine(row + 1);
  const wraps = !!next?.isWrapped;
  let text = line.translateToString(!wraps);
  // xterm reserves a blank cell when a wide character cannot fit before wrapping.
  if (wraps) {
    const last = line.getCell(line.length - 1);
    if (last?.getCode() === 0 && last.getWidth() === 1 && next.getCell(0)?.getWidth() === 2) text = text.slice(0, -1);
  }
  return { row, text, wraps };
}

function cellPosition(buffer, parts, offset, end) {
  let consumed = 0;
  for (let p = 0; p < parts.length; p++) {
    const part = parts[p];
    if (offset > consumed + part.text.length || (!end && offset === consumed + part.text.length && p + 1 < parts.length)) {
      consumed += part.text.length; continue;
    }
    const target = offset - consumed, line = buffer.getLine(part.row);
    let units = 0;
    for (let col = 0; col < line.length; col++) {
      const cell = line.getCell(col), width = cell.getWidth();
      if (!width) continue;
      const size = (cell.getChars() || ' ').length;
      if (!end && units + size > target) return { row:part.row, col };
      units += size;
      if (end && units >= target) return { row:part.row, col:col + width };
    }
    return { row:part.row, col:line.length };
  }
  throw new Error('Search position is unavailable; refresh the search');
}

export class OutputSearch {
  constructor(terminal, send, pause = yieldTask, selection) {
    this.selection = selection;
    this.terminal = terminal; this.send = send; this.pause = pause;
    this.revision = 0; this.current = null; this.retained = [];
    terminal.onResize(() => this.changed());
  }
  changed() { this.revision++; }
  cancel(search) {
    if (search !== this.current) return;
    this.current = null;
    for (const hit of this.retained) hit.marker?.dispose();
    this.retained = [];
  }
  async scan(message, sequence) {
    this.cancel(this.current);
    this.current = message.search;
    const revision = this.revision, buffer = this.terminal.buffer.active, cols = this.terminal.cols;
    const unchanged = () => this.current === message.search && this.revision === revision && this.terminal.buffer.active === buffer;
    let total = 0, parts = [];
    const hits = [];
    try {
      for (let row = 0; row < buffer.length; row++) {
        if (!unchanged()) throw new Error('Output changed during search; refresh to retry');
        parts.push(rowText(buffer, row));
        if (!parts.at(-1).wraps) {
          const text = parts.map(p => p.text).join('');
          const range = matchRange(text, message.query, message.match_case);
          if (range) {
            if (total >= message.skip && hits.length < message.limit) {
              const first = parts[0].row;
              const start = cellPosition(buffer, parts, range[0], false), end = cellPosition(buffer, parts, range[1], true);
              const marker = buffer.type === 'normal' ? this.terminal.registerMarker(first - buffer.baseY - buffer.cursorY) : null;
              const id = this.retained.length;
              this.retained.push({ marker, first, text, range, cols, buffer, revision, wrapped:buffer.getLine(first).isWrapped });
              const before = Array.from(excerpt(text, Math.max(0,range[0]-80), range[0])).slice(-40).join('');
              const after = Array.from(excerpt(text,range[0],range[0]+360)).slice(0, 180).join('');
              hits.push({ id, line:start.row, column:start.col, preview:before + after });
            }
            total++;
          }
          parts = [];
        }
        // MessageChannel yields without relying on throttled hidden-tab timers.
        if (row % 128 === 127) await this.pause();
      }
      if (!unchanged()) throw new Error('Output changed during search; refresh to retry');
      this.send({ type:'search_results', search:message.search, sequence, total, hits, error:null });
    } catch (error) {
      if (this.current !== message.search) return;
      this.cancel(message.search);
      this.send({ type:'search_results', search:message.search, sequence, total:0, hits:[], error:String(error.message) });
    }
  }
  async open(message, sequence) {
    try {
      if (this.current !== message.search) throw new Error('Search expired; refresh the search');
      const hit = this.retained[message.hit], buffer = this.terminal.buffer.active;
      if (!hit || hit.buffer !== buffer || hit.cols !== this.terminal.cols || hit.marker?.isDisposed ||
          (buffer.type !== 'normal' && hit.revision !== this.revision)) throw new Error('Output changed or left scrollback; refresh the search');
      const first = hit.marker ? hit.marker.line : hit.first;
      if (first < 0 || !buffer.getLine(first) || buffer.getLine(first).isWrapped !== hit.wrapped) throw new Error('Output left scrollback; refresh the search');
      const revision = this.revision, parts = [];
      for (let row = first; row < buffer.length; row++) {
        parts.push(rowText(buffer, row));
        if (!parts.at(-1).wraps) break;
        if (parts.length % 128 === 0) await this.pause();
      }
      if (this.current !== message.search || this.revision !== revision || parts.map(p => p.text).join('') !== hit.text) {
        throw new Error('Output changed; refresh the search');
      }
      const start = cellPosition(buffer, parts, hit.range[0], false), end = cellPosition(buffer, parts, hit.range[1], true);
      if (!message.commit) {
        this.send({ type:'search_opened', request:message.request, search:message.search, sequence,
          error:null, selection:'', line:start.row, column:start.col, selected:false });
        return;
      }
      this.selection?.forget();
      this.terminal.select(start.col, start.row, (end.row-start.row)*hit.cols + end.col-start.col);
      this.selection?.capture();
      this.terminal.scrollToLine(Math.max(0, start.row-Math.floor(this.terminal.rows/2)));
      const selection=this.terminal.getSelection();
      if (selection.length>16384) throw new Error('Selected cell text exceeds response limit');
      this.send({ type:'search_opened', request:message.request, search:message.search, sequence,
        error:null, selection:this.terminal.getSelection(), line:start.row, column:start.col, selected:true });
    } catch (error) {
      this.send({ type:'search_opened', request:message.request, search:message.search, sequence,
        error:String(error.message), selection:'', line:0, column:0, selected:!!message.commit });
    }
  }
}
