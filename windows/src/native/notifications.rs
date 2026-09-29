// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::notifications::{self as domain, store::NotificationStore, Op};
use flowmux_core::{NotificationId, NotificationLevel};
use std::collections::VecDeque;
use windows_sys::Win32::UI::Shell::*;
#[path = "notification_panel.rs"]
mod panel;
pub(super) use panel::UiAction;

pub(super) const DESKTOP_MESSAGE: u32 = WM_APP + 2;
pub(super) fn restart_message() -> u32 {
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe { RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()) })
}

#[derive(Default)]
struct Desktop {
    owner: isize,
    serial: u32,
    active: Option<(u32, NotificationId)>,
    queue: VecDeque<NotificationId>,
    error: Option<String>,
    calls: usize,
}
impl Desktop {
    fn shell(&mut self, action: u32, icon: &NOTIFYICONDATAW) -> bool {
        self.calls += 1;
        unsafe { Shell_NotifyIconW(action, icon) != 0 }
    }
    fn icon(&self, serial: u32) -> NOTIFYICONDATAW {
        NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.owner as HWND,
            uID: serial,
            ..Default::default()
        }
    }
    fn remove_active(&mut self) -> Option<NotificationId> {
        let (serial, id) = self.active.take()?;
        self.shell(NIM_DELETE, &self.icon(serial));
        Some(id)
    }
    fn sync(&mut self, owner: HWND, store: &NotificationStore, enabled: bool) {
        if !enabled {
            self.queue.clear();
            self.remove_active();
            return;
        }
        self.owner = owner as isize;
        let unread = |id| store.find(id).is_some_and(|entry| !entry.read);
        self.queue.retain(|id| unread(*id));
        if self.active.is_some_and(|(_, id)| !unread(id)) {
            self.remove_active();
        }
        if self.active.is_some() {
            return;
        }
        let Some(id) = self.queue.pop_front() else {
            return;
        };
        let Some(entry) = store.find(id) else {
            return;
        };
        self.error = None;
        let result = (|| -> anyhow::Result<()> {
            self.serial = self
                .serial
                .checked_add(1)
                .context("desktop notification IDs exhausted; restart flowmux")?;
            let mut icon = self.icon(self.serial);
            icon.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
            icon.uCallbackMessage = DESKTOP_MESSAGE;
            icon.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
            icon.szTip = domain::desktop_text("flowmux");
            anyhow::ensure!(
                !icon.hIcon.is_null(),
                "Windows notification icon is unavailable"
            );
            anyhow::ensure!(
                self.shell(NIM_ADD, &icon),
                "Windows Shell refused the notification icon"
            );
            self.active = Some((self.serial, id));
            // Version 3 keeps the documented 32-bit callback ID. Each balloon
            // gets a fresh ID so delayed dismissals cannot affect its successor.
            icon.Anonymous.uVersion = NOTIFYICON_VERSION;
            anyhow::ensure!(
                self.shell(NIM_SETVERSION, &icon),
                "Windows Shell refused the callback version"
            );
            icon.uFlags = NIF_INFO;
            icon.szInfoTitle = domain::desktop_text(&entry.title);
            icon.szInfo = domain::desktop_text(if entry.body.is_empty() {
                &entry.title
            } else {
                &entry.body
            });
            icon.dwInfoFlags = NIIF_RESPECT_QUIET_TIME
                | match entry.level {
                    NotificationLevel::NeedsInput => NIIF_WARNING,
                    NotificationLevel::Error => NIIF_ERROR,
                    _ => NIIF_INFO,
                };
            anyhow::ensure!(
                self.shell(NIM_MODIFY, &icon),
                "Windows Shell refused the notification"
            );
            Ok(())
        })();
        if let Err(error) = result {
            self.remove_active();
            self.queue.clear(); // Keep the bell transcript; avoid repeated Shell failures.
            self.error = Some(error.to_string());
        }
    }
    fn finish(&mut self, serial: u32, event: u32) -> Option<NotificationId> {
        if self.active.is_none_or(|(active, _)| active != serial)
            || !matches!(
                event,
                NIN_BALLOONHIDE | NIN_BALLOONTIMEOUT | NIN_BALLOONUSERCLICK
            )
        {
            return None;
        }
        let id = self.remove_active();
        if event == NIN_BALLOONUSERCLICK {
            id
        } else {
            None
        }
    }
}
impl Drop for Desktop {
    fn drop(&mut self) {
        self.remove_active();
    }
}

#[derive(Default)]
pub(super) struct Controller {
    pub(super) store: NotificationStore,
    panel: Option<panel::Panel>,
    desktop: RefCell<Desktop>,
}
impl Controller {
    #[cfg(debug_assertions)]
    pub(super) fn capture_window(&self) -> Option<HWND> {
        self.panel.as_ref().and_then(panel::Panel::capture_window)
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|panel| panel.handle_message(message))
    }
}
impl App {
    pub(super) fn sync_desktop_notifications(&self) {
        self.notifications.desktop.borrow_mut().sync(
            self.window,
            &self.notifications.store,
            self.settings.terminal.system_notifications_enabled && !self.background_test,
        );
    }
    pub(super) fn source_is_focused(&self, source: Option<SurfaceId>) -> bool {
        !self.background_test
            && unsafe {
                GetForegroundWindow() == source.map_or(self.window, |id| self.surface_window(id))
            }
            && source.is_some_and(|id| {
                self.current_surface() == Some(id)
                    && (self.surfaces.get(&id).is_some_and(|s| s.visible)
                        || self.browsers.get(&id).is_some_and(|s| s.visible)
                        || self.editors.get(&id).is_some_and(|s| s.view.visible))
            })
    }
    pub(super) fn add_notification(
        &mut self,
        source: Option<SurfaceId>,
        title: String,
        body: String,
        level: NotificationLevel,
    ) -> anyhow::Result<Value> {
        domain::validate(&title, &body)?;
        if domain::suppress(level, self.source_is_focused(source)) {
            return Ok(
                json!({"accepted":false,"reason":"source_focused","desktop_delivery":"suppressed"}),
            );
        }
        let location = source.and_then(|id| self.locate(id));
        anyhow::ensure!(
            source.is_none() || location.is_some(),
            "notification source was closed"
        );
        let pane = location.as_ref().map(|(_, pane, _)| *pane);
        let workspace = location.map(|(ws, _, _)| self.workspaces[ws].id);
        let id = self
            .notifications
            .store
            .push(title, body, level, pane, source, workspace);
        if id.is_some() {
            self.refresh_notifications();
        }
        let delivery = match id {
            None => "duplicate",
            Some(_) if !self.settings.terminal.system_notifications_enabled => "disabled",
            Some(_) if self.background_test => "background_suppressed",
            Some(id) => {
                self.notifications.desktop.borrow_mut().queue.push_back(id);
                self.sync_desktop_notifications();
                let desktop = self.notifications.desktop.borrow();
                if desktop.error.is_some() {
                    "failed"
                } else if desktop.active.is_some_and(|(_, active)| active == id) {
                    "requested"
                } else {
                    "queued"
                }
            }
        };
        Ok(
            json!({"accepted":id.is_some(),"id":id,"reason":if id.is_none() {Some("duplicate")} else {None},"desktop_delivery":delivery}),
        )
    }
    pub(super) fn notification_button_text(&self) -> String {
        format!(
            "Notifications ({})",
            self.notifications.store.unread_count()
        )
    }
    fn notification_rows(&self, unread: bool) -> Vec<Value> {
        self.notifications
            .store
            .entries()
            .into_iter()
            .filter(|entry| !unread || !entry.read)
            .map(|entry| {
                let location = entry.surface.and_then(|id| self.locate(id));
                let workspace = location
                    .as_ref()
                    .map(|(ws, _, _)| self.workspaces[*ws].id)
                    .or(entry.workspace);
                let pane = location.as_ref().map(|(_, pane, _)| *pane).or(entry.pane);
                let workspace_name = workspace.and_then(|id| {
                    self.workspaces
                        .iter()
                        .find(|w| w.id == id)
                        .map(|w| w.name.clone())
                });
                json!({
                    "id":entry.id, "title":entry.title, "body":entry.body,
                    "level":entry.level, "created_at":entry.created_at, "read":entry.read,
                    "surface":entry.surface, "pane":pane, "workspace":workspace,
                    "workspace_name":workspace_name,
                    "closed":entry.surface.is_some() && location.is_none()
                })
            })
            .collect()
    }

    fn notification_status(&self, unread: bool) -> Value {
        let desktop = self.notifications.desktop.borrow();
        json!({"entries":self.notification_rows(unread),"unread_count":self.notifications.store.unread_count(),
            "retained_limit":50,"duplicate_window_ms":8000,"desktop_delivery":"shell_balloon",
            "desktop":{"enabled":self.settings.terminal.system_notifications_enabled,"background_blocked":self.background_test,"active":desktop.active.map(|(_, id)|id),"queued":desktop.queue.len(),"error":desktop.error,"native_calls":desktop.calls},
            "button_text":self.notification_button_text(),
            "panel_handle":self.notifications.panel.as_ref().map(|p|p.window as usize),
            "panel_rows":self.notifications.panel.as_ref().map(|p|p.rows()),
            "panel_status":self.notifications.panel.as_ref().map(|p|p.status_text()),
            "panel_snapshot":self.notifications.panel.as_ref().map(|p|p.snapshot())})
    }
    pub(super) fn refresh_notifications(&self) {
        self.sync_desktop_notifications();
        self.refresh_notification_chrome();
        if let Some(panel) = &self.notifications.panel {
            panel.results(&self.notification_rows(false));
        }
    }
    fn refresh_notification_chrome(&self) {
        self.refresh_chrome_metadata();
        self.refresh_agent_bar();
        for control in &self.controls {
            if matches!(control.action, Action::Notifications) {
                workspaces::set_caption(control.hwnd, &self.notification_button_text());
            }
        }
        for id in self
            .surfaces
            .keys()
            .chain(self.browsers.keys())
            .chain(self.editors.keys())
        {
            self.refresh_tab_title(*id);
        }
    }
    pub(super) fn ack_focused_notifications(&self, source: SurfaceId) {
        if !self.source_is_focused(Some(source)) {
            return;
        }
        self.ack_focused_agent(source);
        let count = self.notifications.store.unread_count();
        self.notifications
            .store
            .mark_source_read(None, None, Some(source));
        if self.notifications.store.unread_count() != count {
            self.refresh_notifications();
        }
    }
    fn open_notification(&mut self, id: NotificationId) -> anyhow::Result<Value> {
        let entry = self
            .notifications
            .store
            .find(id)
            .context("notification not found")?;
        // Surface identity outlives moves; stale saved pane/workspace IDs must not
        // activate a different terminal. Validate before changing read/focus state.
        if let Some(source) = entry.surface {
            anyhow::ensure!(
                self.locate(source).is_some(),
                "notification source was closed"
            );
            self.select(source)?;
            self.notifications
                .store
                .mark_source_read(None, None, Some(source));
            self.rebuild()?;
        }
        self.notifications.store.mark_read(id);
        self.refresh_notifications();
        if !self.background_test {
            let surface = self.current_surface();
            let owner = surface.map_or(self.window, |id| self.surface_window(id));
            unsafe {
                ShowWindow(
                    owner,
                    if IsIconic(owner) != 0 {
                        SW_RESTORE
                    } else {
                        SW_SHOW
                    },
                );
                SetForegroundWindow(owner);
            }
            if surface.is_some() {
                self.focus_active()?;
            }
        }
        Ok(json!({"opened":true,"id":id,"surface":entry.surface}))
    }
    pub(super) fn notification_command(&mut self, op: Op) -> anyhow::Result<Value> {
        match op {
            Op::List { unread } => return Ok(self.notification_status(unread)),
            Op::Show {} => {
                self.notification_ui(UiAction::Show)?;
                return Ok(self.notification_status(false));
            }
            Op::Open { id } => return self.open_notification(NotificationId(id)),
            Op::JumpToUnread {} => {
                return match self
                    .notifications
                    .store
                    .entries()
                    .into_iter()
                    .find(|e| !e.read)
                {
                    Some(entry) => self.open_notification(entry.id),
                    None => Ok(json!({"opened":false})),
                };
            }
            Op::MarkRead { id } => {
                let found = self.notifications.store.find(NotificationId(id)).is_some();
                let changed = self.notifications.store.mark_read(NotificationId(id));
                self.refresh_notifications();
                return Ok(json!({"found":found,"changed":changed}));
            }
            Op::Delete { id } => {
                let found = !matches!(
                    self.notifications.store.remove(NotificationId(id)),
                    domain::store::RemoveOutcome::Unknown
                );
                self.refresh_notifications();
                return Ok(json!({"deleted":found}));
            }
            Op::Clear {} => {
                self.notifications.store.clear_all();
            }
        }
        self.refresh_notifications();
        Ok(self.notification_status(false))
    }
    pub(super) fn notification_ui(&mut self, action: UiAction) -> anyhow::Result<()> {
        match action {
            UiAction::Desktop(serial, event) => {
                let clicked = self
                    .notifications
                    .desktop
                    .borrow_mut()
                    .finish(serial, event);
                if let Some(id) = clicked {
                    if let Err(error) = self.open_notification(id) {
                        self.notifications.desktop.borrow_mut().error = Some(error.to_string());
                    }
                }
                self.sync_desktop_notifications();
                return Ok(());
            }
            UiAction::DesktopRestarted => {
                let mut desktop = self.notifications.desktop.borrow_mut();
                if let Some(id) = desktop.remove_active() {
                    desktop.queue.push_front(id);
                }
                drop(desktop);
                self.sync_desktop_notifications();
                return Ok(());
            }
            _ => {}
        }
        if matches!(action, UiAction::Show) {
            if self.notifications.panel.is_none() {
                self.notifications.panel = Some(panel::Panel::new(self.window)?);
            }
            // Match Linux's first-open contract: preserve the unread appearance
            // in the presented snapshot, then acknowledge its entries in store.
            let panel = self.notifications.panel.as_ref().unwrap();
            panel.results(&self.notification_rows(false));
            let anchor = self
                .controls
                .iter()
                .find(|control| matches!(control.action, Action::Notifications))
                .map_or(self.window, |control| control.hwnd);
            panel.show(self.background_test, anchor);
            self.notifications.store.mark_all_unread_read();
            self.sync_desktop_notifications();
            self.refresh_notification_chrome();
            return Ok(());
        }
        let Some(panel) = &self.notifications.panel else {
            return Ok(());
        };
        let op = match action {
            UiAction::Layout => {
                panel.layout();
                return Ok(());
            }
            UiAction::Close => {
                panel.hide();
                return Ok(());
            }
            UiAction::Scroll(delta) => {
                panel.scroll_by(delta);
                return Ok(());
            }
            UiAction::ScrollTo(offset) => {
                panel.scroll(offset);
                return Ok(());
            }
            UiAction::Navigate(direction) => {
                panel.navigate(direction);
                return Ok(());
            }
            UiAction::Open(id) => Some(Op::Open { id }),
            UiAction::Delete(id) => Some(Op::Delete { id }),
            UiAction::Clear => Some(Op::Clear {}),
            UiAction::Show | UiAction::Desktop(..) | UiAction::DesktopRestarted => unreachable!(),
        };
        if let Some(op) = op {
            let dismiss = matches!(op, Op::Open { .. } | Op::Clear {});
            if let Err(error) = self.notification_command(op) {
                self.notifications
                    .panel
                    .as_ref()
                    .unwrap()
                    .status(&error.to_string());
            } else if dismiss {
                self.notifications.panel.as_ref().unwrap().hide();
            }
        }
        Ok(())
    }
}
