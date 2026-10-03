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
  cargo test -p flowmux --bin flowmux ui::review_window --locked -- --nocapture
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

## Deliver to a running agent

The **Deliver** tab lists this window's live agent sessions from FlowMux's existing runtime
model, including the workspace, tab, and current status. Select the recipient,
then use **Copy & focus agent**. FlowMux validates the review, rechecks the
recipient's process/session identity, copies the complete review, and focuses
that terminal. Paste and submit when ready. The handoff preserves existing
input and works while an agent is idle, working, finished, or asking for approval.
An ended/replaced session cannot receive a handoff intended for its predecessor.

This uses the same path for Claude, Codex, Gemini, Cline, OpenCode, AGY, and
custom agent names. It requires no provider SDK, skill, hook-specific prompt,
or saved recipient configuration. Detection still depends on FlowMux's existing
agent runtime; **Copy review** is available even when a program is not detected.
Agent selection is explicit and can include another local workspace. Verify the
workspace/tab label before handing off. Remote checkout reading and automatic
prompt submission are outside this feature.

The preview displays at most 40,000 characters with a visible notice; copying
includes the complete review. An unfinished composer must be saved or cleared
before delivery.

## Acceptance matrix

| Scenario | Automated check |
| --- | --- |
| Empty/unborn repo, all four scopes, pinned branch endpoints | `flowmux-vcs` review integration tests |
| Add/delete/rename/binary/symlink, literal and non-UTF-8 paths | `flowmux-vcs` review integration tests |
| 601 files, 20,000-line diff, first/last pages, filters, rapid scope changes | Shared native GTK smoke |
| Empty/short/Unicode/multiline/long comments and 300 comments | Model tests and shared native GTK smoke |
| 50 files with mixed-length reviews and staged/unstaged anchors | `reviews_across_many_files_and_scopes_validate_independently` |
| Edit, resolve/reopen, explicit re-anchor, save/reload, concurrent save conflict | Shared native GTK smoke and state tests |
| External changes/deletions, stale copy rejection, binary content change | Model tests and shared native GTK smoke |
| Preview/copy complete text, large-preview bound, provider handoff button | Shared native GTK smoke |
| Seven provider names × five states, stale session, exited process, terminal draft preservation | `review_handoff_preserves_agent_input_and_checks_session` and macOS native harness |
| Hidden-window draft retention, app-close guard, narrow window | Shared native GTK smoke and handoff smoke |
| Installed app: shortcut, typing, multiline save, clipboard, resize, close/reopen | `scripts/test-diff-review-gui.py --gui ... --cli ...` (Linux; Xvfb, Xlib, Pillow, xdotool, xclip) |
| Actual macOS text insertion callback and installed GUI input | macOS native harness plus isolated installed-app keyboard/paste verification |

Provider-state tests use fixture lifecycle records and real terminal widgets;
they verify FlowMux's common handoff, not third-party model responses. The feature
does not submit prompts. Native tests exercise GTK on macOS and Linux X11; an
independent Wayland desktop and other CPU architectures are not covered by the
local verification environment.
