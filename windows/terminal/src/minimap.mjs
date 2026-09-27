// SPDX-License-Identifier: GPL-3.0-or-later
// Public xterm cell APIs only; one physical buffer row per CSS pixel.
export const MAX_PREVIEW_ROWS = 2048;
const names = ['black','red','green','yellow','blue','magenta','cyan','white',
  'brightBlack','brightRed','brightGreen','brightYellow','brightBlue','brightMagenta','brightCyan','brightWhite'];
// xterm 6's default ANSI palette, before explicit theme overrides.
const defaults = ['#2e3436','#cc0000','#4e9a06','#c4a000','#3465a4','#75507b','#06989a','#d3d7cf',
  '#555753','#ef2929','#8ae234','#fce94f','#729fcf','#ad7fa8','#34e2e2','#eeeeec'];
const hex = n => `#${n.toString(16).padStart(6, '0')}`;
const clamp = (n, a, b) => Math.min(b, Math.max(a, n));

export function paletteColor(index, theme) {
  if (index < 16) return theme[names[index]] ?? defaults[index];
  if (index >= 232) { const gray = 8 + (index - 232) * 10; return hex(gray * 0x10101); }
  const n = index - 16, levels = [0, 95, 135, 175, 215, 255];
  return hex((levels[Math.floor(n / 36)] << 16) | (levels[Math.floor(n / 6) % 6] << 8) | levels[n % 6]);
}
export function cellPaint(cell, theme, boldBright = true) {
  const color = (foreground) => {
    if (foreground ? cell.isFgRGB() : cell.isBgRGB()) return hex(foreground ? cell.getFgColor() : cell.getBgColor());
    if (foreground ? cell.isFgPalette() : cell.isBgPalette()) {
      let n = foreground ? cell.getFgColor() : cell.getBgColor();
      if (foreground && boldBright && cell.isBold() && n < 8) n += 8;
      return paletteColor(n, theme);
    }
    return foreground ? theme.foreground : theme.background;
  };
  let fg = color(true), bg = color(false);
  if (cell.isInverse()) [fg, bg] = [bg, fg];
  return { fg, bg, ink: !cell.isInvisible() && /\S/u.test(cell.getChars()), alpha: cell.isDim() ? .5 : 1 };
}
export function previewWindow(length, height, offset) {
  const rows = Math.min(length, Math.max(1, Math.floor(height)), MAX_PREVIEW_ROWS);
  offset = clamp(Math.round(offset), 0, length - rows);
  return { top: length - rows - offset, rows, offset };
}
export function followViewport(window, viewport, rows, length) {
  const top = viewport < window.top ? viewport
    : viewport + rows > window.top + window.rows ? viewport + rows - window.rows : window.top;
  return length - window.rows - clamp(top, 0, length - window.rows);
}
export function pointerRow(window, y, rows, base) {
  return clamp(Math.floor(window.top + clamp(y, 0, window.rows) - rows / 2), 0, base);
}

export class Minimap {
  constructor(terminal, document, blocked, clock = { set: (fn, ms) => setTimeout(fn, ms), clear: id => clearTimeout(id) }) {
    Object.assign(this, { terminal, document, blocked, clock });
    this.area = document.getElementById('minimap');
    this.canvas = this.area.querySelector('canvas'); this.thumb = this.area.querySelector('.viewport');
    this.context = this.canvas.getContext('2d', { willReadFrequently: true });
    this.enabled = false; this.width = 40; this.opacity = 50; this.shown = false;
    this.offset = 0; this.timer = null; this.dirty = true; this.renders = 0; this.drag = null;
    this.stats = { cells: 0, ink_cells: 0, wide_cells: 0, colors: [] };
    terminal.onScroll(() => this.scrolled());
    terminal.onResize(() => { this.offset = 0; this.changed(); });
    terminal.buffer.onBufferChange(() => { this.offset = 0; this.drag = null; this.sync(); });
    this.area.addEventListener('wheel', e => {
      if (!this.interactive()) return;
      e.preventDefault(); e.stopPropagation();
      if (e.deltaY) this.preview(Math.sign(e.deltaY) * 25);
    }, { passive: false });
    this.area.addEventListener('pointerdown', e => {
      if (e.button !== 0 || !this.interactive()) return;
      e.preventDefault(); this.drag = e.pointerId; this.area.setPointerCapture(e.pointerId);
      this.point(e.clientY); this.terminal.focus();
    });
    this.area.addEventListener('pointermove', e => { if (this.drag === e.pointerId && this.interactive()) this.point(e.clientY); });
    const release = () => { this.drag = null; };
    this.area.addEventListener('pointerup', release); this.area.addEventListener('pointercancel', release);
    this.area.addEventListener('lostpointercapture', release);
    this.area.addEventListener('keydown', e => {
      if (!this.interactive() || e.isComposing || e.keyCode === 229 || e.altKey || e.ctrlKey || e.metaKey) return;
      const b = terminal.buffer.active;
      const target = { ArrowUp: b.viewportY - 1, ArrowDown: b.viewportY + 1,
        PageUp: b.viewportY - terminal.rows, PageDown: b.viewportY + terminal.rows, Home: 0, End: b.baseY }[e.key];
      if (target !== undefined) { e.preventDefault(); e.stopPropagation(); terminal.scrollToLine(clamp(target, 0, b.baseY)); }
    });
  }
  configure(settings) {
    this.enabled = settings.minimap_enabled; this.width = settings.minimap_width; this.opacity = settings.minimap_opacity;
    if (!this.enabled) this.offset = 0;
    // Retain this gutter across alternate-buffer transitions; only configuration
    // changes its size. CSS hides the original scrollbar without changing fit.
    this.document.body.style.setProperty('--minimap-width', `${this.enabled ? this.width : 0}px`);
    this.document.body.classList.toggle('minimap-enabled', this.enabled);
    this.area.style.opacity = String(this.opacity / 100);
    this.sync();
  }
  visibility(shown) { this.shown = shown; this.sync(); }
  eligible() { return this.enabled && this.shown && this.terminal.buffer.active.type === 'normal'; }
  interactive() { return this.eligible() && !this.blocked(); }
  sync() {
    this.area.hidden = !this.eligible();
    this.drag = null;
    if (!this.eligible()) {
      if (this.timer !== null) this.clock.clear(this.timer);
      this.timer = null; this.canvas.width = this.canvas.height = 1;
      this.stats = { cells: 0, ink_cells: 0, wide_cells: 0, colors: [] };
    }
    this.changed();
  }
  window() { return previewWindow(this.terminal.buffer.active.length, this.area.getBoundingClientRect().height, this.offset); }
  changed() {
    this.dirty = true;
    if (!this.eligible() || this.timer !== null) return;
    // Keep the first deadline during continuous output; don't debounce forever.
    this.timer = this.clock.set(() => { this.timer = null; this.paint(); }, 100);
  }
  scrolled() {
    if (!this.eligible()) return;
    const b = this.terminal.buffer.active;
    const offset = followViewport(this.window(), b.viewportY, this.terminal.rows, b.length);
    if (offset !== this.offset) { this.offset = offset; this.changed(); }
    this.marker();
  }
  marker() {
    const b = this.terminal.buffer.active, w = this.window();
    const size = Math.min(this.terminal.rows, w.rows), raw = b.viewportY - w.top;
    this.thumb.style.top = `${clamp(raw, 0, w.rows - size)}px`; this.thumb.style.height = `${size}px`;
    this.thumb.style.opacity = raw < 0 || raw + size > w.rows ? '.08' : '.24';
    this.area.setAttribute('aria-valuemin', '0'); this.area.setAttribute('aria-valuemax', String(b.baseY));
    this.area.setAttribute('aria-valuenow', String(b.viewportY));
    this.area.setAttribute('aria-valuetext', `Terminal row ${b.viewportY + 1}; preview rows ${w.top + 1} to ${w.top + w.rows}`);
  }
  preview(delta) {
    this.offset = previewWindow(this.terminal.buffer.active.length, this.area.getBoundingClientRect().height, this.offset - delta).offset;
    this.changed(); this.marker();
  }
  seek(row) {
    const b = this.terminal.buffer.active;
    if (!Number.isInteger(row) || row < 0 || row >= b.length) throw new Error('Minimap row is outside retained history');
    this.terminal.scrollToLine(clamp(Math.floor(row - this.terminal.rows / 2), 0, b.baseY));
  }
  point(clientY) {
    const b = this.terminal.buffer.active;
    this.terminal.scrollToLine(pointerRow(this.window(), clientY - this.area.getBoundingClientRect().top, this.terminal.rows, b.baseY));
  }
  paint() {
    if (!this.eligible() || !this.dirty) return;
    if (!this.context) throw new Error('Minimap canvas is unavailable');
    const b = this.terminal.buffer.active, w = this.window(), rect = this.area.getBoundingClientRect();
    this.offset = w.offset;
    const height = Math.min(MAX_PREVIEW_ROWS, Math.max(1, Math.floor(rect.height)));
    const scale = clamp(this.document.defaultView.devicePixelRatio || 1, 1, 2);
    this.canvas.width = Math.ceil(this.width * scale); this.canvas.height = Math.ceil(height * scale);
    // Keep one preview row per CSS pixel even in windows taller than the cap.
    this.canvas.style.height = `${height}px`;
    const ctx = this.context, theme = this.terminal.options.theme, cols = this.terminal.cols;
    ctx.setTransform(scale, 0, 0, scale, 0, 0); ctx.globalAlpha = 1;
    ctx.fillStyle = theme.background; ctx.fillRect(0, 0, this.width, height);
    const colors = new Map(); let cells = 0, ink = 0, wide = 0;
    const reusable = b.getNullCell();
    for (let y = 0; y < w.rows; y++) {
      const line = b.getLine(w.top + y); if (!line) continue;
      for (let x = 0; x < cols; x++) {
        const cell = line.getCell(x, reusable); if (!cell) continue;
        cells++; const size = cell.getWidth(); if (!size) continue;
        const p = cellPaint(cell, theme, this.terminal.options.drawBoldTextInBrightColors !== false);
        const left = x * this.width / cols, width = Math.min(size, cols - x) * this.width / cols;
        if (p.bg !== theme.background) { ctx.globalAlpha = 1; ctx.fillStyle = p.bg; ctx.fillRect(left, y, width, 1); }
        if (p.ink) {
          ctx.globalAlpha = p.alpha; ctx.fillStyle = p.fg; ctx.fillRect(left, y, width, 1);
          ink++; if (size === 2) wide++;
          if (colors.has(p.fg) || colors.size < 32) colors.set(p.fg, (colors.get(p.fg) ?? 0) + 1);
        }
      }
    }
    ctx.globalAlpha = 1; this.dirty = false; this.renders++;
    this.stats = { cells, ink_cells: ink, wide_cells: wide, colors: [...colors].map(([color, count]) => ({ color, count })) };
    this.marker();
  }
  run(action) {
    try {
      if (action.kind !== 'read') {
        if (!this.interactive()) throw new Error('Minimap navigation is unavailable while hidden, disabled, in alternate screen or composing');
        if (action.kind === 'preview') this.preview(action.rows);
        else if (action.kind === 'seek') this.seek(action.row);
        else throw new Error('Unknown minimap action');
      }
      this.paint();
      const b = this.terminal.buffer.active, w = this.window();
      let hash = null;
      if (this.eligible()) {
        let n = 2166136261;
        for (const byte of this.context.getImageData(0, 0, this.canvas.width, this.canvas.height).data) n = Math.imul(n ^ byte, 16777619);
        hash = (n >>> 0).toString(16).padStart(8, '0');
      }
      return { status: 'ok', snapshot: { enabled: this.enabled, visible: this.eligible(), width: this.width, opacity: this.opacity,
        buffer: b.type, cols: this.terminal.cols, rows: this.terminal.rows, buffer_rows: b.length, viewport_row: b.viewportY,
        preview_top: w.top, preview_rows: w.rows, preview_offset: w.offset, renders: this.renders,
        gutter: this.document.body.clientWidth - this.document.getElementById('terminal').getBoundingClientRect().right - 5,
        raster_width: this.canvas.width, raster_height: this.canvas.height, raster_hash: hash, ...this.stats } };
    } catch (error) { return { status: 'error', message: String(error) }; }
  }
}
