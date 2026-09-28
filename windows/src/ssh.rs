// SPDX-License-Identifier: GPL-3.0-or-later
//! Direct Windows OpenSSH sessions. Local argv and remote POSIX quoting remain
//! separate; no Unix control socket or arbitrary command is accepted here.

use crate::shell::Shell;
use anyhow::ensure;
use flowmux_core::{
    ssh::{shell_quote, validate_remote_cwd},
    SshForwardSpec, SshTarget, SshWorkspaceConfig,
};

/// Display only: remote paths must never become local process/file paths.
/// A browser tab has no cwd, so retain a known SSH directory before falling
/// back to the workspace's configured starting directory.
pub fn display_cwd(workspace: &crate::model::Workspace) -> Option<&str> {
    use flowmux_core::{Pane, PaneContent, PaneId, PaneSurface, SurfaceKind};
    fn remote(surface: &PaneSurface) -> Option<&str> {
        match &surface.kind {
            SurfaceKind::SshTerminal { cwd, .. } => cwd.as_deref(),
            _ => None,
        }
    }
    fn known(root: &Pane, pane: Option<PaneId>) -> Option<&str> {
        match root {
            Pane::Leaf {
                id,
                content: PaneContent::Tabs { active, surfaces },
            } if pane.is_none_or(|pane| pane == *id) => {
                let index = surfaces
                    .iter()
                    .position(|surface| surface.id == *active)
                    .unwrap_or(0);
                surfaces[..index]
                    .iter()
                    .rev()
                    .chain(surfaces[index..].iter())
                    .find_map(remote)
            }
            Pane::Split { first, second, .. } => known(first, pane).or_else(|| known(second, pane)),
            _ => None,
        }
    }
    let config = workspace.ssh.as_ref()?;
    let active = workspace
        .root
        .active_surface_id(workspace.focused)
        .and_then(|active| workspace.root.find_surface_ref(workspace.focused, active));
    if let Some(PaneSurface {
        kind: SurfaceKind::SshTerminal { cwd, .. },
        ..
    }) = active
    {
        // An active home-directory terminal must not borrow a sibling's cwd.
        return cwd.as_deref().or(config.cwd.as_deref());
    }
    known(&workspace.root, Some(workspace.focused))
        .or_else(|| known(&workspace.root, None))
        .or(config.cwd.as_deref())
}

pub fn set_remote_cwd(
    root: &mut flowmux_core::Pane,
    pane: flowmux_core::PaneId,
    surface: flowmux_core::SurfaceId,
    value: Option<String>,
) -> bool {
    use flowmux_core::{Pane, PaneContent, SurfaceKind};
    match root {
        Pane::Leaf {
            id,
            content: PaneContent::Tabs { surfaces, .. },
        } if *id == pane => {
            let Some(tab) = surfaces.iter_mut().find(|tab| tab.id == surface) else {
                return false;
            };
            let SurfaceKind::SshTerminal { cwd, .. } = &mut tab.kind else {
                return false;
            };
            *cwd = value;
            true
        }
        Pane::Split { first, second, .. } => {
            set_remote_cwd(first, pane, surface, value.clone())
                || set_remote_cwd(second, pane, surface, value)
        }
        _ => false,
    }
}

pub fn terminal_shell(
    config: &SshWorkspaceConfig,
    cwd: Option<&str>,
    tmux_session: Option<&str>,
    attach: bool,
) -> anyhow::Result<Shell> {
    config.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        config.tmux == tmux_session.is_some(),
        "SSH tmux configuration and terminal session do not match"
    );
    let cwd = cwd.or(config.cwd.as_deref());
    validate_remote_cwd(cwd).map_err(anyhow::Error::msg)?;
    let login = "exec \"${SHELL:-/bin/sh}\" -l";
    let command = if let Some(session) = tmux_session {
        let suffix = session
            .strip_prefix("flowmux-")
            .ok_or_else(|| anyhow::anyhow!("Invalid flowmux tmux session"))?;
        ensure!(
            suffix
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
                && uuid::Uuid::parse_str(suffix).is_ok(),
            "Invalid flowmux tmux session"
        );
        if attach {
            // A missing attempted/restored session must fail visibly instead of
            // silently starting another shell. Reattach never changes its cwd.
            format!("exec tmux -u attach-session -t {}", shell_quote(session))
        } else {
            let directory = cwd
                .map(|path| format!(" -c {}", shell_quote(path)))
                .unwrap_or_default();
            format!(
                "exec tmux -u new-session -s {}{directory} {}",
                shell_quote(session),
                shell_quote(login)
            )
        }
    } else {
        let directory = cwd
            .map(|path| format!("cd {} || exit; ", shell_quote(path)))
            .unwrap_or_default();
        format!("{directory}{login}")
    };
    let mut args = vec!["-tt".into()];
    args.extend(connection_args(&config.target)?);
    args.extend([config.target.host.clone(), command]);
    let shell = Shell {
        program: "ssh".into(),
        args,
    };
    shell.validate()?;
    Ok(shell)
}

pub fn forwarding_shell(
    target: &SshTarget,
    spec: &SshForwardSpec,
    local_port: u16,
) -> anyhow::Result<Shell> {
    spec.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        local_port != 0,
        "Resolved local forward port must not be zero"
    );
    ensure!(
        spec.local_port
            .is_none_or(|requested| requested == local_port),
        "Resolved local forward port differs from the requested port"
    );
    let mut args = vec!["-N".into(), "-T".into()];
    args.extend(connection_args(target)?);
    args.extend([
        "-o".into(),
        "ExitOnForwardFailure=yes".into(),
        "-L".into(),
        format!("127.0.0.1:{local_port}:127.0.0.1:{}", spec.remote_port),
        target.host.clone(),
    ]);
    let shell = Shell {
        program: "ssh".into(),
        args,
    };
    shell.validate()?;
    Ok(shell)
}

fn connection_args(target: &SshTarget) -> anyhow::Result<Vec<String>> {
    target.validate().map_err(anyhow::Error::msg)?;
    let mut args = Vec::new();
    for option in [
        "ForwardAgent=no",
        "ForwardX11=no",
        "PermitLocalCommand=no",
        "RemoteCommand=none",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
    ] {
        args.extend(["-o".into(), option.into()]);
    }
    if let Some(user) = &target.user {
        args.extend(["-l".into(), user.clone()]);
    }
    if let Some(port) = target.port {
        args.extend(["-p".into(), port.to_string()]);
    }
    if let Some(path) = &target.identity_file {
        args.extend(["-i".into(), path.to_str().unwrap().into()]);
    }
    if let Some(path) = &target.config_file {
        args.extend(["-F".into(), path.to_str().unwrap().into()]);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn config() -> SshWorkspaceConfig {
        SshWorkspaceConfig {
            target: SshTarget::parse("dev@example.test").unwrap(),
            cwd: None,
            tmux: false,
            forwards: Vec::new(),
        }
    }

    #[test]
    fn display_cwd_retains_remote_context_on_browser_tabs_without_local_fallback() {
        use flowmux_core::{Pane, PaneContent, PaneSurface, SplitDirection};
        let mut config = config();
        config.cwd = Some("/설정/한 & 😀".into());
        let mut workspace =
            crate::model::Workspace::new_ssh(PathBuf::from(r"C:\local-only"), config, None)
                .unwrap();
        let terminal = workspace.active();
        let pane = workspace.focused;
        let known = "/현재/한 & 😀";
        assert!(set_remote_cwd(
            &mut workspace.root,
            pane,
            terminal,
            Some(known.into())
        ));
        assert_eq!(display_cwd(&workspace), Some(known));
        let browser = PaneSurface::browser("Preview", "https://example.test".into());
        let browser_id = browser.id;
        workspace.root.add_surface_to_leaf(pane, browser.clone());
        workspace.root.set_active_surface(pane, browser_id);
        assert_eq!(display_cwd(&workspace), Some(known));
        let browser = PaneSurface::browser("Other preview", "https://example.test".into());
        let browser_id = browser.id;
        let browser_pane = workspace
            .root
            .split_leaf(
                pane,
                SplitDirection::Vertical,
                0.5,
                PaneContent::Tabs {
                    active: browser_id,
                    surfaces: vec![browser.clone()],
                },
            )
            .unwrap();
        workspace.focused = browser_pane;
        assert_eq!(display_cwd(&workspace), Some(known));
        workspace.root = Pane::Leaf {
            id: browser_pane,
            content: PaneContent::Tabs {
                active: browser_id,
                surfaces: vec![browser],
            },
        };
        assert_eq!(display_cwd(&workspace), Some("/설정/한 & 😀"));
        workspace.ssh.as_mut().unwrap().cwd = None;
        assert_eq!(display_cwd(&workspace), None);
        assert_eq!(workspace.cwd, PathBuf::from(r"C:\local-only"));
        workspace.ssh = None;
        assert_eq!(display_cwd(&workspace), None);
    }

    #[test]
    fn direct_argv_preserves_unicode_paths_and_quotes_only_the_remote_command() {
        let mut config = config();
        let remote = "/작업/한 😀 & 'quoted' $(literal)";
        let identity = r"C:\키 한 😀\private key";
        let ssh_config = r"C:\사용자\ssh config";
        config.cwd = Some(remote.into());
        config.target.port = Some(2222);
        config.target.identity_file = Some(PathBuf::from(identity));
        config.target.config_file = Some(PathBuf::from(ssh_config));
        let shell = terminal_shell(&config, None, None, false).unwrap();
        assert_eq!(shell.program, "ssh");
        assert!(shell.args.windows(2).any(|args| args == ["-i", identity]));
        assert!(shell.args.windows(2).any(|args| args == ["-F", ssh_config]));
        assert!(shell.args.windows(2).any(|args| args == ["-l", "dev"]));
        assert!(shell.args.windows(2).any(|args| args == ["-p", "2222"]));
        assert_eq!(shell.args[shell.args.len() - 2], "example.test");
        assert_eq!(
            shell.args.last().unwrap(),
            "cd '/작업/한 😀 & '\\''quoted'\\'' $(literal)' || exit; exec \"${SHELL:-/bin/sh}\" -l"
        );
        assert!(!shell.args.iter().any(|arg| arg.contains("ControlMaster")
            || arg == "/dev/null"
            || arg.contains("/bin/false")));
        assert_eq!(config.cwd.as_deref(), Some(remote));
        let changed = terminal_shell(&config, Some("/next"), None, false).unwrap();
        assert!(changed
            .args
            .last()
            .unwrap()
            .starts_with("cd '/next' || exit; "));
    }

    #[test]
    fn rejects_invalid_targets_remote_directories_and_oversized_commands() {
        for host in ["-oProxyCommand=bad", "host name", "host\nnext"] {
            let mut config = config();
            config.target.host = host.into();
            assert!(terminal_shell(&config, None, None, false).is_err());
        }
        for cwd in ["relative", "C:\\local", "/line\nnext", "/nul\0end"] {
            assert!(terminal_shell(&config(), Some(cwd), None, false).is_err());
        }
        let mut forwarding = config();
        forwarding.forwards.push(SshForwardSpec {
            id: uuid::Uuid::new_v4(),
            remote_port: 3000,
            local_port: None,
            https: false,
        });
        let shell = terminal_shell(&forwarding, None, None, false).unwrap();
        // Forward workers own their listeners; terminal tabs never duplicate them.
        assert!(!shell.args.iter().any(|arg| arg == "-L"));
        forwarding.forwards[0].remote_port = 0;
        assert!(terminal_shell(&forwarding, None, None, false).is_err());
        assert!(terminal_shell(
            &config(),
            Some(&format!("/{}", "x".repeat(32700))),
            None,
            false
        )
        .is_err());
    }

    #[test]
    fn forwarding_argv_preserves_target_and_limits_binding_to_exact_loopback_ports() {
        let mut target = SshTarget::parse("dev@[::1]").unwrap();
        target.port = Some(2222);
        let identity = r"C:\키 한 😀\private 'key'";
        let config_path = r"C:\한글\config & file";
        target.identity_file = Some(PathBuf::from(identity));
        target.config_file = Some(PathBuf::from(config_path));
        let mut spec = SshForwardSpec {
            id: uuid::Uuid::new_v4(),
            remote_port: 65535,
            local_port: None,
            https: true,
        };
        let shell = forwarding_shell(&target, &spec, 49152).unwrap();
        assert_eq!(shell.program, "ssh");
        assert_eq!(&shell.args[..2], ["-N", "-T"]);
        for pair in [
            ["-i", identity],
            ["-F", config_path],
            ["-l", "dev"],
            ["-p", "2222"],
            ["-o", "ExitOnForwardFailure=yes"],
            ["-o", "PermitLocalCommand=no"],
            ["-o", "RemoteCommand=none"],
            ["-L", "127.0.0.1:49152:127.0.0.1:65535"],
        ] {
            assert!(shell.args.windows(2).any(|args| args == pair));
        }
        assert_eq!(shell.args.last().unwrap(), "::1");
        assert!(!shell
            .args
            .iter()
            .any(|arg| arg == "-tt" || arg.contains("ControlMaster")));
        spec.https = false; // HTTPS affects previews, not SSH transport arguments.
        assert_eq!(forwarding_shell(&target, &spec, 49152).unwrap(), shell);
        assert!(forwarding_shell(&target, &spec, 0).is_err());
        spec.local_port = Some(49151);
        assert!(forwarding_shell(&target, &spec, 49152).is_err());
        spec.local_port = Some(49152);
        assert_eq!(forwarding_shell(&target, &spec, 49152).unwrap(), shell);
        spec.remote_port = 0;
        assert!(forwarding_shell(&target, &spec, 49152).is_err());
        spec.remote_port = 80;
        spec.local_port = Some(0);
        assert!(forwarding_shell(&target, &spec, 49152).is_err());
        spec.local_port = None;
        target.host = "-oProxyCommand=bad".into();
        assert!(forwarding_shell(&target, &spec, 49152).is_err());
    }

    #[test]
    fn tmux_uses_a_stable_validated_session_for_new_and_restored_tabs() {
        let mut config = config();
        config.tmux = true;
        config.cwd = Some("/remote/한글 'folder'".into());
        let session = "flowmux-50b631aa8fdf47d695081721fdc26c63";
        let first = terminal_shell(&config, None, Some(session), false).unwrap();
        let restored: SshWorkspaceConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(first.args.last().unwrap(),
            "exec tmux -u new-session -s 'flowmux-50b631aa8fdf47d695081721fdc26c63' -c '/remote/한글 '\\''folder'\\''' 'exec \"${SHELL:-/bin/sh}\" -l'");
        let reconnect = terminal_shell(
            &restored,
            Some("/missing/한 'old cwd'"),
            Some(session),
            true,
        )
        .unwrap();
        assert_eq!(
            reconnect.args.last().unwrap(),
            "exec tmux -u attach-session -t 'flowmux-50b631aa8fdf47d695081721fdc26c63'"
        );
        assert_eq!(
            &reconnect.args[..reconnect.args.len() - 1],
            &first.args[..first.args.len() - 1]
        );
        assert!(terminal_shell(&config, None, None, false).is_err());
        for invalid in [
            "flowmux-",
            "flowmux-not-a-uuid",
            "other-50b631aa8fdf47d695081721fdc26c63",
            "flowmux-50b631aa8fdf47d695081721fdc26c63;exit",
        ] {
            for attach in [false, true] {
                assert!(terminal_shell(&config, None, Some(invalid), attach).is_err());
            }
        }
        config.tmux = false;
        assert!(terminal_shell(&config, None, Some(session), true).is_err());
        // A plain SSH reconnect still starts a login shell and honors remote cwd.
        assert_eq!(
            terminal_shell(&config, None, None, true).unwrap(),
            terminal_shell(&config, None, None, false).unwrap()
        );
    }
}
