<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux CLI skill: features and installation

Flowmux's CLI skill is **`flowmux-browser`**. Its source is
[`.agents/skills/flowmux-browser/SKILL.md`](../.agents/skills/flowmux-browser/SKILL.md).
The **Flowmux CLI** skill is a user guide for operating Flowmux, organized by task:

| Category | Covered features |
|---|---|
| Context | Connection checks, window/socket selection, pane and tab IDs |
| Workspaces and terminals | Workspace selection, splits, tabs, terminal input/output |
| Browser | Scoped/compact snapshots, native input, forms and typed outcomes, task handoff/results, files, media, screenshots and sampled recording |
| SSH | Connect, inspect status, reconnect, forward ports and preview |
| Agents and notifications | Live agent list, session names and completion notifications |
| Settings and integrations | Themes, skill installation, diagnostics and removal |
| GUI features | Code Review comments, terminal search, Files, editor and AI Usage |

The skill's existing `flowmux-browser` identifier and install directory are kept
for compatibility. Its title and description cover the full user guide. Flowmux
does not bundle development, design, skill-discovery or MCP-builder skills.

The optional [Flowmux team skill](agent-teams.md) adds role-based pane delegation
and result collection. Options → Skills manages both skills in separate sections.
Flowmux Team installs for Claude Code, Codex, Gemini CLI and Antigravity CLI
(`agy`), including its scripts and sample with the instructions.
All four can lead teams; worker sessions currently use Claude Code or Codex.

The CLI embeds that file at build time. Installation copies the embedded
payload; it does not download skills, run `npx`, or install a browser engine.
After changing the source, rebuild the CLI before updating installed copies.
The guide distinguishes CLI help/capabilities from live `browser diagnose` support,
pane IDs from browser surface IDs, and delivered input from verified outcomes.
Examples use complete snapshot refs and pin subsequent actions to the browser tab.

## Install from Settings

Open **Options → Skills → Flowmux CLI** and click **Install** beside the agent you use
(Codex, Claude Code, OpenCode, Gemini CLI, Antigravity CLI, or Cline). Each row shows whether the
Flowmux CLI skill is missing, installed, or different from the bundled version.
Click **Update** to replace a different version while keeping a backup; the row
shows the backup location. **Up to date** means the installed files match the
version bundled with the running app; click it to check again. This checks file
contents, not whether a newer app release is available. **Refresh status** checks
all rows for changes made outside Flowmux.
Each row displays its destination path. **Check Team setup** checks Python 3.9+,
the agent executable and lifecycle hooks without changing settings or signing in.
It does not prove that authentication or task execution will succeed. Expand **View skill
instructions** to inspect the exact bundled instructions without installing them.
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

On the first launch of a new app version (including in-app updates), Flowmux automatically refreshes existing
Flowmux CLI and Team skills in the known user installation paths, including existing
legacy Codex/shared copies. Missing entrypoints stay missing, even when helper files
remain after removal. Changes are backed up using the same rules as **Update**;
project skills, agent settings and hooks are untouched. Subsequent launches of the
same or an older version do not repeat a successful update. A pending macOS update
does not use the old running app's skill payload. Failed copies are retried on a later
launch and reported in the app log; check **Options → Skills** for their status.

## CLI installation

`flowmux agent install` installs for all six CLI-skill targets when `--agent` is omitted,
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
| `gemini` | `~/.gemini/skills/flowmux-browser/SKILL.md` | `GEMINI_CLI_HOME` replaces the home directory, before appending `.gemini/skills` |
| `cline` | `~/.cline/skills/flowmux-browser/SKILL.md` | Default user skills root |

Paths were checked against the official [Claude skills](https://code.claude.com/docs/en/skills)
and [configuration](https://code.claude.com/docs/en/env-vars) documentation,
[OpenCode](https://opencode.ai/docs/skills/),
[Codex](https://learn.chatgpt.com/docs/build-skills),
[Antigravity](https://antigravity.google/docs/skills), and
[Cline](https://docs.cline.bot/customization/skills) documentation on 2026-10-05.
Agents' additional project, plugin, or custom search directories remain under
that agent's management; Flowmux does not scan every possible discovery source.
`agy` is an alias for `antigravity`; it keeps the existing installation path.
Gemini CLI 0.64.0 discovery and the installed agy skill list were also checked
on 2026-10-22. Gemini also discovers `~/.agents/skills`, with precedence over
`~/.gemini/skills`. The UI and doctor report known shared copies without changing
them. Project skills can override both; check the agent's actual skill list.

```sh
flowmux agent install --agent codex
flowmux agent doctor --agent codex --json
flowmux agent install --agent codex --force
flowmux agent uninstall --agent codex --skills-only --json
flowmux agent install --agent gemini --skill flowmux-team
flowmux agent doctor --agent agy --skill flowmux-team --json
flowmux agent uninstall --agent agy --skill flowmux-team --skills-only
```

`--skill` accepts `flowmux-browser` (default) or `flowmux-team` for all three
operations. Without `--agent`, the Team selection covers Claude Code, Codex,
Gemini CLI and agy. Unsupported agent/skill combinations fail before writing.
Team removal always preserves wrappers and the other skill, including when
`--skills-only` is omitted. Team installation does not install or enable hooks.

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
When upgrading the guide, inspect these reported copies too: an old duplicate
can retain obsolete ref-token rules or omit supported browser commands. Updating
the managed copy does not update the duplicate. If its owner requests that copy's
update, back it up and synchronize it explicitly; preserve unrelated custom text
and user-managed links. Compare against the rebuilt CLI's payload, not an older
running app's bundled guide. Do not restart an active session to refresh discovery.

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
