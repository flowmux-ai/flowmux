// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace metadata/lifecycle and native entry points. IDs survive reordering.
use super::*;
#[path = "metadata_panel.rs"]
mod editor;
pub(super) use editor::{EditAction, Panel};

pub(super) fn set_caption(window: HWND, caption: &str) {
    let escaped = caption.replace('&', "&&");
    unsafe {
        let length = GetWindowTextLengthW(window).max(0) as usize;
        let mut current = vec![0u16; length + 1];
        let read =
            GetWindowTextW(window, current.as_mut_ptr(), current.len() as i32).max(0) as usize;
        if current[..read] != escaped.encode_utf16().collect::<Vec<_>>() {
            SetWindowTextW(window, wide(&escaped).as_ptr());
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum EditTarget {
    WorkspaceName(WorkspaceId),
    WorkspaceColor(WorkspaceId),
    TabName(SurfaceId),
}

pub(super) struct SidebarLayout {
    pub list_top: i32,
    pub list_bottom: i32,
    pub footer_top: i32,
    pub capacity: usize,
    pub pager: bool,
}
impl App {
    pub(super) fn sidebar_layout(&self, height: i32, dpi: u32) -> SidebarLayout {
        let px = |n: i32| (n as f64 * dpi.max(96) as f64 / 96.0).round() as i32;
        let footer_top = (height - px(36)).max(0);
        let list_top = px(40).min(footer_top);
        let without_pager = ((footer_top - list_top) / px(58).max(1)).max(0) as usize;
        let pager = self.workspaces.len() > without_pager;
        let list_bottom = (footer_top - if pager { px(28) } else { 0 }).max(list_top);
        SidebarLayout {
            list_top,
            list_bottom,
            footer_top,
            capacity: ((list_bottom - list_top) / px(58).max(1)).max(0) as usize,
            pager,
        }
    }
    pub(super) fn workspace_caption(&self, id: WorkspaceId) -> Option<String> {
        let workspace = self.workspaces.iter().find(|w| w.id == id)?;
        let cwd = self
            .locate(workspace.active())
            .map(|(_, _, cwd)| cwd)
            .unwrap_or_else(|| workspace.cwd.clone());
        let count = self
            .notifications
            .store
            .entries()
            .iter()
            .filter(|entry| {
                !entry.read
                    && entry
                        .surface
                        .and_then(|s| self.locate(s))
                        .map(|(i, _, _)| self.workspaces[i].id)
                        .or(entry.workspace)
                        == Some(id)
            })
            .count();
        let prefix = if count > 0 {
            format!("[{count}] ")
        } else {
            String::new()
        };
        Some(format!("{prefix}{}\n{}", workspace.name, cwd.display()))
    }
    pub(super) fn chrome_role(&self, action: &Action) -> chrome::Role {
        match *action {
            Action::Workspace(id) => {
                let color = self
                    .workspaces
                    .iter()
                    .find(|w| w.id == id)
                    .and_then(|w| w.color.as_deref())
                    .and_then(|color| u32::from_str_radix(color.trim_start_matches('#'), 16).ok())
                    .map(|rgb| ((rgb & 0xff) << 16) | (rgb & 0xff00) | ((rgb >> 16) & 0xff));
                chrome::Role::Workspace {
                    selected: self.workspace().id == id,
                    color,
                }
            }
            Action::Tab(pane, surface) => {
                let kind = self
                    .workspaces
                    .iter()
                    .flat_map(|w| w.leaves())
                    .flat_map(|(_, _, tabs)| tabs)
                    .find(|tab| tab.id == surface)
                    .map_or(chrome::SurfaceIcon::Terminal, |tab| match tab.kind {
                        SurfaceKind::Browser { .. } => chrome::SurfaceIcon::Browser,
                        SurfaceKind::Editor { .. } => chrome::SurfaceIcon::Editor,
                        _ => chrome::SurfaceIcon::Terminal,
                    });
                chrome::Role::Tab {
                    selected: self.workspace().root.active_surface_id(pane) == Some(surface),
                    focused: self.workspace().focused == pane,
                    kind,
                }
            }
            Action::Settings
            | Action::ShowFiles
            | Action::SearchAll
            | Action::OpenEditor
            | Action::Notifications => chrome::Role::Icon {
                kind: match action {
                    Action::Settings => chrome::ChromeIcon::Settings,
                    Action::ShowFiles => chrome::ChromeIcon::Files,
                    Action::SearchAll => chrome::ChromeIcon::Search,
                    Action::OpenEditor => chrome::ChromeIcon::OpenFile,
                    _ => chrome::ChromeIcon::Notifications,
                },
                marked: matches!(action, Action::Notifications)
                    && self.notifications.store.unread_count() > 0,
            },
            Action::PaneAdd(..)
            | Action::PaneMenu(..)
            | Action::TabClose(..)
            | Action::NewWorkspace => chrome::Role::Tool,
            _ => chrome::Role::Button,
        }
    }
    pub(super) fn refresh_chrome_metadata(&self) {
        for control in &self.controls {
            if let Action::Workspace(id) = control.action {
                if let Some(label) = self.workspace_caption(id) {
                    set_caption(control.hwnd, &label);
                }
            }
            chrome::set_role(control.hwnd, self.chrome_role(&control.action));
        }
    }
    pub(super) fn pane_actions_menu(
        &mut self,
        pane: PaneId,
        surface: SurfaceId,
    ) -> anyhow::Result<()> {
        let (workspace, current, _) = self
            .locate(surface)
            .context("Pane source no longer exists")?;
        anyhow::ensure!(current == pane, "Pane source moved");
        let tabs = self.workspaces[workspace]
            .leaves()
            .into_iter()
            .find(|(id, _, _)| *id == pane)
            .context("Pane no longer exists")?
            .2;
        let mut labels = vec![
            "New terminal tab".to_owned(),
            "New browser tab".into(),
            "Open file…".into(),
            "Split right".into(),
            "Split down".into(),
            "Find in terminal".into(),
            "Search all terminals".into(),
            "Maximize / restore pane".into(),
            "Move tab…".into(),
            "Close tab".into(),
            "Workspace actions…".into(),
        ];
        labels.extend(
            tabs.iter()
                .map(|tab| format!("Switch to: {}", tab.title.replace('&', "&&"))),
        );
        let hwnd = self
            .controls
            .iter()
            .find(|c| matches!(c.action,Action::PaneMenu(p,s) if p==pane&&s==surface))
            .map_or(self.window, |c| c.hwnd);
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut rect);
        }
        let choice = self.popup(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            &[],
            (rect.left, rect.bottom),
        )?;
        if choice == 0 {
            return self.focus_active();
        }
        anyhow::ensure!(
            self.locate(surface)
                .is_some_and(|(_, current, _)| current == pane),
            "Pane source changed while menu was open"
        );
        if choice >= 12 {
            let target = tabs
                .get(choice - 12)
                .context("Tab selection no longer exists")?
                .id;
            anyhow::ensure!(
                self.locate(target)
                    .is_some_and(|(_, current, _)| current == pane),
                "Selected tab moved"
            );
            self.select(target)?;
            return self.rebuild();
        }
        self.select(surface)?;
        self.action(match choice {
            1 => Action::NewTab,
            2 => Action::NewBrowser,
            3 => Action::OpenEditor,
            4 => Action::Vertical,
            5 => Action::Horizontal,
            6 => Action::Find,
            7 => Action::SearchAll,
            8 => Action::TogglePaneZoom,
            9 => Action::MoveTabMenu,
            10 => Action::CloseTab,
            _ => Action::WorkspaceMenu,
        })
    }
    pub(super) fn chrome_status(&self) -> Value {
        let controls=self.controls.iter().map(|control| {
            let (kind,pane,surface,workspace,selected)=match control.action {
                Action::Workspace(id)=>("workspace",None,None,Some(id),id==self.workspace().id),
                Action::Tab(pane,surface)=>("tab",Some(pane),Some(surface),None,self.workspace().root.active_surface_id(pane)==Some(surface)),
                Action::TabClose(pane,surface)=>("tab_close",Some(pane),Some(surface),None,false),
                Action::PaneAdd(pane,surface)=>("pane_add",Some(pane),Some(surface),None,false),
                Action::PaneMenu(pane,surface)=>("pane_menu",Some(pane),Some(surface),None,false),
                Action::SidebarScroll(d)=>(if d<0 {"sidebar_previous"}else{"sidebar_next"},None,None,None,false),
                Action::NewWorkspace=>("workspace_add",None,None,None,false),
                Action::WorkspaceMenu=>("workspace_header",None,None,None,false),
                Action::Settings=>("settings",None,None,None,false),
                Action::ShowFiles=>("files",None,None,None,false),
                Action::SearchAll=>("search_all",None,None,None,false),
                Action::OpenEditor=>("open_file",None,None,None,false),
                Action::Notifications=>("notifications",None,None,None,false),
                _=>("button",None,None,None,false),
            };
            unsafe {
                let mut rect=RECT::default(); GetWindowRect(control.hwnd,&mut rect);
                let mut top=POINT{x:rect.left,y:rect.top};ScreenToClient(self.window,&mut top);
                let length=GetWindowTextLengthW(control.hwnd).clamp(0,1024) as usize;
                let mut label=vec![0u16;length+1];let read=GetWindowTextW(control.hwnd,label.as_mut_ptr(),label.len() as i32).max(0) as usize;
                json!({"handle":control.hwnd as usize,"tooltip":chrome::tooltip_text(control.hwnd),"kind":kind,"pane":pane,"surface":surface,"workspace":workspace,"selected":selected,"focused":pane.is_some_and(|p|p==self.workspace().focused),"label":String::from_utf16_lossy(&label[..read]),"layout_visible":GetWindowLongPtrW(control.hwnd,GWL_STYLE) as u32&WS_VISIBLE!=0,"native_visible":IsWindowVisible(control.hwnd)!=0,"rect":{"x":top.x,"y":top.y,"width":rect.right-rect.left,"height":rect.bottom-rect.top}})
            }
        }).collect::<Vec<_>>();
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let mut client = RECT::default();
        unsafe {
            GetClientRect(self.window, &mut client);
        }
        let layout = self.sidebar_layout(client.bottom, dpi);
        json!({"theme":self.settings.terminal.theme,"dpi":dpi,"sidebar_offset":self.sidebar_offset,"sidebar_width_dip":self.sidebar_width_dip,"sidebar_actual_width":self.sidebar_width(client.right,dpi),"sidebar_dragging":matches!(self.drag,Some(panes::Drag::Sidebar { .. })),"sidebar_gutter_width":(4.0*dpi as f64/96.0).round() as i32,"sidebar_footer_height_dip":36,"sidebar_list_top":layout.list_top,"sidebar_list_bottom":layout.list_bottom,"sidebar_footer_top":layout.footer_top,"sidebar_capacity":layout.capacity,"sidebar_pager_visible":layout.pager,"workspace_row_height_dip":58,"controls":controls})
    }
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
                self.refresh_chrome_metadata();
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
                self.refresh_chrome_metadata();
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
                if self.editor_guard(super::editor::Operation::Workspace(id), None)? {
                    return Ok(json!({"pending":true}));
                }
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
    pub(super) fn context_menu(&mut self, action: Action, x: i32, y: i32) -> anyhow::Result<()> {
        let point = (x, y);
        match action {
            Action::NewTab => self.shell_menu(point),
            Action::PaneAdd(pane, surface) => {
                anyhow::ensure!(
                    self.locate(surface)
                        .is_some_and(|(_, current, _)| current == pane),
                    "Pane source changed before shell selection"
                );
                self.select(surface)?;
                self.shell_menu(point)
            }
            Action::Workspace(id) => self.workspace_menu(id, Some(point)),
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
