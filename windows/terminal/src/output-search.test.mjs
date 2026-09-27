// SPDX-License-Identifier: GPL-3.0-or-later
import test from 'node:test';
import assert from 'node:assert/strict';
import { OutputSearch, matchRange } from './output-search.mjs';

function fixture(texts, cols = 40, pause = async () => {}) {
  const lines = texts.map(value => typeof value === 'string' ? {text:value, wrapped:false} : value);
  const replies = [], markers = [];
  const buffer = { type:'normal', baseY:0, cursorY:0,
    get length() { return lines.length; },
    getLine(row) {
      const source=lines[row]; if (!source) return undefined;
      const cells=[];
      for (const char of source.text) {
        if (/\p{M}/u.test(char) && cells.length) { cells.at(-1).text+=char; continue; }
        const width=/[한글🙂]/u.test(char) ? 2 : 1;
        cells.push({text:char,width}); if (width===2) cells.push({text:'',width:0});
      }
      while(cells.length<cols)cells.push({text:'',width:1});
      return {length:cols,isWrapped:source.wrapped,
        translateToString: trim => {const text=cells.filter(c=>c.width).map(c=>c.text||' ').join('');return trim?text.trimEnd():text;},
        getCell: col => cells[col] ? {getWidth:()=>cells[col].width,getChars:()=>cells[col].text,getCode:()=>cells[col].text.codePointAt(0)||0} : undefined,
      };
    },
  };
  const terminal = { cols, rows:3, buffer:{active:buffer}, onResize() {},
    registerMarker(offset) {const marker={line:offset,isDisposed:false,dispose(){this.isDisposed=true;}};markers.push(marker);return marker;},
    select(col,row,size) {this.selection={col,row,size};}, scrollToLine(row){this.viewport=row;}, getSelection:()=> 'selected',
  };
  const search = new OutputSearch(terminal, message=>replies.push(message), pause);
  const scan = overrides => search.scan({search:'a',query:'needle',match_case:true,skip:0,limit:500,...overrides}, 7);
  const open = overrides => search.open({search:'a',request:'open',hit:0,commit:true,...overrides}, 7);
  return {terminal,buffer,lines,replies,markers,search,scan,open};
}
test('literal case folding maps expanded characters back to the original text', () => {
  assert.deepEqual(matchRange('İZ 한글','z',false),[1,2]);
  assert.equal(matchRange('Case','case',true),null);
  assert.deepEqual(matchRange('[a-z]','[a-z]',false),[0,5]);
});
test('wide soft-wrap padding is excluded from matching and mapped back to cells', async () => {
  const f=fixture(['abcd',{text:'한글',wrapped:true}],5);
  await f.scan({query:'d한'});
  assert.equal(f.replies.at(-1).total,1);
  await f.open();
  assert.equal(f.replies.at(-1).error,null);
  assert.deepEqual(f.terminal.selection,{col:3,row:0,size:4});
});
test('500-line pages count all matches without retaining markers for omitted results', async () => {
  const f=fixture(Array.from({length:503},(_,i)=>`needle ${i}`));
  await f.scan({skip:500});
  assert.equal(f.replies.at(-1).total,503);
  assert.equal(f.replies.at(-1).hits.length,3);
  assert.equal(f.markers.length,3);
  assert.equal(f.replies.at(-1).hits[0].line,500);
});
test('append is allowed; rewritten, trimmed, resized and cancelled references are rejected', async () => {
  const f=fixture(['needle original']); await f.scan();
  await f.open({commit:false});
  assert.equal(f.terminal.selection,undefined,'preflight must not change selection');
  assert.equal(f.replies.at(-1).selected,false);
  f.lines.push({text:'new output',wrapped:false}); f.search.changed(); await f.open();
  assert.equal(f.replies.at(-1).error,null);
  f.lines[0].text='needle rewritten'; f.search.changed(); await f.open();
  assert.match(f.replies.at(-1).error,/changed/);
  f.lines[0].text='needle original'; f.markers[0].dispose(); await f.open();
  assert.match(f.replies.at(-1).error,/scrollback/);
  await f.scan(); f.terminal.cols++; await f.open();
  assert.match(f.replies.at(-1).error,/changed/);
  f.search.cancel('a'); await f.open();
  assert.match(f.replies.at(-1).error,/expired/);
});
test('cancel or changed output during yielded scanning cannot publish mixed results', async () => {
  const f=fixture(Array(260).fill('needle'));
  f.search.pause=async()=>f.search.cancel('a'); await f.scan();
  assert.equal(f.replies.length,0);
  assert.ok(f.markers.every(marker=>marker.isDisposed));
  f.search.pause=async()=>f.search.changed(); await f.scan();
  assert.equal(f.replies.at(-1).total,0);
  assert.match(f.replies.at(-1).error,/changed/);
});
