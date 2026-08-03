// SPDX-License-Identifier: GPL-3.0-or-later

import type { DocumentDiskStatus } from "./protocol";

export const EDITOR_ZOOM_MIN = 50;
export const EDITOR_ZOOM_MAX = 200;
export const EDITOR_ZOOM_STEP = 10;

export function utf8ByteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

export function adjustedZoomPercent(current: number, direction: number): number {
  const delta = Math.sign(direction) * EDITOR_ZOOM_STEP;
  return Math.min(EDITOR_ZOOM_MAX, Math.max(EDITOR_ZOOM_MIN, current + delta));
}

export function editorZoomDirectionForKey(event: {
  altKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  key: string;
  code: string;
}): number | null {
  if (event.altKey || (!event.ctrlKey && !event.metaKey)) {
    return null;
  }
  if (
    event.key === "+" ||
    event.code === "NumpadAdd" ||
    (event.shiftKey && event.code === "Equal")
  ) {
    return 1;
  }
  if (event.key === "-" || event.code === "NumpadSubtract") {
    return -1;
  }
  return null;
}

export interface VisibleDocumentState {
  text: string;
  kind: "normal" | "dirty" | "conflict";
  hidden: boolean;
}

export function visibleDocumentState(
  diskStatus: DocumentDiskStatus,
  readOnly: boolean,
  dirty: boolean,
): VisibleDocumentState {
  if (diskStatus === "deleted") {
    return { text: "Deleted on disk", kind: "conflict", hidden: false };
  }
  if (diskStatus === "modified") {
    return { text: "Changed on disk", kind: "conflict", hidden: false };
  }
  if (readOnly) {
    return { text: "Read only", kind: "normal", hidden: false };
  }
  if (dirty) {
    return { text: "Unsaved", kind: "dirty", hidden: false };
  }
  return { text: "Saved", kind: "normal", hidden: true };
}

export interface ConflictUiState {
  hidden: boolean;
  message: string;
  compareDisabled: boolean;
  reloadDisabled: boolean;
  keepLabel: string;
  closeCompareHidden: boolean;
}

export function conflictUiState(
  diskStatus: DocumentDiskStatus,
  externalChange: boolean,
  comparing: boolean,
): ConflictUiState {
  const deleted = diskStatus === "deleted";
  return {
    hidden: !externalChange,
    message: comparing
      ? "Comparing the disk version with your editor changes."
      : deleted
        ? "This file was deleted on disk."
        : "This file changed on disk while you were editing it.",
    compareDisabled: deleted || comparing,
    reloadDisabled: deleted,
    keepLabel: deleted ? "Recreate on Save" : "Keep Mine",
    closeCompareHidden: !comparing,
  };
}
