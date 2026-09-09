// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace-owned OpenSSH master and its terminal channels. GTK owns all state;
//! asynchronous completions are scoped to a connection generation.
use super::*;
use crate::ui::pane_terminal::PaneTerminal;
use flowmux_core::{PaneSurface, SshForwardSpec, SshTarget, SshWorkspaceConfig};
use flowmux_ipc::protocol::SshRequest;
use gtk::gio;
use std::os::unix::fs::DirBuilderExt;

pub(crate) struct SshRuntime {
    config: SshWorkspaceConfig,
    generation: u64,
    state: &'static str,
    error: Option<String>,
    directory: PathBuf,
    socket: PathBuf,
    master: Option<PaneTerminal>,
    authentication: Option<gtk::Window>,
    launched: HashSet<SurfaceId>,
    tabs: HashMap<SurfaceId, String>,
    instances: HashMap<SurfaceId, uuid::Uuid>,
    reusable: HashMap<SurfaceId, PaneTerminal>,
    rendered_generation: Option<u64>,
    commands: HashMap<SurfaceId, Vec<String>>,
    forwards: HashMap<uuid::Uuid, u16>,
    request_id: Option<uuid::Uuid>,
    bridge: Bridge,
    previews: Vec<(uuid::Uuid, crate::ui::browser_pane::BrowserPane)>,
}

impl SshRuntime {
    pub(crate) fn preview_url(&self, binding: &str) -> Option<String> {
        // Only WebKitGTK currently blocks all navigation after binding expiry.
        if !cfg!(target_os = "linux") || self.state != "connected" {
            return None;
        }
        let id = binding
            .strip_prefix("flowmux-ssh-preview://")?
            .parse::<uuid::Uuid>()
            .ok()?;
        let spec = self.config.forwards.iter().find(|spec| spec.id == id)?;
        let port = self.forwards.get(&id)?;
        Some(format!(
            "{}://127.0.0.1:{port}",
            if spec.https { "https" } else { "http" }
        ))
    }

    pub(crate) fn register_preview(
        &mut self,
        binding: &str,
        pane: crate::ui::browser_pane::BrowserPane,
    ) {
        if let Some(id) = binding
            .strip_prefix("flowmux-ssh-preview://")
            .and_then(|id| id.parse().ok())
        {
            self.previews.push((id, pane));
        }
    }

    fn expire_preview(&mut self, forward: Option<uuid::Uuid>) {
        self.previews.retain(|(id, pane)| {
            if forward.is_some_and(|forward| forward != *id) {
                return true;
            }
            #[cfg(target_os = "linux")]
            pane.expire_ssh_preview();
            #[cfg(not(target_os = "linux"))]
            {
                pane.stop_loading();
                pane.load_uri("about:blank");
                pane.root.set_sensitive(false);
            }
            false
        });
    }

    fn expire_previews(&mut self) {
        self.expire_preview(None);
    }
}

impl Drop for SshRuntime {
    fn drop(&mut self) {
        if let Some(master) = self.master.take() {
            master.close_pty();
        }
        if let Some(window) = self.authentication.take() {
            window.destroy();
        }
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

async fn control(
    target: SshTarget,
    socket: PathBuf,
    operation: &str,
    extra: &[String],
) -> Result<String, String> {
    let mut argv = target.control_argv(&socket, operation)?;
    let host = argv.pop().ok_or("Missing SSH host")?;
    argv.extend_from_slice(extra);
    argv.push(host);
    let args: Vec<&std::ffi::OsStr> = argv.iter().map(std::ffi::OsStr::new).collect();
    let child = gio::Subprocess::newv(
        &args,
        gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE,
    )
    .map_err(|e| e.to_string())?;
    let output =
        glib::future_with_timeout(Duration::from_secs(5), child.communicate_utf8_future(None))
            .await;
    match output {
        Ok(Ok((stdout, _stderr))) if child.is_successful() => {
            Ok(stdout.map(|s| s.to_string()).unwrap_or_default())
        }
        Ok(Ok((_, stderr))) => Err(stderr
            .map(|s| s.to_string())
            .unwrap_or_else(|| "SSH control command failed".into())),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => {
            child.force_exit();
            Err("SSH control command timed out".into())
        }
    }
}

fn ssh_env(workspace: WorkspaceId, pane: PaneId, surface: SurfaceId) -> Vec<(String, String)> {
    let socket = flowmux_config::paths::runtime_socket_for_pid(std::process::id());
    let mut env = flowmux_terminal::agent_pty_env(pane, surface, workspace, &socket, None);
    env.push(("FLOWMUX_SSH_TERMINAL".into(), "1".into()));
    env
}

pub(crate) fn build_ssh_panel(
    workspace: WorkspaceId,
    pane: PaneId,
    surface: &PaneSurface,
    callbacks: &PaneCallbacks,
    registry: Rc<RefCell<PaneRegistry>>,
    theme: Arc<ResolvedTheme>,
) -> gtk::Widget {
    let runtime = registry.borrow().ssh.get(&workspace).cloned();
    let Some(runtime) = runtime else {
        return gtk::Label::new(Some("SSH workspace unavailable")).upcast();
    };
    registry
        .borrow_mut()
        .surface_workspace
        .insert(surface.id, workspace);
    let SurfaceKind::SshTerminal { cwd, tmux_session } = &surface.kind else {
        unreachable!()
    };
    let mut runtime_state = runtime.borrow_mut();
    if runtime_state.state != "connected" {
        return gtk::Label::new(Some("SSH disconnected — use Connect above")).upcast();
    }
    let generation = runtime_state.generation;
    if let Some(terminal) = runtime_state.reusable.remove(&surface.id) {
        terminal.set_pane_id(pane);
        let widget = terminal.root_widget();
        registry.borrow_mut().terminals.insert(surface.id, terminal);
        return widget;
    }
    if runtime_state
        .tabs
        .get(&surface.id)
        .is_some_and(|status| status.starts_with("exited"))
    {
        return gtk::Label::new(Some(
            "SSH session exited — open a new tab to start another session",
        ))
        .upcast();
    }
    let instance = uuid::Uuid::new_v4();
    runtime_state.instances.insert(surface.id, instance);
    let attach = runtime_state.launched.contains(&surface.id);
    let command = runtime_state
        .commands
        .remove(&surface.id)
        .unwrap_or_default();
    let argv = runtime_state.config.target.terminal_argv(
        &runtime_state.socket,
        cwd.as_deref(),
        tmux_session.as_deref(),
        attach,
        &command,
    );
    // Once attempted, a command is never replayed and tmux reconnect is attach-only.
    runtime_state.launched.insert(surface.id);
    runtime_state.tabs.insert(surface.id, "starting".into());
    drop(runtime_state);
    let argv = match argv {
        Ok(argv) => argv,
        Err(error) => return gtk::Label::new(Some(&error)).upcast(),
    };
    let mut scoped = callbacks.clone();
    let weak = Rc::downgrade(&runtime);
    let id = surface.id;
    scoped.on_child_exited = Rc::new(RefCell::new(move |_, status| {
        if let Some(runtime) = weak.upgrade() {
            let mut state = runtime.borrow_mut();
            if state.generation == generation && state.instances.get(&id) == Some(&instance) {
                state.tabs.insert(id, format!("exited ({status})"));
            }
        }
    }));
    // SSH titles/cwd are not local agent or filesystem evidence. Cwd is routed
    // through a dedicated generation-checked message below.
    scoped.on_terminal_title_changed = Rc::new(RefCell::new(|_, _, _| {}));
    scoped.on_terminal_contents_changed = Rc::new(RefCell::new(|_| {}));
    let bridge = runtime.borrow().bridge.clone();
    scoped.on_terminal_cwd_changed = Rc::new(RefCell::new(move |pane, surface, cwd: PathBuf| {
        if let Some(cwd) = cwd.to_str() {
            let _ = bridge.tx.try_send(GtkCommand::SshCwd {
                workspace,
                generation,
                instance,
                pane,
                surface,
                cwd: cwd.to_string(),
            });
        }
    }));
    let opts = (callbacks.read_options)();
    let terminal = PaneTerminal::spawn(
        pane,
        id,
        argv,
        None,
        ssh_env(workspace, pane, id),
        opts.scrollback_lines_or_default(),
        scoped,
    );
    theme.apply_to_ghostty(&terminal);
    terminal.set_font(&theme.terminal_font(&opts));
    terminal.set_cursor_blink(opts.cursor_blink, opts.cursor_blink_interval_ms);
    if opts.restore_terminal_scrollback {
        if let Some(snapshot) = &surface.scrollback {
            terminal.restore_scrollback(snapshot);
        }
    }
    runtime.borrow_mut().tabs.insert(id, "running".into());
    let widget = terminal.root_widget();
    registry.borrow_mut().terminals.insert(id, terminal);
    widget
}

impl WindowController {
    pub(super) fn preserve_ssh_channels(&self, workspace: WorkspaceId) {
        let mut registry = self.pane_registry.borrow_mut();
        let Some(runtime) = registry.ssh.get(&workspace).cloned() else {
            return;
        };
        let mut state = runtime.borrow_mut();
        if state.state != "connected" || state.rendered_generation != Some(state.generation) {
            return;
        }
        state.previews.clear();
        let ids: Vec<_> = registry
            .terminals
            .keys()
            .filter(|id| registry.surface_workspace.get(id) == Some(&workspace))
            .copied()
            .collect();
        for id in ids {
            if let Some(terminal) = registry.terminals.remove(&id) {
                let widget = terminal.root_widget();
                if let Some(stack) = widget.parent().and_downcast::<gtk::Stack>() {
                    stack.remove(&widget);
                }
                state.reusable.insert(id, terminal);
            }
        }
    }

    pub(super) fn discard_unused_ssh_channels(&self, workspace: WorkspaceId) {
        if let Some(runtime) = self.pane_registry.borrow().ssh.get(&workspace) {
            for (_, terminal) in runtime.borrow_mut().reusable.drain() {
                terminal.close_pty();
            }
        }
    }

    pub(super) async fn update_ssh_cwd(
        &self,
        workspace: WorkspaceId,
        generation: u64,
        instance: uuid::Uuid,
        pane: PaneId,
        surface: SurfaceId,
        cwd: String,
    ) {
        let valid = self
            .pane_registry
            .borrow()
            .ssh
            .get(&workspace)
            .is_some_and(|runtime| {
                let state = runtime.borrow();
                state.generation == generation
                    && state.instances.get(&surface) == Some(&instance)
                    && state.state == "connected"
            });
        if valid && self.pane_registry.borrow().workspace_of_pane(pane) == Some(workspace) {
            self.store.set_ssh_cwd(pane, surface, cwd).await;
        }
    }

    pub(super) fn show_ssh_dialog(&self) {
        let dialog = gtk::Window::builder()
            .transient_for(&self.window)
            .modal(true)
            .title("New SSH Workspace")
            .default_width(460)
            .build();
        let form = gtk::Box::new(gtk::Orientation::Vertical, 10);
        for set in [
            gtk::Widget::set_margin_top,
            gtk::Widget::set_margin_bottom,
            gtk::Widget::set_margin_start,
            gtk::Widget::set_margin_end,
        ] {
            set(form.upcast_ref(), 16);
        }
        let field = |label: &str, placeholder: &str| {
            let entry = gtk::Entry::builder()
                .placeholder_text(placeholder)
                .hexpand(true)
                .build();
            let caption = gtk::Label::new(Some(label));
            caption.set_xalign(0.0);
            form.append(&caption);
            form.append(&entry);
            entry
        };
        let host = field("Host", "Host alias or user@hostname");
        let cwd = field("Remote directory", "/srv/project (optional)");
        let name = field("Workspace name", "Optional");
        let port = field("Port", "From SSH config (default 22)");
        let identity = field("Identity file", "Optional local key path");
        let config = field("SSH config file", "Optional; defaults to ~/.ssh/config");
        let tmux = gtk::CheckButton::with_label("Keep remote sessions with tmux");
        form.append(&tmux);
        let hint = gtk::Label::new(Some("Authentication and host key confirmation use OpenSSH.\nRemote host requires a POSIX-compatible login shell."));
        hint.set_wrap(true);
        form.append(&hint);
        let error = gtk::Label::new(None);
        error.set_wrap(true);
        error.add_css_class("error");
        form.append(&error);
        let connect = gtk::Button::with_label("Connect");
        connect.add_css_class("suggested-action");
        form.append(&connect);
        let controller = self.clone();
        let window = dialog.clone();
        connect.connect_clicked(move |button| {
            let optional = |entry: &gtk::Entry| {
                let s = entry.text().trim().to_string();
                (!s.is_empty()).then_some(s)
            };
            let parsed = (|| {
                let mut target = SshTarget::parse(host.text().trim())?;
                target.port = optional(&port)
                    .map(|p| {
                        p.parse::<u16>()
                            .map_err(|_| "Port must be between 1 and 65535".to_string())
                    })
                    .transpose()?;
                target.identity_file = optional(&identity).map(PathBuf::from);
                target.config_file = optional(&config).map(PathBuf::from);
                let config = SshWorkspaceConfig {
                    target,
                    cwd: optional(&cwd),
                    tmux: tmux.is_active(),
                    forwards: vec![],
                };
                config.validate()?;
                Ok::<_, String>(SshRequest::Create {
                    request_id: uuid::Uuid::new_v4(),
                    name: optional(&name),
                    config,
                    command: vec![],
                })
            })();
            match parsed {
                Err(message) => error.set_text(&message),
                Ok(request) => {
                    button.set_sensitive(false);
                    let button = button.clone();
                    let controller = controller.clone();
                    let window = window.clone();
                    let error = error.clone();
                    glib::MainContext::default().spawn_local(async move {
                        match controller.dispatch_ssh(request).await {
                            Ok(_) => window.destroy(),
                            Err(message) => {
                                error.set_text(&message);
                                button.set_sensitive(true);
                            }
                        }
                    });
                }
            }
        });
        dialog.set_child(Some(&form));
        dialog.present();
    }

    fn show_ssh_ports(&self, workspace: WorkspaceId) {
        let Some(runtime) = self.pane_registry.borrow().ssh.get(&workspace).cloned() else {
            return;
        };
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .title("SSH Ports — local browser preview")
            .default_width(480)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
        content.set_margin_top(12);
        content.set_margin_bottom(12);
        content.set_margin_start(12);
        content.set_margin_end(12);
        for spec in &runtime.borrow().config.forwards {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let port = runtime.borrow().forwards.get(&spec.id).copied();
            let label = gtk::Label::new(Some(&format!(
                "Remote {} → local {}",
                spec.remote_port,
                port.map(|p| p.to_string())
                    .unwrap_or_else(|| "inactive".into())
            )));
            label.set_hexpand(true);
            row.append(&label);
            for (label, request) in [
                (
                    "Open preview",
                    SshRequest::Preview {
                        workspace,
                        id: spec.id,
                    },
                ),
                (
                    "Remove",
                    SshRequest::ForwardRemove {
                        workspace,
                        id: spec.id,
                    },
                ),
            ] {
                let button = gtk::Button::with_label(label);
                let controller = self.clone();
                let window = window.clone();
                button.connect_clicked(move |_| {
                    let controller = controller.clone();
                    let request = request.clone();
                    let window = window.clone();
                    glib::MainContext::default().spawn_local(async move {
                        match controller.dispatch_ssh(request).await {
                            Ok(_) => window.destroy(),
                            Err(error) => controller.clipboard_toast.show_with_message(&error),
                        }
                    });
                });
                row.append(&button);
            }
            content.append(&row);
        }
        let remote = gtk::Entry::builder()
            .placeholder_text("Remote TCP port, e.g. 3000")
            .build();
        let local = gtk::Entry::builder()
            .placeholder_text("Local port (empty = automatic)")
            .build();
        let https = gtk::CheckButton::with_label("HTTPS preview");
        content.append(&remote);
        content.append(&local);
        content.append(&https);
        let button = gtk::Button::with_label("Add forwarding");
        let controller = self.clone();
        let dialog = window.clone();
        button.connect_clicked(move |_| {
            let Ok(remote_port) = remote.text().parse::<u16>() else {
                controller
                    .clipboard_toast
                    .show_with_message("Enter a valid remote port");
                return;
            };
            let local_port = if local.text().is_empty() {
                None
            } else {
                match local.text().parse::<u16>() {
                    Ok(p) => Some(p),
                    Err(_) => {
                        controller
                            .clipboard_toast
                            .show_with_message("Enter a valid local port");
                        return;
                    }
                }
            };
            let request = SshRequest::ForwardAdd {
                workspace,
                spec: SshForwardSpec {
                    id: uuid::Uuid::new_v4(),
                    remote_port,
                    local_port,
                    https: https.is_active(),
                },
            };
            let controller = controller.clone();
            let dialog = dialog.clone();
            glib::MainContext::default().spawn_local(async move {
                match controller.dispatch_ssh(request).await {
                    Ok(_) => {
                        dialog.destroy();
                        controller.show_ssh_ports(workspace);
                    }
                    Err(error) => controller.clipboard_toast.show_with_message(&error),
                }
            });
        });
        content.append(&button);
        window.set_child(Some(&content));
        window.present();
    }

    async fn open_ssh_preview(
        &self,
        workspace: WorkspaceId,
        id: uuid::Uuid,
    ) -> Result<serde_json::Value, String> {
        if !cfg!(target_os = "linux") {
            return Err("SSH preview is currently supported only on Linux".into());
        }
        let runtime = self.pane_registry.borrow().ssh[&workspace].clone();
        let (port, https) = {
            let state = runtime.borrow();
            if state.state != "connected" {
                return Err("SSH is not connected".into());
            }
            (
                state
                    .forwards
                    .get(&id)
                    .copied()
                    .ok_or("Forward is not active")?,
                state
                    .config
                    .forwards
                    .iter()
                    .find(|s| s.id == id)
                    .ok_or("Forward not found")?
                    .https,
            )
        };
        let ws = self
            .store
            .get_workspace(workspace)
            .await
            .ok_or("Workspace not found")?;
        let pane = ws
            .surfaces
            .first()
            .and_then(|s| s.root_pane.first_leaf_id())
            .ok_or("Workspace has no pane")?;
        // Persist a binding, never a transient loopback port which could later
        // belong to another program. build_panel resolves each generation.
        let (_, surface) = self
            .store
            .add_browser_surface_to_pane(pane, format!("flowmux-ssh-preview://{id}"))
            .await
            .ok_or("Could not open preview")?;
        self.attach_or_rerender_surface(workspace, pane, surface)
            .await;
        Ok(
            serde_json::json!({"pane": pane, "surface": surface, "url": format!("{}://127.0.0.1:{port}", if https {"https"} else {"http"})}),
        )
    }

    pub(super) fn ensure_ssh_runtime(&self, ws: &Workspace) {
        if self.pane_registry.borrow().ssh.contains_key(&ws.id) {
            return;
        }
        let Some(config) = ws.ssh_config().cloned() else {
            return;
        };
        let directory =
            std::env::temp_dir().join(format!("fm-ssh-{}", uuid::Uuid::new_v4().simple()));
        let mut launched = HashSet::new();
        for root in &ws.surfaces {
            root.root_pane.for_each_leaf(|pane| {
                if let Some(PaneContent::Tabs { surfaces, .. }) =
                    root.root_pane.find_leaf_content(pane)
                {
                    for tab in surfaces {
                        launched.insert(tab.id);
                    }
                }
            });
        }
        self.pane_registry.borrow_mut().ssh.insert(
            ws.id,
            Rc::new(RefCell::new(SshRuntime {
                config,
                generation: 0,
                state: "disconnected",
                error: None,
                socket: directory.join("mux"),
                directory,
                master: None,
                authentication: None,
                launched,
                tabs: HashMap::new(),
                instances: HashMap::new(),
                reusable: HashMap::new(),
                rendered_generation: None,
                commands: HashMap::new(),
                forwards: HashMap::new(),
                request_id: None,
                bridge: self.bridge.clone(),
                previews: vec![],
            })),
        );
    }

    pub(super) fn wrap_ssh_workspace(&self, ws: &Workspace, content: gtk::Widget) -> gtk::Widget {
        let runtime = self.pane_registry.borrow().ssh[&ws.id].clone();
        let mut state = runtime.borrow_mut();
        if state.state == "connected" {
            state.rendered_generation = Some(state.generation);
        }
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.add_css_class("flowmux-ssh-workspace");
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let label = gtk::Label::new(Some(&format!(
            "SSH {} · {}{}",
            ws.location.display(),
            state.state,
            state
                .error
                .as_ref()
                .map(|e| format!(" · {e}"))
                .unwrap_or_default()
        )));
        label.set_hexpand(true);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        bar.append(&label);
        if state.state == "connecting" {
            bar.append(&gtk::Label::new(Some(
                "If SSH needs input, open Authentication",
            )));
        }
        for (text, request) in [
            ("Connect", SshRequest::Connect { workspace: ws.id }),
            ("Disconnect", SshRequest::Disconnect { workspace: ws.id }),
        ] {
            let button = gtk::Button::with_label(text);
            button.set_sensitive(if text == "Connect" {
                !matches!(state.state, "connected" | "connecting" | "disconnecting")
            } else {
                matches!(state.state, "connected" | "connecting" | "disconnecting")
            });
            let controller = self.clone();
            button.connect_clicked(move |_| {
                let controller = controller.clone();
                let request = request.clone();
                glib::MainContext::default().spawn_local(async move {
                    if let Err(error) = controller.dispatch_ssh(request).await {
                        controller.clipboard_toast.show_with_message(&error);
                    }
                });
            });
            bar.append(&button);
        }
        let authentication = gtk::Button::with_label("Authentication");
        authentication.set_tooltip_text(Some(
            "Open SSH password, key passphrase, host confirmation, or connection output",
        ));
        let weak = Rc::downgrade(&runtime);
        authentication.connect_clicked(move |_| {
            if let Some(runtime) = weak.upgrade() {
                if let Some(window) = &runtime.borrow().authentication {
                    window.present();
                }
            }
        });
        authentication.set_sensitive(state.authentication.is_some());
        bar.append(&authentication);
        let ports = gtk::Button::with_label("Ports");
        let controller = self.clone();
        let workspace = ws.id;
        ports.connect_clicked(move |_| controller.show_ssh_ports(workspace));
        bar.append(&ports);
        root.append(&bar);
        root.append(&content);
        root.upcast()
    }

    pub(super) async fn dispatch_ssh(
        &self,
        request: SshRequest,
    ) -> Result<serde_json::Value, String> {
        if let SshRequest::Create {
            request_id,
            name,
            config,
            command,
        } = request
        {
            config.validate()?;
            if command.iter().any(|arg| arg.contains('\0')) {
                return Err("Command contains a NUL byte".into());
            }
            let existing = self
                .pane_registry
                .borrow()
                .ssh
                .iter()
                .find_map(|(id, runtime)| {
                    (runtime.borrow().request_id == Some(request_id)).then_some(*id)
                });
            if let Some(workspace) = existing {
                return Ok(serde_json::json!({"workspace": workspace}));
            }
            let workspace = self.store.create_ssh_workspace(name, config).await?;
            let ws = self
                .store
                .get_workspace(workspace)
                .await
                .ok_or("Workspace no longer exists")?;
            self.ensure_ssh_runtime(&ws);
            let runtime = self.pane_registry.borrow().ssh[&workspace].clone();
            {
                let mut state = runtime.borrow_mut();
                state.launched.clear();
                state.request_id = Some(request_id);
                if let Some(surface) = ws.surfaces.first().and_then(|s| {
                    s.root_pane
                        .first_leaf_id()
                        .and_then(|p| s.root_pane.active_surface_id(p))
                }) {
                    state.commands.insert(surface, command);
                }
            }
            self.render_workspace(&ws);
            self.start_ssh(workspace)?;
            self.rerender_workspace(&ws);
            return Ok(serde_json::json!({"workspace": workspace, "state": "connecting"}));
        }
        let workspace = match request {
            SshRequest::Connect { workspace }
            | SshRequest::Disconnect { workspace }
            | SshRequest::Status { workspace }
            | SshRequest::ForwardAdd { workspace, .. }
            | SshRequest::ForwardRemove { workspace, .. }
            | SshRequest::Preview { workspace, .. } => workspace,
            SshRequest::Create { .. } => unreachable!(),
        };
        let ws = self
            .store
            .get_workspace(workspace)
            .await
            .ok_or("Workspace not found")?;
        if ws.ssh_config().is_none() {
            return Err("Target is not an SSH workspace".into());
        }
        self.ensure_ssh_runtime(&ws);
        let runtime = self.pane_registry.borrow().ssh[&workspace].clone();
        match request {
            SshRequest::Connect { .. } => {
                self.start_ssh(workspace)?;
                self.rerender_workspace(&ws);
            }
            SshRequest::Disconnect { .. } => {
                self.stop_ssh(workspace).await;
                self.rerender_workspace(&ws);
            }
            SshRequest::ForwardAdd { spec, .. } => {
                spec.validate()?;
                self.add_ssh_forward(workspace, spec.clone()).await?;
                let configs = {
                    let mut state = runtime.borrow_mut();
                    if !state.config.forwards.iter().any(|f| f.id == spec.id) {
                        state.config.forwards.push(spec);
                    }
                    state.config.forwards.clone()
                };
                self.store.set_ssh_forwards(workspace, configs).await?;
            }
            SshRequest::ForwardRemove { id, .. } => {
                runtime.borrow_mut().expire_preview(Some(id));
                let (target, socket, generation, spec, port) = {
                    let state = runtime.borrow();
                    (
                        state.config.target.clone(),
                        state.socket.clone(),
                        state.generation,
                        state
                            .config
                            .forwards
                            .iter()
                            .find(|f| f.id == id)
                            .cloned()
                            .ok_or("Forward not found")?,
                        state.forwards.get(&id).copied(),
                    )
                };
                if let Some(port) = port {
                    control(
                        target,
                        socket,
                        "cancel",
                        &[
                            "-L".into(),
                            format!("127.0.0.1:{port}:127.0.0.1:{}", spec.remote_port),
                        ],
                    )
                    .await?;
                }
                let configs = {
                    let mut state = runtime.borrow_mut();
                    if state.generation != generation {
                        return Err("Connection changed".into());
                    }
                    state.forwards.remove(&id);
                    state.config.forwards.retain(|f| f.id != id);
                    state.config.forwards.clone()
                };
                self.store.set_ssh_forwards(workspace, configs).await?;
            }
            SshRequest::Preview { id, .. } => {
                return self.open_ssh_preview(workspace, id).await;
            }
            SshRequest::Status { .. } => {}
            SshRequest::Create { .. } => unreachable!(),
        }
        let mut live = HashSet::new();
        for root in &ws.surfaces {
            root.root_pane.for_each_leaf(|pane| {
                if let Some(PaneContent::Tabs { surfaces, .. }) =
                    root.root_pane.find_leaf_content(pane)
                {
                    for tab in surfaces {
                        live.insert(tab.id);
                    }
                }
            });
        }
        let mut state = runtime.borrow_mut();
        state.tabs.retain(|id, _| live.contains(id));
        state.instances.retain(|id, _| live.contains(id));
        Ok(
            serde_json::json!({"workspace": workspace, "state": state.state, "generation": state.generation, "error": state.error, "tabs": state.tabs, "forwards": state.config.forwards.iter().map(|spec| serde_json::json!({"id":spec.id,"remote_port":spec.remote_port,"local_port":state.forwards.get(&spec.id),"active":state.forwards.contains_key(&spec.id)})).collect::<Vec<_>>() }),
        )
    }

    fn start_ssh(&self, workspace: WorkspaceId) -> Result<(), String> {
        let runtime = self.pane_registry.borrow().ssh[&workspace].clone();
        let (argv, generation, previous_master, previous_window) = {
            let mut state = runtime.borrow_mut();
            if matches!(state.state, "connected" | "connecting" | "disconnecting") {
                return Err("Disconnect before reconnecting".into());
            }
            state.generation += 1;
            // Each attempt owns a different socket. Delayed old cleanup cannot unlink it.
            state.socket = state.directory.join(format!("mux-{}", state.generation));
            if !state.directory.exists() {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&state.directory)
                    .map_err(|e| e.to_string())?;
            }
            state.state = "connecting";
            state.error = None;
            state.forwards.clear();
            (
                state.config.target.master_argv(&state.socket)?,
                state.generation,
                state.master.take(),
                state.authentication.take(),
            )
        };
        if let Some(master) = previous_master {
            master.close_pty();
        }
        if let Some(window) = previous_window {
            window.destroy();
        }
        let mut callbacks = self.callbacks.clone();
        callbacks.on_focus = Rc::new(RefCell::new(|_| {}));
        callbacks.on_terminal_cwd_changed = Rc::new(RefCell::new(|_, _, _| {}));
        callbacks.on_terminal_title_changed = Rc::new(RefCell::new(|_, _, _| {}));
        callbacks.on_terminal_contents_changed = Rc::new(RefCell::new(|_| {}));
        let weak = Rc::downgrade(&runtime);
        let bridge = self.bridge.clone();
        callbacks.on_child_exited = Rc::new(RefCell::new(move |_, status| {
            if let Some(runtime) = weak.upgrade() {
                let mut state = runtime.borrow_mut();
                if state.generation == generation
                    && matches!(state.state, "connecting" | "connected")
                {
                    state.expire_previews();
                    state.state = "failed";
                    state.error = Some(format!("SSH exited ({status}); see authentication output"));
                    state.forwards.clear();
                    let _ = bridge.tx.try_send(GtkCommand::SshRefresh {
                        workspace,
                        generation,
                    });
                }
            }
        }));
        let pane = PaneId::new();
        let surface = SurfaceId::new();
        let opts = (self.callbacks.read_options)();
        let master = PaneTerminal::spawn(
            pane,
            surface,
            argv,
            None,
            ssh_env(workspace, pane, surface),
            opts.scrollback_lines_or_default(),
            callbacks,
        );
        self.current_theme().apply_to_ghostty(&master);
        master.set_font(&self.current_theme().terminal_font(&opts));
        let window = gtk::Window::builder()
            .transient_for(&self.window)
            .title("SSH authentication")
            .default_width(740)
            .default_height(360)
            .child(&master.root_widget())
            .build();
        window.connect_close_request(|window| {
            window.set_visible(false);
            glib::Propagation::Stop
        });
        // Keep the authentication PTY alive without opening a window or taking
        // focus. The workspace's Authentication button exposes it when needed.
        {
            let mut state = runtime.borrow_mut();
            state.master = Some(master);
            state.authentication = Some(window);
        }
        let weak = Rc::downgrade(&runtime);
        let bridge = self.bridge.clone();
        glib::MainContext::default().spawn_local(async move {
            loop {
                glib::timeout_future(Duration::from_millis(700)).await;
                let Some(runtime) = weak.upgrade() else {
                    break;
                };
                let (target, socket, before) = {
                    let state = runtime.borrow();
                    if state.generation != generation
                        || !matches!(state.state, "connecting" | "connected")
                    {
                        break;
                    }
                    (
                        state.config.target.clone(),
                        state.socket.clone(),
                        state.state,
                    )
                };
                let checked = control(target, socket, "check", &[]).await;
                let mut state = runtime.borrow_mut();
                if state.generation != generation || state.state != before {
                    continue;
                }
                if checked.is_ok() && before == "connecting" {
                    state.state = "connected";
                    if let Some(window) = &state.authentication {
                        window.set_visible(false);
                    }
                    let _ = bridge.tx.try_send(GtkCommand::SshRefresh {
                        workspace,
                        generation,
                    });
                } else if let Err(error) = checked {
                    if before == "connected" {
                        state.expire_previews();
                        state.state = "failed";
                        state.error = Some(error);
                        state.forwards.clear();
                        let _ = bridge.tx.try_send(GtkCommand::SshRefresh {
                            workspace,
                            generation,
                        });
                        break;
                    }
                }
            }
        });
        Ok(())
    }

    async fn stop_ssh(&self, workspace: WorkspaceId) {
        let runtime = self.pane_registry.borrow().ssh[&workspace].clone();
        let (target, socket, generation) = {
            let mut state = runtime.borrow_mut();
            state.expire_previews();
            state.generation += 1;
            state.state = "disconnecting";
            state.forwards.clear();
            (
                state.config.target.clone(),
                state.socket.clone(),
                state.generation,
            )
        };
        let terminals: Vec<_> = {
            let registry = self.pane_registry.borrow();
            registry
                .terminals
                .iter()
                .filter(|(id, _)| registry.surface_workspace.get(id) == Some(&workspace))
                .map(|(_, terminal)| terminal.clone())
                .collect()
        };
        for terminal in terminals {
            terminal.close_pty();
        }
        let _ = control(target, socket.clone(), "exit", &[]).await;
        let (master, window) = {
            let mut state = runtime.borrow_mut();
            if state.generation != generation {
                return;
            }
            state.state = "disconnected";
            state.error = None;
            state.commands.clear();
            (state.master.take(), state.authentication.take())
        };
        if let Some(master) = master {
            master.close_pty();
        }
        if let Some(window) = window {
            window.destroy();
        }
        let _ = std::fs::remove_file(socket);
    }

    pub(super) async fn refresh_ssh(&self, workspace: WorkspaceId, generation: u64) {
        let Some(runtime) = self.pane_registry.borrow().ssh.get(&workspace).cloned() else {
            return;
        };
        if runtime.borrow().generation != generation {
            return;
        }
        if runtime.borrow().state == "connected" {
            let specs = runtime.borrow().config.forwards.clone();
            for spec in specs {
                if let Err(error) = self.add_ssh_forward(workspace, spec).await {
                    let mut state = runtime.borrow_mut();
                    if state.generation != generation {
                        return;
                    }
                    state.error = Some(error);
                }
            }
        }
        if runtime.borrow().generation != generation {
            return;
        }
        if let Some(ws) = self.store.get_workspace(workspace).await {
            self.rerender_workspace(&ws);
        }
    }

    async fn add_ssh_forward(
        &self,
        workspace: WorkspaceId,
        spec: SshForwardSpec,
    ) -> Result<u16, String> {
        spec.validate()?;
        let runtime = self.pane_registry.borrow().ssh[&workspace].clone();
        let (target, socket, generation) = {
            let state = runtime.borrow();
            if state.state != "connected" {
                return Err("SSH is not connected".into());
            }
            if let Some(port) = state.forwards.get(&spec.id) {
                if state
                    .config
                    .forwards
                    .iter()
                    .any(|existing| existing.id == spec.id && existing != &spec)
                {
                    return Err("Forward ID already belongs to a different mapping".into());
                }
                return Ok(*port);
            }
            (
                state.config.target.clone(),
                state.socket.clone(),
                state.generation,
            )
        };
        let mut error = String::new();
        for _ in 0..3 {
            let port = match spec.local_port {
                Some(port) => port,
                None => std::net::TcpListener::bind(("127.0.0.1", 0))
                    .and_then(|l| l.local_addr())
                    .map_err(|e| e.to_string())?
                    .port(),
            };
            match control(
                target.clone(),
                socket.clone(),
                "forward",
                &[
                    "-L".into(),
                    format!("127.0.0.1:{port}:127.0.0.1:{}", spec.remote_port),
                ],
            )
            .await
            {
                Ok(_) => {
                    let mut state = runtime.borrow_mut();
                    if state.generation != generation || state.state != "connected" {
                        return Err("Connection changed".into());
                    }
                    state.forwards.insert(spec.id, port);
                    return Ok(port);
                }
                Err(e) => {
                    error = e;
                    if spec.local_port.is_some() {
                        break;
                    }
                }
            }
        }
        Err(error)
    }
}
