// SPDX-License-Identifier: GPL-3.0-or-later

import type { EditorFocusDirection } from "./protocol";

export interface FocusNavigationKey {
  key: string;
  altKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  isComposing: boolean;
}

/** Keep Monaco's normal key handling; only plain Alt+arrow leaves the editor. */
export function focusDirectionForKey(event: FocusNavigationKey): EditorFocusDirection | null {
  if (event.isComposing || !event.altKey || event.ctrlKey || event.shiftKey || event.metaKey) {
    return null;
  }
  switch (event.key) {
    case "ArrowLeft":
      return "left";
    case "ArrowRight":
      return "right";
    case "ArrowUp":
      return "up";
    case "ArrowDown":
      return "down";
    default:
      return null;
  }
}
