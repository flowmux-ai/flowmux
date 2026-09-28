// SPDX-License-Identifier: GPL-3.0-or-later
export const themes = Object.freeze({
  dark: { background: '#282c34', foreground: '#ffffff', cursor: '#b9c6ff', selectionBackground: '#455483',
    black:'#5c6370', red:'#cc6666', green:'#b5bd68', yellow:'#f0c674', blue:'#81a2be', magenta:'#b294bb', cyan:'#8abeb7', white:'#c5c8c6',
    brightBlack:'#7f848e', brightRed:'#d54e53', brightGreen:'#b9ca4a', brightYellow:'#e7c547', brightBlue:'#7aa6da', brightMagenta:'#c397d8', brightCyan:'#70c0b1', brightWhite:'#eaeaea' },
  light: { background: '#ffffff', foreground: '#202124', cursor: '#202124', selectionBackground: '#b5cff7',
    black:'#202124', red:'#b42318', green:'#18733b', yellow:'#805500', blue:'#185abc', magenta:'#8f2db3', cyan:'#007580', white:'#c5c7cb',
    brightBlack:'#686b70', brightRed:'#c5221f', brightGreen:'#188038', brightYellow:'#956500', brightBlue:'#1967d2', brightMagenta:'#a142b8', brightCyan:'#00838f', brightWhite:'#f1f3f4' },
});
export function options(settings) {
  return { fontFamily:settings.font_family, fontSize:settings.font_size, theme:{...themes[settings.theme]},
    scrollback:settings.scrollback, cursorBlink:settings.cursor_blink, cursorStyle:settings.cursor_style };
}
// Apply only the newest desired settings after composition/restore has ended.
// No focus calls, synthetic input or terminal recreation belong in this path.
export class Settings {
  constructor(terminal, fit, send, blocked, changed, document, minimap) {
    Object.assign(this,{terminal,fit,send,blocked,changed,document,minimap}); this.pending=null;
  }
  receive(document) { this.pending=document; this.flush(); }
  flush() {
    if (!this.pending || this.blocked()) return;
    const desired=this.pending; this.pending=null;
    const settings=desired.terminal, opts=options(settings);
    for (const [key,value] of Object.entries(opts)) this.terminal.options[key]=value;
    const theme=this.terminal.options.theme;
    this.document.body.style.backgroundColor=theme.background;
    this.document.body.style.color=theme.foreground;
    this.minimap?.configure(settings);
    this.changed(); this.fit();
    const actual=this.terminal.options;
    this.send({type:'settings_applied',revision:desired.revision,
      terminal:{font_family:actual.fontFamily,font_size:actual.fontSize,theme:settings.theme,
        scrollback:actual.scrollback,cursor_blink:actual.cursorBlink,cursor_style:actual.cursorStyle,
        minimap_enabled:settings.minimap_enabled,minimap_width:settings.minimap_width,minimap_opacity:settings.minimap_opacity},
      background:theme.background,foreground:theme.foreground});
  }
}
