<div align="center">

# flowmux
![icon](resources/icons/flowmux-180.png)

**A Linux terminal for AI coding agents.**

[![Build](https://img.shields.io/github/actions/workflow/status/flowmux-ai/flowmux/release.yml?label=build)](https://github.com/flowmux-ai/flowmux/actions/workflows/release.yml)
[![Test](https://img.shields.io/github/actions/workflow/status/flowmux-ai/flowmux/test.yml?branch=main&label=test)](https://github.com/flowmux-ai/flowmux/actions/workflows/test.yml)
[![Latest release](https://img.shields.io/github/v/release/flowmux-ai/flowmux)](https://github.com/flowmux-ai/flowmux/releases/latest)

[Website](https://flowmux.org/) · [Releases](https://github.com/flowmux-ai/flowmux/releases/latest) · [Documentation](#documentation)

<img src="resources/screenshot/screenshot_1.gif" alt="flowmux overview" width="100%" />

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

## Features

- **Agent notifications** — know when Claude Code, Codex, OpenCode, Gemini CLI,
  or Antigravity CLI finishes or needs input.
- **Workspaces and split panes** — organize tasks and switch between them in overview mode.
- **Built-in browser** — let agents browse, click, and type through the CLI.
- **Repository tools** — browse files and Git worktrees, edit code, and check AI usage.
- **SSH workspaces** — work on remote hosts with the same tabs and splits.

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

- [Keyboard shortcuts](docs/keybindings.md) · [Configuration](docs/configuration.md)
- [Agent CLI and browser automation](AGENTS.md) · [SSH workspaces](docs/ssh-workspace-design.md)
- [Contributing](CONTRIBUTING.md)

## License

[GPL-3.0-or-later](LICENSE) · [Notices](NOTICE).
Inspired by [cmux](https://cmux.com/); an unofficial reimplementation,
not affiliated with cmux.
