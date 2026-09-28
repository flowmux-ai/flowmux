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
  if (error === null && windowsDiskRefresh !== null && pendingFlushRequests.has(windowsDiskRefresh.id)) {
    windowsDiskRefresh.phase = "flushed";
  }
  windowsCompleteFlushRequests(error);
};
let windowsCommandBusy = false;
let windowsHostObserver = null;
let windowsSealedBarrier = null;
let windowsPendingReplacements = 0;
let windowsQuarantined = false;
let windowsDeferredCommandResult = null;
let windowsUiSaveAll = null;
let windowsDiskRefresh = null;
const windowsComposingEditors = new Set();
const windowsObservedEditors = new WeakSet();
let windowsPendingTheme = null;
let windowsThemeFlushQueued = false;
function windowsFlushTheme() {
  if (windowsPendingTheme === null || windowsComposingEditors.size !== 0) return;
  const colors = windowsPendingTheme;
  windowsPendingTheme = null;
  try {
    // Shared applyAppearance also refreshes font/minimap options. Keep the
    // editor's current values, including local minimap toggles and font zoom.
    const targets = [editor, diffEditor?.getModifiedEditor()].filter(Boolean);
    const fonts = targets.map((target) => ({ target,
      fontFamily: target.getOption(monaco.editor.EditorOption.fontFamily),
      fontSize: target.getOption(monaco.editor.EditorOption.fontSize) }));
    applyAppearance({ ...appliedAppearance, ...colors, minimapEnabled, fontSize: editorFontSize });
    for (const { target, fontFamily, fontSize } of fonts) {
      if (target.getOption(monaco.editor.EditorOption.fontFamily) !== fontFamily ||
          target.getOption(monaco.editor.EditorOption.fontSize) !== fontSize) {
        target.updateOptions({ fontFamily, fontSize });
      }
    }
  } catch (error) {
    window.__flowmuxWindowsEditorBridge({ kind: "theme_error", error: String(error.message ?? error).slice(0, 4096) });
  }
}
function windowsQueueThemeFlush() {
  if (windowsThemeFlushQueued) return;
  windowsThemeFlushQueued = true;
  queueMicrotask(() => { windowsThemeFlushQueued = false; windowsFlushTheme(); });
}
function windowsObserveComposition(target) {
  if (target === null || windowsObservedEditors.has(target)) return;
  windowsObservedEditors.add(target);
  target.onDidCompositionStart(() => windowsComposingEditors.add(target));
  const finished = () => { windowsComposingEditors.delete(target); windowsQueueThemeFlush(); };
  target.onDidCompositionEnd(finished);
  target.onDidDispose(finished);
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
  if (message.type === "search_result_open_requested") {
    windowsSearchError("Search results expired; run the search again.");
    return; // Windows opens only a native-retained token/index, never a UI path.
  }
  if (["quick_open_requested", "workspace_search_requested", "search_cancelled"].includes(message.type)) {
    windowsSearchRetained = null;
  }
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
  const rootStyle = window.getComputedStyle(window.document.documentElement);
  const background = window.document.querySelector(".monaco-editor-background");
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
    selection: null,
    content_truncated: false,
    total_bytes: 0,
    diff_visible: diffDocumentId !== null,
    close_confirmation: closeDialog.open,
    search_open: searchDialog.open,
    search_mode: searchMode,
    recovery_available: recoveryDialogDocumentId !== null,
    composing: windowsComposingEditors.size !== 0,
    sealed: windowsSealedBarrier !== null || windowsUiSaveAll !== null,
    replacement_pending: windowsPendingReplacements > 0,
    quarantined: windowsQuarantined,
    appearance: {
      applied: { ...appliedAppearance }, pending: windowsPendingTheme !== null,
      font_family: editor.getOption(monaco.editor.EditorOption.fontFamily),
      font_size: editor.getOption(monaco.editor.EditorOption.fontSize),
      background: background === null ? null : window.getComputedStyle(background).backgroundColor,
      css_background: rootStyle.getPropertyValue("--ink").trim(),
      css_foreground: rootStyle.getPropertyValue("--text").trim(),
    },
  };
  if (document !== undefined) {
    const selection = editor.getModel() === document.model ? editor.getSelection() : null;
    result.selection = selection === null ? null : windowsRange(selection);
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

const windowsFindLimit = 500;
function windowsFindOptions(command) {
  windowsRequire(windowsValidString(command.query, 4096) && command.query.length > 0,
    "A nonempty literal query of at most 4096 UTF-8 bytes is required.");
  windowsRequire(command.regex !== true, "Regular expressions are unavailable for editor commands.");
  const replacement = command.action !== "find";
  const pinned = command.document_id !== undefined || command.version !== undefined;
  windowsRequire(!replacement || pinned, "Replacement requires document_id and version from the current document.");
  windowsRequire(!pinned || (windowsValidString(command.document_id, 128) && command.document_id.length > 0 &&
    Number.isSafeInteger(command.version) && command.version >= 1), "Supply a valid document_id and version together.");
  if (replacement) windowsRequire(windowsValidString(command.text, maxDocumentBytes), "Valid replacement text is required.");
  return { query: command.query, case_sensitive: command.case_sensitive === true,
    whole_word: command.whole_word === true, backward: command.backward === true,
    separators: command.whole_word === true ? editor.getOption(monaco.editor.EditorOption.wordSeparators) : null };
}
function windowsFindOwner(document, command) {
  windowsRequire(documents.get(document.payload.id) === document && activeDocumentId === document.payload.id &&
    editor.getModel() === document.model, "The active document changed before the search completed.");
  windowsRequire(command.document_id === undefined || (command.document_id === document.payload.id &&
    command.version === document.payload.version), "The document or version is stale; find again before replacing.");
}
function windowsRange(range) {
  return { start_line: range.startLineNumber, start_column: range.startColumn,
    end_line: range.endLineNumber, end_column: range.endColumn };
}
function windowsSameRange(left, right) {
  return left !== null && right !== null && left.startLineNumber === right.startLineNumber &&
    left.startColumn === right.startColumn && left.endLineNumber === right.endLineNumber && left.endColumn === right.endColumn;
}
function windowsFindMatches(document, options) {
  return document.model.findMatches(options.query, false, false, options.case_sensitive, options.separators, false, windowsFindLimit + 1);
}
function windowsFindSelection(document, options, useCurrent) {
  const selection = editor.getSelection();
  if (useCurrent && selection !== null) {
    const current = document.model.findNextMatch(options.query,
      { lineNumber: selection.startLineNumber, column: selection.startColumn }, false,
      options.case_sensitive, options.separators, false);
    if (current !== null && windowsSameRange(current.range, selection)) return current;
  }
  const position = selection === null ? { lineNumber: 1, column: 1 } : options.backward
    ? { lineNumber: selection.startLineNumber, column: selection.startColumn }
    : { lineNumber: selection.endLineNumber, column: selection.endColumn };
  return document.model[options.backward ? "findPreviousMatch" : "findNextMatch"](
    options.query, position, false, options.case_sensitive, options.separators, false);
}
function windowsFindResult(document, options, matches, selected, replaced) {
  const retained = matches.slice(0, windowsFindLimit);
  const index = selected === null ? -1 : retained.findIndex((match) => windowsSameRange(match.range, selected.range));
  if (selected !== null) editor.setSelection(selected.range);
  return { document_id: document.payload.id, version: document.payload.version,
    query: options.query, case_sensitive: options.case_sensitive, whole_word: options.whole_word, backward: options.backward,
    count: retained.length, truncated: matches.length > windowsFindLimit,
    matches: retained.map((match) => windowsRange(match.range)),
    selected: selected === null ? null : { index: index < 0 ? null : index, range: windowsRange(selected.range) }, replaced };
}
async function windowsFind(document, command) {
  const options = windowsFindOptions(command);
  // Flush a pending user edit before checking the caller's acknowledged version.
  // The command seal prevents any new input while that asynchronous ACK arrives.
  await windowsSync(document);
  windowsFindOwner(document, command);
  const matches = windowsFindMatches(document, options);
  if (command.action === "find") {
    return windowsFindResult(document, options, matches, windowsFindSelection(document, options, false), 0);
  }
  windowsRequire(!document.payload.readOnly, "The document is read-only.");
  windowsRequire(command.action !== "replace_all" || matches.length <= windowsFindLimit,
    "Replace All is unavailable for truncated results; narrow the query.");
  const selected = command.action === "replace_match" ? windowsFindSelection(document, options, true) : null;
  const edits = command.action === "replace_all" ? matches : selected === null ? [] : [selected];
  if (edits.length === 0) return windowsFindResult(document, options, matches, null, 0);
  // Monaco normalizes inserted line endings to the model's LF. Preflight the
  // exact resulting UTF-8 size before touching the model or its undo history.
  const replacement = command.text.replace(/\r\n|\r/g, "\n");
  const insertedBytes = utf8ByteLength(replacement);
  let bytes = utf8ByteLength(document.model.getValue());
  for (const match of edits) bytes += insertedBytes - utf8ByteLength(document.model.getValueInRange(match.range));
  windowsRequire(bytes <= maxDocumentBytes, "Replacement would exceed the 16 MiB document limit.");
  editor.updateOptions({ readOnly: false });
  try {
    editor.pushUndoStop();
    windowsRequire(editor.executeEdits("flowmux-windows-find", edits.map((match) =>
      ({ range: match.range, text: replacement, forceMoveMarkers: true }))), "Monaco rejected the replacement.");
    editor.pushUndoStop();
  } finally { windowsApplySeal(); }
  await windowsSync(document);
  windowsFindOwner(document, {});
  return windowsFindResult(document, options, windowsFindMatches(document, options),
    windowsFindSelection(document, options, false), edits.length);
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
    case "find":
    case "replace_match":
    case "replace_all":
      return windowsFind(document, command);
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

let windowsSearchRetained = null;
let windowsSearchOpenPending = null;
const windowsSearchComposing = new Set();
function windowsSearchError(error) {
  let text = "", bytes = 0;
  for (const character of String(error)) {
    const size = utf8ByteLength(character);
    if (bytes + size > 4096) break;
    text += character; bytes += size;
  }
  searchStatus.textContent = text;
}
const windowsSearchInputChanged = searchInputChanged;
searchInputChanged = () => {
  if (windowsSearchComposing.size === 0) windowsSearchInputChanged();
};
const windowsScheduleWorkspaceSearch = scheduleWorkspaceSearch;
scheduleWorkspaceSearch = () => {
  if (searchMode === "workspace") windowsSearchRetained = null;
  if (windowsSearchComposing.size === 0) windowsScheduleWorkspaceSearch();
  else clearSearchTimer();
};
const windowsRequestWorkspaceSearch = requestWorkspaceSearch;
requestWorkspaceSearch = () => {
  if (windowsSearchComposing.size === 0) windowsRequestWorkspaceSearch();
  else clearSearchTimer();
};
const windowsRenderQuickOpen = renderQuickOpen;
renderQuickOpen = () => {
  if (windowsSearchComposing.size === 0) windowsRenderQuickOpen();
};
const windowsSearchKeyDown = searchKeyDown;
searchKeyDown = (event) => {
  if (!event.isComposing && event.keyCode !== 229 && windowsSearchComposing.size === 0) windowsSearchKeyDown(event);
};
const windowsCloseSearchDialog = closeSearchDialog;
closeSearchDialog = () => {
  windowsSearchRetained = null;
  windowsCloseSearchDialog();
};
for (const input of [searchQuery, searchInclude, searchExclude]) {
  input.addEventListener("compositionstart", () => {
    windowsSearchComposing.add(input);
    clearSearchTimer();
    // Quick Open filters a query-independent file index locally. Keep that
    // index/token (or its pending request); defer only its ranking/rendering.
    if (searchMode === "workspace") { cancelActiveSearch(); windowsSearchRetained = null; }
  });
  input.addEventListener("compositionend", () => {
    windowsSearchComposing.delete(input);
    if (windowsSearchComposing.size === 0 && searchDialog.open) searchInputChanged();
  });
}
function windowsCompleteSearch(message, metadata) {
  if (!searchDialog.open || metadata?.request_id !== activeSearchRequestId ||
    message?.requestId !== activeSearchRequestId || metadata.kind !== searchMode) return false;
  try {
    windowsRequire(isHostMessage(message) && message.surfaceId === surfaceId &&
      message.type === (searchMode === "quick" ? "quick_open_completed" : "workspace_search_completed"), "Invalid search completion.");
    windowsRequire(utf8ByteLength(JSON.stringify(message)) <= 1024 * 1024, "Search results exceed the display limit.");
    const entries = searchMode === "quick" ? message.paths : message.result?.matches;
    windowsRequire(Array.isArray(entries) && entries.length <= (searchMode === "quick" ? 2000 : 500), "Too many search results.");
    const error = metadata.error ?? (searchMode === "workspace" ? message.error : null);
    windowsRequire(error === null || typeof error === "string", "Invalid search error.");
    for (const entry of entries) {
      const path = searchMode === "quick" ? entry : entry?.path;
      windowsRequire(windowsValidString(path, 16 * 1024) && path.length > 0, "Invalid search result path.");
      if (searchMode === "workspace") {
        windowsRequire([entry.line, entry.column, entry.length].every((value) => Number.isSafeInteger(value) && value >= 0), "Invalid search result range.");
      }
    }
    windowsRequire(error !== null || (typeof metadata.token === "string" && /^[A-Za-z0-9_.-]{1,128}$/.test(metadata.token)), "Search result token is missing.");
    windowsSearchRetained = error === null ? { token: metadata.token, kind: searchMode,
      entries: entries.map((entry) => searchMode === "quick" ? { path: entry, line: 0, column: 0, length: 0 }
        : { path: entry.path, line: entry.line, column: entry.column, length: entry.length }) } : null;
    window.flowmuxEditorHost.receive(message);
    if (error !== null) { clearSearchResultNodes(); windowsSearchError(error); }
    return true;
  } catch (error) {
    windowsSearchRetained = null; activeSearchRequestId = null;
    clearSearchResultNodes(); windowsSearchError(error.message ?? error);
    return false;
  }
}
openSearchResult = (index) => {
  if (windowsSearchComposing.size > 0 || windowsSearchOpenPending !== null || windowsCommandBusy ||
    windowsSealedBarrier !== null || windowsQuarantined) return;
  const selection = renderedSearchResults[index], retained = windowsSearchRetained;
  if (selection === undefined || retained === null || retained.kind !== searchMode || !searchDialog.open) {
    windowsSearchError("Search results expired; run the search again."); return;
  }
  const originalIndex = retained.entries.findIndex((entry) => entry.path === selection.path && entry.line === selection.line &&
    entry.column === selection.column && entry.length === selection.length);
  if (originalIndex < 0) { windowsSearchError("Search results changed; run the search again."); return; }
  windowsSearchOpenPending = { token: retained.token, path: selection.path };
  window.__flowmuxWindowsEditorBridge({ kind: "search_open", token: retained.token, index: originalIndex });
};

window.flowmuxWindowsEditor = Object.freeze({
  setTheme(colors) {
    try {
      const fields = ["background", "foreground", "cursor", "selectionBackground", "selectionForeground"];
      windowsRequire(colors !== null && typeof colors === "object" && !Array.isArray(colors) &&
        Object.keys(colors).length === fields.length + 1 && typeof colors.dark === "boolean" &&
        fields.every((field) => typeof colors[field] === "string" && /^#[0-9a-f]{6}([0-9a-f]{2})?$/i.test(colors[field])),
      "Invalid editor theme colors.");
      windowsPendingTheme = { ...colors };
      windowsFlushTheme();
    } catch (error) {
      window.__flowmuxWindowsEditorBridge({ kind: "theme_error", error: String(error.message ?? error).slice(0, 4096) });
    }
  },
  completeSearch: windowsCompleteSearch,
  searchOpenFinished(token, error) {
    if (windowsSearchOpenPending?.token !== token) return;
    const opened = windowsSearchOpenPending;
    windowsSearchOpenPending = null;
    if (!searchDialog.open || windowsSearchRetained?.token !== token) return;
    if (error !== null) { windowsSearchError(error); return; }
    const recentIndex = recentPaths.indexOf(opened.path);
    if (recentIndex >= 0) recentPaths.splice(recentIndex, 1);
    recentPaths.unshift(opened.path); recentPaths.splice(20);
    closeSearchDialog(); // Never releases the native-owned editor input guard.
  },
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
  beginDiskRefresh(id) {
    if (!Number.isSafeInteger(id) || id <= 0) return;
    const busy = windowsCommandBusy || windowsSealedBarrier !== null || windowsUiSaveAll !== null ||
      windowsPendingReplacements > 0 || windowsQuarantined || windowsDiskRefresh !== null ||
      windowsComposingEditors.size !== 0 || closeDialog.open || recoveryDialogDocumentId !== null ||
      saveAsDialog.open || searchDialog.open || (editor.hasWidgetFocus() && !editor.hasTextFocus()) ||
      diffDocumentId !== null || [...documents.values()].some((document) =>
        document.pendingChanges || document.outstandingChanges.size > 0);
    if (busy || documents.size === 0) {
      window.__flowmuxWindowsEditorBridge({ kind: "refresh_deferred", id });
      return;
    }
    windowsDiskRefresh = { id, phase: "flushing" };
    window.flowmuxWindowsEditor.barrier(id, true);
  },
  applyDiskRefreshMessage(id, message) {
    // A stale completion may belong to a closed/replaced native view. It cannot
    // consume the seal owned by a later refresh, even within this same document.
    if (windowsDiskRefresh?.id !== id) return;
    try {
      windowsRequire(["flushed", "applying"].includes(windowsDiskRefresh.phase), "Refresh was not flushed.");
      windowsRequire(windowsSealedBarrier === id && !windowsQuarantined, "Refresh no longer owns its input guard.");
      windowsRequire(windowsComposingEditors.size === 0, "Composition began before the refresh could apply.");
      windowsRequire(isHostMessage(message) && message.surfaceId === surfaceId, "Invalid refresh message.");
      windowsRequire(["replace_document", "document_disk_status"].includes(message.type), "Unexpected refresh message type.");
      if (message.type === "replace_document") {
        const current = documents.get(message.document.id);
        windowsRequire(current !== undefined, "Refresh cannot open a new document.");
        windowsRequire(!current.payload.dirty && !current.pendingChanges && current.outstandingChanges.size === 0,
          "Refresh cannot replace unacknowledged or dirty content.");
        windowsRequire(!message.document.dirty && message.document.version > current.payload.version,
          "Refresh replacement must advance a clean document version.");
        // The existing shared function preserves the model, undo stack and
        // active view state. Deliberately omit activateDocument/editor.focus.
        addOrReplaceDocument(message.document);
      } else {
        windowsRequire(documents.has(message.documentId), "Refresh cannot report an unknown document.");
        window.flowmuxEditorHost.receive(message);
      }
      windowsDiskRefresh.phase = "applying";
      windowsApplySeal();
    } catch (error) {
      windowsDiskRefresh.phase = "failed";
      windowsQuarantined = true;
      windowsApplySeal();
      window.__flowmuxWindowsEditorBridge({ kind: "refresh_error", id,
        error: String(error.message ?? error).slice(0, 4096) });
    }
  },
  completeDiskRefresh(id) {
    if (windowsDiskRefresh?.id !== id || windowsQuarantined ||
        !["flushed", "applying"].includes(windowsDiskRefresh.phase)) return;
    windowsDiskRefresh.phase = "applied";
    window.__flowmuxWindowsEditorBridge({ kind: "refresh_applied", id });
    // Acknowledge actual application before native bookkeeping releases input.
  },
  releaseDiskRefresh(id) {
    if (windowsDiskRefresh?.id !== id || windowsDiskRefresh.phase !== "applied" || windowsQuarantined) return;
    windowsDiskRefresh = null;
    window.flowmuxWindowsEditor.releaseBarrier(id);
  },
  abortDiskRefreshBeforeWork(id) {
    // Native may call this ONLY before submitting Work::PollDisk. It must retain
    // the seal after dispatch/timeouts until the original worker result applies.
    if (windowsDiskRefresh?.id !== id || !["flushing", "flushed"].includes(windowsDiskRefresh.phase) || windowsQuarantined) return;
    windowsDiskRefresh = null;
    window.flowmuxWindowsEditor.releaseBarrier(id);
  },
  releaseBarrier(id) {
    if (windowsDiskRefresh !== null) return;
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
      windowsRequire(Object.keys(command).every((key) => ["id", "action", "text", "path", "overwrite", "query", "case_sensitive", "whole_word", "backward", "regex", "document_id", "version"].includes(key)), "Unknown editor command field.");
      windowsRequire(typeof command.action === "string" && command.action.length <= 32, "Invalid editor action.");
      windowsRequire(command.overwrite === undefined || typeof command.overwrite === "boolean", "Overwrite must be an explicit boolean.");
      windowsRequire(command.text === undefined || windowsValidString(command.text, maxDocumentBytes), "Invalid or oversized command text.");
      windowsRequire(command.path === undefined || windowsValidString(command.path, 16 * 1024), "Invalid or oversized command path.");
      for (const field of ["case_sensitive", "whole_word", "backward", "regex"]) {
        windowsRequire(command[field] === undefined || typeof command[field] === "boolean", "Search options must be explicit booleans.");
      }
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
