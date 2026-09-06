// SPDX-License-Identifier: GPL-3.0-or-later
//! `flowmux hooks setup` — register flowmux's hook entries with each
//! supported agent so its lifecycle events (Stop / Notification / …)
//! call `flowmux hooks <agent> <event>` and surface as system + bell
//! popover notifications.
//!
//! Idempotent: every entry we own carries a `flowmux-hook` marker
//! string in its `command`, so a re-run refreshes it in place and removes
//! duplicates. Other handlers and their array positions are preserved; Codex
//! uses those positions when tracking hook trust.
//!
//! Supported targets (mirroring cmux):
//! - **Claude Code** — native lifecycle hooks in `~/.claude/settings.json`.
//! - **Codex CLI**   — native lifecycle hooks in `~/.codex/hooks.json`.
//! - **OpenCode**    — `~/.config/opencode/plugins/flowmux-session.js`.
//! - **Gemini CLI**  — `~/.gemini/settings.json` lifecycle hooks.
//! - **Antigravity** — owned plugin in `~/.gemini/config/plugins/flowmux/`.
//! - **Cline**       — skill only; obsolete file hooks are removed on repair.

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Marker the hook installer drops into every command line we generate
/// so a re-run can identify (and prune) previous flowmux entries
/// without touching user-authored entries. Mirrors cmux's
/// `cmux-claude-hook-marker` convention.
pub const FLOWMUX_HOOK_MARKER: &str = "flowmux-hook";

/// Plugin source-marker for the OpenCode JS plugin file. Lets a re-run
/// detect that the file is owned by flowmux and may be overwritten.
pub const FLOWMUX_OPENCODE_PLUGIN_MARKER: &str = "flowmux-opencode-session-plugin v5";
const FLOWMUX_OPENCODE_PLUGIN_MARKER_PREFIX: &str = "flowmux-opencode-session-plugin";

/// One agent flowmux knows how to install hooks for. Same enum shape
/// as `agent::Target` so future merges can collapse them, but kept
/// separate today to keep the SKILL installer focused on text payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookTarget {
    Claude,
    Codex,
    OpenCode,
    Gemini,
    Antigravity,
    Cline,
}

impl HookTarget {
    pub const ALL: &'static [HookTarget] = &[
        HookTarget::Claude,
        HookTarget::Codex,
        HookTarget::OpenCode,
        HookTarget::Gemini,
        HookTarget::Antigravity,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            HookTarget::Claude => "claude",
            HookTarget::Codex => "codex",
            HookTarget::OpenCode => "opencode",
            HookTarget::Gemini => "gemini",
            HookTarget::Antigravity => "antigravity",
            HookTarget::Cline => "cline",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.slug() == s)
    }
}

/// Outcome of installing one target — exposed so a CLI doctor / setup
/// command can render a per-agent summary table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookInstallStatus {
    /// Wrote (or rewrote) the hook entries.
    Installed,
    /// The agent's home directory is not present — flowmux skipped this
    /// target rather than erroring.
    Skipped,
}

#[derive(Debug)]
pub struct HookInstallReport {
    pub target: HookTarget,
    pub status: HookInstallStatus,
    pub touched_paths: Vec<PathBuf>,
}

/// Read-only introspection result for `flowmux doctor`. Mirrors the
/// installer outcomes so the doctor view can report what `flowmux fix`
/// will (or won't) change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookCheckStatus {
    /// Agent home dir is absent. `flowmux fix` will skip this target.
    NoAgentHome,
    /// Agent home is present but no flowmux hook entry was found.
    Missing,
    /// flowmux hook entries are present and look correct.
    Installed,
    /// flowmux hook entries are partially present or stale (e.g.
    /// a previous install left the marker on disk but the matching
    /// settings entry is gone). `flowmux fix` re-syncs.
    Drift,
    /// Could not read the agent's config (parse error, permission, …).
    Error(String),
}

#[derive(Debug, Clone)]
pub struct HookCheckEntry {
    pub target: HookTarget,
    pub status: HookCheckStatus,
    /// Files we inspected — useful for the doctor output and for
    /// telling the user where `flowmux fix` will write.
    pub paths: Vec<PathBuf>,
}

/// Inspect every supported target without touching any file. Safe to
/// call from `flowmux doctor`.
pub fn check_all() -> Vec<HookCheckEntry> {
    HookTarget::ALL.iter().map(|t| check(*t)).collect()
}

/// Inspect a single target.
pub fn check(target: HookTarget) -> HookCheckEntry {
    match target {
        HookTarget::Claude => check_claude(),
        HookTarget::Codex => check_codex(),
        HookTarget::OpenCode => check_opencode(),
        HookTarget::Gemini => check_gemini(),
        HookTarget::Antigravity => check_antigravity(),
        HookTarget::Cline => check_cline(),
    }
}

fn check_claude() -> HookCheckEntry {
    let path = match claude_settings_path() {
        Some(p) => p,
        None => return entry(HookTarget::Claude, HookCheckStatus::NoAgentHome, vec![]),
    };
    let agent_home_present = path.parent().map(|p| p.exists()).unwrap_or(false);
    if !agent_home_present {
        return entry(HookTarget::Claude, HookCheckStatus::NoAgentHome, vec![path]);
    }
    if !path.exists() {
        return entry(HookTarget::Claude, HookCheckStatus::Missing, vec![path]);
    }
    let root: Value = match read_json_or_empty_object(&path) {
        Ok(v) => v,
        Err(e) => {
            return entry(
                HookTarget::Claude,
                HookCheckStatus::Error(e.to_string()),
                vec![path],
            )
        }
    };
    let hooks = root.get("hooks").and_then(|h| h.as_object());
    let mut owned_events = 0usize;
    for event in CLAUDE_EVENTS {
        let arr = hooks
            .and_then(|h| h.get(event.name))
            .and_then(|v| v.as_array());
        if let Some(arr) = arr {
            if arr.iter().any(|entry| claude_entry_matches(entry, *event)) {
                owned_events += 1;
            }
        }
    }
    let status = match owned_events {
        0 => HookCheckStatus::Missing,
        n if n == CLAUDE_EVENTS.len() => HookCheckStatus::Installed,
        _ => HookCheckStatus::Drift,
    };
    entry(HookTarget::Claude, status, vec![path])
}

fn check_codex() -> HookCheckEntry {
    let home = match codex_home() {
        Some(h) => h,
        None => return entry(HookTarget::Codex, HookCheckStatus::NoAgentHome, vec![]),
    };
    let hooks_path = home.join("hooks.json");
    let config_path = home.join("config.toml");
    if !home.exists() {
        return entry(
            HookTarget::Codex,
            HookCheckStatus::NoAgentHome,
            vec![hooks_path, config_path],
        );
    }
    let root = match read_json_or_empty_object(&hooks_path) {
        Ok(root) => root,
        Err(error) => {
            return entry(
                HookTarget::Codex,
                HookCheckStatus::Error(error.to_string()),
                vec![hooks_path, config_path],
            )
        }
    };
    if let Err(error) = validate_codex_hooks_shape(&root) {
        return entry(
            HookTarget::Codex,
            HookCheckStatus::Error(error.to_string()),
            vec![hooks_path, config_path],
        );
    }
    let legacy_notify = match codex_config_has_owned_notify(&config_path) {
        Ok(owned) => owned,
        Err(error) => {
            return entry(
                HookTarget::Codex,
                HookCheckStatus::Error(error.to_string()),
                vec![hooks_path, config_path],
            )
        }
    };
    match codex_config_hooks_disabled(&config_path) {
        Ok(true) => {
            return entry(
                HookTarget::Codex,
                HookCheckStatus::Error(
                    "Codex user hooks are explicitly disabled in config.toml".into(),
                ),
                vec![hooks_path, config_path],
            )
        }
        Ok(false) => {}
        Err(error) => {
            return entry(
                HookTarget::Codex,
                HookCheckStatus::Error(error.to_string()),
                vec![hooks_path, config_path],
            )
        }
    }
    let status = codex_check_status(&root, legacy_notify);
    entry(HookTarget::Codex, status, vec![hooks_path, config_path])
}

fn codex_check_status(root: &Value, legacy_notify: bool) -> HookCheckStatus {
    let installed = CODEX_EVENTS
        .iter()
        .filter(|event| codex_matching_entry_count(root, **event) == 1)
        .count();
    let owned = codex_owned_hook_count(root);
    match (installed, owned, legacy_notify) {
        (count, owned, false) if count == CODEX_EVENTS.len() && owned == count => {
            HookCheckStatus::Installed
        }
        (0, 0, false) => HookCheckStatus::Missing,
        _ => HookCheckStatus::Drift,
    }
}

fn check_opencode() -> HookCheckEntry {
    let homes: Vec<PathBuf> = opencode_homes()
        .into_iter()
        .filter(|h| h.exists())
        .collect();
    if homes.is_empty() {
        let stub = opencode_home()
            .map(|h| h.join("opencode.json"))
            .into_iter()
            .collect();
        return entry(HookTarget::OpenCode, HookCheckStatus::NoAgentHome, stub);
    }
    let mut all_paths: Vec<PathBuf> = Vec::with_capacity(homes.len() * 2);
    let mut every_installed = true;
    let mut every_missing = true;
    for home in &homes {
        let plugin_path = home.join("plugins").join("flowmux-session.js");
        let legacy_path = home.join("plugins").join("flowmux-session.mjs");
        let legacy_owned =
            fs::read_to_string(&legacy_path).is_ok_and(|source| opencode_plugin_is_owned(&source));
        let opencode_json = home.join("opencode.json");
        all_paths.push(plugin_path.clone());
        all_paths.push(opencode_json.clone());

        let plugin_exists = plugin_path.exists();
        let plugin_ok = plugin_exists
            .then(|| fs::read_to_string(&plugin_path).ok())
            .flatten()
            .map(|source| opencode_plugin_is_current(&source))
            .unwrap_or(false);
        let registered = if opencode_json.exists() {
            match read_json_or_empty_object(&opencode_json) {
                Ok(v) => v
                    .get("plugin")
                    .and_then(|p| p.as_array())
                    .map(|arr| {
                        arr.iter().any(|p| {
                            p.as_str()
                                .map(|s| s.contains("flowmux-session"))
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false),
                Err(e) => {
                    return entry(
                        HookTarget::OpenCode,
                        HookCheckStatus::Error(e.to_string()),
                        all_paths,
                    )
                }
            }
        } else {
            false
        };
        let this_installed = plugin_ok && !registered && !legacy_owned;
        let this_missing = !plugin_exists && !registered && !legacy_owned;
        every_installed &= this_installed;
        every_missing &= this_missing;
    }
    let status = if every_installed {
        HookCheckStatus::Installed
    } else if every_missing {
        HookCheckStatus::Missing
    } else {
        HookCheckStatus::Drift
    };
    entry(HookTarget::OpenCode, status, all_paths)
}

fn check_gemini() -> HookCheckEntry {
    let path = match gemini_settings_path() {
        Some(path) => path,
        None => return entry(HookTarget::Gemini, HookCheckStatus::NoAgentHome, vec![]),
    };
    if !gemini_is_installed(&path, agent_has_real_binary("gemini")) {
        return entry(HookTarget::Gemini, HookCheckStatus::NoAgentHome, vec![path]);
    }
    if !path.exists() {
        return entry(HookTarget::Gemini, HookCheckStatus::Missing, vec![path]);
    }
    let root = match read_json_or_empty_object(&path) {
        Ok(root) => root,
        Err(error) => {
            return entry(
                HookTarget::Gemini,
                HookCheckStatus::Error(error.to_string()),
                vec![path],
            )
        }
    };
    let hooks = root.get("hooks").and_then(Value::as_object);
    let installed = GEMINI_EVENTS
        .iter()
        .filter(|event| {
            hooks
                .and_then(|hooks| hooks.get(event.name))
                .and_then(Value::as_array)
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| gemini_entry_matches(entry, **event))
                })
        })
        .count();
    let status = match installed {
        0 => HookCheckStatus::Missing,
        count if count == GEMINI_EVENTS.len() => HookCheckStatus::Installed,
        _ => HookCheckStatus::Drift,
    };
    entry(HookTarget::Gemini, status, vec![path])
}

fn check_antigravity() -> HookCheckEntry {
    let plugin_dir = match antigravity_plugin_dir() {
        Some(path) => path,
        None => {
            return entry(
                HookTarget::Antigravity,
                HookCheckStatus::NoAgentHome,
                vec![],
            )
        }
    };
    if !antigravity_home_exists(&plugin_dir) {
        return entry(
            HookTarget::Antigravity,
            HookCheckStatus::NoAgentHome,
            antigravity_plugin_paths(&plugin_dir).to_vec(),
        );
    }
    check_antigravity_in(&plugin_dir)
}

fn check_antigravity_in(plugin_dir: &Path) -> HookCheckEntry {
    let [manifest_path, hooks_path] = antigravity_plugin_paths(plugin_dir);
    let paths = vec![manifest_path.clone(), hooks_path.clone()];
    let disabled = match antigravity_plugin_disabled(plugin_dir) {
        Ok(disabled) => disabled,
        Err(error) => {
            return entry(
                HookTarget::Antigravity,
                HookCheckStatus::Error(error.to_string()),
                paths,
            )
        }
    };
    if disabled {
        return entry(
            HookTarget::Antigravity,
            HookCheckStatus::Error("Antigravity flowmux plugin is explicitly disabled".into()),
            paths,
        );
    }
    if !manifest_path.exists() && !hooks_path.exists() {
        return entry(HookTarget::Antigravity, HookCheckStatus::Missing, paths);
    }
    let manifest = match read_json_or_empty_object(&manifest_path) {
        Ok(value) => value,
        Err(error) => {
            return entry(
                HookTarget::Antigravity,
                HookCheckStatus::Error(error.to_string()),
                paths,
            )
        }
    };
    let hooks = match read_json_or_empty_object(&hooks_path) {
        Ok(value) => value,
        Err(error) => {
            return entry(
                HookTarget::Antigravity,
                HookCheckStatus::Error(error.to_string()),
                paths,
            )
        }
    };
    let hook_group = hooks
        .as_object()
        .and_then(|root| root.get(ANTIGRAVITY_HOOK_GROUP));
    if !hook_group.is_some_and(antigravity_group_is_owned) {
        return entry(
            HookTarget::Antigravity,
            HookCheckStatus::Error(format!(
                "{} exists and is not managed by flowmux",
                plugin_dir.display()
            )),
            paths,
        );
    }
    let installed = manifest == antigravity_plugin_manifest()
        && hook_group.is_some_and(antigravity_group_matches);
    let status = if installed {
        HookCheckStatus::Installed
    } else {
        HookCheckStatus::Drift
    };
    entry(HookTarget::Antigravity, status, paths)
}

fn entry(target: HookTarget, status: HookCheckStatus, paths: Vec<PathBuf>) -> HookCheckEntry {
    HookCheckEntry {
        target,
        status,
        paths,
    }
}

/// Install hooks for a single target. Returns a report; errors are
/// reserved for genuine I/O / parse failures, not "agent isn't here"
/// (that maps to `Skipped`).
pub fn install(target: HookTarget, flowmux_bin: &str) -> Result<HookInstallReport> {
    match target {
        HookTarget::Claude => install_claude(flowmux_bin),
        HookTarget::Codex => install_codex(flowmux_bin),
        HookTarget::OpenCode => install_opencode(flowmux_bin),
        HookTarget::Gemini => install_gemini(flowmux_bin),
        HookTarget::Antigravity => install_antigravity(flowmux_bin),
        HookTarget::Cline => uninstall_cline(),
    }
}

/// Remove flowmux entries from a target. Mirrors `install` for users
/// who want to opt out without manually editing every file.
pub fn uninstall(target: HookTarget) -> Result<HookInstallReport> {
    match target {
        HookTarget::Claude => uninstall_claude(),
        HookTarget::Codex => uninstall_codex(),
        HookTarget::OpenCode => uninstall_opencode(),
        HookTarget::Gemini => uninstall_gemini(),
        HookTarget::Antigravity => uninstall_antigravity(),
        HookTarget::Cline => uninstall_cline(),
    }
}

// ---- Agent wrapper shims -------------------------------------------

/// Agents that get a PID-capturing wrapper shim. The GUI prepends the
/// shim dir to a PTY's `PATH`, so typing `claude` / `codex` resolves to
/// these scripts first. They export `FLOWMUX_AGENT_PID=$$` and the canonical
/// agent name (read by the hooks), then `exec` the real binary, so they are
/// otherwise fully transparent. Lifecycle presence comes from each agent's
/// native hook (or the process-tree fallback), so the shim never emits a
/// competing synthetic SessionStart.
pub(crate) const SHIM_AGENTS: &[&str] = &["claude", "codex", "opencode", "gemini", "cline", "agy"];

/// Body of a wrapper shim for `agent`. Skips flowmux-managed shims when
/// resolving the real binary so it never re-execs itself or another copy.
pub(crate) fn shim_script(agent: &str) -> String {
    let canonical_agent = if agent == "agy" { "antigravity" } else { agent };
    let claude_session_name = if agent == "claude" {
        r#"
if [ -n "${FLOWMUX_SURFACE_ID:-}" ]; then
  inject_name=1
  expect_name=0
  for arg in "$@"; do
    if [ "$expect_name" = 1 ]; then
      export FLOWMUX_CLAUDE_SESSION_NAME="$arg"
      expect_name=0
      continue
    fi
    case "$arg" in
      -n|--name) inject_name=0; expect_name=1 ;;
      --name=*) inject_name=0; export FLOWMUX_CLAUDE_SESSION_NAME="${arg#--name=}" ;;
      -n?*) inject_name=0; export FLOWMUX_CLAUDE_SESSION_NAME="${arg#-n}" ;;
      --bare|-h|--help|-v|--version) inject_name=0 ;;
    esac
  done
  case "${1:-}" in
    agents|auth|auto-mode|doctor|gateway|import|install|mcp|plugin|plugins|project|setup-token|ultrareview|update|upgrade)
      inject_name=0 ;;
  esac
  if [ "$inject_name" = 1 ]; then
    version=$("$real" --version 2>/dev/null) || version=""
    version=${version%% *}
    IFS=. read -r major minor patch <<< "$version"
    if [[ ${major:-} =~ ^[0-9]+$ && ${minor:-} =~ ^[0-9]+$ && ${patch:-} =~ ^[0-9]+$ ]] &&
       (( major > 2 || (major == 2 && (minor > 1 || (minor == 1 && patch >= 224))) )); then
      if [ -n "${FLOWMUX_BUNDLED_CLI_PATH:-}" ] && [ -x "$FLOWMUX_BUNDLED_CLI_PATH" ]; then
        session_name=$("$FLOWMUX_BUNDLED_CLI_PATH" session-name 2>/dev/null) || session_name=""
      elif command -v flowmuxctl >/dev/null 2>&1; then
        session_name=$(flowmuxctl session-name 2>/dev/null) || session_name=""
      elif command -v flowmux >/dev/null 2>&1; then
        session_name=$(flowmux session-name 2>/dev/null) || session_name=""
      fi
      if [ -n "${session_name:-}" ]; then
        export FLOWMUX_CLAUDE_SESSION_NAME="$session_name"
        set -- --name "$session_name" "$@"
      fi
    fi
  fi
fi
"#
    } else {
        ""
    };
    format!(
        r#"#!/usr/bin/env bash
# flowmux agent wrapper shim (managed by `flowmux fix`).
# Records the real {agent} PID and transparently exec's the real binary.
# Native hooks report lifecycle state; process scanning is the fallback.
if [ -n "${{FLOWMUX_SURFACE_ID:-}}" ]; then
  export FLOWMUX_AGENT_PID=$$
  export FLOWMUX_AGENT_NAME={canonical_agent}
fi
self_dir=$(cd "$(dirname "$0")" && pwd)
is_flowmux_shim() {{
  grep -q "flowmux agent wrapper shim" "$1" 2>/dev/null
}}
real=""
saved_ifs=$IFS
IFS=:
for d in $PATH; do
  [ "$d" = "$self_dir" ] && continue
  candidate="$d/{agent}"
  [ -x "$candidate" ] || continue
  is_flowmux_shim "$candidate" && continue
  real="$candidate"
  break
done
IFS=$saved_ifs
if [ -z "$real" ]; then
  echo "flowmux shim: {agent} not found on PATH" >&2
  exit 127
fi
{claude_session_name}
exec "$real" "$@"
"#
    )
}

/// Write/refresh the agent wrapper shims into the shim dir and mark them
/// executable. Idempotent; returns the paths touched. `Ok(vec![])` when
/// no data dir is resolvable (e.g. `$HOME` unset).
pub fn install_agent_shims() -> Result<Vec<PathBuf>> {
    use std::os::unix::fs::PermissionsExt;
    let dir = match flowmux_config::paths::agent_shim_dir() {
        Some(d) => d,
        None => return Ok(vec![]),
    };
    fs::create_dir_all(&dir)?;
    let mut written = remove_legacy_local_agent_shims()?;
    for agent in SHIM_AGENTS {
        let path = dir.join(agent);
        if !agent_has_real_binary(agent) {
            if is_legacy_local_agent_shim(&path) {
                fs::remove_file(&path)?;
                written.push(path);
            }
            continue;
        }
        let body = shim_script(agent);
        let up_to_date = fs::read_to_string(&path)
            .map(|c| c == body)
            .unwrap_or(false);
        if !up_to_date {
            fs::write(&path, &body)?;
            written.push(path.clone());
        }
        // Always assert the exec bit — cheap, and a non-executable shim
        // would silently break command resolution.
        let mut perms = fs::metadata(&path)?.permissions();
        if perms.mode() & 0o111 != 0o111 {
            perms.set_mode(0o755);
            fs::set_permissions(&path, perms)?;
            if !written.contains(&path) {
                written.push(path.clone());
            }
        }
    }
    Ok(written)
}

/// Body of the `tmux` compatibility shim (Claude Code agent teams).
///
/// The GUI prepends the agent shim dir to every PTY's `PATH`, so a
/// Claude Code lead running inside a flowmux pane resolves `tmux` to
/// this script. Swarm-scoped invocations (the `claude-swarm` server
/// socket / session names, or a pane UUID handed out by tmux-compat)
/// are translated into flowmux workspaces and panes via
/// `flowmuxctl tmux-compat`; every other invocation falls through to
/// the real tmux, so humans using tmux inside flowmux are unaffected.
/// `FLOWMUX_TMUX_SHIM=0` disables interception entirely. Unlike the
/// agent wrapper shims this is deliberately NOT mirrored into
/// `~/.local/bin` — hijacking `tmux` outside flowmux panes would be
/// wrong.
pub fn tmux_shim_script() -> String {
    r#"#!/usr/bin/env bash
# flowmux tmux compat shim (managed by `flowmux fix`).
# Routes Claude Code agent-teams swarm calls into flowmux panes and
# passes everything else through to the real tmux.
self_dir=$(cd "$(dirname "$0")" && pwd)
find_real_tmux() {
  local saved_ifs=$IFS d candidate
  IFS=:
  for d in $PATH; do
    [ "$d" = "$self_dir" ] && continue
    candidate="$d/tmux"
    [ -x "$candidate" ] || continue
    grep -q "flowmux tmux compat shim" "$candidate" 2>/dev/null && continue
    printf '%s' "$candidate"
    IFS=$saved_ifs
    return 0
  done
  IFS=$saved_ifs
  return 1
}

swarm=0
if [ -n "${FLOWMUX_SOCKET_PATH:-}" ] && [ "${FLOWMUX_TMUX_SHIM:-1}" != "0" ]; then
  prev=""
  for a in "$@"; do
    case "$prev" in
      -L|-t|-s)
        case "$a" in
          claude-swarm|claude-swarm:*|claude-swarm-*) swarm=1 ;;
          # Pane UUIDs handed out by tmux-compat (legacy default-socket path).
          ????????-????-????-????-????????????) swarm=1 ;;
        esac
        ;;
    esac
    prev="$a"
  done
fi

if [ "$swarm" = "1" ]; then
  if command -v flowmuxctl >/dev/null 2>&1; then
    exec flowmuxctl tmux-compat "$@"
  elif command -v flowmux >/dev/null 2>&1; then
    exec flowmux tmux-compat "$@"
  fi
  echo "flowmux tmux shim: flowmuxctl not found on PATH" >&2
  exit 127
fi

if real=$(find_real_tmux); then
  exec "$real" "$@"
fi

# No real tmux installed. Inside a flowmux pane, still answer the
# availability probe so Claude Code's agent-teams detection succeeds.
if [ -n "${FLOWMUX_SOCKET_PATH:-}" ] && [ "$1" = "-V" ]; then
  if command -v flowmuxctl >/dev/null 2>&1; then
    exec flowmuxctl tmux-compat -V
  elif command -v flowmux >/dev/null 2>&1; then
    exec flowmux tmux-compat -V
  fi
fi
echo "flowmux tmux shim: tmux not found on PATH" >&2
exit 127
"#
    .to_string()
}

/// Write/refresh the `tmux` compat shim into the shim dir. Idempotent;
/// returns the paths touched. `Ok(vec![])` when no data dir resolves.
pub fn install_tmux_shim() -> Result<Vec<PathBuf>> {
    let dir = match flowmux_config::paths::agent_shim_dir() {
        Some(d) => d,
        None => return Ok(vec![]),
    };
    install_tmux_shim_into(&dir)
}

fn install_tmux_shim_into(dir: &Path) -> Result<Vec<PathBuf>> {
    use std::os::unix::fs::PermissionsExt;
    fs::create_dir_all(dir)?;
    let path = dir.join("tmux");
    let body = tmux_shim_script();
    let mut written = Vec::new();
    let up_to_date = fs::read_to_string(&path)
        .map(|c| c == body)
        .unwrap_or(false);
    if !up_to_date {
        fs::write(&path, &body)?;
        written.push(path.clone());
    }
    let mut perms = fs::metadata(&path)?.permissions();
    if perms.mode() & 0o111 != 0o111 {
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
        if !written.contains(&path) {
            written.push(path);
        }
    }
    Ok(written)
}

fn user_local_bin_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("bin"))
}

pub fn legacy_local_agent_shims() -> Vec<PathBuf> {
    user_local_bin_dir()
        .into_iter()
        .flat_map(|dir| SHIM_AGENTS.iter().map(move |agent| dir.join(agent)))
        .filter(|path| is_legacy_local_agent_shim(path))
        .collect()
}

pub fn agent_has_real_binary(agent: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|path| agent_has_real_binary_on_path(agent, path.as_os_str()))
}

fn agent_has_real_binary_on_path(agent: &str, path: &std::ffi::OsStr) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::env::split_paths(path).any(|dir| {
        let candidate = dir.join(agent);
        std::fs::metadata(&candidate)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            && !fs::read_to_string(candidate)
                .is_ok_and(|source| source.contains("flowmux agent wrapper shim"))
    })
}

fn is_legacy_local_agent_shim(path: &Path) -> bool {
    fs::read_to_string(path).is_ok_and(|source| source.contains("flowmux agent wrapper shim"))
}

fn remove_legacy_local_agent_shims() -> Result<Vec<PathBuf>> {
    let paths = legacy_local_agent_shims();
    for path in &paths {
        fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(paths)
}

pub fn uninstall_agent_shim(agent: &str) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let paths = [
        flowmux_config::paths::agent_shim_dir().map(|dir| dir.join(agent)),
        user_local_bin_dir().map(|dir| dir.join(agent)),
    ];
    for path in paths.into_iter().flatten() {
        if remove_owned_shim(&path, "flowmux agent wrapper shim")? {
            removed.push(path);
        }
    }
    Ok(removed)
}

pub fn uninstall_tmux_shim() -> Result<Option<PathBuf>> {
    let Some(path) = flowmux_config::paths::agent_shim_dir().map(|dir| dir.join("tmux")) else {
        return Ok(None);
    };
    remove_owned_shim(&path, "flowmux tmux compat shim").map(|removed| removed.then_some(path))
}

fn remove_owned_shim(path: &Path, marker: &str) -> Result<bool> {
    if !fs::read_to_string(path).is_ok_and(|source| source.contains(marker)) {
        return Ok(false);
    }
    fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
    Ok(true)
}

// ---- Claude Code ----------------------------------------------------

fn claude_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

fn install_claude(flowmux_bin: &str) -> Result<HookInstallReport> {
    let path = match claude_settings_path() {
        Some(p) => p,
        None => return Ok(skipped(HookTarget::Claude)),
    };
    if !path.parent().map(|p| p.exists()).unwrap_or(false) {
        return Ok(skipped(HookTarget::Claude));
    }

    install_claude_in(&path, flowmux_bin)
}

fn install_claude_in(path: &Path, flowmux_bin: &str) -> Result<HookInstallReport> {
    let mut root: Value = read_json_or_empty_object(path)?;
    let hooks = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} is not a JSON object", path.display()))?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("hooks field is not a JSON object in {}", path.display()))?;

    for event in CLAUDE_EVENTS {
        let arr = hooks
            .entry(event.name.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                anyhow!(
                    "Claude hook {} is not an array in {}",
                    event.name,
                    path.display()
                )
            })?;
        upsert_flowmux_hook_entry(arr, claude_hook_entry(flowmux_bin, *event));
    }

    let changed = write_json(path, &root)?;
    Ok(HookInstallReport {
        target: HookTarget::Claude,
        status: HookInstallStatus::Installed,
        touched_paths: changed.then(|| path.to_path_buf()).into_iter().collect(),
    })
}

fn uninstall_claude() -> Result<HookInstallReport> {
    let path = match claude_settings_path() {
        Some(p) if p.exists() => p,
        _ => return Ok(skipped(HookTarget::Claude)),
    };
    let mut root: Value = read_json_or_empty_object(&path)?;
    if let Some(hooks) = root
        .as_object_mut()
        .and_then(|o| o.get_mut("hooks"))
        .and_then(|h| h.as_object_mut())
    {
        for event in CLAUDE_EVENTS {
            if let Some(arr) = hooks.get_mut(event.name).and_then(|v| v.as_array_mut()) {
                prune_flowmux_claude_entries(arr);
            }
        }
    }
    let changed = write_json(&path, &root)?;
    Ok(HookInstallReport {
        target: HookTarget::Claude,
        status: HookInstallStatus::Installed,
        touched_paths: changed.then_some(path).into_iter().collect(),
    })
}

#[derive(Debug, Clone, Copy)]
struct ClaudeEvent {
    /// Event name as Claude Code spells it ("Stop", "Notification", …).
    name: &'static str,
    /// Subcommand fed to `flowmux hooks claude`.
    subcommand: &'static str,
    timeout_secs: u32,
}

const CLAUDE_EVENTS: &[ClaudeEvent] = &[
    ClaudeEvent {
        name: "Stop",
        subcommand: "stop",
        timeout_secs: 10,
    },
    ClaudeEvent {
        name: "StopFailure",
        subcommand: "stop-failure",
        timeout_secs: 10,
    },
    ClaudeEvent {
        name: "Notification",
        subcommand: "notification",
        timeout_secs: 10,
    },
    ClaudeEvent {
        name: "PermissionRequest",
        subcommand: "permission-request",
        timeout_secs: 10,
    },
    // Live agent-activity tracking. SessionStart registers the agent's
    // presence/PID; UserPromptSubmit + PreToolUse mark it Running;
    // SessionEnd clears it. Its handler may send both clear and binding-forget
    // requests, so leave enough room for both bounded IPC operations.
    ClaudeEvent {
        name: "SessionStart",
        subcommand: "session-start",
        timeout_secs: 5,
    },
    ClaudeEvent {
        name: "UserPromptSubmit",
        subcommand: "prompt-submit",
        timeout_secs: 5,
    },
    ClaudeEvent {
        name: "PreToolUse",
        subcommand: "pre-tool-use",
        timeout_secs: 5,
    },
    ClaudeEvent {
        name: "PostToolUse",
        subcommand: "post-tool-use",
        timeout_secs: 5,
    },
    ClaudeEvent {
        name: "PostToolBatch",
        subcommand: "post-tool-batch",
        timeout_secs: 5,
    },
    ClaudeEvent {
        name: "PostToolUseFailure",
        subcommand: "post-tool-use-failure",
        timeout_secs: 5,
    },
    ClaudeEvent {
        name: "PermissionDenied",
        subcommand: "permission-denied",
        timeout_secs: 5,
    },
    ClaudeEvent {
        name: "SessionEnd",
        subcommand: "session-end",
        timeout_secs: 3,
    },
];

fn claude_hook_entry(flowmux_bin: &str, event: ClaudeEvent) -> Value {
    let prefix = host_invocation_shell_command(flowmux_bin);
    let cmd = format!(
        // Marker `flowmux-hook` lets us identify our own entry on
        // re-install. Whitespace before/after is intentional.
        "{prefix} hooks claude {subcommand} ${{FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}} ${{FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}}  # {marker}",
        subcommand = event.subcommand,
        marker = FLOWMUX_HOOK_MARKER
    );
    json!({
        "matcher": match event.name {
            "Notification" => "permission_prompt|elicitation_dialog|elicitation_url_dialog|agent_needs_input|agent_completed|quota_auto_resume_stale|quota_auto_resume_fired|quota_auto_resume_disabled",
            // SessionStart also fires for mid-turn compaction. A compaction is
            // not a new lifecycle epoch and must not reset waits or children.
            "SessionStart" => "^(startup|resume|clear|fork)$",
            _ => "",
        },
        "hooks": [{
            "type": "command",
            "command": cmd,
            "timeout": event.timeout_secs,
        }]
    })
}

fn hook_is_flowmux_owned(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(FLOWMUX_HOOK_MARKER))
}

/// Remove only FlowMux-owned nested handlers. Matcher groups can be shared by
/// multiple integrations, so deleting the whole group would also delete user
/// hooks that happen to sit beside ours.
fn prune_flowmux_claude_entries(arr: &mut Vec<Value>) -> bool {
    prune_flowmux_hook_handlers(arr)
}

fn prune_flowmux_hook_handlers(entries: &mut Vec<Value>) -> bool {
    let mut removed_any = false;
    entries.retain_mut(|entry| {
        let Some(hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        let before = hooks.len();
        hooks.retain(|hook| !hook_is_flowmux_owned(hook));
        let removed = hooks.len() != before;
        removed_any |= removed;
        // Preserve unrelated groups that were already empty. Only discard a
        // group when removing our handler made it empty.
        !(removed && hooks.is_empty())
    });
    removed_any
}

/// Refresh the first canonical FlowMux handler in place and remove duplicates.
/// Codex persists hook trust by array position, so retaining the surrounding
/// group and handler index avoids invalidating unrelated user approvals.
fn upsert_flowmux_hook_entry(entries: &mut Vec<Value>, replacement: Value) {
    let replacement_matcher = replacement.get("matcher").cloned();
    let mut replacement_hook = replacement
        .get("hooks")
        .and_then(Value::as_array)
        .and_then(|hooks| hooks.first())
        .cloned()
        .expect("FlowMux hook entries always contain one handler");
    let mut replacement_with_extensions = replacement;
    for entry in entries.iter() {
        let Some(hooks) = entry.get("hooks").and_then(Value::as_array) else {
            continue;
        };
        for hook in hooks.iter().filter(|hook| hook_is_flowmux_owned(hook)) {
            preserve_unknown_hook_handler_fields(&mut replacement_hook, hook);
        }
        if !hooks.is_empty() && hooks.iter().all(hook_is_flowmux_owned) {
            preserve_unknown_object_fields(
                &mut replacement_with_extensions,
                entry,
                &["matcher", "hooks"],
            );
        }
    }
    replacement_with_extensions["hooks"][0] = replacement_hook.clone();
    let mut replaced = false;

    entries.retain_mut(|entry| {
        let matcher_matches = entry.get("matcher").cloned() == replacement_matcher;
        let owned_count = entry
            .get("hooks")
            .and_then(Value::as_array)
            .map(|hooks| {
                hooks
                    .iter()
                    .filter(|hook| hook_is_flowmux_owned(hook))
                    .count()
            })
            .unwrap_or(0);
        if owned_count == 0 {
            return true;
        }
        let original_entry = entry.clone();
        let Some(hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };

        // A group containing only our handlers is safe to migrate in place,
        // even when an older release omitted or used a different matcher.
        // Keeping the group slot avoids shifting unrelated Codex trust indexes.
        if !replaced && !matcher_matches && owned_count == hooks.len() {
            let mut refreshed = replacement_with_extensions.clone();
            preserve_unknown_object_fields(&mut refreshed, &original_entry, &["matcher", "hooks"]);
            *entry = refreshed;
            replaced = true;
            return true;
        }

        let before = hooks.len();
        let mut refreshed_here = false;
        hooks.retain_mut(|hook| {
            if !hook_is_flowmux_owned(hook) {
                return true;
            }
            if !replaced && matcher_matches {
                *hook = replacement_hook.clone();
                replaced = true;
                refreshed_here = true;
                true
            } else {
                false
            }
        });
        let keep = !(hooks.is_empty() && hooks.len() != before);
        if refreshed_here {
            preserve_unknown_object_fields(
                entry,
                &replacement_with_extensions,
                &["matcher", "hooks"],
            );
        }
        keep
    });

    if !replaced {
        entries.push(replacement_with_extensions);
    }
}

fn preserve_unknown_hook_handler_fields(target: &mut Value, source: &Value) {
    preserve_unknown_object_fields(
        target,
        source,
        &[
            "type",
            "if",
            "command",
            "commandWindows",
            "command_windows",
            "args",
            "timeout",
            "async",
            "asyncRewake",
            "shell",
            "statusMessage",
            "once",
            "additionalContextLimit",
        ],
    );
}

fn preserve_unknown_object_fields(target: &mut Value, source: &Value, known: &[&str]) {
    let (Some(target), Some(source)) = (target.as_object_mut(), source.as_object()) else {
        return;
    };
    for (field, value) in source {
        if !known.contains(&field.as_str()) && !target.contains_key(field) {
            target.insert(field.clone(), value.clone());
        }
    }
}

#[cfg(test)]
fn claude_entry_is_flowmux_owned(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|v| v.as_array())
        .map(|inner| inner.iter().any(hook_is_flowmux_owned))
        .unwrap_or(false)
}

fn claude_entry_matches(entry: &Value, event: ClaudeEvent) -> bool {
    let expected = claude_hook_entry("flowmux", event);
    entry.get("matcher") == expected.get("matcher")
        && entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| {
                                command.contains(FLOWMUX_HOOK_MARKER)
                                    && command
                                        .contains(&format!("hooks claude {}", event.subcommand))
                                    && command
                                        .contains("${FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}")
                                    && command.contains(
                                        "${FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}",
                                    )
                            })
                        && hook.get("timeout").and_then(Value::as_u64)
                            == Some(event.timeout_secs.into())
                })
            })
}

// ---- Codex CLI ------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct CodexEvent {
    name: &'static str,
    subcommand: &'static str,
    matcher: &'static str,
    timeout_secs: u32,
}

const CODEX_EVENTS: &[CodexEvent] = &[
    // Codex also emits SessionStart during mid-turn compaction. Excluding that
    // source keeps a presence event from masquerading as a turn transition.
    CodexEvent {
        name: "SessionStart",
        subcommand: "session-start",
        matcher: "^(startup|resume|clear)$",
        timeout_secs: 5,
    },
    CodexEvent {
        name: "UserPromptSubmit",
        subcommand: "turn-start",
        matcher: "",
        timeout_secs: 5,
    },
    CodexEvent {
        name: "PreToolUse",
        subcommand: "running",
        matcher: "",
        timeout_secs: 5,
    },
    CodexEvent {
        name: "PermissionRequest",
        subcommand: "notification",
        matcher: "",
        timeout_secs: 5,
    },
    // PermissionRequest has no call identity or separate resolution hook.
    // PostToolUse confirms progress but cannot identify which parallel request
    // resolved, so the daemon keeps a conservative turn-scoped wait marker.
    CodexEvent {
        name: "PostToolUse",
        subcommand: "running",
        matcher: "",
        timeout_secs: 5,
    },
    CodexEvent {
        name: "SubagentStart",
        subcommand: "subagent-start",
        matcher: "",
        timeout_secs: 5,
    },
    CodexEvent {
        name: "SubagentStop",
        subcommand: "subagent-stop",
        matcher: "",
        timeout_secs: 5,
    },
    CodexEvent {
        name: "Stop",
        subcommand: "stop",
        matcher: "",
        timeout_secs: 5,
    },
    CodexEvent {
        name: "Interrupt",
        subcommand: "interrupt",
        matcher: "",
        timeout_secs: 5,
    },
    CodexEvent {
        name: "SessionEnd",
        subcommand: "session-end",
        matcher: "",
        timeout_secs: 3,
    },
];

fn codex_home() -> Option<PathBuf> {
    if let Some(env) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(env));
    }
    dirs::home_dir().map(|h| h.join(".codex"))
}

fn install_codex(flowmux_bin: &str) -> Result<HookInstallReport> {
    let home = match codex_home() {
        Some(h) if h.exists() => h,
        _ => return Ok(skipped(HookTarget::Codex)),
    };
    install_codex_in(&home, flowmux_bin)
}

fn install_codex_in(home: &Path, flowmux_bin: &str) -> Result<HookInstallReport> {
    let hooks_path = home.join("hooks.json");
    let config_path = home.join("config.toml");
    // Parse every file before the first write. A malformed legacy config must
    // not leave hooks.json half-migrated when setup reports an error.
    let legacy_notify = codex_config_has_owned_notify(&config_path)?;
    if codex_config_hooks_disabled(&config_path)? {
        return Err(anyhow!(
            "Codex user hooks are explicitly disabled in {}",
            config_path.display()
        ));
    }
    let mut root = read_json_or_empty_object(&hooks_path)?;
    validate_codex_hooks_shape(&root)?;
    upsert_codex_hooks(&mut root, flowmux_bin)?;
    let mut touched_paths = Vec::new();
    if write_json(&hooks_path, &root)? {
        touched_paths.push(hooks_path);
    }
    if legacy_notify && remove_owned_codex_notify(&config_path)? {
        touched_paths.push(config_path);
    }

    Ok(HookInstallReport {
        target: HookTarget::Codex,
        status: HookInstallStatus::Installed,
        touched_paths,
    })
}

fn uninstall_codex() -> Result<HookInstallReport> {
    let home = match codex_home() {
        Some(h) if h.exists() => h,
        _ => return Ok(skipped(HookTarget::Codex)),
    };
    uninstall_codex_in(&home)
}

fn uninstall_codex_in(home: &Path) -> Result<HookInstallReport> {
    let mut touched_paths = Vec::new();
    let hooks_path = home.join("hooks.json");
    if hooks_path.exists() {
        let mut root: Value = read_json_or_empty_object(&hooks_path)?;
        let before = root.clone();
        prune_codex_hooks(&mut root);
        if root != before {
            if root.as_object().is_some_and(|object| object.is_empty()) {
                let is_symlink = fs::symlink_metadata(&hooks_path)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink());
                if is_symlink {
                    write_json(&hooks_path, &root)?;
                } else {
                    fs::remove_file(&hooks_path)
                        .with_context(|| format!("remove {}", hooks_path.display()))?;
                }
            } else {
                write_json(&hooks_path, &root)?;
            }
            touched_paths.push(hooks_path);
        }
    }
    let config_path = home.join("config.toml");
    if remove_owned_codex_notify(&config_path)? {
        touched_paths.push(config_path);
    }
    Ok(HookInstallReport {
        target: HookTarget::Codex,
        status: HookInstallStatus::Installed,
        touched_paths,
    })
}

fn codex_hook_entry(flowmux_bin: &str, event: CodexEvent) -> Value {
    let command = format!(
        "{} hooks codex {} ${{FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}} ${{FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}}  # {}",
        host_invocation_shell_command(flowmux_bin),
        event.subcommand,
        FLOWMUX_HOOK_MARKER,
    );
    json!({
        "matcher": event.matcher,
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": event.timeout_secs,
        }]
    })
}

fn codex_entry_matches(entry: &Value, event: CodexEvent) -> bool {
    entry.get("matcher").and_then(Value::as_str) == Some(event.matcher)
        && entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| {
                                command.contains(FLOWMUX_HOOK_MARKER)
                                    && command
                                        .contains(&format!("hooks codex {}", event.subcommand))
                                    && codex_direct_executable_available(command, event)
                                    && command
                                        .contains("${FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}")
                                    && command.contains(
                                        "${FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}",
                                    )
                            })
                        && hook.get("timeout").and_then(Value::as_u64)
                            == Some(event.timeout_secs.into())
                        && hook.get("async").and_then(Value::as_bool) != Some(true)
                })
            })
}

/// A direct absolute binary can be checked locally. Relative commands depend
/// on the agent's PATH, while Flatpak commands contain multiple argv words and
/// resolve inside the sandbox; keep those compatible and validate their shape
/// only. FlowMux quotes paths as one POSIX single-quoted word, including the
/// standard `'\''` spelling for an embedded quote.
fn codex_direct_executable_available(command: &str, event: CodexEvent) -> bool {
    let separator = format!(" hooks codex {}", event.subcommand);
    let Some((prefix, _)) = command.rsplit_once(&separator) else {
        return true;
    };
    let Some(argv) = parse_generated_shell_words(prefix) else {
        return false;
    };
    match argv.as_slice() {
        [executable] => {
            let executable = Path::new(executable);
            if executable.is_absolute() {
                executable_path_is_runnable(executable)
            } else {
                // A single component is resolved through PATH by the hook
                // shell. Relative paths containing a separator depend on
                // whichever project directory Codex is running in.
                executable.components().count() == 1
            }
        }
        [flatpak, run, command, app_id]
            if flatpak == "flatpak"
                && run == "run"
                && command == "--command=flowmuxctl"
                && !app_id.is_empty() =>
        {
            true
        }
        _ => false,
    }
}

/// Parse exactly the conservative word form emitted by [`shell_quote`]. This
/// is deliberately not a general shell parser: operators, backslash escapes,
/// unbalanced quotes, and arbitrary multi-command prefixes are rejected.
fn parse_generated_shell_words(mut input: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    input = input.trim();
    while !input.is_empty() {
        if let Some(mut quoted) = input.strip_prefix('\'') {
            let mut word = String::new();
            loop {
                let close = quoted.find('\'')?;
                word.push_str(&quoted[..close]);
                quoted = &quoted[close + 1..];
                if let Some(reopened) = quoted.strip_prefix(r"\''") {
                    word.push('\'');
                    quoted = reopened;
                    continue;
                }
                if !quoted.is_empty() && !quoted.chars().next().is_some_and(char::is_whitespace) {
                    return None;
                }
                words.push(word);
                input = quoted.trim_start();
                break;
            }
        } else {
            let end = input.find(char::is_whitespace).unwrap_or(input.len());
            let word = &input[..end];
            if word.is_empty()
                || !word.chars().all(|character| {
                    character.is_ascii_alphanumeric()
                        || matches!(character, '/' | '.' | '_' | '-' | '=' | ':')
                })
            {
                return None;
            }
            words.push(word.to_string());
            input = input[end..].trim_start();
        }
    }
    (!words.is_empty()).then_some(words)
}

fn executable_path_is_runnable(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn codex_matching_entry_count(root: &Value, event: CodexEvent) -> usize {
    root.get("hooks")
        .and_then(Value::as_object)
        .and_then(|hooks| hooks.get(event.name))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| codex_entry_matches(entry, event))
                .count()
        })
        .unwrap_or(0)
}

fn codex_owned_hook_count(root: &Value) -> usize {
    fn owned_in_entries(entries: &[Value]) -> usize {
        entries
            .iter()
            .filter_map(|entry| entry.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(FLOWMUX_HOOK_MARKER))
            })
            .count()
    }

    let Some(root) = root.as_object() else {
        return 0;
    };
    root.iter()
        .map(|(name, value)| {
            if name == "hooks" {
                value
                    .as_object()
                    .map(|hooks| {
                        hooks
                            .values()
                            .filter_map(Value::as_array)
                            .map(|entries| owned_in_entries(entries))
                            .sum()
                    })
                    .unwrap_or(0)
            } else {
                value
                    .as_array()
                    .map(|entries| owned_in_entries(entries))
                    .unwrap_or(0)
            }
        })
        .sum()
}

fn upsert_codex_hooks(root: &mut Value, flowmux_bin: &str) -> Result<()> {
    validate_codex_hooks_shape(root)?;
    prune_obsolete_codex_hooks(root);
    let hooks = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("Codex hooks root is not a JSON object"))?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("Codex hooks field is not a JSON object"))?;
    for event in CODEX_EVENTS {
        let entries = hooks
            .entry(event.name)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow!("Codex hook {} is not an array", event.name))?;
        upsert_flowmux_hook_entry(entries, codex_hook_entry(flowmux_bin, *event));
    }
    Ok(())
}

fn validate_codex_hooks_shape(root: &Value) -> Result<()> {
    let root = root
        .as_object()
        .ok_or_else(|| anyhow!("Codex hooks root is not a JSON object"))?;

    for (field, value) in root {
        match field.as_str() {
            "description" => {
                if !value.is_null() && !value.is_string() {
                    return Err(anyhow!("Codex hooks description is not a string"));
                }
            }
            "hooks" => {
                let hooks = value
                    .as_object()
                    .ok_or_else(|| anyhow!("Codex hooks field is not a JSON object"))?;
                for (event, entries) in hooks {
                    validate_codex_matcher_groups(event, entries)?;
                }
            }
            legacy_event if CODEX_EVENTS.iter().any(|event| event.name == legacy_event) => {
                validate_codex_matcher_groups(legacy_event, value)?;
                if !legacy_codex_entries_are_owned(value) {
                    return Err(anyhow!(
                        "Codex hooks root contains unsupported field {legacy_event}"
                    ));
                }
            }
            _ => {
                return Err(anyhow!(
                    "Codex hooks root contains unsupported field {field}"
                ))
            }
        }
    }
    Ok(())
}

fn validate_codex_matcher_groups(event: &str, entries: &Value) -> Result<()> {
    let entries = entries
        .as_array()
        .ok_or_else(|| anyhow!("Codex hook {event} is not an array"))?;
    for (group_index, entry) in entries.iter().enumerate() {
        let entry = entry.as_object().ok_or_else(|| {
            anyhow!("Codex hook {event} matcher group {group_index} is not an object")
        })?;
        if entry
            .get("matcher")
            .is_some_and(|matcher| !matcher.is_null() && !matcher.is_string())
        {
            return Err(anyhow!(
                "Codex hook {event} matcher group {group_index} has a non-string matcher"
            ));
        }
        let Some(handlers) = entry.get("hooks") else {
            continue;
        };
        let handlers = handlers.as_array().ok_or_else(|| {
            anyhow!("Codex hook {event} matcher group {group_index} hooks is not an array")
        })?;
        for (handler_index, handler) in handlers.iter().enumerate() {
            validate_codex_handler(event, group_index, handler_index, handler)?;
        }
    }
    Ok(())
}

fn validate_codex_handler(
    event: &str,
    group_index: usize,
    handler_index: usize,
    handler: &Value,
) -> Result<()> {
    let context = || format!("Codex hook {event} handler {group_index}:{handler_index}");
    let handler = handler
        .as_object()
        .ok_or_else(|| anyhow!("{} is not an object", context()))?;
    let handler_type = handler
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{} has no string type", context()))?;

    if handler
        .get("timeout")
        .is_some_and(|value| !value.is_null() && value.as_u64().is_none())
    {
        return Err(anyhow!("{} has an invalid timeout", context()));
    }
    if handler
        .get("statusMessage")
        .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err(anyhow!("{} has an invalid statusMessage", context()));
    }

    match handler_type {
        "command" => {
            if !handler.get("command").is_some_and(Value::is_string) {
                return Err(anyhow!("{} has no string command", context()));
            }
            if handler.contains_key("commandWindows") && handler.contains_key("command_windows") {
                return Err(anyhow!(
                    "{} has duplicate commandWindows aliases",
                    context()
                ));
            }
            for field in ["commandWindows", "command_windows"] {
                if handler
                    .get(field)
                    .is_some_and(|value| !value.is_null() && !value.is_string())
                {
                    return Err(anyhow!("{} has an invalid {field}", context()));
                }
            }
            if handler
                .get("async")
                .is_some_and(|value| !value.is_boolean())
            {
                return Err(anyhow!("{} has an invalid async flag", context()));
            }
            if handler.get("additionalContextLimit").is_some_and(|value| {
                !value.is_null()
                    && value
                        .as_u64()
                        .and_then(|limit| usize::try_from(limit).ok())
                        .is_none()
            }) {
                return Err(anyhow!(
                    "{} has an invalid additionalContextLimit",
                    context()
                ));
            }
        }
        "mcp_tool" => {
            for field in ["server", "tool"] {
                if !handler.get(field).is_some_and(Value::is_string) {
                    return Err(anyhow!("{} has no string {field}", context()));
                }
            }
            if let Some(input) = handler.get("input") {
                let input = input
                    .as_object()
                    .ok_or_else(|| anyhow!("{} has a non-object input", context()))?;
                if input
                    .values()
                    .any(|value| !codex_mcp_input_value_is_valid(value))
                {
                    return Err(anyhow!(
                        "{} has input that cannot be represented as TOML",
                        context()
                    ));
                }
            }
        }
        "prompt" | "agent" => {}
        _ => return Err(anyhow!("{} has unknown type {handler_type}", context())),
    }
    Ok(())
}

fn codex_mcp_input_value_is_valid(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::String(_) => true,
        Value::Number(number) => !number.is_u64() || number.as_u64() <= Some(i64::MAX as u64),
        Value::Array(values) => values.iter().all(codex_mcp_input_value_is_valid),
        Value::Object(values) => values.values().all(codex_mcp_input_value_is_valid),
    }
}

fn legacy_codex_entries_are_owned(entries: &Value) -> bool {
    entries.as_array().is_some_and(|entries| {
        !entries.is_empty()
            && entries.iter().all(|entry| {
                entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .is_some_and(|hooks| {
                        !hooks.is_empty()
                            && hooks.iter().all(|hook| {
                                hook.get("type").and_then(Value::as_str) == Some("command")
                                    && hook_is_flowmux_owned(hook)
                            })
                    })
            })
    })
}

/// Remove marked handlers for events no longer installed, plus the obsolete
/// root-level schema, while leaving current event entries in place for their
/// trust-preserving upsert.
fn prune_obsolete_codex_hooks(root: &mut Value) {
    let Some(root) = root.as_object_mut() else {
        return;
    };
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        let event_names: Vec<String> = hooks.keys().cloned().collect();
        for event_name in event_names {
            if CODEX_EVENTS.iter().any(|event| event.name == event_name) {
                continue;
            }
            let Some(entries) = hooks.get_mut(&event_name).and_then(Value::as_array_mut) else {
                continue;
            };
            let removed = prune_codex_entries(entries);
            if removed && entries.is_empty() {
                hooks.remove(&event_name);
            }
        }
    }

    let root_names: Vec<String> = root
        .keys()
        .filter(|name| name.as_str() != "hooks")
        .cloned()
        .collect();
    for name in root_names {
        let Some(entries) = root.get_mut(&name).and_then(Value::as_array_mut) else {
            continue;
        };
        let removed = prune_codex_entries(entries);
        if removed && entries.is_empty() {
            root.remove(&name);
        }
    }
}

fn prune_codex_hooks(root: &mut Value) {
    let Some(root) = root.as_object_mut() else {
        return;
    };
    let mut removed_from_hooks = false;
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        let event_names: Vec<String> = hooks.keys().cloned().collect();
        for event_name in event_names {
            let Some(entries) = hooks.get_mut(&event_name).and_then(Value::as_array_mut) else {
                continue;
            };
            let removed = prune_codex_entries(entries);
            removed_from_hooks |= removed;
            if removed && entries.is_empty() {
                hooks.remove(&event_name);
            }
        }
        if removed_from_hooks && hooks.is_empty() {
            root.remove("hooks");
        }
    }

    // Codex briefly accepted event arrays at the JSON root. Remove only our
    // marked handlers from that obsolete shape during upgrade/uninstall.
    let root_names: Vec<String> = root
        .keys()
        .filter(|name| name.as_str() != "hooks")
        .cloned()
        .collect();
    for name in root_names {
        let Some(entries) = root.get_mut(&name).and_then(Value::as_array_mut) else {
            continue;
        };
        let removed = prune_codex_entries(entries);
        if removed && entries.is_empty() {
            root.remove(&name);
        }
    }
}

fn prune_codex_entries(entries: &mut Vec<Value>) -> bool {
    prune_flowmux_hook_handlers(entries)
}

fn codex_config_has_owned_notify(config_path: &Path) -> Result<bool> {
    use toml_edit::DocumentMut;

    let original = match fs::read_to_string(config_path) {
        Ok(original) => original,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(anyhow::Error::from(error))
                .context(format!("read {}", config_path.display()))
        }
    };
    let doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;
    Ok(doc
        .get("notify")
        .is_some_and(codex_notify_contains_flowmux_owned))
}

fn codex_config_hooks_disabled(config_path: &Path) -> Result<bool> {
    use toml_edit::DocumentMut;

    let original = match fs::read_to_string(config_path) {
        Ok(original) => original,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(anyhow::Error::from(error))
                .context(format!("read {}", config_path.display()))
        }
    };
    let doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;
    if doc
        .get("allow_managed_hooks_only")
        .and_then(toml_edit::Item::as_bool)
        == Some(true)
    {
        return Ok(true);
    }
    let features = doc.get("features").and_then(toml_edit::Item::as_table_like);
    Ok(["hooks", "codex_hooks"].into_iter().any(|name| {
        features
            .and_then(|features| features.get(name))
            .and_then(toml_edit::Item::as_bool)
            == Some(false)
    }))
}

/// Remove only FlowMux's old direct `notify` command. Native hooks coexist
/// with unrelated user notification commands and explicit feature settings.
fn remove_owned_codex_notify(config_path: &Path) -> Result<bool> {
    use toml_edit::DocumentMut;

    let original = match fs::read_to_string(config_path) {
        Ok(original) => original,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(anyhow::Error::from(error))
                .context(format!("read {}", config_path.display()))
        }
    };
    let mut doc: DocumentMut = original
        .parse()
        .with_context(|| format!("parse {}", config_path.display()))?;
    let Some(notify) = doc.get_mut("notify") else {
        return Ok(false);
    };
    if codex_notify_is_flowmux_owned(notify) {
        doc.as_table_mut().remove("notify");
    } else if !prune_nested_flowmux_notify(notify) {
        return Ok(false);
    }
    write_atomic(config_path, doc.to_string().as_bytes())
}

fn codex_notify_is_flowmux_owned(item: &toml_edit::Item) -> bool {
    let Some(array) = item.as_array() else {
        return false;
    };
    let args: Vec<&str> = array.iter().filter_map(|value| value.as_str()).collect();
    if args.len() != array.len() {
        return false;
    }
    codex_notify_argv_is_flowmux_owned(&args)
}

fn codex_notify_argv_is_flowmux_owned(args: &[&str]) -> bool {
    let direct = args.len() == 4
        && Path::new(args[0])
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| matches!(name, "flowmux" | "flowmuxctl"))
        && args[1..] == ["hooks", "codex", "stop"];
    let flatpak = args.len() == 7
        && Path::new(args[0])
            .file_name()
            .and_then(|name| name.to_str())
            == Some("flatpak")
        && args[1] == "run"
        && args[2] == "--command=flowmuxctl"
        && args[3] == "com.flowmux.App"
        && args[4..] == ["hooks", "codex", "stop"];
    direct || flatpak
}

fn codex_notify_contains_flowmux_owned(item: &toml_edit::Item) -> bool {
    let Some(array) = item.as_array() else {
        return false;
    };
    let args: Vec<&str> = array.iter().filter_map(|value| value.as_str()).collect();
    args.len() == array.len() && notify_args_contain_flowmux_owned(&args)
}

fn json_notify_contains_flowmux_owned(value: &Value) -> bool {
    let Some(array) = value.as_array() else {
        return false;
    };
    let args: Vec<&str> = array.iter().filter_map(Value::as_str).collect();
    if args.len() != array.len() {
        return false;
    }
    notify_args_contain_flowmux_owned(&args)
}

fn notify_args_contain_flowmux_owned(args: &[&str]) -> bool {
    codex_notify_argv_is_flowmux_owned(args)
        || args.windows(2).any(|pair| {
            pair[0] == "--previous-notify"
                && serde_json::from_str::<Value>(pair[1])
                    .ok()
                    .is_some_and(|nested| json_notify_contains_flowmux_owned(&nested))
        })
}

/// Remove a FlowMux callback nested behind a wrapper's `--previous-notify`
/// argument while preserving the wrapper itself and all of its other flags.
fn prune_nested_flowmux_notify(item: &mut toml_edit::Item) -> bool {
    let Some(array) = item.as_array_mut() else {
        return false;
    };
    let Some(mut args) = array
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let changed = prune_nested_flowmux_notify_args(&mut args);
    if !changed {
        return false;
    }
    while !array.is_empty() {
        array.remove(array.len() - 1);
    }
    for arg in args {
        array.push(arg);
    }
    true
}

fn prune_nested_flowmux_notify_args(args: &mut Vec<String>) -> bool {
    let mut changed = false;
    let mut index = 0;
    while index + 1 < args.len() {
        if args[index] != "--previous-notify" {
            index += 1;
            continue;
        }
        let Ok(mut nested) = serde_json::from_str::<Value>(&args[index + 1]) else {
            index += 2;
            continue;
        };
        let direct_owned = nested.as_array().is_some_and(|array| {
            let nested_args: Vec<&str> = array.iter().filter_map(Value::as_str).collect();
            nested_args.len() == array.len() && codex_notify_argv_is_flowmux_owned(&nested_args)
        });
        if direct_owned {
            args.drain(index..=index + 1);
            changed = true;
            continue;
        }
        if prune_nested_flowmux_notify_json(&mut nested) {
            args[index + 1] = serde_json::to_string(&nested)
                .expect("a parsed Codex notify argv always serializes");
            changed = true;
        }
        index += 2;
    }
    changed
}

fn prune_nested_flowmux_notify_json(value: &mut Value) -> bool {
    let Some(array) = value.as_array_mut() else {
        return false;
    };
    let Some(mut args) = array
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<_>>>()
    else {
        return false;
    };
    let changed = prune_nested_flowmux_notify_args(&mut args);
    if changed {
        *array = args.into_iter().map(Value::String).collect();
    }
    changed
}

// ---- Cline ----------------------------------------------------------

const CLINE_EVENTS: &[&str] = &[
    "TaskStart",
    "TaskResume",
    "UserPromptSubmit",
    "TaskComplete",
];

/// Cline's current global hook directory plus its legacy global directory when
/// that tree already exists. `CLINE_HOOKS_DIR` is the documented runtime
/// override and takes precedence when set.
fn cline_hook_dirs() -> Vec<PathBuf> {
    if let Some(path) = std::env::var_os("CLINE_HOOKS_DIR") {
        if !path.is_empty() {
            return vec![PathBuf::from(path)];
        }
    }
    dirs::home_dir()
        .map(|home| cline_hook_dirs_for_home(&home))
        .unwrap_or_default()
}

fn cline_hook_dirs_for_home(home: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let current_root = home.join(".cline");
    if current_root.exists() {
        dirs.push(current_root.join("hooks"));
    }
    let legacy_root = home.join("Documents").join("Cline");
    if legacy_root.exists() {
        dirs.push(legacy_root.join("Hooks"));
    }
    dirs
}

fn check_cline() -> HookCheckEntry {
    let dirs = cline_hook_dirs();
    if dirs.is_empty() {
        let paths = dirs::home_dir()
            .map(|home| home.join(".cline/hooks/TaskComplete"))
            .into_iter()
            .collect();
        return entry(HookTarget::Cline, HookCheckStatus::NoAgentHome, paths);
    }
    check_cline_in_dirs(&dirs)
}

fn check_cline_in_dirs(dirs: &[PathBuf]) -> HookCheckEntry {
    let mut paths = Vec::with_capacity(dirs.len() * CLINE_EVENTS.len());
    let mut legacy_owned = false;
    for dir in dirs {
        for event in CLINE_EVENTS {
            let path = dir.join(event);
            paths.push(path.clone());
            let body = match fs::read(&path) {
                Ok(body) => body,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return entry(
                        HookTarget::Cline,
                        HookCheckStatus::Error(error.to_string()),
                        paths,
                    )
                }
            };
            legacy_owned |= bytes_contain(&body, FLOWMUX_HOOK_MARKER.as_bytes());
        }
    }
    let status = if legacy_owned {
        HookCheckStatus::Drift
    } else {
        HookCheckStatus::NoAgentHome
    };
    entry(HookTarget::Cline, status, paths)
}

fn uninstall_cline() -> Result<HookInstallReport> {
    let dirs = cline_hook_dirs();
    if dirs.is_empty() {
        return Ok(skipped(HookTarget::Cline));
    }
    uninstall_cline_in_dirs(&dirs)
}

fn uninstall_cline_in_dirs(dirs: &[PathBuf]) -> Result<HookInstallReport> {
    let mut touched_paths = Vec::new();
    for dir in dirs {
        for event in CLINE_EVENTS {
            let path = dir.join(event);
            let body = match fs::read(&path) {
                Ok(body) => body,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(anyhow::Error::from(error))
                        .with_context(|| format!("read {}", path.display()))
                }
            };
            if bytes_contain(&body, FLOWMUX_HOOK_MARKER.as_bytes()) {
                fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
                touched_paths.push(path);
            }
        }
    }
    Ok(HookInstallReport {
        target: HookTarget::Cline,
        status: HookInstallStatus::Installed,
        touched_paths,
    })
}

fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

// ---- OpenCode -------------------------------------------------------

fn opencode_home() -> Option<PathBuf> {
    flowmux_config::paths::host_config_dir_for("opencode")
}

/// Every OpenCode config root flowmux should install the plugin
/// into. The primary `~/.config/opencode/` covers the upstream CLI
/// and any fork that honours the default XDG layout. The
/// `opencode-anycli` wrapper at https://github.com/JSUYA/opencode-anycli
/// re-launches opencode with `XDG_CONFIG_HOME=~/.config/opencode-anycli`
/// so its plugin loader only sees
/// `~/.config/opencode-anycli/opencode/plugins/`; without an entry
/// there the hook never reaches OpenCode and the in-app bell stays
/// silent.
///
/// Only existing roots are returned — we never create the
/// `opencode-anycli` tree on machines that don't have the wrapper
/// installed. The Flatpak build still installs into the anycli root
/// because the wrapper always runs on the host, and its
/// `$HOME/.config/opencode-anycli/` tree is bind-mounted into the
/// sandbox via the manifest's `--filesystem=home`, so the same write
/// path lands at the same on-disk bytes either way.
fn opencode_homes() -> Vec<PathBuf> {
    opencode_homes_for(opencode_home(), host_home_dir())
}

/// Pure-function core of [`opencode_homes`] — `primary` is the upstream
/// `~/.config/opencode/` (or `host_config_dir_for("opencode")` inside
/// Flatpak) and `host_home` is the host `$HOME` used to look up the
/// optional `opencode-anycli` tree. Split out so tests can exercise
/// the anycli-detection branch without touching real env vars.
fn opencode_homes_for(primary: Option<PathBuf>, host_home: Option<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = primary {
        out.push(p);
    }
    if let Some(home) = host_home {
        let anycli = home
            .join(".config")
            .join("opencode-anycli")
            .join("opencode");
        if anycli.exists() && !out.contains(&anycli) {
            out.push(anycli);
        }
    }
    out
}

/// `$HOME` as seen on the host filesystem. Inside the Flatpak sandbox
/// the manifest's `--filesystem=home` keeps `$HOME` pointing at the
/// host's user dir (not the sandbox-private `~/.var/app/...`), so a
/// plain `HOME` lookup is the right primitive for hook paths that
/// must agree with the host-side agent's view of disk.
fn host_home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
}

/// Host-side spawn argv the OpenCode plugin should call. Outside a
/// Flatpak sandbox this is just `[FLOWMUX_BIN]` so spawn behaves
/// like before. Inside the sandbox the plugin is read by host
/// OpenCode, so we wrap the in-sandbox `flowmuxctl` with `flatpak
/// run --command=…` — the host has `flatpak` on PATH and the spawn
/// crosses back into the same sandbox the daemon lives in.
fn opencode_spawn_argv(flowmux_bin: &str) -> Vec<String> {
    host_invocation_argv(flowmux_bin)
}

/// Shell-command string the Claude / Codex hook entries write into
/// the agent's config file. Mirrors [`opencode_spawn_argv`] for the
/// agents that expect a single command string rather than an argv —
/// Claude's `settings.json` `hooks[*].command` and Codex's
/// `config.toml` `notify` are both shell strings.
fn host_invocation_shell_command(flowmux_bin: &str) -> String {
    let argv = host_invocation_argv(flowmux_bin);
    argv.iter()
        .map(|s| shell_quote(s))
        .collect::<Vec<_>>()
        .join(" ")
}

fn host_invocation_argv(flowmux_bin: &str) -> Vec<String> {
    if flowmux_config::paths::is_flatpak_sandbox() {
        let app_id = std::env::var("FLATPAK_ID").unwrap_or_else(|_| "com.flowmux.App".to_string());
        // `--command` accepts a name resolved against /app/bin first
        // and an absolute path otherwise. We pass the bare name so the
        // /app/bin/flowmuxctl symlink (added by the manifest) keeps
        // the entry short and stable across path changes.
        let _ = flowmux_bin; // Sandbox builds resolve via FLATPAK_ID + app PATH.
        vec![
            "flatpak".to_string(),
            "run".to_string(),
            "--command=flowmuxctl".to_string(),
            app_id,
        ]
    } else {
        vec![flowmux_bin.to_string()]
    }
}

/// Conservative POSIX shell quoting for paths and app-ids — wraps
/// in single quotes and escapes embedded single quotes. Used only by
/// the hook installer so it stays close to the call site.
fn shell_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '=' | ':'))
    {
        return s.to_string();
    }
    let escaped = s.replace('\'', r"'\''");
    format!("'{escaped}'")
}

fn install_opencode(flowmux_bin: &str) -> Result<HookInstallReport> {
    let homes: Vec<PathBuf> = opencode_homes()
        .into_iter()
        .filter(|h| h.exists())
        .collect();
    if homes.is_empty() {
        return Ok(skipped(HookTarget::OpenCode));
    }
    let argv = opencode_spawn_argv(flowmux_bin);
    let plugin_src = opencode_plugin_source_with_argv(&argv);
    let mut touched: Vec<PathBuf> = Vec::with_capacity(homes.len() * 2);
    for home in &homes {
        touched.extend(install_opencode_in(home, &plugin_src)?);
    }
    Ok(HookInstallReport {
        target: HookTarget::OpenCode,
        status: HookInstallStatus::Installed,
        touched_paths: touched,
    })
}

fn install_opencode_in(home: &Path, plugin_src: &str) -> Result<Vec<PathBuf>> {
    let plugin_dir = home.join("plugins");
    let plugin_path = plugin_dir.join("flowmux-session.js");
    let legacy_path = plugin_dir.join("flowmux-session.mjs");
    for path in [&plugin_path, &legacy_path] {
        let existing = fs::read_to_string(path).ok();
        if existing
            .as_deref()
            .is_some_and(|source| !opencode_plugin_is_owned(source))
        {
            return Err(anyhow!(
                "{} exists and is not managed by flowmux",
                path.display()
            ));
        }
    }
    fs::create_dir_all(&plugin_dir).with_context(|| format!("create {}", plugin_dir.display()))?;
    let mut touched = Vec::new();
    // OpenCode discovers .js/.ts files, including ESM, but skips .mjs files.
    if write_atomic(&plugin_path, plugin_src.as_bytes())? {
        touched.push(plugin_path);
    }
    if legacy_path.exists() {
        fs::remove_file(&legacy_path)?;
        touched.push(legacy_path);
    }
    let opencode_json = home.join("opencode.json");
    if unregister_opencode_plugin(&opencode_json, "flowmux-session")? {
        touched.push(opencode_json);
    }
    Ok(touched)
}

fn uninstall_opencode() -> Result<HookInstallReport> {
    let homes: Vec<PathBuf> = opencode_homes()
        .into_iter()
        .filter(|h| h.exists())
        .collect();
    if homes.is_empty() {
        return Ok(skipped(HookTarget::OpenCode));
    }
    let mut touched: Vec<PathBuf> = Vec::with_capacity(homes.len() * 2);
    for home in &homes {
        for name in ["flowmux-session.js", "flowmux-session.mjs"] {
            let plugin_path = home.join("plugins").join(name);
            if fs::read_to_string(&plugin_path)
                .is_ok_and(|source| opencode_plugin_is_owned(&source))
            {
                fs::remove_file(&plugin_path)?;
                touched.push(plugin_path);
            }
        }

        let opencode_json = home.join("opencode.json");
        if unregister_opencode_plugin(&opencode_json, "flowmux-session")? {
            touched.push(opencode_json);
        }
    }
    Ok(HookInstallReport {
        target: HookTarget::OpenCode,
        status: HookInstallStatus::Installed,
        touched_paths: touched,
    })
}

/// Back-compat single-string entry point for older tests that pass a
/// bare binary path. New call sites prefer
/// [`opencode_plugin_source_with_argv`] so the spawn array can carry
/// the Flatpak `flatpak run …` prefix.
#[cfg(test)]
fn opencode_plugin_source(flowmux_bin: &str) -> String {
    opencode_plugin_source_with_argv(&[flowmux_bin.to_string()])
}

fn opencode_plugin_source_with_argv(argv: &[String]) -> String {
    let head = argv.first().map(|s| s.as_str()).unwrap_or("flowmux");
    let trailing: Vec<String> = argv.iter().skip(1).cloned().collect();
    let trailing_literal = serde_json::to_string(&trailing).unwrap_or_else(|_| "[]".into());
    // OpenCode 1.14+ plugins are ESM modules. Path-loaded plugins
    // (`file:///…`) must export an `id` so OpenCode can name them; npm
    // packages skip that because the package name is the id. The
    // `server` factory returns a `Hooks` object whose `event` callback
    // receives every lifecycle event — we spawn the matching
    // `flowmux hooks opencode <event>` for the ones we care about.
    //
    // Events we surface today:
    // - `session.status` busy/retry → `running` (agent started/resumed)
    // - `session.status` idle       → `stop` (agent finished)
    // - `session.error`             → `notification` (agent errored)
    // - `permission.asked`          → `notification` (needs approval)
    // - `permission.replied`        → `running` (approval handled)
    //
    // The optional second positional arg is a JSON payload that the
    // matching Rust handler (`AgentHookEvent::Notification`) parses to
    // populate the toast body — keeps the alert informative instead of
    // a generic "needs your attention".
    format!(
        r#"// {marker}
// Auto-installed by `flowmux hooks setup`. Do not hand-edit; rerun the
// command instead. Removing this file is safe — flowmux just stops
// surfacing OpenCode lifecycle events to the bell popover.

import {{ spawn }} from "node:child_process";

// `FLOWMUX_BIN` is the executable invoked from the host. Outside
// Flatpak it is the absolute path to `flowmuxctl`. Inside Flatpak
// the hook installer rewrites it to `flatpak` and prepends the
// runtime args (`run --command=… com.flowmux.App`) so the spawn
// crosses back into the same sandbox the daemon lives in. Either
// way the trailing `["hooks", "opencode", <event>]` args land at the
// in-sandbox CLI unchanged.
const FLOWMUX_BIN = {bin_literal};
const FLOWMUX_ARGS_PREFIX = {trailing_literal};

// Build the final argv handed to spawn().
//
// Critical: `flatpak run` strips the host process's env to a minimal
// sandbox set before invoking the in-sandbox program, so values like
// FLOWMUX_PANE_ID never reach the in-sandbox flowmuxctl through env
// inheritance. The daemon then receives Notify{{pane:None}} and the
// sidebar can't blink / clicks can't navigate. We sidestep that by
// pushing the same values as explicit `--pane` / `--surface` flags
// at the end of the CLI invocation: argv survives the sandbox
// boundary, so the in-sandbox CLI sees the real ids regardless of
// what flatpak did to the environment.
function buildSpawnArgs(event, payload) {{
  const args = [...FLOWMUX_ARGS_PREFIX];
  args.push("hooks", "opencode", event);
  const pane = process.env.FLOWMUX_PANE_ID;
  const surface = process.env.FLOWMUX_SURFACE_ID;
  if (pane) args.push("--pane", pane);
  if (surface) args.push("--surface", surface);
  if (payload) args.push(JSON.stringify(payload));
  return args;
}}

let pendingHook = Promise.resolve();

function fireFlowmuxHook(event, payload) {{
  const args = buildSpawnArgs(event, payload);
  // CLI sequence numbers are assigned at delivery time. Preserve event order
  // so a slow busy hook cannot overwrite a newer completion or session end.
  pendingHook = pendingHook.then(() => new Promise((resolve) => {{
    try {{
      const child = spawn(FLOWMUX_BIN, args, {{ stdio: "ignore", timeout: 5000 }});
      child.on("error", resolve);
      child.on("close", resolve);
    }} catch (_) {{
      // Hook failures must never crash OpenCode or block subsequent events.
      resolve();
    }}
  }}));
  return pendingHook;
}}

export const id = "flowmux-session";

function sessionPayload(event) {{
  const properties = event && event.properties;
  if (!properties) return null;
  const session = properties.info || properties.session;
  const sessionId = properties.sessionID || properties.sessionId || properties.session_id ||
    (session && (session.id || session.sessionID || session.sessionId));
  return sessionId ? {{ session_id: String(sessionId) }} : null;
}}

export const server = async () => ({{
  event: async ({{ event }}) => {{
    if (!event || typeof event.type !== "string") return;
    const t = event.type;
    const payload = sessionPayload(event);
    if (t === "session.created") {{
      await fireFlowmuxHook("session-start", payload);
    }} else if (t === "session.deleted") {{
      await fireFlowmuxHook("session-end", payload);
    }} else if (t === "session.status") {{
      const status = event.properties && event.properties.status;
      if (status && (status.type === "busy" || status.type === "retry")) {{
        await fireFlowmuxHook("running", payload);
      }} else if (status && status.type === "idle") {{
        await fireFlowmuxHook("stop", payload);
      }}
    }} else if (t === "session.error") {{
      await fireFlowmuxHook("notification", {{ ...payload, message: "OpenCode session error" }});
    }} else if (t === "permission.asked") {{
      await fireFlowmuxHook("notification", {{ ...payload, message: "OpenCode needs your input" }});
    }} else if (t === "permission.replied") {{
      await fireFlowmuxHook("running", payload);
    }}
  }},
}});

export default {{ id, server }};
"#,
        marker = FLOWMUX_OPENCODE_PLUGIN_MARKER,
        bin_literal = serde_json::to_string(head).unwrap_or_else(|_| "\"flowmux\"".into()),
        trailing_literal = trailing_literal,
    )
}

fn unregister_opencode_plugin(path: &Path, plugin_name: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let mut root: Value = read_json_or_empty_object(path)?;
    let Some(plugins) = root
        .as_object_mut()
        .and_then(|object| object.get_mut("plugin"))
        .and_then(Value::as_array_mut)
    else {
        return Ok(false);
    };
    let before = plugins.len();
    plugins.retain(|value| {
        value
            .as_str()
            .map(|plugin| !plugin.contains(plugin_name))
            .unwrap_or(true)
    });
    if plugins.len() == before {
        return Ok(false);
    }
    write_json(path, &root)?;
    Ok(true)
}

fn opencode_plugin_is_current(source: &str) -> bool {
    source.contains(FLOWMUX_OPENCODE_PLUGIN_MARKER)
        && source.contains("session.created")
        && source.contains("session.deleted")
        && source.contains("session.status")
        && source.contains("permission.asked")
        && source.contains("permission.replied")
        && !source.contains("permission.updated")
}

fn opencode_plugin_is_owned(source: &str) -> bool {
    source.contains(FLOWMUX_OPENCODE_PLUGIN_MARKER_PREFIX)
}

// ---- Gemini CLI ----------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct GeminiEvent {
    name: &'static str,
    subcommand: &'static str,
}

const GEMINI_EVENTS: &[GeminiEvent] = &[
    GeminiEvent {
        name: "SessionStart",
        subcommand: "session-start",
    },
    GeminiEvent {
        name: "BeforeAgent",
        subcommand: "running",
    },
    GeminiEvent {
        name: "AfterAgent",
        subcommand: "stop",
    },
    GeminiEvent {
        name: "SessionEnd",
        subcommand: "session-end",
    },
    GeminiEvent {
        name: "Notification",
        subcommand: "notification",
    },
];

fn gemini_settings_path() -> Option<PathBuf> {
    host_home_dir().map(|home| home.join(".gemini").join("settings.json"))
}

fn gemini_is_installed(settings_path: &Path, binary_present: bool) -> bool {
    settings_path.exists() || binary_present
}

fn gemini_hook_entry(flowmux_bin: &str, event: GeminiEvent) -> Value {
    let command = format!(
        "{} hooks gemini {} ${{FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}} ${{FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}}  # {}",
        host_invocation_shell_command(flowmux_bin),
        event.subcommand,
        FLOWMUX_HOOK_MARKER,
    );
    json!({
        "matcher": "",
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": 5000,
        }]
    })
}

fn gemini_entry_matches(entry: &Value, event: GeminiEvent) -> bool {
    entry.get("matcher").and_then(Value::as_str) == Some("")
        && entry
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|hooks| {
                hooks.iter().any(|hook| {
                    hook.get("type").and_then(Value::as_str) == Some("command")
                        && hook
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| {
                                command.contains(FLOWMUX_HOOK_MARKER)
                                    && command
                                        .contains(&format!("hooks gemini {}", event.subcommand))
                            })
                        && hook.get("timeout").and_then(Value::as_u64) == Some(5000)
                })
            })
}

fn upsert_gemini_hooks(root: &mut Value, flowmux_bin: &str) -> Result<()> {
    let hooks = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("Gemini settings root is not a JSON object"))?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| anyhow!("Gemini hooks field is not a JSON object"))?;
    for event in GEMINI_EVENTS {
        let entries = hooks
            .entry(event.name)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| anyhow!("Gemini hook {} is not an array", event.name))?;
        prune_gemini_entries(entries);
        entries.push(gemini_hook_entry(flowmux_bin, *event));
    }
    Ok(())
}

fn prune_gemini_hooks(root: &mut Value) {
    let Some(hooks) = root
        .as_object_mut()
        .and_then(|root| root.get_mut("hooks"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    for event in GEMINI_EVENTS {
        if let Some(entries) = hooks.get_mut(event.name).and_then(Value::as_array_mut) {
            prune_gemini_entries(entries);
        }
    }
}

fn prune_gemini_entries(entries: &mut Vec<Value>) {
    entries.retain_mut(|entry| {
        let Some(hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        hooks.retain(|hook| {
            !hook
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(FLOWMUX_HOOK_MARKER))
        });
        !hooks.is_empty()
    });
}

fn install_gemini(flowmux_bin: &str) -> Result<HookInstallReport> {
    let path = match gemini_settings_path() {
        Some(path) if gemini_is_installed(&path, agent_has_real_binary("gemini")) => path,
        _ => return Ok(skipped(HookTarget::Gemini)),
    };
    let mut root = read_json_or_empty_object(&path)?;
    upsert_gemini_hooks(&mut root, flowmux_bin)?;
    let changed = write_json(&path, &root)?;
    Ok(HookInstallReport {
        target: HookTarget::Gemini,
        status: HookInstallStatus::Installed,
        touched_paths: changed.then_some(path).into_iter().collect(),
    })
}

fn uninstall_gemini() -> Result<HookInstallReport> {
    let path = match gemini_settings_path() {
        Some(path) if path.exists() => path,
        _ => return Ok(skipped(HookTarget::Gemini)),
    };
    let mut root = read_json_or_empty_object(&path)?;
    prune_gemini_hooks(&mut root);
    let changed = write_json(&path, &root)?;
    Ok(HookInstallReport {
        target: HookTarget::Gemini,
        status: HookInstallStatus::Installed,
        touched_paths: changed.then_some(path).into_iter().collect(),
    })
}

// ---- Antigravity ---------------------------------------------------

const ANTIGRAVITY_HOOK_GROUP: &str = "flowmux";
const ANTIGRAVITY_HOOK_TIMEOUT_SECS: u64 = 10;

fn antigravity_plugin_dir() -> Option<PathBuf> {
    host_home_dir().map(|home| home.join(".gemini/config/plugins/flowmux"))
}

fn antigravity_home_exists(plugin_dir: &Path) -> bool {
    plugin_dir
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .and_then(Path::parent)
        .is_some_and(crate::agent::antigravity_is_installed)
}

fn antigravity_plugin_paths(plugin_dir: &Path) -> [PathBuf; 2] {
    [
        plugin_dir.join("plugin.json"),
        plugin_dir.join("hooks.json"),
    ]
}

fn antigravity_plugin_disabled(plugin_dir: &Path) -> Result<bool> {
    let Some(config_path) = plugin_dir
        .parent()
        .and_then(Path::parent)
        .map(|config_root| config_root.join("config.json"))
    else {
        return Ok(false);
    };
    if !config_path.exists() {
        return Ok(false);
    }
    let config = read_json_or_empty_object(&config_path)?;
    Ok(config
        .get("plugins")
        .and_then(Value::as_object)
        .and_then(|plugins| plugins.get("flowmux"))
        .and_then(Value::as_object)
        .and_then(|plugin| plugin.get("enabled"))
        .and_then(Value::as_bool)
        == Some(false))
}

fn antigravity_plugin_manifest() -> Value {
    json!({ "name": "flowmux" })
}

fn antigravity_command(flowmux_bin: &str, subcommand: &str) -> String {
    let response = if subcommand == "stop" {
        r#"{"decision":""}"#
    } else {
        "{}"
    };
    format!(
        "{} hooks antigravity {subcommand} ${{FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}} ${{FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}} >/dev/null 2>&1; printf '%s' {}  # {}",
        host_invocation_shell_command(flowmux_bin),
        shell_quote(response),
        FLOWMUX_HOOK_MARKER,
    )
}

fn antigravity_command_entry(flowmux_bin: &str, subcommand: &str) -> Value {
    json!({
        "type": "command",
        "command": antigravity_command(flowmux_bin, subcommand),
        "timeout": ANTIGRAVITY_HOOK_TIMEOUT_SECS,
    })
}

fn antigravity_hook_group(flowmux_bin: &str) -> Value {
    json!({
        "PreInvocation": [antigravity_command_entry(flowmux_bin, "running")],
        "PostToolUse": [{
            "matcher": "*",
            "hooks": [antigravity_command_entry(flowmux_bin, "running")],
        }],
        "Stop": [antigravity_command_entry(flowmux_bin, "stop")],
    })
}

fn antigravity_command_matches(value: &Value, subcommand: &str) -> bool {
    let response = if subcommand == "stop" {
        r#"{"decision":""}"#
    } else {
        "{}"
    };
    value.get("type").and_then(Value::as_str) == Some("command")
        && value.get("timeout").and_then(Value::as_u64) == Some(ANTIGRAVITY_HOOK_TIMEOUT_SECS)
        && value
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| {
                command.contains(FLOWMUX_HOOK_MARKER)
                    && command.contains(&format!("hooks antigravity {subcommand}"))
                    && command.contains(&format!(
                        ">/dev/null 2>&1; printf '%s' {}",
                        shell_quote(response)
                    ))
                    && command.contains("${FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}")
                    && command.contains("${FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}")
            })
}

fn antigravity_group_matches(group: &Value) -> bool {
    let Some(group) = group.as_object().filter(|group| group.len() == 3) else {
        return false;
    };
    let flat_event_matches = |name: &str, subcommand: &str| {
        group
            .get(name)
            .and_then(Value::as_array)
            .filter(|entries| entries.len() == 1)
            .is_some_and(|entries| antigravity_command_matches(&entries[0], subcommand))
    };
    let post_tool_use_matches = group
        .get("PostToolUse")
        .and_then(Value::as_array)
        .filter(|entries| entries.len() == 1)
        .and_then(|entries| entries[0].as_object())
        .is_some_and(|entry| {
            entry.len() == 2
                && entry.get("matcher").and_then(Value::as_str) == Some("*")
                && entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .filter(|hooks| hooks.len() == 1)
                    .is_some_and(|hooks| antigravity_command_matches(&hooks[0], "running"))
        });
    flat_event_matches("PreInvocation", "running")
        && post_tool_use_matches
        && flat_event_matches("Stop", "stop")
}

fn antigravity_group_is_owned(value: &Value) -> bool {
    match value {
        Value::Array(values) => values.iter().any(antigravity_group_is_owned),
        Value::Object(object) => {
            object
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| {
                    command.contains(FLOWMUX_HOOK_MARKER) && command.contains("hooks antigravity")
                })
                || object.values().any(antigravity_group_is_owned)
        }
        _ => false,
    }
}

fn install_antigravity(flowmux_bin: &str) -> Result<HookInstallReport> {
    let plugin_dir = match antigravity_plugin_dir() {
        Some(path) if antigravity_home_exists(&path) => path,
        _ => return Ok(skipped(HookTarget::Antigravity)),
    };
    install_antigravity_in(&plugin_dir, flowmux_bin)
}

fn install_antigravity_in(plugin_dir: &Path, flowmux_bin: &str) -> Result<HookInstallReport> {
    if antigravity_plugin_disabled(plugin_dir)? {
        return Err(anyhow!(
            "Antigravity flowmux plugin is explicitly disabled in config.json"
        ));
    }
    let [manifest_path, hooks_path] = antigravity_plugin_paths(plugin_dir);
    let manifest_exists = manifest_path.exists();
    let hooks_exists = hooks_path.exists();
    let manifest = read_json_or_empty_object(&manifest_path)?;
    let hooks = read_json_or_empty_object(&hooks_path)?;
    if !manifest.is_object() || !hooks.is_object() {
        return Err(anyhow!(
            "Antigravity flowmux plugin files must be JSON objects"
        ));
    }
    let hooks_owned = hooks
        .get(ANTIGRAVITY_HOOK_GROUP)
        .is_some_and(antigravity_group_is_owned);
    if (manifest_exists || hooks_exists) && !hooks_owned {
        return Err(anyhow!(
            "{} exists and is not managed by flowmux",
            plugin_dir.display()
        ));
    }

    let hooks = json!({
        (ANTIGRAVITY_HOOK_GROUP): antigravity_hook_group(flowmux_bin),
    });
    let hooks_changed = write_json(&hooks_path, &hooks)?;
    let manifest_changed = write_json(&manifest_path, &antigravity_plugin_manifest())?;
    let touched_paths = [
        (manifest_changed, manifest_path),
        (hooks_changed, hooks_path),
    ]
    .into_iter()
    .filter_map(|(changed, path)| changed.then_some(path))
    .collect();
    Ok(HookInstallReport {
        target: HookTarget::Antigravity,
        status: HookInstallStatus::Installed,
        touched_paths,
    })
}

fn uninstall_antigravity() -> Result<HookInstallReport> {
    let plugin_dir = match antigravity_plugin_dir() {
        Some(path) if path.exists() || antigravity_home_exists(&path) => path,
        _ => return Ok(skipped(HookTarget::Antigravity)),
    };
    uninstall_antigravity_in(&plugin_dir)
}

fn uninstall_antigravity_in(plugin_dir: &Path) -> Result<HookInstallReport> {
    let [manifest_path, hooks_path] = antigravity_plugin_paths(plugin_dir);
    let manifest_matches = manifest_path.exists()
        && read_json_or_empty_object(&manifest_path)? == antigravity_plugin_manifest();
    let hooks_owned = hooks_path.exists()
        && read_json_or_empty_object(&hooks_path)?
            .get(ANTIGRAVITY_HOOK_GROUP)
            .is_some_and(antigravity_group_is_owned);
    let mut touched_paths = Vec::new();
    if hooks_owned {
        fs::remove_file(&hooks_path).with_context(|| format!("remove {}", hooks_path.display()))?;
        touched_paths.push(hooks_path.clone());
    }
    if hooks_owned && manifest_matches {
        fs::remove_file(&manifest_path)
            .with_context(|| format!("remove {}", manifest_path.display()))?;
        touched_paths.push(manifest_path);
    }
    if fs::symlink_metadata(plugin_dir).is_ok_and(|metadata| metadata.file_type().is_dir())
        && fs::read_dir(plugin_dir)?.next().is_none()
    {
        fs::remove_dir(plugin_dir)
            .with_context(|| format!("remove empty directory {}", plugin_dir.display()))?;
    }
    Ok(HookInstallReport {
        target: HookTarget::Antigravity,
        status: HookInstallStatus::Installed,
        touched_paths,
    })
}

// ---- shared helpers -------------------------------------------------

fn skipped(target: HookTarget) -> HookInstallReport {
    HookInstallReport {
        target,
        status: HookInstallStatus::Skipped,
        touched_paths: vec![],
    }
}

fn read_json_or_empty_object(path: &Path) -> Result<Value> {
    match fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            serde_json::from_str(&flowmux_config::cmux_json::strip_jsonc_comments(&s))
                .with_context(|| format!("parse JSON: {}", path.display()))
        }
        Ok(_) => Ok(json!({})),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(anyhow::Error::from(e)).context(format!("read {}", path.display())),
    }
}

fn write_json(path: &Path, value: &Value) -> Result<bool> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let body = serde_json::to_string_pretty(value)
        .with_context(|| format!("serialize {}", path.display()))?;
    write_atomic(path, body.as_bytes())
}

fn write_atomic(path: &Path, body: &[u8]) -> Result<bool> {
    // Preserve a user-managed final-component symlink. Renaming directly onto
    // it would replace the link itself and leave its real config unchanged.
    // `canonicalize` also rejects dangling and looping links instead of
    // silently converting them into regular files.
    let write_path = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => fs::canonicalize(path)
            .with_context(|| format!("resolve config symlink {}", path.display()))?,
        Ok(_) => path.to_path_buf(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(error) => {
            return Err(anyhow::Error::from(error)).context(format!("inspect {}", path.display()))
        }
    };
    // Keep idempotent setup from changing user config mtimes.
    if fs::read(&write_path).is_ok_and(|current| current == body) {
        return Ok(false);
    }
    #[cfg(unix)]
    let target_mode = {
        use std::os::unix::fs::PermissionsExt;
        match fs::metadata(&write_path) {
            Ok(metadata) => metadata.permissions().mode(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0o600,
            Err(error) => {
                return Err(anyhow::Error::from(error))
                    .context(format!("inspect {}", write_path.display()))
            }
        }
    };
    let parent = write_path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", write_path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let tmp = parent.join(format!(
        ".{}.flowmux-tmp",
        write_path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("hook")
    ));
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&tmp)
        .with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The predictable temp may survive an interrupted earlier write. Apply
        // the target's exact mode (or private-by-default mode for a new file)
        // before placing any agent configuration in it.
        fs::set_permissions(&tmp, fs::Permissions::from_mode(target_mode))
            .with_context(|| format!("set permissions on {}", tmp.display()))?;
    }
    file.write_all(body)
        .with_context(|| format!("write {}", tmp.display()))?;
    drop(file);
    fs::rename(&tmp, &write_path)
        .with_context(|| format!("rename {} → {}", tmp.display(), write_path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// A tiny fixture that overrides `dirs::home_dir` via env. We just
    /// use TempDir + explicit paths instead.
    fn tmp() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn cline_cleanup_removes_managed_and_preserves_user_hook() {
        let dir = tmp();
        let hooks_dir = dir.path().join(".cline/hooks");
        fs::create_dir_all(&hooks_dir).unwrap();
        let manual = hooks_dir.join("TaskComplete");
        fs::write(&manual, "#!/bin/sh\nprintf manual\\n\n").unwrap();
        let dirs = vec![hooks_dir.clone()];

        for event in CLINE_EVENTS
            .iter()
            .filter(|event| **event != "TaskComplete")
        {
            fs::write(
                hooks_dir.join(event),
                "#!/bin/sh\n# flowmux-hook obsolete\n",
            )
            .unwrap();
        }
        assert_eq!(check_cline_in_dirs(&dirs).status, HookCheckStatus::Drift);

        let report = uninstall_cline_in_dirs(&dirs).unwrap();
        assert_eq!(report.touched_paths.len(), CLINE_EVENTS.len() - 1);
        assert!(manual.exists());
        assert_eq!(
            fs::read_to_string(&manual).unwrap(),
            "#!/bin/sh\nprintf manual\\n\n"
        );
    }

    #[test]
    fn cline_hook_dirs_include_current_and_existing_legacy_roots() {
        let dir = tmp();
        fs::create_dir_all(dir.path().join(".cline")).unwrap();
        fs::create_dir_all(dir.path().join("Documents/Cline")).unwrap();

        assert_eq!(
            cline_hook_dirs_for_home(dir.path()),
            vec![
                dir.path().join(".cline/hooks"),
                dir.path().join("Documents/Cline/Hooks"),
            ]
        );
    }

    #[test]
    fn claude_install_creates_settings_with_lifecycle_event_entries() {
        let dir = tmp();
        let claude_dir = dir.path().join(".claude");
        fs::create_dir_all(&claude_dir).unwrap();
        let path = claude_dir.join("settings.json");
        // Fresh install path: empty file.
        let mut root = read_json_or_empty_object(&path).unwrap();
        let hooks = root
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        for event in CLAUDE_EVENTS {
            let arr = hooks
                .entry(event.name.to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap();
            upsert_flowmux_hook_entry(arr, claude_hook_entry("flowmux", *event));
        }
        write_json(&path, &root).unwrap();

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let stop = &written["hooks"]["Stop"][0];
        assert_eq!(stop["matcher"], "");
        let cmd = stop["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains("flowmux hooks claude stop"));
        assert!(cmd.contains(FLOWMUX_HOOK_MARKER));

        // The activity-tracking lifecycle events are installed alongside
        // Stop/Notification, each mapped to its kebab-case subcommand.
        for (name, subcommand) in [
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "prompt-submit"),
            ("PreToolUse", "pre-tool-use"),
            ("PermissionRequest", "permission-request"),
            ("PostToolUse", "post-tool-use"),
            ("PostToolBatch", "post-tool-batch"),
            ("PostToolUseFailure", "post-tool-use-failure"),
            ("PermissionDenied", "permission-denied"),
            ("SessionEnd", "session-end"),
        ] {
            let cmd = written["hooks"][name][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_else(|| panic!("missing hook entry for {name}"));
            assert!(
                cmd.contains(&format!("flowmux hooks claude {subcommand}")),
                "event {name} should invoke `{subcommand}`, got: {cmd}"
            );
            assert!(cmd.contains("${FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}"));
            assert!(cmd.contains("${FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}"));
        }
        assert_eq!(
            written["hooks"]["SessionStart"][0]["matcher"],
            "^(startup|resume|clear|fork)$"
        );
    }

    #[test]
    fn shim_script_exports_agent_pid_and_execs_real_binary() {
        let body = shim_script("claude");
        assert!(body.starts_with("#!/usr/bin/env bash"));
        assert!(body.contains("export FLOWMUX_AGENT_PID=$$"));
        assert!(body.contains("export FLOWMUX_AGENT_NAME=claude"));
        // Only when inside flowmux, so it stays transparent elsewhere.
        assert!(body.contains("FLOWMUX_SURFACE_ID"));
        // Native hooks own lifecycle ordering; the shim must not race them.
        assert!(!body.contains("hooks claude session-start"));
        assert!(body.contains("FLOWMUX_CLAUDE_SESSION_NAME"));
        assert!(body.contains("session-name"));
        assert!(body.contains("patch >= 224"));
        // Skips its own dir, skips other flowmux shims, and exec's the resolved real binary.
        assert!(body.contains("[ \"$d\" = \"$self_dir\" ] && continue"));
        assert!(body.contains("is_flowmux_shim \"$candidate\" && continue"));
        assert!(body.contains("exec \"$real\" \"$@\""));
        // Agent name is substituted into the lookup.
        assert!(body.contains("$d/claude"));
    }

    #[test]
    fn agy_shim_exports_canonical_antigravity_identity() {
        assert!(SHIM_AGENTS.contains(&"agy"));
        let body = shim_script("agy");
        assert!(body.contains("export FLOWMUX_AGENT_NAME=antigravity"));
        assert!(body.contains("candidate=\"$d/agy\""));
    }

    #[test]
    fn claude_shim_injects_supported_auto_name_and_preserves_user_args() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tmp();
        let shim_dir = dir.path().join("shims");
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&shim_dir).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        let log = dir.path().join("claude.log");
        let shim = shim_dir.join("claude");
        fs::write(&shim, shim_script("claude")).unwrap();
        fs::write(
            bin_dir.join("claude"),
            format!(
                "#!/bin/bash\nif [ \"$1\" = --version ]; then echo \"${{FAKE_VERSION}} (Claude Code)\"; exit; fi\nprintf '%s|%s\\n' \"${{FLOWMUX_CLAUDE_SESSION_NAME:-}}\" \"$*\" > '{}'\n",
                log.display()
            ),
        )
        .unwrap();
        fs::write(
            bin_dir.join("flowmuxctl"),
            "#!/bin/bash\n[ \"$1\" = session-name ] && echo demo-1234-abcd\n",
        )
        .unwrap();
        for path in [
            shim.clone(),
            bin_dir.join("claude"),
            bin_dir.join("flowmuxctl"),
        ] {
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).unwrap();
        }
        let run = |args: &[&str], version: &str| {
            let status = Command::new(&shim)
                .args(args)
                .env_clear()
                .env(
                    "PATH",
                    format!("{}:{}:/usr/bin:/bin", shim_dir.display(), bin_dir.display()),
                )
                .env("FLOWMUX_SURFACE_ID", "surface")
                .env("FAKE_VERSION", version)
                .status()
                .unwrap();
            assert!(status.success());
            fs::read_to_string(&log).unwrap()
        };

        assert_eq!(
            run(&[], "2.1.226"),
            "demo-1234-abcd|--name demo-1234-abcd\n"
        );
        assert_eq!(run(&["--name", "mine"], "2.1.226"), "mine|--name mine\n");
        assert_eq!(run(&[], "2.1.223"), "|\n");
        assert_eq!(run(&["--bare"], "2.1.226"), "|--bare\n");
    }

    #[test]
    fn legacy_local_shim_detection_preserves_real_binary() {
        let dir = tmp();
        let path = dir.path().join("codex");

        assert!(!is_legacy_local_agent_shim(&path));
        fs::write(&path, "#!/bin/sh\necho real codex\n").unwrap();
        assert!(!is_legacy_local_agent_shim(&path));
        fs::write(&path, shim_script("codex")).unwrap();
        assert!(is_legacy_local_agent_shim(&path));
    }

    #[test]
    fn agent_binary_detection_skips_shims() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmp();
        let shim_dir = dir.path().join("shims");
        let real_dir = dir.path().join("real");
        fs::create_dir_all(&shim_dir).unwrap();
        fs::create_dir_all(&real_dir).unwrap();
        fs::write(shim_dir.join("cline"), shim_script("cline")).unwrap();
        let mut permissions = fs::metadata(shim_dir.join("cline")).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(shim_dir.join("cline"), permissions).unwrap();

        let shim_only = std::env::join_paths([&shim_dir]).unwrap();
        assert!(!agent_has_real_binary_on_path("cline", &shim_only));

        fs::write(real_dir.join("cline"), "#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(real_dir.join("cline")).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(real_dir.join("cline"), permissions).unwrap();
        let with_real = std::env::join_paths([&shim_dir, &real_dir]).unwrap();
        assert!(agent_has_real_binary_on_path("cline", &with_real));
    }

    #[test]
    fn shim_uninstall_preserves_unmanaged_files() {
        let dir = tmp();
        let path = dir.path().join("codex");
        fs::write(&path, "real codex").unwrap();
        assert!(!remove_owned_shim(&path, "flowmux agent wrapper shim").unwrap());
        assert!(path.exists());

        fs::write(&path, shim_script("codex")).unwrap();
        assert!(remove_owned_shim(&path, "flowmux agent wrapper shim").unwrap());
        assert!(!path.exists());
    }

    #[test]
    fn tmux_shim_intercepts_swarm_and_passes_through_everything_else() {
        let body = tmux_shim_script();
        // Marker so the shim recognizes (and skips) itself on PATH.
        assert!(body.contains("flowmux tmux compat shim"));
        // Swarm socket/session names and pane UUIDs are intercepted…
        assert!(body.contains("claude-swarm|claude-swarm:*|claude-swarm-*"));
        assert!(body.contains("????????-????-????-????-????????????"));
        // …and routed to the tmux-compat verb.
        assert!(body.contains("exec flowmuxctl tmux-compat \"$@\""));
        // Interception only happens inside a flowmux pane, with an
        // escape hatch.
        assert!(body.contains("FLOWMUX_SOCKET_PATH"));
        assert!(body.contains("FLOWMUX_TMUX_SHIM"));
        // Non-swarm usage execs the real tmux, skipping the shim dir.
        assert!(body.contains("[ \"$d\" = \"$self_dir\" ] && continue"));
        assert!(body.contains("exec \"$real\" \"$@\""));
    }

    #[test]
    fn tmux_shim_install_is_idempotent_and_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp();
        let written = install_tmux_shim_into(dir.path()).unwrap();
        assert_eq!(written.len(), 1);
        let path = &written[0];
        assert_eq!(path.file_name().unwrap(), "tmux");
        let mode = fs::metadata(path).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "shim must be executable");

        // Second run: nothing to do.
        let written = install_tmux_shim_into(dir.path()).unwrap();
        assert!(written.is_empty());

        // Drift (edited body) is re-synced.
        fs::write(dir.path().join("tmux"), "#!/bin/sh\n").unwrap();
        let written = install_tmux_shim_into(dir.path()).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(
            fs::read_to_string(dir.path().join("tmux")).unwrap(),
            tmux_shim_script()
        );
    }

    /// Run the installed shim under real bash with a controlled PATH:
    /// fake `flowmuxctl` and fake `tmux` record their argv, so the
    /// routing decision (intercept vs pass-through) is observable.
    #[test]
    fn tmux_shim_routes_correctly_under_bash() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tmp();
        let shim_dir = dir.path().join("shims");
        install_tmux_shim_into(&shim_dir).unwrap();
        let bin_dir = dir.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let log = dir.path().join("calls.log");

        let fake = |name: &str, label: &str| {
            let path = bin_dir.join(name);
            fs::write(
                &path,
                format!("#!/bin/sh\necho \"{label}: $*\" >> '{}'\n", log.display()),
            )
            .unwrap();
            let mut p = fs::metadata(&path).unwrap().permissions();
            p.set_mode(0o755);
            fs::set_permissions(&path, p).unwrap();
        };
        fake("flowmuxctl", "ctl");
        fake("tmux", "realtmux");

        // A coreutils-only dir (bash + the externals the shim calls) that
        // deliberately omits any system `tmux`. The "no real tmux installed"
        // cases run with this on PATH instead of /usr/bin, so they stay
        // reproducible on CI images that ship tmux in /usr/bin.
        let tools_dir = dir.path().join("tools");
        fs::create_dir_all(&tools_dir).unwrap();
        for tool in ["bash", "dirname", "grep"] {
            let src = ["/bin", "/usr/bin", "/usr/local/bin"]
                .iter()
                .map(|d| std::path::Path::new(d).join(tool))
                .find(|p| p.exists())
                .unwrap_or_else(|| panic!("required tool not found: {tool}"));
            std::os::unix::fs::symlink(src, tools_dir.join(tool)).unwrap();
        }

        let run = |args: &[&str], socket: Option<&str>, shim_env: Option<&str>| {
            let mut cmd = Command::new(shim_dir.join("tmux"));
            // Keep the system dirs so bash/dirname/grep resolve; our
            // dirs come first so the fakes win.
            cmd.args(args).env_clear().env(
                "PATH",
                format!("{}:{}:/usr/bin:/bin", shim_dir.display(), bin_dir.display()),
            );
            if let Some(s) = socket {
                cmd.env("FLOWMUX_SOCKET_PATH", s);
            }
            if let Some(v) = shim_env {
                cmd.env("FLOWMUX_TMUX_SHIM", v);
            }
            let status = cmd.status().unwrap();
            let calls = fs::read_to_string(&log).unwrap_or_default();
            fs::write(&log, "").unwrap();
            (status, calls)
        };

        // Swarm-shaped + inside flowmux pane → intercepted.
        let (status, calls) = run(
            &["-L", "claude-swarm-42", "has-session", "-t", "claude-swarm"],
            Some("/tmp/flowmux.sock"),
            None,
        );
        assert!(status.success());
        assert_eq!(
            calls.trim(),
            "ctl: tmux-compat -L claude-swarm-42 has-session -t claude-swarm"
        );

        // Legacy path: pane-UUID target without -L is intercepted too.
        let (_, calls) = run(
            &["kill-pane", "-t", "0b8e7f66-90bc-4f74-9e2e-7f3f4be2a111"],
            Some("/tmp/flowmux.sock"),
            None,
        );
        assert!(calls.starts_with("ctl: tmux-compat kill-pane"));

        // Ordinary tmux usage passes through to the real tmux.
        let (_, calls) = run(
            &["new-session", "-s", "mywork"],
            Some("/tmp/flowmux.sock"),
            None,
        );
        assert_eq!(calls.trim(), "realtmux: new-session -s mywork");

        // Outside a flowmux pane, even swarm shapes pass through.
        let (_, calls) = run(
            &["-L", "claude-swarm-42", "has-session", "-t", "claude-swarm"],
            None,
            None,
        );
        assert!(calls.starts_with("realtmux:"));

        // Escape hatch disables interception.
        let (_, calls) = run(
            &["-L", "claude-swarm-42", "has-session", "-t", "claude-swarm"],
            Some("/tmp/flowmux.sock"),
            Some("0"),
        );
        assert!(calls.starts_with("realtmux:"));

        // No real tmux installed: the availability probe still answers
        // through tmux-compat inside a flowmux pane. Runs on an isolated
        // PATH (coreutils only, no system tmux) so this holds on CI.
        fs::remove_file(bin_dir.join("tmux")).unwrap();
        let run_no_tmux = |args: &[&str], socket: Option<&str>| {
            let mut cmd = Command::new(shim_dir.join("tmux"));
            cmd.args(args).env_clear().env(
                "PATH",
                format!(
                    "{}:{}:{}",
                    shim_dir.display(),
                    bin_dir.display(),
                    tools_dir.display()
                ),
            );
            if let Some(s) = socket {
                cmd.env("FLOWMUX_SOCKET_PATH", s);
            }
            let status = cmd.status().unwrap();
            let calls = fs::read_to_string(&log).unwrap_or_default();
            fs::write(&log, "").unwrap();
            (status, calls)
        };
        let (status, calls) = run_no_tmux(&["-V"], Some("/tmp/flowmux.sock"));
        assert!(status.success());
        assert_eq!(calls.trim(), "ctl: tmux-compat -V");

        // …but outside a pane it reports tmux as missing.
        let (status, calls) = run_no_tmux(&["-V"], None);
        assert_eq!(status.code(), Some(127));
        assert_eq!(calls.trim(), "");
    }

    #[test]
    fn claude_install_is_idempotent_and_preserves_user_entries() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        // Pre-existing user hook the user wants kept.
        let initial = json!({
            "hooks": {
                "Stop": [{
                    "matcher": "Bash",
                    "hooks": [{ "type": "command", "command": "/usr/local/bin/userscript.sh", "timeout": 30 }]
                }]
            }
        });
        write_json(&path, &initial).unwrap();

        // Install once.
        let mut root: Value = read_json_or_empty_object(&path).unwrap();
        upsert_claude_for_test(&mut root, "flowmux");
        write_json(&path, &root).unwrap();

        // Install twice.
        let mut root: Value = read_json_or_empty_object(&path).unwrap();
        upsert_claude_for_test(&mut root, "flowmux");
        write_json(&path, &root).unwrap();

        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let stop = written["hooks"]["Stop"].as_array().unwrap();
        // Exactly 2: the user's + flowmux's. No duplicate flowmux.
        assert_eq!(stop.len(), 2, "got: {stop:?}");
        // User entry is unchanged.
        assert_eq!(
            stop[0]["hooks"][0]["command"],
            "/usr/local/bin/userscript.sh"
        );
        // flowmux entry has the marker.
        let cmd = stop[1]["hooks"][0]["command"].as_str().unwrap();
        assert!(cmd.contains(FLOWMUX_HOOK_MARKER));
    }

    #[test]
    fn claude_install_rejects_malformed_hook_shapes_without_rewriting() {
        let dir = tmp();
        let cases: [(&str, &[u8]); 3] = [
            ("root-array.json", b"[]\n"),
            ("hooks-array.json", b"{\"hooks\":[]}\n"),
            ("event-object.json", b"{\"hooks\":{\"SessionEnd\":{}}}\n"),
        ];

        for (name, original) in cases {
            let path = dir.path().join(name);
            fs::write(&path, original).unwrap();

            assert!(install_claude_in(&path, "flowmux").is_err());
            assert_eq!(fs::read(&path).unwrap(), original.to_vec());
        }
    }

    #[test]
    fn claude_hooks_cover_api_failures_and_immediate_attention() {
        let failure = CLAUDE_EVENTS
            .iter()
            .find(|event| event.name == "StopFailure")
            .copied()
            .unwrap();
        assert!(claude_entry_matches(
            &claude_hook_entry("flowmux", failure),
            failure
        ));

        let notification = CLAUDE_EVENTS
            .iter()
            .find(|event| event.name == "Notification")
            .copied()
            .unwrap();
        let entry = claude_hook_entry("flowmux", notification);
        let matcher = entry["matcher"].as_str().unwrap();
        assert!(matcher.contains("permission_prompt"));
        assert!(!matcher.contains("idle_prompt"));
        let mut stale = entry;
        stale["matcher"] = json!("");
        assert!(!claude_entry_matches(&stale, notification));

        let permission = CLAUDE_EVENTS
            .iter()
            .find(|event| event.name == "PermissionRequest")
            .copied()
            .unwrap();
        assert!(claude_entry_matches(
            &claude_hook_entry("flowmux", permission),
            permission
        ));
        for name in ["PostToolBatch", "PostToolUseFailure", "PermissionDenied"] {
            let event = CLAUDE_EVENTS
                .iter()
                .find(|event| event.name == name)
                .copied()
                .unwrap();
            assert!(claude_entry_matches(
                &claude_hook_entry("flowmux", event),
                event
            ));
        }
    }

    fn upsert_claude_for_test(root: &mut Value, bin: &str) {
        let hooks = root
            .as_object_mut()
            .unwrap()
            .entry("hooks")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .unwrap();
        for event in CLAUDE_EVENTS {
            let arr = hooks
                .entry(event.name.to_string())
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .unwrap();
            upsert_flowmux_hook_entry(arr, claude_hook_entry(bin, *event));
        }
    }

    #[test]
    fn claude_uninstall_removes_only_flowmux_entries() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        let initial = json!({
            "hooks": {
                "Stop": [
                    { "matcher": "", "hooks": [{ "type": "command", "command": "user_thing", "timeout": 5 }] },
                    { "matcher": "", "hooks": [{ "type": "command", "command": "/usr/bin/flowmux hooks claude stop  # flowmux-hook", "timeout": 10 }] }
                ]
            }
        });
        write_json(&path, &initial).unwrap();
        let mut root: Value = read_json_or_empty_object(&path).unwrap();
        if let Some(arr) = root["hooks"]["Stop"].as_array_mut() {
            prune_flowmux_claude_entries(arr);
        }
        write_json(&path, &root).unwrap();
        let written: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let stop = written["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][0]["command"], "user_thing");
    }

    #[test]
    fn claude_shared_matcher_group_preserves_user_handler() {
        let user_hook = json!({
            "type": "command",
            "command": "/usr/local/bin/user-hook",
            "timeout": 30,
        });
        let mut root = json!({
            "hooks": {
                "Stop": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "old-flowmux hooks claude stop # flowmux-hook",
                        "timeout": 1,
                    }, user_hook.clone()]
                }]
            }
        });

        upsert_claude_for_test(&mut root, "flowmux");
        let stop = root["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["hooks"][1], user_hook);

        prune_flowmux_claude_entries(root["hooks"]["Stop"].as_array_mut().unwrap());
        assert_eq!(root["hooks"]["Stop"][0]["hooks"], json!([user_hook]));
    }

    #[test]
    fn codex_native_entries_match_current_schema() {
        let mut root = json!({});
        let executable = std::env::current_exe().unwrap();
        upsert_codex_hooks(&mut root, executable.to_str().unwrap()).unwrap();

        assert!(root.get("SessionStart").is_none());
        for event in CODEX_EVENTS {
            assert_eq!(codex_matching_entry_count(&root, *event), 1);
            let entry = &root["hooks"][event.name][0];
            assert_eq!(entry["matcher"], event.matcher);
            assert_eq!(entry["hooks"][0]["timeout"], event.timeout_secs);
            let command = entry["hooks"][0]["command"].as_str().unwrap();
            assert!(command.contains(&format!("hooks codex {}", event.subcommand)));
            assert!(command.contains("FLOWMUX_PANE_ID"));
            assert!(command.contains("FLOWMUX_SURFACE_ID"));
            assert!(command.contains(FLOWMUX_HOOK_MARKER));
        }
        assert_eq!(
            root["hooks"]["SessionStart"][0]["matcher"],
            "^(startup|resume|clear)$"
        );
    }

    #[test]
    fn codex_native_upsert_is_idempotent_and_preserves_user_handlers() {
        let user_hook = json!({
            "type": "command",
            "command": "/usr/local/bin/user-hook",
            "timeout": 9,
        });
        let mut root = json!({
            "description": "keep me",
            "hooks": {
                "Stop": [{
                    "matcher": "",
                    "hooks": [
                        user_hook.clone(),
                        {
                            "type": "command",
                            "command": "old hooks claude stop # flowmux-hook",
                            "timeout": 1,
                        }
                    ]
                }],
                "SubagentStop": [{
                    "hooks": [user_hook.clone()]
                }]
            },
            "Stop": [{
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": "old hooks codex stop # flowmux-hook",
                    "timeout": 1,
                }]
            }]
        });

        upsert_codex_hooks(&mut root, "flowmux").unwrap();
        let after_first = root.clone();
        upsert_codex_hooks(&mut root, "flowmux").unwrap();

        assert_eq!(root, after_first);
        assert_eq!(root["description"], "keep me");
        assert!(root.get("Stop").is_none());
        assert_eq!(
            root["hooks"]["Stop"][0]["hooks"],
            json!([
                user_hook.clone(),
                codex_hook_entry(
                    "flowmux",
                    *CODEX_EVENTS
                        .iter()
                        .find(|event| event.name == "Stop")
                        .unwrap()
                )["hooks"][0]
                    .clone()
            ])
        );
        assert_eq!(
            root["hooks"]["SubagentStop"][0]["hooks"],
            json!([user_hook])
        );
        assert_eq!(codex_owned_hook_count(&root), CODEX_EVENTS.len());
        for event in CODEX_EVENTS {
            assert_eq!(codex_matching_entry_count(&root, *event), 1);
        }
    }

    #[test]
    fn codex_native_prune_preserves_unrelated_state() {
        let mut root = json!({
            "description": "keep me",
            "hooks": {
                "Stop": [{
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": "/usr/local/bin/user-hook",
                    }]
                }],
                "UserEvent": []
            }
        });
        upsert_codex_hooks(&mut root, "flowmux").unwrap();
        prune_codex_hooks(&mut root);

        assert_eq!(root["description"], "keep me");
        assert_eq!(root["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(root["hooks"]["UserEvent"], json!([]));
        assert_eq!(codex_owned_hook_count(&root), 0);
    }

    #[cfg(unix)]
    #[test]
    fn codex_uninstall_preserves_final_symlink_and_clears_target() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tmp();
        let home = dir.path().join("codex");
        fs::create_dir(&home).unwrap();
        let target = dir.path().join("managed-hooks.json");
        let hooks_path = home.join("hooks.json");
        let mut root = json!({});
        upsert_codex_hooks(&mut root, "flowmux").unwrap();
        write_json(&target, &root).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).unwrap();
        symlink("../managed-hooks.json", &hooks_path).unwrap();

        let report = uninstall_codex_in(&home).unwrap();

        assert_eq!(report.touched_paths, vec![hooks_path.clone()]);
        assert!(fs::symlink_metadata(&hooks_path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(read_json_or_empty_object(&target).unwrap(), json!({}));
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[test]
    fn codex_native_upsert_preserves_user_handler_indices() {
        let stop = *CODEX_EVENTS
            .iter()
            .find(|event| event.name == "Stop")
            .unwrap();
        let user_hook = json!({
            "type": "command",
            "command": "/usr/local/bin/user-hook",
            "timeout": 9,
        });
        let mut root = json!({
            "hooks": {
                "Stop": [{
                    "matcher": "",
                    "hooks": [codex_hook_entry("old-flowmux", stop)["hooks"][0].clone(), user_hook.clone()]
                }]
            }
        });

        upsert_codex_hooks(&mut root, "flowmux").unwrap();
        upsert_codex_hooks(&mut root, "flowmux").unwrap();

        let hooks = root["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 2);
        assert!(hook_is_flowmux_owned(&hooks[0]));
        assert_eq!(hooks[1], user_hook);
    }

    #[test]
    fn codex_native_upsert_preserves_owned_group_position_across_legacy_matchers() {
        let session_start = *CODEX_EVENTS
            .iter()
            .find(|event| event.name == "SessionStart")
            .unwrap();
        let replacement = codex_hook_entry("flowmux", session_start);
        let user_before = json!({
            "matcher": "before",
            "hooks": [{ "type": "command", "command": "user-before" }],
        });
        let user_after = json!({
            "matcher": "after",
            "hooks": [{ "type": "command", "command": "user-after" }],
        });

        for legacy_matcher in [None, Some("^legacy$")] {
            let mut legacy = codex_hook_entry("old-flowmux", session_start);
            legacy["futureGroupField"] = json!({ "preserve": true });
            legacy["hooks"][0]["futureHandlerField"] = json!(["preserve"]);
            match legacy_matcher {
                Some(matcher) => legacy["matcher"] = json!(matcher),
                None => {
                    legacy.as_object_mut().unwrap().remove("matcher");
                }
            }
            let mut entries = vec![user_before.clone(), legacy, user_after.clone()];
            let mut expected = replacement.clone();
            expected["futureGroupField"] = json!({ "preserve": true });
            expected["hooks"][0]["futureHandlerField"] = json!(["preserve"]);

            upsert_flowmux_hook_entry(&mut entries, replacement.clone());

            assert_eq!(
                entries,
                vec![user_before.clone(), expected, user_after.clone()]
            );
        }
    }

    #[test]
    fn codex_native_upsert_preserves_owned_handler_extension_fields() {
        let stop = *CODEX_EVENTS
            .iter()
            .find(|event| event.name == "Stop")
            .unwrap();
        let mut existing = codex_hook_entry("old-flowmux", stop);
        existing["hooks"][0]["futureHandlerField"] = json!({ "preserve": true });
        let mut entries = vec![existing];

        upsert_flowmux_hook_entry(&mut entries, codex_hook_entry("flowmux", stop));

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["hooks"][0]["futureHandlerField"],
            json!({ "preserve": true })
        );
        assert!(entries[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .starts_with("flowmux hooks codex stop"));
    }

    #[test]
    fn claude_native_upsert_drops_stale_schema_controlled_handler_fields() {
        let stop = *CLAUDE_EVENTS
            .iter()
            .find(|event| event.name == "Stop")
            .unwrap();
        let mut existing = claude_hook_entry("old-flowmux", stop);
        let handler = &mut existing["hooks"][0];
        handler["if"] = json!("Bash(*)");
        handler["args"] = json!([]);
        handler["asyncRewake"] = json!(true);
        handler["shell"] = json!("powershell");
        handler["once"] = json!(true);
        let mut entries = vec![existing];

        upsert_flowmux_hook_entry(&mut entries, claude_hook_entry("flowmux", stop));

        let handler = entries[0]["hooks"][0].as_object().unwrap();
        for field in ["if", "args", "asyncRewake", "shell", "once"] {
            assert!(
                !handler.contains_key(field),
                "stale schema field {field} must not survive refresh"
            );
        }
        assert!(handler["command"]
            .as_str()
            .unwrap()
            .starts_with("flowmux hooks claude stop"));
    }

    #[test]
    fn codex_native_upsert_preserves_extensions_from_later_owned_group() {
        let stop = *CODEX_EVENTS
            .iter()
            .find(|event| event.name == "Stop")
            .unwrap();
        let first = codex_hook_entry("first-flowmux", stop);
        let mut second = codex_hook_entry("second-flowmux", stop);
        second["futureGroupField"] = json!({ "preserve": true });
        let mut entries = vec![first, second];

        upsert_flowmux_hook_entry(&mut entries, codex_hook_entry("flowmux", stop));

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["futureGroupField"], json!({ "preserve": true }));
        assert!(entries[0]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .starts_with("flowmux hooks codex stop"));
    }

    #[test]
    fn codex_native_upsert_moves_owned_extensions_out_of_shared_legacy_group() {
        let stop = *CODEX_EVENTS
            .iter()
            .find(|event| event.name == "Stop")
            .unwrap();
        let user_hook = json!({
            "type": "command",
            "command": "/usr/local/bin/user-hook",
        });
        let mut owned_hook = codex_hook_entry("old-flowmux", stop)["hooks"][0].clone();
        owned_hook["futureHandlerField"] = json!({ "preserve": true });
        let mut entries = vec![json!({
            "matcher": "^legacy$",
            "hooks": [owned_hook, user_hook.clone()],
            "futureGroupField": "keep shared group",
        })];

        upsert_flowmux_hook_entry(&mut entries, codex_hook_entry("flowmux", stop));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["hooks"], json!([user_hook]));
        assert_eq!(entries[0]["futureGroupField"], "keep shared group");
        assert_eq!(entries[1]["matcher"], stop.matcher);
        assert_eq!(
            entries[1]["hooks"][0]["futureHandlerField"],
            json!({ "preserve": true })
        );
        assert!(entries[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .starts_with("flowmux hooks codex stop"));
    }

    #[test]
    fn codex_native_check_distinguishes_missing_drift_and_installed() {
        assert_eq!(
            codex_check_status(&json!({}), false),
            HookCheckStatus::Missing
        );

        let mut root = json!({});
        upsert_codex_hooks(&mut root, "flowmux").unwrap();
        assert_eq!(codex_check_status(&root, false), HookCheckStatus::Installed);

        let mut stale_context = root.clone();
        stale_context["hooks"]["Stop"][0]["hooks"][0]["command"] =
            json!("flowmux hooks codex stop # flowmux-hook");
        assert_eq!(
            codex_check_status(&stale_context, false),
            HookCheckStatus::Drift
        );
        let mut asynchronous = root.clone();
        asynchronous["hooks"]["Stop"][0]["hooks"][0]["async"] = json!(true);
        assert_eq!(
            codex_check_status(&asynchronous, false),
            HookCheckStatus::Drift
        );

        root["hooks"].as_object_mut().unwrap().remove("SessionEnd");
        assert_eq!(codex_check_status(&root, false), HookCheckStatus::Drift);
        assert_eq!(codex_check_status(&json!({}), true), HookCheckStatus::Drift);

        let legacy = json!({
            "Stop": [{
                "hooks": [{
                    "type": "command",
                    "command": "flowmux hooks codex stop # flowmux-hook",
                }]
            }]
        });
        assert_eq!(codex_check_status(&legacy, false), HookCheckStatus::Drift);
    }

    #[test]
    fn codex_native_check_rejects_missing_direct_absolute_executable() {
        let dir = tmp();
        let missing = dir.path().join("deleted flowmux's");
        let mut root = json!({});
        upsert_codex_hooks(&mut root, missing.to_str().unwrap()).unwrap();

        assert_eq!(codex_check_status(&root, false), HookCheckStatus::Drift);
    }

    #[test]
    fn codex_native_check_accepts_existing_shell_quoted_executable() {
        let dir = tmp();
        let executable = dir.path().join("flowmux build's");
        fs::write(&executable, "fixture").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut root = json!({});
        upsert_codex_hooks(&mut root, executable.to_str().unwrap()).unwrap();

        let command = root["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(
            command.starts_with('\''),
            "fixture path should be shell quoted"
        );
        assert_eq!(codex_check_status(&root, false), HookCheckStatus::Installed);

        fs::remove_file(&executable).unwrap();
        assert_eq!(codex_check_status(&root, false), HookCheckStatus::Drift);
    }

    #[test]
    fn codex_native_check_rejects_cwd_relative_executable_path() {
        let mut root = json!({});
        upsert_codex_hooks(&mut root, "./target/debug/flowmux").unwrap();

        assert_eq!(codex_check_status(&root, false), HookCheckStatus::Drift);
    }

    #[test]
    fn codex_native_check_rejects_arbitrary_multiword_or_malformed_prefix() {
        let stop = *CODEX_EVENTS
            .iter()
            .find(|event| event.name == "Stop")
            .unwrap();
        for prefix in ["definitely-missing --bad", "'unterminated"] {
            let mut root = json!({});
            upsert_codex_hooks(&mut root, "flowmux").unwrap();
            let command = root["hooks"]["Stop"][0]["hooks"][0]["command"]
                .as_str()
                .unwrap();
            let (_, suffix) = command
                .rsplit_once(&format!(" hooks codex {}", stop.subcommand))
                .unwrap();
            root["hooks"]["Stop"][0]["hooks"][0]["command"] =
                json!(format!("{prefix} hooks codex {}{suffix}", stop.subcommand));

            assert_eq!(codex_check_status(&root, false), HookCheckStatus::Drift);
        }
    }

    #[test]
    fn codex_native_check_accepts_canonical_flatpak_prefix() {
        let stop = *CODEX_EVENTS
            .iter()
            .find(|event| event.name == "Stop")
            .unwrap();
        let mut root = json!({});
        upsert_codex_hooks(&mut root, "flowmux").unwrap();
        let command = root["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let (_, suffix) = command
            .rsplit_once(&format!(" hooks codex {}", stop.subcommand))
            .unwrap();
        root["hooks"]["Stop"][0]["hooks"][0]["command"] = json!(format!(
            "flatpak run --command=flowmuxctl com.flowmux.App hooks codex {}{suffix}",
            stop.subcommand
        ));

        assert_eq!(codex_check_status(&root, false), HookCheckStatus::Installed);
    }

    #[cfg(unix)]
    #[test]
    fn codex_native_check_rejects_non_executable_absolute_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmp();
        let executable = dir.path().join("flowmux");
        fs::write(&executable, "fixture").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o600)).unwrap();
        let mut root = json!({});
        upsert_codex_hooks(&mut root, executable.to_str().unwrap()).unwrap();

        assert_eq!(codex_check_status(&root, false), HookCheckStatus::Drift);
    }

    #[test]
    fn codex_native_migration_removes_only_owned_notify() {
        let dir = tmp();
        let owned = dir.path().join("owned.toml");
        fs::write(
            &owned,
            r#"model = "gpt-x"
notify = ["flowmux", "hooks", "codex", "stop"]

[features]
hooks = true
codex_hooks = true
"#,
        )
        .unwrap();
        assert!(remove_owned_codex_notify(&owned).unwrap());
        let migrated = fs::read_to_string(&owned).unwrap();
        assert!(!migrated.contains("notify ="));
        assert!(migrated.contains("model = \"gpt-x\""));
        assert!(migrated.contains("hooks = true"));
        assert!(migrated.contains("codex_hooks = true"));

        let user = dir.path().join("user.toml");
        let original = r#"model = "gpt-x"
notify = ["/usr/local/bin/user-notifier", "--keep"]
"#;
        fs::write(&user, original).unwrap();

        assert!(!remove_owned_codex_notify(&user).unwrap());
        assert_eq!(fs::read_to_string(&user).unwrap(), original);

        let wrapped = dir.path().join("wrapped.toml");
        fs::write(
            &wrapped,
            r#"notify = ["/usr/local/bin/user-wrapper", "turn-ended", "--previous-notify", "[\"/Applications/FlowMux.app/Contents/MacOS/flowmuxctl\",\"hooks\",\"codex\",\"stop\"]", "--keep"]
"#,
        )
        .unwrap();
        assert!(codex_config_has_owned_notify(&wrapped).unwrap());
        assert!(remove_owned_codex_notify(&wrapped).unwrap());
        let migrated = fs::read_to_string(&wrapped).unwrap();
        assert!(migrated.contains("user-wrapper"));
        assert!(migrated.contains("turn-ended"));
        assert!(migrated.contains("--keep"));
        assert!(!migrated.contains("previous-notify"));
        assert!(!migrated.contains("flowmuxctl"));
    }

    #[test]
    fn codex_notify_ownership_requires_flowmux_executable_and_hook_suffix() {
        use toml_edit::DocumentMut;

        let custom: DocumentMut =
            r#"notify = ["/usr/local/bin/flowmux-notifier"]"#.parse().unwrap();
        let owned: DocumentMut =
            r#"notify = ["flowmux", "hooks", "codex", "stop"]"#.parse().unwrap();
        let user_wrapper: DocumentMut =
            r#"notify = ["/usr/local/bin/user-wrapper", "hooks", "codex", "stop"]"#
                .parse()
                .unwrap();
        let flatpak: DocumentMut = r#"notify = ["flatpak", "run", "--command=flowmuxctl", "com.flowmux.App", "hooks", "codex", "stop"]"#
            .parse()
            .unwrap();

        assert!(!codex_notify_is_flowmux_owned(&custom["notify"]));
        assert!(!codex_notify_is_flowmux_owned(&user_wrapper["notify"]));
        assert!(codex_notify_is_flowmux_owned(&owned["notify"]));
        assert!(codex_notify_is_flowmux_owned(&flatpak["notify"]));
    }

    #[test]
    fn codex_shape_validation_rejects_malformed_containers() {
        assert!(validate_codex_hooks_shape(&json!([])).is_err());
        assert!(validate_codex_hooks_shape(&json!({ "hooks": [] })).is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": {} }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [42] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [{ "matcher": 42 }] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [{ "hooks": {} }] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [{ "hooks": [42] }] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [{ "hooks": [{ "type": "command" }] }] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [{
                "hooks": [{ "type": "command", "command": "ok", "timeout": "soon" }]
            }] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [{
                "hooks": [{ "type": "mcp_tool", "server": "example" }]
            }] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [{
                "hooks": [{
                    "type": "mcp_tool",
                    "server": "example",
                    "tool": "check",
                    "input": null
                }]
            }] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({
            "hooks": { "Stop": [{ "hooks": [{ "type": "future" }] }] }
        }))
        .is_err());
        assert!(validate_codex_hooks_shape(&json!({ "future": {} })).is_err());
    }

    #[test]
    fn codex_shape_validation_accepts_and_preserves_valid_user_handlers() {
        let user_handlers = json!([
            {
                "type": "command",
                "command": "/usr/local/bin/user-hook",
                "commandWindows": "C:\\user-hook.exe",
                "timeout": 9,
                "async": false,
                "statusMessage": "Checking",
                "additionalContextLimit": 2500,
                "futureField": { "preserve": true }
            },
            {
                "type": "mcp_tool",
                "server": "example",
                "tool": "check",
                "input": { "nested": [1, 2] }
            },
            { "type": "prompt", "futureField": "preserve" },
            { "type": "agent" }
        ]);
        let mut root = json!({
            "description": "keep me",
            "hooks": {
                "Stop": [{
                    "matcher": null,
                    "hooks": user_handlers.clone(),
                    "futureGroupField": true
                }],
                "FutureEvent": []
            }
        });

        validate_codex_hooks_shape(&root).unwrap();
        upsert_codex_hooks(&mut root, "flowmux").unwrap();

        assert_eq!(root["description"], "keep me");
        assert_eq!(root["hooks"]["Stop"][0]["hooks"], user_handlers);
        assert_eq!(root["hooks"]["Stop"][0]["futureGroupField"], true);
        assert_eq!(root["hooks"]["FutureEvent"], json!([]));
    }

    #[test]
    fn codex_install_rejects_invalid_hooks_without_rewriting() {
        let dir = tmp();
        let hooks = dir.path().join("hooks.json");
        for original in [
            br#"{"hooks":{"Stop":[42]}}"#.as_slice(),
            br#"{"hooks":{"Stop":[{"hooks":[{"type":"future"}]}]}}"#.as_slice(),
            br#"{"unknown_root":{"preserve":true}}"#.as_slice(),
        ] {
            fs::write(&hooks, original).unwrap();

            assert!(install_codex_in(dir.path(), "flowmux").is_err());
            assert_eq!(fs::read(&hooks).unwrap(), original);
        }
    }

    #[test]
    fn codex_disabled_feature_flags_are_detected_without_rewriting() {
        let dir = tmp();
        for (name, config) in [
            ("canonical.toml", "[features]\nhooks = false\n"),
            ("deprecated.toml", "[features]\ncodex_hooks = false\n"),
            ("managed-only.toml", "allow_managed_hooks_only = true\n"),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, config).unwrap();
            assert!(codex_config_hooks_disabled(&path).unwrap());
        }
        let enabled = dir.path().join("enabled.toml");
        fs::write(
            &enabled,
            "allow_managed_hooks_only = false\n[features]\nhooks = true\n",
        )
        .unwrap();
        assert!(!codex_config_hooks_disabled(&enabled).unwrap());
    }

    #[test]
    fn codex_install_preflights_config_before_writing_hooks() {
        let dir = tmp();
        let hooks = dir.path().join("hooks.json");
        let original = b"{\n  \"description\": \"keep\"\n}\n";
        fs::write(&hooks, original).unwrap();
        fs::write(dir.path().join("config.toml"), "notify = [").unwrap();

        assert!(install_codex_in(dir.path(), "flowmux").is_err());
        assert_eq!(fs::read(&hooks).unwrap(), original);

        fs::write(
            dir.path().join("config.toml"),
            "allow_managed_hooks_only = true\n",
        )
        .unwrap();
        assert!(install_codex_in(dir.path(), "flowmux").is_err());
        assert_eq!(fs::read(&hooks).unwrap(), original);
    }

    #[test]
    fn gemini_hook_entries_follow_the_official_schema_and_event_mapping() {
        let expected = [
            ("SessionStart", "session-start"),
            ("BeforeAgent", "running"),
            ("AfterAgent", "stop"),
            ("SessionEnd", "session-end"),
            ("Notification", "notification"),
        ];

        assert_eq!(GEMINI_EVENTS.len(), expected.len());
        for (event, (name, subcommand)) in GEMINI_EVENTS.iter().zip(expected) {
            assert_eq!((event.name, event.subcommand), (name, subcommand));
            assert_eq!(
                gemini_hook_entry("flowmux", *event),
                json!({
                    "matcher": "",
                    "hooks": [{
                        "type": "command",
                        "command": format!(
                            "flowmux hooks gemini {subcommand} ${{FLOWMUX_PANE_ID:+--pane=$FLOWMUX_PANE_ID}} ${{FLOWMUX_SURFACE_ID:+--surface=$FLOWMUX_SURFACE_ID}}  # {FLOWMUX_HOOK_MARKER}"
                        ),
                        "timeout": 5000,
                    }]
                })
            );
        }
    }

    #[test]
    fn gemini_upsert_is_idempotent_and_uninstall_preserves_user_settings() {
        let user_hook = json!({
            "type": "command",
            "command": "/usr/local/bin/user-hook",
            "timeout": 3000,
        });
        let grouped_hooks = json!({
            "matcher": "",
            "hooks": [user_hook.clone(), {
                "type": "command",
                "command": format!("old-flowmux hooks gemini stop # {FLOWMUX_HOOK_MARKER}"),
                "timeout": 5000,
            }]
        });
        let mut root = json!({
            "theme": "user-theme",
            "hooks": { "AfterAgent": [grouped_hooks.clone()] }
        });

        upsert_gemini_hooks(&mut root, "flowmux").unwrap();
        upsert_gemini_hooks(&mut root, "flowmux").unwrap();

        assert_eq!(root["theme"], "user-theme");
        for event in GEMINI_EVENTS {
            let entries = root["hooks"][event.name].as_array().unwrap();
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| claude_entry_is_flowmux_owned(entry))
                    .count(),
                1,
                "duplicate or missing flowmux entry for {}: {entries:?}",
                event.name
            );
            assert!(entries
                .iter()
                .any(|entry| gemini_entry_matches(entry, *event)));
        }
        assert_eq!(root["hooks"]["AfterAgent"][0]["hooks"], json!([user_hook]));

        prune_gemini_hooks(&mut root);
        assert_eq!(
            root["hooks"]["AfterAgent"],
            json!([{
                "matcher": "",
                "hooks": [user_hook],
            }])
        );
        for event in GEMINI_EVENTS
            .iter()
            .filter(|event| event.name != "AfterAgent")
        {
            assert_eq!(root["hooks"][event.name], json!([]));
        }
    }

    #[test]
    fn antigravity_hooks_follow_native_schema_and_emit_json() {
        let group = antigravity_hook_group("flowmux");

        assert_eq!(antigravity_plugin_manifest(), json!({ "name": "flowmux" }));
        assert!(antigravity_group_matches(&group));
        for (event, subcommand) in [("PreInvocation", "running"), ("Stop", "stop")] {
            let hook = &group[event][0];
            assert!(antigravity_command_matches(hook, subcommand));
            assert_eq!(hook["timeout"], ANTIGRAVITY_HOOK_TIMEOUT_SECS);
        }
        let post_tool_use = &group["PostToolUse"][0];
        assert_eq!(post_tool_use["matcher"], "*");
        assert!(antigravity_command_matches(
            &post_tool_use["hooks"][0],
            "running"
        ));
        let pre_invocation = group["PreInvocation"][0]["command"].as_str().unwrap();
        let post_tool_use = post_tool_use["hooks"][0]["command"].as_str().unwrap();
        let stop = group["Stop"][0]["command"].as_str().unwrap();
        assert!(pre_invocation.contains("printf '%s' '{}'"));
        assert!(post_tool_use.contains("printf '%s' '{}'"));
        assert!(stop.contains(r#"printf '%s' '{"decision":""}'"#));
    }

    #[test]
    fn antigravity_home_accepts_cli_state_root_or_binary() {
        let dir = tmp();
        let plugin_dir = dir.path().join(".gemini/config/plugins/flowmux");
        fs::create_dir_all(plugin_dir.parent().unwrap()).unwrap();
        assert!(!antigravity_home_exists(&plugin_dir));

        fs::create_dir_all(dir.path().join(".gemini/antigravity-cli")).unwrap();
        assert!(antigravity_home_exists(&plugin_dir));

        let binary_dir = tmp();
        let plugin_dir = binary_dir.path().join(".gemini/config/plugins/flowmux");
        let binary = binary_dir.path().join(".local/bin/agy");
        fs::create_dir_all(binary.parent().unwrap()).unwrap();
        fs::write(binary, "").unwrap();
        assert!(antigravity_home_exists(&plugin_dir));
    }

    #[test]
    fn gemini_presence_ignores_antigravity_shared_directory() {
        let dir = tmp();
        let settings = dir.path().join(".gemini/settings.json");
        fs::create_dir_all(settings.parent().unwrap()).unwrap();

        assert!(!gemini_is_installed(&settings, false));
        assert!(gemini_is_installed(&settings, true));
        fs::write(&settings, "{}").unwrap();
        assert!(gemini_is_installed(&settings, false));
    }

    #[test]
    fn antigravity_plugin_install_and_uninstall_leave_neighboring_config_untouched() {
        let dir = tmp();
        let gemini_home = dir.path().join(".gemini");
        let plugin_dir = gemini_home.join("config/plugins/flowmux");
        let neighbor = gemini_home.join("config/plugins/orca-status/plugin.json");
        let user_hooks = gemini_home.join("config/hooks.json");
        write_json(&neighbor, &json!({ "name": "orca-status" })).unwrap();
        write_json(
            &user_hooks,
            &json!({ "orca-status": { "PreInvocation": [] } }),
        )
        .unwrap();
        let neighbor_before = fs::read(&neighbor).unwrap();
        let user_hooks_before = fs::read(&user_hooks).unwrap();
        let [manifest_path, hooks_path] = antigravity_plugin_paths(&plugin_dir);

        assert_eq!(
            check_antigravity_in(&plugin_dir).status,
            HookCheckStatus::Missing
        );
        let first = install_antigravity_in(&plugin_dir, "flowmux").unwrap();
        assert_eq!(
            first.touched_paths,
            vec![manifest_path.clone(), hooks_path.clone()]
        );
        assert_eq!(
            check_antigravity_in(&plugin_dir).status,
            HookCheckStatus::Installed
        );
        let second = install_antigravity_in(&plugin_dir, "flowmux").unwrap();
        assert!(second.touched_paths.is_empty());

        assert_eq!(
            read_json_or_empty_object(&manifest_path).unwrap(),
            antigravity_plugin_manifest()
        );
        let installed = read_json_or_empty_object(&hooks_path).unwrap();
        assert!(antigravity_group_matches(
            &installed[ANTIGRAVITY_HOOK_GROUP]
        ));
        assert_eq!(fs::read(&neighbor).unwrap(), neighbor_before);
        assert_eq!(fs::read(&user_hooks).unwrap(), user_hooks_before);

        fs::write(plugin_dir.join("README.user"), "keep").unwrap();
        let report = uninstall_antigravity_in(&plugin_dir).unwrap();
        assert_eq!(report.touched_paths, vec![hooks_path, manifest_path]);
        assert!(plugin_dir.join("README.user").exists());
        assert_eq!(fs::read(&neighbor).unwrap(), neighbor_before);
        assert_eq!(fs::read(&user_hooks).unwrap(), user_hooks_before);
        assert!(uninstall_antigravity_in(&plugin_dir)
            .unwrap()
            .touched_paths
            .is_empty());
        fs::remove_file(plugin_dir.join("README.user")).unwrap();
        uninstall_antigravity_in(&plugin_dir).unwrap();
        assert!(!plugin_dir.exists());
    }

    #[test]
    fn antigravity_plugin_does_not_overwrite_or_remove_unowned_files() {
        let dir = tmp();
        let plugin_dir = dir.path().join("flowmux");
        let [manifest_path, hooks_path] = antigravity_plugin_paths(&plugin_dir);
        // A matching name alone is not an ownership marker. Never replace a
        // same-name plugin unless its hooks contain flowmux's command marker.
        write_json(&manifest_path, &antigravity_plugin_manifest()).unwrap();
        write_json(&hooks_path, &json!({ "user-hook": {} })).unwrap();
        let manifest_before = fs::read(&manifest_path).unwrap();
        let hooks_before = fs::read(&hooks_path).unwrap();

        assert!(install_antigravity_in(&plugin_dir, "flowmux").is_err());
        assert!(uninstall_antigravity_in(&plugin_dir)
            .unwrap()
            .touched_paths
            .is_empty());
        assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
        assert_eq!(fs::read(&hooks_path).unwrap(), hooks_before);

        let manifest_only_dir = dir.path().join("manifest-only");
        let [manifest_path, _] = antigravity_plugin_paths(&manifest_only_dir);
        write_json(&manifest_path, &antigravity_plugin_manifest()).unwrap();
        assert!(matches!(
            check_antigravity_in(&manifest_only_dir).status,
            HookCheckStatus::Error(_)
        ));
        assert!(install_antigravity_in(&manifest_only_dir, "flowmux").is_err());
        assert!(uninstall_antigravity_in(&manifest_only_dir)
            .unwrap()
            .touched_paths
            .is_empty());
        assert!(manifest_path.exists());
    }

    #[test]
    fn antigravity_plugin_respects_an_explicit_disable() {
        let dir = tmp();
        let plugin_dir = dir.path().join(".gemini/config/plugins/flowmux");
        let config = dir.path().join(".gemini/config/config.json");
        write_json(
            &config,
            &json!({ "plugins": { "flowmux": { "enabled": false } } }),
        )
        .unwrap();

        assert!(matches!(
            check_antigravity_in(&plugin_dir).status,
            HookCheckStatus::Error(message) if message.contains("explicitly disabled")
        ));
        assert!(install_antigravity_in(&plugin_dir, "flowmux").is_err());

        write_json(
            &config,
            &json!({ "plugins": { "flowmux": { "enabled": true } } }),
        )
        .unwrap();
        install_antigravity_in(&plugin_dir, "flowmux").unwrap();
        write_json(
            &config,
            &json!({ "plugins": { "flowmux": { "enabled": false } } }),
        )
        .unwrap();
        assert!(matches!(
            check_antigravity_in(&plugin_dir).status,
            HookCheckStatus::Error(message) if message.contains("explicitly disabled")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn antigravity_uninstall_preserves_plugin_directory_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tmp();
        let target = dir.path().join("plugin-target");
        let plugin_dir = dir.path().join("flowmux");
        fs::create_dir(&target).unwrap();
        symlink(&target, &plugin_dir).unwrap();
        install_antigravity_in(&plugin_dir, "flowmux").unwrap();

        uninstall_antigravity_in(&plugin_dir).unwrap();

        assert!(plugin_dir.is_symlink());
        assert!(fs::read_dir(target).unwrap().next().is_none());
    }

    #[test]
    fn antigravity_check_reports_drift_for_stale_owned_group() {
        let dir = tmp();
        let plugin_dir = dir.path().join("flowmux");
        let [manifest_path, hooks_path] = antigravity_plugin_paths(&plugin_dir);
        write_json(&manifest_path, &antigravity_plugin_manifest()).unwrap();
        write_json(
            &hooks_path,
            &json!({
                (ANTIGRAVITY_HOOK_GROUP): {
                    "Stop": [{
                        "type": "command",
                        "command": format!("flowmux hooks antigravity stop # {FLOWMUX_HOOK_MARKER}"),
                        "timeout": 10,
                    }]
                }
            }),
        )
        .unwrap();

        assert_eq!(
            check_antigravity_in(&plugin_dir).status,
            HookCheckStatus::Drift
        );
    }

    #[test]
    fn hook_settings_accept_jsonc_comments() {
        let dir = tmp();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            r#"{
                // Gemini CLI accepts JSONC settings.
                "theme": "literal // text",
                /* Preserve all semantic settings. */
                "hooks": {}
            }"#,
        )
        .unwrap();

        let root = read_json_or_empty_object(&path).unwrap();
        assert_eq!(root["theme"], "literal // text");
        assert_eq!(root["hooks"], json!({}));
    }

    #[test]
    fn opencode_unregisters_only_flowmux_config_entries() {
        let dir = tmp();
        let path = dir.path().join("opencode.json");
        let initial = json!({
            "plugin": [
                "file://./plugins/flowmux-session.mjs",
                "@user/unrelated",
            ],
            "theme": "user-theme"
        });
        write_json(&path, &initial).unwrap();
        assert!(unregister_opencode_plugin(&path, "flowmux-session").unwrap());
        assert!(!unregister_opencode_plugin(&path, "flowmux-session").unwrap());
        let v: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        let plugins = v["plugin"].as_array().unwrap();
        assert_eq!(plugins, &[json!("@user/unrelated")]);
        assert_eq!(v["theme"], "user-theme");
    }

    #[test]
    fn opencode_plugin_source_carries_marker_and_bin_path() {
        let src = opencode_plugin_source("/usr/local/bin/flowmux");
        assert!(src.contains(FLOWMUX_OPENCODE_PLUGIN_MARKER));
        assert!(src.contains("/usr/local/bin/flowmux"));
        assert!(src.contains("session.created"));
        assert!(src.contains("session.status"));
        assert!(src.contains("status.type === \"idle\""));
        assert!(src.contains("fireFlowmuxHook(\"stop\", payload)"));
        assert!(!src.contains("t === \"session.updated\""));
        assert!(!src.contains("t === \"session.idle\""));
        assert!(src.contains("sessionPayload"));
        assert!(src.contains("session_id"));
        // Must be an ESM module so OpenCode 1.14+ loads it.
        assert!(src.contains("import"));
        assert!(src.contains("export const server"));
    }

    #[test]
    fn opencode_plugin_executes_lifecycle_events_in_order() {
        let dir = tmp();
        let plugin = dir.path().join("flowmux-session.mjs");
        fs::write(&plugin, opencode_plugin_source("flowmux")).unwrap();
        let output = match std::process::Command::new("node")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/opencode_plugin_events.mjs"
            ))
            .arg(&plugin)
            .output()
        {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skipping OpenCode plugin execution: node is not installed");
                return;
            }
            Err(error) => panic!("run OpenCode plugin test: {error}"),
        };
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn opencode_install_migrates_to_discoverable_esm_without_losing_user_hooks() {
        let home = tmp();
        let plugins = home.path().join("plugins");
        fs::create_dir_all(&plugins).unwrap();
        let legacy = plugins.join("flowmux-session.mjs");
        fs::write(&legacy, "// flowmux-opencode-session-plugin v4").unwrap();
        let config = home.path().join("opencode.json");
        write_json(&config, &json!({"plugin": ["file://./plugins/flowmux-session.mjs", "user-plugin"], "theme": "user-theme"})).unwrap();
        let source = opencode_plugin_source("flowmux");
        install_opencode_in(home.path(), &source).unwrap();
        assert!(!legacy.exists());
        assert_eq!(
            fs::read_to_string(plugins.join("flowmux-session.js")).unwrap(),
            source
        );
        assert_eq!(
            read_json_or_empty_object(&config).unwrap(),
            json!({"plugin": ["user-plugin"], "theme": "user-theme"})
        );
        assert!(install_opencode_in(home.path(), &source)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn opencode_install_preserves_unowned_js_and_mjs_files() {
        for name in ["flowmux-session.js", "flowmux-session.mjs"] {
            let home = tmp();
            let plugins = home.path().join("plugins");
            fs::create_dir_all(&plugins).unwrap();
            let path = plugins.join(name);
            fs::write(&path, "// user-owned plugin").unwrap();
            assert!(install_opencode_in(home.path(), &opencode_plugin_source("flowmux")).is_err());
            assert_eq!(fs::read_to_string(&path).unwrap(), "// user-owned plugin");
        }
    }

    #[test]
    fn opencode_plugin_source_passes_pane_and_surface_as_cli_args() {
        // `flatpak run` resets env to a minimal sandbox set, so the
        // in-sandbox flowmuxctl could not recover FLOWMUX_PANE_ID /
        // FLOWMUX_SURFACE_ID from inherited env. Solution: push the
        // values onto the CLI argv as explicit `--pane` / `--surface`
        // flags — argv survives the sandbox boundary intact. Pin the
        // JS-side argv shape so a future refactor cannot quietly
        // regress the sidebar / click-navigation path.
        let src = opencode_plugin_source_with_argv(&[
            "flatpak".to_string(),
            "run".to_string(),
            "--command=flowmuxctl".to_string(),
            "com.flowmux.App".to_string(),
        ]);
        // CLI argv path
        assert!(src.contains("FLOWMUX_PANE_ID"));
        assert!(src.contains("FLOWMUX_SURFACE_ID"));
        assert!(src.contains("\"--pane\""));
        assert!(src.contains("\"--surface\""));
        // The legacy `--env=` forwarding is removed; the argv path is
        // the single source of truth across the flatpak boundary.
        assert!(!src.contains("--env="));
    }

    #[test]
    fn opencode_plugin_source_omits_pane_flag_outside_flatpak() {
        // Outside Flatpak the spawn inherits env directly, so the
        // legacy env-var path still resolves pane/surface. The plugin
        // can stay symmetrical (always push `--pane` when the env var
        // is set) — but it must NOT push the flag with an empty string,
        // which would make clap fail to parse Option<PaneId>. This
        // pins the `if (pane) args.push(...)` guard.
        let src = opencode_plugin_source("/usr/local/bin/flowmux");
        // The guard is what keeps empty-value pushes out.
        assert!(src.contains("if (pane) args.push(\"--pane\""));
        assert!(src.contains("if (surface) args.push(\"--surface\""));
    }

    #[test]
    fn opencode_plugin_source_routes_permission_events_to_notification() {
        let src = opencode_plugin_source("flowmux");
        assert!(src.contains("permission.asked"));
        assert!(!src.contains("permission.updated"));
        assert!(src.contains("permission.replied"));
        assert!(src.contains("session.status"));
        assert!(src.contains("status.type === \"busy\""));
        assert!(src.contains("status.type === \"retry\""));
        assert!(src.contains("fireFlowmuxHook(\"running\", payload)"));
        // Errors and permission requests both go through the
        // `notification` subcommand with a JSON payload so the toast
        // body is informative.
        assert!(src.contains("OpenCode needs your input"));
        assert!(src.contains("OpenCode session error"));
    }

    #[test]
    fn opencode_homes_includes_anycli_tree_when_present() {
        // The opencode-anycli wrapper sets
        // XDG_CONFIG_HOME=~/.config/opencode-anycli, so its plugin
        // loader only sees ~/.config/opencode-anycli/opencode/plugins/.
        // The Flatpak build must still install there: the wrapper
        // always runs on the host, and the tree is bind-mounted into
        // the sandbox via --filesystem=home. Before this assertion the
        // sandbox branch dropped the anycli root entirely and OpenCode
        // never saw the flowmux plugin.
        let dir = tmp();
        let host_home = dir.path().to_path_buf();
        let anycli_tree = host_home
            .join(".config")
            .join("opencode-anycli")
            .join("opencode");
        fs::create_dir_all(&anycli_tree).unwrap();
        let primary = host_home.join(".config").join("opencode");
        let homes = opencode_homes_for(Some(primary.clone()), Some(host_home));
        assert_eq!(homes, vec![primary, anycli_tree]);
    }

    #[test]
    fn opencode_homes_skips_anycli_tree_when_absent() {
        // Machines without opencode-anycli should not have the
        // wrapper's plugin tree fabricated on disk — only the primary
        // root is returned.
        let dir = tmp();
        let host_home = dir.path().to_path_buf();
        let primary = host_home.join(".config").join("opencode");
        let homes = opencode_homes_for(Some(primary.clone()), Some(host_home));
        assert_eq!(homes, vec![primary]);
    }

    #[test]
    fn write_atomic_replaces_target_via_rename() {
        let dir = tmp();
        let path = dir.path().join("a/b/c.txt");
        assert!(write_atomic(&path, b"first").unwrap());
        assert!(write_atomic(&path, b"second").unwrap());
        assert!(!write_atomic(&path, b"second").unwrap());
        assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_uses_private_mode_and_preserves_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmp();
        let path = dir.path().join("config.toml");
        assert!(write_atomic(&path, b"first").unwrap());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
        assert!(write_atomic(&path, b"second").unwrap());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_preserves_relative_final_symlink_and_target_mode() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let dir = tmp();
        let config_dir = dir.path().join(".codex");
        let dotfiles_dir = dir.path().join("dotfiles");
        fs::create_dir_all(&config_dir).unwrap();
        fs::create_dir_all(&dotfiles_dir).unwrap();
        let target = dotfiles_dir.join("config.toml");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = config_dir.join("config.toml");
        symlink("../dotfiles/config.toml", &link).unwrap();

        assert!(write_atomic(&link, b"new").unwrap());
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_atomic_rejects_dangling_or_looping_final_symlinks() {
        use std::os::unix::fs::symlink;

        let dir = tmp();
        let dangling = dir.path().join("dangling.json");
        symlink("missing.json", &dangling).unwrap();
        assert!(write_atomic(&dangling, b"new").is_err());
        assert!(fs::symlink_metadata(&dangling)
            .unwrap()
            .file_type()
            .is_symlink());

        let first = dir.path().join("first.json");
        let second = dir.path().join("second.json");
        symlink("second.json", &first).unwrap();
        symlink("first.json", &second).unwrap();
        assert!(write_atomic(&first, b"new").is_err());
        assert!(fs::symlink_metadata(&first)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(fs::symlink_metadata(&second)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn read_json_or_empty_object_handles_missing_and_empty_files() {
        let dir = tmp();
        let missing = dir.path().join("missing.json");
        assert_eq!(read_json_or_empty_object(&missing).unwrap(), json!({}));
        let empty = dir.path().join("empty.json");
        fs::write(&empty, "").unwrap();
        assert_eq!(read_json_or_empty_object(&empty).unwrap(), json!({}));
        let blank = dir.path().join("blank.json");
        fs::write(&blank, "   \n  \t").unwrap();
        assert_eq!(read_json_or_empty_object(&blank).unwrap(), json!({}));
    }

    #[test]
    fn read_json_or_empty_object_errors_on_invalid_json() {
        let dir = tmp();
        let bad = dir.path().join("bad.json");
        fs::write(&bad, "{ not valid").unwrap();
        assert!(read_json_or_empty_object(&bad).is_err());
    }
}
