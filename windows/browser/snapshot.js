// SPDX-License-Identifier: GPL-3.0-or-later
// Adapted from flowmux-browser's non-mutating SNAPSHOT_JS. Windows adds bounded
// traversal, unique selectors, Unicode-safe excerpts and a DOM revision stamp.
(args => {
  const MAX_NODES = 10000, MAX_REFS = 2048;
  const segments = typeof Intl.Segmenter === 'function' ? new Intl.Segmenter('ko',{granularity:'grapheme'}) : null;
  const clip = (value, limit) => {
    const text = String(value ?? '');
    if (text.length <= limit) return text;
    if (segments) {
      let end=0;
      for (const part of segments.segment(text)) {
        const next=part.index+part.segment.length;if(next>limit)break;end=next;
      }
      return text.slice(0,end);
    }
    let end = Math.min(text.length, limit);
    if (end < text.length && end > 0 && /[\uD800-\uDBFF]/.test(text[end - 1]) && /[\uDC00-\uDFFF]/.test(text[end])) end--;
    return text.slice(0, end);
  };
  if (!Object.prototype.hasOwnProperty.call(window, args.key)) {
    let revision = 0;
    const observer = new MutationObserver(records => { if (records.length) revision++; });
    observer.observe(document, {subtree:true, childList:true, attributes:true, characterData:true});
    // An observer/property does not mutate the DOM or dispatch page input events.
    Object.defineProperty(window, args.key, {value: () => {
      if (observer.takeRecords().length) revision++;
      return revision;
    }});
  }
  const revision = window[args.key]();
  const interactive = new Set(['button','link','textbox','checkbox','radio','combobox','listbox',
    'menuitem','menuitemcheckbox','menuitemradio','option','searchbox','slider','spinbutton','switch','tab','treeitem']);
  const content = new Set(['heading','cell','listitem','article','region','main','navigation']);
  function role(el) {
    const explicit = el.getAttribute('role'); if (explicit) return explicit;
    const tag = el.tagName.toLowerCase();
    if (tag === 'a' && el.hasAttribute('href')) return 'link';
    if (tag === 'button' || tag === 'summary') return 'button';
    if (tag === 'select') return 'combobox';
    if (tag === 'textarea') return 'textbox';
    if (tag === 'input') {
      const type = el.type;
      if (type === 'checkbox' || type === 'radio') return type;
      if (['submit','button','reset'].includes(type)) return 'button';
      return type === 'search' ? 'searchbox' : 'textbox';
    }
    if (/^h[1-6]$/.test(tag)) return 'heading';
    if (tag === 'li') return 'listitem';
    if (tag === 'nav') return 'navigation';
    return tag;
  }
  function visible(el) {
    const box=el.getBoundingClientRect(); if (box.width < 1 || box.height < 1) return false;
    for (let node=el; node; node=node.parentElement) {
      const css=getComputedStyle(node);
      if (css.display==='none'||css.visibility==='hidden'||css.visibility==='collapse'||Number(css.opacity)===0) return false;
    }
    return true;
  }
  function selector(el) {
    const unique = value => { const found=document.querySelectorAll(value); return found.length===1 && found[0]===el; };
    if (el.id) { const id='#'+CSS.escape(el.id); if (id.length<=4096 && unique(id)) return id; }
    const parts=[];let node=el;
    for (let depth=0;node && depth<64;depth++,node=node.parentElement) {
      const tag=CSS.escape(node.tagName.toLowerCase());let nth=1;
      for(let prev=node.previousElementSibling;prev;prev=prev.previousElementSibling) if(prev.tagName===node.tagName) nth++;
      parts.unshift(tag+':nth-of-type('+nth+')');
      const path=parts.join(' > ');if(path.length>4096)return null;
      if(unique(path))return path;
    }
    return null;
  }
  function name(el) {
    let value=el.getAttribute('aria-label');
    if (!value && el.getAttribute('aria-labelledby')) value=el.getAttribute('aria-labelledby').split(/\s+/).slice(0,32).map(id=>document.getElementById(id)?.textContent||'').join(' ');
    if (!value && el.labels?.length) value=Array.from(el.labels).slice(0,32).map(label=>label.innerText||'').join(' ');
    value=value||el.getAttribute('alt')||el.getAttribute('title')||el.getAttribute('placeholder')||(el.innerText||'').trim();
    return clip(value.replace(/\n+/g,' '),120);
  }
  const refs={};let nodes=0,omitted=0,frames=0,count=0;
  const walker=document.createTreeWalker(document,NodeFilter.SHOW_ELEMENT);
  for(let el=walker.nextNode();el;el=walker.nextNode()) {
    if(++nodes>MAX_NODES)throw new Error('snapshot exceeds 10000 elements');
    if(el.tagName==='IFRAME'||el.tagName==='FRAME')frames++;
    const r=role(el);if((!interactive.has(r)&&!content.has(r))||!visible(el))continue;
    if(count>=MAX_REFS)throw new Error('snapshot exceeds 2048 refs');
    const path=selector(el);if(!path){omitted++;continue;}
    refs['e'+(++count)]={role:r,name:name(el),selector:path};
  }
  if(window[args.key]()!==revision)throw new Error('DOM changed during snapshot; take a new snapshot');
  return {markdown:'',refs,page:{url:location.href,title:clip(document.title,1000),ready_state:document.readyState,
    text:clip(document.body?.innerText||'',4000)},revision,node_count:nodes,omitted_refs:omitted,frame_count:frames};
})
