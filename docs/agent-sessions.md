<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent sessions

Click **Agent sessions** in the side-panel footer or press **Ctrl+Alt+J** to toggle
the right-side session panel. While open, it follows the focused terminal tab's
Claude Code, Codex, OpenCode, Antigravity (`agy`), Cline, or Gemini CLI identity.
Closing it
leaves the file and worktree panels as they were.

The list includes local sessions from all projects, ordered by saved
modification time. Search matches titles, recent-message summaries, directories,
and session IDs. Select a row to read its conversation. **Refresh sessions**
reloads the on-disk history.

Compact rows show the agent icon, title, project name, and modification time.
Hover for the full path, summary, and session ID. Each row uses the workspace
color palette and stripe style. Sessions with the same project path share a
color, including across agents; colors stay stable while the window is open.
The list takes the remaining height above a compact conversation preview.
Drag the divider to give either section more space.
On first opening, the panel uses about a quarter of the window width. Resizing
the panel is retained when closing and reopening it within the same window.

To resume a session, select it and click **Resume in new tab**:

- **Claude: Resume in new tab** starts `claude --resume <UUID>` in the selected
  session's project directory, preserving the running agent's configuration
  directory. The current tab and its draft remain intact. Claude's in-app
  `/resume <UUID>` searches the current project and can report "not found" for
  sessions from other projects.
- **Codex: Resume in new tab** starts `codex resume <UUID>` in the selected
  session's project directory, preserving the running agent's `CODEX_HOME` or
  default home. The current tab and its draft remain intact.
- **OpenCode:** `opencode --session <ses_ID>`.
- **Antigravity:** `agy --conversation <UUID>`.
- **Cline CLI:** `cline --id <UUID> --tui`.
- **Gemini CLI:** `gemini --resume <UUID>`. Gemini resolves sessions per project,
  so the saved project directory is required. Gemini CLI 0.62.0 was observed to
  delete a resumed session's transcript at its next launch in that project when
  no new message had been sent after resuming.

All six agents open a new tab in the saved project directory and preserve the
original tab and draft. The new tab uses the normal configured terminal shell;
exiting the agent returns to that shell in the selected project directory.
On Linux, OpenCode retains its XDG/config overrides,
Antigravity retains its home, Gemini CLI retains `GEMINI_CLI_HOME`, and Cline retains its directory overrides and
`--config` / `--data-dir` arguments, including isolated sandbox storage.

Another session can be opened while the focused agent is working or waiting for
input; the original tab remains untouched. The currently active session cannot
be opened again from that tab. The session ID remains visible for manual use.

## Session Flow

Right-click an identified, idle Claude Code, Codex, or OpenCode terminal and
choose **Fork session** to split that pane into left and right halves. The
original session stays on the left; the new native session opens on the right.
The action is disabled when the installed tool or current session cannot safely
fork. A conversation fork shares the project directory; it does not create a Git
branch or worktree.

When an open terminal tab belongs to a known fork family, its workspace item
shows a **Session Flow** button. Click it to open the graph above the existing
panes, initially using about a third of the available height. The graph includes
known ancestors, siblings, and descendants. Separate families appear together;
opening several sessions in one family does not duplicate that graph. The
relationships belong to sessions, so different workspaces can show the same
family with independent camera positions.

- Scroll or pinch to zoom around the pointer; use **Fit** to see the whole graph.
- Drag empty space, hold Space while dragging, use the middle mouse button, or
  enable **Pan** to move the view. Dragging a node moves it and its connected
  layout without opening a session.
- Click an available node, or select it with the arrow keys and press Enter, to
  open it. A closed session resumes to the right of the last focused pane below
  the graph. An already open session focuses its existing terminal.
- Unavailable nodes retain their relationships and show a disabled mark; they
  cannot start a session. Closing the graph leaves the running terminals intact.

External lineage is currently imported from explicit Codex fork metadata.
Flowmux also records forks it performs and verifies. Other tools may not retain
enough metadata to recover forks made outside Flowmux; similar titles or prompt
text are not treated as relationship evidence. Gemini, agy, and Cline retain
their existing history/resume paths, but their **Fork session** action is
disabled. Remote Session Flow execution is not supported.

Launch confirmation requires the actual native session ID. If confirmation
times out, the operation is not automatically retried: it may already have
created a session. This version has no dedicated UI for resolving an unknown
operation. See the [design](agent-session-flow-design.md) and
[validation record](agent-session-flow-validation.md) for tested provider versions
and outstanding verification.

## History and limits

Titles come from native titles or the first user message. The summary is a recent conversation excerpt, not a generated
summary. Conversation previews include user/assistant text and supported image
placeholders;
tool payloads, system instructions, and reasoning are omitted. Antigravity shows
its native summary only: its full transcript uses binary protobuf records.
Resume the session to read the full conversation in agy.

History is read from Claude's `projects/*/*.jsonl` and Codex's
`sessions/YYYY/MM/DD/*.jsonl`, with Codex names from `session_index.jsonl`.
The additional native stores are:

| Agent | Default history store | Supported format |
|---|---|---|
| OpenCode | `~/.local/share/opencode/opencode.db` | Current SQLite `session`, `message`, and `part` tables |
| Antigravity | `~/.gemini/antigravity-cli/conversation_summaries.db` | Local workspace summaries |
| Cline CLI | `~/.cline/data/db/sessions.db` | CLI 3 sessions and their saved messages JSON |
| Gemini CLI | `~/.gemini/tmp/<project>/chats/session-*.jsonl` | JSONL transcripts with the project's `.project_root` |

OpenCode's older JSON store, Cline's legacy VS Code task history, and Gemini CLI's
older single-file `session-*.json` transcripts are not read. Gemini titles use its
native summary when present; context-only transcripts that Gemini cannot resume are
hidden, and messages removed with its rewind remain visible in previews.
Archives and child/subagent sessions are excluded where the store identifies them.
Linux reads the running process's home/config directory overrides. Other platforms
use flowmux's environment. Remote SSH session history is not read from the local
machine.

File reads run off the GTK thread. Claude/Codex/Gemini list scanning samples the beginning
and end of each transcript. Their previews show the latest 2 MiB of a transcript
and explicitly report when earlier content was omitted. Malformed/incomplete
JSONL records are skipped; a disappeared transcript reports a read error.
OpenCode previews read up to 1,000 recent parts / 2 MiB; Cline previews read a
saved messages artifact up to 2 MiB and report when it is too large. SQLite
connections are read-only, with no schema migrations. Missing stores show an
empty list; incompatible schemas report an error.
The agent's original files are never edited by the panel.

Focus/surface changes and refreshes invalidate pending results. Resume checks
the current target again after process inspection and validates UUIDs or
OpenCode’s `ses_` identifiers.
Launch commands quote paths and reject terminal control characters.
Only the newly created shell receives an automatically submitted command;
existing agent processes are never restarted or sent an automatic Enter.

The native commands are supported by
[Codex's CLI and slash-command reference](https://developers.openai.com/codex/cli/reference/).
