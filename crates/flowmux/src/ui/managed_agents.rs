// SPDX-License-Identifier: GPL-3.0-or-later
//! Registry rows use process identity even while no terminal view exists.

use crate::agent_host::RegistrySnapshot;
use crate::bridge::{Bridge, GtkCommand};
use crate::ui::workspace_view::ManagedAgentDndPayload;
#[cfg(target_os = "macos")]
use crate::ui::workspace_view::PaneRegistry;
#[cfg(not(target_os = "macos"))]
use crate::ui::workspace_view::{managed_agent_dnd_content_provider, prepare_agent_tab_drag};
use flowmux_core::{AgentAttention, AgentInstanceId, AgentStatus, WorkspaceId};
use flowmux_ipc::agent_host::{AgentInstance, ProcessState, TerminateMode};
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

#[cfg(target_os = "macos")]
mod macos_drag;

#[cfg(all(test, target_os = "macos"))]
pub(super) use macos_drag::check_native as check_native_drag;

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
    notice: gtk::Label,
    stop_other_host: gtk::Button,
    live: gtk::Box,
    rows: Rc<RefCell<HashMap<(AgentInstanceId, uuid::Uuid), Row>>>,
    attention: Rc<RefCell<Vec<AgentAttention>>>,
    #[cfg(target_os = "macos")]
    drag_registry: std::rc::Weak<RefCell<PaneRegistry>>,
}

struct Row {
    root: gtk::Box,
    button: gtk::Button,
    content: gtk::Box,
    color_bar: Option<(String, gtk::Widget)>,
    /// Stops the agent; becomes the force stop once a normal stop has timed out.
    stop: gtk::Button,
    stop_mode: Rc<Cell<TerminateMode>>,
    title: gtk::Label,
    summary: gtk::Label,
    icon: gtk::Image,
    spinner: gtk::Spinner,
}

impl ManagedAgents {
    pub fn new(orientation: gtk::Orientation) -> Self {
        let root = gtk::Box::new(orientation, 6);
        root.set_visible(false);
        // Shown even without rows: a fresh GUI has never observed the old Host.
        let notice = gtk::Label::new(None);
        notice.set_xalign(0.0);
        notice.add_css_class("warning");
        notice.set_visible(false);
        if orientation == gtk::Orientation::Vertical {
            notice.set_wrap(true);
            notice.set_wrap_mode(gtk::pango::WrapMode::WordChar);
            notice.set_selectable(true);
        } else {
            notice.set_ellipsize(gtk::pango::EllipsizeMode::End);
            notice.set_max_width_chars(48);
        }
        root.append(&notice);
        let stop_other_host = gtk::Button::with_label("Stop other Host…");
        stop_other_host.set_action_name(Some("win.stop-other-agent-host"));
        stop_other_host.set_halign(gtk::Align::Start);
        stop_other_host.set_visible(false);
        root.append(&stop_other_host);
        let live = gtk::Box::new(orientation, 6);
        root.append(&live);
        Self {
            root,
            notice,
            stop_other_host,
            live,
            rows: Default::default(),
            attention: Default::default(),
            #[cfg(target_os = "macos")]
            drag_registry: Default::default(),
        }
    }

    #[cfg(target_os = "macos")]
    pub fn set_drag_registry(&mut self, registry: &Rc<RefCell<PaneRegistry>>) {
        self.drag_registry = Rc::downgrade(registry);
    }

    pub fn attention(&self) -> Vec<AgentAttention> {
        self.attention.borrow().clone()
    }

    pub fn render(
        &self,
        snapshot: &RegistrySnapshot,
        bridge: &Bridge,
        #[cfg_attr(target_os = "macos", allow(unused_variables))] drop_state: (
            Rc<Cell<bool>>,
            Rc<Cell<bool>>,
        ),
        workspace_color: impl Fn(WorkspaceId) -> Option<String>,
    ) {
        // An ended execution has nothing to open or stop, so it leaves the list.
        let agents: Vec<_> = snapshot
            .agents
            .iter()
            .filter(|agent| !matches!(agent.state, ProcessState::Exited | ProcessState::Failed))
            // A terminal shows its own agent; its shell is listed here only when an agent
            // keeps it running without any terminal showing it.
            .filter(|agent| !agent.shell || (agent.presence.is_some() && agent.view.is_none()))
            .collect();
        let mut rows = self.rows.borrow_mut();
        rows.retain(|key, row| {
            let keep = agents
                .iter()
                .any(|agent| (agent.instance, agent.epoch) == *key);
            if !keep {
                self.live.remove(&row.root);
            }
            keep
        });
        let mut attention = self.attention.borrow_mut();
        attention.clear();
        let mut previous: Option<gtk::Widget> = None;
        // The Host supplies admission order, independent of activity changes.
        for agent in agents {
            let row = rows
                .entry((agent.instance, agent.epoch))
                .or_insert_with(|| {
                    let row = Row::new(
                        agent,
                        bridge.clone(),
                        #[cfg(target_os = "macos")]
                        self.drag_registry.clone(),
                        #[cfg(not(target_os = "macos"))]
                        drop_state.clone(),
                    );
                    self.live.append(&row.root);
                    row
                });
            let color = if snapshot.available {
                agent
                    .view
                    .as_ref()
                    .and_then(|view| workspace_color(view.workspace))
            } else {
                None
            };
            row.set_workspace_color(color);
            // The provider exited while its descendants keep the execution alive.
            let root_exited = agent.state == ProcessState::Running && agent.exit_status.is_some();
            let presence = agent
                .presence
                .as_ref()
                .filter(|_| !root_exited)
                .map(|p| p.display_presence());
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
            let row_attention = flowmux_core::agent_attention(status, seen, outcome, waits);
            attention.push(row_attention);
            let visual = crate::ui::agent_attention_visual_status(status, row_attention);
            let working = visual == AgentStatus::Working && agent.state == ProcessState::Running;
            row.spinner.set_spinning(working);
            row.spinner.set_visible(working);
            row.icon.set_visible(!working);
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
            } else if root_exited {
                "Provider exited; background processes still running"
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
            let tooltip = format!(
                "{}\n{}\n{}\nOpen the same running agent, or drag it to a pane",
                agent.label,
                agent.cwd.display(),
                agent.instance,
            );
            row.button.set_tooltip_text(Some(&tooltip));
            row.button
                .update_property(&[gtk::accessible::Property::Description(&tooltip)]);
            row.root.set_sensitive(snapshot.available);
            let running = agent.state == ProcessState::Running;
            row.button.set_sensitive(running);
            // One button: stop, then force once the normal stop has timed out. An agent in a
            // terminal stays running while it is asked to stop: its shell is not stopping.
            let stopping =
                agent.state == ProcessState::Stopping || (running && agent.force_terminate_allowed);
            let label = if stopping {
                "Force terminate agent"
            } else {
                "Terminate agent"
            };
            row.stop_mode.set(if stopping {
                TerminateMode::Force
            } else {
                TerminateMode::Terminate
            });
            row.stop
                .set_sensitive(running || (stopping && agent.force_terminate_allowed));
            // The force stop must not look like the ordinary stop.
            row.stop.set_icon_name(if stopping {
                "process-stop-symbolic"
            } else {
                "window-close-symbolic"
            });
            if stopping {
                row.stop.add_css_class("destructive-action");
            } else {
                row.stop.remove_css_class("destructive-action");
            }
            row.stop.set_tooltip_text(Some(label));
            row.stop
                .update_property(&[gtk::accessible::Property::Label(label)]);
            if row.root.prev_sibling() != previous {
                self.live.reorder_child_after(&row.root, previous.as_ref());
            }
            previous = Some(row.root.clone().upcast());
        }
        let notice = snapshot.incompatible.as_ref().map(ToString::to_string).or_else(|| {
            (!snapshot.other_hosts.is_empty()).then(|| {
                "Another Flowmux Agent Host is still running. Open its agents with the Flowmux build that started it.".to_owned()
            })
        });
        self.notice.set_label(notice.as_deref().unwrap_or_default());
        self.notice.set_tooltip_text(notice.as_deref());
        self.notice.set_visible(notice.is_some());
        self.stop_other_host
            .set_visible(!snapshot.other_hosts.is_empty());
        self.root.set_visible(!rows.is_empty() || notice.is_some());
    }
}

impl Row {
    fn set_workspace_color(&mut self, color: Option<String>) {
        if self.color_bar.as_ref().map(|(color, _)| color) == color.as_ref() {
            return;
        }
        if let Some((_, old)) = self.color_bar.take() {
            self.content.remove(&old);
        }
        if let Some(color) = color {
            let bar = super::sidebar::color_bar(&color);
            bar.set_vexpand(false);
            self.content.prepend(&bar);
            self.color_bar = Some((color, bar));
        }
    }

    fn new(
        agent: &AgentInstance,
        bridge: Bridge,
        #[cfg(target_os = "macos")] drag_registry: std::rc::Weak<RefCell<PaneRegistry>>,
        #[cfg(not(target_os = "macos"))] drop_state: (Rc<Cell<bool>>, Rc<Cell<bool>>),
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let payload = ManagedAgentDndPayload::local(agent);
        let button = gtk::Button::new();
        button.add_css_class("flat");
        button.add_css_class("flowmux-agent-bar-item");
        button.set_hexpand(true);
        let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let spinner = gtk::Spinner::builder()
            .accessible_role(gtk::AccessibleRole::Img)
            .build();
        spinner.set_size_request(12, 12);
        spinner.add_css_class(crate::ui::agent_status_css_class(
            AgentStatus::Working,
            true,
        ));
        spinner.update_property(&[gtk::accessible::Property::Label("Agent working")]);
        spinner.set_visible(false);
        content.append(&spinner);
        let icon = gtk::Image::new();
        icon.set_pixel_size(12);
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
        // The row itself is the drag source; a click still opens the agent.
        #[cfg(target_os = "macos")]
        macos_drag::install(
            button.upcast_ref(),
            drag_registry,
            bridge.clone(),
            payload.clone(),
        );
        #[cfg(not(target_os = "macos"))]
        {
            let drag = gtk::DragSource::new();
            drag.set_actions(gtk::gdk::DragAction::MOVE);
            drag.set_propagation_phase(gtk::PropagationPhase::Capture);
            prepare_agent_tab_drag(&drag, &button, drop_state);
            let payload = payload.clone();
            drag.connect_prepare(move |_, _, _| Some(managed_agent_dnd_content_provider(&payload)));
            button.add_controller(drag);
        }
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
        let stop = gtk::Button::from_icon_name("window-close-symbolic");
        stop.add_css_class("flat");
        let stop_mode = Rc::new(Cell::new(TerminateMode::Terminate));
        let mode = stop_mode.clone();
        let binding = flowmux_core::AgentViewBinding {
            instance: agent.instance,
            epoch: agent.epoch,
        };
        stop.connect_clicked(move |button| {
            let Some(window) = button.root().and_downcast::<adw::ApplicationWindow>() else {
                return;
            };
            bridge.send_from_ui(GtkCommand::ControlManagedAgent {
                binding,
                mode: mode.get(),
                window: window.downgrade().into(),
            });
        });
        root.append(&stop);
        Self {
            root,
            button,
            content,
            color_bar: None,
            stop,
            stop_mode,
            title,
            summary,
            icon,
            spinner,
        }
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    use adw::prelude::AdwApplicationWindowExt;

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
            shell: false,
        };
        let (bridge, rx) = Bridge::new();
        let list = ManagedAgents::new(gtk::Orientation::Vertical);
        let mut snapshot = RegistrySnapshot {
            agents: vec![agent.clone()],
            available: true,
            ..Default::default()
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
        sidebar.refresh_activity_panel(&[], None, |_| None);
        assert!(
            sidebar.activity_panel_is_visible(),
            "the first registry row must reveal a hidden side panel"
        );
        let bar = crate::ui::agent_bar::AgentBar::new(bridge.clone(), Default::default());
        let empty = flowmux_core::AgentBarModel {
            visible: false,
            items: vec![],
        };
        bar.render_managed(&empty, true, |_| None);
        assert!(
            bar.root.get_visible(),
            "the first registry row must reveal a hidden bar"
        );
        bar.render(&empty, &Default::default(), None);
        bar.render_managed(&empty, true, |_| None);
        assert!(
            bar.root.get_visible(),
            "refreshing must not hide a registry-only bar"
        );
        let single_bar_height = bar.root.measure(gtk::Orientation::Vertical, -1).1;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        let single_list_height = list.root.measure(gtk::Orientation::Vertical, -1).1;
        let key = (agent.instance, agent.epoch);
        let button = list.rows.borrow()[&key].button.clone();
        snapshot.agents[0].presence = Some(flowmux_core::AgentPresence::new(
            "codex",
            flowmux_core::AgentActivity::Running,
            Some(42),
        ));
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        let content = button.child().unwrap().downcast::<gtk::Box>().unwrap();
        let spinner = content
            .first_child()
            .unwrap()
            .downcast::<gtk::Spinner>()
            .expect("managed Working must animate like other Agents rows");
        assert!(spinner.is_spinning() && spinner.get_visible());
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert_eq!(list.rows.borrow()[&key].button, button);
        assert_eq!(content.first_child().unwrap(), spinner);
        assert!(spinner.is_spinning());
        snapshot.agents[0].presence = None;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert!(!spinner.is_spinning() && !spinner.get_visible());
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Running · Background"
        );
        assert_eq!(
            list.rows.borrow()[&key].stop.tooltip_text().as_deref(),
            Some("Terminate agent")
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
        list.render(&snapshot, &bridge, Default::default(), |_| None);
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
        let workspace = snapshot.agents[0].view.as_ref().unwrap().workspace;
        let workspace_color = |id| (id == workspace).then(|| "#33aabb".to_string());
        list.render(&snapshot, &bridge, Default::default(), workspace_color);
        assert_eq!(list.rows.borrow()[&key].summary.label(), "Running · Open");
        let stripe = list.rows.borrow()[&key].color_bar.clone().unwrap();
        assert_eq!(stripe.0, "#33aabb");
        assert!(!stripe.1.vexpands());
        assert_eq!(content.first_child(), Some(stripe.1.clone()));
        list.render(&snapshot, &bridge, Default::default(), workspace_color);
        assert_eq!(content.first_child(), Some(stripe.1));
        let destination = WorkspaceId::new();
        snapshot.agents[0].view.as_mut().unwrap().workspace = destination;
        list.render(&snapshot, &bridge, Default::default(), |id| {
            (id == destination).then(|| "#cc7722".to_string())
        });
        assert_eq!(
            list.rows.borrow()[&key].color_bar.as_ref().unwrap().0,
            "#cc7722"
        );

        snapshot.agents[0].view.as_mut().unwrap().visible = true;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Running · Visible"
        );
        snapshot.agents[0].attachment.as_mut().unwrap().ready = false;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Running · Connecting"
        );
        snapshot.agents[0].attachment.as_mut().unwrap().ready = true;
        snapshot.available = false;
        list.render(&snapshot, &bridge, Default::default(), |_| {
            Some("#cc7722".into())
        });
        assert_eq!(list.rows.borrow()[&key].button, button);
        assert!(list.rows.borrow()[&key].color_bar.is_none());
        assert_eq!(
            list.rows.borrow()[&key].summary.label(),
            "Host unavailable · Location unknown"
        );
        assert!(!list.rows.borrow()[&key].root.is_sensitive());
        snapshot.available = true;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert!(list.rows.borrow()[&key].root.is_sensitive());
        // An agent in a terminal that ignored its stop: still running, and force is offered.
        snapshot.agents[0].force_terminate_allowed = true;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        {
            let rows = list.rows.borrow();
            let stop = &rows[&key].stop;
            assert!(rows[&key].button.is_sensitive() && stop.is_sensitive());
            assert_eq!(
                stop.tooltip_text().as_deref(),
                Some("Force terminate agent")
            );
        }
        snapshot.agents[0].force_terminate_allowed = false;
        snapshot.agents[0].state = ProcessState::Stopping;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        {
            let rows = list.rows.borrow();
            let row = &rows[&key];
            assert_eq!(row.summary.label(), "Stopping · Visible");
            assert!(!row.button.is_sensitive());
            // The stop button becomes the force stop, disabled until the normal stop times out.
            assert_eq!(
                row.stop.tooltip_text().as_deref(),
                Some("Force terminate agent")
            );
            assert!(!row.stop.is_sensitive());
        }
        snapshot.agents[0].force_terminate_allowed = true;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        let force = list.rows.borrow()[&key].stop.clone();
        assert!(force.is_sensitive());
        let window = adw::ApplicationWindow::builder().build();
        window.set_content(Some(&list.root));
        force.emit_clicked();
        assert!(
            matches!(rx.try_recv().unwrap(), GtkCommand::ControlManagedAgent {
            binding, mode: TerminateMode::Force, window: source
        } if binding.instance == agent.instance && binding.epoch == agent.epoch
            && source.upgrade().as_ref() == Some(&window))
        );
        // An ended execution leaves the list without a dismissal step.
        snapshot.agents[0].state = ProcessState::Exited;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert!(list.rows.borrow().is_empty());
        assert!(list.attention().is_empty());
        assert!(!list.root.is_visible());

        let mut second = agent.clone();
        second.instance = AgentInstanceId(uuid::Uuid::nil());
        snapshot.agents = vec![agent.clone(), second.clone()];
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        bridge.agent_registry.send_replace(snapshot.clone());
        bar.render_managed(&empty, true, |_| None);
        assert_eq!(
            bar.root.measure(gtk::Orientation::Vertical, -1).1,
            single_bar_height
        );
        assert!(list.root.measure(gtk::Orientation::Vertical, -1).1 > single_list_height);
        assert_eq!(
            list.live.first_child(),
            Some(list.rows.borrow()[&key].root.clone().upcast())
        );
        snapshot.agents[0].state = ProcessState::Stopping;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert_eq!(
            list.live.first_child(),
            Some(list.rows.borrow()[&key].root.clone().upcast())
        );
        snapshot.agents[0].state = ProcessState::Failed;
        snapshot.agents[0].error = Some("fixture failure".into());
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert!(!list.rows.borrow().contains_key(&key));
        assert_eq!(
            list.live.first_child(),
            Some(
                list.rows.borrow()[&(second.instance, second.epoch)]
                    .root
                    .clone()
                    .upcast()
            )
        );

        window.close();
    }

    #[gtk::test]
    fn incompatible_host_notice_shows_without_rows_and_clears_when_the_host_is_gone() {
        let (bridge, _rx) = Bridge::new();
        for orientation in [gtk::Orientation::Vertical, gtk::Orientation::Horizontal] {
            let list = ManagedAgents::new(orientation);
            let mut snapshot = RegistrySnapshot::default();
            list.render(&snapshot, &bridge, Default::default(), |_| None);
            assert!(!list.root.is_visible());
            snapshot.incompatible = Some(flowmux_ipc::agent_host::IncompatibleHost {
                host_version: Some(14),
                socket: "/run/user/1000/flowmux-agent-host.sock".into(),
            });
            list.render(&snapshot, &bridge, Default::default(), |_| None);
            assert!(list.root.is_visible() && list.notice.is_visible());
            assert!(list.rows.borrow().is_empty());
            let text = list.notice.label();
            assert_eq!(text, snapshot.incompatible.as_ref().unwrap().to_string());
            assert!(text.contains("uses protocol 14"), "{text}");
            assert!(text.contains("cannot open them"), "{text}");
            assert_eq!(list.notice.tooltip_text().as_deref(), Some(text.as_str()));
            snapshot.incompatible = None;
            list.render(&snapshot, &bridge, Default::default(), |_| None);
            assert!(!list.notice.is_visible() && !list.root.is_visible());
        }
    }

    #[gtk::test]
    fn provider_exit_with_surviving_children_shows_plain_state_and_keeps_terminate() {
        let mut presence = flowmux_core::AgentPresence::new(
            "codex",
            flowmux_core::AgentActivity::Running,
            Some(42),
        );
        presence.status = AgentStatus::Working;
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
            exit_status: Some(0),
            view: None,
            attachment: None,
            view_owner: None,
            error: None,
            presence: Some(presence),
            shell: false,
        };
        let (bridge, _rx) = Bridge::new();
        let list = ManagedAgents::new(gtk::Orientation::Horizontal);
        let mut snapshot = RegistrySnapshot {
            agents: vec![agent.clone()],
            available: true,
            ..Default::default()
        };
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        let key = (agent.instance, agent.epoch);
        {
            let rows = list.rows.borrow();
            let row = &rows[&key];
            assert_eq!(
                row.summary.label(),
                "Provider exited; background processes still running · Background"
            );
            assert!(!row.spinner.is_spinning() && !row.spinner.get_visible());
            assert!(row.stop.is_sensitive());
            assert!(row.button.is_sensitive());
        }
        assert!(list.live.first_child().is_some());
        snapshot.agents[0].state = ProcessState::Exited;
        list.render(&snapshot, &bridge, Default::default(), |_| None);
        assert!(list.live.first_child().is_none());
    }
}
