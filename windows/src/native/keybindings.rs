// SPDX-License-Identifier: GPL-3.0-or-later
//! Terminal shortcuts are authorized against the renderer's acknowledged settings.
use super::*;
use crate::keybindings::{ActionId, Chord};

#[derive(Default)]
pub(super) struct WindowKeys {
    modifiers: u16,
    composing: bool,
    settling: bool,
}

thread_local! {
    static NATIVE_KEY_GUARDS: RefCell<Vec<(HWND, u32)>> = const { RefCell::new(Vec::new()) };
}

// IME and focus messages can be sent synchronously, outside GetMessageW.
// Record only guards here; never mutate App reentrantly or reorder IPC work.
pub(super) fn native_key_guard(window: HWND, message: u32) {
    if matches!(
        message,
        WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION | WM_KILLFOCUS | WM_ACTIVATE
    ) {
        NATIVE_KEY_GUARDS.with(|guards| guards.borrow_mut().push((window, message)));
    }
}

impl App {
    pub(super) fn apply_native_key_guards(&mut self) {
        let guards = NATIVE_KEY_GUARDS.with(|guards| std::mem::take(&mut *guards.borrow_mut()));
        for (window, message) in guards {
            self.empty_window_key_guard(window, message);
        }
    }

    fn empty_window_key_target(&self, window: HWND) -> bool {
        !self.main_closed
            && self.current_workspace().is_none()
            && (window == self.window || self.controls.iter().any(|control| control.hwnd == window))
    }

    pub(super) fn empty_window_key_guard(&mut self, window: HWND, message: u32) {
        if !self.empty_window_key_target(window) {
            return;
        }
        match message {
            WM_IME_STARTCOMPOSITION | WM_IME_ENDCOMPOSITION => {
                self.empty_window_keys.composing = message == WM_IME_STARTCOMPOSITION;
                self.empty_window_keys.settling = true;
                self.empty_window_keys.modifiers = 0;
            }
            _ => self.empty_window_keys = WindowKeys::default(),
        }
    }

    pub(super) fn empty_window_shortcut(&mut self, message: &MSG) -> bool {
        if !matches!(
            message.message,
            WM_KEYDOWN | WM_KEYUP | WM_SYSKEYDOWN | WM_SYSKEYUP
        ) {
            return false;
        }
        if !self.empty_window_key_target(message.hwnd)
            || unsafe { IsWindowEnabled(self.window) } == 0
            || unsafe { IsWindowEnabled(message.hwnd) } == 0
            || self.close_request.is_some()
            || self.close_accepted
            || self.closing
            || self.editor_barrier.is_some()
            || self.command_palette.is_open()
        {
            self.empty_window_keys = WindowKeys::default();
            return false;
        }
        let down = matches!(message.message, WM_KEYDOWN | WM_SYSKEYDOWN);
        let modifier = crate::keybindings::native_modifier(message.wParam, message.lParam);
        let keys = &mut self.empty_window_keys;
        if let Some(bit) = modifier {
            if down {
                keys.modifiers |= bit;
            } else {
                keys.modifiers &= !bit;
            }
        } else if !down {
            keys.settling = false;
        }
        if down && message.wParam == 229 {
            keys.settling = true;
        }
        if !down || keys.composing || keys.settling || keys.modifiers & (8 | 64 | 128) != 0 {
            return false;
        }
        let Some(chord) = crate::keybindings::captured_key(
            message.wParam as u32,
            keys.modifiers & 3 != 0,
            keys.modifiers & 12 != 0,
            keys.modifiers & 48 != 0,
        )
        .ok()
        .flatten()
        .and_then(|value| crate::keybindings::parse(&value).ok()) else {
            return false;
        };
        let action = crate::keybindings::resolved(&self.settings.keybindings)
            .ok()
            .and_then(|bindings| bindings.into_iter().find(|binding| binding.chord == chord))
            .and_then(|binding| ActionId::from_wire(&binding.action))
            .filter(|action| {
                matches!(
                    action,
                    ActionId::NewWorkspace
                        | ActionId::NewWindow
                        | ActionId::CommandPalette
                        | ActionId::ToggleUsagePopover
                        | ActionId::ToggleSessionPanel
                        | ActionId::QuitApp
                )
            });
        let Some(action) = action else {
            return false;
        };
        if message.lParam as usize & (1 << 30) == 0 {
            post(Event::EmptyWindowShortcut(action));
        }
        true
    }

    pub(super) fn empty_window_shortcut_action(&mut self, action: ActionId) -> anyhow::Result<()> {
        if !self.empty_window_key_target(self.window)
            || unsafe { IsWindowEnabled(self.window) } == 0
            || self.close_request.is_some()
            || self.close_accepted
            || self.closing
            || self.editor_barrier.is_some()
            || self.command_palette.is_open()
        {
            return Ok(());
        }
        // A detached window can remain alive beside the empty main window.
        // Main-window shortcuts use the main context, even after detached focus.
        self.detached_focus = None;
        match action {
            ActionId::NewWorkspace => self.action(Action::NewWorkspace),
            ActionId::NewWindow => self.action(Action::NewWindow),
            ActionId::CommandPalette => self.action(Action::CommandPalette),
            ActionId::ToggleUsagePopover => self.action(Action::Usage),
            ActionId::ToggleSessionPanel => self.action(Action::Sessions),
            ActionId::QuitApp => self.request_close(CloseRequest::Native),
            _ => Ok(()),
        }
    }

    #[cfg(debug_assertions)]
    pub(super) fn test_terminal_menu(
        &mut self,
        source: SurfaceId,
        event: &str,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.background_test,
            "terminal menu tests require an owned hidden debug host"
        );
        anyhow::ensure!(
            event.len() <= 2048,
            "terminal menu test event exceeds limit"
        );
        let event: Value = serde_json::from_str(event)?;
        let fields = event
            .as_object()
            .context("terminal menu event must be an object")?;
        anyhow::ensure!(
            matches!(event["action"].as_str(), Some("open" | "click" | "key")),
            "invalid terminal menu test action"
        );
        for (key, value) in fields {
            let valid = match key.as_str() {
                "action" => true,
                "item" => matches!(
                    value.as_str(),
                    Some("split_right" | "split_down" | "close_pane")
                ),
                "key" => matches!(
                    value.as_str(),
                    Some("ArrowUp" | "ArrowDown" | "Home" | "End" | "Escape" | "Tab")
                ),
                "shiftKey" | "isComposing" => value.is_boolean(),
                "keyCode" => value.as_u64().is_some_and(|code| code <= 255),
                "x" | "y" => value
                    .as_u64()
                    .is_some_and(|coordinate| coordinate <= 1000000),
                _ => false,
            };
            anyhow::ensure!(valid, "invalid terminal menu event field: {key}");
        }
        anyhow::ensure!(
            event["action"] != "click" || event.get("item").is_some(),
            "menu click requires an item"
        );
        anyhow::ensure!(
            event["action"] != "key" || event.get("key").is_some(),
            "menu key requires a key"
        );
        anyhow::ensure!(
            self.pending_terminal_ui_tests.len() < 2,
            "terminal UI test already pending"
        );
        let terminal = self
            .surfaces
            .get(&source)
            .context("terminal menu test requires a terminal surface")?;
        anyhow::ensure!(
            terminal.ready && !terminal.restoring,
            "terminal menu test terminal is not ready"
        );
        let request = Uuid::new_v4();
        terminal.send(&HostMessage::TestTerminalMenu { request, event })?;
        self.pending_terminal_ui_tests.insert(
            request,
            PendingRead {
                surface: source,
                after: 0,
                reply,
                started: Instant::now(),
            },
        );
        Ok(())
    }

    pub(super) fn shortcut(
        &mut self,
        source: SurfaceId,
        action: &str,
        chord: &Chord,
        revision: Uuid,
    ) -> anyhow::Result<()> {
        let Some(surface) = self.surfaces.get(&source) else {
            return Ok(());
        };
        let authentication = self
            .ssh_auth_window
            .as_ref()
            .is_some_and(|(id, _)| *id == source);
        if (self.current_surface() != Some(source) && !authentication)
            || !surface.visible
            || !surface.ready
            || surface.restoring
            || self.close_accepted
            || self.close_request.is_some()
            || self.editor_barrier.is_some()
            || self.overview.is_open()
            || revision != self.settings.revision
            || surface
                .applied_settings
                .as_ref()
                .and_then(|s| s.get("revision"))
                != Some(&json!(revision))
        {
            return Ok(());
        }
        if !crate::keybindings::resolved(&self.settings.keybindings)?
            .iter()
            .any(|binding| binding.action == action && &binding.chord == chord)
        {
            return Ok(());
        }
        let Some(action) = ActionId::from_wire(action) else {
            return Ok(());
        };
        self.keybinding_action(source, action)
    }

    pub(super) fn keybinding_action(
        &mut self,
        source: SurfaceId,
        action: ActionId,
    ) -> anyhow::Result<()> {
        let (source_workspace, _, _) = self
            .locate(source)
            .context("No active workspace for shortcut source")?;
        anyhow::ensure!(
            !self.close_accepted
                && self.close_request.is_none()
                && self.editor_barrier.is_none()
                && !self.overview.is_open()
                && !self.command_palette.is_open(),
            "window is busy"
        );
        use ActionId::*;
        let authentication = self
            .ssh_auth_window
            .as_ref()
            .is_some_and(|(id, _)| *id == source);
        if authentication && unsafe { IsWindowEnabled(self.surface_window(source)) } == 0 {
            return Ok(());
        }
        if self.detached.contains_key(&source) || authentication {
            match action {
                CloseSurface => {
                    return if authentication {
                        self.close_ssh_auth()
                    } else {
                        self.close_detached(source)
                    }
                }
                TerminalSearch => {
                    self.surfaces
                        .get(&source)
                        .context("Shortcut requires a terminal surface")?
                        .send(&HostMessage::OpenFind {
                            focus: !self.background_test,
                        })?;
                    return Ok(());
                }
                QuitApp => return self.request_close(CloseRequest::Native),
                NewWindow if !authentication => return self.new_window(Some(source)),
                _ => return Ok(()), // Single-surface window tools match Linux's disabled controls.
            }
        }
        let action = action.as_str();
        let action = match ActionId::from_wire(action) {
            Some(SplitRight) => Action::Vertical,
            Some(SplitDown) => Action::Horizontal,
            Some(FocusLeft) => {
                return self
                    .focus_direction(source, FocusDirection::Left)
                    .map(|_| ())
            }
            Some(FocusRight) => {
                return self
                    .focus_direction(source, FocusDirection::Right)
                    .map(|_| ())
            }
            Some(FocusUp) => return self.focus_direction(source, FocusDirection::Up).map(|_| ()),
            Some(FocusDown) => {
                return self
                    .focus_direction(source, FocusDirection::Down)
                    .map(|_| ())
            }
            Some(CloseSurface) => Action::CloseTab,
            Some(QuitApp) => return self.request_close(CloseRequest::Native),
            Some(NextSurface | PrevSurface) => {
                let previous = ActionId::from_wire(action) == Some(PrevSurface);
                let (_, pane, _) = self.locate(source).context("shortcut source disappeared")?;
                let (_, _, tabs) = self.workspaces[source_workspace]
                    .leaves()
                    .into_iter()
                    .find(|(id, _, _)| *id == pane)
                    .context("shortcut pane disappeared")?;
                let current = tabs
                    .iter()
                    .position(|tab| tab.id == source)
                    .context("shortcut tab disappeared")?;
                let next = (current + if previous { tabs.len() - 1 } else { 1 }) % tabs.len();
                self.select(tabs[next].id)?;
                return self.rebuild();
            }
            Some(NextWorkspace | PrevWorkspace) => {
                let previous = ActionId::from_wire(action) == Some(PrevWorkspace);
                let indices = self.main_workspace_indices();
                if indices.is_empty() {
                    return Ok(());
                }
                let at = indices
                    .iter()
                    .position(|i| *i == self.active_workspace)
                    .unwrap_or(0);
                let next = (at + if previous { indices.len() - 1 } else { 1 }) % indices.len();
                Action::Workspace(self.workspaces[indices[next]].id)
            }
            Some(
                Workspace1 | Workspace2 | Workspace3 | Workspace4 | Workspace5 | Workspace6
                | Workspace7 | Workspace8,
            ) => {
                let index = action
                    .rsplit('-')
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(0);
                let indices = self.main_workspace_indices();
                let Some(workspace) = index
                    .checked_sub(1)
                    .and_then(|i| indices.get(i))
                    .map(|i| &self.workspaces[*i])
                else {
                    return Ok(());
                };
                Action::Workspace(workspace.id)
            }
            Some(CommandPalette) => Action::CommandPalette,
            Some(NewSurface) => Action::NewTab,
            Some(NewBrowserSurface) => Action::NewBrowser,
            Some(NewWorkspace) => Action::NewWorkspace,
            Some(NewWindow) => return self.new_window(Some(source)),
            Some(OpenTig) => return self.open_tig(source),
            Some(CopyPanePath) => return self.copy_pane_path(source),
            Some(ToggleUsagePopover) => Action::Usage,
            Some(ToggleSessionPanel) => Action::Sessions,
            Some(TerminalSearch) => Action::Find,
            Some(SearchAllTerminals) => Action::SearchAll,
            Some(TogglePaneZoom) => Action::TogglePaneZoom,
            Some(ToggleWorkspaceOverview) => Action::Overview,
            Some(ToggleFileBrowser) => Action::ShowFiles,
            Some(ToggleWorktreePanel) => Action::Worktrees,
            _ => return Ok(()),
        };
        self.action(action)
    }

    #[cfg(debug_assertions)]
    pub(super) fn test_shortcut(
        &mut self,
        source: SurfaceId,
        event: &str,
        reply: ipc::Reply,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.background_test,
            "shortcut tests require an owned hidden debug host"
        );
        anyhow::ensure!(event.len() <= 2048, "shortcut test event exceeds limit");
        let event: Value = serde_json::from_str(event)?;
        let fields = event
            .as_object()
            .context("shortcut test event must be an object")?;
        for (key, value) in fields {
            let valid = match key.as_str() {
                "type" => matches!(value.as_str(), Some("keydown" | "keyup")),
                "key" | "code" => value.as_str().is_some_and(|s| s.len() <= 64),
                "ctrlKey" | "altKey" | "shiftKey" | "metaKey" | "repeat" | "isComposing"
                | "altGraph" => value.is_boolean(),
                "keyCode" => value.as_u64().is_some_and(|n| n <= 255),
                _ => false,
            };
            anyhow::ensure!(valid, "invalid shortcut test event field: {key}");
        }
        anyhow::ensure!(
            self.pending_terminal_ui_tests.len() < 2,
            "shortcut test already pending"
        );
        let surface = self
            .surfaces
            .get(&source)
            .context("shortcut test requires a terminal surface")?;
        anyhow::ensure!(
            surface.ready && !surface.restoring,
            "shortcut test terminal is not ready"
        );
        let request = Uuid::new_v4();
        surface.send(&HostMessage::TestShortcut { request, event })?;
        self.pending_terminal_ui_tests.insert(
            request,
            PendingRead {
                surface: source,
                after: 0,
                reply,
                started: Instant::now(),
            },
        );
        Ok(())
    }
}
