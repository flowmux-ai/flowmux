// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use vte::prelude::TerminalExt;

impl WindowController {
    pub(super) async fn open_diff_review(&self, pane: Option<PaneId>) {
        let Some(pane) = pane.or(self.focused_pane.get()) else {
            return;
        };
        let Some(host) = self.pane_registry.borrow().stack_for_pane(pane) else {
            return;
        };
        let previous = self.reviews.borrow().get(&pane).cloned();
        if let Some(review) = &previous {
            if host.visible_child().as_ref() == Some(review.root_widget.upcast_ref()) {
                self.focused_pane.set(Some(pane));
                review.hide();
                return;
            }
        }
        if self.pane_registry.borrow().is_ssh_pane(pane) {
            self.clipboard_toast.show_with_message("Code Review requires a local checkout. Open the remote checkout locally to review it.");
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
        if let Some(previous) = &previous {
            if previous.root != root {
                if previous.has_unsaved_review() {
                    previous.attach(&host);
                    previous.present();
                    previous.delivery_message(
                        "Save or cancel this comment before reviewing another checkout.",
                    );
                    return;
                }
                previous.detach();
                self.reviews.borrow_mut().remove(&pane);
            }
        }
        let review = self
            .reviews
            .borrow_mut()
            .entry(pane)
            .or_insert_with(|| {
                let review =
                    crate::ui::review_window::ReviewWindow::new(&self.window, &host, pane, root);
                review.connect_targets(self.bridge.clone());
                let focus = gtk::EventControllerFocus::new();
                let on_focus = self.callbacks.on_focus.clone();
                focus.connect_enter(move |_| (on_focus.borrow_mut())(pane));
                review.root_widget.add_controller(focus);
                review
            })
            .clone();
        review.attach(&host);
        review
            .workspace
            .set(self.pane_registry.borrow().workspace_of_pane(pane));
        self.focused_pane.set(Some(pane));
        review.present();
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
            targets.push((
                located.workspace,
                crate::ui::review_window::ReviewTarget {
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
                },
            ));
        }
        for review in self.reviews.borrow().values() {
            review.set_targets(
                targets
                    .iter()
                    .filter(|(workspace, _)| Some(*workspace) == review.workspace.get())
                    .map(|(_, target)| target.clone())
                    .collect(),
            );
        }
    }

    pub(super) async fn focus_review_target(
        &self,
        pane: PaneId,
        target: crate::ui::review_window::ReviewTarget,
        prompt: String,
    ) {
        let Some(review) = self.reviews.borrow().get(&pane).cloned() else {
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
        if !matches!(
            located.presence.status,
            flowmux_core::AgentStatus::Idle | flowmux_core::AgentStatus::Done
        ) {
            review.delivery_message(
                "This agent is busy or waiting for approval. Send again when it is ready.",
            );
            return;
        }
        let terminal = self
            .pane_registry
            .borrow()
            .terminals
            .get(&located.surface)
            .cloned();
        let Some(terminal) = terminal else {
            review.delivery_message("This agent terminal is no longer available.");
            return;
        };
        if review
            .workspace
            .get()
            .is_some_and(|workspace| workspace != located.workspace)
        {
            review.delivery_message("Choose an agent in this workspace.");
            return;
        }
        let (column, row) = terminal.widget.cursor_position();
        let (line, _) = terminal
            .widget
            .text_range_format(vte::Format::Text, row, 0, row + 1, 0);
        if !empty_agent_prompt(&target.name, line.as_deref().unwrap_or_default(), column) {
            review.present();
            review.delivery_message("The agent already has input, or its prompt is not ready. Clear or submit that input, then send again. Your feedback is saved.");
            return;
        }
        // Paste as one bracketed block; do not feed embedded newlines as keys.
        terminal.widget.paste_text(&prompt);
        terminal.write_input(b"\r").ok();
        review.hide();
        if let Some(target_review) = self.reviews.borrow().get(&located.pane) {
            target_review.hide();
        }
        self.open_activity_target(located.workspace, located.pane, located.surface)
            .await;
        self.clipboard_toast
            .show_with_message("Feedback sent to agent");
    }
}

fn empty_agent_prompt(agent: &str, line: &str, column: i64) -> bool {
    matches!(line.trim(), "›" | "❯" | ">")
        || (agent.eq_ignore_ascii_case("codex")
            && column == 2
            && line.trim() == "› Ask Codex to do anything")
}

#[cfg(test)]
pub(super) async fn handoff_smoke(controller: &WindowController) {
    use crate::ui::review_window::{ReviewTarget, ReviewWindow};
    use flowmux_core::{AgentActivity, AgentPresence, AgentStatus};
    assert!(empty_agent_prompt("codex", "› Ask Codex to do anything", 2));
    assert!(!empty_agent_prompt(
        "codex",
        "› Ask Codex to do anything",
        25
    ));
    assert!(!empty_agent_prompt("codex", "› existing draft", 2));
    assert!(!empty_agent_prompt(
        "other",
        "› Ask Codex to do anything",
        2
    ));
    fn receiver_ready(terminal: &crate::ui::pane_terminal::PaneTerminal, phase: &str) -> bool {
        let (column, row) = terminal.widget.cursor_position();
        let (line, _) = terminal
            .widget
            .text_range_format(vte::Format::Text, row, 0, row + 1, 0);
        let (marker, _) = terminal
            .widget
            .text_range_format(vte::Format::Text, row - 1, 0, row, 0);
        marker.as_deref().unwrap_or_default().trim() == format!("REVIEW_READY:{phase}")
            && empty_agent_prompt("codex", line.as_deref().unwrap_or_default(), column)
    }

    async fn start_receiver(
        terminal: &crate::ui::pane_terminal::PaneTerminal,
        receiver: &std::path::Path,
        receipt: &std::path::Path,
        phase: &str,
        monitor: bool,
    ) {
        // Each launch prints a different marker next to its actual raw-mode
        // prompt. A previous receiver's prompt cannot acknowledge this launch.
        terminal
            .write_input(
                format!(
                    "python3 '{}' '{}' {phase}{}; printf '\\nREVIEW_DONE:%s\\n' {phase}\r",
                    receiver.display(),
                    receipt.display(),
                    if monitor { " monitor" } else { "" },
                )
                .as_bytes(),
            )
            .unwrap();
        glib::future_with_timeout(Duration::from_secs(10), async {
            loop {
                if receiver_ready(terminal, phase) {
                    break;
                }
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "receiver {phase} not ready: {error:?}; screen={:?}",
                terminal.screen_text()
            )
        });
    }

    async fn wait_receiver_exit(terminal: &crate::ui::pane_terminal::PaneTerminal, phase: &str) {
        // The shell emits this only after Python exits and restores termios.
        glib::future_with_timeout(Duration::from_secs(10), async {
            while !terminal.screen_text().is_some_and(|text| {
                text.lines()
                    .any(|line| line.trim() == format!("REVIEW_DONE:{phase}"))
            }) {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "receiver {phase} did not exit: {error:?}; screen={:?}",
                terminal.screen_text()
            )
        });
    }

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
    glib::future_with_timeout(Duration::from_secs(15), async {
        while terminal
            .screen_text()
            .is_none_or(|text| text.trim().is_empty())
        {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    terminal.write_input(b"REVIEW_UNSUBMITTED_DRAFT").unwrap();
    glib::future_with_timeout(Duration::from_secs(10), async {
        while !terminal
            .screen_text()
            .is_some_and(|text| text.contains("REVIEW_UNSUBMITTED_DRAFT"))
        {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let before = terminal.screen_text().unwrap();
    assert!(before.contains("REVIEW_UNSUBMITTED_DRAFT"));
    let dir = tempfile::tempdir().unwrap();
    std::process::Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    std::fs::write(dir.path().join("review.txt"), "fixture\n").unwrap();
    std::fs::write(
        dir.path().join(".git/info/exclude"),
        "receiver.py\nreceipt.bin*\nsource-receipt.bin*\n",
    )
    .unwrap();
    let root = flowmux_vcs::review::repository_root(dir.path()).unwrap();
    let review = ReviewWindow::new(
        &controller.window,
        &controller
            .pane_registry
            .borrow()
            .stack_for_pane(pane)
            .unwrap(),
        pane,
        root.clone(),
    );
    review.connect_targets(controller.bridge.clone());
    controller.reviews.borrow_mut().insert(pane, review.clone());
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
                .focus_review_target(pane, target, prompt.into())
                .await;
            assert!(
                review.status.text().contains("input") || review.status.text().contains("busy")
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
        .focus_review_target(pane, stale.clone(), "wrong session".into())
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
        .focus_review_target(pane, stale, "agent exited".into())
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
            pane,
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
    // A real child PTY records a bracketed multiline paste and its submit key.
    terminal.write_input(b"\x15").unwrap();
    let receiver = dir.path().join("receiver.py");
    #[cfg(target_os = "macos")]
    let receipt = dir.path().join("receipt.bin");
    let source_receipt = dir.path().join("source-receipt.bin");
    std::fs::write(
        &receiver,
        r#"import os, sys, termios, tty
previous = termios.tcgetattr(0)
tty.setraw(0)
try:
    os.write(1, ('\x1b[?2004h\r\nREVIEW_READY:' + sys.argv[2] + '\r\n› ').encode())
    data = b''
    while True:
        chunk = os.read(0, 65536)
        if not chunk or chunk == b'\x04':
            break
        data += chunk
        # Publish complete bytes atomically; existence must not race the write.
        with open(sys.argv[1] + '.tmp', 'wb') as output:
            output.write(data)
        os.replace(sys.argv[1] + '.tmp', sys.argv[1])
        if len(sys.argv) == 3 and data.endswith(b'\x1b[201~\r'):
            break
finally:
    os.write(1, b'\x1b[?2004l\r\nREVIEW_RECEIVER_EXITED\r\n')
    termios.tcsetattr(0, termios.TCSANOW, previous)
"#,
    )
    .unwrap();
    // Keep the source alive across split/resizing. Its byte receipt detects a
    // misrouted send without mistaking shell prompt reflow for terminal input.
    start_receiver(&terminal, &receiver, &source_receipt, "source", true).await;
    let mut ready = AgentPresence::new(
        "fixture-agent",
        AgentActivity::Idle,
        Some(std::process::id()),
    );
    ready.status = AgentStatus::Idle;
    ready.session_id = Some("receiver".into());
    let target = ReviewTarget {
        surface,
        name: ready.name.clone(),
        pid: ready.pid,
        session: ready.session_id.clone(),
        label: "Fixture".into(),
    };
    controller
        .store
        .set_agent_activity(surface, Some(ready))
        .await;
    controller
        .focus_review_target(pane, target, prompt.into())
        .await;
    glib::future_with_timeout(Duration::from_secs(10), async {
        while !std::fs::read(&source_receipt).is_ok_and(|bytes| bytes.ends_with(b"\x1b[201~\r")) {
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let received = std::fs::read(&source_receipt).unwrap();
    assert_eq!(
        received,
        format!("\x1b[200~{}\x1b[201~\r", prompt.replace('\n', "\r")).as_bytes()
    );
    println!("DIFF_REVIEW_DIRECT_PTY_RECEIPT_OK");
    #[cfg(target_os = "macos")]
    {
        // Follow the actual menu -> save -> bridge -> second pane -> PTY path.
        let destination = controller
            .split_pane(pane, flowmux_core::SplitDirection::Vertical)
            .await
            .unwrap();
        let (destination_surface, destination_terminal, destination_stack) = {
            let registry = controller.pane_registry.borrow();
            (
                registry.active_surface(destination).unwrap(),
                registry.active_terminal(destination).unwrap().clone(),
                registry.stack_for_pane(destination).unwrap(),
            )
        };
        start_receiver(
            &destination_terminal,
            &receiver,
            &receipt,
            "worktree",
            false,
        )
        .await;
        let target_review = ReviewWindow::new(
            &controller.window,
            &destination_stack,
            destination,
            root.clone(),
        );
        target_review.workspace.set(Some(workspace));
        target_review.connect_targets(controller.bridge.clone());
        controller
            .reviews
            .borrow_mut()
            .insert(destination, target_review.clone());
        target_review.present();
        target_review.smoke_wait_for_files(&["review.txt"]).await;
        review.workspace.set(Some(workspace));
        review.present();
        review.smoke_wait_for_files(&["review.txt"]).await;
        review
            .smoke_prepare_unchanged_edit("Send unchanged Edit to the chosen Codex pane")
            .await;
        // Keep outgoing reviews mapped during the handoff so focus routing
        // must follow the selected stack page, not the transition's widgets.
        for pane in [pane, destination] {
            controller
                .pane_registry
                .borrow()
                .stack_for_pane(pane)
                .unwrap()
                .set_transition_duration(1000);
        }
        // Publish fixture identities after Git/storage awaits so background
        // polling cannot age them out while the views are being prepared.
        for (surface, session) in [
            (surface, "source-codex"),
            (destination_surface, "destination-codex"),
        ] {
            let mut agent =
                AgentPresence::new("codex", AgentActivity::Idle, Some(std::process::id()));
            agent.status = AgentStatus::Idle;
            agent.session_id = Some(session.into());
            controller
                .store
                .set_agent_activity(surface, Some(agent))
                .await;
        }
        controller.refresh_review_targets().await;
        let source_before = std::fs::read(&source_receipt).unwrap();
        review.smoke_activate_target(destination_surface);
        glib::future_with_timeout(Duration::from_secs(10), async {
            while !std::fs::read(&receipt).is_ok_and(|bytes| bytes.ends_with(b"\x1b[201~\r"))
                || controller.focused_pane.get() != Some(destination)
                || review.root_widget.is_mapped()
                || target_review.root_widget.is_mapped()
            {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|error| panic!("review handoff timed out: {error:?}; receipt={}, focused={:?}, expected={destination:?}, source_mapped={}, target_mapped={}, status={}", receipt.exists(), controller.focused_pane.get(), review.root_widget.is_mapped(), target_review.root_widget.is_mapped(), review.status.text()));
        let received = std::fs::read(&receipt).unwrap();
        assert!(received.starts_with(b"\x1b[200~Code review feedback"));
        assert!(received.ends_with(b"\x1b[201~\r"));
        assert!(String::from_utf8_lossy(&received)
            .contains("Send unchanged Edit to the chosen Codex pane"));
        assert!(String::from_utf8_lossy(&received).contains("Review target: Unstaged + Staged"));
        assert!(!review.root_widget.is_mapped());
        assert!(!target_review.root_widget.is_mapped());
        assert_eq!(std::fs::read(&source_receipt).unwrap(), source_before);
        target_review.present();
        glib::future_with_timeout(Duration::from_secs(5), async {
            while destination_stack.is_transition_running() {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        target_review.hide();
        controller.focus_pane(destination);
        glib::future_with_timeout(Duration::from_secs(5), async {
            while !gtk::prelude::GtkWindowExt::focus(&controller.window).is_some_and(|focus| {
                focus == destination_terminal.widget.clone().upcast::<gtk::Widget>()
            }) {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("focus must reach the terminal while the outgoing review is still mapped");
        println!("DIFF_REVIEW_TRANSITION_TERMINAL_FOCUS_OK");
        // The same real Send -> bridge -> PTY path carries a historical review
        // with its immutable commit ID and the committed-change template.
        for args in [
            vec!["add", "review.txt"],
            vec![
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "test: create committed review fixture",
            ],
        ] {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .env("GIT_AUTHOR_NAME", "JunsuChoi")
                .env("GIT_AUTHOR_EMAIL", "jsuya.choi@samsung.com")
                .env("GIT_COMMITTER_NAME", "JunsuChoi")
                .env("GIT_COMMITTER_EMAIL", "jsuya.choi@samsung.com")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        review.present();
        let oid = review.smoke_select_latest_commit().await;
        review
            .smoke_prepare_unchanged_edit("Send this historical commit review")
            .await;
        wait_receiver_exit(&destination_terminal, "worktree").await;
        // Model delayed terminal repaint: the prior prompt is still visible
        // while a new receiver has not acknowledged startup.
        destination_terminal
            .widget
            .feed(b"\r\nREVIEW_READY:worktree\r\n\xe2\x80\xba ");
        glib::future_with_timeout(Duration::from_secs(10), async {
            while !receiver_ready(&destination_terminal, "worktree") {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("stale receiver prompt must be rendered");
        assert!(
            !receiver_ready(&destination_terminal, "commit"),
            "a prior receiver's empty prompt must not acknowledge the next launch"
        );
        println!("DIFF_REVIEW_STALE_RECEIVER_PROMPT_REJECTED_OK");
        let receipt = dir.path().join("receipt.bin.commit");
        start_receiver(&destination_terminal, &receiver, &receipt, "commit", false).await;
        let mut agent = AgentPresence::new("codex", AgentActivity::Idle, Some(std::process::id()));
        agent.status = AgentStatus::Idle;
        agent.session_id = Some("destination-commit-codex".into());
        controller
            .store
            .set_agent_activity(destination_surface, Some(agent))
            .await;
        controller.refresh_review_targets().await;
        review.smoke_activate_target(destination_surface);
        glib::future_with_timeout(Duration::from_secs(10), async {
            while !std::fs::read(&receipt).is_ok_and(|bytes| bytes.ends_with(b"\x1b[201~\r")) {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|error| panic!("commit review handoff timed out: {error:?}; receipt={:?}, focused={:?}, expected={destination:?}, status={}, screen={:?}", std::fs::read(&receipt), controller.focused_pane.get(), review.status.text(), destination_terminal.screen_text()));
        let received = std::fs::read_to_string(&receipt).unwrap();
        assert!(
            received.starts_with("\x1b[200~Code review feedback")
                && received.ends_with("\x1b[201~\r")
        );
        assert!(received.contains(&format!("Review target: commit {oid}")));
        assert!(received.contains("Send this historical commit review"));
        assert!(!received.contains("Send unchanged Edit to the chosen Codex pane"));
        assert_eq!(std::fs::read(&source_receipt).unwrap(), source_before);
        wait_receiver_exit(&destination_terminal, "commit").await;
        println!("DIFF_REVIEW_COMMIT_TEMPLATE_PTY_HANDOFF_OK");
        controller
            .store
            .set_agent_activity(destination_surface, None)
            .await;
        controller.store.close_pane(destination).await.unwrap();
        controller
            .apply_close_pane_incremental_or_rerender(workspace, destination)
            .await;
        println!("DIFF_REVIEW_MENU_MULTI_CODEX_PTY_HANDOFF_OK");
    }

    terminal.write_input(b"\x04").unwrap();
    wait_receiver_exit(&terminal, "source").await;
    assert_eq!(
        std::fs::read(&source_receipt).unwrap(),
        format!("\x1b[200~{}\x1b[201~\r", prompt.replace('\n', "\r")).as_bytes(),
        "only the initial direct send may reach the source PTY"
    );
    controller.store.set_agent_activity(surface, None).await;
    review.smoke_set_draft("Keep unfinished review");
    controller.close_window().await;
    assert!(controller.window.is_visible());
    assert!(review.root_widget.is_mapped());
    assert!(review.status.text().contains("Save or clear"));
    review.smoke_set_draft("");
    glib::timeout_future(Duration::from_millis(50)).await;
    controller.reviews.borrow_mut().remove(&pane);
    review.detach();
    terminal.write_input(b"\x15").unwrap();
    controller.focus_pane(pane);
    println!("DIFF_REVIEW_PROVIDER_HANDOFF_OK");
}

#[cfg(test)]
pub(super) async fn pane_scope_smoke(controller: &WindowController) {
    async fn change_directory(
        terminal: &crate::ui::pane_terminal::PaneTerminal,
        path: &std::path::Path,
    ) {
        glib::future_with_timeout(Duration::from_secs(10), async {
            while terminal
                .screen_text()
                .is_none_or(|text| text.trim().is_empty())
            {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        terminal
            .write_input(
                format!(
                    "cd '{}' && printf '\\033]7;file://%s\\007' \"$PWD\"\r",
                    path.display()
                )
                .as_bytes(),
            )
            .unwrap();
        glib::future_with_timeout(Duration::from_secs(10), async {
            while terminal
                .current_dir()
                .and_then(|dir| std::fs::canonicalize(dir).ok())
                != std::fs::canonicalize(path).ok()
            {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|error| {
            panic!(
                "cwd not updated: {error:?}; expected {path:?}; actual {:?}; screen {:?}",
                terminal.current_dir(),
                terminal.screen_text()
            )
        });
    }
    let workspace = controller.store.active_workspace().await.unwrap();
    let ws = controller.store.get_workspace(workspace).await.unwrap();
    let left = ws.surfaces[0].root_pane.first_leaf_id().unwrap();
    let right = controller
        .split_pane(left, flowmux_core::SplitDirection::Vertical)
        .await
        .unwrap();
    let left_dir = tempfile::tempdir().unwrap();
    let right_dir = tempfile::tempdir().unwrap();
    for (pane, dir, file) in [
        (left, &left_dir, "left-only.txt"),
        (right, &right_dir, "right-only.txt"),
    ] {
        assert!(std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .status
            .success());
        std::fs::write(dir.path().join(file), "pane-local change\n").unwrap();
        let terminal = controller
            .pane_registry
            .borrow()
            .active_terminal(pane)
            .unwrap()
            .clone();
        change_directory(&terminal, dir.path()).await;
    }
    glib::timeout_future(Duration::from_millis(50)).await;
    let (left_stack, right_stack) = {
        let registry = controller.pane_registry.borrow();
        (
            registry.stack_for_pane(left).unwrap(),
            registry.stack_for_pane(right).unwrap(),
        )
    };
    let workspace_view = controller.stack.visible_child();
    let left_terminal = left_stack.visible_child();
    let right_terminal = right_stack.visible_child();
    controller.focused_pane.set(Some(left));
    controller.open_diff_review(Some(right)).await;
    let right_review = controller.reviews.borrow()[&right].clone();
    assert_eq!(
        right_review.root,
        std::fs::canonicalize(right_dir.path()).unwrap()
    );
    assert_eq!(controller.stack.visible_child(), workspace_view);
    assert_eq!(left_stack.visible_child(), left_terminal);
    assert_eq!(
        right_stack.visible_child().as_ref(),
        Some(right_review.root_widget.upcast_ref())
    );
    right_review.smoke_wait_for_files(&["right-only.txt"]).await;
    controller.open_diff_review(Some(left)).await;
    let left_review = controller.reviews.borrow()[&left].clone();
    left_review.smoke_wait_for_files(&["left-only.txt"]).await;
    assert_eq!(
        right_stack.visible_child().as_ref(),
        Some(right_review.root_widget.upcast_ref())
    );
    assert!(!Rc::ptr_eq(&left_review, &right_review));
    let right_pid = controller
        .pane_registry
        .borrow()
        .active_terminal(right)
        .unwrap()
        .pid
        .get();
    right_review.smoke_set_draft("Keep this comment while toggling Code Review");
    controller.focused_pane.set(Some(right));
    controller.open_diff_review(None).await;
    assert_eq!(
        right_stack.visible_child(),
        right_terminal,
        "repeated action must return to the previous tab"
    );
    assert!(right_review.has_unsaved_review());
    assert_eq!(
        left_stack.visible_child().as_ref(),
        Some(left_review.root_widget.upcast_ref())
    );
    controller.open_diff_review(None).await;
    assert_eq!(
        right_stack.visible_child().as_ref(),
        Some(right_review.root_widget.upcast_ref())
    );
    assert!(right_review.has_unsaved_review());
    assert!(Rc::ptr_eq(
        &controller.reviews.borrow()[&right],
        &right_review
    ));
    assert_eq!(
        controller
            .pane_registry
            .borrow()
            .active_terminal(right)
            .unwrap()
            .pid
            .get(),
        right_pid
    );
    right_review.smoke_set_draft("");
    controller.open_diff_review(Some(right)).await;
    assert_eq!(right_stack.visible_child(), right_terminal);
    println!("CODE_REVIEW_TOGGLE_PRESERVES_DRAFT_AND_TERMINAL_OK");
    assert_eq!(
        left_stack.visible_child().as_ref(),
        Some(left_review.root_widget.upcast_ref())
    );
    // A changed cwd rebinds only the requested pane, even for the same repository.
    let terminal = controller
        .pane_registry
        .borrow()
        .active_terminal(right)
        .unwrap()
        .clone();
    right_review.smoke_set_draft("Keep the unfinished comment in its original checkout");
    change_directory(&terminal, left_dir.path()).await;
    controller.open_diff_review(Some(right)).await;
    assert!(Rc::ptr_eq(
        &controller.reviews.borrow()[&right],
        &right_review
    ));
    assert!(right_review.status.text().contains("Save or cancel"));
    right_review.smoke_set_draft("");
    right_review.hide();
    controller.open_diff_review(Some(right)).await;
    let rebound = controller.reviews.borrow()[&right].clone();
    assert_eq!(rebound.root, left_review.root);
    assert!(!Rc::ptr_eq(&rebound, &left_review));
    rebound.smoke_wait_for_files(&["left-only.txt"]).await;
    assert!(right_review.root_widget.parent().is_none());
    rebound.smoke_set_draft("Do not lose this when closing its pane");
    let surface = controller
        .pane_registry
        .borrow()
        .active_surface(right)
        .unwrap();
    assert!(!controller.confirm_dirty_surfaces(&[surface]).await);
    assert!(rebound.root_widget.is_mapped());
    rebound.smoke_set_draft("");
    controller.store.close_pane(right).await.unwrap();
    controller
        .apply_close_pane_incremental_or_rerender(workspace, right)
        .await;
    assert!(!controller.reviews.borrow().contains_key(&right));
    assert!(rebound.root_widget.parent().is_none());
    assert!(controller.reviews.borrow().contains_key(&left));
    left_review.smoke_set_draft("Keep this review visible across layout reconstruction");
    controller.rerender_workspace(&controller.store.get_workspace(workspace).await.unwrap());
    let left_stack = controller
        .pane_registry
        .borrow()
        .stack_for_pane(left)
        .unwrap();
    assert_eq!(
        left_stack.visible_child().as_ref(),
        Some(left_review.root_widget.upcast_ref()),
        "layout reconstruction must reattach the visible review"
    );
    assert!(left_review.has_unsaved_review());
    left_review.smoke_set_draft("");
    println!("CODE_REVIEW_LAYOUT_REATTACH_OK");
    controller
        .reviews
        .borrow_mut()
        .remove(&left)
        .unwrap()
        .detach();
    assert_eq!(left_stack.visible_child(), left_terminal);
    controller.focus_pane(left);
    println!("DIFF_REVIEW_PANE_ROOT_ISOLATION_OK");
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
        pane_scope_smoke(&controller).await;
        controller.window.destroy();
    }
}
