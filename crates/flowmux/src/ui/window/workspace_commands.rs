// SPDX-License-Identifier: GPL-3.0-or-later
//! Workspace lifecycle command dispatch on the GTK main thread.

use super::*;

impl WindowController {
    async fn materialize_workspace(&self, id: WorkspaceId, activate: bool) -> Result<(), String> {
        // Use the authoritative pane ids so later split/close commands find them.
        let ws = self
            .store
            .get_workspace(id)
            .await
            .ok_or("Workspace no longer exists")?;
        if activate {
            self.render_workspace(&ws);
            self.activate_workspace(id).await;
        } else {
            self.render_workspace_with_activation(&ws, false);
            self.sync_workspace_label(id).await;
        }
        Ok(())
    }

    pub(super) async fn dispatch_workspace_command(&self, cmd: GtkCommand) {
        match cmd {
            GtkCommand::CreateWorkspace {
                name,
                root,
                team,
                activate,
                ack,
            } => {
                let id = self.store.create_workspace_kind(name, root, team).await;
                let result = self.materialize_workspace(id, activate).await;
                if result.is_err() {
                    self.store.remove_workspace(id).await;
                }
                let _ = ack.send(result.map(|()| id));
            }
            GtkCommand::WorkspaceCreated { id, activate, ack } => {
                let _ = ack.send(self.materialize_workspace(id, activate).await);
            }
            GtkCommand::NewWorkspace {
                root,
                team,
                window_id,
            } => {
                let host = self
                    .detached_workspaces
                    .borrow()
                    .values()
                    .find(|host| Some(host.window.id()) == window_id)
                    .cloned();
                if window_id.is_some_and(|id| id != self.window.id()) && host.is_none() {
                    tracing::warn!(?window_id, "workspace creation window no longer exists");
                    return;
                }
                // Prefer the focused pane's cwd so a new tab opens
                // where the user was working, falling back to the
                // root the caller suggested (typically the daemon's
                // own current_dir) and finally to "/".
                let requesting_window = host
                    .as_ref()
                    .map(|host| &host.window)
                    .unwrap_or(&self.window);
                let resolved = self
                    .visible_workspace_in_window(requesting_window)
                    .and_then(|id| self.remembered_pane(id))
                    .and_then(|id| {
                        let r = self.pane_registry.borrow();
                        r.active_terminal(id).cloned()
                    })
                    .and_then(|p| p.current_dir())
                    .unwrap_or(root);
                let id = self.store.create_workspace_kind(None, resolved, team).await;
                if let Some(host) = host {
                    self.detached_workspaces.borrow_mut().insert(id, host);
                }
                if let Some(ws) = self.store.get_workspace(id).await {
                    self.render_workspace(&ws);
                    self.activate_workspace(id).await;
                }
            }
            GtkCommand::CloseDetachedWindow { workspaces } => {
                let surfaces = workspaces
                    .iter()
                    .flat_map(|id| self.pane_registry.borrow().surface_ids_in_workspace(*id))
                    .collect::<Vec<_>>();
                if !self.confirm_dirty_surfaces(&surfaces).await {
                    return;
                }
                for id in workspaces {
                    if self.store.remove_workspace(id).await {
                        self.drop_workspace(id);
                    }
                }
                self.activate_active_or_show_empty().await;
            }
            GtkCommand::RemoveWorkspace { id, confirm, ack } => {
                if confirm && !self.confirm_close_workspace(id).await {
                    let _ = ack.send(Err("Workspace close cancelled".into()));
                    return;
                }
                let closing_surfaces = self.pane_registry.borrow().surface_ids_in_workspace(id);
                if !self.confirm_dirty_surfaces(&closing_surfaces).await {
                    let _ = ack.send(Err("Workspace close cancelled".into()));
                    return;
                }
                if self.store.remove_workspace(id).await {
                    self.drop_workspace(id);
                    self.activate_active_or_show_empty().await;
                }
                let _ = ack.send(Ok(()));
            }
            GtkCommand::RemoveAllWorkspaces { ack } => {
                if !self.confirm_close_all_workspaces().await {
                    let _ = ack.send(());
                    return;
                }
                let closing_surfaces = self
                    .pane_registry
                    .borrow()
                    .editors
                    .keys()
                    .copied()
                    .collect::<Vec<_>>();
                if !self.confirm_dirty_surfaces(&closing_surfaces).await {
                    let _ = ack.send(());
                    return;
                }
                for id in self.store.remove_all_workspaces().await {
                    self.drop_workspace(id);
                }
                self.activate_active_or_show_empty().await;
                let _ = ack.send(());
            }
            GtkCommand::RenameWorkspace { id, name, ack } => {
                self.store.rename_workspace(id, name).await;
                if let Some(ws) = self.store.get_workspace(id).await {
                    self.upsert_workspace_sidebar(&ws);
                }
                let _ = ack.send(());
            }
            GtkCommand::SetWorkspaceColor { id, color, ack } => {
                self.store.set_workspace_color(id, color).await;
                if let Some(ws) = self.store.get_workspace(id).await {
                    self.upsert_workspace_sidebar(&ws);
                }
                self.refresh_agent_displays().await;
                let _ = ack.send(());
            }
            GtkCommand::ReorderWorkspace { id, target_index } => {
                tracing::info!(workspace = %id, target_index, "ReorderWorkspace dispatch start");
                let store_result = self.store.reorder_workspace(id, target_index).await;
                if store_result {
                    self.sidebar.reorder(id, target_index);
                    tracing::info!(workspace = %id, target_index, "ReorderWorkspace applied");
                } else {
                    tracing::warn!(
                        workspace = %id,
                        target_index,
                        "ReorderWorkspace store update returned false (no-op or unknown id)"
                    );
                }
            }
            GtkCommand::ShowRenameDialog { id } => {
                let id = match id {
                    Some(id) => Some(id),
                    None => self.store.active_workspace().await,
                };
                let Some(id) = id else {
                    return;
                };
                if let Some(ws) = self.store.get_workspace(id).await {
                    // Match cmux prefill behavior: start from custom_title when
                    // present so the user can edit it, otherwise show the current
                    // automatic name (`name`).
                    let prefill = ws.custom_title.as_deref().unwrap_or(&ws.name).to_string();
                    show_rename_dialog(
                        &self.workspace_window(id),
                        id,
                        &prefill,
                        self.bridge.clone(),
                    );
                }
            }
            GtkCommand::ShowColorDialog { id } => {
                let current = self.store.get_workspace(id).await.and_then(|w| w.color);
                show_color_dialog(&self.window, id, current.as_deref(), self.bridge.clone());
            }
            GtkCommand::FocusWorkspaceDir { dir } => {
                let (workspace_order, active_workspace) =
                    self.store.workspace_order_and_active().await;
                if workspace_order.is_empty() {
                    return;
                }
                let active = self
                    .sidebar
                    .selected_workspace()
                    .or(active_workspace.or_else(|| workspace_order.first().copied()));
                let Some(active) = active else { return };
                let cur = workspace_order
                    .iter()
                    .position(|x| *x == active)
                    .unwrap_or(0);
                let len = workspace_order.len();
                let next = match dir {
                    WsNav::Next => (cur + 1) % len,
                    WsNav::Prev => (cur + len - 1) % len,
                };
                let target = workspace_order[next];
                self.activate_workspace(target).await;
            }
            GtkCommand::FocusWorkspaceAt { idx } => {
                let (workspace_order, _) = self.store.workspace_order_and_active().await;
                let target_idx = (idx as usize).saturating_sub(1);
                if let Some(id) = workspace_order.get(target_idx).copied() {
                    self.activate_workspace(id).await;
                }
            }
            GtkCommand::ActivateWorkspace { id, ack } => {
                let available = ack.is_none() || self.surfaces.borrow().contains_key(&id);
                let result = if available {
                    self.activate_workspace(id).await;
                    Ok(())
                } else {
                    Err(format!("workspace not found: {id}"))
                };
                if let Some(ack) = ack {
                    let _ = ack.send(result);
                }
            }
            other => {
                unreachable!("workspace router got a non-workspace command: {other:?}")
            }
        }
    }
}
