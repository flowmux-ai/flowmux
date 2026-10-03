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
