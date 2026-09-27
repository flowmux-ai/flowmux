// SPDX-License-Identifier: GPL-3.0-or-later
(args => {
  switch (args.kind) {
    case 'selector': return Boolean(document.querySelector(args.value));
    case 'text': return Boolean(document.body && document.body.innerText.includes(args.value));
    case 'url': return location.href.includes(args.value);
    case 'ready_state': return document.readyState === args.value;
    case 'js': {
      let predicate;
      // Only a compilation failure selects the body form. A runtime exception
      // must never execute the predicate a second time in the same poll.
      try { predicate = Function('return (' + args.value + '\n)'); }
      catch (error) { if (!(error instanceof SyntaxError)) throw error; predicate = Function(args.value); }
      let result = predicate();
      if (typeof result === 'function') result = result();
      if (result && typeof result.then === 'function') throw new Error('asynchronous wait predicates are not supported');
      return Boolean(result);
    }
    default: throw new Error('unknown wait condition');
  }
})
