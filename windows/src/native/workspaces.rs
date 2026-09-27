// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace metadata/lifecycle and native entry points. IDs survive reordering.
use super::*;
use windows_sys::Win32::System::SystemServices::{SS_CENTER, SS_CENTERIMAGE};
#[path = "metadata_panel.rs"]
mod editor;
pub(super) use editor::{EditAction, Panel};

thread_local! { static SWATCHES: RefCell<HashMap<isize, COLORREF>> = RefCell::new(HashMap::new()); }
pub(super) fn clear_swatches() {
    SWATCHES.with(|colors| colors.borrow_mut().clear());
}
pub(super) fn swatch_color(hwnd: HWND) -> Option<COLORREF> {
    SWATCHES.with(|colors| colors.borrow().get(&(hwnd as isize)).copied())
}

#[derive(Clone, Copy)]
pub(super) enum EditTarget {
    Setting(crate::settings::SettingKey),
    WorkspaceName(WorkspaceId),
    WorkspaceColor(WorkspaceId),
    TabName(SurfaceId),
}

impl App {
    pub(super) fn workspace_index(&self, id: WorkspaceId) -> anyhow::Result<usize> {
        self.workspaces
            .iter()
            .position(|w| w.id == id)
            .context("workspace not found")
    }
    pub(super) fn rename_tab(&mut self, id: SurfaceId, name: String) -> anyhow::Result<()> {
        model::validate_name(&name)?;
        let (ws, pane, _) = self.locate(id).context("tab no longer exists")?;
        self.workspaces[ws].root.rename_surface(pane, id, name);
        self.refresh_tab_title(id);
        Ok(())
    }
    pub(super) fn workspace_command(
        &mut self,
        op: WorkspaceOp,
        caller: Option<SurfaceId>,
    ) -> anyhow::Result<Value> {
        match op {
            WorkspaceOp::List => {
                return Ok(
                    json!({"workspaces":self.workspaces.iter().enumerate().map(|(index,w)|json!({"id":w.id,"name":w.name,"color":w.color,"index":index,"active":index==self.active_workspace})).collect::<Vec<_>>()}),
                )
            }
            WorkspaceOp::Current => {
                let surface = self.target(None, caller)?;
                let (ws, _, _) = self.locate(surface).unwrap();
                return Ok(json!({"workspace":self.workspaces[ws].id}));
            }
            WorkspaceOp::Focus { workspace } => {
                let index = self.workspace_index(WorkspaceId(workspace))?;
                self.select(self.workspaces[index].active())?;
                self.rebuild()?;
            }
            WorkspaceOp::Rename { workspace, name } => {
                model::validate_name(&name)?;
                let index = self.workspace_index(WorkspaceId(workspace))?;
                self.workspaces[index].name = name;
                self.rebuild_without_focus()?;
            }
            WorkspaceOp::Color {
                workspace,
                color,
                clear,
            } => {
                anyhow::ensure!(clear != color.is_some(), "provide a color or --clear");
                let color = model::parse_color(color.as_deref().unwrap_or(""))?;
                let index = self.workspace_index(WorkspaceId(workspace))?;
                self.workspaces[index].color = color;
                self.rebuild_without_focus()?;
            }
            WorkspaceOp::Reorder { workspace, index } => {
                model::reorder_workspace(
                    &mut self.workspaces,
                    &mut self.active_workspace,
                    WorkspaceId(workspace),
                    index,
                )?;
                self.rebuild_without_focus()?;
            }
            WorkspaceOp::Close { workspace } => {
                let id = WorkspaceId(workspace);
                let active = self.workspace().id == id;
                let removed =
                    model::remove_workspace(&mut self.workspaces, &mut self.active_workspace, id)?;
                if active {
                    self.zoomed = None;
                }
                for (_, _, tabs) in removed.leaves() {
                    for tab in tabs {
                        self.remove_surface(tab.id);
                    }
                }
                self.search_tick()?;
                self.rebuild_without_focus()?;
                if active {
                    self.focus_active()?;
                }
            }
        }
        Ok(json!({"ok":true}))
    }
    pub(super) fn swatch(&mut self, id: WorkspaceId, color: &str) -> anyhow::Result<()> {
        let rgb = u32::from_str_radix(&color[1..], 16)?;
        let native = ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff);
        unsafe {
            let hwnd = CreateWindowExW(
                0,
                wide("STATIC").as_ptr(),
                wide("■").as_ptr(),
                WS_CHILD | WS_VISIBLE | SS_CENTER | SS_CENTERIMAGE,
                0,
                0,
                1,
                1,
                self.window,
                (self.controls.len() + 100) as HMENU,
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null(),
            );
            checked((!hwnd.is_null()) as i32)?;
            SendMessageW(
                hwnd,
                WM_SETFONT,
                GetStockObject(DEFAULT_GUI_FONT) as WPARAM,
                1,
            );
            SWATCHES.with(|colors| colors.borrow_mut().insert(hwnd as isize, native));
            let action = Action::WorkspaceColor(id);
            CONTROL_ACTIONS
                .with(|actions| actions.borrow_mut().insert(hwnd as isize, action.clone()));
            self.controls.push(Control { hwnd, action });
        }
        Ok(())
    }
    pub(super) fn context_menu(&mut self, action: Action, x: i32, y: i32) -> anyhow::Result<()> {
        let point = (x, y);
        match action {
            Action::Workspace(id) | Action::WorkspaceColor(id) => {
                self.workspace_menu(id, Some(point))
            }
            Action::Tab(_, id) => {
                if self.popup(&["Rename tab…"], &[], point)? == 1 {
                    self.edit_metadata(EditTarget::TabName(id))?;
                } else {
                    self.focus_active()?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    pub(super) fn popup(
        &self,
        items: &[&str],
        disabled: &[usize],
        point: (i32, i32),
    ) -> anyhow::Result<usize> {
        if self.background_test {
            return Ok(0);
        }
        unsafe {
            let menu = CreatePopupMenu();
            anyhow::ensure!(!menu.is_null(), "cannot create workspace menu");
            for (i, label) in items.iter().enumerate() {
                let flags = MF_STRING
                    | if disabled.contains(&(i + 1)) {
                        MF_GRAYED
                    } else {
                        0
                    };
                if AppendMenuW(menu, flags, i + 1, wide(label).as_ptr()) == 0 {
                    DestroyMenu(menu);
                    anyhow::bail!("cannot add workspace menu item");
                }
            }
            // Route menu keyboard navigation away from the embedded terminal.
            SetFocus(self.window);
            let result = TrackPopupMenuEx(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.0,
                point.1,
                self.window,
                std::ptr::null(),
            ) as usize;
            DestroyMenu(menu);
            Ok(result)
        }
    }
    pub(super) fn workspace_menu(
        &mut self,
        id: WorkspaceId,
        point: Option<(i32, i32)>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.close_request.is_none(),
            "window is saving before close"
        );
        let index = self.workspace_index(id)?;
        let point = point.unwrap_or_else(|| {
            let mut rect = RECT::default();
            let hwnd = self
                .controls
                .iter()
                .find(|c| matches!(c.action, Action::WorkspaceMenu))
                .map_or(self.window, |c| c.hwnd);
            unsafe {
                GetWindowRect(hwnd, &mut rect);
            }
            (rect.left, rect.bottom)
        });
        let mut disabled = Vec::new();
        if index == 0 {
            disabled.push(4);
        }
        if index + 1 == self.workspaces.len() {
            disabled.push(5);
        }
        if self.workspaces.len() == 1 {
            disabled.push(6);
        }
        match self.popup(
            &[
                "Rename workspace…",
                "Workspace color…",
                "Clear color",
                "Move up",
                "Move down",
                "Close workspace…",
            ],
            &disabled,
            point,
        )? {
            1 => return self.edit_metadata(EditTarget::WorkspaceName(id)),
            2 => return self.edit_metadata(EditTarget::WorkspaceColor(id)),
            3 => {
                self.workspace_command(
                    WorkspaceOp::Color {
                        workspace: id.0,
                        color: None,
                        clear: true,
                    },
                    None,
                )?;
            }
            choice @ (4 | 5) => {
                let next = if choice == 4 {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(self.workspaces.len() - 1)
                };
                self.workspace_command(
                    WorkspaceOp::Reorder {
                        workspace: id.0,
                        index: next,
                    },
                    None,
                )?;
            }
            6 => {
                if self.workspaces.len() == 1 {
                    anyhow::bail!("cannot close the final workspace; close the window instead");
                }
                let ws = &self.workspaces[index];
                let count: usize = ws.leaves().iter().map(|(_, _, tabs)| tabs.len()).sum();
                let confirmed = unsafe {
                    MessageBoxW(
                        self.window,
                        wide(format!(
                            "Close workspace ‘{}’ and terminate its {} terminal processes?",
                            ws.name, count
                        ))
                        .as_ptr(),
                        wide("Close workspace").as_ptr(),
                        MB_YESNO | MB_DEFBUTTON2 | MB_ICONWARNING,
                    ) == IDYES
                };
                if confirmed {
                    self.workspace_command(WorkspaceOp::Close { workspace: id.0 }, None)?;
                }
            }
            _ => {}
        }
        self.focus_active()
    }
    fn metadata_text(&self, target: EditTarget) -> anyhow::Result<String> {
        match target {
            EditTarget::Setting(key) => Ok(self.settings.terminal.value(key)),
            EditTarget::WorkspaceName(id) => {
                Ok(self.workspaces[self.workspace_index(id)?].name.clone())
            }
            EditTarget::WorkspaceColor(id) => Ok(self.workspaces[self.workspace_index(id)?]
                .color
                .clone()
                .unwrap_or_default()),
            EditTarget::TabName(id) => {
                let (ws, pane, _) = self.locate(id).context("tab no longer exists")?;
                Ok(self.workspaces[ws]
                    .root
                    .surface_title(pane, id)
                    .unwrap()
                    .to_owned())
            }
        }
    }
    pub(super) fn edit_metadata(&mut self, target: EditTarget) -> anyhow::Result<()> {
        let value = self.metadata_text(target)?;
        let locked = match target {
            EditTarget::TabName(id) => self.title_locked(id)?,
            _ => false,
        };
        if self.metadata.is_none() {
            self.metadata = Some(Panel::new(self.window)?);
        }
        self.metadata
            .as_mut()
            .unwrap()
            .edit(target, &value, locked, self.background_test);
        Ok(())
    }
    fn title_locked(&self, id: SurfaceId) -> anyhow::Result<bool> {
        let (ws, _, _) = self.locate(id).context("tab no longer exists")?;
        self.workspaces[ws]
            .leaves()
            .into_iter()
            .flat_map(|(_, _, tabs)| tabs)
            .find(|t| t.id == id)
            .map(|t| t.title_locked)
            .context("tab no longer exists")
    }
    pub(super) fn metadata_action(&mut self, action: EditAction) -> anyhow::Result<()> {
        let Some(panel) = self.metadata.as_ref() else {
            return Ok(());
        };
        match action {
            EditAction::Layout => panel.layout(),
            EditAction::Close => {
                panel.hide();
                self.focus_active()?;
            }
            EditAction::Apply => {
                let target = panel.target;
                let original = panel.original.clone();
                let original_locked = panel.original_locked;
                let value = panel.value();
                if let Some(EditTarget::Setting(key)) = target {
                    let editor = panel.edit_id;
                    let result = self.settings_submit(
                        crate::command::SettingsOp::Set {
                            key,
                            value,
                            expected: Some(original),
                        },
                        None,
                        Some(editor),
                    );
                    if let Some(panel) = &self.metadata {
                        panel.status(
                            &result
                                .err()
                                .map(|e| e.to_string())
                                .unwrap_or_else(|| "Saving…".into()),
                        );
                    }
                    return Ok(());
                }
                let result = (|| -> anyhow::Result<()> {
                    anyhow::ensure!(
                        self.close_request.is_none(),
                        "window is saving before close"
                    );
                    let target = target.context("no metadata target")?;
                    let current = self.metadata_text(target)?;
                    let unchanged = if let EditTarget::TabName(id) = target {
                        let locked = self.title_locked(id)?;
                        // Ordinary shell OSC titles can change while the user types.
                        // Only a competing manual rename invalidates this pending edit.
                        locked == original_locked && (!locked || current == original)
                    } else {
                        current == original
                    };
                    anyhow::ensure!(unchanged, "This name or color changed elsewhere. Close and reopen the editor to reload it.");
                    match target {
                        EditTarget::Setting(_) => unreachable!("handled above"),
                        EditTarget::WorkspaceName(id) => {
                            self.workspace_command(
                                WorkspaceOp::Rename {
                                    workspace: id.0,
                                    name: value,
                                },
                                None,
                            )?;
                        }
                        EditTarget::WorkspaceColor(id) => {
                            self.workspace_command(
                                WorkspaceOp::Color {
                                    workspace: id.0,
                                    color: Some(value),
                                    clear: false,
                                },
                                None,
                            )?;
                        }
                        EditTarget::TabName(id) => self.rename_tab(id, value)?,
                    }
                    Ok(())
                })();
                match result {
                    Ok(()) => {
                        self.metadata.as_ref().unwrap().hide();
                        self.focus_active()?;
                    }
                    Err(error) => self.metadata.as_ref().unwrap().status(&error.to_string()),
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn queued_control_actions_keep_original_targets_when_controls_are_rebuilt() {
        let original = WorkspaceId::new();
        let replacement = WorkspaceId::new();
        let (sender, receive) = mpsc::channel();
        EVENTS.with(|events| *events.borrow_mut() = Some(EventSender { window: 0, sender }));
        CONTROL_ACTIONS.with(|actions| {
            actions
                .borrow_mut()
                .insert(123, Action::Workspace(original))
        });
        // Invoke the same callback without sending desktop keyboard/mouse input.
        unsafe {
            window_proc(std::ptr::null_mut(), WM_COMMAND, 100, 123);
        }
        CONTROL_ACTIONS.with(|actions| {
            actions
                .borrow_mut()
                .insert(123, Action::Workspace(replacement))
        });
        assert!(
            matches!(receive.try_recv().unwrap(),Event::Button(Action::Workspace(id)) if id==original)
        );
        unsafe {
            window_proc(std::ptr::null_mut(), WM_CONTEXTMENU, 123, 10 | (20 << 16));
        }
        CONTROL_ACTIONS.with(|actions| actions.borrow_mut().clear());
        assert!(
            matches!(receive.try_recv().unwrap(),Event::ContextMenu(Action::Workspace(id),10,20) if id==replacement)
        );
        EVENTS.with(|events| *events.borrow_mut() = None);
    }
}
