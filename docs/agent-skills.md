<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux CLI skill: features and installation

Flowmux's built-in installer manages **`flowmux-browser`**. Its source is
[`.agents/skills/flowmux-browser/SKILL.md`](../.agents/skills/flowmux-browser/SKILL.md).
The **Flowmux CLI** skill is a user guide for operating Flowmux, organized by task:

| Category | Covered features |
|---|---|
| Context | Connection checks, window/socket selection, pane and tab IDs |
| Workspaces and terminals | Workspace selection, splits, tabs, terminal input/output |
| Browser | Open, snapshot, interact, wait, inspect and screenshot |
| SSH | Connect, inspect status, reconnect, forward ports and preview |
| Agents and notifications | Live agent list, session names and completion notifications |
| Settings and integrations | Themes, skill installation, diagnostics and removal |
| GUI features | Code Review comments, terminal search, Files, editor and AI Usage |

The skill's existing `flowmux-browser` identifier and install directory are kept
for compatibility. Its title and description cover the full user guide. Flowmux
does not bundle development, design, skill-discovery or MCP-builder skills.

The optional [Flowmux team skill](agent-teams.md) adds role-based pane delegation
and result collection. It is installed separately as a complete skill directory;
the built-in installer and Options → Skills still manage only `flowmux-browser`.

The CLI embeds that file at build time. Installation copies the embedded
payload; it does not download skills, run `npx`, or install a browser engine.
After changing the source, rebuild the CLI before updating installed copies.

## Install from Settings

Open **Options → Skills** and click **Install** beside the agent you use
(Codex, Claude Code, OpenCode, Antigravity, or Cline). Each row shows whether the
Flowmux CLI skill is missing, installed, or different from the bundled version.
Click **Update** to replace a different version while keeping a backup; the row
shows the backup location. **Refresh status** checks changes made outside Flowmux.
Hover over an agent row to inspect its destination path. Expand **View skill
contents** to inspect the exact bundled instructions without installing them.
The display name is **Flowmux CLI**; agents discover it under the existing
`flowmux-browser` identifier. A known extra Codex copy is shown in the row and
remains visible there after removal of the managed copy.

Click **Remove** beside an installed skill to delete that agent's Flowmux skill.
Modified content is backed up before deletion, and the row shows its backup path.
Other agents' copies, supporting files, hooks, wrappers and settings are preserved.
A file symlink is unlinked without deleting its target; linked skill directories
must be managed manually and are labeled in the row. After removal the row offers **Install** again.

Installation is per agent and affects only the Flowmux CLI skill. It does not
install the agent itself, change its hooks/settings, or restart running sessions.
Check the agent's skill list or open a new session to load the updated guide.
User-managed symlinks and filesystem errors are explained in the affected row.
Special files such as named pipes are rejected without waiting or modifying them.

## CLI installation

`flowmux agent install` installs for all five targets when `--agent` is omitted,
even if those agents have not been installed. Prefer an explicit target when
setting up one agent. `flowmux fix` instead checks for existing agent config
roots and skips absent ones; directory presence is a setup heuristic, not proof
that an agent is currently running. App installers print the repair command;
they do not automatically install these user-level integrations.

| `--agent` | Default `SKILL.md` path | Overrides |
|---|---|---|
| `claude-code` | `~/.claude/skills/flowmux-browser/SKILL.md` | `CLAUDE_CONFIG_DIR` replaces `~/.claude` |
| `opencode` | `~/.config/opencode/skills/flowmux-browser/SKILL.md` | `XDG_CONFIG_HOME` replaces `~/.config`; Flatpak uses the host default |
| `codex` | `~/.agents/skills/flowmux-browser/SKILL.md` | `CODEX_HOME` affects legacy-copy diagnostics and hooks, not this shared skills root |
| `antigravity` | `~/.gemini/config/skills/flowmux-browser/SKILL.md` | Shared Antigravity config root |
| `cline` | `~/.cline/skills/flowmux-browser/SKILL.md` | Default user skills root |

Paths were checked against the official [Claude skills](https://code.claude.com/docs/en/skills)
and [configuration](https://code.claude.com/docs/en/env-vars) documentation,
[OpenCode](https://opencode.ai/docs/skills/),
[Codex](https://learn.chatgpt.com/docs/build-skills),
[Antigravity](https://antigravity.google/docs/skills), and
[Cline](https://docs.cline.bot/customization/skills) documentation on 2026-10-05.
Agents' additional project, plugin, or custom search directories remain under
that agent's management; Flowmux does not scan every possible discovery source.
Gemini has lifecycle-hook support but is not a product-skill install target.

```sh
flowmux agent install --agent codex
flowmux agent doctor --agent codex --json
flowmux agent install --agent codex --force
flowmux agent uninstall --agent codex --skills-only --json
```

- `install`: identical content is a no-op. Different content fails without
  `--force`. Forced updates preserve the old bytes in a unique sibling named
  `SKILL.md.flowmux-backup-*`, then atomically replace `SKILL.md`. Text and JSON
  results report the backup path. Restore by copying the chosen backup back to
  `SKILL.md`; a subsequent doctor correctly reports drift.
- `doctor`: read-only comparison with this binary's payload. Exit code is 0
  only when all selected files match; missing, drifted, and unreadable files
  return 1. JSON includes error details. A duplicate legacy Codex copy is a
  warning and does not independently change this exit code.
- `uninstall --skills-only`: removes the selected active skill entry while
  preserving hooks and agent/tmux wrappers. A modified skill is backed up first.
  Supporting files and backups remain; an empty skill directory is removed.
- `uninstall` without `--skills-only`: also removes Flowmux-owned wrappers
  for those targets, including the tmux wrapper for Claude. Hook settings are
  separate: use `flowmux hooks uninstall` when intentionally removing hooks.
- `fix`: repairs multiple integrations, including skills, hooks, wrappers and
  desktop files. It backs up drifted skills and obsolete Codex sibling files.
  Use `agent install --agent …` for a skill-only change. A later `fix` can
  reinstall a removed skill; uninstall is not a persistent opt-out setting.

Multi-target operations run sequentially and may stop after an error, leaving
successful earlier operations in place. Re-run doctor to inspect the result;
installation is idempotent. `--json` describes successful operations; fatal
errors go to stderr with a nonzero exit status.

A symlinked `SKILL.md` or `flowmux-browser` directory is user-managed:
matching content is healthy and a no-op. If content differs or a link is
broken, installation/repair refuses to overwrite its source and doctor explains why.
Uninstall can unlink a `SKILL.md` symlink, including a dangling one, while
preserving its target. A symlinked skill directory must be managed manually.
Flowmux does not rewrite `AGENTS.md`, `CLAUDE.md`, other skill directories, or
Codex hook trust decisions during skill installation.

## Discovery and activation

A matching file proves installation, not that a running agent loaded it.
Check the agent's skill list after an update. Reload at a convenient session
boundary if needed; never restart an active user session just to validate setup.
Skills describe workflows, hooks report lifecycle events, and wrapper shims
establish launch identity. These are independent integrations.

Some agents also discover compatible agents' skill directories. Removing one
copy may leave another discoverable copy. Codex can show duplicate names rather
than merging them; Flowmux reports an existing `$CODEX_HOME/skills/flowmux-browser`
copy without modifying it. Inspect user and project copies before removing any.

For persistent activation controls, use the agent's own settings:
[Codex `skills.config`](https://learn.chatgpt.com/docs/build-skills),
[Claude skill visibility](https://code.claude.com/docs/en/skills),
[OpenCode skill permissions](https://opencode.ai/docs/skills/), or
[Cline's skills controls](https://docs.cline.bot/customization/skills).
Flowmux's `doctor` deliberately does not change these settings or claim a
file check proves that a skill is enabled.

`AGENTS.md` describes this repository's runtime/CLI contract; `CLAUDE.md`
contains contributor instructions. Neither is copied into users' other projects
by the skill installer.
