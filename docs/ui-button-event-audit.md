<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# UI button and event audit

## Contract and scope

Button-owned menus open on the first activation and close on the next. Closing
must not replay that activation into a new opening. Outside presses dismiss the
menu and reach the selected control; Escape dismisses only the menu. Keyboard
opening of another menu replaces the previous menu. Nested menus retain their
parent, and changing windows or unmapping the anchor dismisses the popup.

This audit covers menu construction, button/toggle signals, and window command
routing. It is not a claim that every platform-specific UI action was physically
clicked. Destructive actions, navigation, refresh, and item selection remain
ordinary actions; panel visibility and menu visibility are toggles.

| Area | Finding / action | Verification |
| --- | --- | --- |
| AI Usage, commit history, Comments, Send, notifications, pane actions, bookmarks, downloads | Use one menu binding without a native autohide grab. Keep GTK's button toggle; explicitly handle dismissal and controller cleanup. | Common mapped-widget regression; existing menu/shortcut tests; macOS clicks, Escape, and AI Usage shortcut |
| Tab context menu → Move | An ordinary Button unconditionally called `popup()`. Replace it with a MenuButton using the same binding. | Open/close/reopen, destination callback, parent/submenu/callback release test |
| Options | Repeated commands created independent windows. Toggle the existing Options window belonging to the same main window. | Two native open/close cycles; isolated desktop button open/reclick close |
| File browser, worktrees, workspace overview, Code Review, pane zoom, agent bar, usage bar | Existing visibility/state transitions already distinguish open and closed. Retain their pane/window scope and draft/focus safeguards. | Existing Linux controller, layout, focus, and toggle regressions; native review toggle and pane isolation |
| Browser find, terminal find, search dialogs and command palette | Inline find controls maintain visibility; search reuses its window; modal actions/selection close through their existing flow. No new menu grab or unconditional button popup found here. | Existing Linux UI tests and native browser/terminal checks |
| Right-click menus and ordinary actions | Context menus dismiss before action callbacks and release one-shot widget trees. No matching button to toggle. Preserve these semantics. | Existing context-menu release and action tests |

## Why this change

A native autohide popup can dismiss on an anchor press before the button receives
that press. Letting GTK own the toggle while also replaying that dismissal can
reopen the popup. The shared binding removes this competing dismissal path,
without a second manual click handler or a second active-state variable.

The original whole-window input freeze was not reproduced reliably in the local
baseline. This is therefore a deterministic interaction fix, not proof of a
specific backend freeze root cause. During validation, opening AI Usage by shortcut
while history was open exposed two simultaneously visible menus in the first
patch; the final binding closes other button menus on opening, and has a regression
assertion for this case.

The binding attaches capture/deactivation handlers only while the anchor is
mapped, uses weak widget references, and removes the handlers on unmap. Popup
surface coordinates are not treated as main-window coordinates. This matters for
the Move submenu and for clicking inside a popup.

## Commit message display

A commit snapshot exposes its full subject and body from the commit object already
loaded for parent resolution. Code Review displays selectable, wrapped plain text
in a height-limited scroll area. Changing to uncommitted changes or starting a new
load clears the old message. Tests cover Korean text, paragraphs, literal markup
characters, commit switching, and return to the working tree.

## Validation

- `cargo test -p flowmux-vcs -p flowmux-state --locked`: 103 tests passed.
- Linux GTK 4.14 / Xvfb, `G_DEBUG=fatal-criticals`: 715 GUI tests passed.
- macOS main-thread native review suite, `G_DEBUG=fatal-criticals`: passed,
  including menu toggles/remapping, Options, commit messages, pagination, draft
  preservation, anchor relocation, pane isolation, and actual test PTY handoff.
- Full macOS native suite: passed, including browser/editor, themes, IPC, and close/save paths.
- Isolated rebuilt macOS app: history ON/OFF and subsequent typing; AI Usage
  button and Cmd+Option+U toggles; outside click/menu switching; Escape preserving
  Code Review; Options button toggle; selected commit message displayed.

Synthetic same-frame close/reopen signals hit macOS GTK's
`gdk_surface_thaw_updates` assertion with both the old autohide mode and the new
binding. The test lets the native surface finish unmapping between synthetic
activations. It does not suppress GTK criticals. Desktop click validation is
recorded separately from these signal-based tests. The native Move submenu was
also opened through keyboard navigation and Escape dismissed only its child; its direct coordinate-click attempt
encountered a computer-use pipe error while the app process remained healthy.
The Linux mapped-widget test covers its complete toggle/action/release sequence.

The user's running Flowmux sessions were not restarted or replaced.
