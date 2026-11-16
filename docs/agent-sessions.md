<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent sessions

## Running agents and closing tabs

On Linux, **New Agent**, **Resume in new tab**, and Session Flow resume/fork
start local managed executions in the independent Agent Host. Closing their tab
detaches the view; closing Flowmux leaves the execution running. Reopening it
connects to the same process. An already running matching native session is
reused. Host failure, OS shutdown, and expired external credentials can still
interrupt work. macOS and Flatpak lifetime behavior has not been verified.

The **Agents** list includes executions without a tab as **Background**.
**Connecting** means the relay has not acknowledged the initial screen transfer.
**Open** means a connected tab is hidden; **Visible** means its tab and workspace
are shown. Drag the row's handle to another pane to move or open its view. A normal
shell command such as `claude` remains **Tab-bound** and may stop when its tab
or Flowmux closes. Existing live terminal processes are not automatically
converted; use **New Agent** or the managed CLI for a new persistent execution.

Clicking an execution open in this window focuses its existing tab. Clicking a **Background**
execution first asks where to open it. **Agent actions → Open in… / Move to…**
offers this window's local terminal panes, including other workspaces, or
**New workspace** in the agent's project directory. The current workspace's
terminal pane is selected by default when available. Confirming reconnects or
moves the same execution; cancelling leaves the layout alone. If the selected
pane closes before confirmation, choose again. A view open in another window can
move into the selected pane using the same local Agent Host. SSH panes are excluded.

Drag the handle beside an execution to a pane body to open or move its tab.
Drop on the left or right half of a tab to insert before or after it, including
reordering within the same pane. A Background execution reconnects to the same
process at that position. Dropping on its own tab leaves its order unchanged.
The handle also supports dragging between Flowmux windows on the same local Host.
The original view keeps input until the destination writes its initial screen
and the destination GUI admits the move. Closing the destination, a failed screen
write, or a connection timeout leaves the original view usable. An uncertain
control response is not automatically retried.

Dragging a managed Agent tab outside its window opens the same execution in a
new Flowmux window. The original tab closes after the new view is ready; other
tabs stay in place. If the new window cannot start, the original view remains
usable. Closing the detached tab or window leaves the execution in Background.

When restoring a saved layout, Flowmux checks the surviving Host's last committed
view. An obsolete saved Agent tab is removed without taking the execution from
its current view or Background state; ordinary terminal tabs are preserved.
The winning saved tab reconnects automatically after a GUI crash. Use Agents
to explicitly open a new view when an obsolete saved tab has been removed.

An unterminated synchronized screen update is released after the terminal
parser's 150ms deadline, including without further provider output. PTY closure
also releases the final screen. OSC control strings exceeding 64KiB are discarded;
normal output resumes at their terminator or the next escape sequence.

```sh
flowmux agent run --background --cwd /path/to/project -- claude
flowmux agent ls
flowmux agent open INSTANCE_ID --pane PANE_ID
flowmux agent interrupt INSTANCE_ID
flowmux agent terminate INSTANCE_ID
# Only after normal termination has timed out (5 seconds):
flowmux agent terminate INSTANCE_ID --force
```

Omit `--background` to open the new execution in the current pane, or provide
`--pane PANE_ID`. **Agent actions** provides the same interrupt and termination
controls. Ctrl+C may interrupt work or exit the provider. Termination can lose
work; GUI termination requires confirmation. Normal termination shows
**Stopping** until observed exit. A timeout enables explicit force termination;
it does not automatically kill the execution. A CLI acceptance receipt does not
mean the process has exited; check `agent ls`. If a control response is lost,
inspect the execution before retrying. Controls never start a missing Host.
Automatic retirement of idle agents is currently disabled.

Inside a managed execution, `flowmux identify` and the Flowmux Team context
check resolve the current view through the Host, including after a drag or GUI
restart. Inherited pane IDs are not used. An optional `identify --session ID`
must match the observed native session. A detached execution returns
`no_attached_view`; it never selects another pane. Missing or invalid managed
identity and an unavailable Host fail without a GUI fallback.

Managed browser operations and terminal read, input, focus, resize, split and
tab operations also go through the Host. Implicit pane/workspace arguments use
the verified source; explicit targets remain explicit. The Host checks the caller
and source again for every operation, and GTK checks the actual source before
applying an effect. A move during a CLI operation or batch fails clearly instead
of using the old pane; commands are never automatically replayed. Long browser
operations hold attachment transfer until their response. `workspace current`
returns the source workspace, including when another workspace is visible.
`workspace new` creates a workspace in that same GUI and waits for its terminal
to be materialized. `--background` keeps the visible workspace; without it the
new workspace becomes visible. `--root`, `--name` and `--team` retain their usual
meaning; an omitted root uses the calling process's working directory.
`workspace focus ID` activates that exact workspace in the current GUI without
moving or relaunching the Agent. Both commands require an attached view and
fail without choosing another window when the execution is in Background.
An explicit `--socket` must name the managed Host or its current GUI. Inherited
native session variables do not redirect these commands; use `identify --session`
to request an explicit native session match. `session-name` also uses the verified
workspace and surface.

`agent open INSTANCE_ID` opens that execution in the verified source pane.
`--pane ID` opens or moves its view to that exact pane in the same GUI. Opening
the calling execution reuses its tab; opening another execution preserves the
caller identity. A detached caller cannot use this command to choose a window
or reconnect itself. A successful open response waits for the owning relay's
initial screen ACK and the GUI's verification of that relay. Input stays disabled
until then. The ACK confirms writing the snapshot to the relay PTY; it does not
prove VTE has rendered every byte. A closed, moved or failed destination returns
an error while preserving the execution. Connection errors remain visible in the
tab; reopen it from Agents to retry. After a timeout, close that tab first.
Failures never trigger a replacement execution or automatically repeat the open.

Commands outside this routing scope fail with
`operation does not support managed context`. They are not sent to another GUI.
This routing is not a cross-window move transaction.

Native lifecycle hooks store input requests and turn-completion notifications in
the Host, even with no tab or GUI. Connected GUIs share the notification history,
read state and deletion. Reopening a GUI imports history without replaying old
desktop notifications. Desktop delivery is claimed once across registered GUIs;
failed or ambiguous delivery is not retried. The Host keeps at most 50 entries
within a 128 KiB serialized log, evicting read entries first. Opening a notification
follows the execution's current view; a Background execution shows the destination
picker without starting another provider process.

Managed `notify`, `notify-complete` and `notify-stream` also store notifications
in the Host without requiring a tab or GUI. Omit `--pane`: notifications belong
to the calling execution and follow its verified view when opened. Inherited
pane or native-session variables cannot redirect them; copying the managed
environment into an unrelated process does not grant access. An explicit
`--socket` must name the Host or the currently verified GUI. A headless execution
can name only its Host. Each stream notification uses a fresh authenticated
connection, so silence beyond the IPC idle timeout does not lose the next event.
Ambiguous sends are not replayed. Explicit completion text does not change the
Agent's lifecycle state or establish that its work is finished.

## Saved sessions

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

All six agents open a tab in the saved project directory and preserve the
original tab and draft. Managed launches use the configured shell's startup
environment and then execute the provider. Exiting the provider ends that
execution; it does not open a replacement shell.
On Linux, OpenCode retains its XDG/config overrides,
Antigravity retains its home, Gemini CLI retains `GEMINI_CLI_HOME`, and Cline retains its directory overrides and
`--config` / `--data-dir` arguments, including isolated sandbox storage.
Automatic window restoration also retains Gemini and Cline storage overrides
captured from their running process.

Another session can be opened while the focused agent is working or waiting for
input; the original tab remains untouched. Selecting a live managed session
focuses its existing view or reconnects its Background execution. The session ID
remains visible for manual use.

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
