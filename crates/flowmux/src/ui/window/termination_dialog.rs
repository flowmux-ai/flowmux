// SPDX-License-Identifier: GPL-3.0-or-later
//! A queued, tab-specific explanation for unexpected terminal process deaths.
use super::*;
#[cfg(target_os = "linux")]
use crate::termination::OomEvidence;
use crate::termination::{self, ChildTermination, TerminationContext, TerminationReport, WaitKind};
use crate::ui::pane_terminal::ChildExit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TerminationChoice {
    Close,
    Keep,
}

pub(super) fn build_termination_dialog(
    report: &TerminationReport,
    last: Option<&str>,
) -> (adw::AlertDialog, oneshot::Receiver<TerminationChoice>) {
    let dialog = adw::AlertDialog::new(
        Some(&termination::dialog_heading(&report.termination)),
        Some(&termination::dialog_body(report, last)),
    );
    dialog.add_response("keep", "Keep tab");
    dialog.add_response(
        "close",
        if last.is_some() {
            "Close workspace"
        } else {
            "Close tab"
        },
    );
    dialog.set_default_response(Some("close"));
    dialog.set_close_response("close");
    dialog.set_response_appearance("close", adw::ResponseAppearance::Suggested);
    let (tx, rx) = oneshot::channel();
    let tx = Cell::new(Some(tx));
    dialog.connect_response(None, move |_, response| {
        if let Some(tx) = tx.take() {
            let _ = tx.send(if response == "close" {
                TerminationChoice::Close
            } else {
                TerminationChoice::Keep
            });
        }
    });
    (dialog, rx)
}

impl WindowController {
    pub(super) fn handle_child_exit(
        &self,
        exit: ChildExit,
        scope_unit: Option<String>,
        cgroup_dir: Option<PathBuf>,
        isolation_unavailable: Option<String>,
    ) {
        let controller = self.clone();
        glib::spawn_future_local(async move {
            #[cfg(target_os = "linux")]
            let oom = {
                let iso = controller.scope_isolation.borrow().clone();
                let unit = scope_unit.clone();
                match (controller.tokio_handle.clone(), iso, unit) {
                    (Some(handle), Some(iso), Some(unit)) => handle
                        .spawn(async move {
                            let result = tokio::time::timeout(
                                Duration::from_millis(500),
                                crate::process_scope::scope_result(&iso, &unit),
                            )
                            .await;
                            let evidence = if matches!(result, Ok(Ok(ref r)) if r == "oom-kill") {
                                Some(OomEvidence::SystemdResult)
                            } else if cgroup_dir
                                .as_deref()
                                .and_then(crate::process_scope::cgroup_oom_kills)
                                .unwrap_or(0)
                                > 0
                            {
                                Some(OomEvidence::CgroupEvents)
                            } else {
                                None
                            };
                            let _ = tokio::time::timeout(
                                Duration::from_millis(100),
                                crate::process_scope::reset_failed(&iso, &unit),
                            )
                            .await;
                            evidence
                        })
                        .await
                        .unwrap_or(None),
                    _ => None,
                }
            };
            #[cfg(not(target_os = "linux"))]
            let oom = {
                let _ = cgroup_dir;
                None
            };
            if exit.closing || controller.window_close.approved.get() {
                return;
            }
            #[cfg(target_os = "macos")]
            let pressure = termination::memory_pressure_level();
            #[cfg(not(target_os = "macos"))]
            let pressure = None;
            let termination =
                termination::classify(WaitKind::from_status(exit.status), oom, pressure);
            if matches!(termination, ChildTermination::Exited { .. }) {
                tracing::info!(pane = %exit.pane, status = exit.status, "child exited");
                return;
            }
            // A tab can move or close while evidence is collected.
            let pane = controller
                .pane_registry
                .borrow()
                .pane_for_surface(exit.surface);
            let Some(pane) = pane else {
                return;
            };
            let workspace = controller.pane_registry.borrow().workspace_of_pane(pane);
            let Some(workspace) = workspace else {
                return;
            };
            let Some(ws) = controller.store.get_workspace(workspace).await else {
                return;
            };
            let Some(tab_name) = controller.store.surface_title(pane, exit.surface).await else {
                return;
            };
            let context = TerminationContext {
                workspace_name: ws.display_title().to_string(),
                pane_label: pane.to_string()[..8].to_string(),
                tab_name,
                command: exit.command,
                wrapper_pid: exit.wrapper_pid,
                time: glib::DateTime::now_local()
                    .and_then(|d| d.format("%H:%M:%S"))
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
                scope_unit,
                isolation_unavailable,
            };
            controller.enqueue_termination_report(TerminationReport {
                pane,
                surface: exit.surface,
                termination,
                context,
            });
        });
    }

    pub(super) fn enqueue_termination_report(&self, report: TerminationReport) {
        self.termination_reports.borrow_mut().push_back(report);
        // Claim the presenter before the first await, so simultaneous exits cannot race.
        if self.termination_presenting.replace(true) {
            return;
        }
        let controller = self.clone();
        glib::spawn_future_local(async move {
            loop {
                let report = controller.termination_reports.borrow_mut().pop_front();
                let Some(mut report) = report else {
                    break;
                };
                if controller.window_close.approved.get() {
                    break;
                }
                let pane = controller
                    .pane_registry
                    .borrow()
                    .pane_for_surface(report.surface);
                let Some(pane) = pane else {
                    continue;
                };
                report.pane = pane;
                let last = if controller.store.tab_count_in_pane(pane).await == Some(1) {
                    if let Some((workspace, 1)) =
                        controller.store.workspace_pane_count_for(pane).await
                    {
                        controller
                            .store
                            .get_workspace(workspace)
                            .await
                            .map(|ws| ws.display_title().to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };
                let (dialog, rx) = build_termination_dialog(&report, last.as_deref());
                *controller.termination_dialog.borrow_mut() = Some(dialog.clone());
                let choice = {
                    let _native_suspend =
                        crate::ui::browser_pane::suspend_native_browser_views_for_window(
                            controller.window.upcast_ref(),
                        );
                    dialog.present(Some(&controller.window));
                    let choice = rx.await;
                    dialog.close();
                    choice
                };
                controller.termination_dialog.borrow_mut().take();
                if matches!(choice, Ok(TerminationChoice::Close)) {
                    let (ack, rx) = oneshot::channel();
                    if controller
                        .bridge
                        .tx
                        .send(GtkCommand::CloseTerminatedSurface {
                            pane,
                            surface: report.surface,
                            ack,
                        })
                        .await
                        .is_ok()
                    {
                        let _ = rx.await;
                    }
                }
            }
            controller.termination_presenting.set(false);
        });
    }

    #[cfg(any(test, feature = "native-smoke"))]
    pub(super) fn current_termination_dialog(&self) -> Option<adw::AlertDialog> {
        self.termination_dialog.borrow().clone()
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;
    use crate::termination::Cause;

    async fn setup(name: &str) -> (WindowController, WorkspaceId, PaneId) {
        let (mut c, ws, pane) = super::super::tests::build_single_workspace_controller(name).await;
        let (bridge, rx) = Bridge::new();
        c.bridge = bridge;
        c.install_process_callbacks();
        spawn_dispatch_loop(rx, c.clone());
        c.window.present();
        (c, ws, pane)
    }

    async fn add_tab(c: &WindowController, ws: WorkspaceId, pane: PaneId) -> SurfaceId {
        let (_, surface) = c
            .store
            .add_terminal_surface_to_pane(pane, None)
            .await
            .unwrap();
        c.attach_or_rerender_surface(ws, pane, surface, None)
            .await
            .unwrap();
        surface
    }

    async fn signal(
        c: &WindowController,
        pane: PaneId,
        surface: SurfaceId,
        status: i32,
        closing: bool,
    ) {
        c.pane_registry.borrow().terminals[&surface].pid.set(None);
        c.dispatch(GtkCommand::ChildExited {
            exit: ChildExit {
                pane,
                surface,
                status,
                closing,
                wrapper_pid: Some(1),
                command: "sh".into(),
            },
            scope_unit: None,
            cgroup_dir: None,
            isolation_unavailable: None,
        })
        .await;
    }

    async fn dialog(c: &WindowController) -> adw::AlertDialog {
        glib::future_with_timeout(Duration::from_secs(5), async {
            loop {
                if let Some(dialog) = c.current_termination_dialog() {
                    // Native input cannot click a response before the opening frame.
                    glib::timeout_future(Duration::from_millis(300)).await;
                    return dialog;
                }
                glib::timeout_future(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("termination dialog never appeared")
    }

    fn respond(dialog: &adw::AlertDialog, response: &str) {
        let label = dialog.response_label(response);
        let mut pending = vec![dialog.clone().upcast::<gtk::Widget>()];
        while let Some(widget) = pending.pop() {
            if let Some(button) = widget.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some(label.as_str()) {
                    button.emit_clicked();
                    return;
                }
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                child = widget.next_sibling();
                pending.push(widget);
            }
        }
        panic!("response button not found: {label}");
    }

    async fn settled(c: &WindowController) {
        glib::future_with_timeout(Duration::from_secs(5), async {
            while c.termination_presenting.get() {
                glib::timeout_future(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("termination presenter did not finish");
    }

    #[gtk::test]
    async fn dialog_responses_and_last_workspace_copy() {
        adw::init().unwrap();
        let report = TerminationReport {
            pane: PaneId::new(),
            surface: SurfaceId::new(),
            termination: ChildTermination::Signaled {
                signal: libc::SIGKILL,
                cause: Cause::Signal,
            },
            context: TerminationContext {
                workspace_name: "ws".into(),
                pane_label: "p".into(),
                tab_name: "sh".into(),
                command: "sh".into(),
                wrapper_pid: None,
                time: "now".into(),
                scope_unit: None,
                isolation_unavailable: None,
            },
        };
        for (response, expected) in [
            ("close", TerminationChoice::Close),
            ("keep", TerminationChoice::Keep),
        ] {
            let (dialog, rx) = build_termination_dialog(&report, None);
            assert_eq!(dialog.response_label("close"), "Close tab");
            assert_eq!(dialog.response_label("keep"), "Keep tab");
            assert_eq!(dialog.default_response().as_deref(), Some("close"));
            assert_eq!(dialog.close_response(), "close");
            dialog.emit_by_name::<()>("response", &[&response]);
            assert_eq!(rx.await.unwrap(), expected);
        }
        let (dialog, _) = build_termination_dialog(&report, Some("ws"));
        assert_eq!(dialog.response_label("close"), "Close workspace");
        assert!(dialog.body().contains("last tab in workspace ws"));
    }

    #[gtk::test]
    async fn close_only_dead_tab_preserves_sibling_and_keep_retains_scrollback() {
        let (c, ws, pane) = setup("com.flowmux.App.UiTest.TerminationClose").await;
        let original = c.pane_registry.borrow().active_surface(pane).unwrap();
        let widget = c.pane_registry.borrow().terminals[&original].widget.clone();
        let dead = add_tab(&c, ws, pane).await;
        signal(&c, pane, dead, libc::SIGKILL, false).await;
        let d = dialog(&c).await;
        assert_eq!(
            d.heading().as_deref(),
            Some("This tab's process was terminated by SIGKILL")
        );
        respond(&d, "keep");
        settled(&c).await;
        assert_eq!(c.store.tab_count_in_pane(pane).await, Some(2));
        signal(&c, pane, dead, libc::SIGKILL, false).await;
        respond(&dialog(&c).await, "close");
        settled(&c).await;
        assert_eq!(c.store.tab_count_in_pane(pane).await, Some(1));
        assert_eq!(c.pane_registry.borrow().terminals[&original].widget, widget);
        assert!(c.store.surface_title(pane, dead).await.is_none());
        c.window.destroy();
    }

    #[gtk::test]
    async fn closing_dead_tab_preserves_browser_and_unsaved_editor() {
        use webkit6::prelude::*;
        let (c, ws, pane) = setup("com.flowmux.App.UiTest.TerminationMixed").await;
        let dead = c.pane_registry.borrow().active_surface(pane).unwrap();
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("unsaved.txt");
        std::fs::write(&file, "original").unwrap();
        let (_, browser_id) = c
            .store
            .add_browser_surface_to_pane(pane, "about:blank".into())
            .await
            .unwrap();
        c.attach_or_rerender_surface(ws, pane, browser_id, None)
            .await
            .unwrap();
        let browser = c.pane_registry.borrow().browsers[&browser_id].root.clone();
        let (_, editor_id) = c
            .store
            .add_editor_surface_to_pane(pane, root.path().into())
            .await
            .unwrap();
        c.attach_or_rerender_surface(ws, pane, editor_id, None)
            .await
            .unwrap();
        let editor = c.pane_registry.borrow().editors[&editor_id].clone();
        editor.open_file(&file).await.unwrap();
        editor.flush_pending_changes().await.unwrap();
        editor.grab_focus();
        let view = editor
            .focus_widget()
            .downcast::<webkit6::WebView>()
            .unwrap();
        view.execute_editing_command_with_argument("InsertText", "unsaved");
        glib::future_with_timeout(Duration::from_secs(5), async {
            while editor.dirty_document_paths().is_empty() {
                glib::timeout_future(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("editor edit must be unsaved before the terminal dies");
        signal(&c, pane, dead, libc::SIGKILL, false).await;
        respond(&dialog(&c).await, "close");
        settled(&c).await;
        assert_eq!(c.store.tab_count_in_pane(pane).await, Some(2));
        assert_eq!(c.pane_registry.borrow().browsers[&browser_id].root, browser);
        assert_eq!(
            c.pane_registry.borrow().editors[&editor_id].root,
            editor.root
        );
        assert_eq!(editor.dirty_document_paths(), vec![file.clone()]);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
        editor.flush_pending_changes().await.unwrap();
        editor.save_all_dirty().await.unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("unsaved"));
        c.window.destroy();
    }

    #[gtk::test]
    async fn normal_exit_and_user_close_are_silent_but_external_hup_reports() {
        let (c, _, pane) = setup("com.flowmux.App.UiTest.TerminationSilence").await;
        let surface = c.pane_registry.borrow().active_surface(pane).unwrap();
        for (status, closing) in [
            (0, false),
            (130 << 8, false),
            (137 << 8, false),
            (libc::SIGHUP, true),
        ] {
            signal(&c, pane, surface, status, closing).await;
            glib::timeout_future(Duration::from_millis(50)).await;
            assert!(c.current_termination_dialog().is_none());
        }
        signal(&c, pane, surface, libc::SIGHUP, false).await;
        let d = dialog(&c).await;
        assert_eq!(
            d.heading().as_deref(),
            Some("This tab's process was terminated by SIGHUP")
        );
        assert_eq!(d.response_label("close"), "Close workspace");
        respond(&d, "close");
        settled(&c).await;
        assert!(c.store.tab_count_in_pane(pane).await.is_none());
        c.window.destroy();
    }

    #[gtk::test]
    async fn reports_queue_and_manual_close_makes_response_harmless() {
        let (c, ws, pane) = setup("com.flowmux.App.UiTest.TerminationQueue").await;
        let a = add_tab(&c, ws, pane).await;
        let b = add_tab(&c, ws, pane).await;
        signal(&c, pane, a, libc::SIGKILL, false).await;
        signal(&c, pane, b, libc::SIGTERM, false).await;
        let first = dialog(&c).await;
        glib::timeout_future(Duration::from_millis(50)).await;
        assert_eq!(c.current_termination_dialog(), Some(first.clone()));
        let (ack, rx) = oneshot::channel();
        c.dispatch(GtkCommand::CloseSurface {
            pane,
            surface: a,
            ack,
        })
        .await;
        rx.await.unwrap().unwrap();
        respond(&first, "close");
        let second = glib::future_with_timeout(Duration::from_secs(5), async {
            loop {
                if let Some(d) = c.current_termination_dialog().filter(|d| *d != first) {
                    break d;
                }
                glib::timeout_future(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            second.heading().as_deref(),
            Some("This tab's process was terminated by SIGTERM")
        );
        respond(&second, "close");
        settled(&c).await;
        assert_eq!(c.store.tab_count_in_pane(pane).await, Some(1));
        c.window.destroy();
    }

    #[gtk::test]
    async fn terminated_close_cannot_remove_a_live_terminal() {
        let (c, _, pane) = setup("com.flowmux.App.UiTest.TerminationLiveGuard").await;
        let surface = c.pane_registry.borrow().active_surface(pane).unwrap();
        let (ack, rx) = oneshot::channel();
        c.dispatch(GtkCommand::CloseTerminatedSurface { pane, surface, ack })
            .await;
        rx.await.unwrap().unwrap();
        assert_eq!(c.store.tab_count_in_pane(pane).await, Some(1));
        c.window.destroy();
    }
}
