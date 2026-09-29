// SPDX-License-Identifier: GPL-3.0-or-later
export const themes = Object.freeze({
  dark: { background: '#282c34', foreground: '#ffffff', cursor: '#b9c6ff', selectionBackground: '#455483',
    black:'#5c6370', red:'#cc6666', green:'#b5bd68', yellow:'#f0c674', blue:'#81a2be', magenta:'#b294bb', cyan:'#8abeb7', white:'#c5c8c6',
    brightBlack:'#7f848e', brightRed:'#d54e53', brightGreen:'#b9ca4a', brightYellow:'#e7c547', brightBlue:'#7aa6da', brightMagenta:'#c397d8', brightCyan:'#70c0b1', brightWhite:'#eaeaea' },
  light: { background: '#ffffff', foreground: '#202124', cursor: '#202124', selectionBackground: '#b5cff7',
    black:'#202124', red:'#b42318', green:'#18733b', yellow:'#805500', blue:'#185abc', magenta:'#8f2db3', cyan:'#007580', white:'#c5c7cb',
    brightBlack:'#686b70', brightRed:'#c5221f', brightGreen:'#188038', brightYellow:'#956500', brightBlue:'#1967d2', brightMagenta:'#a142b8', brightCyan:'#00838f', brightWhite:'#f1f3f4' },
});
const paletteKeys = ['black','red','green','yellow','blue','magenta','cyan','white',
  'brightBlack','brightRed','brightGreen','brightYellow','brightBlue','brightMagenta','brightCyan','brightWhite'];
function resolvedOptions(colors) {
  if (!colors) return null;
  if (!Array.isArray(colors.palette) || colors.palette.length !== 16 ||
      typeof colors.dark !== 'boolean' ||
      ![colors.background,colors.foreground,colors.cursor,...colors.palette].every(value => typeof value === 'string' && /^#[0-9a-f]{6}$/i.test(value)) ||
      ![colors.selection_background,colors.selection_foreground].every(value => value === null || (typeof value === 'string' && /^#[0-9a-f]{6}$/i.test(value)))) {
    throw Error('Invalid resolved terminal colors');
  }
  return {background:colors.background,foreground:colors.foreground,cursor:colors.cursor,
    selectionBackground:colors.selection_background ?? undefined,
    selectionForeground:colors.selection_foreground ?? undefined,
    ...Object.fromEntries(paletteKeys.map((key,index) => [key,colors.palette[index]]))};
}
export function options(settings, colors) {
  return { fontFamily:settings.font_family, fontSize:Math.round(settings.font_size * (settings.zoom_percent ?? 100) / 100), theme:resolvedOptions(colors) ?? {...themes[settings.theme]},
    scrollback:settings.scrollback, cursorBlink:settings.cursor_blink, cursorStyle:settings.cursor_style };
}
// Apply only the newest desired settings after composition/restore has ended.
// No focus calls, synthetic input or terminal recreation belong in this path.
export class Settings {
  constructor(terminal, fit, send, blocked, changed, document, minimap, shortcuts) {
    Object.assign(this,{terminal,fit,send,blocked,changed,document,minimap,shortcuts}); this.pending=null;
  }
  receive(document, bindings, colors) { this.pending={ document, bindings, colors }; this.flush(); }
  flush() {
    if (!this.pending || this.blocked()) return;
    const {document:desired,bindings,colors}=this.pending;
    const settings=desired.terminal, opts=options(settings,colors);
    this.shortcuts?.configure(desired.revision,bindings);
    this.pending=null;
    for (const [key,value] of Object.entries(opts)) this.terminal.options[key]=value;
    const theme=this.terminal.options.theme;
    this.document.body.style.backgroundColor=theme.background;
    this.document.body.style.color=theme.foreground;
    this.document.body.style.colorScheme=(colors?.dark ?? settings.theme === 'dark') ? 'dark' : 'light';
    for (const [name,value] of Object.entries({background:theme.background,foreground:theme.foreground,
      selection:theme.selectionBackground ?? theme.brightBlack,accent:theme.cursor,error:theme.red})) {
      this.document.body.style.setProperty('--theme-'+name,value);
    }
    this.minimap?.configure(settings);
    this.changed(); this.fit();
    const actual=this.terminal.options;
    this.send({type:'settings_applied',revision:desired.revision,rendered_font_size:actual.fontSize,bindings:this.shortcuts?.snapshot() ?? [],
      terminal:{...settings,font_family:actual.fontFamily,font_size:settings.font_size,theme:settings.theme,
        scrollback:actual.scrollback,cursor_blink:actual.cursorBlink,cursor_style:actual.cursorStyle,
        minimap_enabled:settings.minimap_enabled,minimap_width:settings.minimap_width,minimap_opacity:settings.minimap_opacity},
      background:theme.background,foreground:theme.foreground,
      colors:{background:theme.background,foreground:theme.foreground,cursor:theme.cursor,
        selection_background:theme.selectionBackground ?? null,selection_foreground:theme.selectionForeground ?? null,
        palette:paletteKeys.map(key => theme[key]),dark:colors?.dark ?? settings.theme === 'dark'}});
  }
}
