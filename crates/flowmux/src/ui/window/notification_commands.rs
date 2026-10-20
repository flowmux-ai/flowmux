// SPDX-License-Identifier: GPL-3.0-or-later
//! Notification and agent-status command dispatch on the GTK main thread.

use super::*;

impl WindowController {
    pub(super) async fn dispatch_notification_command(&self, cmd: GtkCommand) {
        match cmd {
            GtkCommand::AddActivity { entry } => {
                if self.activities.push(entry) {
                    self.refresh_activity_panel().await;
                }
            }
            GtkCommand::SetUsageBarEnabled { enabled } => {
                let before = self.options.borrow().clone();
                self.options.borrow_mut().usage_bar_enabled = enabled;
                if let Err(error) =
                    flowmux_config::options::save_changes(&before, &self.options.borrow())
                {
                    tracing::warn!(%error, "usage bar option save failed");
                }
                self.sidebar.usage.set_bar_enabled(enabled);
            }
            GtkCommand::SetUsageBarPins { pins } => {
                let before = self.options.borrow().clone();
                self.options.borrow_mut().usage_bar_pins = pins.clone();
                if let Err(error) =
                    flowmux_config::options::save_changes(&before, &self.options.borrow())
                {
                    tracing::warn!(%error, "usage pins save failed");
                }
                self.sidebar.usage.set_pins(pins);
            }
            GtkCommand::SetAgentBarMode { enabled } => {
                let before = self.options.borrow().clone();
                self.options.borrow_mut().agent_bar_mode = enabled;
                if let Err(error) =
                    flowmux_config::options::save_changes(&before, &self.options.borrow())
                {
                    tracing::warn!(%error, "agent display mode save failed");
                }
                self.sidebar.set_agent_bar_mode(enabled);
                self.refresh_agent_displays().await;
            }
            GtkCommand::SetAgentSortMode { mode } => {
                let before = self.options.borrow().clone();
                self.options.borrow_mut().agent_sort_mode = mode;
                if let Err(error) =
                    flowmux_config::options::save_changes(&before, &self.options.borrow())
                {
                    tracing::warn!(%error, "agent sort mode save failed");
                }
                self.sidebar.set_agent_sort_mode(mode);
                self.refresh_activity_panel().await;
            }
            GtkCommand::OpenActivityTarget {
                workspace,
                pane,
                surface,
            } => {
                self.open_activity_target(workspace, pane, surface).await;
            }
            GtkCommand::AddNotification {
                pane,
                surface,
                workspace,
                title,
                body,
                level,
                ack,
            } => {
                // Suppress when the user is already on the source
                // pane+surface AND the app window is focused. Mirrors
                // cmux's `shouldSuppressExternalDelivery` policy: don't
                // toast or grow the bell list for an event the user is
                // literally watching.
                //
                // Exception: NeedsInput (agent paused, waiting for
                // the user) and Error notifications always pierce the
                // suppression. "Same pane focused" is not the same as
                // "user is reading right now" — they may have scrolled
                // past the prompt, be on a different monitor, or have
                // their eyes off the screen entirely. Silencing the
                // bell for the only event class that exists to say
                // "stop typing and look here" defeats its purpose.
                let window_active = self.window.is_active();
                let focused = self.focused_pane.get();
                let pierces_focus = matches!(
                    level,
                    flowmux_core::NotificationLevel::NeedsInput
                        | flowmux_core::NotificationLevel::Error
                );
                let suppress =
                    should_suppress_notification(level, self.is_source_focused(pane, surface));
                tracing::info!(
                    ?pane,
                    ?surface,
                    ?workspace,
                    ?level,
                    ?focused,
                    window_active,
                    pierces_focus,
                    suppress,
                    "AddNotification: suppress decision"
                );
                if suppress {
                    let _ = ack.send(None);
                    return;
                }
                let source = match workspace {
                    Some(id) => self.store.get_workspace_without_scrollback(id).await,
                    None => None,
                };
                let agent = source
                    .as_ref()
                    .and_then(|ws| {
                        surface.and_then(|id| {
                            ws.surfaces
                                .iter()
                                .find_map(|s| s.root_pane.agent_presence_for_surface(id))
                        })
                    })
                    .map(|presence| presence.name)
                    .or_else(|| {
                        let name = title.split_whitespace().next()?;
                        (crate::builtin_icons::agent_icon_name(name)
                            != "applications-utilities-symbolic")
                            .then(|| name.to_owned())
                    });
                let workspace_label = source.as_ref().map(|ws| ws.display_title().to_owned());
                let surface_label = source.as_ref().and_then(|ws| {
                    let (pane, surface) = (pane?, surface?);
                    ws.surfaces
                        .iter()
                        .find_map(|s| s.root_pane.surface_title(pane, surface))
                        .map(str::to_owned)
                });
                let Some((entry_id, evicted_desktop_id)) = self
                    .notifications
                    .push(title, body, level, pane, surface, workspace)
                else {
                    // Near-duplicate of an entry pushed within
                    // `DUP_WINDOW`: the OSC path and the lifecycle
                    // hook both fired for the same Stop event. Ack
                    // with None so the IPC handler also skips the
                    // desktop toast — one row, one toast per event.
                    tracing::info!(
                        ?pane,
                        ?surface,
                        ?level,
                        "AddNotification: reusing recent in-app entry; skipping desktop toast"
                    );
                    // Deduplication can upgrade an OSC row to the hook's completion kind.
                    self.refresh_launcher_badge();
                    let _ = ack.send(None);
                    return;
                };
                self.notifications.set_source_labels(
                    entry_id,
                    agent,
                    workspace_label,
                    surface_label,
                );
                if let Some(desktop_id) = evicted_desktop_id {
                    self.close_desktop_notifications(vec![desktop_id]);
                }

                let mut marked_attention = false;
                if matches!(level, flowmux_core::NotificationLevel::NeedsInput) {
                    let flags = AgentNotificationVisualFlags::for_unread(
                        self.options.borrow().agent_notification_target,
                        false,
                    );
                    if flags.agent_bar {
                        if let Some(surface_id) = surface {
                            self.mark_agent_bar_attention(surface_id);
                            marked_attention = true;
                        }
                    }
                    if flags.workspace {
                        if let Some(ws_id) = workspace {
                            self.sidebar.mark_attention(ws_id);
                            marked_attention = true;
                        }
                    }
                }
                tracing::info!(
                    ?entry_id,
                    marked_attention,
                    workspace_known = workspace.is_some(),
                    "AddNotification: in-app entry stored, badges updated"
                );
                self.refresh_launcher_badge();
                // System-notification toggle: when disabled, the in-app bell
                // entry above stays (and badges update), but ack=None tells the
                // IPC handler to skip the desktop toast so nothing reaches the
                // system notification service.
                let system_notifications_enabled =
                    self.options.borrow().system_notifications_enabled;
                if !system_notifications_enabled {
                    let _ = ack.send(None);
                    return;
                }
                let _ = ack.send(Some(entry_id));
            }
            GtkCommand::SetNotificationDesktopId { id, desktop_id } => {
                // The daemon's `Notify` reply may race the user's read
                // gesture: by the time the desktop_id arrives, the user
                // may already have opened the bell popover or activated
                // the source workspace, in which case the previous
                // sweep had nothing to close. Detect that here and fire
                // a one-off close so the FDO toast does not linger and
                // the dock badge stays in sync.
                match self.notifications.set_desktop_id(id, desktop_id.clone()) {
                    SetDesktopIdResult::Stale | SetDesktopIdResult::Unknown => {
                        self.close_desktop_notifications(vec![desktop_id]);
                        self.refresh_launcher_badge();
                    }
                    SetDesktopIdResult::Stored => {}
                }
            }
            GtkCommand::OpenNotification { id } => {
                self.open_notification_id(id).await;
            }
            GtkCommand::ListNotifications { unread_only, ack } => {
                let entries = self
                    .notifications
                    .entries()
                    .into_iter()
                    .filter(|entry| !unread_only || !entry.read)
                    .map(notification_summary)
                    .collect();
                let _ = ack.send((entries, self.notifications.unread_count()));
            }

            GtkCommand::OpenNotificationWithAck { id, ack } => {
                let changed = self.open_notification_id(id).await;
                let _ = ack.send(changed);
            }

            GtkCommand::OpenOldestUnreadNotification { ack } => {
                let id = self
                    .notifications
                    .entries()
                    .into_iter()
                    .find(|entry| !entry.read)
                    .map(|entry| entry.id);
                let changed = match id {
                    Some(id) => self.open_notification_id(id).await,
                    None => false,
                };
                let _ = ack.send(changed);
            }

            GtkCommand::MarkNotificationRead { id, ack } => {
                let entry = self.notifications.find(id);
                let desktop_id = entry.as_ref().and_then(|entry| entry.desktop_id.clone());
                let changed = self.notifications.mark_read(id);
                if changed {
                    if let Some(desktop_id) = desktop_id {
                        self.close_desktop_notifications(vec![desktop_id]);
                    }
                    if let Some(entry) = &entry {
                        self.clear_notification_attention_for_entry(entry);
                    }
                    self.refresh_launcher_badge();
                }
                let _ = ack.send(changed);
            }

            GtkCommand::ClearNotifications { ack } => {
                let entries = self.notifications.entries();
                let had_entries = !entries.is_empty();
                let desktop_ids = self.notifications.clear_all();
                self.clear_all_notification_attention_for_entries(&entries);
                if !desktop_ids.is_empty() {
                    self.close_desktop_notifications(desktop_ids);
                }
                self.refresh_launcher_badge();
                let _ = ack.send(had_entries);
            }

            GtkCommand::DeleteNotification { id } => {
                // Trash button on the bell-popover row. Drop the entry,
                // close any live FDO toast (so the system notification
                // center shrinks in lockstep), re-publish the dock
                // badge if the unread count changed, and re-render the
                // popover so the deleted row vanishes immediately.
                let entry = self.notifications.find(id);
                match self.notifications.remove(id) {
                    RemoveOutcome::Unknown => {
                        tracing::debug!(%id, "delete notification: id not found");
                    }
                    RemoveOutcome::RemovedRead => {
                        // Read-only delete: unread count unchanged, no
                        // FDO toast was outstanding for this entry.
                        if let Some(entry) = &entry {
                            self.clear_notification_attention_for_entry(entry);
                        }
                        self.sidebar.refresh_notification_popover();
                    }
                    RemoveOutcome::RemovedUnread { desktop_id } => {
                        if let Some(did) = desktop_id {
                            self.close_desktop_notifications(vec![did]);
                        }
                        if let Some(entry) = &entry {
                            self.clear_notification_attention_for_entry(entry);
                        }
                        self.refresh_launcher_badge();
                        self.sidebar.refresh_notification_popover();
                    }
                }
            }
            GtkCommand::MarkAllNotificationsRead => {
                let entries = self.notifications.entries();
                let desktop_ids = self.notifications.mark_all_unread_read();
                self.clear_all_notification_attention_for_entries(&entries);
                self.close_desktop_notifications(desktop_ids);
                self.refresh_launcher_badge();
            }
            GtkCommand::DeleteReadNotifications => {
                for entry in self
                    .notifications
                    .entries()
                    .into_iter()
                    .filter(|entry| entry.read)
                {
                    let _ = self.notifications.remove(entry.id);
                }
                self.refresh_launcher_badge();
            }
            GtkCommand::ClearAllNotifications => {
                let entries = self.notifications.entries();
                let desktop_ids = self.notifications.clear_all();
                self.clear_all_notification_attention_for_entries(&entries);
                if !desktop_ids.is_empty() {
                    self.close_desktop_notifications(desktop_ids);
                }
                self.refresh_launcher_badge();
                self.sidebar.refresh_notification_popover();
            }
            GtkCommand::SetAgentStatus { workspace } => {
                self.sync_workspace_agent_status(workspace).await;
            }
            GtkCommand::QueryTerminalAgentPids { ack } => {
                let _ = ack.send(self.pane_registry.borrow().terminal_agent_pids());
            }
            GtkCommand::QueryAgentSurfaceVisible { surface, ack } => {
                let _ = ack.send(self.is_agent_surface_visible(surface));
            }
            GtkCommand::OpenAgentBarItem {
                workspace,
                pane,
                surface,
            } => {
                self.open_agent_bar_item(workspace, pane, surface).await;
            }
            other => {
                unreachable!("notification router got a non-notification command: {other:?}")
            }
        }
    }
}
