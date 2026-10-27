// SPDX-License-Identifier: GPL-3.0-or-later
//! GUI-aware IPC handler.
//!
//! Wraps `flowmux_daemon::DaemonHandler` and intercepts the verbs that
//! need to mutate the GTK widget tree (workspace creation, pane split,
//! send-keys, browser open). Those verbs are forwarded across the
//! [`Bridge`] to the GTK main loop and the response is awaited via a
//! `oneshot` channel.

use crate::activity::ActivityEntry;
use crate::bridge::{Bridge, BrowserActionResult, BrowserOp, GtkCommand};
use flowmux_core::{AgentStatusReport, SplitDirection};
use flowmux_daemon::{tmux_compat::TmuxCompatUi, DaemonHandler};
use flowmux_ipc::protocol::{BrowserWaitCondition, Request, Response, RpcError};
use flowmux_ipc::server::Handler;
use flowmux_state::session_history::{codex_thread_names, SessionAgent};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use tokio::sync::oneshot;
use tracing::warn;

const AGENT_VISIBILITY_QUERY_TIMEOUT: Duration = Duration::from_millis(100);
const CODEX_STOP_INGRESS_GRACE: Duration = Duration::from_millis(250);
const BROWSER_WAIT_MAX_TIMEOUT_MS: u64 = 300_000;
const BROWSER_WAIT_MIN_POLL_MS: u64 = 10;

async fn query_agent_surface_visible(bridge: &Bridge, surface: flowmux_core::SurfaceId) -> bool {
    tokio::time::timeout(AGENT_VISIBILITY_QUERY_TIMEOUT, async {
        let (visibility_tx, visibility_rx) = oneshot::channel();
        bridge
            .send(GtkCommand::QueryAgentSurfaceVisible {
                surface,
                ack: visibility_tx,
            })
            .await
            .ok()?;
        visibility_rx.await.ok()
    })
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

fn browser_error_response(error: String) -> Response {
    if error.starts_with("browser surface not found:")
        || error.starts_with("browser pane not found:")
        || error.starts_with("pane not found:")
        || error == "no target pane focused"
    {
        Response::Error(RpcError::NotFound(error))
    } else {
        Response::Error(RpcError::Internal(error))
    }
}

async fn materialize_workspace(
    bridge: &Bridge,
    id: flowmux_core::WorkspaceId,
) -> Result<(), String> {
    let (ack, reply) = oneshot::channel();
    bridge
        .tx
        .send(GtkCommand::WorkspaceCreated { id, ack })
        .await
        .map_err(|_| "bridge closed".to_string())?;
    reply.await.map_err(|_| "bridge closed".to_string())?
}

/// Success means the model and GTK tree have both applied the split.
async fn split_pane(
    bridge: &Bridge,
    pane: flowmux_core::PaneId,
    direction: SplitDirection,
) -> Result<flowmux_core::PaneId, String> {
    let (ack, reply) = oneshot::channel();
    bridge
        .tx
        .send(GtkCommand::SplitPane {
            pane,
            direction,
            ack,
        })
        .await
        .map_err(|_| "bridge closed".to_string())?;
    reply.await.map_err(|_| "bridge closed".to_string())?
}

async fn activate_workspace(bridge: &Bridge, id: flowmux_core::WorkspaceId) -> Result<(), String> {
    let (ack, reply) = oneshot::channel();
    bridge
        .tx
        .send(GtkCommand::ActivateWorkspace { id, ack: Some(ack) })
        .await
        .map_err(|_| "bridge closed".to_string())?;
    reply.await.map_err(|_| "bridge closed".to_string())?
}

async fn browser_action(
    bridge: &Bridge,
    pane: flowmux_core::PaneId,
    surface: Option<flowmux_core::SurfaceId>,
    op: BrowserOp,
) -> Response {
    let validated = match &op {
        BrowserOp::WithTask { op, .. } => op.as_ref(),
        op => op,
    };
    let validated = match validated {
        BrowserOp::Receipt { op, .. } => op.as_ref(),
        op => op,
    };
    if let BrowserOp::Inspect(options) = validated {
        if options.max_nodes.is_some_and(|n| !(1..=2000).contains(&n))
            || options
                .max_chars
                .is_some_and(|n| !(1024..=262144).contains(&n))
        {
            return Response::Error(RpcError::InvalidArgument(
                "snapshot limits: max_nodes 1..2000, max_chars 1024..262144".into(),
            ));
        }
    }
    // A wait or async script holds its connection permit and, for mutations,
    // a mutation permit until it settles. Bound both so an abandoned client
    // cannot exhaust the permits or poll the page every millisecond.
    if matches!(validated, BrowserOp::EvalAsync { timeout_ms, .. }
        if !(1..=BROWSER_WAIT_MAX_TIMEOUT_MS as u32).contains(timeout_ms))
    {
        return Response::Error(RpcError::InvalidArgument(
            "async timeout must be 1..300000 ms".into(),
        ));
    }
    if let BrowserOp::Wait {
        timeout_ms,
        poll_ms,
        ..
    } = validated
    {
        if !(1..=BROWSER_WAIT_MAX_TIMEOUT_MS).contains(timeout_ms)
            || !(BROWSER_WAIT_MIN_POLL_MS..=BROWSER_WAIT_MAX_TIMEOUT_MS).contains(poll_ms)
        {
            return Response::Error(RpcError::InvalidArgument(
                "wait timeout must be 1..300000 ms and poll interval 10..300000 ms".into(),
            ));
        }
    }
    if let BrowserOp::Drag { from, to } = validated {
        if from
            .iter()
            .chain(to)
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Response::Error(RpcError::InvalidArgument(
                "drag coordinates must be finite and nonnegative".into(),
            ));
        }
    }
    if let BrowserOp::Pointer { x, y, .. } = validated {
        if !x.is_finite() || !y.is_finite() || *x < 0.0 || *y < 0.0 {
            return Response::Error(RpcError::InvalidArgument(
                "pointer coordinates must be finite and nonnegative".into(),
            ));
        }
    }
    if let BrowserOp::Media {
        action: flowmux_ipc::protocol::BrowserMediaAction::Seek(seconds),
        ..
    } = validated
    {
        if !seconds.is_finite() || *seconds < 0.0 {
            return Response::Error(RpcError::InvalidArgument(
                "seek seconds must be finite and nonnegative".into(),
            ));
        }
    }
    tracing::debug!(
        %pane,
        verb = op.capability_name(),
        query = op.is_query(),
        "dispatching browser action"
    );
    let (tx, rx) = oneshot::channel();
    let _ = bridge
        .send(GtkCommand::BrowserAction {
            pane,
            surface,
            op,
            ack: tx,
        })
        .await;
    match rx.await {
        Ok(Ok(BrowserActionResult::Ok)) => Response::BrowserOk,
        Ok(Ok(BrowserActionResult::Bool(value))) => Response::BrowserBoolResult { value },
        Ok(Ok(BrowserActionResult::String(value))) => Response::BrowserResult { value },
        Ok(Err(e)) => browser_error_response(e),
        Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
    }
}

/// Browser requests that map directly to the shared GTK browser action path.
/// Open and cookie import keep their dedicated response contracts.
struct BrowserActionRequest {
    pane: flowmux_core::PaneId,
    op: BrowserOp,
}

impl TryFrom<Request> for BrowserActionRequest {
    type Error = Request;

    fn try_from(request: Request) -> Result<Self, Self::Error> {
        let (pane, op) = match request {
            Request::BrowserDrag { pane, from, to } => (pane, BrowserOp::Drag { from, to }),
            Request::BrowserPointer { pane, action, x, y } => {
                (pane, BrowserOp::Pointer { action, x, y })
            }
            Request::BrowserFileChooser { pane, action } => {
                (pane, BrowserOp::FileChooser { action })
            }
            Request::BrowserDialog { pane, action } => (pane, BrowserOp::Dialog { action }),
            Request::BrowserDownload { pane, action } => (pane, BrowserOp::Download { action }),
            Request::BrowserMedia {
                pane,
                target,
                action,
            } => (pane, BrowserOp::Media { target, action }),
            Request::BrowserEvalAsync {
                pane,
                body,
                timeout_ms,
            } => (pane, BrowserOp::EvalAsync { body, timeout_ms }),
            Request::BrowserNavigate { pane, url } => (pane, BrowserOp::Navigate { url }),
            Request::BrowserBack { pane } => (pane, BrowserOp::Back),
            Request::BrowserForward { pane } => (pane, BrowserOp::Forward),
            Request::BrowserReload { pane } => (pane, BrowserOp::Reload),
            Request::BrowserUrl { pane } => (pane, BrowserOp::Url),
            Request::BrowserTitle { pane } => (pane, BrowserOp::Title),
            Request::BrowserClick { pane, target } => (pane, BrowserOp::Click { target }),
            Request::BrowserFill {
                pane,
                target,
                value,
            } => (pane, BrowserOp::Fill { target, value }),
            Request::BrowserSelect {
                pane,
                target,
                value,
            } => (pane, BrowserOp::Select { target, value }),
            Request::BrowserScroll { pane, target, x, y } => {
                (pane, BrowserOp::Scroll { target, x, y })
            }
            Request::BrowserType { pane, text } => (pane, BrowserOp::Type { text }),
            Request::BrowserPress { pane, key } => (pane, BrowserOp::Press { key }),
            Request::BrowserText { pane, target } => (pane, BrowserOp::Text { target }),
            Request::BrowserValue { pane, target } => (pane, BrowserOp::Value { target }),
            Request::BrowserAttr { pane, target, name } => (pane, BrowserOp::Attr { target, name }),
            Request::BrowserWait {
                pane,
                condition,
                timeout_ms,
                poll_ms,
            } => (
                pane,
                BrowserOp::Wait {
                    condition,
                    timeout_ms,
                    poll_ms,
                },
            ),
            Request::BrowserScreenshot {
                pane,
                path,
                full_page,
                element,
            } => (
                pane,
                BrowserOp::Screenshot {
                    path,
                    full_page,
                    element,
                },
            ),
            Request::BrowserDblClick { pane, target } => (pane, BrowserOp::DblClick { target }),
            Request::BrowserHover { pane, target } => (pane, BrowserOp::Hover { target }),
            Request::BrowserFocus { pane, target } => (pane, BrowserOp::Focus { target }),
            Request::BrowserBlur { pane, target } => (pane, BrowserOp::Blur { target }),
            Request::BrowserCheck { pane, target } => (pane, BrowserOp::Check { target }),
            Request::BrowserUncheck { pane, target } => (pane, BrowserOp::Uncheck { target }),
            Request::BrowserIsVisible { pane, target } => (pane, BrowserOp::IsVisible { target }),
            Request::BrowserIsEnabled { pane, target } => (pane, BrowserOp::IsEnabled { target }),
            Request::BrowserIsChecked { pane, target } => (pane, BrowserOp::IsChecked { target }),
            Request::BrowserCount { pane, selector } => (pane, BrowserOp::Count { selector }),
            Request::BrowserSnapshot { pane } => (pane, BrowserOp::Snapshot),
            Request::BrowserInspect { pane, options } => (pane, BrowserOp::Inspect(options)),
            Request::BrowserTask { pane, details } => (pane, BrowserOp::Task(details)),
            Request::BrowserForm { pane, fields } => (pane, BrowserOp::Form(fields)),
            Request::BrowserControl { pane, action } => (pane, BrowserOp::Control { action }),
            Request::BrowserEval { pane, source } => (pane, BrowserOp::Eval { source }),
            Request::BrowserDiagnose { pane } => (pane, BrowserOp::Diagnose),
            other => return Err(other),
        };
        Ok(Self { pane, op })
    }
}

pub struct GuiHandler {
    inner: DaemonHandler,
    bridge: Bridge,
    /// Agent-session store rooted at `$XDG_DATA_HOME/flowmux/agent-sessions/`.
    /// `None` when the XDG data dir is unavailable (very rare on Linux, but
    /// possible in minimal containers without HOME / XDG_DATA_HOME). Injected
    /// so tests never touch the developer's real store.
    session_store: Option<flowmux_state::AgentSessionStore>,
}

impl GuiHandler {
    pub fn new(inner: DaemonHandler, bridge: Bridge) -> Self {
        Self::with_session_store(inner, bridge, flowmux_state::default_agent_session_store())
    }

    pub fn with_session_store(
        inner: DaemonHandler,
        bridge: Bridge,
        session_store: Option<flowmux_state::AgentSessionStore>,
    ) -> Self {
        Self {
            inner,
            bridge,
            session_store,
        }
    }
}

// Resolve outside the caller's sandbox: /proc there can contain only namespace PIDs.
fn direct_window_ancestors(mut pid: u32, window: u32) -> Vec<u32> {
    let mut ancestors = Vec::new();
    for _ in 0..64 {
        if pid == window {
            return ancestors;
        }
        if flowmux_procmon::cmdline_of(pid)
            .iter()
            .any(|arg| arg == "--managed-daemon")
        {
            return Vec::new();
        }
        ancestors.push(pid);
        let Some(parent) = flowmux_procmon::parent_pid(pid).filter(|p| *p > 1) else {
            break;
        };
        pid = parent;
    }
    Vec::new()
}

impl Handler for GuiHandler {
    fn handle_from<'a>(
        &'a self,
        req: Request,
        peer_pid: Option<u32>,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        Box::pin(async move {
            let Request::AgentSurfaceCurrent { agent } = req else {
                return self.handle(req).await;
            };
            let missing =
                || Response::Error(RpcError::NotFound("caller has no direct agent pane".into()));
            let Some(peer_pid) = peer_pid else {
                return missing();
            };
            let ancestors = direct_window_ancestors(peer_pid, std::process::id());
            tracing::debug!(target: "flowmux_agent", peer_pid, ?ancestors, "direct agent source lookup");
            if ancestors.is_empty() {
                return missing();
            }
            // First tools can beat both native hooks and the periodic process poll.
            let agent_rank = ancestors
                .iter()
                .position(|pid| flowmux_procmon::agent_name_for_pid(*pid) == Some(agent.as_str()));
            let terminal_pids = if agent_rank.is_some() {
                let (ack, rx) = oneshot::channel();
                let query = async {
                    self.bridge
                        .send(GtkCommand::QueryTerminalAgentPids { ack })
                        .await
                        .ok()?;
                    rx.await.ok()
                };
                tokio::time::timeout(std::time::Duration::from_secs(1), query)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            let Response::Tree { workspaces } = self.inner.handle(Request::WorkspaceTree).await
            else {
                return missing();
            };
            let mut matches = Vec::new();
            for workspace in workspaces {
                for pane in workspace.panes {
                    for tab in pane.tabs {
                        let mut rank = terminal_pids
                            .iter()
                            .find(|(surface, _)| *surface == tab.id)
                            .and_then(|(_, pid)| ancestors.iter().position(|p| p == pid))
                            // A terminal root is the window's direct child. A recycled
                            // PID deeper in another terminal cannot establish ownership.
                            .filter(|rank| *rank + 1 == ancestors.len());
                        if terminal_pids.is_empty() {
                            if let Some(located) =
                                self.inner.store().located_agent_presence(tab.id).await
                            {
                                if located.presence.name == agent {
                                    rank = located.presence.pid.and_then(|pid| {
                                        ancestors.iter().position(|ancestor| *ancestor == pid)
                                    });
                                }
                            }
                        }
                        if let Some(rank) = rank {
                            matches.push((rank, pane.id, tab.id));
                        }
                    }
                }
            }
            tracing::debug!(target: "flowmux_agent", ?matches, "direct agent source candidates");
            matches.sort_unstable_by_key(|candidate| candidate.0);
            match matches.as_slice() {
                [(rank, pane, surface), rest @ ..]
                    if rest.first().is_none_or(|next| next.0 != *rank) =>
                {
                    Response::AgentSurface {
                        pane: *pane,
                        surface: *surface,
                        session_id: None,
                    }
                }
                _ => missing(),
            }
        })
    }

    fn handle<'a>(&'a self, req: Request) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        Box::pin(async move {
            match req {
                Request::Ssh { request } => {
                    let (ack, rx) = oneshot::channel();
                    let _ = self.bridge.send(GtkCommand::Ssh { request, ack }).await;
                    match rx.await {
                        Ok(Ok(value)) => Response::Ssh { value },
                        Ok(Err(error)) => Response::Error(RpcError::Internal(error)),
                        Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                    }
                }
                Request::WorkspaceCreate { .. }
                | Request::WorkspaceFocus { .. }
                | Request::SurfaceCreate { .. } => self.handle_workspace_verb(req).await,
                Request::TeamSpawn { .. }
                | Request::PaneSplit { .. }
                | Request::PaneSendKeys { .. }
                | Request::PaneReadScreen { .. }
                | Request::TerminalOutput { .. }
                | Request::SurfaceFocus { .. }
                | Request::SurfaceClose { .. }
                | Request::PaneFocus { .. }
                | Request::PaneResize { .. }
                | Request::PaneClose { .. } => self.handle_pane_verb(req).await,
                Request::BrowserReceipt { .. }
                | Request::BrowserWithTask { .. }
                | Request::BrowserTask { .. }
                | Request::BrowserForm { .. }
                | Request::BrowserControl { .. }
                | Request::BrowserOnSurface { .. }
                | Request::BrowserOpen { .. }
                | Request::BrowserNavigate { .. }
                | Request::BrowserBack { .. }
                | Request::BrowserForward { .. }
                | Request::BrowserReload { .. }
                | Request::BrowserUrl { .. }
                | Request::BrowserTitle { .. }
                | Request::BrowserClick { .. }
                | Request::BrowserFill { .. }
                | Request::BrowserSelect { .. }
                | Request::BrowserScroll { .. }
                | Request::BrowserType { .. }
                | Request::BrowserPress { .. }
                | Request::BrowserText { .. }
                | Request::BrowserValue { .. }
                | Request::BrowserAttr { .. }
                | Request::BrowserWait { .. }
                | Request::BrowserScreenshot { .. }
                | Request::BrowserDblClick { .. }
                | Request::BrowserHover { .. }
                | Request::BrowserFocus { .. }
                | Request::BrowserBlur { .. }
                | Request::BrowserCheck { .. }
                | Request::BrowserUncheck { .. }
                | Request::BrowserIsVisible { .. }
                | Request::BrowserIsEnabled { .. }
                | Request::BrowserIsChecked { .. }
                | Request::BrowserCount { .. }
                | Request::BrowserSnapshot { .. }
                | Request::BrowserInspect { .. }
                | Request::BrowserDiagnose { .. }
                | Request::ImportCookies { .. }
                | Request::BrowserEval { .. } => self.handle_browser_verb(req).await,
                Request::BrowserEvalAsync { .. }
                | Request::BrowserMedia { .. }
                | Request::BrowserFileChooser { .. }
                | Request::BrowserPointer { .. }
                | Request::BrowserDrag { .. }
                | Request::BrowserDialog { .. }
                | Request::BrowserDownload { .. } => self.handle_browser_verb(req).await,
                Request::ClaudeTeams { .. }
                | Request::TmuxCompat { .. }
                | Request::AgentSessionUpdate { .. }
                | Request::AgentSessionGet { .. }
                | Request::AgentSessionForget { .. }
                | Request::AgentSurfaceResolve { .. }
                | Request::AgentActivityUpdate { .. }
                | Request::AgentLifecycleUpdate { .. } => self.handle_agent_verb(req).await,
                Request::Notify { .. }
                | Request::NotificationsList { .. }
                | Request::NotificationOpen { .. }
                | Request::NotificationJumpToUnread
                | Request::NotificationMarkRead { .. }
                | Request::NotificationsClear => self.handle_notification_verb(req).await,
                _ => self.inner.handle(req).await,
            }
        })
    }
}

const _: fn() = || {
    fn assert_clone<T: Clone>() {}
    assert_clone::<Request>();
};

/// Quote a single argv element for safe `sh` re-parsing.
fn shell_escape(arg: String) -> String {
    if !arg.is_empty()
        && arg
            .bytes()
            .all(|b| matches!(b, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' | b'/' | b':' | b'='))
    {
        return arg;
    }
    let escaped = arg.replace('\'', "'\\''");
    format!("'{escaped}'")
}

impl GuiHandler {
    /// Dispatch for the workspace verb group (split out of the `handle` match).
    async fn handle_workspace_verb(&self, req: Request) -> Response {
        match req {
            Request::WorkspaceCreate { .. } => {
                // Create the model, then require the GUI to materialize it.
                let resp = self.inner.handle(req.clone()).await;
                let id = match &resp {
                    Response::WorkspaceCreated { id } => *id,
                    _ => return resp,
                };
                match materialize_workspace(&self.bridge, id).await {
                    Ok(()) => Response::WorkspaceCreated { id },
                    Err(error) => {
                        self.inner.store().remove_workspace(id).await;
                        Response::Error(RpcError::Internal(error))
                    }
                }
            }
            Request::WorkspaceFocus { workspace } => {
                // Validate against live state so a bad id returns a
                // clean NotFound instead of a silent no-op. The focus
                // itself reuses ActivateWorkspace — the exact operation
                // a sidebar row click performs: no dialog, reversible,
                // creates/destroys nothing.
                let exists = self
                    .inner
                    .store()
                    .snapshot()
                    .await
                    .workspaces
                    .iter()
                    .any(|w| w.id == workspace);
                if exists {
                    match activate_workspace(&self.bridge, workspace).await {
                        Ok(()) => Response::Ok,
                        Err(error) => Response::Error(RpcError::Internal(error)),
                    }
                } else {
                    Response::Error(RpcError::NotFound(workspace.to_string()))
                }
            }
            Request::SurfaceCreate {
                workspace,
                cwd,
                shell,
            } => {
                if self.inner.store().get_workspace(workspace).await.is_none() {
                    return Response::Error(RpcError::NotFound(workspace.to_string()));
                }
                let (tx, rx) = oneshot::channel();
                if let Err(error) = self
                    .bridge
                    .tx
                    .send(GtkCommand::CreateSurface {
                        workspace,
                        cwd,
                        shell,
                        ack: tx,
                    })
                    .await
                {
                    return Response::Error(RpcError::Internal(error.to_string()));
                }
                match rx.await {
                    Ok(Ok((pane, id))) => Response::SurfaceCreated { id, pane },
                    Ok(Err(error)) => Response::Error(RpcError::Internal(error)),
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            other => unreachable!("workspace router got a non-workspace verb: {other:?}"),
        }
    }

    /// Dispatch for the pane verb group (split out of the `handle` match).
    async fn handle_pane_verb(&self, req: Request) -> Response {
        match req {
            Request::TeamSpawn {
                pane,
                cwd,
                shell,
                role,
            } => {
                let (ack, reply) = oneshot::channel();
                if self
                    .bridge
                    .tx
                    .send(GtkCommand::TeamSpawn {
                        pane,
                        cwd,
                        shell,
                        role,
                        ack,
                    })
                    .await
                    .is_err()
                {
                    return Response::Error(RpcError::Internal("bridge closed".into()));
                }
                match reply.await {
                    Ok(Ok((pane, id))) => Response::SurfaceCreated { pane, id },
                    Ok(Err(error)) => Response::Error(RpcError::Internal(error)),
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::PaneSplit { pane, direction } => {
                if self.inner.store().workspace_for_pane(pane).await.is_none() {
                    return Response::Error(RpcError::NotFound(pane.to_string()));
                }
                match split_pane(&self.bridge, pane, direction).await {
                    Ok(new_pane) => Response::PaneSplitDone { new_pane },
                    Err(error) => Response::Error(RpcError::Internal(error)),
                }
            }
            Request::PaneSendKeys { pane, keys } => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .tx
                    .send(GtkCommand::PaneSendKeys {
                        pane,
                        keys,
                        ack: tx,
                    })
                    .await;
                match rx.await {
                    Ok(Ok(())) => Response::Ok,
                    Ok(Err(e)) => Response::Error(RpcError::NotFound(e)),
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::PaneReadScreen { pane } => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .send(GtkCommand::PaneReadScreen { pane, ack: tx })
                    .await;
                match rx.await {
                    Ok(Ok(Some(text))) => Response::ScreenContents { text },
                    // `None` = the pane has no readable terminal surface
                    // (e.g. a browser tab). Terminal panes expose screen text through VTE,
                    // so this is not feature-gated.
                    Ok(Ok(None)) => Response::Error(RpcError::Unimplemented(
                        "read-screen: this pane has no readable terminal surface".into(),
                    )),
                    Ok(Err(e)) => Response::Error(RpcError::NotFound(e)),
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::TerminalOutput {
                surface,
                alternate_screen,
            } => {
                let (tx, rx) = oneshot::channel();
                if let Err(error) = self
                    .bridge
                    .tx
                    .send(GtkCommand::TerminalOutputObserved {
                        surface,
                        alternate_screen,
                        ack: tx,
                    })
                    .await
                {
                    return Response::Error(RpcError::Internal(error.to_string()));
                }
                match rx.await {
                    Ok(()) => Response::Ok,
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::SurfaceFocus { pane, surface } => {
                // Validate the pane against live state (reusing the
                // tree flattener), then await the same ActivateSurface the tab
                // bar uses so a following query observes the selected tab.
                let workspaces = self.inner.store().ordered_workspaces().await;
                let tree = flowmux_ipc::protocol::describe_workspaces(&workspaces);
                let pane_found = tree.iter().flat_map(|w| &w.panes).any(|p| p.id == pane);
                let surface_found = tree
                    .iter()
                    .flat_map(|w| &w.panes)
                    .find(|p| p.id == pane)
                    .is_some_and(|p| p.tabs.iter().any(|tab| tab.id == surface));
                if surface_found {
                    let (ack, reply) = oneshot::channel();
                    let _ = self
                        .bridge
                        .tx
                        .send(GtkCommand::ActivateSurface {
                            pane,
                            surface,
                            ack: Some(ack),
                        })
                        .await;
                    match reply.await {
                        Ok(Ok(())) => Response::Ok,
                        Ok(Err(error)) => Response::Error(RpcError::Internal(error)),
                        Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                    }
                } else if pane_found {
                    Response::Error(RpcError::NotFound(format!(
                        "surface not found in pane {pane}: {surface}"
                    )))
                } else {
                    Response::Error(RpcError::NotFound(format!("pane not found: {pane}")))
                }
            }
            Request::SurfaceClose { pane, surface } => {
                // Refuse workspace teardown here; dirty editors can still prompt below.
                let workspaces = self.inner.store().ordered_workspaces().await;
                let tree = flowmux_ipc::protocol::describe_workspaces(&workspaces);
                let pane_found = tree.iter().flat_map(|w| &w.panes).any(|p| p.id == pane);
                let surface_found = tree
                    .iter()
                    .flat_map(|w| &w.panes)
                    .find(|p| p.id == pane)
                    .is_some_and(|p| p.tabs.iter().any(|tab| tab.id == surface));
                let tabs = self.inner.store().tab_count_in_pane(pane).await;
                let panes = self.inner.store().workspace_pane_count_for(pane).await;
                if !pane_found {
                    Response::Error(RpcError::NotFound(format!("pane not found: {pane}")))
                } else if !surface_found {
                    Response::Error(RpcError::NotFound(format!(
                        "surface not found in pane {pane}: {surface}"
                    )))
                } else if tabs == Some(1) && matches!(panes, Some((_, 1))) {
                    Response::Error(RpcError::InvalidArgument(
                        "refusing to close the last tab of the workspace's last pane".into(),
                    ))
                } else {
                    let (tx, rx) = oneshot::channel();
                    let _ = self
                        .bridge
                        .tx
                        .send(GtkCommand::CloseSurface {
                            pane,
                            surface,
                            ack: tx,
                        })
                        .await;
                    match rx.await {
                        Ok(Ok(())) => Response::Ok,
                        Ok(Err(e)) => Response::Error(RpcError::Internal(e)),
                        Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                    }
                }
            }
            Request::PaneFocus { pane } => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .tx
                    .send(GtkCommand::FocusPane { pane, ack: tx })
                    .await;
                match rx.await {
                    Ok(Ok(())) => Response::Ok,
                    Ok(Err(e)) => Response::Error(RpcError::NotFound(e)),
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::PaneResize { pane, ratio } => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .tx
                    .send(GtkCommand::ResizePane {
                        pane,
                        ratio,
                        ack: tx,
                    })
                    .await;
                match rx.await {
                    Ok(Ok(())) => Response::Ok,
                    Ok(Err(e)) => Response::Error(RpcError::NotFound(e)),
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::PaneClose { pane } => {
                // Refuse the workspace's last pane; dirty editors in other panes can still prompt.
                match self.inner.store().workspace_pane_count_for(pane).await {
                    None => Response::Error(RpcError::NotFound(format!("pane not found: {pane}"))),
                    Some((_, 1)) => Response::Error(RpcError::InvalidArgument(
                        "refusing to close the workspace's last pane; close the workspace \
                             instead"
                            .into(),
                    )),
                    Some((_, _)) => {
                        let (tx, rx) = oneshot::channel();
                        let _ = self
                            .bridge
                            .tx
                            .send(GtkCommand::CloseFocused { pane, ack: tx })
                            .await;
                        match rx.await {
                            Ok(Ok(())) => Response::Ok,
                            Ok(Err(e)) => Response::Error(RpcError::Internal(e)),
                            Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                        }
                    }
                }
            }
            other => unreachable!("pane router got a non-pane verb: {other:?}"),
        }
    }

    /// Dispatch for the browser verb group (split out of the `handle` match).
    async fn handle_browser_verb(&self, req: Request) -> Response {
        let (surface, req) = match req {
            Request::BrowserOnSurface { surface, request } => (Some(surface), *request),
            req => (None, req),
        };
        let (task, req) = match req {
            Request::BrowserWithTask { task, request } => (Some(task), *request),
            req => (None, req),
        };
        let (receipt, req) = match req {
            Request::BrowserReceipt {
                request,
                condition,
                timeout_ms,
            } => {
                if timeout_ms == 0
                    || timeout_ms > 60_000
                    || matches!(condition, Some(BrowserWaitCondition::Js(_)))
                {
                    return Response::Error(RpcError::InvalidArgument("receipt timeout must be 1..60000 ms and its condition must not execute JavaScript".into()));
                }
                (Some((condition, timeout_ms)), *request)
            }
            req => (None, req),
        };
        let req = match BrowserActionRequest::try_from(req) {
            Ok(action) => {
                if receipt.is_some()
                    && matches!(action.op, BrowserOp::Control { .. } | BrowserOp::Diagnose)
                {
                    return Response::Error(RpcError::InvalidArgument(
                        "receipt requires a browser action".into(),
                    ));
                }
                let action_op = match receipt {
                    Some((condition, timeout_ms)) => BrowserOp::Receipt {
                        op: Box::new(action.op),
                        condition,
                        timeout_ms,
                    },
                    None => action.op,
                };
                let op = match task {
                    Some(task) => BrowserOp::WithTask {
                        task,
                        op: Box::new(action_op),
                    },
                    None => action_op,
                };
                return browser_action(&self.bridge, action.pane, surface, op).await;
            }
            Err(req) => req,
        };
        if surface.is_some() || task.is_some() || receipt.is_some() {
            return Response::Error(RpcError::InvalidArgument(
                "surface targeting requires a browser action or eval".into(),
            ));
        }
        match req {
            Request::BrowserOpen {
                url,
                target_pane,
                direction,
            } => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .tx
                    .send(GtkCommand::BrowserOpenSplit {
                        target_pane,
                        url,
                        direction,
                        ack: tx,
                    })
                    .await;
                match rx.await {
                    Ok(Ok(outcome)) => Response::BrowserPaneOpened {
                        pane: outcome.pane,
                        placement_strategy: outcome.placement_strategy,
                    },
                    Ok(Err(e)) => browser_error_response(e),
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::ImportCookies { .. } => Response::Error(RpcError::Unimplemented(
                "Cookie insertion into browser profiles is not implemented; no cookies were read or imported".into(),
            )),
            other => unreachable!("browser router got a non-browser verb: {other:?}"),
        }
    }

    /// Dispatch for the agent verb group (split out of the `handle` match).
    async fn handle_agent_verb(&self, req: Request) -> Response {
        if let Request::AgentSessionUpdate { surface, .. }
        | Request::AgentSessionGet { surface, .. }
        | Request::AgentSessionForget { surface, .. } = &req
        {
            if self.inner.store().is_ssh_surface(*surface).await {
                return Response::Error(RpcError::Unimplemented(
                    "Local agent sessions are unavailable for SSH terminals".into(),
                ));
            }
        }
        match req {
            // One tmux CLI invocation forwarded by the `tmux` shim —
            // Claude Code agent teams driving flowmux panes natively.
            // State mutations run in the shared orchestrator; widget
            // work goes through the bridge-backed UI below.
            Request::TmuxCompat { args, cwd } => {
                let ui = GuiTmuxUi {
                    bridge: &self.bridge,
                };
                let out =
                    flowmux_daemon::tmux_compat::execute(self.inner.store(), &ui, &args, &cwd)
                        .await;
                Response::TmuxCompatResult(out)
            }
            Request::ClaudeTeams { count, args, root } => {
                let count = count.clamp(1, 8);
                let store = self.inner.store();
                // Create a fresh workspace.
                let ws_id = store
                    .create_workspace(Some("claude-teams".into()), root)
                    .await;

                if let Err(error) = materialize_workspace(&self.bridge, ws_id).await {
                    store.remove_workspace(ws_id).await;
                    return Response::Error(RpcError::Internal(error));
                }

                // Split the root pane (count - 1) times to get `count` panes.
                let ws = match store.get_workspace(ws_id).await {
                    Some(w) => w,
                    None => {
                        return Response::Error(RpcError::Internal("workspace vanished".into()))
                    }
                };
                // First leaf id from the root pane:
                let mut leaves = vec![];
                if let Some(s) = ws.surfaces.first() {
                    s.root_pane.for_each_leaf(|id| leaves.push(id));
                }
                let mut current = match leaves.first().copied() {
                    Some(id) => id,
                    None => {
                        return Response::Error(RpcError::Internal(
                            "workspace had no leaves".into(),
                        ))
                    }
                };
                let mut all_panes = vec![current];
                for i in 1..count {
                    let dir = if i % 2 == 1 {
                        SplitDirection::Vertical
                    } else {
                        SplitDirection::Horizontal
                    };
                    match split_pane(&self.bridge, current, dir).await {
                        Ok(new_pane) => {
                            all_panes.push(new_pane);
                            current = new_pane;
                        }
                        Err(error) => {
                            return Response::Error(RpcError::Internal(format!(
                                "workspace {ws_id} partially created; split failed: {error}; inspect the workspace before retrying"
                            )))
                        }
                    }
                }
                // Feed the `claude` invocation into each pane.
                let cmd_line = std::iter::once("claude".to_string())
                    .chain(args.iter().cloned())
                    .map(shell_escape)
                    .collect::<Vec<_>>()
                    .join(" ");
                let cmd_line = format!("{cmd_line}\n");
                let ui = GuiTmuxUi {
                    bridge: &self.bridge,
                };
                for (delivered, pane) in all_panes.iter().enumerate() {
                    if let Err(error) = ui.send_keys(*pane, &cmd_line).await {
                        return Response::Error(RpcError::Internal(format!(
                            "workspace {ws_id} partially created; input failed for pane {pane} after {delivered} confirmed deliveries: {error}; inspect the workspace before retrying"
                        )));
                    }
                }
                Response::WorkspaceCreated { id: ws_id }
            }
            Request::AgentSessionUpdate {
                agent,
                surface,
                session_id,
            } => match self.session_store.as_ref() {
                Some(store) => match store.record(&agent, surface, &session_id) {
                    Ok(()) => Response::Ok,
                    Err(e) => Response::Error(RpcError::Io(e.to_string())),
                },
                None => Response::Error(RpcError::Internal(
                    "XDG data dir unavailable; cannot persist agent session".into(),
                )),
            },
            Request::AgentSurfaceResolve {
                agent,
                session_id,
                cwd,
                session_start,
            } => {
                let store = self.inner.store();
                let mut tab = Ok(None);
                if agent.eq_ignore_ascii_case("codex") {
                    tab = store.codex_session_tab(&session_id, None).await;
                    if matches!(tab, Ok(None)) {
                        let id = session_id.clone();
                        // Only native SessionStart may use cwd before the first
                        // transcript write; title-job Stop must still prove a root session.
                        let thread = tokio::task::spawn_blocking(move || {
                            let home = SessionAgent::Codex.env_history_home()?;
                            let cwd = flowmux_state::session_history::codex_session_start_cwd(
                                &home,
                                &id,
                                cwd.filter(|_| session_start),
                            )?;
                            let names = codex_thread_names(&home).into_iter().collect();
                            Some((names, cwd))
                        });
                        if let Some((names, cwd)) = thread.await.ok().flatten() {
                            tab = store
                                .codex_session_tab(&session_id, Some((&names, &cwd)))
                                .await;
                        }
                    }
                }
                match tab {
                    Ok(Some((pane, surface, session_id))) => Response::AgentSurface {
                        pane,
                        surface,
                        session_id,
                    },
                    Err(exact_session) => Response::AgentSurfaceAmbiguous { exact_session },
                    Ok(None) => Response::Error(RpcError::NotFound(format!(
                        "no tab runs {agent} session {session_id}"
                    ))),
                }
            }
            Request::AgentSessionGet { agent, surface } => match self.session_store.as_ref() {
                Some(store) => Response::AgentSession {
                    session_id: store.lookup(&agent, surface),
                },
                None => Response::AgentSession { session_id: None },
            },
            Request::AgentSessionForget { agent, surface } => {
                let live_claude_session = agent.eq_ignore_ascii_case("claude")
                    && self
                        .inner
                        .store()
                        .located_agent_presence(surface)
                        .await
                        .is_some_and(|located| located.presence.name.eq_ignore_ascii_case(&agent));
                if live_claude_session {
                    Response::Ok
                } else {
                    match self.session_store.as_ref() {
                        Some(store) => match store.forget(&agent, surface) {
                            Ok(()) => Response::Ok,
                            Err(e) => Response::Error(RpcError::Io(e.to_string())),
                        },
                        None => Response::Ok,
                    }
                }
            }

            Request::AgentLifecycleUpdate {
                pane,
                surface,
                agent,
                pid,
                seq,
                session_id,
                lifecycle,
            } => {
                let surface_visible = query_agent_surface_visible(&self.bridge, surface).await;
                let mut outcome = self
                    .inner
                    .store()
                    .report_agent_lifecycle_with_visibility(
                        surface,
                        &agent,
                        pid,
                        seq,
                        &session_id,
                        lifecycle,
                        surface_visible,
                    )
                    .await;
                if let Some(settlement) = outcome.settle_codex_after_grace.take() {
                    tokio::time::sleep(CODEX_STOP_INGRESS_GRACE).await;
                    let surface_visible = query_agent_surface_visible(&self.bridge, surface).await;
                    outcome = self
                        .inner
                        .store()
                        .settle_codex_turn_after_grace(
                            surface,
                            pid,
                            settlement.stop_seq,
                            &session_id,
                            &settlement.turn_id,
                            surface_visible,
                        )
                        .await;
                }
                if let Some(ws_id) = outcome.workspace {
                    if let Some(store) = self.session_store.as_ref() {
                        if let Err(error) = store.record(&agent, surface, &session_id) {
                            warn!(%surface, %agent, %error, "failed to persist agent session");
                        }
                    }
                    let _ = self
                        .bridge
                        .send(GtkCommand::SetAgentStatus { workspace: ws_id })
                        .await;
                    if let Some(located) = self.inner.store().located_agent_presence(surface).await
                    {
                        if let Some(entry) = ActivityEntry::from_hook_presence(located) {
                            let _ = self.bridge.send(GtkCommand::AddActivity { entry }).await;
                        }
                    }
                }
                if outcome.completed {
                    let display_agent = if agent.eq_ignore_ascii_case("codex") {
                        "Codex"
                    } else if agent.eq_ignore_ascii_case("claude") {
                        "Claude"
                    } else {
                        agent.as_str()
                    };
                    let body = outcome
                        .completion_message
                        .filter(|message| !message.trim().is_empty())
                        .unwrap_or_else(|| "task complete".into());
                    let _ = self
                        .handle_notification_verb(Request::Notify {
                            pane,
                            surface: Some(surface),
                            title: format!("{display_agent} ready"),
                            body,
                            level: flowmux_core::NotificationLevel::TurnCompleted,
                        })
                        .await;
                }
                Response::Ok
            }

            // ---- Live agent activity (Running / NeedsInput / Idle).
            // Hooks pass FLOWMUX_SURFACE_ID, so a surface is expected;
            // without one we can't route the presence to a tab.
            Request::AgentActivityUpdate {
                surface,
                agent,
                status,
                activity,
                pid,
                source,
                seq,
                message,
                custom_status,
                session_id,
                session_name,
                messaging_socket,
                ..
            } => match surface {
                Some(surface) => {
                    if status.is_none() && activity.is_none() {
                        if let Some(removed) = self
                            .inner
                            .store()
                            .end_agent_session(surface, &agent, seq, session_id.as_deref(), pid)
                            .await
                        {
                            let ws_id = removed.workspace;
                            let _ = self
                                .bridge
                                .send(GtkCommand::AddActivity {
                                    entry: ActivityEntry::session_ended(
                                        removed,
                                        source.as_deref().unwrap_or("flowmux:hook"),
                                    ),
                                })
                                .await;
                            let _ = self
                                .bridge
                                .send(GtkCommand::SetAgentStatus { workspace: ws_id })
                                .await;
                        }
                    } else {
                        let surface_visible =
                            query_agent_surface_visible(&self.bridge, surface).await;
                        let record_activity = source.as_deref() == Some("flowmux:hook");
                        let session_binding = session_id.clone();
                        let session_agent = agent.clone();
                        let report = AgentStatusReport {
                            name: agent,
                            status,
                            activity,
                            pid,
                            source,
                            seq,
                            message,
                            custom_status,
                            session_id,
                            session_name,
                            messaging_socket,
                        };
                        if let Some((ws_id, _)) = self
                            .inner
                            .store()
                            .report_agent_status_with_visibility(surface, report, surface_visible)
                            .await
                        {
                            if let Some(session_id) = session_binding.as_deref() {
                                if let Some(store) = self.session_store.as_ref() {
                                    if let Err(error) =
                                        store.record(&session_agent, surface, session_id)
                                    {
                                        warn!(%surface, agent = %session_agent, %error, "failed to persist agent session");
                                    }
                                }
                            }
                            let _ = self
                                .bridge
                                .send(GtkCommand::SetAgentStatus { workspace: ws_id })
                                .await;
                            if record_activity {
                                if let Some(located) =
                                    self.inner.store().located_agent_presence(surface).await
                                {
                                    if let Some(entry) = ActivityEntry::from_hook_presence(located)
                                    {
                                        let _ = self
                                            .bridge
                                            .send(GtkCommand::AddActivity { entry })
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                    Response::Ok
                }
                None => Response::Ok,
            },
            other => unreachable!("agent router got a non-agent verb: {other:?}"),
        }
    }

    /// Dispatch for the notification verb group (split out of the `handle` match).
    async fn handle_notification_verb(&self, req: Request) -> Response {
        match req {
            Request::Notify {
                pane,
                surface,
                ref title,
                ref body,
                level,
            } => {
                // Try to recover pane/surface from the pane title
                // when the hook source could not pass them. The
                // Flatpak OpenCode plugin path is the trigger: the
                // host->sandbox transition strips env, so the
                // in-sandbox CLI cannot read FLOWMUX_PANE_ID and
                // the request lands with `pane=None surface=None`.
                // The pane that actually ran the agent already has
                // its tab title flipped to the agent name by
                // workspace_view's terminal title hook, so we can
                // resolve back to it by matching on the first
                // whitespace-delimited token of `title` (e.g.
                // "OpenCode" out of "OpenCode ready"). When both
                // pane and surface arrive as None, this fallback
                // is the only path that lets `mark_attention`
                // (sidebar blink) and `focus_pane` (bell-click
                // navigation) work for the Flatpak hook.
                let (resolved_pane, resolved_surface, fallback_used) =
                    if pane.is_none() && surface.is_none() {
                        let needle = title.split_whitespace().next().unwrap_or("");
                        match self
                            .inner
                            .store()
                            .find_pane_by_active_title_prefix(needle)
                            .await
                        {
                            Some((_ws_id, p, s)) => (Some(p), Some(s), true),
                            None => (pane, surface, false),
                        }
                    } else {
                        (pane, surface, false)
                    };
                // Pre-resolve the workspace here so the GTK side
                // can route the click without a second store
                // lookup (the dispatcher still falls back to a
                // late lookup if it sees `workspace = None`).
                let workspace = match resolved_pane {
                    Some(p) => self.inner.store().workspace_for_pane(p).await,
                    None => None,
                };
                tracing::info!(
                    ?pane,
                    ?surface,
                    resolved_pane = ?resolved_pane,
                    resolved_surface = ?resolved_surface,
                    fallback_used,
                    ?workspace,
                    title = %title,
                    ?level,
                    "Notify request received — routing to GTK"
                );
                let pane = resolved_pane;
                let surface = resolved_surface;
                // Ask the GTK side to record the entry. The ack
                // returns `None` when the source pane+surface is
                // already focused — in that case we also skip the
                // desktop toast so flowmux stays out of the way.
                // `Some(entry_id)` is the in-process popover id we
                // need so we can later attach the gtk notifications
                // id returned by the daemon.
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .send(GtkCommand::AddNotification {
                        pane,
                        surface,
                        workspace,
                        title: title.clone(),
                        body: body.clone(),
                        level,
                        ack: tx,
                    })
                    .await;
                let entry_id = rx.await.unwrap_or(None);
                if let Some(entry_id) = entry_id {
                    let resp = self
                        .inner
                        .send_notification(entry_id, pane, title.clone(), body.clone(), level)
                        .await;
                    // Forward the desktop id (when present) to the
                    // GUI store so the bell popover's "mark all
                    // read" sweep can later ask the FDO daemon to
                    // close the toast and shrink the dock badge.
                    if let Response::Notified {
                        desktop_id: Some(ref desktop_id),
                    } = resp
                    {
                        let _ = self
                            .bridge
                            .send(GtkCommand::SetNotificationDesktopId {
                                id: entry_id,
                                desktop_id: desktop_id.clone(),
                            })
                            .await;
                    }
                    // Hooks and other CLI callers expect a benign
                    // success — collapse Notified into Ok so the
                    // wire shape stays stable for them.
                    match resp {
                        Response::Notified { .. } => Response::Ok,
                        other => other,
                    }
                } else {
                    Response::Ok
                }
            }
            Request::NotificationsList { unread_only } => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .send(GtkCommand::ListNotifications {
                        unread_only,
                        ack: tx,
                    })
                    .await;
                match rx.await {
                    Ok((entries, unread_count)) => Response::Notifications {
                        entries,
                        unread_count,
                    },
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::NotificationOpen { id } => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .tx
                    .send(GtkCommand::OpenNotificationWithAck { id, ack: tx })
                    .await;
                match rx.await {
                    Ok(changed) => Response::NotificationState { changed },
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::NotificationJumpToUnread => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .tx
                    .send(GtkCommand::OpenOldestUnreadNotification { ack: tx })
                    .await;
                match rx.await {
                    Ok(changed) => Response::NotificationState { changed },
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::NotificationMarkRead { id } => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .tx
                    .send(GtkCommand::MarkNotificationRead { id, ack: tx })
                    .await;
                match rx.await {
                    Ok(changed) => Response::NotificationState { changed },
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }
            Request::NotificationsClear => {
                let (tx, rx) = oneshot::channel();
                let _ = self
                    .bridge
                    .tx
                    .send(GtkCommand::ClearNotifications { ack: tx })
                    .await;
                match rx.await {
                    Ok(changed) => Response::NotificationState { changed },
                    Err(_) => Response::Error(RpcError::Internal("bridge closed".into())),
                }
            }

            // Everything else is fully GUI-independent: ping, list, and
            // notification operations delegated above.
            other => unreachable!("notification router got a non-notification verb: {other:?}"),
        }
    }
}

/// Bridge-backed [`TmuxCompatUi`]: applies each tmux-compat side effect
/// on the GTK main thread, mirroring how the plain pane verbs dispatch.
///
/// [`TmuxCompatUi`]: flowmux_daemon::tmux_compat::TmuxCompatUi
struct GuiTmuxUi<'a> {
    bridge: &'a Bridge,
}

impl flowmux_daemon::tmux_compat::TmuxCompatUi for GuiTmuxUi<'_> {
    async fn workspace_created(
        &self,
        id: flowmux_core::WorkspaceId,
        _name: &str,
        _root: &std::path::Path,
    ) -> Result<(), String> {
        materialize_workspace(self.bridge, id).await
    }

    async fn split_pane(
        &self,
        pane: flowmux_core::PaneId,
        direction: SplitDirection,
    ) -> Result<flowmux_core::PaneId, String> {
        split_pane(self.bridge, pane, direction).await
    }

    async fn send_keys(&self, pane: flowmux_core::PaneId, keys: &str) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .bridge
            .tx
            .send(GtkCommand::PaneSendKeys {
                pane,
                keys: keys.to_string(),
                ack: tx,
            })
            .await;
        match rx.await {
            Ok(result) => result,
            Err(_) => Err("bridge closed".into()),
        }
    }

    async fn rename_surface(
        &self,
        pane: flowmux_core::PaneId,
        surface: flowmux_core::SurfaceId,
        title: &str,
    ) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .bridge
            .tx
            .send(GtkCommand::RenameSurface {
                pane,
                surface,
                title: title.to_string(),
                ack: tx,
            })
            .await;
        match rx.await {
            Ok(result) => result,
            Err(_) => Err("bridge closed".into()),
        }
    }

    async fn close_pane(&self, pane: flowmux_core::PaneId) -> Result<(), String> {
        // The orchestrator closes non-last panes; dirty editors can still prompt.
        let (tx, rx) = oneshot::channel();
        let _ = self
            .bridge
            .tx
            .send(GtkCommand::CloseFocused { pane, ack: tx })
            .await;
        match rx.await {
            Ok(result) => result,
            Err(_) => Err("bridge closed".into()),
        }
    }

    async fn remove_workspace(&self, id: flowmux_core::WorkspaceId) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.bridge
            .tx
            .send(GtkCommand::RemoveWorkspace {
                id,
                // Skip workspace confirmation; dirty-editor checks still apply.
                confirm: false,
                ack: tx,
            })
            .await
            .map_err(|_| "bridge closed".to_string())?;
        rx.await.map_err(|_| "bridge closed".to_string())?
    }

    async fn workspace_activated(&self, id: flowmux_core::WorkspaceId) -> Result<(), String> {
        activate_workspace(self.bridge, id).await
    }
}

#[cfg(test)]
#[path = "ipc_handler_tests.rs"]
mod tests;
