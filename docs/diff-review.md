<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Review changes

Open the pane's top-right **… → View Diff** menu, click the sidebar's Diff icon
right of AI usage, or press **Ctrl+Alt+E**
for the focused pane. The review stays inside that pane, including split layouts.
Other panes remain visible and usable. The back arrow or **Escape** returns to
that pane's previous tab without stopping its process.

## Read the current checkout

The review uses the selected pane's current local Git checkout (or its active
editor's project path). Its path is shown above the files. It compares the
working tree with **HEAD**, including staged edits, unstaged edits, and new
untracked files in one list. Already committed changes are excluded. There are
no branch inputs or scope dropdowns. A file with both staged and unstaged edits
appears once with its net change.

Use **Refresh** after changing files or Git state. Reopening also refreshes the
comparison. Changing the pane's directory and reopening selects that checkout;
an unfinished comment is preserved until saved or cancelled. Reviews in separate
panes keep their own navigation, even when they point at the same repository.

Select or filter a file. Diffs scroll continuously without manual pages.
Transport headers and full object hashes are hidden; old/new line numbers,
changed lines, and Git descriptions for binary, rename, mode, and symlink
changes remain visible. With the diff focused, **n / p** moves between hunks.

## Comment where you read

Click **+** beside a code line, select a range and click **+ Comment**, or press
**c** in the diff. The comment editor opens directly below the selected code.
Use **File comment** for feedback on the entire file. **Ctrl/Cmd+Enter** saves;
**Cancel** or **Escape** cancels from anywhere in the composer. An open menu
closes first on **Escape** or an outside click. Saved comments stay inline with Edit and Delete actions. Delete removes the saved comment immediately.
The **Comments** menu jumps to a comment's file and location, including feedback
on files no longer present in the comparison.
Below **Reload saved comments**, **Remove all comments** deletes the saved
comments for this checkout and closes its current composer after a successful save.

Comments are saved locally in `reviews.sqlite3`. They do not modify repository
files or Git's index. Open Diff panes and windows using the same checkout and
local storage automatically pick up saved changes within about half a second.
Updates wait while a Comments or Send menu is open to preserve its focused rows.
Unfinished text stays local. If another pane changes the same comment being
edited, the local text is preserved and saving is blocked until that edit is
cancelled; copy any text you want to keep first. Concurrent saves also use
revision checks; **Reload saved comments** recovers a storage revision conflict.
Closing
FlowMux with an unfinished comment brings it forward for saving or cancellation.
A forced process termination can lose unfinished text.

## Send feedback

Open **Send** and select an agent in the same workspace. FlowMux checks
comments against the current code, verifies the live session, and sends the
batch as a bracketed paste followed by Enter. An agent must be idle or done and
show an empty recognized prompt (`›`, `❯`, or `>`), or Codex's known empty-input placeholder with the cursor at its start. Existing input, working
agents, approval waits, exited sessions, and unrecognized prompts are left
untouched. Feedback remains saved so it can be retried. **Copy feedback** is
available in the same menu for other programs or prompt styles.

Comments follow unchanged selected code when other lines shift. Nearby context
helps disambiguate repeated code. If a selected passage changes or its location
is ambiguous, the review opens that comment for inspection; delete it or use
**Reattach** (attach to selected lines). Whole-file text comments stay attached across
edits. Missing files and changed binary content still need inspection.

## Limits and verification

Only local checkouts are supported. Git reads run outside the GUI thread with
a 30-second command timeout and an explicit 8 MiB output limit. Non-UTF-8 patch
content reports an error. Git paths retain their original bytes; control
characters are escaped for display. Untracked symlinks are never followed.

Model and persistence checks:

```sh
cargo test -p flowmux-vcs -p flowmux-state --locked
```

Linux native GUI checks (GTK on X11):

```sh
GDK_BACKEND=x11 GTK_A11Y=test G_DEBUG=fatal-criticals \
  xvfb-run -a dbus-run-session -- \
  cargo test -p flowmux --bin flowmux ui::review_window --locked -- --test-threads=1
GDK_BACKEND=x11 GTK_A11Y=test G_DEBUG=fatal-criticals \
  xvfb-run -a dbus-run-session -- \
  cargo test -p flowmux --bin flowmux ui::window::review --locked -- --test-threads=1
```

macOS native GUI checks:

```sh
./scripts/test-diff-review-macos.sh
```

The native scenarios check continuous 20,000-line rendering, pane-local navigation and checkout selection,
menu dismissal and comment cancellation, stacked comment edit geometry,
unchanged Edit versus unsaved text, menu-to-PTY delivery between two Codex targets,
inline Unicode comments, persistence, relocation after insertion, missing-file
feedback, existing terminal input protection, session identity checks, and an
actual child PTY's receipt of the multiline bracketed paste and submit key.
They do not assert that every third-party agent recognizes every prompt style.
Installed-app keyboard/mouse verification uses a separate app bundle and state
directory, preserving the user's running FlowMux sessions.

Detailed event coverage and outstanding verification limits: [event audit](diff-review-event-audit.md).
