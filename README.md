<div align="center">

# flowmux
![icon](resources/icons/flowmux-180.png)

**Agent Workflow Multiplexer Terminal** — *Go with the agents' flow.*

[![Build](https://img.shields.io/github/actions/workflow/status/flowmux-ai/flowmux/release.yml?label=build)](https://github.com/flowmux-ai/flowmux/actions/workflows/release.yml)
[![Test](https://img.shields.io/github/actions/workflow/status/flowmux-ai/flowmux/test.yml?branch=main&label=test)](https://github.com/flowmux-ai/flowmux/actions/workflows/test.yml)
[![Latest release](https://img.shields.io/github/v/release/flowmux-ai/flowmux)](https://github.com/flowmux-ai/flowmux/releases/latest)

[Website](https://flowmux.org/) · [Releases](https://github.com/flowmux-ai/flowmux/releases/latest) · [Documentation](#documentation)

<a href="https://flowmux.org/assets/flowmux-intro.mp4"><img src="resources/screenshot/flowmux-intro-preview.jpg" alt="Watch the flowmux introduction video" width="100%" /></a>

[▶ Watch the introduction video](https://flowmux.org/assets/flowmux-intro.mp4)

</div>

Run AI coding agents side by side, with terminals, browser tabs, and task
notifications in one window.

## Install

**Ubuntu 24.04+ · amd64**

```sh
curl -fsSL https://flowmux.org/install.sh | sh
flowmux fix
```

Open flowmux and launch your agents in terminal tabs. `flowmux fix` sets up
agent hooks; run it again after upgrades and restart running agent sessions.
Codex users must approve changed hooks in `/hooks`.

Uninstall: `sudo apt remove flowmux`.

On first launch, Flowmux shows a six-page welcome tour with screenshots of
notifications, pane splitting, session history, search, Files and the editor,
and workspace overview. Use `flowmux --first-screen` to show it again.

For read-only integration diagnostics, open **Options → Update → Integration
status → Run diagnostics**, or run `flowmux doctor` (`flowmux --json doctor`
for scripts). Both check agent skills and hooks, IPC connectivity, browser data,
and desktop installation. Rows tagged `fix` can be repaired with `flowmux fix`;
errors include details for manual investigation. The CLI exits with status 1
for `fix` or `error` rows; warnings alone do not change its exit status.

## Features

### Agent notifications

Know which agent needs you. When Claude Code, Codex, OpenCode, Gemini CLI,
or Antigravity CLI finishes a task or stops for approval, its workspace
lights up, the bell keeps the list, and a desktop notification pops up.
Click a notification to jump straight back to that agent.

<img src="resources/screenshot/claude_notification.gif" alt="agent notifications" width="100%" />

### AgentOffice

Open **AgentOffice** immediately left of **Agents bar** in the scrollable footer
of the side panel. Options stays fixed at the far left.
All workspaces fit into one adaptive map, with more space and seats for larger teams.
Rooms share adjoining boundaries, and their furniture reflows to fill each room's
shape without stretching characters or leaving gaps between offices.
Office signs follow workspace names live. Select a sign to enlarge that office,
or **All offices** to fit the whole map again. The design button cycles through
24 furnished designs: six themes with four spatial layouts each.
Speech bubbles show work, input waits, completion, rest, and session departure,
including the agent's reported task text. Agents follow clear paths between
desks, review positions, sofas, coffee spots, and the exit. There are 120 animal and
outfit combinations, with agent names and icons identifying Claude, Codex,
Gemini, and other agents. Select a character to return to its terminal; press
**Esc** to leave the office. Animation follows the desktop's reduced-motion setting.

### Browser tab

Browse next to your terminals, or let your agents do it. With the
`flowmux browser` CLI an agent can open a page, read it, fill in fields,
and click through it in a pane you can watch. Browser tabs share WebKit
session data within each flowmux profile, separately from your host browsers.

Linux also offers opt-in [WebKitWebDriver automation](docs/webkit-automation.md)
for Selenium clients and engine-level mouse/keyboard input in ephemeral browser
windows. It requires a WebKitGTK runtime built with WebDriver input enabled.

<img src="resources/screenshot/video_control_browser.gif" alt="browser control" width="100%" />

### Split panes and overview mode

Split right or down, zoom into one pane when you need focus, and start
another agent in a new tab. Overview mode (**Ctrl+Alt+K**) shows every
workspace at once so you can jump between tasks.

<img src="resources/screenshot/view_split.gif" alt="split panes and overview mode" width="100%" />

### Files, worktrees, and AI usage

Browse the repository and open files in the built-in editor. The worktree
panel lists every Git worktree with its branch and path. **Ctrl+Alt+U** shows
Claude and Codex usage, which also stays in the bar below the terminal.

<img src="resources/screenshot/usage_fileview_worktreeview.gif" alt="file and worktree views" width="100%" />

### Code Review

Review staged, unstaged, and new files in your terminal pane. Add comments
to code or whole files, then send the feedback with code context to a running
agent. Toggle Code Review with **Ctrl+Alt+E**.

<img src="resources/screenshot/flowmux-code-review.gif" alt="Code Review: comment on changes and send feedback to an agent" width="100%" />

### Search terminal output

Search output across workspaces and terminal tabs with **Ctrl+Alt+Shift+F**.
Each result shows its workspace and tab. Click a result or use the arrow keys
and Enter to jump to the matching line, even in a hidden tab.

<img src="resources/screenshot/flowmux-search-terminals.gif" alt="Search terminal output across workspaces and jump to matching lines" width="100%" />

### Themes and keybindings

Pick a built-in theme, from Dracula to GitHub Light, or set your own colors
and fonts. Every shortcut can be rebound in **Options**, and the change works
right away without a restart.

<img src="resources/screenshot/setting_theme.gif" alt="theme settings" width="100%" />

### Image and Markdown viewers

Ctrl+click an image path to open it, or double-click it in the file browser.
SVG stays sharp at any size and Lottie animations play right away. Markdown
opens as a formatted preview that refreshes every time you save, so you can
write in the built-in editor and watch the result side by side. Images require
[ThorVG](docs/setup.md#thorvg-image-viewer).

<img src="resources/screenshot/image_viewer.gif" alt="image viewer" width="100%" />
<img src="resources/screenshot/md_viewer.gif" alt="markdown live preview" width="100%" />

### Agent CLI

Control browser tabs, panes, and terminals through `flowmux` commands.
See the [CLI guide](AGENTS.md) for automation and JSON output.

### SSH workspaces

Work on remote hosts with the same tabs and splits. Right-click the side
panel → **New SSH Workspace**, or run `flowmux ssh connect user@host`.

## Build from source

From a repository checkout:

```sh
./install.sh
```

See the [setup guide](docs/setup.md#build-from-source) for prerequisites,
development commands, and macOS development builds.

## Verify & repair

```sh
flowmux doctor   # check setup
flowmux fix      # repair integrations
```

See [setup and troubleshooting](docs/setup.md) for optional image/media
support, hook configuration, and logs.

## Documentation

- [Documentation index](docs/README.md) · [Release history](https://github.com/flowmux-ai/flowmux/releases)
- [Keyboard shortcuts](docs/keybindings.md) · [Configuration](docs/configuration.md)
- [Agent CLI and browser automation](AGENTS.md) · [SSH workspaces](docs/ssh-workspaces.md)
- [Flowmux CLI skill: features and installation](docs/agent-skills.md) — install for your agent from **Options → Skills**.
- [Contributing](.github/CONTRIBUTING.md)

## License

[GPL-3.0-or-later](LICENSE) · [Notices](NOTICE).
[Third-party licenses and notices](docs/legal/THIRD_PARTY_LICENSES.md).
Inspired by [cmux](https://cmux.com/); an unofficial reimplementation,
not affiliated with cmux.
