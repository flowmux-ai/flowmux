// SPDX-License-Identifier: GPL-3.0-or-later
//! Executes parsed tmux-compat invocations against the [`StateStore`].
//!
//! This is the daemon half of the Claude Code agent-teams integration:
//! `flowmuxctl tmux-compat` forwards the argv of a `tmux` shim call as
//! [`Request::TmuxCompat`], `flowmux_ipc::tmux_compat::parse` turns it
//! into a [`TmuxCommand`], and [`execute`] maps it onto workspaces and
//! panes. Widget side effects go through [`TmuxCompatUi`], so the same
//! orchestration runs embedded in the GUI (bridge → GTK) and in the
//! headless daemon binary (log-only), where it doubles as the test
//! harness.
//!
//! Mapping: one tmux *session* (keyed by server socket name, which
//! Claude Code derives from its pid) is one workspace; every tmux
//! *pane* is a flowmux pane, identified by its UUID — Claude Code
//! treats pane ids as opaque tokens, so no `%N` translation is kept.

use crate::state_store::StateStore;
use flowmux_core::{Pane, PaneContent, PaneId, SplitDirection, SurfaceId, Workspace, WorkspaceId};
use flowmux_ipc::tmux_compat::{
    expand_format, parse, session_workspace_name, Target, TmuxCommand, TmuxCompatOutput,
    SHIM_VERSION_LINE,
};
use std::path::Path;

/// Widget-side effects of tmux-compat execution. [`execute`] owns every
/// [`StateStore`] mutation except pane close / workspace removal (whose
/// GUI paths mutate the store themselves); implementations apply the
/// matching UI change — the GUI sends bridge commands, the headless
/// daemon logs the verb traffic.
// Local trait consumed only by the GUI crate and this crate's tests;
// the Send-bound subtleties the lint warns about do not apply.
#[allow(async_fn_in_trait)]
pub trait TmuxCompatUi {
    /// A workspace was created in the store; materialize its window.
    async fn workspace_created(&self, id: WorkspaceId, name: &str, root: &Path);
    /// `split_pane` succeeded in the store; materialize the new pane.
    async fn pane_split_applied(
        &self,
        workspace: WorkspaceId,
        pane: PaneId,
        new_pane: PaneId,
        direction: SplitDirection,
    ) -> Result<(), String>;
    /// Type `keys` into the pane's PTY (runs the teammate command).
    async fn send_keys(&self, pane: PaneId, keys: &str) -> Result<(), String>;
    /// Rename the tab `surface` inside `pane` (store + widget).
    async fn rename_surface(
        &self,
        pane: PaneId,
        surface: SurfaceId,
        title: &str,
    ) -> Result<(), String>;
    /// Close a pane that is *not* the workspace's last (store + widget).
    async fn close_pane(&self, pane: PaneId) -> Result<(), String>;
    /// Remove a whole workspace (store + widget) — kill-pane on the
    /// last pane and kill-server land here, mirroring tmux where
    /// killing the last pane kills the session.
    async fn remove_workspace(&self, id: WorkspaceId);
    /// Focus the workspace (attach).
    async fn workspace_activated(&self, id: WorkspaceId);
}

/// Log-only [`TmuxCompatUi`] for the headless daemon binary: state
/// mutations still happen in the store (via [`execute`] and the store
/// calls below), widget work is reported and skipped.
pub struct HeadlessTmuxUi<'a> {
    pub store: &'a StateStore,
}

impl TmuxCompatUi for HeadlessTmuxUi<'_> {
    async fn workspace_created(&self, id: WorkspaceId, name: &str, _root: &Path) {
        tracing::info!(%id, name, "tmux-compat: workspace created (headless, no widgets)");
    }

    async fn pane_split_applied(
        &self,
        workspace: WorkspaceId,
        pane: PaneId,
        new_pane: PaneId,
        direction: SplitDirection,
    ) -> Result<(), String> {
        tracing::info!(%workspace, %pane, %new_pane, ?direction, "tmux-compat: pane split (headless)");
        Ok(())
    }

    async fn send_keys(&self, pane: PaneId, keys: &str) -> Result<(), String> {
        tracing::info!(%pane, keys, "tmux-compat: send-keys (headless, dropped — no PTY)");
        Ok(())
    }

    async fn rename_surface(
        &self,
        pane: PaneId,
        surface: SurfaceId,
        title: &str,
    ) -> Result<(), String> {
        self.store
            .rename_surface(pane, surface, title.to_string())
            .await
            .map(|_| ())
            .ok_or_else(|| format!("no tab surface {surface} in pane {pane}"))
    }

    async fn close_pane(&self, pane: PaneId) -> Result<(), String> {
        self.store
            .close_pane(pane)
            .await
            .map(|_| ())
            .ok_or_else(|| format!("no pane {pane}"))
    }

    async fn remove_workspace(&self, id: WorkspaceId) {
        self.store.remove_workspace(id).await;
    }

    async fn workspace_activated(&self, id: WorkspaceId) {
        self.store.set_active_workspace(Some(id)).await;
    }
}

/// Run one tmux-compat invocation. Never panics on bad input — errors
/// come back as the exit-status mirror the shim reproduces.
pub async fn execute(
    store: &StateStore,
    ui: &impl TmuxCompatUi,
    args: &[String],
    cwd: &Path,
) -> TmuxCompatOutput {
    let invocation = match parse(args) {
        Ok(inv) => inv,
        Err(e) => return TmuxCompatOutput::fail(1, format!("{e}\n")),
    };
    let socket = invocation.socket_name.as_deref();

    match invocation.command {
        TmuxCommand::Version => TmuxCompatOutput::out(format!("{SHIM_VERSION_LINE}\n")),

        TmuxCommand::HasSession { target } => match resolve_session(store, socket, &target).await {
            Some(_) => TmuxCompatOutput::ok(),
            None => TmuxCompatOutput::fail(1, no_such_session(socket, &target)),
        },

        TmuxCommand::NewSession {
            name,
            window_name,
            print,
            format,
        } => {
            let key = session_workspace_name(socket, &name);
            if find_workspace(store, &key).await.is_some() {
                return TmuxCompatOutput::fail(1, format!("duplicate session: {name}\n"));
            }
            let ws_id = store
                .create_workspace(Some(key.clone()), cwd.to_path_buf())
                .await;
            // A regular flowmux workspace's automatic `name` follows
            // the focused tab/cwd. The tmux session key must remain
            // stable across that UI sync, so pin it as the displayed
            // workspace title as well.
            store.rename_workspace(ws_id, key.clone()).await;
            ui.workspace_created(ws_id, &key, cwd).await;
            let Some(first) = first_leaf(store, ws_id).await else {
                return TmuxCompatOutput::fail(
                    1,
                    "new-session: workspace has no pane\n".to_string(),
                );
            };
            print_pane(
                print,
                format.as_deref(),
                first,
                window_name.as_deref(),
                &name,
            )
        }

        TmuxCommand::ListWindows { target, format } => {
            let Some(ws) = resolve_session(store, socket, &target).await else {
                return TmuxCompatOutput::fail(1, no_such_session(socket, &target));
            };
            // flowmux's pane grid is flat: report the single window
            // Claude Code's swarm layout expects. The name converges
            // with what it will create/look up ("swarm-view").
            let line = match format.as_deref() {
                Some(f) => expand_format(f, "", "swarm-view", session_of(&target)),
                None => format!("swarm-view ({} panes)", leaf_panes(&ws).len()),
            };
            TmuxCompatOutput::out(format!("{line}\n"))
        }

        TmuxCommand::NewWindow {
            target,
            window_name,
            print,
            format,
        } => {
            let Some(ws) = resolve_session(store, socket, &target).await else {
                return TmuxCompatOutput::fail(1, no_such_session(socket, &target));
            };
            let panes = leaf_panes(&ws);
            let Some(&first) = panes.first() else {
                return TmuxCompatOutput::fail(
                    1,
                    "new-window: workspace has no pane\n".to_string(),
                );
            };
            if window_name.as_deref() == Some("swarm-view") {
                // The swarm-view "window" is the workspace itself.
                return print_pane(
                    print,
                    format.as_deref(),
                    first,
                    window_name.as_deref(),
                    session_of(&target),
                );
            }
            // Legacy teammate path: one "window" per teammate becomes
            // one more pane in the workspace grid, alternating split
            // direction like the claude-teams launcher does.
            let direction = if panes.len() % 2 == 1 {
                SplitDirection::Vertical
            } else {
                SplitDirection::Horizontal
            };
            let split_from = *panes.last().unwrap_or(&first);
            match store.split_pane(split_from, direction).await {
                Some((ws_id, new_pane)) => {
                    match ui
                        .pane_split_applied(ws_id, split_from, new_pane, direction)
                        .await
                    {
                        Ok(()) => print_pane(
                            print,
                            format.as_deref(),
                            new_pane,
                            window_name.as_deref(),
                            session_of(&target),
                        ),
                        Err(error) => {
                            let _ = store.close_pane(new_pane).await;
                            TmuxCompatOutput::fail(1, format!("{error}\n"))
                        }
                    }
                }
                None => TmuxCompatOutput::fail(1, "new-window: split failed\n".to_string()),
            }
        }

        TmuxCommand::ListPanes { target, format } => {
            let Some(ws) = resolve_session(store, socket, &target).await else {
                return TmuxCompatOutput::fail(1, no_such_session(socket, &target));
            };
            let mut lines = String::new();
            for pane in leaf_panes(&ws) {
                let line = match format.as_deref() {
                    Some(f) => expand_format(f, &pane.to_string(), "swarm-view", &ws.name),
                    None => pane.to_string(),
                };
                lines.push_str(&line);
                lines.push('\n');
            }
            TmuxCompatOutput::out(lines)
        }

        TmuxCommand::SplitWindow {
            target,
            horizontal,
            print,
            format,
        } => {
            let Some(pane) = resolve_pane(store, socket, &target).await else {
                return TmuxCompatOutput::fail(1, no_such_pane(&target));
            };
            // tmux `-h` puts the new pane beside the old one, which is
            // flowmux's `Vertical` split (vertical divider).
            let direction = if horizontal {
                SplitDirection::Vertical
            } else {
                SplitDirection::Horizontal
            };
            match store.split_pane(pane, direction).await {
                Some((ws_id, new_pane)) => {
                    match ui
                        .pane_split_applied(ws_id, pane, new_pane, direction)
                        .await
                    {
                        Ok(()) => print_pane(print, format.as_deref(), new_pane, None, ""),
                        Err(error) => {
                            let _ = store.close_pane(new_pane).await;
                            TmuxCompatOutput::fail(1, format!("{error}\n"))
                        }
                    }
                }
                None => TmuxCompatOutput::fail(1, no_such_pane(&target)),
            }
        }

        // Border styles, border formats and remain-on-exit are visual
        // tmux concerns flowmux handles natively; acknowledge them.
        TmuxCommand::SetOption { .. } | TmuxCommand::SelectLayout { .. } => TmuxCompatOutput::ok(),

        TmuxCommand::SelectPane { target, title } => {
            let Some(pane) = resolve_pane(store, socket, &target).await else {
                return TmuxCompatOutput::fail(1, no_such_pane(&target));
            };
            if let Some(title) = title {
                let Some(surface) = active_surface(store, pane).await else {
                    return TmuxCompatOutput::fail(1, no_such_pane(&target));
                };
                if let Err(e) = ui.rename_surface(pane, surface, &title).await {
                    return TmuxCompatOutput::fail(1, format!("select-pane: {e}\n"));
                }
            }
            TmuxCompatOutput::ok()
        }

        TmuxCommand::RespawnPane { target, command } => {
            let Some(pane) = resolve_pane(store, socket, &target).await else {
                return TmuxCompatOutput::fail(1, no_such_pane(&target));
            };
            // The pane runs the user's shell; typing the command in is
            // the same mechanism the claude-teams launcher uses.
            match ui.send_keys(pane, &format!("{command}\n")).await {
                Ok(()) => TmuxCompatOutput::ok(),
                Err(e) => TmuxCompatOutput::fail(1, format!("respawn-pane: {e}\n")),
            }
        }

        TmuxCommand::KillPane { target } => {
            let Some(pane) = resolve_pane(store, socket, &target).await else {
                return TmuxCompatOutput::fail(1, no_such_pane(&target));
            };
            let Some(ws_id) = store.workspace_of_pane(pane).await else {
                return TmuxCompatOutput::fail(1, no_such_pane(&target));
            };
            let pane_count = store
                .get_workspace(ws_id)
                .await
                .map(|ws| leaf_panes(&ws).len())
                .unwrap_or(0);
            if pane_count <= 1 {
                // tmux semantics: killing the last pane kills the
                // window/session — remove the workspace.
                ui.remove_workspace(ws_id).await;
                return TmuxCompatOutput::ok();
            }
            match ui.close_pane(pane).await {
                Ok(()) => TmuxCompatOutput::ok(),
                Err(e) => TmuxCompatOutput::fail(1, format!("kill-pane: {e}\n")),
            }
        }

        TmuxCommand::DisplayMessage { format } => {
            // Used only for cosmetic probes (client term type). Expand
            // known tokens against nothing rather than erroring.
            let line = format
                .map(|f| expand_format(&f, "", "", ""))
                .unwrap_or_default();
            TmuxCompatOutput::out(format!("{line}\n"))
        }

        TmuxCommand::Attach { target } => {
            let ws = match &target {
                Some(t) => resolve_session(store, socket, t).await,
                None => match socket {
                    Some(sock) => find_workspace(store, sock).await,
                    None => None,
                },
            };
            match ws {
                Some(ws) => {
                    ui.workspace_activated(ws.id).await;
                    TmuxCompatOutput::out(format!(
                        "flowmux: workspace '{}' focused (teammate panes are shown there)\n",
                        ws.name
                    ))
                }
                None => TmuxCompatOutput::fail(1, "no sessions\n".to_string()),
            }
        }

        TmuxCommand::KillServer => match socket {
            Some(sock) => {
                if let Some(ws) = find_workspace(store, sock).await {
                    ui.remove_workspace(ws.id).await;
                }
                TmuxCompatOutput::ok()
            }
            None => TmuxCompatOutput::fail(
                1,
                "tmux-compat: kill-server without -L is not supported\n".to_string(),
            ),
        },
    }
}

// ---- resolution helpers -------------------------------------------------

fn session_of(target: &Target) -> &str {
    match target {
        Target::Session { session, .. } => session,
        Target::Pane(_) => "",
    }
}

fn no_such_session(socket: Option<&str>, target: &Target) -> String {
    format!(
        "can't find session: {}\n",
        session_workspace_name(socket, session_of(target))
    )
}

fn no_such_pane(target: &Target) -> String {
    match target {
        Target::Pane(id) => format!("can't find pane: {id}\n"),
        Target::Session { session, .. } => format!("can't find pane in session: {session}\n"),
    }
}

async fn find_workspace(store: &StateStore, key: &str) -> Option<Workspace> {
    store
        .ordered_workspaces()
        .await
        .into_iter()
        .find(|w| w.custom_title.as_deref() == Some(key) || w.name == key)
}

/// Resolve a `-t` target to the workspace it names. Pane targets
/// resolve through the pane's owner so leader-mode invocations work.
async fn resolve_session(
    store: &StateStore,
    socket: Option<&str>,
    target: &Target,
) -> Option<Workspace> {
    match target {
        Target::Session { session, .. } => {
            find_workspace(store, &session_workspace_name(socket, session)).await
        }
        Target::Pane(raw) => {
            let pane: PaneId = raw.parse().ok()?;
            let ws_id = store.workspace_of_pane(pane).await?;
            store.get_workspace(ws_id).await
        }
    }
}

/// Resolve a `-t` target to a single pane (session targets mean the
/// workspace's first pane).
async fn resolve_pane(store: &StateStore, socket: Option<&str>, target: &Target) -> Option<PaneId> {
    match target {
        Target::Pane(raw) => {
            let pane: PaneId = raw.parse().ok()?;
            store.workspace_of_pane(pane).await.map(|_| pane)
        }
        Target::Session { .. } => {
            let ws = resolve_session(store, socket, target).await?;
            leaf_panes(&ws).first().copied()
        }
    }
}

fn leaf_panes(ws: &Workspace) -> Vec<PaneId> {
    let mut leaves = Vec::new();
    for surface in &ws.surfaces {
        surface.root_pane.for_each_leaf(|id| leaves.push(id));
    }
    leaves
}

async fn first_leaf(store: &StateStore, ws_id: WorkspaceId) -> Option<PaneId> {
    let ws = store.get_workspace(ws_id).await?;
    leaf_panes(&ws).first().copied()
}

/// The tab currently shown in a leaf pane.
async fn active_surface(store: &StateStore, pane: PaneId) -> Option<SurfaceId> {
    let ws_id = store.workspace_of_pane(pane).await?;
    let ws = store.get_workspace(ws_id).await?;
    for surface in &ws.surfaces {
        if let Some(active) = find_active_in_pane(&surface.root_pane, pane) {
            return Some(active);
        }
    }
    None
}

fn find_active_in_pane(pane: &Pane, target: PaneId) -> Option<SurfaceId> {
    match pane {
        Pane::Leaf { id, content } if *id == target => match content {
            PaneContent::Tabs { active, .. } => Some(*active),
            _ => None,
        },
        Pane::Leaf { .. } => None,
        Pane::Split { first, second, .. } => {
            find_active_in_pane(first, target).or_else(|| find_active_in_pane(second, target))
        }
    }
}

fn print_pane(
    print: bool,
    format: Option<&str>,
    pane: PaneId,
    window_name: Option<&str>,
    session: &str,
) -> TmuxCompatOutput {
    if !print {
        return TmuxCompatOutput::ok();
    }
    let id = pane.to_string();
    let line = match format {
        Some(f) => expand_format(f, &id, window_name.unwrap_or(""), session),
        None => id,
    };
    TmuxCompatOutput::out(format!("{line}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_state::State;
    use std::path::PathBuf;

    fn store() -> StateStore {
        StateStore::new_lazy(State::default())
    }

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| w.to_string()).collect()
    }

    async fn run(store: &StateStore, argv: &[&str]) -> TmuxCompatOutput {
        let ui = HeadlessTmuxUi { store };
        execute(store, &ui, &args(argv), &PathBuf::from("/tmp/team-root")).await
    }

    /// The full external-swarm sequence Claude Code issues when a lead
    /// (not inside tmux) spawns two teammates.
    #[tokio::test]
    async fn external_swarm_end_to_end() {
        let store = store();
        let sock = "claude-swarm-4242";

        // 1. has-session → no session yet.
        let out = run(&store, &["-L", sock, "has-session", "-t", "claude-swarm"]).await;
        assert_eq!(out.code, 1, "stderr: {}", out.stderr);

        // 2. new-session prints the initial pane id.
        let out = run(
            &store,
            &[
                "-L",
                sock,
                "new-session",
                "-d",
                "-s",
                "claude-swarm",
                "-n",
                "swarm-view",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "cat",
            ],
        )
        .await;
        assert_eq!(out.code, 0, "stderr: {}", out.stderr);
        let first_pane = out.stdout.trim().to_string();
        assert!(
            first_pane.parse::<PaneId>().is_ok(),
            "pane id: {first_pane}"
        );

        // Workspace root came from the request cwd.
        let ws = find_workspace(&store, sock).await.expect("workspace");
        assert_eq!(ws.root_dir, PathBuf::from("/tmp/team-root"));
        assert_eq!(ws.custom_title.as_deref(), Some(sock));

        // The GTK side continuously updates the automatic workspace
        // name from the focused terminal cwd. The pinned session key
        // must keep subsequent tmux calls resolvable.
        store
            .set_workspace_name(ws.id, "team-root".to_string())
            .await;

        // 3. has-session now succeeds; list-windows shows swarm-view.
        let out = run(&store, &["-L", sock, "has-session", "-t", "claude-swarm"]).await;
        assert_eq!(out.code, 0);
        let out = run(
            &store,
            &[
                "-L",
                sock,
                "list-windows",
                "-t",
                "claude-swarm",
                "-F",
                "#{window_name}",
            ],
        )
        .await;
        assert_eq!(out.stdout, "swarm-view\n");

        // 4. list-panes returns the single pane.
        let out = run(
            &store,
            &[
                "-L",
                sock,
                "list-panes",
                "-t",
                "claude-swarm:swarm-view",
                "-F",
                "#{pane_id}",
            ],
        )
        .await;
        assert_eq!(out.stdout.trim(), first_pane);

        // 5. First teammate reuses the initial pane: set-option +
        //    select-pane -T + respawn-pane all succeed.
        for argv in [
            vec![
                "-L",
                sock,
                "set-option",
                "-p",
                "-t",
                &first_pane,
                "remain-on-exit",
                "failed",
            ],
            vec![
                "-L",
                sock,
                "select-pane",
                "-t",
                &first_pane,
                "-T",
                "researcher",
            ],
            vec![
                "-L",
                sock,
                "respawn-pane",
                "-k",
                "-t",
                &first_pane,
                "--",
                "cd /tmp/team-root && env CLAUDECODE=1 claude --agent-name researcher",
            ],
        ] {
            let out = run(&store, &argv).await;
            assert_eq!(out.code, 0, "argv {argv:?} stderr: {}", out.stderr);
        }
        // The title stuck on the pane's active tab.
        let pane: PaneId = first_pane.parse().unwrap();
        let surface = active_surface(&store, pane).await.unwrap();
        assert_eq!(
            store.surface_title(pane, surface).await.as_deref(),
            Some("researcher")
        );

        // 6. Second teammate: split-window -v prints a fresh pane id.
        let out = run(
            &store,
            &[
                "-L",
                sock,
                "split-window",
                "-d",
                "-t",
                &first_pane,
                "-v",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "cat",
            ],
        )
        .await;
        assert_eq!(out.code, 0, "stderr: {}", out.stderr);
        let second_pane = out.stdout.trim().to_string();
        assert_ne!(second_pane, first_pane);

        // select-layout tiled is acknowledged.
        let out = run(
            &store,
            &[
                "-L",
                sock,
                "select-layout",
                "-t",
                "claude-swarm:swarm-view",
                "tiled",
            ],
        )
        .await;
        assert_eq!(out.code, 0);

        // list-panes now shows both.
        let out = run(
            &store,
            &[
                "-L",
                sock,
                "list-panes",
                "-t",
                "claude-swarm:swarm-view",
                "-F",
                "#{pane_id}",
            ],
        )
        .await;
        let panes: Vec<&str> = out.stdout.lines().collect();
        assert_eq!(panes.len(), 2);

        // 7. kill-pane removes the teammate pane…
        let out = run(&store, &["-L", sock, "kill-pane", "-t", &second_pane]).await;
        assert_eq!(out.code, 0);
        let out = run(
            &store,
            &[
                "-L",
                sock,
                "list-panes",
                "-t",
                "claude-swarm:swarm-view",
                "-F",
                "#{pane_id}",
            ],
        )
        .await;
        assert_eq!(out.stdout.lines().count(), 1);

        // …and killing the last pane removes the whole workspace.
        let out = run(&store, &["-L", sock, "kill-pane", "-t", &first_pane]).await;
        assert_eq!(out.code, 0);
        assert!(find_workspace(&store, sock).await.is_none());
    }

    #[tokio::test]
    async fn duplicate_session_is_rejected() {
        let store = store();
        let new_session = [
            "-L",
            "claude-swarm-7",
            "new-session",
            "-d",
            "-s",
            "claude-swarm",
        ];
        assert_eq!(run(&store, &new_session).await.code, 0);
        let out = run(&store, &new_session).await;
        assert_eq!(out.code, 1);
        assert!(out.stderr.contains("duplicate session"));
    }

    #[tokio::test]
    async fn sockets_isolate_concurrent_teams() {
        let store = store();
        for sock in ["claude-swarm-1", "claude-swarm-2"] {
            let out = run(
                &store,
                &["-L", sock, "new-session", "-d", "-s", "claude-swarm"],
            )
            .await;
            assert_eq!(out.code, 0);
        }
        // Same session name, different sockets → different workspaces.
        assert!(find_workspace(&store, "claude-swarm-1").await.is_some());
        assert!(find_workspace(&store, "claude-swarm-2").await.is_some());
        // has-session on socket 1 does not see socket 2's removal.
        let out = run(&store, &["-L", "claude-swarm-2", "kill-server"]).await;
        assert_eq!(out.code, 0);
        assert!(find_workspace(&store, "claude-swarm-2").await.is_none());
        let out = run(
            &store,
            &["-L", "claude-swarm-1", "has-session", "-t", "claude-swarm"],
        )
        .await;
        assert_eq!(out.code, 0);
    }

    #[tokio::test]
    async fn legacy_window_per_teammate_path() {
        let store = store();
        // Default socket: new-session then one new-window per teammate.
        let out = run(&store, &["new-session", "-d", "-s", "claude-swarm"]).await;
        assert_eq!(out.code, 0);
        let out = run(
            &store,
            &[
                "new-window",
                "-t",
                "claude-swarm",
                "-n",
                "teammate-researcher",
                "-P",
                "-F",
                "#{pane_id}",
                "--",
                "cat",
            ],
        )
        .await;
        assert_eq!(out.code, 0, "stderr: {}", out.stderr);
        let teammate_pane = out.stdout.trim().to_string();
        assert!(teammate_pane.parse::<PaneId>().is_ok());

        let ws = find_workspace(&store, "claude-swarm").await.unwrap();
        assert_eq!(leaf_panes(&ws).len(), 2);

        // respawn into the teammate pane works.
        let out = run(
            &store,
            &[
                "respawn-pane",
                "-k",
                "-t",
                &teammate_pane,
                "--",
                "claude --agent-name r",
            ],
        )
        .await;
        assert_eq!(out.code, 0);
    }

    #[tokio::test]
    async fn attach_focuses_workspace() {
        let store = store();
        let sock = "claude-swarm-9";
        run(
            &store,
            &["-L", sock, "new-session", "-d", "-s", "claude-swarm"],
        )
        .await;
        // Another workspace steals focus.
        let other = store
            .create_workspace(Some("other".into()), PathBuf::from("/tmp/o"))
            .await;
        store.set_active_workspace(Some(other)).await;

        // The status-line hint `tmux -L claude-swarm-<pid> a`.
        let out = run(&store, &["-L", sock, "a"]).await;
        assert_eq!(out.code, 0);
        let ws = find_workspace(&store, sock).await.unwrap();
        assert_eq!(store.snapshot().await.active_workspace, Some(ws.id));
    }

    #[tokio::test]
    async fn version_and_errors() {
        let store = store();
        let out = run(&store, &["-V"]).await;
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("tmux 3.4"));

        let out = run(&store, &["kill-pane", "-t", "not-a-uuid"]).await;
        assert_eq!(out.code, 1);

        let out = run(&store, &["pipe-pane", "-t", "%1"]).await;
        assert_eq!(out.code, 1);
        assert!(out.stderr.contains("unsupported"));
    }
}
