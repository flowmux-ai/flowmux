// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::notifications::{self as domain, store::NotificationStore, Op};
use flowmux_core::{NotificationId, NotificationLevel};
#[path = "notification_panel.rs"]
mod panel;
pub(super) use panel::UiAction;

#[derive(Default)]
pub(super) struct Controller {
    pub(super) store: NotificationStore,
    panel: Option<panel::Panel>,
}
impl Controller {
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|panel| panel.handle_message(message))
    }
}
impl App {
    fn source_is_focused(&self, source: Option<SurfaceId>) -> bool {
        !self.background_test
            && unsafe { GetForegroundWindow() == self.window }
            && source.is_some_and(|id| {
                id == self.active()
                    && (self.surfaces.get(&id).is_some_and(|s| s.visible)
                        || self.browsers.get(&id).is_some_and(|s| s.visible))
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
                json!({"accepted":false,"reason":"source_focused","desktop_delivery":"not_implemented"}),
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
        Ok(
            json!({"accepted":id.is_some(),"id":id,"reason":if id.is_none() {Some("duplicate")} else {None},"desktop_delivery":"not_implemented"}),
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
        json!({"entries":self.notification_rows(unread),"unread_count":self.notifications.store.unread_count(),
            "retained_limit":50,"duplicate_window_ms":8000,"desktop_delivery":"not_implemented",
            "button_text":self.notification_button_text(),
            "panel_handle":self.notifications.panel.as_ref().map(|p|p.window as usize),
            "panel_rows":self.notifications.panel.as_ref().map(|p|p.rows()),
            "panel_status":self.notifications.panel.as_ref().map(|p|p.status_text())})
    }
    pub(super) fn refresh_notifications(&self) {
        let entries = self.notifications.store.entries();
        for control in &self.controls {
            let label = match control.action {
                Action::Notifications => Some(self.notification_button_text()),
                Action::Workspace(id) => self
                    .workspaces
                    .iter()
                    .enumerate()
                    .find(|(_, w)| w.id == id)
                    .map(|(index, w)| {
                        let count = entries
                            .iter()
                            .filter(|e| {
                                !e.read
                                    && e.surface
                                        .and_then(|s| self.locate(s))
                                        .map(|(i, _, _)| self.workspaces[i].id)
                                        .or(e.workspace)
                                        == Some(id)
                            })
                            .count();
                        let prefix = if count > 0 {
                            format!("[{count}] ")
                        } else {
                            String::new()
                        };
                        format!(
                            "{prefix}{} {}",
                            if index == self.active_workspace {
                                "●"
                            } else {
                                "○"
                            },
                            w.name
                        )
                    }),
                _ => None,
            };
            if let Some(label) = label {
                unsafe {
                    SetWindowTextW(control.hwnd, wide(label.replace('&', "&&")).as_ptr());
                }
            }
        }
        for id in self.surfaces.keys().chain(self.browsers.keys()) {
            self.refresh_tab_title(*id);
        }
        if let Some(panel) = &self.notifications.panel {
            panel.results(&self.notification_rows(false));
        }
    }
    pub(super) fn ack_focused_notifications(&self, source: SurfaceId) {
        if !self.source_is_focused(Some(source)) {
            return;
        }
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
            unsafe {
                ShowWindow(
                    self.window,
                    if IsIconic(self.window) != 0 {
                        SW_RESTORE
                    } else {
                        SW_SHOW
                    },
                );
                SetForegroundWindow(self.window);
            }
            self.focus_active()?;
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
        if matches!(action, UiAction::Show) {
            if self.notifications.panel.is_none() {
                self.notifications.panel = Some(panel::Panel::new(self.window)?);
            }
            self.notifications.store.mark_all_unread_read();
            self.refresh_notifications();
            self.notifications
                .panel
                .as_ref()
                .unwrap()
                .show(self.background_test);
            return Ok(());
        }
        let Some(panel) = &self.notifications.panel else {
            return Ok(());
        };
        let id = panel.selected();
        let op = match action {
            UiAction::Layout => {
                panel.layout();
                return Ok(());
            }
            UiAction::Close => {
                panel.hide();
                return Ok(());
            }
            UiAction::Selected => {
                panel.detail();
                return Ok(());
            }
            UiAction::Open => id.map(|id| Op::Open { id }),
            UiAction::Read => id.map(|id| Op::MarkRead { id }),
            UiAction::Delete => id.map(|id| Op::Delete { id }),
            UiAction::Clear => Some(Op::Clear {}),
            UiAction::Show => unreachable!(),
        };
        if let Some(op) = op {
            if let Err(error) = self.notification_command(op) {
                self.notifications
                    .panel
                    .as_ref()
                    .unwrap()
                    .status(&error.to_string());
            }
        }
        Ok(())
    }
}
