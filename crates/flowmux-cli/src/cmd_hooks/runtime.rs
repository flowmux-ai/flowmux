// SPDX-License-Identifier: GPL-3.0-or-later
//! Native hooks target either a GUI tab or a Host execution, never both.

use super::*;
use flowmux_core::{AgentActivity, AgentInstanceId, AgentStatus};
use flowmux_ipc::agent_host::{ClientRole, HostClient, HostRequest, NativeSessionEvent};

#[derive(Clone, Copy)]
pub(super) enum HookTarget {
    Tab(SurfaceId),
    Managed {
        instance: AgentInstanceId,
        epoch: uuid::Uuid,
        pid: u32,
        process_start: u64,
    },
}

impl HookTarget {
    pub(super) fn tab(self) -> Option<SurfaceId> {
        match self {
            Self::Tab(surface) => Some(surface),
            Self::Managed { .. } => None,
        }
    }

    pub(super) fn managed_from_env(agent: &str) -> anyhow::Result<Option<Self>> {
        let get = |key| {
            std::env::var(key).with_context(|| format!("missing managed hook context: {key}"))
        };
        if std::env::var_os("FLOWMUX_AGENT_INSTANCE_ID").is_none()
            && std::env::var_os("FLOWMUX_AGENT_HOST_EPOCH").is_none()
        {
            return Ok(None);
        }
        let instance = get("FLOWMUX_AGENT_INSTANCE_ID")?.parse()?;
        let epoch = get("FLOWMUX_AGENT_HOST_EPOCH")?.parse()?;
        get("FLOWMUX_SOCKET_PATH")?;
        // Inherited wrapper PID/environment cannot identify a nested provider.
        // Find the nearest provider ancestor; the Host checks its root birth.
        let mut ancestor = std::process::id();
        for _ in 0..64 {
            ancestor = flowmux_procmon::parent_pid(ancestor)
                .filter(|pid| *pid > 1)
                .context("managed hook has no live provider ancestor")?;
            if let Some(name) = flowmux_procmon::agent_name_for_pid(ancestor) {
                anyhow::ensure!(
                    name.eq_ignore_ascii_case(agent),
                    "managed hook provider changed"
                );
                let process_start = flowmux_procmon::process_start_time(ancestor)
                    .context("managed hook provider identity unavailable")?;
                return Ok(Some(Self::Managed {
                    instance,
                    epoch,
                    pid: ancestor,
                    process_start,
                }));
            }
        }
        anyhow::bail!("managed hook provider ancestry is too deep")
    }

    fn session(self, session_id: &str, event: NativeSessionEvent) -> Request {
        let Self::Managed {
            instance,
            epoch,
            pid,
            process_start,
        } = self
        else {
            unreachable!("native Host session requires a managed target")
        };
        Request::AgentHost {
            request: HostRequest::NativeSession {
                instance,
                epoch,
                pid,
                process_start,
                session_id: session_id.into(),
                seq: hooks::hook_seq().unwrap_or(0),
                event,
            },
        }
    }
}

pub(super) fn build_runtime_lifecycle_update(
    agent: &str,
    pid: Option<u32>,
    pane: Option<PaneId>,
    target: HookTarget,
    session_id: &str,
    lifecycle: AgentLifecycleEvent,
) -> Request {
    match target {
        HookTarget::Tab(surface) => {
            hooks::build_agent_lifecycle_update(agent, pid, pane, surface, session_id, lifecycle)
        }
        HookTarget::Managed {
            instance,
            epoch,
            pid,
            process_start,
        } => Request::AgentHost {
            request: HostRequest::Lifecycle {
                instance,
                epoch,
                pid,
                process_start,
                session_id: session_id.into(),
                seq: hooks::hook_seq().unwrap_or(0),
                event: lifecycle,
                notification: None,
            },
        },
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_runtime_activity_update(
    agent: &str,
    activity: Option<AgentActivity>,
    pid: Option<u32>,
    pane: Option<PaneId>,
    target: Option<HookTarget>,
    message: Option<&str>,
    status: Option<&str>,
    session: Option<&str>,
) -> Request {
    if let Some(target @ HookTarget::Managed { .. }) = target {
        let status_text = status.unwrap_or("Working").to_string();
        let event = match activity {
            None => return target.session(session.unwrap_or_default(), NativeSessionEvent::Ended),
            Some(AgentActivity::Running) => AgentLifecycleEvent::ProgressObserved { status_text },
            Some(AgentActivity::Idle) => AgentLifecycleEvent::TurnStopped {
                message: message.map(str::to_string),
                status_text,
            },
            Some(AgentActivity::NeedsInput) => AgentLifecycleEvent::SessionWaitStarted {
                context: None,
                outcome: None,
                reason: Default::default(),
                message: message.map(str::to_string),
                status_text,
                scope: None,
            },
        };
        return build_runtime_lifecycle_update(
            agent,
            pid,
            None,
            target,
            session.unwrap_or_default(),
            event,
        );
    }
    hooks::build_activity_update_with_metadata(
        agent,
        activity,
        pid,
        pane,
        target.and_then(HookTarget::tab),
        message,
        status,
        session,
    )
}

pub(super) fn session_start(
    target: HookTarget,
    session: Option<&str>,
    status: AgentStatus,
) -> Request {
    target.session(
        session.unwrap_or_default(),
        NativeSessionEvent::Started { status },
    )
}

pub(super) async fn send_managed(socket: Option<PathBuf>, requests: Vec<Request>) {
    let socket = socket.or_else(|| std::env::var_os("FLOWMUX_SOCKET_PATH").map(PathBuf::from));
    let Some(socket) = socket else {
        return;
    };
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let client = HostClient::connect(&socket, ClientRole::Hook).await?;
        let mut requests = requests.into_iter().peekable();
        while let Some(request) = requests.next() {
            if let Request::AgentHost { mut request } = request {
                if let HostRequest::Lifecycle { notification, .. } = &mut request {
                    if let Some(Request::Notify {
                        title, body, level, ..
                    }) = requests.peek()
                    {
                        *notification = Some(flowmux_ipc::protocol::AgentLifecycleNotification {
                            title: title.clone(),
                            body: body.clone(),
                            level: *level,
                        });
                        requests.next();
                    }
                }
                client.call(request).await?;
            }
        }
        Ok::<_, anyhow::Error>(())
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        tracing::debug!(target: "flowmux_hook", "managed hook delivery failed; no GUI fallback");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_events_keep_execution_identity_and_never_claim_a_tab() {
        let instance = AgentInstanceId::new();
        let epoch = uuid::Uuid::new_v4();
        let target = HookTarget::Managed {
            instance,
            epoch,
            pid: 42,
            process_start: 123,
        };
        assert!(target.tab().is_none());
        let Request::AgentHost {
            request:
                HostRequest::Lifecycle {
                    instance: actual_instance,
                    epoch: actual_epoch,
                    pid,
                    process_start,
                    session_id,
                    event: AgentLifecycleEvent::ProgressObserved { .. },
                    ..
                },
        } = build_runtime_activity_update(
            "claude",
            Some(AgentActivity::Running),
            Some(999),
            Some(PaneId::new()),
            Some(target),
            None,
            None,
            Some("native-session"),
        )
        else {
            panic!("managed activity must use the Host");
        };
        assert_eq!(
            (actual_instance, actual_epoch, pid, process_start),
            (instance, epoch, 42, 123)
        );
        assert_eq!(session_id, "native-session");
        assert!(matches!(
            build_runtime_activity_update(
                "claude",
                None,
                None,
                None,
                Some(target),
                None,
                None,
                Some("native-session")
            ),
            Request::AgentHost {
                request: HostRequest::NativeSession {
                    event: NativeSessionEvent::Ended,
                    ..
                }
            }
        ));
        assert!(matches!(
            session_start(target, Some("next-session"), AgentStatus::Idle),
            Request::AgentHost {
                request: HostRequest::NativeSession {
                    event: NativeSessionEvent::Started {
                        status: AgentStatus::Idle
                    },
                    ..
                }
            }
        ));
    }

    #[test]
    fn ordinary_tab_keeps_the_existing_hook_request() {
        let surface = SurfaceId::new();
        let target = HookTarget::Tab(surface);
        assert_eq!(target.tab(), Some(surface));
        let event = AgentLifecycleEvent::ProgressObserved {
            status_text: "Working".into(),
        };
        let request = build_runtime_lifecycle_update(
            "claude",
            Some(42),
            None,
            target,
            "session",
            event.clone(),
        );
        let expected = hooks::build_agent_lifecycle_update(
            "claude",
            Some(42),
            None,
            surface,
            "session",
            event,
        );
        let mut actual = serde_json::to_value(request).unwrap();
        let mut expected = serde_json::to_value(expected).unwrap();
        actual.as_object_mut().unwrap().remove("seq");
        expected.as_object_mut().unwrap().remove("seq");
        assert_eq!(actual, expected);
    }
}
