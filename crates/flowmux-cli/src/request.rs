// SPDX-License-Identifier: GPL-3.0-or-later
//! Cmd -> IPC Request builders (send-key, browser, all verbs).
//!
//! Split out of `main.rs` (pure move; behavior unchanged).

use super::*;

/// Map a `flowmux browser <op>` invocation to its IPC request. Every
/// arm maps 1:1 to an existing `Request::Browser*` variant, so the new
/// namespace and the hidden hyphenated aliases share one handler path.
pub(crate) fn browser_op_to_request(op: BrowserOp) -> anyhow::Result<Request> {
    let request = match op {
        BrowserOp::Open {
            url,
            right: _,
            down,
        } => {
            // `--down` splits horizontally; everything else (including the
            // default and `--right`) splits vertically, matching the prior
            // `flowmux browser <url>` behavior.
            let direction = if down {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            };
            // When invoked from a terminal that flowmux spawned, the PTY's
            // `FLOWMUX_PANE_ID` lets the daemon resolve "next to me" without
            // the caller passing a pane id explicitly.
            Request::BrowserOpen {
                url,
                target_pane: pane_from_env(),
                direction,
            }
        }
        BrowserOp::Snapshot { pane } => Request::BrowserSnapshot { pane },
        BrowserOp::Eval { pane, source } => Request::BrowserEval { pane, source },
        BrowserOp::Navigate { pane, url } => Request::BrowserNavigate { pane, url },
        BrowserOp::Back { pane } => Request::BrowserBack { pane },
        BrowserOp::Forward { pane } => Request::BrowserForward { pane },
        BrowserOp::Reload { pane } => Request::BrowserReload { pane },
        BrowserOp::Url { pane } => Request::BrowserUrl { pane },
        BrowserOp::Title { pane } => Request::BrowserTitle { pane },
        BrowserOp::Click { pane, target } => Request::BrowserClick { pane, target },
        BrowserOp::Fill {
            pane,
            target,
            value,
        } => Request::BrowserFill {
            pane,
            target,
            value,
        },
        BrowserOp::Select {
            pane,
            target,
            value,
        } => Request::BrowserSelect {
            pane,
            target,
            value,
        },
        BrowserOp::Scroll { pane, target, x, y } => Request::BrowserScroll { pane, target, x, y },
        BrowserOp::Type { pane, text } => Request::BrowserType { pane, text },
        BrowserOp::Press { pane, key } => Request::BrowserPress { pane, key },
        BrowserOp::Text { pane, target } => Request::BrowserText { pane, target },
        BrowserOp::Value { pane, target } => Request::BrowserValue { pane, target },
        BrowserOp::Attr { pane, target, name } => Request::BrowserAttr { pane, target, name },
        BrowserOp::DblClick { pane, target } => Request::BrowserDblClick { pane, target },
        BrowserOp::Hover { pane, target } => Request::BrowserHover { pane, target },
        BrowserOp::Focus { pane, target } => Request::BrowserFocus { pane, target },
        BrowserOp::Blur { pane, target } => Request::BrowserBlur { pane, target },
        BrowserOp::Check { pane, target } => Request::BrowserCheck { pane, target },
        BrowserOp::Uncheck { pane, target } => Request::BrowserUncheck { pane, target },
        BrowserOp::IsVisible { pane, target } => Request::BrowserIsVisible { pane, target },
        BrowserOp::IsEnabled { pane, target } => Request::BrowserIsEnabled { pane, target },
        BrowserOp::IsChecked { pane, target } => Request::BrowserIsChecked { pane, target },
        BrowserOp::Wait {
            pane,
            selector,
            text,
            url,
            ready_state,
            js,
            timeout_ms,
            poll_ms,
        } => {
            if timeout_ms == 0 {
                anyhow::bail!("--timeout-ms must be greater than 0");
            }
            if poll_ms == 0 {
                anyhow::bail!("--poll-ms must be greater than 0");
            }
            Request::BrowserWait {
                pane,
                condition: browser_wait_condition(selector, text, url, ready_state, js)?,
                timeout_ms,
                poll_ms,
            }
        }
        BrowserOp::Screenshot { pane, path } => Request::BrowserScreenshot { pane, path },
        BrowserOp::Count { pane, selector } => Request::BrowserCount { pane, selector },
    };
    Ok(request)
}
pub(crate) fn browser_wait_condition(
    selector: Option<String>,
    text: Option<String>,
    url: Option<String>,
    ready_state: Option<String>,
    js: Option<String>,
) -> anyhow::Result<BrowserWaitCondition> {
    let mut conditions = [
        selector.map(BrowserWaitCondition::Selector),
        text.map(BrowserWaitCondition::Text),
        url.map(BrowserWaitCondition::Url),
        ready_state.map(BrowserWaitCondition::ReadyState),
        js.map(BrowserWaitCondition::Js),
    ]
    .into_iter()
    .flatten();

    let Some(condition) = conditions.next() else {
        anyhow::bail!("one wait condition is required");
    };
    if conditions.next().is_some() {
        anyhow::bail!("only one wait condition may be used");
    }
    Ok(condition)
}
pub(crate) fn build_request(cmd: Cmd) -> anyhow::Result<Request> {
    Ok(match cmd {
        Cmd::Ping => Request::Ping,
        Cmd::Ssh { op } => ssh_op_to_request(op)?,
        Cmd::Tree | Cmd::Agents | Cmd::SessionName => Request::WorkspaceTree,
        Cmd::Workspace {
            op: WorkspaceOp::New { name, root },
        } => Request::WorkspaceCreate {
            name,
            root: root.map(Ok).unwrap_or_else(std::env::current_dir)?,
        },
        Cmd::Workspace {
            op: WorkspaceOp::Ls,
        } => Request::WorkspaceList,
        Cmd::Workspace {
            op: WorkspaceOp::Current,
        } => Request::WorkspaceCurrent,
        Cmd::Workspace {
            op: WorkspaceOp::Focus { workspace },
        } => Request::WorkspaceFocus { workspace },
        Cmd::Notify {
            pane,
            title,
            level,
            body,
        } => Request::Notify {
            pane: pane.or_else(pane_from_env),
            surface: hooks::surface_from_env(),
            title,
            body,
            level: parse_level(&level),
        },
        Cmd::NotifyComplete {
            agent,
            message,
            pane,
        } => {
            let body = message.unwrap_or_else(|| "task complete".to_string());
            Request::Notify {
                pane: pane.or_else(pane_from_env),
                surface: hooks::surface_from_env(),
                title: format!("{agent} ready"),
                body,
                level: NotificationLevel::TurnCompleted,
            }
        }
        Cmd::Notifications { op } => match op {
            NotificationOp::List { unread } => Request::NotificationsList {
                unread_only: unread,
            },
            NotificationOp::Open { id } => Request::NotificationOpen { id },
            NotificationOp::JumpToUnread => Request::NotificationJumpToUnread,
            NotificationOp::MarkRead { id } => Request::NotificationMarkRead { id },
            NotificationOp::Clear => Request::NotificationsClear,
        },
        Cmd::Split { pane, down, .. } => {
            let direction = if down {
                SplitDirection::Horizontal
            } else {
                SplitDirection::Vertical
            };
            Request::PaneSplit { pane, direction }
        }
        Cmd::SendKeys { pane, keys } => Request::PaneSendKeys { pane, keys },
        Cmd::SendKey { pane, key } => Request::PaneSendKeys {
            pane: resolve_pane(pane)?,
            keys: named_key_to_bytes(&key)?,
        },
        Cmd::ReadScreen { pane } => Request::PaneReadScreen {
            pane: resolve_pane(pane)?,
        },
        Cmd::CapturePane { pane } => Request::PaneReadScreen {
            pane: resolve_pane(pane)?,
        },
        Cmd::ListPanes => Request::WorkspaceTree,
        Cmd::SelectPane { pane } => Request::PaneFocus { pane },
        Cmd::ResizePane { pane, ratio } => Request::PaneResize { pane, ratio },
        Cmd::FocusPane { pane } => Request::PaneFocus {
            pane: resolve_pane(pane)?,
        },
        Cmd::ClosePane { pane } => Request::PaneClose {
            pane: resolve_pane(pane)?,
        },
        Cmd::FocusTab { surface, pane } => Request::SurfaceFocus {
            pane: resolve_pane(pane)?,
            surface,
        },
        Cmd::CloseTab { surface, pane } => Request::SurfaceClose {
            pane: resolve_pane(pane)?,
            surface,
        },
        Cmd::NewTab {
            workspace,
            cwd,
            shell,
        } => Request::SurfaceCreate {
            workspace: resolve_workspace(workspace)?,
            cwd,
            shell,
        },
        Cmd::Browser { op } => browser_op_to_request(op)?,
        Cmd::NotifyStream { .. } => unreachable!("handled before request build"),
        Cmd::TmuxCompat { .. } => unreachable!("handled before request build"),
        Cmd::ClaudeTeams { count, root, args } => Request::ClaudeTeams {
            count,
            args,
            root: root.map(Ok).unwrap_or_else(std::env::current_dir)?,
        },
        Cmd::BrowserSnapshot { pane } => Request::BrowserSnapshot { pane },
        Cmd::BrowserEval { pane, source } => Request::BrowserEval { pane, source },
        Cmd::BrowserNavigate { pane, url } => Request::BrowserNavigate { pane, url },
        Cmd::BrowserBack { pane } => Request::BrowserBack { pane },
        Cmd::BrowserForward { pane } => Request::BrowserForward { pane },
        Cmd::BrowserReload { pane } => Request::BrowserReload { pane },
        Cmd::BrowserUrl { pane } => Request::BrowserUrl { pane },
        Cmd::BrowserTitle { pane } => Request::BrowserTitle { pane },
        Cmd::BrowserClick { pane, target } => Request::BrowserClick { pane, target },
        Cmd::BrowserFill {
            pane,
            target,
            value,
        } => Request::BrowserFill {
            pane,
            target,
            value,
        },
        Cmd::BrowserSelect {
            pane,
            target,
            value,
        } => Request::BrowserSelect {
            pane,
            target,
            value,
        },
        Cmd::BrowserScroll { pane, target, x, y } => Request::BrowserScroll { pane, target, x, y },
        Cmd::BrowserType { pane, text } => Request::BrowserType { pane, text },
        Cmd::BrowserPress { pane, key } => Request::BrowserPress { pane, key },
        Cmd::BrowserText { pane, target } => Request::BrowserText { pane, target },
        Cmd::BrowserValue { pane, target } => Request::BrowserValue { pane, target },
        Cmd::BrowserAttr { pane, target, name } => Request::BrowserAttr { pane, target, name },
        Cmd::ImportCookies { from, domain } => Request::ImportCookies {
            source: from,
            domain,
        },
        Cmd::ListBrowsers => unreachable!("handled before request build"),
        Cmd::Theme { .. } => unreachable!("handled before request build"),
        Cmd::Agent { .. } => unreachable!("handled before request build"),
        Cmd::Hooks { .. } => unreachable!("handled before request build"),
        Cmd::Doctor => unreachable!("handled before request build"),
        Cmd::Fix => unreachable!("handled before request build"),
        Cmd::PtyTee { .. } => unreachable!("handled before request build"),
        Cmd::Identify => unreachable!("handled before request build"),
        Cmd::Capabilities => unreachable!("handled before request build"),
    })
}

fn ssh_op_to_request(op: SshOp) -> anyhow::Result<Request> {
    use flowmux_core::{SshForwardSpec, SshTarget, SshWorkspaceConfig};
    use flowmux_ipc::protocol::SshRequest;
    let request = match op {
        SshOp::Host(args) => {
            let cli = Cli::try_parse_from(
                ["flowmux", "ssh", "connect"]
                    .into_iter()
                    .map(String::from)
                    .chain(args),
            )?;
            return build_request(cli.cmd);
        }
        SshOp::Connect {
            host,
            cwd,
            name,
            port,
            user,
            identity_file,
            config_file,
            tmux,
            command,
        } => {
            let mut target = SshTarget::parse(&host).map_err(anyhow::Error::msg)?;
            target.user = user.or(target.user);
            target.port = port;
            target.identity_file = identity_file;
            target.config_file = config_file;
            let config = SshWorkspaceConfig {
                target,
                cwd,
                tmux,
                forwards: Vec::new(),
            };
            config.validate().map_err(anyhow::Error::msg)?;
            if command.iter().any(|arg| arg.contains('\0')) {
                anyhow::bail!("SSH command contains a NUL byte");
            }
            SshRequest::Create {
                request_id: uuid::Uuid::new_v4(),
                name,
                config,
                command,
            }
        }
        SshOp::Status { workspace }
        | SshOp::Forward {
            op: SshForwardOp::List { workspace },
        } => SshRequest::Status {
            workspace: resolve_workspace(workspace)?,
        },
        SshOp::Reconnect { workspace } => SshRequest::Connect {
            workspace: resolve_workspace(workspace)?,
        },
        SshOp::Disconnect { workspace } => SshRequest::Disconnect {
            workspace: resolve_workspace(workspace)?,
        },
        SshOp::Forward {
            op:
                SshForwardOp::Add {
                    workspace,
                    remote_port,
                    local_port,
                    https,
                },
        } => {
            let spec = SshForwardSpec {
                id: uuid::Uuid::new_v4(),
                remote_port,
                local_port,
                https,
            };
            spec.validate().map_err(anyhow::Error::msg)?;
            SshRequest::ForwardAdd {
                workspace: resolve_workspace(workspace)?,
                spec,
            }
        }
        SshOp::Forward {
            op: SshForwardOp::Remove { workspace, id },
        } => SshRequest::ForwardRemove {
            workspace: resolve_workspace(workspace)?,
            id,
        },
        SshOp::Preview { workspace, id } => SshRequest::Preview {
            workspace: resolve_workspace(workspace)?,
            id,
        },
    };
    Ok(Request::Ssh { request })
}

pub(crate) fn parse_level(s: &str) -> NotificationLevel {
    match s {
        "complete" => NotificationLevel::TurnCompleted,
        "attention" | "needs_input" => NotificationLevel::NeedsInput,
        "error" => NotificationLevel::Error,
        _ => NotificationLevel::Info,
    }
}

#[cfg(test)]
mod ssh_tests {
    use super::*;
    use flowmux_ipc::protocol::SshRequest;

    #[test]
    fn ssh_host_alias_reuses_connect_flags_and_remote_command_parser() {
        let arguments = [
            "alice@devbox",
            "--cwd",
            "/srv/a b",
            "--name",
            "Remote",
            "--port",
            "2222",
            "--user",
            "bob",
            "--identity-file",
            "/tmp/key",
            "--config-file",
            "/tmp/config",
            "--tmux",
            "--",
            "agent",
            "--prompt",
            "a ' literal $(value)",
        ];
        let create = |prefix: &[&str]| {
            let cli = Cli::try_parse_from(prefix.iter().chain(arguments.iter()).copied()).unwrap();
            let Request::Ssh {
                request:
                    SshRequest::Create {
                        config,
                        command,
                        name,
                        ..
                    },
            } = build_request(cli.cmd).unwrap()
            else {
                panic!("SSH create request");
            };
            (config, command, name)
        };
        assert_eq!(
            create(&["flowmux", "ssh"]),
            create(&["flowmux", "ssh", "connect"])
        );
        for arguments in [
            vec!["flowmux", "ssh", "devbox", "--port", "0"],
            vec!["flowmux", "ssh", "devbox", "--cwd", "relative"],
            vec!["flowmux", "ssh", "devbox", "--unknown"],
        ] {
            assert!(build_request(Cli::try_parse_from(arguments).unwrap().cmd).is_err());
        }
    }

    #[test]
    fn ssh_reserved_host_names_require_explicit_connect() {
        for host in [
            "status",
            "reconnect",
            "disconnect",
            "forward",
            "preview",
            "connect",
            "help",
        ] {
            let cli = Cli::try_parse_from(["flowmux", "ssh", "connect", host]).unwrap();
            assert!(matches!(build_request(cli.cmd).unwrap(), Request::Ssh {
                request: SshRequest::Create { config, .. }
            } if config.target.host == host));
        }
        let cli = Cli::try_parse_from(["flowmux", "ssh", "status"]).unwrap();
        assert!(matches!(
            cli.cmd,
            Cmd::Ssh {
                op: SshOp::Status { .. }
            }
        ));
        assert!(Cli::try_parse_from(["flowmux", "ssh", "status", "--port", "22"]).is_err());
    }

    #[test]
    fn ssh_connect_preserves_remote_arguments_and_validates_configuration() {
        let cli = Cli::try_parse_from([
            "flowmux",
            "ssh",
            "connect",
            "alice@devbox",
            "--port",
            "2222",
            "--cwd",
            "/srv/a b",
            "--name",
            "Remote",
            "--config-file",
            "/tmp/ssh config",
            "--identity-file",
            "/tmp/key",
            "--tmux",
            "--",
            "agent",
            "--prompt",
            "keep spaces",
        ])
        .unwrap();
        let Request::Ssh {
            request:
                SshRequest::Create {
                    config,
                    command,
                    name,
                    request_id,
                },
        } = build_request(cli.cmd).unwrap()
        else {
            panic!("SSH create request")
        };
        assert_eq!(config.target.destination(), "alice@devbox");
        assert_eq!(config.target.port, Some(2222));
        assert_eq!(config.cwd.as_deref(), Some("/srv/a b"));
        assert_eq!(config.target.config_file, Some("/tmp/ssh config".into()));
        assert_eq!(config.target.identity_file, Some("/tmp/key".into()));
        assert_eq!(name.as_deref(), Some("Remote"));
        assert!(config.tmux && !request_id.is_nil());
        assert_eq!(command, ["agent", "--prompt", "keep spaces"]);
        for args in [
            vec!["flowmux", "ssh", "connect", "bad host"],
            vec!["flowmux", "ssh", "connect", "devbox", "--port", "0"],
            vec!["flowmux", "ssh", "connect", "devbox", "--cwd", "relative"],
        ] {
            assert!(build_request(Cli::try_parse_from(args).unwrap().cmd).is_err());
        }
    }

    #[test]
    fn ssh_controls_and_forwards_target_the_explicit_workspace() {
        let id = "11111111-1111-1111-1111-111111111111";
        for action in ["status", "reconnect", "disconnect"] {
            let cli = Cli::try_parse_from(["flowmux", "ssh", action, "--workspace", id]).unwrap();
            let Request::Ssh { request } = build_request(cli.cmd).unwrap() else {
                panic!("SSH request")
            };
            let actual = match request {
                SshRequest::Status { workspace }
                | SshRequest::Connect { workspace }
                | SshRequest::Disconnect { workspace } => workspace,
                _ => panic!("SSH control request"),
            };
            assert_eq!(actual.to_string(), id);
        }
        let cli = Cli::try_parse_from([
            "flowmux",
            "ssh",
            "forward",
            "add",
            "--workspace",
            id,
            "--remote-port",
            "3000",
            "--https",
        ])
        .unwrap();
        let Request::Ssh {
            request: SshRequest::ForwardAdd { workspace, spec },
        } = build_request(cli.cmd).unwrap()
        else {
            panic!("forward add request")
        };
        assert_eq!(workspace.to_string(), id);
        assert_eq!(
            (spec.remote_port, spec.local_port, spec.https),
            (3000, None, true)
        );
        let cli =
            Cli::try_parse_from(["flowmux", "ssh", "preview", id, "--workspace", id]).unwrap();
        assert!(matches!(
            build_request(cli.cmd).unwrap(),
            Request::Ssh {
                request: SshRequest::Preview { .. }
            }
        ));
        let cli = Cli::try_parse_from([
            "flowmux",
            "ssh",
            "forward",
            "add",
            "--workspace",
            id,
            "--remote-port",
            "0",
        ])
        .unwrap();
        assert!(build_request(cli.cmd).is_err());
    }
}
