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

pub(super) struct PaneHeaderLayout {
    /// Linux order: zoom, split right, split down, add tab, browser, pane menu.
    pub tools: [Option<model::Rect>; 6],
    pub tabs_width: i32,
}

pub(super) fn pane_header_layout(area: model::Rect, dpi: u32) -> PaneHeaderLayout {
    let px = |value: i32| (value as f64 * dpi.max(96) as f64 / 96.0).round() as i32;
    let mut layout = PaneHeaderLayout {
        tools: [None; 6],
        tabs_width: 0,
    };
    if area.width <= 0 || area.height <= 0 {
        return layout;
    }
    let width = px(28);
    let gap = px(1);
    let tab_gap = px(4);
    let available = (area.width - px(30) - tab_gap).max(0);
    let count = ((available + gap) / (width + gap)).clamp(1, 6) as usize;
    let occupied = (count as i32 * width + (count as i32 - 1) * gap).min(area.width);
    let start = area.x + area.width - occupied;
    layout.tabs_width = (area.width - occupied - tab_gap).max(0);
    for (offset, slot) in layout.tools[6 - count..].iter_mut().enumerate() {
        let x = start + offset as i32 * (width + gap);
        *slot = Some(model::Rect {
            x,
            y: area.y,
            width: width.min(area.x + area.width - x),
            height: width.min(area.height),
        });
    }
    layout
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
            | Action::Overview
            | Action::CommandPalette
            | Action::ShowFiles
            | Action::SearchAll
            | Action::OpenEditor
            | Action::Notifications => chrome::Role::Icon {
                kind: match action {
                    Action::Overview => chrome::ChromeIcon::Overview,
                    Action::CommandPalette => chrome::ChromeIcon::CommandPalette,
                    Action::Settings => chrome::ChromeIcon::Settings,
                    Action::ShowFiles => chrome::ChromeIcon::Files,
                    Action::SearchAll => chrome::ChromeIcon::Search,
                    Action::OpenEditor => chrome::ChromeIcon::OpenFile,
                    _ => chrome::ChromeIcon::Notifications,
                },
                marked: matches!(action, Action::Notifications)
                    && self.notifications.store.unread_count() > 0,
            },
            Action::PaneZoom(pane, _) => chrome::Role::Icon {
                kind: if self.zoomed == Some(pane) {
                    chrome::ChromeIcon::Restore
                } else {
                    chrome::ChromeIcon::Maximize
                },
                marked: self.zoomed == Some(pane),
            },
            Action::PaneSplitRight(..)
            | Action::PaneSplitDown(..)
            | Action::PaneBrowser(..)
            | Action::PaneMenu(..) => chrome::Role::Icon {
                kind: match action {
                    Action::PaneSplitRight(..) => chrome::ChromeIcon::SplitRight,
                    Action::PaneSplitDown(..) => chrome::ChromeIcon::SplitDown,
                    Action::PaneBrowser(..) => chrome::ChromeIcon::Browser,
                    _ => chrome::ChromeIcon::More,
                },
                marked: false,
            },
            Action::PaneAdd(..) | Action::TabClose(..) | Action::NewWorkspace => chrome::Role::Tool,
            _ => chrome::Role::Button,
        }
    }
    pub(super) fn refresh_chrome_metadata(&self) {
        for control in &self.controls {
            if let Action::Workspace(id) = control.action {
                if let Some(label) = self.workspace_caption(id) {
                    set_caption(control.hwnd, &label);
                }
            } else if let Action::PaneZoom(pane, _) = control.action {
                set_caption(
                    control.hwnd,
                    if self.zoomed == Some(pane) {
                        "Restore pane"
                    } else {
                        "Maximize pane"
                    },
                );
            }
            chrome::set_role(control.hwnd, self.chrome_role(&control.action));
        }
    }
    pub(super) fn pane_surface_ids(&self, pane: PaneId) -> anyhow::Result<Vec<SurfaceId>> {
        self.workspaces
            .iter()
            .flat_map(|workspace| workspace.leaves())
            .find(|(id, _, _)| *id == pane)
            .map(|(_, _, tabs)| tabs.into_iter().map(|tab| tab.id).collect())
            .context("Pane no longer exists")
    }
    /// Seal every editor before removing any tab or terminal process in the pane.
    /// True means the asynchronous editor barrier owns the eventual reply.
    pub(super) fn close_pane(
        &mut self,
        pane: PaneId,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(
            !self.close_accepted && self.close_request.is_none(),
            "window is saving before close"
        );
        let surfaces = self.pane_surface_ids(pane)?;
        let workspace = self.locate(surfaces[0]).context("Pane no longer exists")?.0;
        anyhow::ensure!(
            self.workspaces[workspace].leaves().len() > 1,
            "cannot close the final pane; close its workspace or window instead"
        );
        anyhow::ensure!(
            self.editor_open_pending.is_empty(),
            "wait for editor open preparation to finish before closing a pane"
        );
        if self.editor_guard(
            super::editor::Operation::Pane {
                pane,
                surfaces: surfaces.clone(),
            },
            reply,
        )? {
            return Ok(true);
        }
        // Removing a cloned tree is atomic with respect to the UI model. No
        // terminal or editor is dropped until all validation has succeeded.
        let next = match self.workspaces[workspace].root.clone().remove_leaf(pane) {
            flowmux_core::RemoveOutcome::Replaced(root) => root,
            _ => anyhow::bail!("Pane changed before close"),
        };
        let focused = self.workspaces[workspace].focused == pane;
        self.workspaces[workspace].root = next;
        if focused {
            self.workspaces[workspace].focused = self.workspaces[workspace]
                .root
                .first_leaf_id()
                .expect("another pane remains after close");
        }
        if self.zoomed == Some(pane) {
            self.zoomed = None;
        }
        for surface in surfaces {
            self.remove_surface(surface);
        }
        self.search_tick()?;
        self.rebuild_without_focus()?;
        if focused && self.active_workspace == workspace {
            self.focus_active()?;
        }
        Ok(false)
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
        let disabled = if self.workspaces[workspace].leaves().len() == 1 {
            vec![1]
        } else {
            vec![]
        };
        let hwnd = self
            .controls
            .iter()
            .find(|c| matches!(c.action,Action::PaneMenu(p,s) if p==pane&&s==surface))
            .map_or(self.window, |c| c.hwnd);
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut rect);
        }
        if self.popup(&["Close Pane"], &disabled, (rect.left, rect.bottom))? == 1 {
            anyhow::ensure!(
                self.locate(surface)
                    .is_some_and(|(_, current, _)| current == pane),
                "Pane source changed while menu was open"
            );
            self.close_pane(pane, None)?;
            return self.focus_active();
        }
        self.focus_active()
    }
    fn tab_actions_menu(
        &mut self,
        pane: PaneId,
        surface: SurfaceId,
        point: (i32, i32),
    ) -> anyhow::Result<()> {
        let (workspace, current, _) = self.locate(surface).context("Tab no longer exists")?;
        anyhow::ensure!(current == pane, "Tab moved before opening its menu");
        let tabs = self.workspaces[workspace]
            .leaves()
            .into_iter()
            .find(|(id, _, _)| *id == pane)
            .context("Pane no longer exists")?
            .2;
        let mut labels: Vec<String> = [
            "Rename tab…",
            "Move tab…",
            "Close tab",
            "Find in terminal",
            "Search all terminals",
            "Open file…",
            "New terminal tab",
            "New browser tab",
            "Split right",
            "Split down",
            "Maximize / restore pane",
            "Workspace actions…",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        labels.extend(
            tabs.iter()
                .map(|tab| format!("Switch to: {}", tab.title.replace('&', "&&"))),
        );
        let choice = self.popup(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
            &[],
            point,
        )?;
        if choice == 0 {
            return self.focus_active();
        }
        anyhow::ensure!(
            self.locate(surface)
                .is_some_and(|(_, current, _)| current == pane),
            "Tab moved while menu was open"
        );
        if choice >= 13 {
            let target = tabs
                .get(choice - 13)
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
        if choice == 1 {
            return self.edit_metadata(EditTarget::TabName(surface));
        }
        self.action(match choice {
            2 => Action::MoveTabMenu,
            3 => Action::CloseTab,
            4 => Action::Find,
            5 => Action::SearchAll,
            6 => Action::OpenEditor,
            7 => Action::NewTab,
            8 => Action::NewBrowser,
            9 => Action::Vertical,
            10 => Action::Horizontal,
            11 => Action::TogglePaneZoom,
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
                Action::PaneZoom(pane,surface)=>("pane_zoom",Some(pane),Some(surface),None,false),
                Action::PaneSplitRight(pane,surface)=>("pane_split_right",Some(pane),Some(surface),None,false),
                Action::PaneSplitDown(pane,surface)=>("pane_split_down",Some(pane),Some(surface),None,false),
                Action::PaneBrowser(pane,surface)=>("pane_browser",Some(pane),Some(surface),None,false),
                Action::SidebarScroll(d)=>(if d<0 {"sidebar_previous"}else{"sidebar_next"},None,None,None,false),
                Action::NewWorkspace=>("workspace_add",None,None,None,false),
                Action::WorkspaceMenu=>("workspace_header",None,None,None,false),
                Action::Settings=>("settings",None,None,None,false),
                Action::CommandPalette=>("command_palette",None,None,None,false),
                Action::Overview=>("overview",None,None,None,false),
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
        json!({"theme":self.settings.terminal.theme,"dpi":dpi,"sidebar_offset":self.sidebar_offset,"sidebar_width_dip":self.sidebar_width_dip,"sidebar_actual_width":self.sidebar_width(client.right,dpi),"sidebar_dragging":matches!(self.drag,Some(panes::Drag::Sidebar { .. })),"tab_dragging":matches!(self.drag,Some(panes::Drag::Tab { .. })),"tab_drop_preview":self.drop_preview.as_ref().map(chrome::DropPreview::diagnostics),"sidebar_gutter_width":(4.0*dpi as f64/96.0).round() as i32,"sidebar_footer_height_dip":36,"sidebar_list_top":layout.list_top,"sidebar_list_bottom":layout.list_bottom,"sidebar_footer_top":layout.footer_top,"sidebar_capacity":layout.capacity,"sidebar_pager_visible":layout.pager,"workspace_row_height_dip":58,"controls":controls})
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
            Action::Tab(pane, surface) | Action::TabClose(pane, surface) => {
                self.tab_actions_menu(pane, surface, point)
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
                "Command Palette…",
            ],
            &disabled,
            point,
        )? {
            7 => return self.action(Action::CommandPalette),
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
