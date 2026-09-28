// SPDX-License-Identifier: GPL-3.0-or-later
//! Themed Linux menus, captured by surface/workspace identity rather than row index.
use super::*;
#[path = "tab_menu_panel.rs"]
mod panel;
pub(super) use panel::UiAction;

#[derive(Clone, Copy)]
enum MenuAction {
    Folder,
    Copy,
    Move,
    Destination(WorkspaceId),
    NewWorkspace,
    RenameWorkspace,
    WorkspaceColor,
    CloseWorkspace,
    CloseAllWorkspaces,
    ClosePane,
    Separator,
    Unsupported,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Tab,
    Workspace,
    Creation,
    Pane,
}
#[derive(Clone)]
struct Entry {
    label: String,
    enabled: bool,
    action: MenuAction,
}
pub(super) struct Menu {
    kind: Kind,
    surface: Option<SurfaceId>,
    pane: Option<PaneId>,
    workspace: Option<WorkspaceId>,
    owner: HWND,
    copy_text: String,
    folder: Option<PathBuf>,
    menu: panel::Panel,
    submenu: Option<panel::Panel>,
    destinations: Vec<Entry>,
}
impl Menu {
    #[cfg(debug_assertions)]
    pub(super) fn capture_window(&self) -> HWND {
        self.submenu.as_ref().unwrap_or(&self.menu).window
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.submenu
            .as_ref()
            .is_some_and(|p| p.handle_message(message))
            || self.menu.handle_message(message)
    }
    pub(super) fn diagnostics(&self) -> Value {
        let kind = match self.kind {
            Kind::Tab => "tab",
            Kind::Workspace => "workspace",
            Kind::Creation => "creation",
            Kind::Pane => "pane",
        };
        json!({"kind":kind,"surface":self.surface,"pane":self.pane,"workspace":self.workspace,
            "copy_text":self.copy_text,"folder":self.folder,"menu":self.menu.diagnostics(),
            "submenu":self.submenu.as_ref().map(panel::Panel::diagnostics)})
    }
}
impl Drop for Menu {
    fn drop(&mut self) {
        // The submenu is owned by the root popup, so release it first.
        self.submenu.take();
    }
}
impl App {
    pub(super) fn refresh_terminal_menu(&mut self, source: SurfaceId) -> anyhow::Result<()> {
        let (workspace, pane, _) = self.locate(source).context("Terminal no longer exists")?;
        let split = !self.is_detached_workspace(self.workspaces[workspace].id);
        let close = split && self.workspaces[workspace].leaves().len() > 1;
        let terminal = self
            .surfaces
            .get_mut(&source)
            .context("Terminal no longer exists")?;
        let capabilities = (pane, split, close);
        if terminal.menu_capabilities != Some(capabilities) {
            terminal.send(&HostMessage::TerminalMenuState {
                pane: pane.0,
                split,
                close,
            })?;
            terminal.menu_capabilities = Some(capabilities);
        }
        Ok(())
    }

    pub(super) fn terminal_menu_action(
        &mut self,
        source: SurfaceId,
        captured_pane: PaneId,
        action: crate::protocol::TerminalMenuAction,
    ) -> anyhow::Result<()> {
        let Some(terminal) = self.surfaces.get(&source) else {
            return Ok(());
        };
        let (workspace, pane, cwd) = self.locate(source).context("Terminal no longer exists")?;
        let owner = self.surface_window(source);
        // A right-click may come from another visible pane. Bind the action to
        // that surface and reject hidden tabs and menus left over after a move.
        if pane != captured_pane
            || !terminal.visible
            || !terminal.ready
            || terminal.restoring
            || self.workspaces[workspace].root.active_surface_id(pane) != Some(source)
            || self.close_request.is_some()
            || self.close_accepted
            || self.closing
            || self.editor_barrier.is_some()
            || self.overview.is_open()
            || self.command_palette.is_open()
            || unsafe { IsWindowEnabled(owner) } == 0
        {
            return Ok(());
        }
        use crate::protocol::TerminalMenuAction::*;
        match action {
            SplitRight | SplitDown => {
                self.files_operation_guard()?;
                self.new_terminal(
                    source,
                    None,
                    None,
                    shells::NewTerminal::Split(if matches!(action, SplitRight) {
                        SplitDirection::Vertical
                    } else {
                        SplitDirection::Horizontal
                    }),
                )?;
            }
            ClosePane => {
                self.close_pane(pane, None)?;
            }
            CopyPath => {
                anyhow::ensure!(
                    !self.background_test,
                    "Clipboard access is disabled in background hosts"
                );
                copy_text(owner, &cwd.to_string_lossy())?;
            }
        }
        Ok(())
    }

    fn tab_copy_text(&self, surface: SurfaceId) -> anyhow::Result<(String, Option<PathBuf>)> {
        let (workspace, pane, cwd) = self.locate(surface).context("Tab no longer exists")?;
        let tab = self.workspaces[workspace]
            .leaves()
            .into_iter()
            .find(|(id, _, _)| *id == pane)
            .and_then(|(_, _, tabs)| tabs.into_iter().find(|tab| tab.id == surface))
            .context("Tab no longer exists")?;
        match &tab.kind {
            SurfaceKind::Terminal { .. } => Ok((cwd.to_string_lossy().into_owned(), Some(cwd))),
            SurfaceKind::Editor { workspace_root, .. } => {
                Ok((workspace_root.to_string_lossy().into_owned(), None))
            }
            SurfaceKind::Browser { initial_url } => {
                let url = self
                    .browsers
                    .get(&surface)
                    .map(|browser| browser.view.url())
                    .transpose()?
                    .or_else(|| initial_url.clone())
                    .unwrap_or_default();
                Ok((url, None))
            }
            _ => anyhow::bail!("Tab has no path or URL"),
        }
    }
    pub(super) fn show_tab_menu(
        &mut self,
        pane: PaneId,
        surface: SurfaceId,
        point: (i32, i32),
    ) -> anyhow::Result<()> {
        let (workspace, current, _) = self.locate(surface).context("Tab no longer exists")?;
        anyhow::ensure!(current == pane, "Tab moved before opening its menu");
        let workspace = self.workspaces[workspace].id;
        let (copy_text, folder) = self.tab_copy_text(surface)?;
        let destinations: Vec<_> = self
            .main_workspace_indices()
            .into_iter()
            .enumerate()
            .filter_map(|(i, index)| {
                let target = &self.workspaces[index];
                (target.id != workspace).then(|| Entry {
                    label: format!("{}. {}", i + 1, target.name),
                    enabled: true,
                    action: MenuAction::Destination(target.id),
                })
            })
            .collect();
        let mut entries = Vec::new();
        if folder.is_some() {
            entries.push(Entry {
                label: "Show in folder".into(),
                enabled: true,
                action: MenuAction::Folder,
            });
        }
        entries.push(Entry {
            label: if folder.is_some() {
                "Copy path"
            } else {
                "Copy URL"
            }
            .into(),
            enabled: !copy_text.is_empty(),
            action: MenuAction::Copy,
        });
        entries.push(Entry {
            label: "Move".into(),
            enabled: !destinations.is_empty(),
            action: MenuAction::Move,
        });
        self.show_context_menu(Kind::Tab, Some(surface), entries, destinations, point)
    }
    pub(super) fn show_workspace_menu(
        &mut self,
        workspace: WorkspaceId,
        point: (i32, i32),
        creation: bool,
    ) -> anyhow::Result<()> {
        let index = self.workspace_index(workspace)?;
        let surface = self.workspaces[index].active();
        let mut rows = vec![
            ("New workspace", true, MenuAction::NewWorkspace),
            ("New SSH Workspace", false, MenuAction::Unsupported),
        ];
        if !creation {
            rows.extend([
                ("", false, MenuAction::Separator),
                ("Change tab name", true, MenuAction::RenameWorkspace),
                ("Change color…", true, MenuAction::WorkspaceColor),
                ("", false, MenuAction::Separator),
                ("Close tab", true, MenuAction::CloseWorkspace),
                ("Close all tabs", true, MenuAction::CloseAllWorkspaces),
                ("", false, MenuAction::Separator),
                ("Show in folder", true, MenuAction::Folder),
                (
                    "Copy path",
                    !self.tab_copy_text(surface)?.0.is_empty(),
                    MenuAction::Copy,
                ),
            ]);
        }
        let entries = rows
            .into_iter()
            .map(|(label, enabled, action)| Entry {
                label: label.into(),
                enabled,
                action,
            })
            .collect();
        self.show_context_menu(
            if creation {
                Kind::Creation
            } else {
                Kind::Workspace
            },
            Some(surface),
            entries,
            Vec::new(),
            point,
        )
    }
    pub(super) fn show_pane_menu(
        &mut self,
        pane: PaneId,
        surface: SurfaceId,
        point: (i32, i32),
    ) -> anyhow::Result<()> {
        let (workspace, current, _) = self.locate(surface).context("Pane no longer exists")?;
        anyhow::ensure!(current == pane, "Pane source moved");
        let entries = vec![Entry {
            label: "Close Pane".into(),
            enabled: self.workspaces[workspace].leaves().len() > 1,
            action: MenuAction::ClosePane,
        }];
        self.show_context_menu(Kind::Pane, Some(surface), entries, Vec::new(), point)
    }
    pub(super) fn show_creation_menu(&mut self, point: (i32, i32)) -> anyhow::Result<()> {
        if let Some(workspace) = self.current_workspace() {
            return self.show_workspace_menu(workspace.id, point, true);
        }
        let entries = vec![
            Entry {
                label: "New workspace".into(),
                enabled: true,
                action: MenuAction::NewWorkspace,
            },
            Entry {
                label: "New SSH Workspace".into(),
                enabled: false,
                action: MenuAction::Unsupported,
            },
        ];
        self.show_context_menu(Kind::Creation, None, entries, Vec::new(), point)
    }
    fn show_context_menu(
        &mut self,
        kind: Kind,
        surface: Option<SurfaceId>,
        entries: Vec<Entry>,
        destinations: Vec<Entry>,
        point: (i32, i32),
    ) -> anyhow::Result<()> {
        self.tab_menu.take();
        let source = surface
            .map(|id| self.locate(id).context("Menu source no longer exists"))
            .transpose()?;
        let workspace = source
            .as_ref()
            .map(|(index, _, _)| self.workspaces[*index].id);
        let pane = source.as_ref().map(|(_, pane, _)| *pane);
        let owner = surface.map_or(self.window, |surface| self.surface_window(surface));
        anyhow::ensure!(
            self.close_request.is_none()
                && !self.close_accepted
                && !self.closing
                && self.editor_barrier.is_none()
                && unsafe { IsWindowEnabled(owner) } != 0,
            "Window is busy"
        );
        let (copy_text, mut folder) = surface
            .map(|surface| self.tab_copy_text(surface))
            .transpose()?
            .unwrap_or_default();
        if kind == Kind::Workspace {
            folder = source.map(|(_, _, cwd)| cwd);
        }
        self.cancel_drag();
        let menu = panel::Panel::new(owner, entries, point, self.background_test)?;
        self.tab_menu = Some(Menu {
            kind,
            surface,
            pane,
            workspace,
            owner,
            copy_text,
            folder,
            menu,
            submenu: None,
            destinations,
        });
        Ok(())
    }
    pub(super) fn tab_menu_surface_closing(&mut self, surface: SurfaceId) {
        if self
            .tab_menu
            .as_ref()
            .is_some_and(|menu| menu.surface == Some(surface))
        {
            self.tab_menu.take();
        }
    }
    pub(super) fn tab_menu_action(&mut self, id: Uuid, action: UiAction) -> anyhow::Result<()> {
        let Some(menu) = self.tab_menu.as_ref() else {
            return Ok(());
        };
        let submenu = menu.submenu.as_ref().is_some_and(|p| p.id == id);
        if menu.menu.id != id && !submenu {
            return Ok(());
        }
        if let UiAction::Dismiss(to_owner) = action {
            if submenu && to_owner {
                self.tab_menu.as_mut().unwrap().submenu.take();
            } else {
                self.tab_menu.take();
            }
            // A click or activation elsewhere owns the new focus.
            return Ok(());
        }
        if matches!(action, UiAction::Close) || (matches!(action, UiAction::Back) && !submenu) {
            self.tab_menu.take();
            return self.focus_active();
        }
        if matches!(action, UiAction::Back) {
            let menu = self.tab_menu.as_mut().unwrap();
            menu.submenu.take();
            menu.menu.focus_selected();
            return Ok(());
        }
        let UiAction::Choose(index) = action else {
            return Ok(());
        };
        let panel = if submenu {
            menu.submenu.as_ref().unwrap()
        } else {
            &menu.menu
        };
        let Some(entry) = panel.entries.get(index).filter(|entry| entry.enabled) else {
            return Ok(());
        };
        let action = entry.action;
        let kind = menu.kind;
        let (mut surface, source_pane, source_workspace, owner) =
            (menu.surface, menu.pane, menu.workspace, menu.owner);
        // Workspace actions follow that workspace's focused pane, even if the
        // user focuses another tab or reorders the sidebar while the menu is open.
        if matches!(kind, Kind::Workspace | Kind::Creation) {
            if let Some(index) = source_workspace.and_then(|id| self.workspace_index(id).ok()) {
                surface = Some(self.workspaces[index].active());
            }
        }
        let valid = kind == Kind::Creation
            || surface
                .and_then(|surface| self.locate(surface).map(|found| (surface, found)))
                .is_some_and(|(surface, (ws, pane, _))| {
                    (matches!(kind, Kind::Workspace | Kind::Creation) || Some(pane) == source_pane)
                        && Some(self.workspaces[ws].id) == source_workspace
                        && self.surface_window(surface) == owner
                });
        if !valid
            || self.close_request.is_some()
            || self.close_accepted
            || self.closing
            || self.editor_barrier.is_some()
            || unsafe { IsWindowEnabled(owner) } == 0
        {
            self.tab_menu.take();
            return Ok(());
        }
        if matches!(action, MenuAction::Move) {
            let menu = self.tab_menu.as_mut().unwrap();
            menu.menu.select(index);
            let rect = menu
                .menu
                .row_rect(index)
                .context("Move row no longer exists")?;
            menu.submenu.take();
            menu.submenu = Some(panel::Panel::new(
                menu.menu.window,
                menu.destinations.clone(),
                (rect.right, rect.top),
                self.background_test,
            )?);
            return Ok(());
        }
        // Resolve live content and destinations again; menu labels are only a snapshot.
        self.tab_menu.take();
        match action {
            MenuAction::Copy => {
                anyhow::ensure!(
                    !self.background_test,
                    "Clipboard access is disabled in background hosts"
                );
                let (text, _) = self.tab_copy_text(surface.context("No tab to copy")?)?;
                copy_text(owner, &text)?;
                self.focus_active()?;
            }
            MenuAction::Folder => {
                anyhow::ensure!(
                    !self.background_test,
                    "Opening folders is disabled in background hosts"
                );
                let surface = surface.context("No tab folder")?;
                let folder = if kind == Kind::Workspace {
                    self.locate(surface).map(|(_, _, cwd)| cwd)
                } else {
                    self.tab_copy_text(surface)?.1
                };
                let folder = folder.context("Tab has no folder")?;
                unsafe {
                    let result = windows_sys::Win32::UI::Shell::ShellExecuteW(
                        owner,
                        wide("open").as_ptr(),
                        wide(folder).as_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        SW_SHOWNORMAL,
                    );
                    anyhow::ensure!(
                        result as isize > 32,
                        "Windows could not open the folder ({})",
                        result as isize
                    );
                }
            }
            MenuAction::Destination(workspace) => {
                let index = self.workspace_index(workspace)?;
                anyhow::ensure!(
                    Some(workspace) != source_workspace && !self.is_detached_workspace(workspace),
                    "Move destination changed"
                );
                let pane = self.workspaces[index]
                    .root
                    .first_leaf_id()
                    .context("Destination workspace has no pane")?;
                self.move_tab(surface.context("No tab to move")?, pane, usize::MAX)?;
            }
            MenuAction::NewWorkspace => {
                self.new_workspace(surface, None, None)?;
            }
            MenuAction::RenameWorkspace => {
                self.edit_metadata(workspaces::EditTarget::WorkspaceName(
                    source_workspace.context("No workspace to rename")?,
                ))?;
            }
            MenuAction::WorkspaceColor => {
                self.edit_metadata(workspaces::EditTarget::WorkspaceColor(
                    source_workspace.context("No workspace color")?,
                ))?;
            }
            MenuAction::CloseWorkspace => self.confirm_close_workspaces(vec![
                source_workspace.context("No workspace to close")?
            ])?,
            MenuAction::CloseAllWorkspaces => {
                let ids = self
                    .main_workspace_indices()
                    .into_iter()
                    .map(|index| self.workspaces[index].id)
                    .collect();
                self.confirm_close_workspaces(ids)?;
            }
            MenuAction::ClosePane => {
                if !self.close_pane(source_pane.context("No pane to close")?, None)? {
                    self.focus_active()?;
                }
            }
            MenuAction::Move | MenuAction::Separator | MenuAction::Unsupported => unreachable!(),
        }
        Ok(())
    }
}

pub(super) fn copy_text(owner: HWND, text: &str) -> anyhow::Result<()> {
    use windows_sys::Win32::System::{DataExchange::*, Memory::*, Ole::CF_UNICODETEXT};
    anyhow::ensure!(!text.contains('\0'), "Clipboard text contains NUL");
    let text: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    struct Memory(HGLOBAL);
    impl Drop for Memory {
        fn drop(&mut self) {
            unsafe {
                if !self.0.is_null() {
                    GlobalFree(self.0);
                }
            }
        }
    }
    struct Clipboard;
    impl Drop for Clipboard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }
    unsafe {
        let mut memory = Memory(GlobalAlloc(GMEM_MOVEABLE, text.len() * 2));
        anyhow::ensure!(!memory.0.is_null(), "Cannot allocate clipboard text");
        let buffer = GlobalLock(memory.0).cast::<u16>();
        anyhow::ensure!(!buffer.is_null(), "Cannot lock clipboard text");
        std::ptr::copy_nonoverlapping(text.as_ptr(), buffer, text.len());
        GlobalUnlock(memory.0);
        anyhow::ensure!(OpenClipboard(owner) != 0, "Clipboard is busy");
        let _clipboard = Clipboard;
        checked(EmptyClipboard())?;
        anyhow::ensure!(
            !SetClipboardData(CF_UNICODETEXT as u32, memory.0).is_null(),
            "Cannot copy tab text"
        );
        memory.0 = std::ptr::null_mut(); // Ownership passes to Windows only on success.
    }
    Ok(())
}
