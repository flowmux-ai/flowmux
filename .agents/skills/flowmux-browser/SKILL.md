---
name: flowmux-browser
description: Operate Flowmux workspaces, terminal panes, in-app browser, SSH connections and notifications through its CLI. Use for Flowmux feature guidance or automation inside a Flowmux pane or with an explicitly supplied socket and pane.
---

# Flowmux CLI

Use the category matching the user's task. This skill describes product usage;
it does not install development tools or prescribe a software development process.

| Category | Tasks |
|---|---|
| [Context](#context) | Connect to the intended window; identify panes and tabs |
| [Workspaces and terminals](#workspaces-and-terminals) | Split, focus, resize, send input and read output |
| [Browser](#browser) | Open pages, snapshot, interact, wait and capture |
| [SSH](#ssh) | Connect, reconnect, forward ports and preview |
| [Agents and notifications](#agents-and-notifications) | Inspect live agents and report task completion |
| [Settings and integrations](#settings-and-integrations) | Themes, skills and diagnostics |
| [GUI features](#gui-features) | Code Review, search, Files, editor and AI Usage |

## Context

```bash
FLOWMUX_CLI="${FLOWMUX_BUNDLED_CLI_PATH:-flowmux}"
"$FLOWMUX_CLI" ping
"$FLOWMUX_CLI" --json identify
"$FLOWMUX_CLI" --json tree
"$FLOWMUX_CLI" --help
```

`FLOWMUX_PANE_ID` means the shell was launched inside Flowmux. `identify` reads
context; only a successful `ping` proves the GUI socket is reachable. If the
socket is stale, report it. Do not restart the user's app or silently select
another window. Outside Flowmux, use the user's supplied socket with the global
`--socket /path/to/socket` option **before** the command; use explicit pane IDs.

- `FLOWMUX_SOCKET_PATH`: window connection; different windows have different sockets.
- `FLOWMUX_WORKSPACE_ID` (`FLOWMUX_TAB_ID` alias): workspace UUID.
- `FLOWMUX_PANE_ID`: pane UUID; arguments accept bare UUID or `pane:<uuid>`.
- `FLOWMUX_SURFACE_ID`: tab UUID inside a pane; distinct from the pane ID.
- `FLOWMUX_BUNDLED_CLI_PATH`: optional matching CLI binary.

Read `tree` to select targets. In examples, `$PANE`, `$WORKSPACE`, `$SURFACE`
and `$FORWARD` are IDs from the corresponding command responses, not names.
`--json` gives structured responses. `capabilities` describes browser automation
support; use `--help` and `<command> --help` for the full command inventory.

## Workspaces and terminals

```bash
"$FLOWMUX_CLI" workspace ls
"$FLOWMUX_CLI" workspace current
"$FLOWMUX_CLI" workspace new --name Research --root /path/to/project
"$FLOWMUX_CLI" workspace focus "$WORKSPACE"
"$FLOWMUX_CLI" split "pane:$PANE" --right
"$FLOWMUX_CLI" split "pane:$PANE" --down
"$FLOWMUX_CLI" resize-pane "pane:$PANE" --ratio 0.5
"$FLOWMUX_CLI" focus-pane "pane:$PANE"
"$FLOWMUX_CLI" new-tab --workspace "$WORKSPACE" --cwd /path/to/project
"$FLOWMUX_CLI" focus-tab "$SURFACE" --pane "pane:$PANE"
```

`resize-pane` sets the parent split's first-child ratio. `new-tab` creates a
terminal tab; inspect the returned/tree IDs before operating on it.

To run a user-requested command in another terminal: inspect its screen, confirm
it is the intended shell, send text, send Enter, then read the result.

```bash
"$FLOWMUX_CLI" read-screen "pane:$PANE"
"$FLOWMUX_CLI" send-keys "pane:$PANE" 'pwd'
"$FLOWMUX_CLI" send-key Enter --pane "pane:$PANE"
"$FLOWMUX_CLI" read-screen "pane:$PANE"
```

`send-keys` requires an explicit target and accepts escapes; `send-key` sends a
named key to the terminal, **not an application UI shortcut**. `read-screen`
reads the visible terminal buffer, not all scrollback or browser/editor text.
Command submission does not prove completion; inspect its output. Avoid typing
into an active agent or interrupting an unrelated task.

Only close the user's intended target:

```bash
"$FLOWMUX_CLI" close-tab "$SURFACE" --pane "pane:$PANE"
"$FLOWMUX_CLI" close-pane "pane:$PANE"
```

The final workspace pane/tab is protected. Unsaved editor documents can prompt
for confirmation; a pending request is not a completed close. Do not bypass it.

## Browser

Inside Flowmux, prefer its visible in-app browser for web tasks. Do not launch
Playwright, Puppeteer or system Chromium merely to read a page.

The example uses `jq` to extract the opened pane ID. If unavailable, read the JSON
response directly instead of blindly installing dependencies.

```bash
OPEN=$("$FLOWMUX_CLI" --json browser open https://example.com) || exit 1
PANE=$(printf '%s' "$OPEN" | jq -er '.browser_pane_opened.pane') || exit 1
[ "$("$FLOWMUX_CLI" browser wait "pane:$PANE" --url example.com)" = true ] || exit 1
[ "$("$FLOWMUX_CLI" browser wait "pane:$PANE" --ready-state complete)" = true ] || exit 1
"$FLOWMUX_CLI" --json browser snapshot "pane:$PANE"
```

Open reuses a right-sibling browser pane as a new tab, or splits beside the
calling pane. A new WebView initially shows `about:blank`. Wait for the requested
URL and readiness before taking a snapshot. Wait prints `true` or `false`;
**check the value**, not just the exit code. Stop on timeout.

A snapshot returns a Markdown tree and `eN` refs. Use only refs actually returned
for the current page; these are independent examples, not a script to run blindly:

```bash
"$FLOWMUX_CLI" browser click "pane:$PANE" e3
"$FLOWMUX_CLI" browser fill "pane:$PANE" e1 "search terms"
"$FLOWMUX_CLI" browser select "pane:$PANE" e2 "option value"
"$FLOWMUX_CLI" browser type "pane:$PANE" "text for the focused field"
"$FLOWMUX_CLI" browser press "pane:$PANE" Enter
"$FLOWMUX_CLI" browser is-visible "pane:$PANE" e3
"$FLOWMUX_CLI" browser text "pane:$PANE" e3
"$FLOWMUX_CLI" browser value "pane:$PANE" e1
"$FLOWMUX_CLI" browser attr "pane:$PANE" e3 href
"$FLOWMUX_CLI" browser count "pane:$PANE" ".result-row"
"$FLOWMUX_CLI" browser screenshot "pane:$PANE" /tmp/flowmux-page.png
```

| Command family | Usage after `browser` |
|---|---|
| Navigation | `navigate PANE URL`, `back PANE`, `forward PANE`, `reload PANE` |
| Page metadata | `url PANE`, `title PANE` |
| Element interaction | `dblclick`, `hover`, `focus`, `blur`, `check`, `uncheck`: each takes `PANE REF` |
| State | `is-enabled PANE REF`, `is-checked PANE REF` |
| Scrolling | `scroll PANE REF X Y` |
| Wait | `wait PANE --selector CSS`, `--text TEXT`, `--url TEXT`, `--ready-state complete`, or `--js PREDICATE`; optional `--timeout-ms` / `--poll-ms` |
| Escape hatch | `eval PANE JAVASCRIPT` when normal verbs cannot express the task |

Refs belong to the latest snapshot of one browser tab. After navigation, reload,
tab switches or page changes, wait and take a fresh snapshot before using refs.
A ref-not-found error also requires a fresh snapshot. Never add tracking
attributes to the DOM; Flowmux resolves refs without modifying the page.

Screenshots cover the visible viewport. WebKitGTK/WKWebView do not expose CDP:
no device viewport emulation, network mocking, full-page tracing or screencast.
When a task requires those, explain the need for external tooling and keep the
user-visible URL/results in Flowmux. Cookie import currently extracts/counts
cookies; it does not insert them into the WebView.

## SSH

Run these commands from the local Flowmux context. Remote shells do not inherit
its pane/socket context. Replace the sample host and ports with the user's target.

```bash
"$FLOWMUX_CLI" ssh connect user@host --cwd /srv/project
"$FLOWMUX_CLI" ssh status --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh reconnect --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh forward add --workspace "$WORKSPACE" --remote-port 3000 --local-port 3000
"$FLOWMUX_CLI" ssh forward list --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh preview "$FORWARD" --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh forward remove "$FORWARD" --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh disconnect --workspace "$WORKSPACE"
```

Workspace creation does not prove SSH authentication succeeded: inspect `status`
and the authentication terminal. `connect` also accepts `--port`, `--user`,
`--identity-file`, `--config-file`, `--name`, `--tmux`, and a command after `--`.
That command runs once; reconnect does not replay it. `--tmux` requires remote
tmux; without persistence, disconnection can end remote processes.
`ssh preview` is Linux-only. Remote Files/editor/worktrees/project commands and
remote hook installation are not supported.

## Agents and notifications

```bash
"$FLOWMUX_CLI" --json agents
"$FLOWMUX_CLI" session-name
"$FLOWMUX_CLI" notify --pane "pane:$PANE" --title "Task update" --level info "Ready for review"
"$FLOWMUX_CLI" notify-complete --agent Codex --pane "pane:$PANE" --message "Requested task finished"
"$FLOWMUX_CLI" --json notifications list --unread
"$FLOWMUX_CLI" notifications jump-to-unread
```

`agents` reports live agents; saved session history is separate. Claude session
names help map panes to sessions. If Claude's `ListAgents` and `SendMessage`
tools are available and the task calls for coordination, verify the name with
`ListAgents` before messaging; a user rename can make the mapping stale.
Notifications appear in Flowmux/desktop notification surfaces; they are not
messages to another agent. `notifications open ID` focuses the source pane,
`mark-read ID` marks without focusing, and `clear` removes all notifications.
Do not simulate lifecycle hooks to make an agent appear active.

`claude-teams --count N --root PATH` starts 1–8 Claude agents in panes. Use it
only when the user requests a team; listing agents does not require launching any.

## Settings and integrations

For users, open **Options → Skills** and click **Install** next to the agent they
use. The same tab offers **Update**, **Remove**, and **Refresh status**. Updates
and removal back up modified skill content and preserve agent hooks/settings.
**View skill contents** previews the bundled instructions. Agent skill lists use
its existing `flowmux-browser` identifier. A separate Codex copy shown in the row
can remain discoverable after removing this managed copy; do not delete user-owned
copies automatically. Each agent is managed separately. The equivalent CLI
commands are below.

```bash
"$FLOWMUX_CLI" theme path
"$FLOWMUX_CLI" theme import /path/to/example.theme
"$FLOWMUX_CLI" agent install --agent codex
"$FLOWMUX_CLI" --json agent doctor --agent codex
"$FLOWMUX_CLI" agent install --agent codex --force
"$FLOWMUX_CLI" agent uninstall --agent codex --skills-only
"$FLOWMUX_CLI" doctor
```

Install targets: `claude-code`, `opencode`, `codex`, `antigravity`, `cline`.
Omitting `--agent` selects all five. This user guide retains the
`flowmux-browser` skill name/path for existing installations. Installation copies
the CLI's bundled guide; no external skill catalog or browser download is needed.
Identical content is unchanged. `--force` backs up modified content before update;
`--skills-only` removal preserves wrappers/hooks and backs up modified text.
User-managed symlinks are not overwritten. `agent doctor` checks installed bytes,
not whether a running agent has loaded/enabled the skill. Use the agent's own
skill controls and reload at a convenient session boundary if needed.

`fix` repairs skills, hooks and wrappers for detected agent configurations, so use
it for requested setup/repair, not read-only diagnosis. It can reinstall a removed
skill. Hooks (`hooks setup` / `hooks uninstall`) are separate from skill text;
uninstall without `--skills-only` also removes owned wrappers. Installation does
not rewrite project `AGENTS.md` or `CLAUDE.md`.

## GUI features

These features have no dedicated public CLI command. Guide the user to the UI;
do not invent `flowmux review`, `search`, `editor` or `usage` commands. Shortcuts
below are defaults and can be customized in Options.

| Feature | How to use |
|---|---|
| Code Review | Pane menu/footer button or `Ctrl+Alt+E`; toggles back to the previous tab. Review working changes or recent commits, add code comments and send them to a selected running agent. Scope follows the pane's repository/worktree. |
| Terminal search | `Ctrl+Shift+F` with terminal focus searches that terminal. `Ctrl+Alt+Shift+F` or the magnifier next to Files searches all open terminal tabs, including SSH. Results jump/highlight; Refresh updates results. Only retained scrollback is searchable. |
| Files and editor | Files button; double-click a file to edit. `Ctrl+S` / macOS `Cmd+S` saves. Search with editor focus searches editor content/workspace instead of terminal output. |
| Worktrees | Worktrees sidebar manages repository worktrees; select the intended pane/project first. |
| AI Usage | AI Usage button opens/closes usage information; click again to dismiss. |
| Session history | `Ctrl+Alt+J`; saved sessions are distinct from the live agent list. |
| Workspace overview / pane zoom | `Ctrl+Alt+K` / `Ctrl+Alt+M`. |
| Preferences | Options controls themes, fonts and keybindings. |
