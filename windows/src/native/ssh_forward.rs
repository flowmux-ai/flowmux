// SPDX-License-Identifier: GPL-3.0-or-later
//! A forwarding process and its authentication terminal, independent of model tabs.
use super::*;
use flowmux_core::SshForwardSpec;

#[derive(Clone, Copy)]
pub(super) enum UiAction {
    Close,
    Layout,
    Reconnect,
}

thread_local! {
    static ROUTES: RefCell<HashMap<isize, SurfaceId>> = RefCell::new(HashMap::new());
}

unsafe extern "system" fn procedure(window: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    let action = match message {
        WM_CLOSE => Some(UiAction::Close),
        WM_SIZE | WM_MOVE => Some(UiAction::Layout),
        WM_DPICHANGED if l != 0 => {
            let rect = &*(l as *const RECT);
            SetWindowPos(
                window,
                std::ptr::null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            Some(UiAction::Layout)
        }
        WM_NCDESTROY => {
            ROUTES.with(|routes| routes.borrow_mut().remove(&(window as isize)));
            None
        }
        _ => None,
    };
    if let Some(action) = action {
        let id = ROUTES.with(|routes| routes.borrow().get(&(window as isize)).copied());
        if let Some(id) = id {
            post(Event::SshForwardUi(id, action));
        }
        return 0;
    }
    chrome::message(window, message, w, l).unwrap_or_else(|| DefWindowProcW(window, message, w, l))
}

struct Window(HWND);
impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

pub(super) struct Forward {
    pub(super) surface: SurfaceId,
    pub(super) workspace: WorkspaceId,
    pub(super) spec: SshForwardSpec,
    pub(super) port: u16,
    pub(super) error: Option<String>,
    // The WebView and its holder must drop before the owned top-level HWND.
    terminal: Surface,
    window: Window,
    owner: HWND,
    shell: crate::shell::Shell,
    cwd: PathBuf,
    pipe: String,
    sender: EventSender,
    background: bool,
    open: bool,
    state: &'static str,
}

impl Forward {
    pub(super) fn new(
        app: &mut App,
        workspace: WorkspaceId,
        spec: SshForwardSpec,
        port: u16,
    ) -> anyhow::Result<Self> {
        let workspace_spec = app
            .workspaces
            .iter()
            .find(|ws| ws.id == workspace)
            .context("SSH workspace no longer exists")?;
        let target = &workspace_spec
            .ssh
            .as_ref()
            .context("Workspace is not SSH")?
            .target;
        let shell = crate::ssh::forwarding_shell(target, &spec, port)?;
        let cwd = workspace_spec.cwd.clone();
        let title = format!(
            "SSH Authentication — {} : {}",
            target.destination(),
            spec.remote_port
        );
        let surface = SurfaceId::new();
        let owner = app.window;
        let window = unsafe {
            anyhow::ensure!(
                IsWindow(owner) != 0,
                "SSH authentication owner is unavailable"
            );
            let class = wide("flowmux.windows.ssh.forward");
            let instance = GetModuleHandleW(std::ptr::null());
            let definition = WNDCLASSW {
                lpfnWndProc: Some(procedure),
                hInstance: instance,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                lpszClassName: class.as_ptr(),
                ..Default::default()
            };
            anyhow::ensure!(
                RegisterClassW(&definition) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS,
                "Cannot register SSH authentication window"
            );
            let dpi = GetDpiForWindow(owner).max(96);
            let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
            let mut frame = RECT {
                left: 0,
                top: 0,
                right: (740 * dpi / 96) as i32,
                bottom: (360 * dpi / 96) as i32,
            };
            checked(AdjustWindowRectExForDpi(
                &mut frame,
                style,
                0,
                WS_EX_TOOLWINDOW,
                dpi,
            ))?;
            let mut parent = RECT::default();
            checked(GetWindowRect(owner, &mut parent))?;
            let width = frame.right - frame.left;
            let height = frame.bottom - frame.top;
            let handle = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                class.as_ptr(),
                wide(&title).as_ptr(),
                style,
                parent.left + (parent.right - parent.left - width) / 2,
                parent.top + (parent.bottom - parent.top - height) / 2,
                width,
                height,
                owner,
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            );
            checked((!handle.is_null()) as i32)?;
            Window(handle)
        };
        ROUTES.with(|routes| routes.borrow_mut().insert(window.0 as isize, surface));
        chrome::window_theme(window.0, app.settings.terminal.theme);
        let terminal = app.terminal_view(surface, window.0, Vec::new())?;
        let forward = Self {
            surface,
            workspace,
            spec,
            port,
            error: None,
            terminal,
            window,
            owner,
            shell,
            cwd,
            pipe: app._ipc.name.clone(),
            sender: app.sender.clone(),
            background: app.background_test,
            open: false,
            state: "connecting",
        };
        forward.layout()?;
        Ok(forward)
    }

    pub(super) fn state(&self) -> &str {
        self.state
    }
    pub(super) fn session_generation(&self) -> Uuid {
        self.terminal.session_generation
    }

    fn layout(&self) -> anyhow::Result<()> {
        let mut client = RECT::default();
        unsafe {
            checked(GetClientRect(self.window.0, &mut client))?;
        }
        let area = model::Rect {
            x: 0,
            y: 0,
            width: client.right.max(1),
            height: client.bottom.max(1),
        };
        self.terminal
            .holder
            .layout(Some(area), self.background || !self.open)?;
        self.terminal.view.set_bounds(bounds(area))?;
        unsafe {
            self.terminal
                .view
                .controller()
                .NotifyParentWindowPositionChanged()?;
        }
        Ok(())
    }

    pub(super) fn show(&mut self) -> anyhow::Result<()> {
        self.open = true;
        self.layout()?;
        if !self.background {
            unsafe {
                ShowWindow(self.window.0, SW_SHOWNORMAL);
            }
            self.terminal.view.set_visible(true)?;
            self.terminal.view.focus()?;
        }
        self.terminal.visible = self.open && !self.background;
        if self.terminal.ready {
            self.terminal.send(&HostMessage::Visibility {
                visible: self.terminal.visible,
            })?;
        }
        Ok(())
    }

    pub(super) fn ui(&mut self, action: UiAction) -> anyhow::Result<()> {
        match action {
            UiAction::Close => {
                self.open = false;
                self.terminal.visible = false;
                if !self.background {
                    unsafe {
                        ShowWindow(self.window.0, SW_HIDE);
                    }
                }
                self.terminal.view.set_visible(false)?;
                if self.terminal.ready {
                    self.terminal
                        .send(&HostMessage::Visibility { visible: false })?;
                }
                self.layout()
            }
            UiAction::Layout => self.layout(),
            UiAction::Reconnect => Ok(()), // Routed through the workspace lifecycle by App.
        }
    }

    fn start(&mut self) -> anyhow::Result<()> {
        let generation = Uuid::new_v4();
        let id = self.surface;
        let sender = self.sender.clone();
        self.terminal.ready = true;
        self.terminal.session_generation = generation;
        self.terminal.send(&HostMessage::SessionStart {
            session: generation,
        })?;
        match Session::spawn_after(
            &self.cwd,
            &self.shell,
            PaneId::new(),
            id,
            self.workspace,
            &self.pipe,
            self.terminal.cols,
            self.terminal.rows,
            0,
            move |event| sender.send(Event::Session(id, generation, event)),
        ) {
            Ok(session) => {
                self.terminal.process_pid = Some(session.pid);
                self.terminal.session = Some(session);
                self.state = "connecting";
                self.terminal
                    .send(&HostMessage::ShellStatus { error: None })?;
            }
            Err(error) => self.fail(format!("{error:#}"))?,
        }
        Ok(())
    }

    fn fail(&mut self, error: String) -> anyhow::Result<()> {
        self.error = Some(error.clone());
        self.state = "failed";
        self.terminal.ssh_connected = false;
        self.terminal.startup_error = Some(error.clone());
        self.terminal.send(&HostMessage::SshStatus {
            state: "failed".into(),
            error: Some(error),
        })
    }

    fn abort(&mut self, error: String) {
        // Release the owned job before fallible WebView calls. Queued final
        // output/ACKs can still drain, but this forward can never become active.
        self.terminal.session.take();
        let hidden = self.ui(UiAction::Close);
        let failed = self.fail(error);
        for result in [hidden, failed] {
            if let Err(error) = result {
                report(&format!("SSH forwarding cleanup: {error:#}"));
            }
        }
    }

    fn input(&self, bytes: Vec<u8>) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.terminal.exit_code.is_none() && self.state != "failed",
            "SSH forwarding process has closed"
        );
        self.terminal
            .session
            .as_ref()
            .context("SSH forwarding process is not ready")?
            .input(bytes)
    }

    pub(super) fn apply_settings(
        &self,
        settings: &crate::settings::Document,
    ) -> anyhow::Result<()> {
        chrome::window_theme(self.window.0, settings.terminal.theme);
        self.terminal.send(&HostMessage::Settings {
            document: Box::new(settings.clone()),
            bindings: Vec::new(),
            colors: Box::new(crate::theme::resolve(&settings.terminal)),
        })
    }

    pub(super) fn bridge(
        &mut self,
        origin: &str,
        body: &str,
        settings: &crate::settings::Document,
    ) -> anyhow::Result<()> {
        let envelope = self.terminal.identity.decode(origin, body)?;
        if !envelope.accepts_session(self.terminal.session_generation) {
            return Ok(());
        }
        match envelope.message {
            ClientMessage::Ready if !self.terminal.ready && self.state != "failed" => {
                self.terminal.send(&HostMessage::Visibility {
                    visible: self.terminal.visible,
                })?;
                self.apply_settings(settings)?;
                self.start()?;
            }
            ClientMessage::SettingsApplied {
                revision,
                terminal,
                bindings,
                background,
                foreground,
                colors,
            } if revision == settings.revision
                && *terminal == settings.terminal
                && bindings.is_empty()
                && *colors == crate::theme::resolve(&settings.terminal)
                && background == colors.background
                && foreground == colors.foreground =>
            {
                self.terminal.applied_settings = Some(
                    json!({"revision":revision,"terminal":terminal,"colors":colors,"bindings":bindings}),
                );
            }
            ClientMessage::Input { data } => self.input(data.into_bytes())?,
            ClientMessage::BinaryInput { data } => {
                anyhow::ensure!(
                    data.chars().all(|ch| ch as u32 <= 255),
                    "Invalid binary authentication input"
                );
                self.input(data.chars().map(|ch| ch as u8).collect())?;
            }
            ClientMessage::Pasted {
                request: None,
                sequence,
                outcome,
            } => {
                let result = (|| {
                    anyhow::ensure!(
                        sequence <= self.terminal.output_sequence,
                        "Invalid authentication paste sequence"
                    );
                    let (bytes, _) = outcome.into_input()?;
                    if !bytes.is_empty() {
                        self.input(bytes)?;
                    }
                    Ok::<_, anyhow::Error>(())
                })();
                self.terminal.send(&HostMessage::PasteResult {
                    error: result.err().map(|error| error.to_string()),
                })?;
            }
            ClientMessage::Resize { cols, rows } => {
                self.terminal.cols = cols;
                self.terminal.rows = rows;
                if self.terminal.exit_code.is_none() {
                    if let Some(session) = &self.terminal.session {
                        session.resize(cols, rows)?;
                    }
                }
            }
            ClientMessage::Ack { sequence } => {
                anyhow::ensure!(
                    sequence >= self.terminal.acknowledged_sequence
                        && sequence <= self.terminal.output_sequence,
                    "Invalid authentication parser acknowledgement"
                );
                if let Some(session) = &self.terminal.session {
                    session.acknowledge(sequence)?;
                }
                self.terminal.acknowledged_sequence = sequence;
            }
            ClientMessage::Fault { message } => self.abort(message),
            ClientMessage::SshConnect => self
                .sender
                .send(Event::SshForwardUi(self.surface, UiAction::Reconnect)),
            // Authentication is not a model tab: no structural actions, links or global shortcuts.
            _ => {}
        }
        Ok(())
    }

    pub(super) fn session_event(&mut self, event: SessionEvent) -> anyhow::Result<()> {
        match event {
            SessionEvent::Output { sequence, bytes } => {
                anyhow::ensure!(
                    sequence == self.terminal.output_sequence + 1,
                    "Out-of-order authentication output"
                );
                if let Err(error) = self.terminal.send(&HostMessage::Output {
                    sequence,
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                }) {
                    self.terminal.output_sequence = sequence;
                    self.abort(format!("SSH authentication output failed: {error:#}"));
                    return Ok(());
                }
                self.terminal.output_sequence = sequence;
                self.terminal.observed_output_bytes = self
                    .terminal
                    .observed_output_bytes
                    .saturating_add(bytes.len() as u64);
            }
            SessionEvent::OutputEnd => self.terminal.output_ended = true,
            SessionEvent::Exit(code) => {
                self.terminal.exit_code = Some(code);
                if self.terminal.output_ended {
                    self.terminal.session.take();
                }
                let exited = self.terminal.send(&HostMessage::Exit { code });
                let error = self
                    .error
                    .clone()
                    .unwrap_or_else(|| format!("SSH forwarding process exited with code {code}"));
                let failed = self.fail(error);
                for result in [exited, failed] {
                    if let Err(error) = result {
                        report(&format!("SSH forwarding exit notification: {error:#}"));
                    }
                }
            }
            SessionEvent::Error(error) => self.abort(error),
        }
        if self.terminal.output_ended && self.terminal.exit_code.is_some() {
            self.terminal.session.take();
        }
        Ok(())
    }

    pub(super) fn tick(&mut self) -> anyhow::Result<bool> {
        if matches!(self.state, "connecting" | "connected") && self.terminal.exit_code.is_none() {
            if let Some(session) = &self.terminal.session {
                match ssh_listener::owns_listener(session.pid, self.port) {
                    Ok(true) if self.state == "connecting" => {
                        self.state = "connected";
                        self.terminal.ssh_connected = true;
                        if let Err(error) = self.ui(UiAction::Close) {
                            report(&format!("SSH authentication window: {error:#}"));
                        }
                        return Ok(true);
                    }
                    Ok(false) if self.state == "connected" => {
                        self.abort("SSH forwarding listener closed".into());
                        return Ok(true);
                    }
                    Err(error) => {
                        self.abort(format!("Cannot verify SSH forwarding listener: {error:#}"));
                        return Ok(true);
                    }
                    _ => {}
                }
            }
        }
        Ok(false)
    }

    pub(super) fn status(&self) -> Value {
        json!({"workspace":self.workspace,"id":self.spec.id,"surface":self.surface,"spec":self.spec,"port":self.port,
            "state":self.state,"error":self.error,"window":self.window.0 as usize,"owner":self.owner as usize,
            "open":self.open,"native_visible":unsafe { IsWindowVisible(self.window.0) } != 0,
            "view_handle":self.terminal.view.hwnd().0 as usize,"holder":self.terminal.holder.diagnostics(),
            "ready":self.terminal.ready,"running":self.terminal.session.is_some(),"pid":self.terminal.process_pid,
            "session":self.terminal.session_generation,"exit_code":self.terminal.exit_code,"output_ended":self.terminal.output_ended,
            "output_sequence":self.terminal.output_sequence,"parsed_sequence":self.terminal.acknowledged_sequence,
            "cols":self.terminal.cols,"rows":self.terminal.rows,"applied_settings":self.terminal.applied_settings})
    }
}
