// SPDX-License-Identifier: GPL-3.0-or-later
//! Direct Windows OpenSSH sessions. Local argv and remote POSIX quoting remain
//! separate; no Unix control socket or arbitrary command is accepted here.

use crate::shell::Shell;
use anyhow::ensure;
use flowmux_core::{
    ssh::{shell_quote, validate_remote_cwd},
    SshWorkspaceConfig,
};

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
) -> anyhow::Result<Shell> {
    config.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        config.forwards.is_empty(),
        "SSH port forwarding is not implemented on Windows"
    );
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
        // -A attaches to the persisted session if it survives. Keep cwd in -c
        // rather than an earlier cd, so reattachment does not depend on that
        // directory still existing. A missing tmux/server is reported in the PTY.
        let directory = cwd
            .map(|path| format!(" -c {}", shell_quote(path)))
            .unwrap_or_default();
        format!(
            "exec tmux -u new-session -A -s {}{directory} {}",
            shell_quote(session),
            shell_quote(login)
        )
    } else {
        let directory = cwd
            .map(|path| format!("cd {} || exit; ", shell_quote(path)))
            .unwrap_or_default();
        format!("{directory}{login}")
    };
    let mut args = vec!["-tt".into()];
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
    let target = &config.target;
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
    args.extend([target.host.clone(), command]);
    let shell = Shell {
        program: "ssh".into(),
        args,
    };
    shell.validate()?;
    Ok(shell)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowmux_core::{SshForwardSpec, SshTarget};
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
    fn direct_argv_preserves_unicode_paths_and_quotes_only_the_remote_command() {
        let mut config = config();
        let remote = "/작업/한 😀 & 'quoted' $(literal)";
        let identity = r"C:\키 한 😀\private key";
        let ssh_config = r"C:\사용자\ssh config";
        config.cwd = Some(remote.into());
        config.target.port = Some(2222);
        config.target.identity_file = Some(PathBuf::from(identity));
        config.target.config_file = Some(PathBuf::from(ssh_config));
        let shell = terminal_shell(&config, None, None).unwrap();
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
        let changed = terminal_shell(&config, Some("/next"), None).unwrap();
        assert!(changed
            .args
            .last()
            .unwrap()
            .starts_with("cd '/next' || exit; "));
    }

    #[test]
    fn rejects_invalid_targets_remote_directories_forwarding_and_oversized_commands() {
        for host in ["-oProxyCommand=bad", "host name", "host\nnext"] {
            let mut config = config();
            config.target.host = host.into();
            assert!(terminal_shell(&config, None, None).is_err());
        }
        for cwd in ["relative", "C:\\local", "/line\nnext", "/nul\0end"] {
            assert!(terminal_shell(&config(), Some(cwd), None).is_err());
        }
        let mut forwarding = config();
        forwarding.forwards.push(SshForwardSpec {
            id: uuid::Uuid::new_v4(),
            remote_port: 3000,
            local_port: None,
            https: false,
        });
        assert!(terminal_shell(&forwarding, None, None)
            .unwrap_err()
            .to_string()
            .contains("not implemented"));
        assert!(terminal_shell(&config(), Some(&format!("/{}", "x".repeat(32700))), None).is_err());
    }

    #[test]
    fn tmux_uses_a_stable_validated_session_for_new_and_restored_tabs() {
        let mut config = config();
        config.tmux = true;
        config.cwd = Some("/remote/한글 'folder'".into());
        let session = "flowmux-50b631aa8fdf47d695081721fdc26c63";
        let first = terminal_shell(&config, None, Some(session)).unwrap();
        let restored: SshWorkspaceConfig =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(
            terminal_shell(&restored, None, Some(session)).unwrap(),
            first
        );
        assert_eq!(first.args.last().unwrap(),
            "exec tmux -u new-session -A -s 'flowmux-50b631aa8fdf47d695081721fdc26c63' -c '/remote/한글 '\\''folder'\\''' 'exec \"${SHELL:-/bin/sh}\" -l'");
        assert!(terminal_shell(&config, None, None).is_err());
        for invalid in [
            "flowmux-",
            "flowmux-not-a-uuid",
            "other-50b631aa8fdf47d695081721fdc26c63",
            "flowmux-50b631aa8fdf47d695081721fdc26c63;exit",
        ] {
            assert!(terminal_shell(&config, None, Some(invalid)).is_err());
        }
        config.tmux = false;
        assert!(terminal_shell(&config, None, Some(session)).is_err());
    }
}
