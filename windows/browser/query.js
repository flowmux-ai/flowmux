// SPDX-License-Identifier: GPL-3.0-or-later
// Parameters are serialized by Rust. No DOM writes, input events or focus calls.
(args => {
  if(args.op==='count')return document.querySelectorAll(args.selector).length;
  if(typeof window[args.key]!=='function'||window[args.key]()!==args.revision||location.href!==args.url)
    throw new Error('DOM changed since snapshot; take a new snapshot');
  const found=document.querySelectorAll(args.selector);
  if(found.length!==1)throw new Error('snapshot selector no longer resolves uniquely');
  const el=found[0];
  switch(args.op) {
    case 'text': return String(el.innerText||'');
    case 'value': return String(el.value??'');
    case 'attr': return String(el.getAttribute(args.name)??'');
    case 'is_checked': return el.checked===true;
    case 'is_enabled': return !el.matches(':disabled')&&!el.closest('[inert]')&&el.getAttribute('aria-disabled')!=='true';
    case 'is_visible': {
      const box=el.getBoundingClientRect();if(box.width<1||box.height<1)return false;
      for(let node=el;node;node=node.parentElement){const css=getComputedStyle(node);
        if(css.display==='none'||css.visibility==='hidden'||css.visibility==='collapse'||Number(css.opacity)===0)return false;}
      return true;
    }
    default:throw new Error('unknown DOM query');
  }
})
