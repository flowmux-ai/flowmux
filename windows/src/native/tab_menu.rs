// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux tab-menu actions, captured by surface/workspace identity rather than row index.
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
}
#[derive(Clone)]
struct Entry {
    label: String,
    enabled: bool,
    action: MenuAction,
}
pub(super) struct Menu {
    surface: SurfaceId,
    pane: PaneId,
    workspace: WorkspaceId,
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
        json!({"surface":self.surface,"pane":self.pane,"workspace":self.workspace,
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
        self.tab_menu.take();
        let (workspace, current, _) = self.locate(surface).context("Tab no longer exists")?;
        anyhow::ensure!(current == pane, "Tab moved before opening its menu");
        let owner = self.surface_window(surface);
        anyhow::ensure!(
            self.close_request.is_none()
                && !self.close_accepted
                && !self.closing
                && self.editor_barrier.is_none()
                && unsafe { IsWindowEnabled(owner) } != 0,
            "Window is busy"
        );
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
        self.cancel_drag();
        let menu = panel::Panel::new(owner, entries, point, self.background_test)?;
        self.tab_menu = Some(Menu {
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
            .is_some_and(|menu| menu.surface == surface)
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
        let (surface, source_pane, source_workspace, owner) =
            (menu.surface, menu.pane, menu.workspace, menu.owner);
        let valid = self.locate(surface).is_some_and(|(ws, pane, _)| {
            pane == source_pane
                && self.workspaces[ws].id == source_workspace
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
                let (text, _) = self.tab_copy_text(surface)?;
                copy_text(owner, &text)?;
                self.focus_active()?;
            }
            MenuAction::Folder => {
                anyhow::ensure!(
                    !self.background_test,
                    "Opening folders is disabled in background hosts"
                );
                let (_, folder) = self.tab_copy_text(surface)?;
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
                    workspace != source_workspace && !self.is_detached_workspace(workspace),
                    "Move destination changed"
                );
                let pane = self.workspaces[index]
                    .root
                    .first_leaf_id()
                    .context("Destination workspace has no pane")?;
                self.move_tab(surface, pane, usize::MAX)?;
            }
            MenuAction::Move => unreachable!(),
        }
        Ok(())
    }
}

fn copy_text(owner: HWND, text: &str) -> anyhow::Result<()> {
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
