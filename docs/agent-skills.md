<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent skills: installation and maintenance

FlowMux ships **one product skill, `flowmux-browser`**. Its source is
[`.agents/skills/flowmux-browser/SKILL.md`](../.agents/skills/flowmux-browser/SKILL.md).
The CLI embeds that file at build time. Installation copies the embedded
payload; it does not download skills, run `npx`, or install a browser engine.
After changing the source, rebuild the CLI before updating installed copies.

## Product installation

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
that agent's management; FlowMux does not scan every possible discovery source.
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
- `uninstall` without `--skills-only`: also removes FlowMux-owned wrappers
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
FlowMux does not rewrite `AGENTS.md`, `CLAUDE.md`, other skill directories, or
Codex hook trust decisions during skill installation.

## Discovery and activation

A matching file proves installation, not that a running agent loaded it.
Check the agent's skill list after an update. Reload at a convenient session
boundary if needed; never restart an active user session just to validate setup.
Skills describe workflows, hooks report lifecycle events, and wrapper shims
establish launch identity. These are independent integrations.

Some agents also discover compatible agents' skill directories. Removing one
copy may leave another discoverable copy. Codex can show duplicate names rather
than merging them; FlowMux reports an existing `$CODEX_HOME/skills/flowmux-browser`
copy without modifying it. Inspect user and project copies before removing any.

For persistent activation controls, use the agent's own settings:
[Codex `skills.config`](https://learn.chatgpt.com/docs/build-skills),
[Claude skill visibility](https://code.claude.com/docs/en/skills),
[OpenCode skill permissions](https://opencode.ai/docs/skills/), or
[Cline's skills controls](https://docs.cline.bot/customization/skills).
FlowMux's `doctor` deliberately does not change these settings or claim a
file check proves that a skill is enabled.

## Repository-only development skills

The following tracked files help contributors. They are **not embedded in the
application or copied by `flowmux agent install` / `fix`**.

| Skill | Source | Intended use in this repository |
|---|---|---|
| `agent-browser` | [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) | Explicit Chrome/CDP or Electron work; ordinary FlowMux page tasks use the in-app browser |
| `design-taste-frontend` | [Leonxlnx/taste-skill](https://github.com/Leonxlnx/taste-skill) | Marketing/landing-page design; not a replacement for GTK UI conventions |
| `find-skills` | [vercel-labs/skills](https://github.com/vercel-labs/skills) | Discover optional tooling when needed; no automatic bulk installation |
| `mcp-builder` | [anthropics/skills](https://github.com/anthropics/skills) | Explicit MCP-server development; its evaluation script can contact APIs and run tools |

Their source files are in `.claude/skills/`. Other agents may read compatible
paths; do not describe them as universally installed for all five targets.
Each directory retains its upstream `LICENSE.txt`. Local changes currently
include narrowing `agent-browser` routing to respect `AGENTS.md` and fixing
MCP evaluation result handling. The MCP Python requirements are separate from
FlowMux's Rust build; do not install or execute them merely to run the app.

`skills-lock.json` records upstream sources and hashes for this development
collection. It is tracked provenance, not FlowMux's installer database, a
runtime dependency, or a verifier of locally patched skill bytes. Review a
skill update's actual diff, references, license and local adaptations before
committing. An upstream update can replace local fixes. Run the existing
MCP offline tests after changing its evaluator:

```sh
python3 -m unittest discover -s .claude/skills/mcp-builder/scripts -p 'test_*.py'
```

`AGENTS.md` is the agent-facing runtime/CLI contract. `CLAUDE.md` is the shared
development guide and links back to it. Neither should be installed into a
user's other repositories by the product installer.

## Additional skills worth considering

Prioritize focused FlowMux workflows over installing another general catalog:

1. **Isolated native UI validation** (repository skill): launching a separate
   instance with private state, short macOS socket paths, the main-thread GTK
   harness, and proof from mapped widgets. Existing entry points are in
   `scripts/test-ci.sh` and `docs/ci-reliability.md`.
2. **CI failure investigation** (repository skill): compare the failing commit
   and logs, reproduce through the shared OS gate, then verify the patch on
   the same commit. This could package the current CI reliability procedure.
3. **Workspace/terminal control** (optional future product skill): pane/surface
   IDs, `tree`, `read-screen`, and scoped terminal operations already documented
   in `AGENTS.md`. Keep browser triggers separate; prove command examples and
   session-preservation rules before adding another embedded payload.

These are proposals, not new installed skills. Native agent activation and
explicit user task scope still govern their use.

## Regression verification

`crates/flowmux-cli/tests/agent_skills.rs` invokes the built CLI in private home,
config, data, cache and runtime directories. It covers all five default
install targets, repeated install, drift/refusal/forced update, retained backups,
selective removal, wrapper preservation/removal, reinstall, JSON output,
custom config roots, Flatpak path resolution, and Codex duplicate preservation.
Unit tests additionally cover linked directories and dangling links.

```sh
cargo test -p flowmux-cli --bin flowmuxctl --test agent_skills --locked
cargo test -p flowmux-config --locked paths::tests
```

These tests prove CLI filesystem behavior without contacting an AI service or
restarting user sessions. Flatpak path tests simulate environment routing;
they do not replace sandbox-package or each agent's native discovery tests.

### Audit evidence (2026-10-05)

Five added regression scenarios failed before the installer changes: lost update
backups, lost modified files on uninstall, overwritten symlink targets, writes
through linked skill directories, and dangling links misreported as missing.
They pass after the changes. Review also retained already-current symlinks as
healthy, verified atomic replacement preserves file mode and hardlink siblings,
and checked all four upstream paths recorded in `skills-lock.json` still exist.

The focused suite passes 353 tests (252 CLI unit, 7 CLI lifecycle/config
scenarios, 94 config unit). Strict Clippy for these two crates/all targets,
formatting, local documentation links, asset license checks and the existing
MCP offline evaluator test pass.

The wider default-member run reports 1,070 passed, 5 failed and 8 ignored on this
Mac. Each failure also reproduces in an untouched `6051b406` checkout:

- `pty_tee_restores_shared_stdout_flags`
- `pty_tee_preserves_input_queued_before_startup`
- `pty_tee_delivers_final_output_in_order_after_outer_eof`
- `pty_tee_outer_eof_kills_signal_ignoring_inner_group` (reproduced separately)
- `ssh::tests::remote_bootstrap_preserves_literal_arguments_and_rejects_missing_cwd`

These failures are not fixed by this skill-management patch. Two rustdoc
crate-resolution errors appeared while the baseline comparison reused the
build cache; the separate current-tree `cargo test --locked --doc` run verifies
that part again. A subsequent stale-path result after that shared-cache
comparison prompted a fresh, independent target-directory build: all 353
focused tests pass there too. Use distinct target directories for baseline
comparisons. The affected local CLI/config build artifacts were also cleaned
and rebuilt. This audit does not claim a green full macOS default suite,
Linux execution, or native discovery by every external agent.
