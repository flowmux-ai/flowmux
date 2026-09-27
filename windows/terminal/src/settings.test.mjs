// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { Settings } from './settings.mjs';
const documentFor=(revision,font_size,theme='dark')=>({revision,terminal:{font_size,theme,font_family:'"한글 한 😀", monospace',scrollback:1000,cursor_blink:false,cursor_style:'bar',minimap_enabled:true,minimap_width:40,minimap_opacity:50}});
test('settings defer and coalesce across composition/restore without focus or input',()=>{
  let blocked=true, fits=0, changes=0; const replies=[], maps=[];
  const terminal={options:{fontSize:14},focus(){throw Error('focus stolen');},write(){throw Error('input/output changed');}};
  const document={body:{style:{}}};
  const controller=new Settings(terminal,()=>{ assert.equal(maps.at(-1).minimap_width,40); fits++; },message=>replies.push(message),()=>blocked,()=>changes++,document,{ configure: settings=>maps.push(settings) });
  controller.receive(documentFor('old',20)); controller.receive(documentFor('new',24,'light'));
  assert.equal(terminal.options.fontSize,14); assert.equal(replies.length,0);
  assert.equal(maps.length,0);
  blocked=false; controller.flush(); controller.flush();
  assert.equal(terminal.options.fontSize,24); assert.equal(replies.length,1); assert.equal(replies[0].revision,'new');
  assert.equal(replies[0].terminal.font_family,'"한글 한 😀", monospace');
  assert.equal(replies[0].background,'#ffffff'); assert.equal(document.body.style.color,'#202124');
  assert.equal(fits,1); assert.equal(changes,1);
  assert.equal(maps.length,1); assert.equal(replies[0].terminal.minimap_opacity,50);
});
