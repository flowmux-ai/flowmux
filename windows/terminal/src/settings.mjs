// SPDX-License-Identifier: GPL-3.0-or-later
export const themes = Object.freeze({
  dark: { background: '#17191f', foreground: '#e2e5ed', cursor: '#b9c6ff', selectionBackground: '#455483' },
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
  constructor(terminal, fit, send, blocked, changed, document) {
    Object.assign(this,{terminal,fit,send,blocked,changed,document}); this.pending=null;
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
    this.changed(); this.fit();
    const actual=this.terminal.options;
    this.send({type:'settings_applied',revision:desired.revision,
      terminal:{font_family:actual.fontFamily,font_size:actual.fontSize,theme:settings.theme,
        scrollback:actual.scrollback,cursor_blink:actual.cursorBlink,cursor_style:actual.cursorStyle},
      background:theme.background,foreground:theme.foreground});
  }
}
