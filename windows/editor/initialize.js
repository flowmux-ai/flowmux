// SPDX-License-Identifier: GPL-3.0-or-later
// This function is installed at document creation, before the Monaco bundle.
(configuration) => {
  "use strict";
  if (window !== window.top || window.location.href !== configuration.url) return;
  const send = window.ipc.postMessage.bind(window.ipc);
  const forward = (message) => send(JSON.stringify({
    ...message,
    signal_id: configuration.signal_id,
    credential: configuration.credential,
  }));
  Object.defineProperty(window, "__flowmuxWindowsEditorBridge", { value: forward });
  window.webkit = { messageHandlers: { flowmuxEditor: {
    postMessage(raw) {
      if (typeof raw !== "string" || raw.length > 6 * 16 * 1024 * 1024 + 65536) return;
      try { forward({ kind: "editor_message", message: JSON.parse(raw) }); } catch (_) { /* invalid bridge input */ }
    },
  } } };
  const shortcuts = new PaneShortcuts();
  const handledShortcuts = new WeakSet();
  const testEvents = new WeakSet();
  let composing = false, settling = false;
  const modifier = (event) => /^(Control|Shift|Alt|Meta)(Left|Right)$/.test(event.code) ||
    ["Control", "Shift", "Alt", "Meta", "AltGraph"].includes(event.key);
  window.addEventListener("compositionstart", () => { composing = true; settling = true; }, true);
  window.addEventListener("compositionend", () => { composing = false; settling = true; }, true);
  window.addEventListener("blur", () => {
    shortcuts.reset(); composing = false; settling = false;
  });
  const shortcutKey = (event) => {
    if (event.type === "keydown" && (event.isComposing || event.keyCode === 229 ||
        ["Dead", "Process", "Unidentified"].includes(event.key))) settling = true;
    const action = shortcuts.event(event, composing || settling ||
      window.__flowmuxWindowsEditorSealed === true || !!document.querySelector("dialog[open]"));
    if (event.type === "keyup" && !modifier(event) && !composing) settling = false;
    if (!action) return;
    handledShortcuts.add(event);
    event.preventDefault();
    event.stopImmediatePropagation();
    if (action !== "consume") forward({ kind: "shortcut", action: action.action,
      chord: action.chord, revision: action.revision });
  };
  // Track releases even while sealed, ahead of the input barrier below.
  window.addEventListener("keydown", shortcutKey, true);
  window.addEventListener("keyup", shortcutKey, true);
  if (configuration.background) {
    const finishTestKey = (event) => {
      if (testEvents.has(event)) { event.preventDefault(); event.stopImmediatePropagation(); }
    };
    window.addEventListener("keydown", finishTestKey, true);
    window.addEventListener("keyup", finishTestKey, true);
  }
  const snapshot = () => ({ revision: shortcuts.revision, bindings: shortcuts.snapshot() });
  const shortcutApi = {
    configure(revision, bindings) {
      shortcuts.configure(revision, bindings);
      const applied = snapshot();
      forward({ kind: "keybindings_applied", ...applied });
      return applied;
    },
    snapshot,
  };
  if (configuration.background) {
    // Uses the production DOM listeners. Synthetic events are not OS IME proof.
    shortcutApi.test = (spec) => {
      const type = spec.type ?? "keydown";
      let event;
      if (type === "compositionstart" || type === "compositionend") {
        event = new CompositionEvent(type, { bubbles: true, data: spec.data ?? "" });
      } else if (type === "blur") {
        event = new Event(type);
      } else if (type === "keydown" || type === "keyup") {
        event = new KeyboardEvent(type, { bubbles: true, cancelable: true,
          code: spec.code ?? "", key: spec.key ?? spec.code ?? "", keyCode: spec.keyCode ?? 0,
          ctrlKey: !!spec.ctrlKey, altKey: !!spec.altKey, shiftKey: !!spec.shiftKey,
          metaKey: !!spec.metaKey, repeat: !!spec.repeat, isComposing: !!spec.isComposing,
          modifierAltGraph: !!spec.altGraph });
      } else { throw new Error("Unsupported editor shortcut test event"); }
      testEvents.add(event);
      window.dispatchEvent(event);
      return handledShortcuts.has(event);
    };
  }
  Object.defineProperty(window, "__flowmuxWindowsEditorShortcuts", { value: Object.freeze(shortcutApi) });
  shortcutApi.configure(configuration.revision, configuration.bindings);
  // Register ahead of the frontend's key handlers. A close barrier seals native
  // edits and dialog/pointer actions until the host explicitly releases it.
  const blockSealedInput = (event) => {
    if (window.__flowmuxWindowsEditorSealed === true) {
      event.preventDefault();
      event.stopImmediatePropagation();
    }
  };
  for (const name of ["keydown", "keyup", "beforeinput", "pointerdown", "pointerup", "click", "dblclick", "contextmenu", "paste", "cut", "drop"]) {
    window.addEventListener(name, blockSealedInput, true);
    document.addEventListener(name, blockSealedInput, true);
  }
  if (configuration.background) {
    // HTMLElement.focus covers Monaco's textarea and explicit dialog controls.
    // showModal has its own native focus steps, so use the nonmodal path too.
    Object.defineProperty(HTMLElement.prototype, "focus", { value() {}, configurable: true });
    const show = HTMLDialogElement.prototype.show;
    Object.defineProperty(HTMLDialogElement.prototype, "showModal", {
      value() { if (!this.open) show.call(this); }, configurable: true,
    });
    Object.defineProperty(window, "focus", { value() {}, configurable: true });
  }
}
