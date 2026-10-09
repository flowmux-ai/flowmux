// SPDX-License-Identifier: GPL-3.0-or-later
//! AgentOffice uses the same live model and navigation as the Agents bar.

use super::*;
use crate::ui::agent_office::AgentOffice;

pub(super) struct ActiveAgentOffice {
    pub(super) view: AgentOffice,
    saved_focus: Option<glib::WeakRef<gtk::Widget>>,
    _native_views: crate::ui::browser_pane::NativeBrowserViewsSuspend,
}

impl WindowController {
    pub(super) async fn toggle_agent_office(&self) {
        if self.agent_office.borrow().is_some() {
            self.close_agent_office();
            return;
        }
        self.dismiss_workspace_overview_immediately();
        self.cancel_pane_zoom_transition();
        let view = AgentOffice::new(self.bridge.clone());
        let model = self.store.agent_bar_model().await;
        view.render(&self.sidebar.workspace_titles().borrow(), &model);
        let saved_focus =
            gtk::prelude::GtkWindowExt::focus(&self.window).map(|widget| widget.downgrade());
        let native_views = crate::ui::browser_pane::suspend_native_browser_views_for_window(
            self.window.upcast_ref(),
        );
        self.content_overlay.add_overlay(&view.root);
        view.close.grab_focus();
        *self.agent_office.borrow_mut() = Some(ActiveAgentOffice {
            view,
            saved_focus,
            _native_views: native_views,
        });
    }

    pub(super) fn close_agent_office(&self) {
        let Some(office) = self.agent_office.borrow_mut().take() else {
            return;
        };
        self.content_overlay.remove_overlay(&office.view.root);
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
