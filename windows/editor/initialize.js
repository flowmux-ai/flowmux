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
