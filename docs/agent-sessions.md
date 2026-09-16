<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent sessions

Click **Agent sessions** in the side-panel footer to open the right-side session
panel. While open, it follows the focused terminal tab's Claude Code, Codex,
OpenCode, Antigravity (`agy`), or Cline identity. Closing it leaves the file and
worktree panels as they were.

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

All five agents open a new tab in the saved project directory and preserve the
original tab and draft. The new tab uses the normal configured terminal shell;
exiting the agent returns to that shell in the selected project directory.
On Linux, OpenCode retains its XDG/config overrides,
Antigravity retains its home, and Cline retains its directory overrides and
`--config` / `--data-dir` arguments, including isolated sandbox storage.

Another session can be opened while the focused agent is working or waiting for
input; the original tab remains untouched. The currently active session cannot
be opened again from that tab. The session ID remains visible for manual use.

## Implementation and limits

This uses existing GTK panels, process identity, and terminal input; it needs
no model call. Database adapters reuse the workspace’s SQLite and URL libraries.
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

OpenCode's older JSON store and Cline's legacy VS Code task history are not read.
Archives and child/subagent sessions are excluded where the store identifies them.
Linux reads the running process's home/config directory overrides. Other platforms
use flowmux's environment. Remote SSH session history is not read from the local
machine.

File reads run off the GTK thread. Claude/Codex list scanning samples the beginning
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
Live integration was checked with Codex 0.154.0 and Claude Code 2.1.273 in an
isolated flowmux instance: selecting, previewing, switching both agents, changing
active tabs, and preserving nonempty inputs. Both agents' cross-project resume
was also verified in a new tab while preserving the original tab's Unicode draft.
OpenCode 1.18.27 and Cline CLI 3.0.62 were also checked with synthetic histories
in a running isolated flowmux: list, preview, native restoration in a new tab,
project directory, and original Unicode draft preservation. Antigravity's list,
summary preview, and new-tab `--conversation` launch were checked; full native
restoration could not be checked because the isolated agy home requires Google
login. No model request was submitted during these checks.
Parser and GTK tests cover missing
and damaged histories, Unicode, long records, child sessions, search, stale
results, busy agents, current sessions, and preservation of other panels.

Build with `cargo build -p flowmux`. Running `./target/debug/flowmux` opens a new
window with the updated UI; already-running windows keep their current binary.
