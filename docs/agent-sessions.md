<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent sessions

Click **Agent sessions** in the side-panel footer to open the right-side session
panel. While open, it follows the focused terminal tab's Claude Code or Codex
identity. Closing it leaves the file and worktree panels as they were.

The list includes local sessions from all projects, ordered by transcript
modification time. Search matches titles, recent-message summaries, directories,
and session IDs. Select a row to read its conversation. **Refresh sessions**
reloads the on-disk history.

To switch sessions, finish the current agent task and clear its input field.
Select a session, click **Resume in focused tab**, and press **Enter** in the
agent terminal. The button inserts `/resume <UUID>`; the native agent confirms
and performs the switch. Existing drafts, recognized permission dialogs, and
busy agents are left untouched. Unknown terminal prompt layouts are rejected;
the session ID remains visible for use with the agent's native resume command.

## Implementation and limits

This uses existing GTK panels, process identity, and terminal input; it needs
no new dependency or model call. Titles come from native titles or the first
user message. The summary is a recent conversation excerpt, not a generated
summary. Only user/assistant text and image placeholders appear in previews;
tool payloads, system instructions, and reasoning are omitted.

History is read from Claude's `projects/*/*.jsonl` and Codex's
`sessions/YYYY/MM/DD/*.jsonl`, with Codex names from `session_index.jsonl`.
Archives and subagent transcripts are excluded. Linux reads the running
process's `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, and `HOME` overrides. Other platforms
use flowmux's environment. Remote SSH session history is not read from the local
machine.

File reads run off the GTK thread. List scanning samples the beginning and end
of each transcript. Conversation previews show the latest 2 MiB of a transcript
and explicitly report when earlier content was omitted. Malformed/incomplete
JSONL records are skipped; a disappeared transcript reports a read error.
The agent's original files are never edited by the panel.

Focus/surface changes and refreshes invalidate pending results. Resume checks
the current target again after process inspection, accepts UUIDs only, and
never automatically submits terminal input or restarts an agent process.

The native commands are supported by
[Codex's CLI and slash-command reference](https://developers.openai.com/codex/cli/reference/).
Live integration was checked with Codex 0.154.0 and Claude Code 2.1.273 in an
isolated flowmux instance: selecting, previewing, switching both agents, changing
active tabs, and preserving nonempty inputs. Parser and GTK tests cover missing
and damaged histories, Unicode, long records, child sessions, search, stale
results, busy agents, current sessions, and preservation of other panels.

Build with `cargo build -p flowmux`. Running `./target/debug/flowmux` opens a new
window with the updated UI; already-running windows keep their current binary.
