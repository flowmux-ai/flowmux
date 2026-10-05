<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Code Review

Open the pane's top-right **… → Code Review** menu, click the sidebar's Code Review icon
right of AI usage, or press **Ctrl+Alt+E**
for the focused pane. The review stays inside that pane, including split layouts.
Other panes remain visible and usable. Use the same icon, menu item, or shortcut
again to return to that pane's previous tab. The back arrow or **Escape** also
returns without stopping its process. Toggling preserves unfinished comments.

## Choose changes to review

The review uses the selected pane's current local Git checkout (or its active
editor's project path). Its path is shown above the files. It compares the
working tree with **HEAD**, including staged edits, unstaged edits, and new
untracked files in one list. The default **Unstaged + Staged** selection shows
this combined comparison. A file with both staged and unstaged edits appears
once with its net change.

Use the dropdown beside **Code Review** to select a commit from the current
branch's history. The newest 50 commits load first; scrolling to the bottom
loads 50 more at a time. Each row shows a short commit ID and subject. Opening
the menu again refreshes the history; additional pages remain pinned to the
same HEAD while the menu is open, even if another agent creates a commit.

Selecting a commit shows only that commit's changed files and diffs against
its first parent. A root commit is compared with an empty tree. Merge commits
use their first parent. Reviewing history does not switch branches or change
the checkout. In a shallow clone, a missing parent reports an error and asks
you to fetch the missing history. Save or cancel unfinished comments before
changing the selection.
Each commit and the uncommitted comparison keep separate comments, counts,
and Send batches. Refreshing or toggling Code Review keeps the selected commit fixed.

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
The **Comments** menu jumps to a comment's file and location.
Opening or refreshing Code Review, reloading saved comments, and preparing
feedback automatically remove saved comments whose file or selected code is no
longer in the current diff. Comments whose code merely moved follow its new
location. Whole-file comments remain while that file still has a valid diff.
Git read failures and concurrent saves do not authorize deleting comments.
Unfinished local text is preserved; if its saved comment was removed, cancel
the edit before starting a new comment.
Below **Reload saved comments**, **Remove all comments** deletes the saved
comments for the selected comparison and closes its current composer after a successful save.

Comments are saved locally in `reviews.sqlite3`. They do not modify repository
files or Git's index. Open Code Review panes and windows using the same checkout and
local storage automatically pick up saved changes within about half a second.
Updates wait while a Comments or Send menu is open to preserve its focused rows.
Unfinished text stays local. If another pane changes the same comment being
edited, the local text is preserved and saving is blocked until that edit is
cancelled; copy any text you want to keep first. Concurrent saves also use
revision checks; **Reload saved comments** recovers a storage revision conflict.
Closing
Flowmux with an unfinished comment brings it forward for saving or cancellation.
A forced process termination can lose unfinished text.

## Send feedback

Open **Send** and select an agent in the same workspace. Flowmux checks
comments against the selected comparison's code, verifies the live session, and sends the
batch as a bracketed paste followed by Enter. An agent must be idle or done and
show an empty recognized prompt (`›`, `❯`, or `>`), or Codex's known empty-input placeholder with the cursor at its start. Existing input, working
agents, approval waits, exited sessions, and unrecognized prompts are left
untouched. Feedback remains saved so it can be retried. **Copy feedback** is
available in the same menu for other programs or prompt styles.

Uncommitted feedback identifies **Unstaged + Staged**, compares against HEAD,
and asks the agent to preserve existing staging choices. Commit feedback
includes the full commit ID, explains that line numbers refer to historical
code, and asks the agent to locate the corresponding current code before
making corrections without rewriting the reviewed commit. Only comments from
the selected comparison are sent or copied. Historical comments remain tied
to their commit when the working tree changes or HEAD advances.

Uncommitted comments follow unchanged selected code when other lines shift. Nearby context
helps disambiguate repeated code. Saved comments with changed or ambiguous
selections that still exist are preserved for reattachment. Missing files,
removed selected code, or changed binary content are removed during validation.
Whole-file text comments stay attached across edits. If no valid comments remain,
the list is cleared and no feedback is copied or sent.

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

The native scenarios check 50/100/103-commit dropdown scrolling, commit and
uncommitted comment isolation, scope-specific feedback, historical review reopening,
and protection of unfinished comments when switching scopes. The macOS handoff
scenario verifies both message templates at the receiving child PTY.
They also check continuous 20,000-line rendering, pane-local navigation and checkout selection,
menu dismissal and comment cancellation, stacked comment edit geometry,
unchanged Edit versus unsaved text, menu-to-PTY delivery between two Codex targets,
inline Unicode comments, persistence, relocation after insertion, missing-file
feedback, existing terminal input protection, session identity checks, and an
actual child PTY's receipt of the multiline bracketed paste and submit key.
They do not assert that every third-party agent recognizes every prompt style.
Installed-app keyboard/mouse verification uses a separate app bundle and state
directory, preserving the user's running Flowmux sessions.

Detailed event coverage and outstanding verification limits: [event audit](diff-review-event-audit.md).
