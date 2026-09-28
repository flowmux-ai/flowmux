// SPDX-License-Identifier: GPL-3.0-or-later
//! Single-surface windows share the process's IPC broker and live sessions.
use super::*;

impl App {
    pub(super) fn is_detached_workspace(&self, workspace: WorkspaceId) -> bool {
        self.detached
            .values()
            .any(|window| window.workspace == workspace)
    }

    pub(super) fn main_workspace_indices(&self) -> Vec<usize> {
        self.workspaces
            .iter()
            .enumerate()
            .filter_map(|(i, workspace)| (!self.is_detached_workspace(workspace.id)).then_some(i))
            .collect()
    }

    pub(super) fn main_surface_ids(&self) -> Vec<SurfaceId> {
        let mut surfaces: Vec<_> = self
            .workspaces
            .iter()
            .filter(|workspace| !self.is_detached_workspace(workspace.id))
            .flat_map(|workspace| {
                workspace
                    .leaves()
                    .into_iter()
                    .flat_map(|(_, _, tabs)| tabs.into_iter().map(|tab| tab.id))
            })
            .collect();
        surfaces.sort_by_key(|surface| surface.0);
        surfaces
    }

    pub(super) fn ensure_attached(&self, surface: SurfaceId) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.detached.contains_key(&surface),
            "this action requires a tab in the main window; move the tab back first"
        );
        Ok(())
    }

    pub(super) fn surface_window(&self, surface: SurfaceId) -> HWND {
        self.detached
            .get(&surface)
            .map_or(self.window, |window| window.window)
    }

    fn normalize_main_workspace(&mut self) {
        let indices = self.main_workspace_indices();
        if !indices.contains(&self.active_workspace) {
            self.active_workspace = indices.first().copied().unwrap_or(0);
        }
    }

    pub(super) fn detach_tab(&mut self, surface: SurfaceId) -> anyhow::Result<()> {
        self.files_operation_guard()?;
        anyhow::ensure!(
            self.editor_barrier.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && self.pending_save.is_none(),
            "window is busy"
        );
        self.ensure_attached(surface)?;
        anyhow::ensure!(
            self.surfaces
                .get(&surface)
                .is_some_and(|terminal| terminal.ready && !terminal.restoring),
            "only a ready terminal can currently move to a separate window"
        );
        let main = self.workspace().id;
        let mut candidate = self.workspaces.clone();
        let index = model::detach_surface(&mut candidate, surface)?;
        let workspace = &candidate[index];
        let title = workspace
            .root
            .surface_title(workspace.focused, surface)
            .unwrap_or("Terminal");
        let window = detached::Window::new(surface, workspace.id, title)?;
        chrome::window_theme(window.window, self.settings.terminal.theme);
        // Native construction can pump messages; domain state is still untouched.
        self.surfaces[&surface].holder.reparent(window.window)?;
        self.workspaces = candidate;
        self.detached.insert(surface, window);
        self.active_workspace = self
            .workspaces
            .iter()
            .position(|workspace| workspace.id == main)
            .unwrap_or(0);
        self.normalize_main_workspace();
        self.detached_focus = Some(surface);
        self.zoomed = None;
        let layout = self.rebuild_without_focus();
        self.detached[&surface].show(self.background_test);
        layout?;
        self.select(surface)?;
        self.focus_active()
    }

    pub(super) fn detached_layout(&mut self, surface: SurfaceId) -> anyhow::Result<()> {
        let Some(window) = self.detached.get(&surface) else {
            return Ok(());
        };
        window.layout()?;
        let area = window.area()?;
        let terminal = self
            .surfaces
            .get_mut(&surface)
            .context("detached terminal disappeared")?;
        terminal.holder.layout(Some(area), self.background_test)?;
        terminal
            .view
            .set_bounds(bounds(model::Rect { x: 0, y: 0, ..area }))?;
        unsafe {
            terminal
                .view
                .controller()
                .NotifyParentWindowPositionChanged()?;
        }
        if !terminal.visible {
            terminal.view.set_visible(true)?;
            terminal.visible = true;
            terminal.send(&HostMessage::Visibility { visible: true })?;
        }
        Ok(())
    }

    pub(super) fn detached_event(
        &mut self,
        surface: SurfaceId,
        signal: detached::Signal,
    ) -> anyhow::Result<()> {
        if !self.detached.contains_key(&surface) {
            return Ok(());
        }
        match signal {
            detached::Signal::Layout => self.detached_layout(surface),
            detached::Signal::Moved => {
                if let Some(terminal) = self.surfaces.get(&surface) {
                    unsafe {
                        terminal
                            .view
                            .controller()
                            .NotifyParentWindowPositionChanged()?;
                    }
                }
                Ok(())
            }
            detached::Signal::Activated => {
                self.detached_focus = Some(surface);
                self.focus_active()
            }
            detached::Signal::Close => self.close_detached(surface),
        }
    }

    pub(super) fn close_detached(&mut self, surface: SurfaceId) -> anyhow::Result<()> {
        self.files_operation_guard()?;
        let window = self
            .detached
            .get(&surface)
            .context("separate window no longer exists")?;
        if self.workspaces.len() == 1 {
            return self.request_close(CloseRequest::Native);
        }
        anyhow::ensure!(
            self.pending_save.is_none()
                && self.editor_barrier.is_none()
                && self.close_request.is_none(),
            "window is busy"
        );
        let workspace = window.workspace;
        model::remove_workspace(&mut self.workspaces, &mut self.active_workspace, workspace)?;
        // Close the WebView and holder before destroying their top-level HWND.
        self.remove_surface(surface);
        self.normalize_main_workspace();
        if self.detached_focus == Some(surface) {
            self.detached_focus = if self.main_closed {
                self.detached.keys().next().copied()
            } else {
                None
            };
        }
        self.rebuild_without_focus()
    }

    pub(super) fn close_main_window(&mut self) -> anyhow::Result<()> {
        if self.main_closed {
            return Ok(());
        }
        if self.detached.is_empty() {
            return self.request_close(CloseRequest::Native);
        }
        self.files_operation_guard()?;
        anyhow::ensure!(
            self.pending_save.is_none() && self.close_request.is_none(),
            "window is busy"
        );
        let surfaces = self.main_surface_ids();
        if self.editor_guard(
            editor::Operation::MainWindow {
                surfaces: surfaces.clone(),
            },
            None,
        )? {
            return Ok(());
        }
        self.cancel_drag();
        self.files_shutdown();
        self.editor_cancel_opens(None, "main window closed before editor Open completed");
        self.options.take();
        self.metadata.take();
        let detached: Vec<_> = self
            .detached
            .values()
            .map(|window| window.workspace)
            .collect();
        self.workspaces
            .retain(|workspace| detached.contains(&workspace.id));
        self.active_workspace = 0;
        for surface in surfaces {
            self.remove_surface(surface);
        }
        for control in self.controls.drain(..) {
            chrome::unregister(control.hwnd);
            unsafe {
                DestroyWindow(control.hwnd);
            }
        }
        CONTROL_ACTIONS.with(|actions| actions.borrow_mut().clear());
        self.main_closed = true;
        self.detached_focus = self.detached.keys().next().copied();
        unsafe {
            ShowWindow(self.window, SW_HIDE);
        }
        // Keep the dispatch HWND, timers, IPC endpoint and contexts alive until
        // the last separate window closes. Save only the remaining sessions.
        if self.store.is_some() {
            self.begin_save(None)?;
        }
        Ok(())
    }
}
