// SPDX-License-Identifier: GPL-3.0-or-later
//! AgentOffice maps live agents to their workspace terminals.

use super::*;
use crate::ui::agent_office::AgentOffice;
use flowmux_core::{AgentActivity, AgentBarItem, AgentBarModel, AgentPresence, AgentStatus};

pub(super) struct ActiveAgentOffice {
    pub(super) view: AgentOffice,
    saved_focus: Option<glib::WeakRef<gtk::Widget>>,
    background_can_focus: bool,
    _native_views: crate::ui::browser_pane::NativeBrowserViewsSuspend,
}

impl WindowController {
    pub(super) async fn agent_office_model(
        &self,
        terminal_agents: &AgentBarModel,
    ) -> AgentBarModel {
        let mut model = terminal_agents.clone();
        for agent in self.running_agents() {
            let Some(view) = self.verified_agent_location(&agent).await else {
                continue;
            };
            // A terminal's hooks reach this window; the Host only sees its agent's process,
            // which it reports as idle. The terminal's own item keeps the live status.
            if model.items.iter().any(|item| item.surface == view.surface) {
                continue;
            }
            let Some(title) = self.store.surface_title(view.pane, view.surface).await else {
                continue;
            };
            let presence = agent.presence.unwrap_or_else(|| {
                let mut presence =
                    AgentPresence::new(agent.provider, AgentActivity::Idle, agent.pid);
                presence.status = AgentStatus::Unknown;
                presence
            });
            let color = self.sidebar.workspace_color(view.workspace);
            if let Some(item) = AgentBarItem::from_presence(
                view.workspace,
                view.pane,
                view.surface,
                &title,
                &presence,
                color.as_deref(),
            ) {
                model.items.push(item);
            }
        }
        model.visible = !model.items.is_empty();
        model
    }

    pub(super) async fn toggle_agent_office(&self) {
        if self.agent_office.borrow().is_some() {
            self.close_agent_office();
            return;
        }
        self.dismiss_workspace_overview_immediately();
        self.cancel_pane_zoom_transition();
        let view = AgentOffice::new(self.bridge.clone());
        let terminals = self
            .agent_display_model(self.store.agent_bar_model().await)
            .await;
        let model = self.agent_office_model(&terminals).await;
        view.render(&self.sidebar.workspace_titles().borrow(), &model);
        let saved_focus =
            gtk::prelude::GtkWindowExt::focus(&self.window).map(|widget| widget.downgrade());
        let background_can_focus = self.sidebar_split.can_focus();
        // Queued pane focus and Tab navigation must not reach a covered terminal.
        self.sidebar_split.set_can_focus(false);
        let native_views = crate::ui::browser_pane::suspend_native_browser_views_for_window(
            self.window.upcast_ref(),
        );
        self.content_overlay.add_overlay(&view.root);
        view.close.grab_focus();
        *self.agent_office.borrow_mut() = Some(ActiveAgentOffice {
            view,
            saved_focus,
            background_can_focus,
            _native_views: native_views,
        });
    }

    pub(super) fn close_agent_office(&self) {
        let Some(office) = self.agent_office.borrow_mut().take() else {
            return;
        };
        self.content_overlay.remove_overlay(&office.view.root);
        self.sidebar_split
            .set_can_focus(office.background_can_focus);
        let focus = office.saved_focus.as_ref().and_then(glib::WeakRef::upgrade);
        drop(office);
        if let Some(focus) = focus {
            focus.grab_focus();
        }
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    async fn agent_office_blocks_deferred_focus_into_hidden_terminals() {
        let (controller, _, pane) = super::super::tests::build_single_workspace_controller(
            "com.flowmux.App.UiTest.AgentOfficeFocus",
        )
        .await;
        controller.window.present();
        glib::timeout_future(Duration::from_millis(100)).await;
        controller.focus_pane(pane);
        controller.dispatch(GtkCommand::ToggleAgentOffice).await;
        glib::timeout_future(Duration::from_millis(100)).await;
        let root = controller
            .agent_office
            .borrow()
            .as_ref()
            .unwrap()
            .view
            .root
            .clone();
        let focus = gtk::prelude::GtkWindowExt::focus(&controller.window).unwrap();
        assert!(
            focus.is_ancestor(&root),
            "queued focus escaped behind AgentOffice"
        );
        for _ in 0..10 {
            controller
                .window
                .child_focus(gtk::DirectionType::TabForward);
            let focus = gtk::prelude::GtkWindowExt::focus(&controller.window).unwrap();
            assert!(focus.is_ancestor(&root), "Tab escaped behind AgentOffice");
        }
        controller.dispatch(GtkCommand::ToggleAgentOffice).await;
        controller.focus_pane(pane);
        glib::timeout_future(Duration::from_millis(100)).await;
        let frame = controller.pane_registry.borrow().pane_frame(pane).unwrap();
        let focus = gtk::prelude::GtkWindowExt::focus(&controller.window).unwrap();
        assert!(focus.is_ancestor(&frame));
        controller.window.destroy();
    }

    #[gtk::test]
    async fn agent_office_tracks_live_status_and_navigates_without_replacing_terminal() {
        let (controller, workspace, pane) = super::super::tests::build_single_workspace_controller(
            "com.flowmux.App.UiTest.AgentOffice",
        )
        .await;
        let terminal = controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .widget
            .clone();
        let surface = controller
            .pane_registry
            .borrow()
            .active_surface(pane)
            .unwrap();
        controller.dispatch(GtkCommand::ToggleAgentOffice).await;
        assert!(controller.agent_office.borrow().is_some());
        assert!(controller
            .agent_office
            .borrow()
            .as_ref()
            .unwrap()
            .view
            .room_titles()
            .is_empty());
        controller
            .store
            .set_agent_activity(
                surface,
                Some(flowmux_core::AgentPresence::new(
                    "codex",
                    flowmux_core::AgentActivity::Running,
                    Some(42),
                )),
            )
            .await;
        controller
            .dispatch(GtkCommand::SetAgentStatus { workspace })
            .await;
        let (ack, reply) = tokio::sync::oneshot::channel();
        controller
            .dispatch(GtkCommand::RenameWorkspace {
                id: workspace,
                name: "Live office name".into(),
                ack,
            })
            .await;
        reply.await.unwrap();
        assert!(controller
            .agent_office
            .borrow()
            .as_ref()
            .unwrap()
            .view
            .room_titles()[0]
            .starts_with("Live office name"));
        let added = controller
            .store
            .create_workspace(
                Some("Second office".into()),
                std::path::PathBuf::from("/tmp"),
            )
            .await;
        let (ack, reply) = tokio::sync::oneshot::channel();
        controller
            .dispatch(GtkCommand::WorkspaceCreated {
                id: added,
                activate: true,
                ack,
            })
            .await;
        reply.await.unwrap().unwrap();
        assert_eq!(
            controller
                .agent_office
                .borrow()
                .as_ref()
                .unwrap()
                .view
                .room_titles()
                .len(),
            1
        );
        let (ack, reply) = tokio::sync::oneshot::channel();
        controller
            .dispatch(GtkCommand::RemoveWorkspace {
                id: added,
                confirm: false,
                ack,
            })
            .await;
        reply.await.unwrap().unwrap();
        assert_eq!(
            controller
                .agent_office
                .borrow()
                .as_ref()
                .unwrap()
                .view
                .room_titles()
                .len(),
            1
        );

        for activity in [
            flowmux_core::AgentActivity::Running,
            flowmux_core::AgentActivity::NeedsInput,
            flowmux_core::AgentActivity::Idle,
        ] {
            controller
                .store
                .set_agent_activity(
                    surface,
                    Some(flowmux_core::AgentPresence::new(
                        "codex",
                        activity,
                        Some(42),
                    )),
                )
                .await;
            controller
                .dispatch(GtkCommand::SetAgentStatus { workspace })
                .await;
            let office = controller.agent_office.borrow();
            let office = &office.as_ref().unwrap().view;
            assert_eq!(
                office.rendered_status(surface),
                Some(flowmux_core::AgentStatus::from_activity(activity))
            );
        }
        controller.store.set_agent_activity(surface, None).await;
        controller
            .dispatch(GtkCommand::SetAgentStatus { workspace })
            .await;
        assert_eq!(
            controller
                .agent_office
                .borrow()
                .as_ref()
                .unwrap()
                .view
                .rendered_status(surface),
            None
        );
        assert!(controller
            .agent_office
            .borrow()
            .as_ref()
            .unwrap()
            .view
            .room_titles()
            .is_empty());
        controller
            .dispatch(GtkCommand::OpenAgentBarItem {
                workspace,
                pane,
                surface,
            })
            .await;
        assert!(controller.agent_office.borrow().is_none());
        assert_eq!(
            controller
                .pane_registry
                .borrow()
                .active_terminal(pane)
                .unwrap()
                .widget,
            terminal
        );
        controller.dispatch(GtkCommand::ToggleAgentOffice).await;
        controller.dispatch(GtkCommand::ToggleAgentOffice).await;
        assert!(controller.agent_office.borrow().is_none());
        controller.window.destroy();
    }
}
