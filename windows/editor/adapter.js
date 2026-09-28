// SPDX-License-Identifier: GPL-3.0-or-later
// Appended in the shared main.ts lexical scope by the Windows-only build.
// These commands operate on Monaco and the existing UI/host protocol. They do
// not bypass document validation, disk conflict checks, or unsaved-close UI.
// The backend holds LF-normalized text and owns the on-disk EOL encoding.
// Monaco otherwise defaults empty/single-line models to CRLF on Windows. The
// creation event fires before createModel returns and before the shared frontend
// attaches its change listener, so this does not mark a new document dirty.
monaco.editor.onDidCreateModel((model) => model.setEOL(monaco.editor.EndOfLineSequence.LF));
const windowsReadLimit = 128 * 1024;
// The shared frontend debounces cursor/scroll reporting for 300 ms. Flush the
// current view only after content ACKs have advanced its document version, and
// before the host receives the barrier acknowledgment and snapshots the session.
const windowsCompleteFlushRequests = completeFlushRequests;
completeFlushRequests = (error) => {
  if (error === null) reportActiveViewState();
  windowsCompleteFlushRequests(error);
};
let windowsCommandBusy = false;
let windowsHostObserver = null;
let windowsSealedBarrier = null;
let windowsPendingReplacements = 0;
let windowsQuarantined = false;
let windowsDeferredCommandResult = null;
let windowsUiSaveAll = null;
const windowsComposingEditors = new Set();
const windowsObservedEditors = new WeakSet();
function windowsObserveComposition(target) {
  if (target === null || windowsObservedEditors.has(target)) return;
  windowsObservedEditors.add(target);
  target.onDidCompositionStart(() => windowsComposingEditors.add(target));
  target.onDidCompositionEnd(() => windowsComposingEditors.delete(target));
  target.onDidDispose(() => windowsComposingEditors.delete(target));
}
windowsObserveComposition(editor);
function windowsApplySeal() {
  if (windowsSealedBarrier !== null || windowsUiSaveAll !== null || windowsPendingReplacements > 0 || windowsQuarantined) {
    window.__flowmuxWindowsEditorSealed = true;
    editor.updateOptions({ readOnly: true });
    diffEditor?.getModifiedEditor().updateOptions({ readOnly: true });
  }
}
function windowsRefreshSeal() {
  if (windowsSealedBarrier !== null || windowsUiSaveAll !== null || windowsPendingReplacements > 0 || windowsQuarantined) {
    windowsApplySeal();
    return;
  }
  window.__flowmuxWindowsEditorSealed = false;
  const document = activeDocumentId === null ? undefined : documents.get(activeDocumentId);
  editor.updateOptions({ readOnly: document?.payload.readOnly ?? false });
  diffEditor?.getModifiedEditor().updateOptions({ readOnly: document?.payload.readOnly ?? false });
}
const windowsPostToHost = postToHost;
postToHost = (message) => {
  const replacement = (message.type === "conflict_action_requested" &&
    ["keep_mine", "reload_from_disk"].includes(message.action)) ||
    (message.type === "recovery_decision" && message.choice === "restore") ||
    ["save_requested", "save_as_requested", "close_requested", "discard_close_requested"].includes(message.type);
  if (replacement) {
    // Also covers normal UI actions: Windows performs their disk work on a
    // worker, so another user edit must not race its eventual replacement.
    // Save All may send several requests in one event, or send its next request
    // from an acknowledgment microtask before the prior native completion. Keep
    // the input shield until every corresponding worker response has arrived.
    windowsRequire(!windowsQuarantined, "Editor synchronization failed; editing is unavailable.");
    windowsPendingReplacements += 1;
    windowsApplySeal();
  }
  windowsPostToHost(message);
};
function windowsCommandResult(id, result, error) {
  if (windowsPendingReplacements > 0) {
    windowsDeferredCommandResult = { id, result, error };
    return;
  }
  window.__flowmuxWindowsEditorBridge({ kind: "command_result", id, result, error });
  windowsCommandBusy = false;
}
const windowsReceive = window.flowmuxEditorHost.receive;
window.flowmuxEditorHost.receive = (message) => {
  windowsReceive(message);
  windowsObserveComposition(diffEditor?.getModifiedEditor() ?? null);
  windowsApplySeal();
  if (isHostMessage(message) && message.surfaceId === surfaceId) {
    windowsHostObserver?.(message);
    windowsFinishUiSaveAll();
  }
};

function windowsRequire(condition, reason) {
  if (!condition) throw new Error(reason);
}

function windowsValidString(value, limit) {
  if (typeof value !== "string" || value.length > limit || utf8ByteLength(value) > limit) return false;
  // The Rust JSON decoder rejects unpaired UTF-16 surrogates; reject them before
  // editing the model rather than silently replacing them during UTF-8 transport.
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code >= 0xd800 && code <= 0xdbff) {
      const low = value.charCodeAt(++index);
      if (!(low >= 0xdc00 && low <= 0xdfff)) return false;
    } else if (code >= 0xdc00 && code <= 0xdfff) return false;
  }
  return true;
}

function windowsRead() {
  const document = activeDocumentId === null ? undefined : documents.get(activeDocumentId);
  const result = {
    document_id: document?.payload.id ?? null,
    path: document?.payload.relativePath ?? null,
    encoding: document?.payload.encoding ?? null,
    eol: document?.payload.eol ?? null,
    dirty: document?.payload.dirty ?? false,
    read_only: document?.payload.readOnly ?? false,
    external_change: document?.payload.externalChange ?? false,
    active_version: document?.payload.version ?? null,
    language: document?.model.getLanguageId() ?? null,
    document_focused: window.document.hasFocus(),
    open_documents: documents.size,
    content: null,
    content_truncated: false,
    total_bytes: 0,
    diff_visible: diffDocumentId !== null,
    close_confirmation: closeDialog.open,
    recovery_available: recoveryDialogDocumentId !== null,
    composing: windowsComposingEditors.size !== 0,
    sealed: windowsSealedBarrier !== null || windowsUiSaveAll !== null,
    replacement_pending: windowsPendingReplacements > 0,
    quarantined: windowsQuarantined,
  };
  if (document !== undefined) {
    const content = document.model.getValue();
    result.total_bytes = utf8ByteLength(content);
    result.content_truncated = result.total_bytes > windowsReadLimit;
    if (!result.content_truncated) result.content = content;
    else {
      let bytes = 0;
      let units = 0;
      for (const character of content) {
        const size = utf8ByteLength(character);
        if (bytes + size > windowsReadLimit) break;
        bytes += size;
        units += character.length;
      }
      result.content = content.slice(0, units);
    }
  }
  return result;
}

function windowsDocument() {
  const document = activeDocumentId === null ? undefined : documents.get(activeDocumentId);
  windowsRequire(document !== undefined, "There is no active document.");
  return document;
}

function windowsWait(start, completed) {
  return new Promise((resolve, reject) => {
    const finish = (error) => {
      clearTimeout(timer);
      windowsHostObserver = null;
      if (error) reject(error);
      else resolve();
    };
    const inspect = (message) => {
      try {
        if (completed(message)) finish(null);
      } catch (error) { finish(error); }
    };
    const timer = setTimeout(() => finish(new Error("Editor response timed out; the operation may have completed.")), 8000);
    windowsHostObserver = inspect;
    try {
      start();
      inspect(null);
    } catch (error) { finish(error); }
  });
}

async function windowsSync(document) {
  await windowsWait(() => {
    windowsRequire(syncDocument(document), document.saveError ?? "Cannot synchronize the document.");
  }, () => !document.pendingChanges && document.outstandingChanges.size === 0);
}

async function windowsSave(document) {
  windowsRequire(!document.payload.readOnly, "The document is read-only.");
  await windowsSync(document);
  if (!document.payload.dirty) return;
  const sequence = document.changeSequence;
  await windowsWait(() => requestSave(document.payload.id), (message) => {
    if (message?.documentId !== document.payload.id || message.changeSequence !== sequence) return false;
    if (message.type === "save_failed") throw new Error(message.reason);
    return message.type === "save_completed";
  });
}

async function windowsSaveAll() {
  const queued = [...documents.values()].filter((document) => document.payload.dirty && !document.payload.readOnly);
  // Keep the pending content, but prevent each queued model's debounce timer
  // from filling the bounded native worker before its turn in this batch.
  for (const document of queued) clearChangeTimer(document);
  let saved = 0;
  for (const document of queued) {
    if (documents.get(document.payload.id) !== document || !document.payload.dirty) continue;
    try {
      await windowsSave(document);
      // The save ACK arrives before the native response-completion script. Do
      // not dispatch another large payload until that response has drained.
      await windowsWait(() => {}, () => windowsPendingReplacements === 0);
      saved += 1;
    } catch (error) {
      throw new Error(`Could not save ${document.payload.name ?? document.payload.relativePath}: ${error.message ?? error}`);
    }
  }
  return saved;
}

function windowsFinishUiSaveAll() {
  if (windowsUiSaveAll === null || !windowsUiSaveAll.finished || windowsPendingReplacements > 0 ||
      [...documents.values()].some((document) => document.outstandingChanges.size > 0)) return;
  windowsUiSaveAll = null;
  windowsCommandBusy = false;
  windowsRefreshSeal();
}

function windowsSaveAllError(error) {
  const document = activeDocumentId === null ? undefined : documents.get(activeDocumentId);
  if (document !== undefined) {
    document.saveError = `Save All stopped: ${String(error.message ?? error).slice(0, 4096)}`;
    renderState();
  }
}

requestSaveAll = () => {
  try {
    windowsRequire(!windowsCommandBusy, "Another editor command is pending.");
    windowsRequire(windowsSealedBarrier === null && windowsPendingReplacements === 0 && !windowsQuarantined, "The editor is waiting for another operation.");
    windowsRequire(windowsComposingEditors.size === 0, "Finish text composition before saving the documents.");
  } catch (error) {
    windowsSaveAllError(error);
    return;
  }
  // Reserve the same observer used by CLI commands for the entire UI batch.
  // The input shield prevents normal actions, and CLI dispatch rejects busy.
  windowsCommandBusy = true;
  windowsUiSaveAll = { finished: false };
  windowsApplySeal();
  void windowsSaveAll().catch(windowsSaveAllError).finally(() => {
    windowsUiSaveAll.finished = true;
    // A timeout stops the batch without unlocking any outstanding disk work.
    // Its later host acknowledgment/completion retries this release.
    windowsFinishUiSaveAll();
  });
};

async function windowsPerform(command) {
  windowsRequire(windowsPendingReplacements === 0, "An editor replacement is still pending.");
  if (command.action === "read") return windowsRead();
  windowsRequire(!windowsQuarantined, "Editor synchronization failed; editing is unavailable.");
  windowsRequire(windowsSealedBarrier === null, "The editor is waiting for a close decision.");
  windowsRequire(windowsComposingEditors.size === 0, "Finish text composition before changing the document.");
  // The host owns release after its final ordered worker snapshot, including on
  // failure. A JavaScript timeout must not reopen editing before a late disk or
  // recovery response can replace the model. Normal UI dialogs remain intact.
  windowsSealedBarrier = command.id;
  window.__flowmuxWindowsEditorSealed = true;
  windowsApplySeal();
  if (command.action === "save_all") {
    const saved = await windowsSaveAll();
    return { ...windowsRead(), saved };
  }
  const document = windowsDocument();
  switch (command.action) {
    case "replace_text":
      windowsRequire(!document.payload.readOnly, "The document is read-only.");
      windowsRequire(windowsValidString(command.text, maxDocumentBytes), "Replacement text is invalid or exceeds the document limit.");
      // executeEdits and pushUndoStop refuse read-only editor widgets. Permit
      // only this synchronous programmatic edit while input capture stays sealed;
      // no task or user event can interleave before the finally restores the seal.
      editor.updateOptions({ readOnly: false });
      try {
        editor.pushUndoStop();
        windowsRequire(editor.executeEdits("flowmux-windows", [{ range: document.model.getFullModelRange(), text: command.text, forceMoveMarkers: true }]), "Monaco rejected the edit.");
        editor.pushUndoStop();
      } finally { windowsApplySeal(); }
      await windowsSync(document);
      break;
    case "undo":
    case "redo":
      windowsRequire(!document.payload.readOnly, "The document is read-only.");
      await document.model[command.action]();
      await windowsSync(document);
      break;
    case "save":
      await windowsSave(document);
      break;
    case "save_as":
      windowsRequire(windowsValidString(command.path, 16 * 1024) && command.path.trim().length > 0 && !/[\u0000-\u001f]/.test(command.path), "A valid workspace-relative path is required.");
      await windowsSync(document);
      showSaveAsDialog(document.payload.id);
      saveAsPath.value = command.path;
      saveAsOverwrite = command.overwrite === true;
      await windowsWait(() => submitSaveAs(), (message) => {
        if (message?.type === "save_as_failed" && message.documentId === document.payload.id) throw new Error(message.reason);
        return message?.type === "save_as_completed" && message.document.id === document.payload.id;
      });
      break;
    case "close_document":
      await windowsSync(document);
      if (document.payload.dirty) {
        requestCloseActiveDocument();
        return { ...windowsRead(), closed: false, confirmation_required: true };
      }
      await windowsWait(() => requestCloseActiveDocument(), () => !documents.has(document.payload.id));
      return { ...windowsRead(), ok: true, closed: true, confirmation_required: false };
    case "discard_document":
      await windowsSync(document);
      showCloseDialog(document);
      await windowsWait(() => discardCloseDialogDocument(), () => !documents.has(document.payload.id));
      return { ...windowsRead(), ok: true, closed: true, confirmation_required: false };
    case "compare":
    case "keep_mine":
    case "reload": {
      windowsRequire(document.payload.externalChange, "The document has no reported disk conflict.");
      await windowsSync(document);
      const action = command.action === "reload" ? "reload_from_disk" : command.action;
      await windowsWait(() => requestConflictAction(action), (message) => {
        if (message?.documentId === document.payload.id && message.type === "conflict_action_failed") throw new Error(message.reason);
        if (action === "compare") return message?.type === "show_diff" && message.documentId === document.payload.id;
        return message?.type === "replace_document" && message.document.id === document.payload.id;
      });
      break;
    }
    case "recover":
      windowsRequire(recoveryDialogDocumentId === document.payload.id, "There is no recovery proposal for the active document.");
      await windowsWait(() => resolveRecovery("restore"), (message) => message?.type === "replace_document" && message.document.id === document.payload.id);
      break;
    case "discard_recovery":
      windowsRequire(recoveryDialogDocumentId === document.payload.id, "There is no recovery proposal for the active document.");
      // Discard has no shared HostMessage acknowledgment. The native caller's
      // subsequent barrier and worker snapshot confirm the recovery operation.
      resolveRecovery("discard");
      break;
    default:
      throw new Error("Unknown editor command.");
  }
  return windowsRead();
}

window.flowmuxWindowsEditor = Object.freeze({
  barrier(id, seal) {
    try {
      windowsRequire(Number.isSafeInteger(id) && id >= 0 && typeof seal === "boolean", "Invalid editor barrier.");
      windowsRequire(!windowsCommandBusy, "An editor command is still pending.");
      windowsRequire(windowsPendingReplacements === 0, "An editor replacement is still pending.");
      windowsRequire(windowsComposingEditors.size === 0, "Finish text composition before saving or closing the editor.");
      windowsRequire(windowsSealedBarrier === null || windowsSealedBarrier === id, "Another editor close is pending.");
      if (seal) {
        windowsSealedBarrier = id;
        window.__flowmuxWindowsEditorSealed = true;
        windowsApplySeal();
      }
      pendingFlushRequests.add(id);
      flushChangesForHost();
    } catch (error) {
      window.__flowmuxWindowsEditorBridge({ kind: "barrier_error", id, error: String(error.message ?? error).slice(0, 4096) });
    }
  },
  releaseBarrier(id) {
    // Zero is the native close-failure escape hatch, never a user command.
    if (id !== 0 && windowsSealedBarrier !== id) return;
    if (windowsSealedBarrier === null) return;
    windowsSealedBarrier = null;
    windowsRefreshSeal();
  },
  replacementCompleted() {
    if (windowsPendingReplacements === 0) return;
    windowsPendingReplacements -= 1;
    windowsRefreshSeal();
    windowsHostObserver?.(null);
    windowsFinishUiSaveAll();
    if (windowsPendingReplacements === 0 && windowsDeferredCommandResult !== null) {
      const { id, result, error } = windowsDeferredCommandResult;
      windowsDeferredCommandResult = null;
      windowsCommandResult(id, result, error);
    }
  },
  quarantine() {
    windowsQuarantined = true;
    windowsApplySeal();
  },
  command(command) {
    const id = command?.id;
    const validId = (typeof id === "number" && Number.isSafeInteger(id) && id >= 0) ||
      (typeof id === "string" && /^[A-Za-z0-9_.-]{1,128}$/.test(id));
    if (!validId) return;
    const reply = (result, error) => window.__flowmuxWindowsEditorBridge({ kind: "command_result", id, result, error });
    if (windowsCommandBusy) {
      reply(null, "Another editor command is pending.");
      return;
    }
    try {
      windowsRequire(command !== null && typeof command === "object" && !Array.isArray(command), "Invalid editor command.");
      windowsRequire(Object.keys(command).every((key) => ["id", "action", "text", "path", "overwrite"].includes(key)), "Unknown editor command field.");
      windowsRequire(typeof command.action === "string" && command.action.length <= 32, "Invalid editor action.");
      windowsRequire(command.overwrite === undefined || typeof command.overwrite === "boolean", "Overwrite must be an explicit boolean.");
      windowsRequire(command.text === undefined || windowsValidString(command.text, maxDocumentBytes), "Invalid or oversized command text.");
      windowsRequire(command.path === undefined || windowsValidString(command.path, 16 * 1024), "Invalid or oversized command path.");
    } catch (error) {
      reply(null, String(error.message ?? error).slice(0, 4096));
      return;
    }
    windowsCommandBusy = true;
    Promise.resolve().then(() => windowsPerform(command)).then(
      (result) => windowsCommandResult(id, result, null),
      (error) => windowsCommandResult(id, null, String(error.message ?? error).slice(0, 4096)),
    );
  },
});
