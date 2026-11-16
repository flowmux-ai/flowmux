// SPDX-License-Identifier: GPL-3.0-or-later
//! Registry rows use process identity even while no terminal view exists.

use crate::agent_host::RegistrySnapshot;
use crate::bridge::{Bridge, GtkCommand};
use crate::ui::workspace_view::{
    managed_agent_dnd_content_provider, prepare_agent_tab_drag, ManagedAgentDndPayload,
};
use flowmux_core::{AgentAttention, AgentInstanceId, AgentStatus};
use flowmux_ipc::agent_host::{AgentInstance, ProcessState, TerminateMode};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

pub(super) fn new_agent_button() -> gtk::Button {
    let button = gtk::Button::from_icon_name("list-add-symbolic");
    button.add_css_class("flat");
    button.set_tooltip_text(Some("New Agent"));
    button.update_property(&[gtk::accessible::Property::Label("New Agent")]);
    button.set_action_name(Some("win.new-agent"));
    button
}

#[derive(Clone)]
pub(super) struct ManagedAgents {
    pub root: gtk::Box,
    rows: Rc<RefCell<HashMap<(AgentInstanceId, uuid::Uuid), Row>>>,
    attention: Rc<RefCell<Vec<AgentAttention>>>,
}

struct Row {
    root: gtk::Box,
    handle: gtk::Image,
    button: gtk::Button,
    interrupt: gtk::Button,
    destination: gtk::Button,
    terminate: gtk::Button,
    force: gtk::Button,
    title: gtk::Label,
    summary: gtk::Label,
    icon: gtk::Image,
}

impl ManagedAgents {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.set_visible(false);
        Self {
            root,
            rows: Default::default(),
            attention: Default::default(),
        }
    }

    pub fn attention(&self) -> Vec<AgentAttention> {
        self.attention.borrow().clone()
    }

    pub fn render(
        &self,
        snapshot: &RegistrySnapshot,
        bridge: &Bridge,
        drop_state: (Rc<Cell<bool>>, Rc<Cell<bool>>),
    ) {
        let live: Vec<_> = snapshot
            .agents
            .iter()
            .filter(|agent| {
                matches!(
                    agent.state,
                    ProcessState::Starting | ProcessState::Running | ProcessState::Stopping
                )
            })
            .collect();
        let mut rows = self.rows.borrow_mut();
        rows.retain(|key, row| {
            let keep = live
                .iter()
                .any(|agent| (agent.instance, agent.epoch) == *key);
            if !keep {
                self.root.remove(&row.root);
            }
            keep
        });
        let mut attention = self.attention.borrow_mut();
        attention.clear();
        let mut previous: Option<gtk::Widget> = None;
        // Host's map order is unspecified; keep rows stable between observations.
        let mut live = live;
        live.sort_by_key(|agent| agent.instance.0);
        for agent in live {
            let row = rows
                .entry((agent.instance, agent.epoch))
                .or_insert_with(|| {
                    let row = Row::new(agent, bridge.clone(), drop_state.clone());
                    self.root.append(&row.root);
                    row
                });
            let presence = agent.presence.as_ref().map(|p| p.display_presence());
            let (status, seen, waits, outcome) = if snapshot.available {
                (
                    presence.as_ref().map_or(AgentStatus::Unknown, |p| p.status),
                    presence.as_ref().is_none_or(|p| p.seen),
                    presence
                        .as_ref()
                        .map_or(&[][..], |p| p.pending_waits.as_slice()),
                    presence.as_ref().and_then(|p| p.turn_outcome),
                )
            } else {
                (AgentStatus::Unknown, true, &[][..], None)
            };
            attention.push(flowmux_core::agent_attention(status, seen, outcome, waits));
            let visual =
                crate::ui::agent_attention_visual_status(status, *attention.last().unwrap());
            row.icon
                .set_icon_name(Some(crate::ui::agent_status_icon_name(visual, seen)));
            row.title
                .set_label(&format!("{} · {}", agent.provider, agent.label));
            let location = if !snapshot.available {
                "Location unknown"
            } else if agent
                .attachment
                .as_ref()
                .is_some_and(|attachment| !attachment.ready)
            {
                "Connecting"
            } else if let Some(view) = &agent.view {
                if view.visible {
                    "Visible"
                } else {
                    "Open"
                }
            } else if agent.attachment.is_some() {
                "Location pending"
            } else {
                "Background"
            };
            let state = if !snapshot.available {
                "Host unavailable"
            } else {
                presence
                    .as_ref()
                    .filter(|_| agent.state == ProcessState::Running)
                    .and_then(|p| p.status_text())
                    .unwrap_or(match agent.state {
                        ProcessState::Starting => "Starting",
                        ProcessState::Stopping => "Stopping",
                        _ => "Running",
                    })
            };
            row.summary.set_label(&format!("{state} · {location}"));
            row.button.set_tooltip_text(Some(&format!(
                "{}\n{}\n{}\nOpen the same running agent",
                agent.label,
                agent.cwd.display(),
                agent.instance
            )));
            row.root.set_sensitive(snapshot.available);
            let running = agent.state == ProcessState::Running;
            row.button.set_sensitive(running);
            row.handle.set_sensitive(running);
            row.interrupt.set_sensitive(running);
            row.destination.set_sensitive(running);
            row.destination.set_label(if agent.attachment.is_some() {
                "Move to…"
            } else {
                "Open in…"
            });
            row.terminate.set_sensitive(running);
            row.force.set_sensitive(
                agent.state == ProcessState::Stopping && agent.force_terminate_allowed,
            );
            row.force.set_visible(agent.state == ProcessState::Stopping);
            if row.root.prev_sibling() != previous {
                self.root.reorder_child_after(&row.root, previous.as_ref());
            }
            previous = Some(row.root.clone().upcast());
        }
        self.root.set_visible(!rows.is_empty());
    }
}

impl Row {
    fn new(
        agent: &AgentInstance,
        bridge: Bridge,
        drop_state: (Rc<Cell<bool>>, Rc<Cell<bool>>),
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let handle = gtk::Image::from_icon_name("list-drag-handle-symbolic");
        handle.set_tooltip_text(Some("Drag running agent to a pane"));
        handle.update_property(&[gtk::accessible::Property::Label(
            "Drag running agent to a pane",
        )]);
        handle.set_cursor_from_name(Some("grab"));
        handle.set_pixel_size(14);
        let drag = gtk::DragSource::new();
        drag.set_actions(gtk::gdk::DragAction::MOVE);
        prepare_agent_tab_drag(&drag, &handle, drop_state);
        let payload = ManagedAgentDndPayload::local(agent);
        let drag_payload = payload.clone();
        drag.connect_prepare(move |_, _, _| {
            Some(managed_agent_dnd_content_provider(&drag_payload))
        });
        handle.add_controller(drag);
        root.append(&handle);
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("flowmux-agent-bar-item");
        button.set_hexpand(true);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let icon = gtk::Image::new();
        content.append(&icon);
        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.set_hexpand(true);
        let title = gtk::Label::new(None);
        title.add_css_class("caption-heading");
        let summary = gtk::Label::new(None);
        summary.add_css_class("caption");
        for label in [&title, &summary] {
            label.set_xalign(0.0);
            label.set_ellipsize(gtk::pango::EllipsizeMode::End);
            label.set_max_width_chars(40);
            text.append(label);
        }
        content.append(&text);
        button.set_child(Some(&content));
        let open_bridge = bridge.clone();
        let open_payload = payload.clone();
        button.connect_clicked(move |_| {
            let (ack, _) = tokio::sync::oneshot::channel();
            open_bridge.send_from_ui(GtkCommand::OpenManagedAgent {
                pane: None,
                tab_anchor: None,
                payload: open_payload.clone(),
                ack,
            });
        });
        root.append(&button);
        let menu = gtk::MenuButton::new();
        menu.set_icon_name("view-more-symbolic");
        menu.add_css_class("flat");
        menu.set_tooltip_text(Some("Agent actions"));
        menu.update_property(&[gtk::accessible::Property::Label("Agent actions")]);
        let popover = gtk::Popover::new();
        let actions = gtk::Box::new(gtk::Orientation::Vertical, 4);
        let destination = gtk::Button::with_label("Open in…");
        destination.add_css_class("flat");
        let destination_bridge = bridge.clone();
        let destination_popover = popover.downgrade();
        destination.connect_clicked(move |_| {
            if let Some(popover) = destination_popover.upgrade() {
                popover.popdown();
            }
            destination_bridge.send_from_ui(GtkCommand::ChooseManagedAgentDestination {
                payload: payload.clone(),
            });
        });
        actions.append(&destination);
        let interrupt = gtk::Button::with_label("Interrupt current turn (Ctrl+C)");
        interrupt.set_tooltip_text(Some(
            "Send Ctrl+C to the agent. The provider may interrupt work or exit.",
        ));
        let terminate = gtk::Button::with_label("Terminate agent…");
        let force = gtk::Button::with_label("Force terminate…");
        force.set_tooltip_text(Some("Available after normal termination times out"));
        for (button, mode) in [
            (&interrupt, TerminateMode::Interrupt),
            (&terminate, TerminateMode::Terminate),
            (&force, TerminateMode::Force),
        ] {
            button.add_css_class("flat");
            let bridge = bridge.clone();
            let popover = popover.downgrade();
            let binding = flowmux_core::AgentViewBinding {
                instance: agent.instance,
                epoch: agent.epoch,
            };
            button.connect_clicked(move |_| {
                if let Some(popover) = popover.upgrade() {
                    popover.popdown();
                }
                bridge.send_from_ui(GtkCommand::ControlManagedAgent { binding, mode });
            });
            actions.append(button);
        }
        popover.set_child(Some(&actions));
        menu.set_popover(Some(&popover));
        root.append(&menu);
        Self {
            root,
            handle,
            button,
            interrupt,
            destination,
            terminate,
            force,
            title,
            summary,
            icon,
        }
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    fn registry_rows_keep_identity_without_a_tab_and_distinguish_contact_loss() {
        let agent = AgentInstance {
            instance: AgentInstanceId::new(),
            epoch: uuid::Uuid::new_v4(),
            provider: "codex".into(),
            label: "Review".into(),
            cwd: "/tmp".into(),
            pid: Some(42),
            process_start: Some(1),
            boot_id: None,
            state: ProcessState::Running,
            force_terminate_allowed: false,
            exit_status: None,
            view: None,
            attachment: None,
            view_owner: None,
            error: None,
            presence: None,
        };
        let (bridge, rx) = Bridge::new();
        let list = ManagedAgents::new();
        let mut snapshot = RegistrySnapshot {
            agents: vec![agent.clone()],
            available: true,
            notifications: Vec::new(),
        };
        bridge.agent_registry.send_replace(snapshot.clone());
        let sidebar = crate::ui::sidebar::Sidebar::new(
            |_| {},
            |_| {},
            || {},
            bridge.clone(),
            crate::notifications::NotificationStore::new(),
            crate::activity::ActivityStore::new(),
            None,
            false,
        );
        sidebar.refresh_activity_panel(&[], None);
        assert!(
            sidebar.activity_panel_is_visible(),
            "the first registry row must reveal a hidden side panel"
        );
        let bar = crate::ui::agent_bar::AgentBar::new(bridge.clone(), Default::default());
        let empty = flowmux_core::AgentBarModel {
            visible: false,
            items: vec![],
        };
        bar.render_managed(&empty, true);
        assert!(
            bar.root.get_visible(),
            "the first registry row must reveal a hidden bar"
        );
        bar.render(&empty, &Default::default(), None);
        bar.render_managed(&empty, true);
        assert!(
            bar.root.get_visible(),
            "refreshing must not hide a registry-only bar"
        );
        list.render(&snapshot, &bridge, Default::default());
        let key = (agent.instance, agent.epoch);
        let button = list.rows.borrow()[&key].button.clone();
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Running · Background"
        );
        assert_eq!(
            list.rows.borrow()[&key].destination.label().as_deref(),
            Some("Open in…")
        );
        list.rows.borrow()[&key].destination.emit_clicked();
        assert!(
            matches!(rx.try_recv().unwrap(), GtkCommand::ChooseManagedAgentDestination { payload }
            if payload.instance == agent.instance && payload.epoch == agent.epoch)
        );
        button.emit_clicked();
        assert!(
            matches!(rx.try_recv().unwrap(), GtkCommand::OpenManagedAgent { pane: None, payload, .. }
            if payload.instance == agent.instance && payload.epoch == agent.epoch)
        );
        let attachment = flowmux_ipc::agent_host::AgentAttachment {
            key: flowmux_ipc::agent_host::AttachmentKey {
                instance: agent.instance,
                epoch: agent.epoch,
                lease: uuid::Uuid::new_v4(),
                generation: 1,
            },
            relay_pid: Some(43),
            relay_start: Some(2),
            ready: true,
        };
        snapshot.agents[0].attachment = Some(attachment);
        list.render(&snapshot, &bridge, Default::default());
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Running · Location pending"
        );
        snapshot.agents[0].view = Some(flowmux_ipc::agent_host::AgentViewLocation {
            window: uuid::Uuid::new_v4(),
            workspace: flowmux_core::WorkspaceId::new(),
            pane: flowmux_core::PaneId::new(),
            surface: flowmux_core::SurfaceId::new(),
            visible: false,
        });
        list.render(&snapshot, &bridge, Default::default());
        assert_eq!(list.rows.borrow()[&key].summary.label(), "Running · Open");
        assert_eq!(
            list.rows.borrow()[&key].destination.label().as_deref(),
            Some("Move to…")
        );
        snapshot.agents[0].view.as_mut().unwrap().visible = true;
        list.render(&snapshot, &bridge, Default::default());
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Running · Visible"
        );
        snapshot.agents[0].attachment.as_mut().unwrap().ready = false;
        list.render(&snapshot, &bridge, Default::default());
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Running · Connecting"
        );
        snapshot.agents[0].attachment.as_mut().unwrap().ready = true;
        snapshot.available = false;
        list.render(&snapshot, &bridge, Default::default());
        assert_eq!(list.rows.borrow()[&key].button, button);
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Host unavailable · Location unknown"
        );
        assert!(!list.rows.borrow()[&key].root.is_sensitive());
        snapshot.available = true;
        list.render(&snapshot, &bridge, Default::default());
        assert!(list.rows.borrow()[&key].root.is_sensitive());
        snapshot.agents[0].state = ProcessState::Stopping;
        list.render(&snapshot, &bridge, Default::default());
        {
            let rows = list.rows.borrow();
            let row = &rows[&key];
            assert_eq!(row.summary.label(), "Stopping · Visible");
            assert!(!row.button.is_sensitive());
            assert!(!row.interrupt.is_sensitive());
            assert!(!row.destination.is_sensitive());
            assert!(!row.terminate.is_sensitive());
            assert!(row.force.get_visible());
            assert!(!row.force.is_sensitive());
        }
        snapshot.agents[0].force_terminate_allowed = true;
        list.render(&snapshot, &bridge, Default::default());
        let force = list.rows.borrow()[&key].force.clone();
        assert!(force.is_sensitive());
        force.emit_clicked();
        assert!(
            matches!(rx.try_recv().unwrap(), GtkCommand::ControlManagedAgent {
            binding, mode: TerminateMode::Force
        } if binding.instance == agent.instance && binding.epoch == agent.epoch)
        );
        snapshot.agents[0].state = ProcessState::Exited;
        list.render(&snapshot, &bridge, Default::default());
        assert!(list.rows.borrow().is_empty());
        assert!(!list.root.is_visible());
    }
}
