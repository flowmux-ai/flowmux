// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

impl WindowController {
    pub(super) async fn open_diff_review(&self) {
        let Some(pane) = self.focused_pane.get() else {
            return;
        };
        if self.pane_registry.borrow().is_ssh_pane(pane) {
            self.clipboard_toast.show_with_message("Diff review requires a local checkout. Open the remote checkout locally to review it.");
            return;
        }
        let Some(root) = self.file_browser_root_for_pane(pane).await else {
            return;
        };
        let root =
            match gtk::gio::spawn_blocking(move || flowmux_vcs::review::repository_root(&root))
                .await
            {
                Ok(Ok(root)) => root,
                Ok(Err(error)) => {
                    self.clipboard_toast.show_with_message(&error);
                    return;
                }
                Err(_) => {
                    self.clipboard_toast
                        .show_with_message("Cannot locate the Git checkout.");
                    return;
                }
            };
        let review = self
            .reviews
            .borrow_mut()
            .entry(root.clone())
            .or_insert_with(|| {
                let review = crate::ui::review_window::ReviewWindow::new(&self.window, root);
                review.connect_targets(self.bridge.clone());
                review
            })
            .clone();
        review.window.present();
        self.refresh_review_targets().await;
    }

    pub(super) async fn refresh_review_targets(&self) {
        if self.reviews.borrow().is_empty() {
            return;
        }
        let mut targets = Vec::new();
        for item in self.store.agent_bar_model().await.items {
            let Some(located) = self.store.located_agent_presence(item.surface).await else {
                continue;
            };
            if !self.surfaces.borrow().contains_key(&located.workspace) {
                continue;
            }
            if located
                .presence
                .pid
                .is_some_and(|pid| !flowmux_procmon::pid_alive(pid))
            {
                continue;
            }
            targets.push(crate::ui::review_window::ReviewTarget {
                surface: item.surface,
                name: located.presence.name.clone(),
                pid: located.presence.pid,
                session: located.presence.session_id.clone(),
                label: format!(
                    "{} · {} / {} · {}",
                    located.presence.name,
                    located.workspace_label,
                    located.surface_label,
                    item.status_text
                ),
            });
        }
        for review in self.reviews.borrow().values() {
            review.set_targets(targets.clone());
        }
    }

    pub(super) async fn focus_review_target(
        &self,
        root: PathBuf,
        target: crate::ui::review_window::ReviewTarget,
        prompt: String,
    ) {
        let Some(review) = self.reviews.borrow().get(&root).cloned() else {
            return;
        };
        let Some(located) = self
            .store
            .located_agent_presence(target.surface)
            .await
            .filter(|p| target.matches(&p.presence))
        else {
            review.delivery_message(
                "This agent ended or restarted. Select its current session and try again.",
            );
            self.refresh_review_targets().await;
            return;
        };
        if located
            .presence
            .pid
            .is_some_and(|pid| !flowmux_procmon::pid_alive(pid))
        {
            review.delivery_message(
                "This agent process has exited. Select another agent or use Copy review.",
            );
            self.refresh_review_targets().await;
            return;
        }
        // Terminal contents and input state are opaque across agent providers.
        // Clipboard + focus is the common handoff; never inject text or Enter
        // into a busy agent, approval prompt, existing draft, or returning shell.
        self.open_activity_target(located.workspace, located.pane, located.surface)
            .await;
        if self.pane_registry.borrow().active_surface(located.pane) != Some(located.surface) {
            review.delivery_message(
                "The agent tab is no longer available. Refresh agents and try again.",
            );
            return;
        }
        self.window.clipboard().set_text(&prompt);
        review
            .delivery_message("Review copied. Paste and submit in the selected agent when ready.");
        review.window.set_visible(false);
        self.clipboard_toast.show_with_message(
            "Review copied — paste into the agent when ready. Existing input is preserved.",
        );
    }
}

#[cfg(test)]
pub(super) async fn handoff_smoke(controller: &WindowController) {
    use crate::ui::review_window::{ReviewTarget, ReviewWindow};
    use flowmux_core::{AgentActivity, AgentPresence, AgentStatus};
    let workspace = controller.store.active_workspace().await.unwrap();
    let ws = controller.store.get_workspace(workspace).await.unwrap();
    let pane = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
    let surface = ws.surfaces[0].root_pane.active_surface_id(pane).unwrap();
    let terminal = controller
        .pane_registry
        .borrow()
        .terminals
        .get(&surface)
        .unwrap()
        .clone();
    terminal.write_input(b"REVIEW_UNSUBMITTED_DRAFT").unwrap();
    glib::timeout_future(Duration::from_millis(100)).await;
    let before = terminal.screen_text().unwrap();
    assert!(before.contains("REVIEW_UNSUBMITTED_DRAFT"));
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::fs::write(dir.path().join("review.txt"), "fixture\n").unwrap();
    let root = flowmux_vcs::review::repository_root(dir.path()).unwrap();
    let review = ReviewWindow::new(&controller.window, root.clone());
    review.connect_targets(controller.bridge.clone());
    controller
        .reviews
        .borrow_mut()
        .insert(root.clone(), review.clone());
    let prompt = "Code review feedback\n검토해주세요.\nSecond line";
    for name in [
        "claude",
        "codex",
        "gemini",
        "cline",
        "opencode",
        "agy",
        "custom-agent",
    ] {
        for status in [
            AgentStatus::Unknown,
            AgentStatus::Idle,
            AgentStatus::Working,
            AgentStatus::Blocked,
            AgentStatus::Done,
        ] {
            let mut presence =
                AgentPresence::new(name, status.to_activity(), Some(std::process::id()));
            presence.status = status;
            presence.session_id = Some(format!("review-{name}"));
            let target = ReviewTarget {
                surface,
                name: name.into(),
                pid: presence.pid,
                session: presence.session_id.clone(),
                label: name.into(),
            };
            controller
                .store
                .set_agent_activity(surface, Some(presence))
                .await;
            controller.focused_pane.set(None);
            controller
                .focus_review_target(root.clone(), target, prompt.into())
                .await;
            assert_eq!(controller.focused_pane.get(), Some(pane));
            assert_eq!(
                controller
                    .window
                    .clipboard()
                    .read_text_future()
                    .await
                    .unwrap()
                    .unwrap(),
                prompt
            );
            assert_eq!(
                terminal.screen_text().unwrap(),
                before,
                "handoff changed {name} terminal in {status:?}"
            );
        }
    }
    let stale = ReviewTarget {
        surface,
        name: "custom-agent".into(),
        pid: Some(std::process::id()),
        session: Some("old-session".into()),
        label: "stale".into(),
    };
    controller.window.clipboard().set_text("keep clipboard");
    controller.focused_pane.set(None);
    controller
        .focus_review_target(root.clone(), stale.clone(), "wrong session".into())
        .await;
    assert_eq!(controller.focused_pane.get(), None);
    assert_eq!(
        controller
            .window
            .clipboard()
            .read_text_future()
            .await
            .unwrap()
            .unwrap(),
        "keep clipboard"
    );
    controller.store.set_agent_activity(surface, None).await;
    controller.focused_pane.set(None);
    controller
        .focus_review_target(root.clone(), stale, "agent exited".into())
        .await;
    assert_eq!(controller.focused_pane.get(), None);
    let mut dead = AgentPresence::new("dead-agent", AgentActivity::Idle, Some(9_999_999));
    dead.session_id = Some("dead".into());
    controller
        .store
        .set_agent_activity(surface, Some(dead))
        .await;
    controller.focused_pane.set(None);
    controller
        .focus_review_target(
            root.clone(),
            ReviewTarget {
                surface,
                name: "dead-agent".into(),
                pid: Some(9_999_999),
                session: Some("dead".into()),
                label: "dead".into(),
            },
            "dead".into(),
        )
        .await;
    assert_eq!(controller.focused_pane.get(), None);
    assert_eq!(terminal.screen_text().unwrap(), before);
    controller.store.set_agent_activity(surface, None).await;
    review.smoke_set_draft("Keep unfinished review");
    controller.close_window().await;
    assert!(controller.window.is_visible());
    assert!(review.window.is_visible());
    assert!(review.status.text().contains("Save or clear"));
    review.smoke_set_draft("");
    glib::timeout_future(Duration::from_millis(50)).await;
    controller.reviews.borrow_mut().remove(&root);
    review.window.destroy();
    terminal.write_input(b"\x15").unwrap();
    controller.focus_pane(pane);
    println!("DIFF_REVIEW_PROVIDER_HANDOFF_OK");
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    #[gtk::test]
    async fn review_handoff_preserves_agent_input_and_checks_session() {
        let (controller, _, _) = super::super::tests::build_single_workspace_controller(
            "com.flowmux.App.UiTest.ReviewHandoff",
        )
        .await;
        controller.window.present();
        handoff_smoke(&controller).await;
        controller.window.destroy();
    }
}
