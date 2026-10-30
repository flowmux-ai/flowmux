// SPDX-License-Identifier: GPL-3.0-or-later
//! Session Flow projection and native launch coordination; no workspace-owned lineage.

use super::sessions::{session_home, FocusedSession, SessionHome, SessionTarget};
use super::*;
use crate::ui::session_flow::{FlowAction, FlowPanel, Node};
use flowmux_core::session_flow::SessionKey;
use flowmux_core::AgentStatus;
use flowmux_state::session_flow::{import_native, session_key, FlowStore, Snapshot};
use flowmux_state::session_history::{HistorySession, SessionAgent};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Anchor {
    workspace: WorkspaceId,
    target: SessionTarget,
}

struct Catalog {
    snapshot: Snapshot,
    available: BTreeMap<SessionKey, HistorySession>,
    resumable: BTreeSet<SessionKey>,
    homes: BTreeMap<SessionKey, SessionHome>,
    bindings: HashMap<SurfaceId, SessionKey>,
    forkable: BTreeSet<SessionKey>,
    views: BTreeMap<uuid::Uuid, flowmux_state::session_flow::SavedView>,
}

#[derive(Clone)]
pub(super) struct FlowState {
    pub panel: FlowPanel,
    catalog: Rc<RefCell<Option<Catalog>>>,
    targets: Rc<RefCell<Vec<Anchor>>>,
    loading: Rc<Cell<bool>>,
    last_load: Rc<Cell<Option<Instant>>>,
    pub owner: uuid::Uuid,
}

impl FlowState {
    pub fn new(panel: FlowPanel) -> Self {
        Self {
            panel,
            catalog: Rc::new(RefCell::new(None)),
            targets: Rc::new(RefCell::new(Vec::new())),
            loading: Rc::new(Cell::new(false)),
            last_load: Rc::new(Cell::new(None)),
            owner: uuid::Uuid::new_v4(),
        }
    }
}

/// Help is queried outside any Flowmux pane context, with a bounded child lifetime.
/// This probes the resolved CLI rather than assuming every installed version supports a flag.
fn probe_agent(agent: SessionAgent, home: &SessionHome) -> (bool, bool) {
    use std::process::{Command, Stdio};
    let (command, resume, fork) = match agent {
        SessionAgent::Claude => ("claude --help", "--resume", Some("--fork-session")),
        SessionAgent::Codex => ("codex --help", "resume", Some("fork")),
        SessionAgent::OpenCode => ("opencode --help", "--session", Some("--fork")),
        SessionAgent::Antigravity => ("agy --help", "--conversation", None),
        SessionAgent::Gemini => ("gemini --help", "--resume", None),
        SessionAgent::Cline => ("cline --help", "--id", None),
    };
    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    // Use a file so a verbose helper cannot fill a pipe and deadlock before wait.
    let Ok(output) = tempfile::tempfile() else {
        return (false, false);
    };
    let Ok(stdout) = output.try_clone() else {
        return (false, false);
    };
    let mut cmd = Command::new(shell);
    let environment = home
        .environment
        .iter()
        .map(|(key, value)| shell_quote(&format!("{key}={value}")))
        .collect::<Vec<_>>()
        .join(" ");
    let command = format!("env {environment} {command}");
    cmd.args(["-lc", &command])
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(Stdio::null());
    cmd.envs(&home.environment);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("FLOWMUX_") {
            cmd.env_remove(key);
        }
    }
    let Ok(mut child) = cmd.spawn() else {
        return (false, false);
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return (false, false);
                }
                use std::io::{Read, Seek};
                let mut output = output;
                let _ = output.rewind();
                let mut text = String::new();
                if output.take(256 * 1024).read_to_string(&mut text).is_err() {
                    return (false, false);
                }
                return (
                    text.contains(resume),
                    fork.is_some_and(|token| text.contains(token)),
                );
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return (false, false);
            }
        }
    }
}

fn load_catalog(targets: &[Anchor]) -> Result<Catalog, String> {
    let mut store = FlowStore::open(&FlowStore::default_path().map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut catalog = Catalog {
        snapshot: Snapshot::default(),
        available: BTreeMap::new(),
        resumable: BTreeSet::new(),
        homes: BTreeMap::new(),
        bindings: HashMap::new(),
        forkable: BTreeSet::new(),
        views: store.views().map_err(|e| e.to_string())?,
    };
    let mut namespaces = BTreeSet::new();
    for anchor in targets {
        let home = session_home(&anchor.target)?;
        let focused = anchor
            .target
            .focused
            .as_ref()
            .ok_or("Session Flow requires a live source")?;
        let id = focused
            .session_id
            .as_ref()
            .ok_or("Native session ID is unknown")?;
        let key = session_key(anchor.target.agent, &home.path, id).map_err(|e| e.to_string())?;
        catalog.bindings.insert(focused.surface, key.clone());
        if namespaces.insert((key.provider.clone(), key.home.clone())) {
            let native = import_native(&mut store, anchor.target.agent, &home.path)
                .map_err(|e| e.to_string())?;
            let (resumable, forkable) = probe_agent(anchor.target.agent, &home);
            for (native_key, session) in native {
                if resumable
                    && session.cwd.is_dir()
                    && session.path.is_file()
                    && session.resume_command().is_ok()
                {
                    catalog.resumable.insert(native_key.clone());
                }
                if forkable && session.fork_command().is_ok() {
                    catalog.forkable.insert(native_key.clone());
                }
                catalog.homes.insert(native_key.clone(), home.clone());
                catalog.available.insert(native_key, session);
            }
        }
    }
    catalog.snapshot = store.snapshot().map_err(|e| e.to_string())?;
    Ok(catalog)
}

impl WindowController {
    async fn flow_anchors(&self) -> Vec<Anchor> {
        let terminals: Vec<_> = self
            .pane_registry
            .borrow()
            .terminals
            .iter()
            .filter_map(|(surface, terminal)| {
                let registry = self.pane_registry.borrow();
                let pane = terminal.id();
                if registry.is_ssh_pane(pane) {
                    return None;
                }
                Some((
                    registry.workspace_of_pane(pane)?,
                    pane,
                    *surface,
                    terminal.pid.get()? as u32,
                ))
            })
            .collect();
        let mut anchors = Vec::new();
        for (workspace, pane, surface, root_pid) in terminals {
            if let Some(located) = self.store.located_agent_presence(surface).await {
                if let (Some(agent), Some(session_id)) = (
                    SessionAgent::from_name(&located.presence.name),
                    located.presence.session_id,
                ) {
                    anchors.push(Anchor {
                        workspace,
                        target: SessionTarget {
                            agent,
                            focused: Some(FocusedSession {
                                pane,
                                surface,
                                root_pid,
                                session_id: Some(session_id),
                            }),
                        },
                    });
                }
            }
        }
        anchors.sort_by_key(|a| a.target.focused.as_ref().map(|f| f.surface.0));
        anchors
    }

    pub(super) async fn refresh_session_flow(&self, force: bool) {
        let anchors = self.flow_anchors().await;
        self.render_session_flow(&anchors).await;
        let recent = self
            .flow
            .last_load
            .get()
            .is_some_and(|time| time.elapsed() < Duration::from_secs(30));
        if self.flow.loading.get() || (!force && recent && *self.flow.targets.borrow() == anchors) {
            return;
        }
        *self.flow.targets.borrow_mut() = anchors.clone();
        self.flow.loading.set(true);
        self.flow.last_load.set(Some(Instant::now()));
        self.flow
            .panel
            .status
            .set_text("Reading session relationships…");
        let controller = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let worker_anchors = anchors.clone();
            let result = gtk::gio::spawn_blocking(move || load_catalog(&worker_anchors)).await;
            controller.flow.loading.set(false);
            if controller.flow_anchors().await != anchors {
                controller.flow.targets.borrow_mut().clear();
                let _ = controller
                    .bridge
                    .tx
                    .try_send(GtkCommand::SessionFlow(FlowAction::Refresh));
                return;
            }
            match result {
                Ok(Ok(catalog)) => {
                    *controller.flow.catalog.borrow_mut() = Some(catalog);
                    controller.render_session_flow(&anchors).await;
                }
                Ok(Err(error)) => controller
                    .flow
                    .panel
                    .status
                    .set_text(&format!("Session Flow unavailable: {error}")),
                Err(_) => controller
                    .flow
                    .panel
                    .status
                    .set_text("Session Flow history reader failed. Refresh to retry."),
            }
        });
    }

    async fn render_session_flow(&self, anchors: &[Anchor]) {
        let active = self.store.active_workspace().await;
        let mut idle = HashSet::new();
        for anchor in anchors {
            if let Some(focused) = &anchor.target.focused {
                if self
                    .store
                    .located_agent_presence(focused.surface)
                    .await
                    .is_some_and(|p| {
                        matches!(p.presence.status, AgentStatus::Idle | AgentStatus::Done)
                    })
                {
                    idle.insert(focused.surface);
                }
            }
        }
        let catalog = self.flow.catalog.borrow();
        let Some(catalog) = catalog.as_ref() else {
            return;
        };
        let mut workspace_anchors: HashMap<WorkspaceId, BTreeSet<SessionKey>> = HashMap::new();
        let mut running = BTreeSet::new();
        let mut forkable = HashSet::new();
        for anchor in anchors {
            let Some(focused) = &anchor.target.focused else {
                continue;
            };
            let Some(key) = catalog
                .bindings
                .get(&focused.surface)
                .filter(|k| Some(&k.id) == focused.session_id.as_ref())
            else {
                continue;
            };
            workspace_anchors
                .entry(anchor.workspace)
                .or_default()
                .insert(key.clone());
            running.insert(key.clone());
            if idle.contains(&focused.surface) && catalog.forkable.contains(key) {
                forkable.insert(focused.surface);
            }
        }
        self.pane_registry.borrow_mut().session_flow_forkable = forkable;
        for (id, _) in self.sidebar.workspace_titles().borrow().iter() {
            let keys = workspace_anchors.get(id).cloned().unwrap_or_default();
            self.sidebar
                .set_flow_available(*id, !catalog.snapshot.lineage.project(&keys).is_empty());
        }
        let keys = active
            .and_then(|id| workspace_anchors.get(&id).cloned())
            .unwrap_or_default();
        let graph = catalog.snapshot.lineage.project(&keys);
        let nodes = catalog
            .snapshot
            .lineage
            .nodes()
            .into_iter()
            .map(|key| {
                let record = catalog.snapshot.records.get(key);
                let available = running.contains(key) || catalog.resumable.contains(key);
                let title = record
                    .map(|r| r.title.clone())
                    .unwrap_or_else(|| key.id.clone());
                let detail = format!(
                    "{} · {} · {}",
                    key.provider,
                    title,
                    if running.contains(key) {
                        "Open session · select to focus"
                    } else if available {
                        "Select to resume in a right split"
                    } else {
                        "Cannot resume: native history, project directory, or CLI support is unavailable"
                    }
                );
                (
                    key.clone(),
                    Node {
                        title,
                        detail,
                        available,
                        running: running.contains(key),
                    },
                )
            })
            .collect();
        self.flow.panel.restore_views(&catalog.views);
        self.flow.panel.set_nodes(
            catalog.snapshot.lineage.clone(),
            nodes,
            &catalog.snapshot.positions,
        );
        self.flow.panel.status.set_text(&format!(
            "{} sessions · scroll to zoom · drag nodes or space",
            graph.nodes().len()
        ));
        self.flow.panel.show_workspace(active, graph, keys);
    }

    fn save_flow_view(&self) {
        let positions = self.flow.panel.positions();
        let views = self.flow.panel.saved_views();
        let status = self.flow.panel.status.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let result = gtk::gio::spawn_blocking(move || -> std::io::Result<()> {
                let mut store = FlowStore::open(&FlowStore::default_path()?)?;
                store.save_positions(&positions)?;
                store.save_views(&views)
            })
            .await;
            if !matches!(result, Ok(Ok(()))) {
                if let Some(status) = status.upgrade() {
                    status.set_text("Session Flow view could not be saved");
                }
            }
        });
    }

    pub(super) async fn dispatch_session_flow(&self, action: FlowAction) {
        match action {
            FlowAction::Refresh => self.refresh_session_flow(true).await,
            FlowAction::SaveView => self.save_flow_view(),
            FlowAction::Toggle(workspace) => {
                if self.store.active_workspace().await != Some(workspace) {
                    self.activate_workspace(workspace).await;
                }
                self.refresh_session_flow(false).await;
                self.flow.panel.toggle(workspace);
                self.save_flow_view();
                if !self.flow.panel.root.is_visible() {
                    if let Some(pane) = self.focused_pane.get() {
                        self.focus_pane(pane);
                    }
                }
            }
            FlowAction::Open(key) => self.open_flow_session(key, None).await,
            FlowAction::Fork { pane, surface } => {
                let key = self
                    .flow
                    .catalog
                    .borrow()
                    .as_ref()
                    .and_then(|c| c.bindings.get(&surface).cloned());
                if let Some(key) = key {
                    self.open_flow_session(key, Some((pane, surface))).await;
                }
            }
        }
    }
}

impl WindowController {
    async fn open_flow_session(&self, key: SessionKey, fork_source: Option<(PaneId, SurfaceId)>) {
        if let Err(error) = self.launch_flow_session(key, fork_source).await {
            self.flow.panel.status.set_text(&error);
            self.clipboard_toast.show_with_message(&error);
        }
    }

    async fn launch_flow_session(
        &self,
        key: SessionKey,
        fork_source: Option<(PaneId, SurfaceId)>,
    ) -> Result<(), String> {
        use flowmux_state::session_flow::{Operation, OperationKind};
        let anchors = self.flow_anchors().await;
        if fork_source.is_none() {
            let live = {
                let catalog = self.flow.catalog.borrow();
                anchors.iter().find_map(|anchor| {
                    let focused = anchor.target.focused.as_ref()?;
                    let matches = catalog.as_ref()?.bindings.get(&focused.surface) == Some(&key)
                        && focused.session_id.as_ref() == Some(&key.id);
                    matches.then_some((anchor.workspace, focused.pane, focused.surface))
                })
            };
            if let Some((workspace, pane, surface)) = live {
                self.activate_workspace_at(workspace, Some((pane, Some(surface))))
                    .await;
                return Ok(());
            }
        }
        let (session, home) = {
            let catalog = self.flow.catalog.borrow();
            let catalog = catalog.as_ref().ok_or("Session history is still loading")?;
            let session = catalog
                .available
                .get(&key)
                .cloned()
                .ok_or("Native session history is unavailable")?;
            let home = catalog
                .homes
                .get(&key)
                .cloned()
                .ok_or("Session store is unavailable")?;
            if fork_source.is_some() && !catalog.forkable.contains(&key) {
                return Err("This agent's installed CLI does not support external fork".into());
            }
            (session, home)
        };
        if !session.cwd.is_dir() || !session.path.is_file() {
            return Err("Session history or project directory is no longer available".into());
        }
        let source = match fork_source {
            Some(source) => source,
            None => {
                let pane = self
                    .focused_pane
                    .get()
                    .ok_or("Focus a terminal pane to resume this session")?;
                let registry = self.pane_registry.borrow();
                if registry.is_ssh_pane(pane) || registry.active_terminal(pane).is_none() {
                    return Err("Focus a local terminal pane to resume this session".into());
                }
                (
                    pane,
                    registry
                        .active_surface(pane)
                        .ok_or("Focused terminal was closed")?,
                )
            }
        };
        let workspace = self
            .pane_registry
            .borrow()
            .workspace_of_pane(source.0)
            .ok_or("Target workspace was closed")?;
        if self.store.active_workspace().await != Some(workspace) {
            return Err("Target workspace changed; select the session again".into());
        }
        if fork_source.is_some() {
            let presence = self
                .store
                .located_agent_presence(source.1)
                .await
                .ok_or("Fork source is no longer running")?
                .presence;
            if SessionAgent::from_name(&presence.name) != Some(session.agent)
                || presence.session_id.as_deref() != Some(&key.id)
                || !matches!(presence.status, AgentStatus::Idle | AgentStatus::Done)
            {
                return Err("Wait for this session's current turn to finish before forking".into());
            }
        }
        let line = sessions::session_shell_line(&session, &home, false, fork_source.is_some())?;
        let surface = SurfaceId::new();
        let operation = Operation {
            request: uuid::Uuid::new_v4(),
            owner: self.flow.owner,
            owner_pid: std::process::id(),
            source: key.clone(),
            kind: if fork_source.is_some() {
                OperationKind::Fork
            } else {
                OperationKind::Resume
            },
            target_surface: surface.0,
        };
        let reserve = operation.clone();
        let reserved = gtk::gio::spawn_blocking(move || {
            let mut store = FlowStore::open(&FlowStore::default_path()?)?;
            store.begin(&reserve)
        })
        .await
        .map_err(|_| "Session Flow reservation worker failed")?
        .map_err(|e| format!("Cannot reserve this session: {e}"))?;
        if !reserved {
            return Err("This session operation has already been requested".into());
        }
        let outcome = async {
            if self.store.active_workspace().await != Some(workspace)
                || (fork_source.is_none() && self.focused_pane.get() != Some(source.0))
            {
                return Err("Target changed while preparing the session".into());
            }
            self.clear_pane_zoom();
            let (workspace, pane) = self
                .store
                .split_session_pane(
                    source.0,
                    source.1,
                    fork_source.map(|_| key.id.as_str()),
                    surface,
                    session.cwd.clone(),
                )
                .await?;
            #[cfg(target_os = "macos")]
            self.pane_registry
                .borrow_mut()
                .pending_terminal_commands
                .insert(surface, line.clone());
            if let Err(error) = self
                .apply_split_incremental_or_rerender(
                    workspace,
                    source.0,
                    pane,
                    SplitDirection::Vertical,
                )
                .await
            {
                #[cfg(target_os = "macos")]
                self.pane_registry
                    .borrow_mut()
                    .pending_terminal_commands
                    .remove(&surface);
                return Err(error);
            }
            #[cfg(not(target_os = "macos"))]
            {
                let terminal = self
                    .pane_registry
                    .borrow()
                    .terminals
                    .get(&surface)
                    .cloned()
                    .ok_or("New session terminal is unavailable")?;
                terminal
                    .write_input(format!("{line}\r").as_bytes())
                    .map_err(|e| e.to_string())?;
            }
            self.focus_pane(pane);
            Ok(pane)
        }
        .await;
        let pane = match outcome {
            Ok(pane) => pane,
            Err(error) => {
                let failed = operation.clone();
                let _ = gtk::gio::spawn_blocking(move || {
                    let mut store = FlowStore::open(&FlowStore::default_path()?)?;
                    store.fail(&failed)
                })
                .await;
                return Err(error);
            }
        };
        self.flow
            .panel
            .status
            .set_text("Opening session · waiting for its native identity…");
        let controller = self.clone();
        glib::MainContext::default().spawn_local(async move {
            for _ in 0..30 {
                glib::timeout_future(Duration::from_secs(1)).await;
                if !controller.window.is_visible() { return; }
                let Some(located) = controller.store.located_agent_presence(surface).await else { continue; };
                if located.pane != pane || SessionAgent::from_name(&located.presence.name) != Some(session.agent) { continue; }
                let Some(id) = located.presence.session_id else { continue; };
                let observed = SessionKey { id, ..key.clone() };
                let operation = operation.clone(); let home = home.clone(); let native = observed.clone();
                let result = gtk::gio::spawn_blocking(move || -> Result<(), String> {
                    let native_sessions = flowmux_state::session_history::list_sessions(session.agent, &home.path).map_err(|e| e.to_string())?;
                    let child = native_sessions.iter().find(|s| s.id == native.id).ok_or("Native session record is not yet available")?;
                    if operation.kind == OperationKind::Fork && session.agent == SessionAgent::Codex && child.fork_parent().map_err(|e| e.to_string())?.as_deref() != Some(&operation.source.id) { return Err("Fork parent identity has not been confirmed".into()); }
                    let mut store = FlowStore::open(&FlowStore::default_path().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
                    store.import(&[(native.clone(), flowmux_state::session_flow::SessionRecord::from(child))], &[]).map_err(|e| e.to_string())?;
                    store.complete(&operation, &native).map_err(|e| e.to_string())
                }).await;
                if matches!(result, Ok(Ok(()))) {
                    let _ = controller.bridge.tx.try_send(GtkCommand::SessionFlow(FlowAction::Refresh));
                    return;
                }
            }
            controller.flow.panel.status.set_text("Session identity could not be confirmed. The operation remains reserved; inspect the new terminal before retrying.");
        });
        Ok(())
    }
}

#[cfg(all(target_os = "macos", feature = "native-smoke"))]
pub(super) async fn check_native(
    controller: &WindowController,
    workspace: WorkspaceId,
    pane: PaneId,
    root: &std::path::Path,
) {
    use flowmux_state::session_flow::SessionRecord;
    let source = controller
        .pane_registry
        .borrow()
        .active_surface(pane)
        .unwrap();
    let terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(pane)
        .unwrap()
        .clone();
    let pid = terminal.pid.get();
    let widget = terminal.widget.clone();
    let home = root.join("flow-history");
    std::fs::create_dir_all(home.join("sessions")).unwrap();
    let parent = uuid::Uuid::new_v4().to_string();
    let child = uuid::Uuid::new_v4().to_string();
    let other = uuid::Uuid::new_v4().to_string();
    let other_child = uuid::Uuid::new_v4().to_string();
    let deleted = uuid::Uuid::new_v4().to_string();
    let mut records = Vec::new();
    let mut available = BTreeMap::new();
    let mut homes = BTreeMap::new();
    for (id, title, fork) in [
        (&parent, "Explore architecture", None),
        (&child, "Improve the renderer", Some(&parent)),
        (&other, "Independent experiment", None),
        (&other_child, "Regression investigation", Some(&other)),
    ] {
        let path = home.join("sessions").join(format!("{id}.jsonl"));
        std::fs::write(&path, serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":root,"source":"cli","forked_from_id":fork}}).to_string()).unwrap();
        let key = session_key(SessionAgent::Codex, &home, id).unwrap();
        let session = HistorySession {
            agent: SessionAgent::Codex,
            id: id.clone(),
            title: title.into(),
            summary: String::new(),
            cwd: root.into(),
            modified: std::time::SystemTime::now(),
            path,
        };
        records.push((key.clone(), SessionRecord::from(&session)));
        homes.insert(
            key.clone(),
            SessionHome {
                path: home.clone(),
                environment: BTreeMap::new(),
                arguments: Vec::new(),
            },
        );
        available.insert(key, session);
    }
    let key = |id: &str| session_key(SessionAgent::Codex, &home, id).unwrap();
    let mut store = FlowStore::open(&FlowStore::default_path().unwrap()).unwrap();
    store
        .import(
            &records,
            &[
                (key(&parent), key(&child)),
                (key(&child), key(&deleted)),
                (key(&other), key(&other_child)),
            ],
        )
        .unwrap();
    let mut presence =
        flowmux_core::AgentPresence::new("Codex", flowmux_core::AgentActivity::Idle, None);
    presence.session_id = Some(child.clone());
    controller
        .store
        .set_agent_activity(source, Some(presence))
        .await
        .unwrap();
    let anchor = Anchor {
        workspace,
        target: SessionTarget {
            agent: SessionAgent::Codex,
            focused: Some(FocusedSession {
                pane,
                surface: source,
                root_pid: pid.unwrap() as u32,
                session_id: Some(child.clone()),
            }),
        },
    };
    *controller.flow.targets.borrow_mut() = vec![anchor.clone()];
    *controller.flow.catalog.borrow_mut() = Some(Catalog {
        snapshot: store.snapshot().unwrap(),
        resumable: available.keys().cloned().collect(),
        available,
        homes,
        bindings: [(source, key(&child))].into(),
        forkable: BTreeSet::new(),
        views: BTreeMap::new(),
    });
    controller
        .render_session_flow(std::slice::from_ref(&anchor))
        .await;
    controller.flow.panel.toggle(workspace);
    glib::timeout_future(Duration::from_millis(400)).await;
    assert!(controller.flow.panel.root.is_mapped());
    let ratio =
        controller.flow.panel.root.height() as f64 / controller.flow.panel.host.height() as f64;
    assert!(
        (0.20..0.48).contains(&ratio),
        "unexpected panel height ratio: {ratio}"
    );
    assert_eq!(
        controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .widget,
        widget
    );
    assert_eq!(terminal.pid.get(), pid);
    // Moving through another anchor of the same family must not change its geometry.
    let positions = controller.flow.panel.positions();
    let graph = store.snapshot().unwrap().lineage;
    assert_eq!(
        graph.project([&key(&parent)]),
        graph.project([&key(&child)])
    );
    controller.flow.panel.show_workspace(
        Some(workspace),
        graph.project([&key(&child), &key(&other_child)]),
        [key(&child), key(&other_child)].into(),
    );
    glib::timeout_future(Duration::from_millis(100)).await;
    assert_eq!(controller.flow.panel.positions().len(), positions.len());
    controller
        .flow
        .panel
        .check_native_interactions(&key(&child), &key(&deleted));
    controller.flow.panel.toggle(workspace);
    assert!(!controller.flow.panel.root.is_visible());
    assert_eq!(terminal.pid.get(), pid);
    assert_eq!(terminal.widget, widget);
    // Exercise the same incremental split path as a session launch, with a normal shell.
    let new_surface = SurfaceId::new();
    let (_, sibling) = controller
        .store
        .split_session_pane(pane, source, Some(&child), new_surface, root.into())
        .await
        .unwrap();
    controller
        .apply_split_incremental_or_rerender(workspace, pane, sibling, SplitDirection::Vertical)
        .await
        .unwrap();
    glib::timeout_future(Duration::from_millis(100)).await;
    assert_eq!(
        controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .widget,
        widget
    );
    assert_eq!(terminal.pid.get(), pid);
    assert!(controller
        .pane_registry
        .borrow()
        .terminals
        .contains_key(&new_surface));
    controller.store.close_pane(sibling).await;
    controller
        .apply_close_pane_incremental_or_rerender(workspace, sibling)
        .await;
    assert_eq!(
        controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .widget,
        widget
    );
    assert_eq!(terminal.pid.get(), pid);
    controller.flow.panel.toggle(workspace);
    glib::timeout_future(Duration::from_millis(300)).await;
    if let Ok(path) = std::env::var("FLOWMUX_SESSION_FLOW_CAPTURE") {
        let paintable = gtk::WidgetPaintable::new(Some(&controller.flow.panel.root));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(
            &snapshot,
            controller.flow.panel.root.width() as f64,
            controller.flow.panel.root.height() as f64,
        );
        if let Some(node) = snapshot.to_node() {
            let renderer = controller.window.renderer().unwrap();
            renderer
                .render_texture(&node, None)
                .save_to_png(path)
                .unwrap();
        }
    }
    println!("MACOS_SESSION_FLOW_LAYOUT_OK");
}
