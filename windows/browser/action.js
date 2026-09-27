// SPDX-License-Identifier: GPL-3.0-or-later
// DOM-level actions, adapted from flowmux-browser scripts. No OS input or retry.
(args => {
  if (typeof window[args.key] !== 'function' || window[args.key]() !== args.revision || location.href !== args.url)
    throw new Error('DOM changed since snapshot; take a new snapshot');
  const found = document.querySelectorAll(args.selector);
  if (found.length !== 1) throw new Error('snapshot selector no longer resolves uniquely');
  const el = found[0];
  const enabled = () => {
    if (!el.isConnected || el.matches(':disabled') || el.closest('[inert],[aria-disabled="true"]'))
      throw new Error('element is detached or disabled');
  };
  const events = () => {
    el.dispatchEvent(new Event('input', {bubbles:true, composed:true}));
    el.dispatchEvent(new Event('change', {bubbles:true}));
  };
  const textControl = () => {
    const input = el instanceof HTMLInputElement;
    if (!(input || el instanceof HTMLTextAreaElement) ||
        (input && ['file','checkbox','radio','button','submit','reset','image','hidden'].includes(el.type)))
      throw new Error('fill requires a value input or textarea; use select/check for other controls');
    if (el.readOnly) throw new Error('element is read-only');
    enabled();
    return input ? HTMLInputElement.prototype : HTMLTextAreaElement.prototype;
  };
  switch (args.op) {
    case 'click':
      enabled();
      if (typeof el.click !== 'function') throw new Error('element does not support click');
      el.click(); break;
    case 'dblclick':
      enabled();
      el.dispatchEvent(new MouseEvent('dblclick', {bubbles:true,cancelable:true,composed:true,view:window,detail:2,button:0})); break;
    case 'hover':
      enabled();
      el.dispatchEvent(new MouseEvent('mouseenter', {bubbles:false,view:window}));
      el.dispatchEvent(new MouseEvent('mouseover', {bubbles:true,cancelable:true,composed:true,view:window})); break;
    case 'focus':
      enabled();
      if (typeof el.focus !== 'function') throw new Error('element is not focusable');
      el.focus({preventScroll:true});
      if (document.activeElement !== el) throw new Error('element did not receive DOM focus');
      break;
    case 'blur':
      if (typeof el.blur !== 'function') throw new Error('element does not support blur');
      el.blur();
      if (document.activeElement === el) throw new Error('element retained DOM focus');
      break;
    case 'scroll':
      el.scrollIntoView({block:'center',inline:'nearest',behavior:'instant'});
      window.scrollBy({left:args.x,top:args.y,behavior:'instant'}); break;
    case 'fill': {
      textControl();
      if (el.value === args.value) break;
      if (!el.dispatchEvent(new InputEvent('beforeinput', {bubbles:true,composed:true,cancelable:true,inputType:'insertReplacementText',data:args.value,isComposing:false})))
        throw new Error('beforeinput was canceled; fill was not applied');
      const current=document.querySelectorAll(args.selector);
      if(current.length!==1 || current[0]!==el) throw new Error('element changed during beforeinput');
      const prototype=textControl();
      const previous=el.value;
      Object.getOwnPropertyDescriptor(prototype,'value').set.call(el,args.value);
      if(el.value!==previous) events();
      break;
    }
    case 'select': {
      enabled();
      if (!(el instanceof HTMLSelectElement)) throw new Error('select requires a select element');
      const options=Array.from(el.options);
      const option=options.find(option=>option.value===args.value) || options.find(option=>option.textContent.trim()===args.value);
      if (!option) throw new Error('option not found');
      if (option.disabled || option.closest('optgroup[disabled]')) throw new Error('option is disabled');
      if (!option.selected) {Object.getOwnPropertyDescriptor(HTMLOptionElement.prototype,'selected').set.call(option,true);events();}
      break;
    }
    case 'check': case 'uncheck': {
      enabled();
      if (!(el instanceof HTMLInputElement) || !['checkbox','radio'].includes(el.type)) throw new Error('element is not checkable');
      if (args.op==='uncheck' && el.type==='radio') throw new Error('radios cannot be unchecked individually');
      const checked=args.op==='check';
      if (el.checked!==checked) {Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'checked').set.call(el,checked);events();}
      break;
    }
    default: throw new Error('unknown browser action');
  }
  return 'ok';
})
