// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace metadata/lifecycle and native entry points. IDs survive reordering.
use super::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetFocus, GetKeyState};
#[path = "metadata_panel.rs"]
mod editor;
pub(super) use editor::{EditAction, Panel};

pub(super) struct Close {
    id: Uuid,
    ids: Vec<WorkspaceId>,
    surfaces: Vec<SurfaceId>,
    pub(super) panel: super::editor::ClosePanel,
}

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

#[derive(Clone, Copy)]
pub(super) enum Scroll {
    Wheel(i32),
    Line(i32),
    Page(i32),
    Start,
    End,
}

pub(super) struct SidebarLayout {
    pub list_top: i32,
    pub list_bottom: i32,
    pub footer_top: i32,
    pub capacity: usize,
    pub pager: bool,
    pub offset: i32,
    pub max_offset: i32,
    pub first_row: usize,
    pub rows: Vec<(WorkspaceId, i32, i32)>,
    pub metrics: (i32, u32, i32),
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
    let width = px(22);
    let gap = px(1);
    let tab_gap = px(4);
    let available = (area.width - px(30) - tab_gap - px(4)).max(0);
    let count = ((available + gap) / (width + gap)).clamp(1, 6) as usize;
    let occupied =
        (count as i32 * width + (count as i32 - 1) * gap).min((area.width - px(4)).max(0));
    let start = area.x + area.width - px(2) - occupied;
    layout.tabs_width = (area.width - occupied - tab_gap - px(4)).max(0);
    for (offset, slot) in layout.tools[6 - count..].iter_mut().enumerate() {
        let x = start + offset as i32 * (width + gap);
        *slot = Some(model::Rect {
            x,
            y: area.y + px(4),
            width: width.min((area.x + area.width - px(2) - x).max(0)),
            height: width.min((area.height - px(5)).max(0)),
        });
    }
    layout
}
impl App {
    pub(super) fn refresh_pane_headers(&self, areas: &[(PaneId, model::Rect)]) {
        let workspace = self.current_workspace();
        let solo = workspace.is_none_or(|w| {
            let panes = w.leaves();
            panes.len() == 1 && panes[0].2.len() == 1
        });
        let height =
            (28.0 * unsafe { GetDpiForWindow(self.window) }.max(96) as f64 / 96.0).round() as i32;
        chrome::set_pane_headers(
            self.window,
            areas
                .iter()
                .map(|(pane, area)| {
                    (
                        model::Rect {
                            height: height.min(area.height),
                            ..*area
                        },
                        !solo && workspace.is_some_and(|w| w.focused == *pane),
                    )
                })
                .collect(),
        );
    }
    pub(super) fn sidebar_layout(&self, height: i32, dpi: u32) -> SidebarLayout {
        let px = |n: i32| (n as f64 * dpi.max(96) as f64 / 96.0).round() as i32;
        let footer_top = (height - px(36)).max(0);
        let list_top = px(40).min(footer_top);
        let indices = self.main_workspace_indices();
        let heights: Vec<i32> = indices
            .iter()
            .map(|i| px(38 + 20 * self.workspace_lines(self.workspaces[*i].id).len().max(1) as i32))
            .collect();
        let metrics = (height, dpi, heights.iter().sum::<i32>());
        let pager = metrics.2 > footer_top - list_top;
        let list_bottom = (footer_top - if pager { px(28) } else { 0 }).max(list_top);
        let available = list_bottom - list_top;
        let max_offset = (metrics.2 - available).max(0);
        let mut offset = self.sidebar_offset.clamp(0, max_offset);
        let active_changed = self.sidebar_active != self.current_workspace().map(|w| w.id);
        if active_changed || self.sidebar_metrics != metrics {
            if let Some(active) = indices.iter().position(|i| *i == self.active_workspace) {
                let top: i32 = heights[..active].iter().sum();
                let bottom = top + heights[active];
                if heights[active] > available {
                    offset = if active_changed {
                        top
                    } else {
                        offset.clamp(top, bottom - available)
                    };
                } else if top < offset {
                    offset = top;
                } else if bottom > offset + available {
                    offset = bottom - available;
                }
            }
        }
        offset = offset.clamp(0, max_offset);
        let mut rows = Vec::new();
        let mut y = list_top - offset;
        let mut first_row = 0;
        for (index, height) in heights.iter().enumerate() {
            if y >= list_bottom {
                break;
            }
            if y + height > list_top {
                if rows.is_empty() {
                    first_row = index;
                }
                rows.push((self.workspaces[indices[index]].id, y, *height));
            }
            y += height;
        }
        SidebarLayout {
            list_top,
            list_bottom,
            footer_top,
            capacity: rows.len(),
            pager,
            offset,
            max_offset,
            first_row,
            rows,
            metrics,
        }
    }
    pub(super) fn scroll_sidebar(&mut self, scroll: Scroll) -> anyhow::Result<()> {
        let mut client = RECT::default();
        checked(unsafe { GetClientRect(self.window, &mut client) })?;
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let layout = self.sidebar_layout(client.bottom, dpi);
        let line = (20 * dpi as i32 + 48) / 96;
        let page = (layout.list_bottom - layout.list_top - line).max(line);
        let offset = match scroll {
            Scroll::Start => 0,
            Scroll::End => layout.max_offset,
            Scroll::Line(delta) => layout.offset.saturating_add(delta.saturating_mul(line)),
            Scroll::Page(delta) => layout.offset.saturating_add(delta.saturating_mul(page)),
            Scroll::Wheel(delta) => {
                let mut lines = 3u32;
                unsafe {
                    SystemParametersInfoW(
                        SPI_GETWHEELSCROLLLINES,
                        0,
                        (&mut lines as *mut u32).cast(),
                        0,
                    );
                }
                let step = if lines == u32::MAX {
                    page
                } else {
                    line.saturating_mul(lines.min(100) as i32)
                };
                let amount = self
                    .sidebar_wheel_remainder
                    .saturating_add(delta.saturating_mul(step));
                self.sidebar_wheel_remainder = amount % 120;
                layout.offset.saturating_add(amount / 120)
            }
        };
        let offset = offset.clamp(0, layout.max_offset);
        if offset == self.sidebar_offset && layout.metrics == self.sidebar_metrics {
            return Ok(());
        }
        let focused = unsafe { GetFocus() };
        let retain_keyboard = !self.background_test
            && unsafe { GetForegroundWindow() } == self.window
            && self.controls.iter().any(|c| {
                c.hwnd == focused
                    && matches!(
                        c.action,
                        Action::Workspace(_) | Action::WorkspaceClose(_) | Action::SidebarScroll(_)
                    )
            });
        self.sidebar_offset = offset;
        self.layout()?;
        if retain_keyboard
            && unsafe { GetForegroundWindow() } == self.window
            && (unsafe { GetWindowLongPtrW(focused, GWL_STYLE) } as u32 & WS_VISIBLE == 0
                || unsafe { IsWindowEnabled(focused) } == 0)
        {
            if let Some(next) = self.controls.iter().find(|c| {
                matches!(c.action, Action::SidebarScroll(_))
                    && unsafe { IsWindowEnabled(c.hwnd) } != 0
                    && unsafe { IsWindowVisible(c.hwnd) } != 0
            }) {
                unsafe {
                    SetFocus(next.hwnd);
                }
            }
        }
        Ok(())
    }

    pub(super) fn sidebar_scroll_key(&self, message: &MSG) -> bool {
        if message.message != WM_KEYDOWN || self.overview.is_open() {
            return false;
        }
        let target = self.controls.iter().any(|control| {
            control.hwnd == message.hwnd
                && matches!(
                    control.action,
                    Action::Workspace(_) | Action::WorkspaceClose(_) | Action::SidebarScroll(_)
                )
        });
        if !target || unsafe { IsWindowEnabled(message.hwnd) } == 0 {
            return false;
        }
        if !self.background_test
            && unsafe {
                GetForegroundWindow() != self.window
                    || GetFocus() != message.hwnd
                    || [0x10, 0x11, 0x12, 0x5b, 0x5c]
                        .iter()
                        .any(|key| GetKeyState(*key) < 0)
            }
        {
            return false;
        }
        let scroll = match message.wParam {
            0x21 => Scroll::Page(-1),
            0x22 => Scroll::Page(1),
            0x24 => Scroll::Start,
            0x23 => Scroll::End,
            0x26 => Scroll::Line(-1),
            0x28 => Scroll::Line(1),
            _ => return false,
        };
        self.sender.send(Event::SidebarScroll(scroll));
        true
    }

    fn workspace_lines(&self, id: WorkspaceId) -> Vec<chrome::WorkspaceLine> {
        let Some(workspace) = self.workspaces.iter().find(|w| w.id == id) else {
            return Vec::new();
        };
        let leaves = workspace.leaves();
        let mut histories = self.sidebar_mru.borrow_mut();
        let history = histories.entry(id).or_default();
        history.retain(|pane| leaves.iter().any(|(id, _, _)| id == pane));
        if history.first() != Some(&workspace.focused) {
            history.retain(|pane| *pane != workspace.focused);
            history.insert(0, workspace.focused);
        }
        let mru = history.clone();
        let mut order = mru.clone();
        drop(histories);
        for (pane, _, _) in &leaves {
            if !order.contains(pane) {
                order.push(*pane);
            }
        }
        let mut agents = Vec::new();
        for (pane, _, tabs) in &leaves {
            for tab in tabs {
                if let Some(agent) = self.agent_presence(tab.id) {
                    let path = match &tab.kind {
                        SurfaceKind::Terminal { cwd, .. } => {
                            cwd.as_ref().map(|p| p.display().to_string())
                        }
                        _ => None,
                    };
                    agents.push((*pane, tab.id, agent, path));
                }
            }
        }
        // Match Linux: urgency, pane MRU, provider, then stable surface identity.
        agents.sort_by(|a, b| {
            b.2.status
                .rollup_rank()
                .cmp(&a.2.status.rollup_rank())
                .then_with(|| {
                    mru.iter()
                        .position(|id| *id == a.0)
                        .unwrap_or(usize::MAX)
                        .cmp(&mru.iter().position(|id| *id == b.0).unwrap_or(usize::MAX))
                })
                .then_with(|| a.2.name.cmp(&b.2.name))
                .then_with(|| a.1 .0.cmp(&b.1 .0))
        });
        let overflow = agents.len().saturating_sub(4);
        agents.truncate(4);
        let mut paths = Vec::new();
        for pane in order
            .into_iter()
            .filter(|id| !agents.iter().any(|(pane, _, _, _)| pane == id))
            .take(3)
        {
            let Some((_, active, tabs)) = leaves.iter().find(|(id, _, _)| *id == pane) else {
                continue;
            };
            let Some(tab) = tabs.iter().find(|tab| tab.id == *active) else {
                continue;
            };
            let (text, path) = match &tab.kind {
                SurfaceKind::Terminal { cwd, .. } => (
                    cwd.as_ref().unwrap_or(&workspace.cwd).display().to_string(),
                    true,
                ),
                SurfaceKind::Browser { .. } => (format!("Browser-{}", tab.title), false),
                SurfaceKind::Editor { .. } => (format!("Editor-{}", tab.title), false),
                SurfaceKind::SshTerminal { cwd, .. } => (
                    format!(
                        "{}:{}",
                        workspace
                            .ssh
                            .as_ref()
                            .map(|config| config.target.destination())
                            .unwrap_or_else(|| "SSH".into()),
                        cwd.as_deref().unwrap_or("~")
                    ),
                    true,
                ),
            };
            paths.push((text, path));
        }
        let count = agents.len();
        let mut lines = Vec::new();
        for (index, (_, _, agent, path)) in agents.into_iter().enumerate() {
            let parent_continues = index + 1 < count || !paths.is_empty();
            let mut name = agent.name.clone();
            if index + 1 == count && overflow > 0 {
                name.push_str(&format!(
                    " +{overflow} agent{}",
                    if overflow == 1 { "" } else { "s" }
                ));
            }
            let text = agent
                .status_text()
                .unwrap_or(agent.status.as_str())
                .to_string();
            lines.push(chrome::WorkspaceLine {
                text: name,
                path: false,
                agent: Some(agent),
                parent: None,
                continues: parent_continues,
            });
            lines.push(chrome::WorkspaceLine {
                text,
                path: false,
                agent: None,
                parent: Some(parent_continues),
                continues: path.is_some(),
            });
            if let Some(text) = path {
                lines.push(chrome::WorkspaceLine {
                    text,
                    path: true,
                    agent: None,
                    parent: Some(parent_continues),
                    continues: false,
                });
            }
        }
        let count = paths.len();
        for (index, (text, path)) in paths.into_iter().enumerate() {
            lines.push(chrome::WorkspaceLine {
                text,
                path,
                agent: None,
                parent: None,
                continues: index + 1 < count,
            });
        }
        for line in &mut lines {
            line.text = line.text.replace(['\r', '\n'], " ");
        }
        lines
    }
    pub(super) fn workspace_caption(&self, id: WorkspaceId) -> Option<String> {
        let mut caption = self.workspaces.iter().find(|w| w.id == id)?.name.clone();
        for line in self.workspace_lines(id) {
            caption.push('\n');
            caption.push_str(&line.text);
            if let Some(agent) = line.agent {
                // Owner-drawn glyphs have no accessible name of their own.
                caption.push_str(&format!(" ({})", agent.status.as_str()));
            }
        }
        Some(caption)
    }
    fn workspace_has_unread(&self, id: WorkspaceId) -> bool {
        self.notifications.store.entries().iter().any(|entry| {
            !entry.read
                && entry
                    .surface
                    .and_then(|surface| self.locate(surface))
                    .map(|(index, _, _)| self.workspaces[index].id)
                    .or(entry.workspace)
                    == Some(id)
        })
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
                    tree: !self.is_detached_workspace(id),
                    selected: self
                        .current_workspace()
                        .is_some_and(|workspace| workspace.id == id)
                        || self.is_detached_workspace(id),
                    color,
                    unread: self.workspace_has_unread(id),
                }
            }
            Action::Tab(pane, surface) | Action::TabClose(pane, surface) => {
                let workspace = self.current_workspace();
                let selected =
                    workspace.and_then(|w| w.root.active_surface_id(pane)) == Some(surface);
                let multiple = workspace
                    .and_then(|w| w.leaves().into_iter().find(|(p, _, _)| *p == pane))
                    .is_some_and(|(_, _, tabs)| tabs.len() > 1);
                if matches!(action, Action::TabClose(..)) {
                    return chrome::Role::TabClose { selected, multiple };
                }
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
                    selected,
                    multiple,
                    kind,
                }
            }
            Action::Settings
            | Action::Overview
            | Action::CommandPalette
            | Action::ShowFiles
            | Action::Worktrees
            | Action::Usage
            | Action::Sessions
            | Action::SearchAll
            | Action::OpenEditor
            | Action::Notifications => chrome::Role::Icon {
                kind: match action {
                    Action::Overview => chrome::ChromeIcon::Overview,
                    Action::CommandPalette => chrome::ChromeIcon::CommandPalette,
                    Action::Settings => chrome::ChromeIcon::Settings,
                    Action::ShowFiles => chrome::ChromeIcon::Files,
                    Action::Worktrees => chrome::ChromeIcon::Worktrees,
                    Action::Usage => chrome::ChromeIcon::Usage,
                    Action::Sessions => chrome::ChromeIcon::Sessions,
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
            Action::PaneAdd(..) | Action::NewWorkspace | Action::WorkspaceClose(_) => {
                chrome::Role::Tool
            }
            _ => chrome::Role::Button,
        }
    }
    pub(super) fn refresh_chrome_metadata(&self) {
        self.sidebar_mru
            .borrow_mut()
            .retain(|id, _| self.workspaces.iter().any(|w| w.id == *id));
        self.refresh_pane_headers(&self.pane_layout.panes);
        self.refresh_ssh_toolbar();
        for window in self.detached.values() {
            if let Some(workspace) = self.workspaces.iter().find(|w| w.id == window.workspace) {
                window.workspace_caption(
                    &workspace.name,
                    self.chrome_role(&Action::Workspace(workspace.id)),
                );
            }
        }
        for control in &self.controls {
            if let Action::Workspace(id) = control.action {
                if let Some(label) = self.workspace_caption(id) {
                    set_caption(control.hwnd, &label);
                    chrome::set_workspace_lines(control.hwnd, self.workspace_lines(id));
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
            if !matches!(control.action, Action::EmptyState | Action::SshStatus(_)) {
                chrome::set_role(control.hwnd, self.chrome_role(&control.action));
            }
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
        let (_, current, _) = self
            .locate(surface)
            .context("Pane source no longer exists")?;
        anyhow::ensure!(current == pane, "Pane source moved");
        let hwnd = self
            .controls
            .iter()
            .find(|c| matches!(c.action,Action::PaneMenu(p,s) if p==pane&&s==surface))
            .map_or(self.window, |c| c.hwnd);
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut rect);
        }
        self.show_pane_menu(pane, surface, (rect.left, rect.bottom))
    }

    pub(super) fn chrome_status(&self) -> Value {
        let controls=self.controls.iter().map(|control| {
            let (kind,pane,surface,workspace,selected)=match control.action {
                Action::Workspace(id)=>("workspace",None,None,Some(id),self.current_workspace().is_some_and(|workspace| workspace.id == id)),
                Action::WorkspaceClose(id)=>("workspace_close",None,None,Some(id),false),
                Action::Tab(pane,surface)=>("tab",Some(pane),Some(surface),None,self.current_workspace().and_then(|workspace|workspace.root.active_surface_id(pane))==Some(surface)),
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
                Action::EmptyState=>("empty_state",None,None,None,false),
                Action::SshStatus(id)=>("ssh_status",None,None,Some(id),false),
                Action::SshConnect(id)=>("ssh_connect",None,None,Some(id),false),
                Action::SshDisconnect(id)=>("ssh_disconnect",None,None,Some(id),false),
                Action::SshAuthentication(id)=>("ssh_authentication",None,None,Some(id),false),
                Action::SshPorts(id)=>("ssh_ports",None,None,Some(id),false),
                Action::Settings=>("settings",None,None,None,false),
                Action::CommandPalette=>("command_palette",None,None,None,false),
                Action::Overview=>("overview",None,None,None,false),
                Action::ShowFiles=>("files",None,None,None,false),
                Action::Worktrees=>("worktrees",None,None,None,false),
                Action::Usage=>("usage",None,None,None,false),
                Action::Sessions=>("sessions",None,None,None,false),
                Action::SearchAll=>("search_all",None,None,None,false),
                Action::OpenEditor=>("open_file",None,None,None,false),
                Action::Notifications=>("notifications",None,None,None,false),
                _=>("button",None,None,None,false),
            };
            unsafe {
                let mut rect=RECT::default(); GetWindowRect(control.hwnd,&mut rect);
                let mut top=POINT{x:rect.left,y:rect.top};ScreenToClient(self.window,&mut top);
                let mut clip=RECT::default();let clipped=GetWindowRgnBox(control.hwnd,&mut clip)!=0;
                let length=GetWindowTextLengthW(control.hwnd).clamp(0,1024) as usize;
                let mut label=vec![0u16;length+1];let read=GetWindowTextW(control.hwnd,label.as_mut_ptr(),label.len() as i32).max(0) as usize;
                json!({"handle":control.hwnd as usize,"workspace_lines":chrome::workspace_lines(control.hwnd),"tooltip":chrome::tooltip_text(control.hwnd),"kind":kind,"pane":pane,"surface":surface,"workspace":workspace,"selected":selected,"unread":workspace.is_some_and(|id|self.workspace_has_unread(id)),"focused":pane.is_some_and(|p|self.current_workspace().is_some_and(|workspace|p==workspace.focused)),"label":String::from_utf16_lossy(&label[..read]),"layout_visible":GetWindowLongPtrW(control.hwnd,GWL_STYLE) as u32&WS_VISIBLE!=0,"native_visible":IsWindowVisible(control.hwnd)!=0,"clip":clipped.then(||json!({"x":clip.left,"y":clip.top,"width":clip.right-clip.left,"height":clip.bottom-clip.top})),"rect":{"x":top.x,"y":top.y,"width":rect.right-rect.left,"height":rect.bottom-rect.top}})
            }
        }).collect::<Vec<_>>();
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let mut client = RECT::default();
        unsafe {
            GetClientRect(self.window, &mut client);
        }
        let layout = self.sidebar_layout(client.bottom, dpi);
        json!({"theme":self.settings.terminal.theme,"dpi":dpi,"sidebar_offset":layout.first_row,"sidebar_offset_px":self.sidebar_offset,"sidebar_max_offset_px":layout.max_offset,"sidebar_width_dip":self.sidebar_width_dip,"sidebar_actual_width":self.sidebar_width(client.right,dpi),"sidebar_dragging":matches!(self.drag,Some(panes::Drag::Sidebar { .. })),"tab_dragging":matches!(self.drag,Some(panes::Drag::Tab { .. })),"workspace_dragging":matches!(self.drag,Some(panes::Drag::Workspace { .. })),"tab_drop_preview":self.drop_preview.as_ref().map(chrome::DropPreview::diagnostics),"sidebar_gutter_width":(4.0*dpi as f64/96.0).round() as i32,"sidebar_footer_height_dip":36,"sidebar_list_top":layout.list_top,"sidebar_list_bottom":layout.list_bottom,"sidebar_footer_top":layout.footer_top,"sidebar_capacity":layout.capacity,"sidebar_pager_visible":layout.pager,"workspace_row_height_dip":58,"controls":controls})
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
        self.refresh_surface_metadata(id);
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
                    json!({"workspaces":self.main_workspace_indices().into_iter().enumerate().map(|(index,i)| {let w=&self.workspaces[i];json!({"id":w.id,"name":w.name,"color":w.color,"index":index,"active":i==self.active_workspace})}).collect::<Vec<_>>()}),
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
                let index = self.workspace_index(WorkspaceId(workspace))?;
                self.workspaces[index].rename(name)?;
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
                anyhow::ensure!(
                    !self.is_detached_workspace(WorkspaceId(workspace)),
                    "separate windows are not sidebar workspaces"
                );
                let index = *self
                    .main_workspace_indices()
                    .get(index)
                    .context("workspace index is outside the list")?;
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
                if let Some(surface) = self
                    .detached
                    .iter()
                    .find_map(|(surface, window)| (window.workspace == id).then_some(*surface))
                {
                    self.close_detached(surface)?;
                    return Ok(json!({"ok":true}));
                }
                let surfaces = self.workspace_surface_ids(&[id])?;
                self.close_workspaces(vec![id], surfaces)?;
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
            Action::WorkspaceMenu | Action::NewWorkspace => {
                self.workspace_creation_menu(Some(point))
            }
            Action::Tab(pane, surface) | Action::TabClose(pane, surface) => {
                self.show_tab_menu(pane, surface, point)
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
        self.popup_for(self.window, items, disabled, point)
    }
    pub(super) fn popup_for(
        &self,
        owner: HWND,
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
            SetFocus(owner);
            let result = TrackPopupMenuEx(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.0,
                point.1,
                owner,
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
        let index = self.workspace_index(id)?;
        anyhow::ensure!(
            !self.is_detached_workspace(id),
            "separate windows have no workspace menu"
        );
        let creation = point.is_none();
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
        self.show_workspace_menu(self.workspaces[index].id, point, creation)
    }
    pub(super) fn workspace_creation_menu(
        &mut self,
        point: Option<(i32, i32)>,
    ) -> anyhow::Result<()> {
        let point = point.unwrap_or_else(|| {
            let hwnd = self
                .controls
                .iter()
                .find(|control| matches!(control.action, Action::WorkspaceMenu))
                .map_or(self.window, |control| control.hwnd);
            let mut rect = RECT::default();
            unsafe {
                GetWindowRect(hwnd, &mut rect);
            }
            (rect.left, rect.bottom)
        });
        self.show_creation_menu(point)
    }
    pub(super) fn workspace_surface_ids(
        &self,
        ids: &[WorkspaceId],
    ) -> anyhow::Result<Vec<SurfaceId>> {
        let mut surfaces = Vec::new();
        for id in ids {
            let workspace = &self.workspaces[self.workspace_index(*id)?];
            anyhow::ensure!(
                !self.is_detached_workspace(*id),
                "Separate windows are not in this close request"
            );
            surfaces.extend(
                workspace
                    .leaves()
                    .into_iter()
                    .flat_map(|(_, _, tabs)| tabs.into_iter().map(|tab| tab.id)),
            );
        }
        surfaces.sort_by_key(|surface| surface.0);
        Ok(surfaces)
    }
    pub(super) fn confirm_close_workspaces(&mut self, ids: Vec<WorkspaceId>) -> anyhow::Result<()> {
        self.files_operation_guard()?;
        anyhow::ensure!(
            !ids.is_empty()
                && self.workspace_close.is_none()
                && self.editor_barrier.is_none()
                && self.close_request.is_none()
                && !self.close_accepted
                && self.pending_save.is_none()
                && self.editor_open_pending.is_empty()
                && unsafe { IsWindowEnabled(self.window) } != 0,
            "Window is busy"
        );
        let surfaces = self.workspace_surface_ids(&ids)?;
        let summary = if ids.len() == 1 {
            format!(
                "Close workspace ‘{}’ and stop its tabs?",
                self.workspaces[self.workspace_index(ids[0])?].name
            )
        } else {
            format!(
                "This will close all {} workspaces and stop their tabs.",
                ids.len()
            )
        };
        let id = Uuid::new_v4();
        let panel = super::editor::ClosePanel::workspace(
            self.window,
            id,
            &summary,
            ids.len(),
            self.background_test,
        )?;
        self.workspace_close = Some(Close {
            id,
            ids,
            surfaces,
            panel,
        });
        Ok(())
    }
    pub(super) fn workspace_close_choice(
        &mut self,
        id: Uuid,
        accepted: bool,
    ) -> anyhow::Result<()> {
        if self
            .workspace_close
            .as_ref()
            .is_none_or(|close| close.id != id)
        {
            return Ok(());
        }
        let Close {
            ids,
            surfaces,
            panel,
            ..
        } = self.workspace_close.take().unwrap();
        drop(panel);
        if accepted {
            self.close_workspaces(ids, surfaces)?;
        } else {
            self.focus_active()?;
        }
        Ok(())
    }
    pub(super) fn close_workspaces(
        &mut self,
        ids: Vec<WorkspaceId>,
        surfaces: Vec<SurfaceId>,
    ) -> anyhow::Result<()> {
        self.files_operation_guard()?;
        anyhow::ensure!(
            self.close_request.is_none()
                && !self.close_accepted
                && self.pending_save.is_none()
                && self.editor_open_pending.is_empty(),
            "Window is busy"
        );
        anyhow::ensure!(
            self.workspace_surface_ids(&ids)? == surfaces,
            "Workspace tabs changed while close was pending"
        );
        if self.editor_guard(
            super::editor::Operation::Workspaces {
                ids: ids.clone(),
                surfaces: surfaces.clone(),
            },
            None,
        )? {
            return Ok(());
        }
        let active = self.current_workspace().map(|workspace| workspace.id);
        self.cancel_drag();
        self.tab_menu.take();
        self.metadata.take();
        self.workspaces
            .retain(|workspace| !ids.contains(&workspace.id));
        let forward_ids: Vec<_> = self
            .ssh_forwards
            .iter()
            .filter(|(_, f)| ids.contains(&f.workspace))
            .map(|(id, _)| *id)
            .collect();
        let retired: Vec<_> = forward_ids
            .into_iter()
            .filter_map(|id| self.ssh_forwards.remove(&id))
            .collect();
        if self
            .ssh_ports
            .as_ref()
            .is_some_and(|panel| ids.contains(&panel.workspace))
        {
            self.ssh_ports.take();
        }
        self.active_workspace = active
            .and_then(|id| {
                self.workspaces
                    .iter()
                    .position(|workspace| workspace.id == id)
            })
            .unwrap_or(0);
        self.normalize_main_workspace();
        self.zoomed = None;
        self.detached_focus = None;
        for surface in surfaces {
            self.remove_surface(surface);
        }
        drop(retired);
        self.search_tick()?;
        self.rebuild()
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
        anyhow::ensure!(
            self.metadata.as_ref().is_none_or(|panel| !panel.is_open()),
            "another metadata dialog is open"
        );
        anyhow::ensure!(
            self.editor_barrier.is_none() && self.close_request.is_none() && !self.close_accepted,
            "window is busy"
        );
        let value = self.metadata_text(target)?;
        let locked = match target {
            EditTarget::TabName(id) => self.title_locked(id)?,
            EditTarget::WorkspaceName(id) => self.workspaces[self.workspace_index(id)?].name_locked,
            _ => false,
        };
        let surface = match target {
            EditTarget::TabName(id) => id,
            EditTarget::WorkspaceName(id) | EditTarget::WorkspaceColor(id) => {
                self.workspaces[self.workspace_index(id)?].active()
            }
        };
        let owner = self.surface_window(surface);
        anyhow::ensure!(
            unsafe { IsWindowEnabled(owner) } != 0,
            "another dialog is open"
        );
        let mut panel = Panel::new(owner)?;
        chrome::window_theme(panel.window, self.settings.terminal.theme);
        panel.edit(target, &value, locked, self.background_test);
        self.metadata = Some(panel);
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
    pub(super) fn metadata_owner_closing(&mut self, owner: HWND) {
        if self
            .ssh_dialog
            .as_ref()
            .is_some_and(|panel| panel.owner == owner)
        {
            self.ssh_dialog.take();
        }
        if self
            .metadata
            .as_ref()
            .is_some_and(|panel| panel.owner == owner)
        {
            self.metadata.take();
        }
    }
    pub(super) fn metadata_action(&mut self, id: Uuid, action: EditAction) -> anyhow::Result<()> {
        let Some(panel) = self
            .metadata
            .as_ref()
            .filter(|panel| panel.edit_id == id && panel.is_open())
        else {
            return Ok(());
        };
        match action {
            EditAction::Changed => panel.preview(),
            EditAction::Pick => {
                let panel = self.metadata.as_mut().unwrap();
                if let Err(error) = panel.choose_color() {
                    panel.status(&error.to_string());
                }
            }
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
                    // Linux treats a blank tab-name response as dismissal.
                    if matches!(target, EditTarget::TabName(_)) && value.trim().is_empty() {
                        return Ok(());
                    }
                    let current = self.metadata_text(target)?;
                    // Automatic title changes do not invalidate a pending rename.
                    // A competing manual name or mode change does.
                    let locked = match target {
                        EditTarget::TabName(id) => Some(self.title_locked(id)?),
                        EditTarget::WorkspaceName(id) => {
                            Some(self.workspaces[self.workspace_index(id)?].name_locked)
                        }
                        _ => None,
                    };
                    let unchanged = locked.map_or(current == original, |locked| {
                        locked == original_locked && (!locked || current == original)
                    });
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
                        EditTarget::TabName(id) => self.rename_tab(id, value.trim().to_owned())?,
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

impl App {
    pub(super) fn ssh_toolbar_caption(&self, workspace: WorkspaceId) -> String {
        let Some(ws) = self.workspaces.iter().find(|w| w.id == workspace) else {
            return String::new();
        };
        let Some(config) = &ws.ssh else {
            return String::new();
        };
        let status = self.ssh_status(workspace);
        let state = status["state"].as_str().unwrap_or("disconnected");
        let cwd = crate::ssh::display_cwd(ws);
        let mut caption = format!(
            "SSH {} · {state} · {}",
            config.target.destination(),
            cwd.unwrap_or("~")
        );
        if let Some(error) = status["error"].as_str() {
            caption.push_str(&format!(" · {error}"));
        }
        if state == "connecting" {
            caption.push_str(" · If SSH needs input, open Authentication");
        }
        caption
    }
    pub(super) fn refresh_ssh_toolbar(&self) {
        for control in &self.controls {
            let enabled = match control.action {
                Action::SshStatus(id) => {
                    set_caption(control.hwnd, &self.ssh_toolbar_caption(id));
                    continue;
                }
                Action::SshConnect(_)
                | Action::SshDisconnect(_)
                | Action::SshAuthentication(_)
                | Action::SshPorts(_) => self.ssh_action_enabled(&control.action),
                _ => continue,
            };
            unsafe {
                windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow(
                    control.hwnd,
                    i32::from(enabled),
                );
            }
        }
    }
    pub(super) fn ssh_toolbar_height(&self, workspace: usize, width: i32) -> i32 {
        if self
            .workspaces
            .get(workspace)
            .is_none_or(|w| w.ssh.is_none())
        {
            return 0;
        }
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let px = |v: i32| (v as f64 * dpi as f64 / 96.0).round() as i32;
        if width - self.sidebar_width(width, dpi) - px(8) < px(560) {
            px(64)
        } else {
            px(32)
        }
    }
    pub(super) fn ssh_toolbar_rect(
        &self,
        action: &Action,
        width: i32,
    ) -> Option<(i32, i32, i32, i32)> {
        self.current_workspace().filter(|w| w.ssh.is_some())?;
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        let px = |v: i32| (v as f64 * dpi as f64 / 96.0).round() as i32;
        let left = self.sidebar_width(width, dpi) + px(4);
        let available = (width - left - px(4)).max(0);
        let compact = available < px(560);
        let gap = px(if compact { 4 } else { 8 });
        let widths = [px(64), px(80), px(104), px(48)];
        let button_width = widths.iter().sum::<i32>() + 3 * gap;
        let start = (width - px(4) - button_width).max(left);
        let top = px(if compact { 36 } else { 4 });
        if matches!(action, Action::SshStatus(_)) {
            return Some((
                left,
                px(4),
                if compact {
                    available
                } else {
                    (start - left - gap).max(0)
                },
                px(28),
            ));
        }
        let index = match action {
            Action::SshConnect(_) => 0,
            Action::SshDisconnect(_) => 1,
            Action::SshAuthentication(_) => 2,
            Action::SshPorts(_) => 3,
            _ => return None,
        };
        let x = start + widths[..index].iter().sum::<i32>() + index as i32 * gap;
        Some((
            x,
            top,
            widths[index].min((width - px(4) - x).max(0)),
            px(28),
        ))
    }
    pub(super) fn ssh_toolbar_status(&self) -> Option<Value> {
        let ws = self.current_workspace().filter(|w| w.ssh.is_some())?;
        let mut status = self.ssh_status(ws.id);
        let mut controls = serde_json::Map::new();
        for control in &self.controls {
            let key = match control.action {
                Action::SshStatus(_) => "status",
                Action::SshConnect(_) => "connect",
                Action::SshDisconnect(_) => "disconnect",
                Action::SshAuthentication(_) => "authentication",
                Action::SshPorts(_) => "ports",
                _ => continue,
            };
            controls.insert(key.into(), json!(control.hwnd as usize));
        }
        status["controls"] = json!(controls);
        status["window"] = json!(self.window as usize);
        Some(status)
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
