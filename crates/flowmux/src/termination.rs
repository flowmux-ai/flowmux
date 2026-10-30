// SPDX-License-Identifier: GPL-3.0-or-later
//! Exit classification and plain-text copy shared by the termination UI.

use flowmux_core::{PaneId, SurfaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitKind {
    Exited { code: i32 },
    Signaled { signal: i32 },
}

impl WaitKind {
    pub fn from_status(status: i32) -> Self {
        if libc::WIFSIGNALED(status) {
            Self::Signaled {
                signal: libc::WTERMSIG(status),
            }
        } else {
            Self::Exited {
                code: libc::WEXITSTATUS(status),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OomEvidence {
    SystemdResult,
    CgroupEvents,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    OutOfMemory { evidence: OomEvidence },
    MemoryPressureSuspected { level: u32 },
    Signal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildTermination {
    Exited { code: i32 },
    Signaled { signal: i32, cause: Cause },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminationContext {
    pub workspace_name: String,
    pub pane_label: String,
    pub tab_name: String,
    pub command: String,
    pub wrapper_pid: Option<i32>,
    pub time: String,
    pub scope_unit: Option<String>,
    pub isolation_unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminationReport {
    pub pane: PaneId,
    pub surface: SurfaceId,
    pub termination: ChildTermination,
    pub context: TerminationContext,
}

pub fn classify(
    kind: WaitKind,
    oom: Option<OomEvidence>,
    pressure_level: Option<u32>,
) -> ChildTermination {
    let signal = match kind {
        WaitKind::Exited { code } if oom.is_none() => return ChildTermination::Exited { code },
        WaitKind::Exited { .. } => libc::SIGKILL,
        WaitKind::Signaled { signal } => signal,
    };
    let cause = if let Some(evidence) = oom {
        Cause::OutOfMemory { evidence }
    } else if let Some(level) =
        pressure_level.filter(|level| signal == libc::SIGKILL && *level >= 2)
    {
        Cause::MemoryPressureSuspected { level }
    } else {
        Cause::Signal
    };
    ChildTermination::Signaled { signal, cause }
}

pub fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGTRAP => "SIGTRAP",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGKILL => "SIGKILL",
        libc::SIGUSR1 => "SIGUSR1",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGUSR2 => "SIGUSR2",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        libc::SIGTERM => "SIGTERM",
        libc::SIGSYS => "SIGSYS",
        libc::SIGXCPU => "SIGXCPU",
        libc::SIGXFSZ => "SIGXFSZ",
        _ => return format!("signal {signal}"),
    };
    name.into()
}

pub fn dialog_heading(t: &ChildTermination) -> String {
    match t {
        ChildTermination::Exited { .. } => "This tab's process exited".into(),
        ChildTermination::Signaled {
            cause: Cause::OutOfMemory { .. },
            ..
        } => "A process in this tab was killed: out of memory".into(),
        ChildTermination::Signaled {
            cause: Cause::MemoryPressureSuspected { .. },
            ..
        } => "A process in this tab was force-quit while the system was low on memory".into(),
        ChildTermination::Signaled { signal, .. } => format!(
            "This tab's process was terminated by {}",
            signal_name(*signal)
        ),
    }
}

pub fn dialog_body(report: &TerminationReport, last_tab_in_workspace: Option<&str>) -> String {
    let c = &report.context;
    let mut lines = vec![
        format!("Workspace: {}", c.workspace_name),
        format!("Pane: {}", c.pane_label),
        format!("Tab: {}", c.tab_name),
        format!("Command: {}", c.command),
    ];
    if let Some(pid) = c.wrapper_pid {
        lines.push(format!("Wrapper pid: {pid}"));
    }
    if let ChildTermination::Signaled { signal, .. } = report.termination {
        lines.push(format!("Signal: {} ({signal})", signal_name(signal)));
    }
    lines.push(format!("Time: {}", c.time));
    let evidence = match report.termination {
        ChildTermination::Signaled {
            cause:
                Cause::OutOfMemory {
                    evidence: OomEvidence::SystemdResult,
                },
            ..
        } => format!(
            "systemd reported oom-kill for {}",
            c.scope_unit.as_deref().unwrap_or("this tab's scope")
        ),
        ChildTermination::Signaled {
            cause:
                Cause::OutOfMemory {
                    evidence: OomEvidence::CgroupEvents,
                },
            ..
        } => "cgroup memory.events recorded an OOM kill".into(),
        ChildTermination::Signaled {
            cause: Cause::MemoryPressureSuspected { level },
            ..
        } => format!(
            "macOS memory pressure level was {}",
            match level {
                2 => "warning".into(),
                4 => "critical".into(),
                _ => level.to_string(),
            }
        ),
        _ => "No memory evidence was available on this platform".into(),
    };
    lines.push(evidence);
    if let Some(reason) = &c.isolation_unavailable {
        lines.push(format!(
            "Process isolation was unavailable for this tab: {reason}"
        ));
    }
    if let Some(name) = last_tab_in_workspace {
        lines.push(format!(
            "This is the last tab in workspace {name}; closing it closes the workspace."
        ));
    }
    lines.push("Flowmux and your other tabs and panes were not affected.".into());
    lines.join("\n")
}

#[cfg(target_os = "macos")]
pub fn memory_pressure_level() -> Option<u32> {
    let mut level: u32 = 0;
    let mut len = std::mem::size_of_val(&level);
    // SAFETY: the output buffer and length match the sysctl's uint32_t value.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut level as *mut u32).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0 && len == std::mem::size_of_val(&level)).then_some(level)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn report(t: ChildTermination) -> TerminationReport {
        TerminationReport {
            pane: PaneId::new(),
            surface: SurfaceId::new(),
            termination: t,
            context: TerminationContext {
                workspace_name: "ws".into(),
                pane_label: "p1".into(),
                tab_name: "shell".into(),
                command: "bash".into(),
                wrapper_pid: Some(4242),
                time: "12:00:00".into(),
                scope_unit: Some("flowmux-1-abc.scope".into()),
                isolation_unavailable: None,
            },
        }
    }

    #[test]
    fn wait_kind_decodes_exit_and_signal_statuses() {
        assert_eq!(WaitKind::from_status(3 << 8), WaitKind::Exited { code: 3 });
        assert_eq!(
            WaitKind::from_status(libc::SIGKILL),
            WaitKind::Signaled {
                signal: libc::SIGKILL
            }
        );
    }

    #[test]
    fn classify_exit_code_without_evidence_is_exited() {
        assert_eq!(
            classify(WaitKind::Exited { code: 137 }, None, Some(4)),
            ChildTermination::Exited { code: 137 }
        );
    }

    #[test]
    fn classify_exit_code_with_scope_oom_is_an_oom_kill() {
        assert_eq!(
            classify(
                WaitKind::Exited { code: 137 },
                Some(OomEvidence::SystemdResult),
                None
            ),
            ChildTermination::Signaled {
                signal: libc::SIGKILL,
                cause: Cause::OutOfMemory {
                    evidence: OomEvidence::SystemdResult
                }
            }
        );
    }

    #[test]
    fn classify_signal_prefers_oom_then_pressure_then_plain_signal() {
        let term = WaitKind::Signaled {
            signal: libc::SIGTERM,
        };
        let kill = WaitKind::Signaled {
            signal: libc::SIGKILL,
        };
        assert_eq!(
            classify(term, Some(OomEvidence::CgroupEvents), None),
            ChildTermination::Signaled {
                signal: libc::SIGTERM,
                cause: Cause::OutOfMemory {
                    evidence: OomEvidence::CgroupEvents
                }
            }
        );
        assert_eq!(
            classify(kill, None, Some(2)),
            ChildTermination::Signaled {
                signal: libc::SIGKILL,
                cause: Cause::MemoryPressureSuspected { level: 2 }
            }
        );
        assert_eq!(
            classify(kill, None, Some(1)),
            ChildTermination::Signaled {
                signal: libc::SIGKILL,
                cause: Cause::Signal
            }
        );
        assert_eq!(
            classify(term, None, Some(4)),
            ChildTermination::Signaled {
                signal: libc::SIGTERM,
                cause: Cause::Signal
            }
        );
    }

    #[test]
    fn headings_match_the_spec() {
        assert_eq!(
            dialog_heading(&ChildTermination::Signaled {
                signal: libc::SIGKILL,
                cause: Cause::OutOfMemory {
                    evidence: OomEvidence::SystemdResult
                }
            }),
            "A process in this tab was killed: out of memory"
        );
        assert_eq!(
            dialog_heading(&ChildTermination::Signaled {
                signal: libc::SIGKILL,
                cause: Cause::MemoryPressureSuspected { level: 4 }
            }),
            "A process in this tab was force-quit while the system was low on memory"
        );
        assert_eq!(
            dialog_heading(&ChildTermination::Signaled {
                signal: libc::SIGTERM,
                cause: Cause::Signal
            }),
            "This tab's process was terminated by SIGTERM"
        );
        assert_eq!(signal_name(libc::SIGHUP), "SIGHUP");
        assert_eq!(signal_name(64), "signal 64");
    }

    #[test]
    fn body_lists_context_evidence_and_last_tab_notice() {
        let mut r = report(ChildTermination::Signaled {
            signal: libc::SIGTERM,
            cause: Cause::OutOfMemory {
                evidence: OomEvidence::SystemdResult,
            },
        });
        let body = dialog_body(&r, Some("ws"));
        for line in [
            "Workspace: ws",
            "Pane: p1",
            "Tab: shell",
            "Command: bash",
            "Wrapper pid: 4242",
            "Signal: SIGTERM (15)",
            "Time: 12:00:00",
            "systemd reported oom-kill for flowmux-1-abc.scope",
            "This is the last tab in workspace ws; closing it closes the workspace.",
            "Flowmux and your other tabs and panes were not affected.",
        ] {
            assert!(
                body.lines().any(|l| l == line),
                "missing line {line:?} in:\n{body}"
            );
        }
        assert!(!body.contains("Process isolation was unavailable"));
        r.context.isolation_unavailable = Some("no session bus".into());
        r.context.wrapper_pid = None;
        r.termination = ChildTermination::Signaled {
            signal: libc::SIGKILL,
            cause: Cause::Signal,
        };
        let body = dialog_body(&r, None);
        assert!(body.contains("Process isolation was unavailable for this tab: no session bus"));
        assert!(body.contains("No memory evidence was available on this platform"));
        assert!(!body.contains("Wrapper pid"));
        assert!(!body.contains("last tab"));
    }
}
