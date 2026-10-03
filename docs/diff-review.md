<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Diff review

Click **Diff** in the side-panel footer or press **Ctrl+Alt+D**. The shortcut can
be changed in Options. A separate native review window leaves the terminal,
its running process, and its input untouched. macOS and Linux share the same
GTK implementation; no agent skill, account, or extra configuration is required.

The window uses the focused local tab's directory. SSH checkouts are not read
through the local filesystem. Switch to a local checkout to review it.

## Compare changes

- **All changes** compares tracked files against HEAD and includes untracked,
  non-ignored files. It also works before the first commit.
- **Unstaged** compares the working tree with the index and includes untracked files.
- **Staged** compares the index with HEAD.
- **Branch changes** compares the merge base of the entered branch/commit with
  HEAD. Uncommitted changes are excluded. Press Enter or Refresh after editing
  the base. Both commit endpoints are pinned until the next refresh.

Filter the file list, then select a file. Old and new line numbers are displayed
next to the unified diff. Only the selected file is loaded; long patches have
500-line pages. The virtual file list handles large sets without creating a
widget for every file. Binary, rename-only, mode, and symlink changes retain
Git's explanatory text. Untracked symlinks are described without following them.

Git operations run outside the GUI thread with a 30-second timeout. A single
Git result has an 8 MiB limit, reported explicitly rather than silently cut off.
Non-UTF-8 patch content reports an error. Paths remain byte-preserving for Git
operations, while control characters in display labels are escaped.

## Regression checks

`cargo test -p flowmux-vcs -p flowmux-config --locked` covers Git scopes,
unborn/empty/non-repositories, renames, deletions, binary files, literal pathspec
characters, Unicode, symlinks, ignored files, pinned branch comparisons, line
numbers, 600 files, 20,000-line patches, and explicit oversized-file handling.
Linux also exercises filenames that are not valid UTF-8 (macOS filesystems may
reject those filenames).

Linux native GUI checks:

```sh
GDK_BACKEND=x11 GTK_A11Y=test G_DEBUG=fatal-criticals \
  xvfb-run -a dbus-run-session -- \
  cargo test -p flowmux --bin flowmux review --locked -- --nocapture
```

macOS native GUI checks run on the AppKit main thread:

```sh
FLOWMUX_BUNDLED_CLI_PATH="$PWD/target/debug/flowmuxctl" \
  cargo test -p flowmux --test macos_native --features native-smoke --locked
```

The shared GUI scenario verifies small/large diffs, a 601-file list, filtering,
first/last page access, narrow-window layout, fast scope changes while reads
are pending, bad base errors, and close/reopen. Set
`FLOWMUX_REVIEW_SNAPSHOT_DIR` to retain a rendered PNG. These tests run alongside
existing macOS terminal/browser/editor/theme regression scenarios.

## Review comments

Select one or more diff lines and write a comment in **Write**, or leave the
selection empty to comment on the whole file. The anchor is captured when you
start writing, so switching files while composing does not move the comment.
**Save comment** persists it locally in `reviews.sqlite3` in FlowMux's state
directory. Review data never changes files or the Git index. **Comments** lets
you edit, resolve, reopen, or reload saved comments. Resolved comments are kept
and excluded from delivery. A concurrent edit in another window reports a
conflict instead of overwriting it; reload preserves the text you are composing.

**Deliver → Validate & preview** prepares one review containing all open
comments, file paths, old/new line ranges, and quoted diff context. **Copy review**
checks the current diff again before copying. A full diff fingerprint marks
changed/deleted anchors as STALE. Refresh the diff, edit a stale comment and
choose **Use current diff selection** to attach it explicitly, or resolve it.
Unrelated changes in the same file also require this check. Reviews containing
stale comments cannot be copied as current feedback.

Hiding the review window keeps the draft. Closing FlowMux with an unsaved review
focuses that draft so it can be saved or explicitly cleared first. Saved comments
survive restarts; unfinished text does not survive a forced process termination.

The shared native scenario additionally covers line selection on page 41,
Unicode and long multiline comments, 300 saved comments, edit/resolve/reopen,
clipboard readback, persistence reload, cross-window conflicts, stale anchors,
and preservation of unfinished text when the window is hidden.
