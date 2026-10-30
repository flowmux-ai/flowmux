// SPDX-License-Identifier: GPL-3.0-or-later
//! Nonblocking scope attachment and exit forwarding; GTK callbacks remain local.
use super::*;
use crate::ui::pane_terminal::{ChildExit, ChildSpawned};

impl WindowController {
    #[cfg(target_os = "linux")]
    pub fn use_process_isolation(&mut self, isolation: crate::process_scope::ScopeIsolation) {
        *self.scope_isolation.borrow_mut() = Some(isolation);
    }

    pub(super) fn install_process_callbacks(&mut self) {
        #[cfg(target_os = "linux")]
        {
            use crate::process_scope::{self, ScopeIsolation, ScopeLimits};
            let isolation = self.scope_isolation.clone();
            let scopes = self.pane_scopes.clone();
            let options = self.options.clone();
            let handle = self.tokio_handle.clone();
            let warned = Rc::new(RefCell::new(HashSet::new()));
            let registry = self.pane_registry.clone();
            self.callbacks.on_child_spawned = Rc::new(RefCell::new(move |child: ChildSpawned| {
                let iso = isolation.borrow().clone();
                let reason = iso.as_ref().and_then(ScopeIsolation::unavailable_reason);
                if let Some(reason) = reason {
                    scopes
                        .borrow_mut()
                        .insert(child.surface, Err(reason.into()));
                    return; // Dropping child releases the gate.
                }
                let (Some(iso), Some(handle)) = (iso, handle.clone()) else {
                    return;
                };
                let limits = ScopeLimits {
                    memory_high: options.borrow().pane_memory_high_bytes(),
                    memory_max: options.borrow().pane_memory_max_bytes(),
                };
                let fallback_score = options.borrow().child_oom_score_adj_or_none();
                let scopes = scopes.clone();
                let warned = warned.clone();
                let registry = registry.clone();
                let unit = process_scope::unit_name(std::process::id(), child.surface);
                glib::spawn_future_local(async move {
                    // Tokio work stays off GTK, including session-bus connection I/O.
                    let task_unit = unit.clone();
                    let result = handle
                        .spawn(async move {
                            tokio::time::timeout(
                                Duration::from_secs(1),
                                process_scope::start_scope(
                                    &iso,
                                    &task_unit,
                                    &format!("Flowmux terminal pane {}", child.pane),
                                    child.pid as u32,
                                    limits,
                                ),
                            )
                            .await
                            .map_err(|_| "scope start timed out".to_string())
                            .and_then(|r| r)
                        })
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r);
                    // The tab may have been closed while the bus call was pending.
                    let live = registry
                        .borrow()
                        .terminals
                        .get(&child.surface)
                        .is_some_and(|t| t.pid.get() == Some(child.pid));
                    if live {
                        if let Err(reason) = &result {
                            if warned.borrow_mut().insert(reason.clone()) {
                                tracing::warn!(%reason, "terminal process isolation unavailable");
                            }
                            if let Some(score) = fallback_score {
                                // Still gated: no descendants exist yet to miss this fallback.
                                let _ = std::fs::write(
                                    format!("/proc/{}/oom_score_adj", child.pid),
                                    score.to_string(),
                                );
                            }
                        }
                        scopes.borrow_mut().insert(child.surface, result);
                    }
                    child.gate.release();
                });
            }));
        }
        #[cfg(not(target_os = "linux"))]
        {
            self.callbacks.on_child_spawned = Rc::new(RefCell::new(|_: ChildSpawned| {}));
        }

        let bridge = self.bridge.clone();
        #[cfg(target_os = "linux")]
        let scopes = self.pane_scopes.clone();
        self.callbacks.on_child_exited = Rc::new(RefCell::new(move |exit: ChildExit| {
            #[cfg(target_os = "linux")]
            let (scope_unit, cgroup_dir, isolation_unavailable) =
                match scopes.borrow_mut().remove(&exit.surface) {
                    Some(Ok(scope)) => (Some(scope.unit), Some(scope.cgroup_dir), None),
                    Some(Err(reason)) => (None, None, Some(reason)),
                    None => (None, None, None),
                };
            #[cfg(not(target_os = "linux"))]
            let (scope_unit, cgroup_dir, isolation_unavailable) = (None, None, None);
            let _ = bridge.tx.try_send(GtkCommand::ChildExited {
                exit,
                scope_unit,
                cgroup_dir,
                isolation_unavailable,
            });
        }));
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    async fn callbacks_release_unisolated_gate_and_forward_scope_evidence() {
        let (mut controller, _, pane) = super::super::tests::build_single_workspace_controller(
            "com.flowmux.App.UiTest.ProcessCallbacks",
        )
        .await;
        let (bridge, rx) = Bridge::new();
        controller.bridge = bridge;
        controller.install_process_callbacks();
        let (pty, gate) = flowmux_terminal::pty::Pty::spawn_gated(
            &["sh", "-c", "printf up"],
            None,
            &[],
            20,
            5,
            None,
        )
        .unwrap();
        let surface = SurfaceId::new();
        (controller.callbacks.on_child_spawned.borrow_mut())(ChildSpawned {
            pane,
            surface,
            pid: pty.child_pid(),
            gate,
        });
        let mut poll = libc::pollfd {
            fd: pty.master_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert!(
            unsafe { libc::poll(&mut poll, 1, 5000) } > 0,
            "unisolated gate must release immediately"
        );
        pty.close_async();
        let scope = crate::process_scope::PaneScope {
            unit: "flowmux-1-test.scope".into(),
            cgroup_dir: "/tmp/fm-test-cgroup".into(),
        };
        controller
            .pane_scopes
            .borrow_mut()
            .insert(surface, Ok(scope.clone()));
        (controller.callbacks.on_child_exited.borrow_mut())(ChildExit {
            pane,
            surface,
            status: libc::SIGKILL,
            closing: false,
            wrapper_pid: Some(1),
            command: "sh".into(),
        });
        match rx.try_recv().unwrap() {
            GtkCommand::ChildExited {
                exit,
                scope_unit,
                cgroup_dir,
                isolation_unavailable,
            } => {
                assert_eq!(exit.surface, surface);
                assert_eq!(scope_unit, Some(scope.unit));
                assert_eq!(cgroup_dir, Some(scope.cgroup_dir));
                assert_eq!(isolation_unavailable, None);
            }
            _ => panic!("wrong command"),
        }
        assert!(controller.pane_scopes.borrow().is_empty());
        controller
            .pane_scopes
            .borrow_mut()
            .insert(surface, Err("scope timeout".into()));
        (controller.callbacks.on_child_exited.borrow_mut())(ChildExit {
            pane,
            surface,
            status: libc::SIGHUP,
            closing: true,
            wrapper_pid: None,
            command: "sh".into(),
        });
        assert!(matches!(rx.try_recv().unwrap(), GtkCommand::ChildExited {
            isolation_unavailable: Some(reason), ..
        } if reason == "scope timeout"));
    }
}
