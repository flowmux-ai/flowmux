// SPDX-License-Identifier: GPL-3.0-or-later
//! Persisted SSH configuration and deterministic OpenSSH argument construction.
//! No network or local filesystem access: remote paths remain strings.
//! Remote bootstraps require a Unix host with a POSIX-compatible login shell;
//! fish and csh login shells are not supported.

use crate::{PaneSurface, SurfaceId, SurfaceKind};
use serde::{Deserialize, Serialize};
use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkspaceLocation {
    Local { root_dir: PathBuf },
    Ssh { config: SshWorkspaceConfig },
}

impl WorkspaceLocation {
    pub fn local_root(&self) -> Option<&Path> {
        match self {
            Self::Local { root_dir } => Some(root_dir),
            Self::Ssh { .. } => None,
        }
    }

    pub fn ssh(&self) -> Option<&SshWorkspaceConfig> {
        match self {
            Self::Ssh { config } => Some(config),
            Self::Local { .. } => None,
        }
    }

    pub fn display(&self) -> String {
        match self {
            Self::Local { root_dir } => root_dir.display().to_string(),
            Self::Ssh { config } => format!(
                "{}:{}",
                config.target.destination(),
                config.cwd.as_deref().unwrap_or("~")
            ),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshTarget {
    pub host: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub identity_file: Option<PathBuf>,
    /// OpenSSH -F, useful for managed hosts and isolated test configurations.
    #[serde(default)]
    pub config_file: Option<PathBuf>,
}

impl SshTarget {
    pub fn parse(destination: &str) -> Result<Self, String> {
        let (user, host) = match destination.split_once('@') {
            Some((user, host)) => (Some(user.to_string()), host),
            None => (None, destination),
        };
        let host = if host.starts_with('[') || host.ends_with(']') {
            let address = host
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .ok_or("SSH IPv6 address must have balanced brackets")?;
            if !valid_ipv6_host(address) {
                return Err("Invalid SSH IPv6 address".into());
            }
            address
        } else {
            host
        };
        let target = Self {
            host: host.into(),
            user,
            port: None,
            identity_file: None,
            config_file: None,
        };
        target.validate()?;
        Ok(target)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.host.is_empty()
            || self.host.starts_with('-')
            || !self
                .host
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".-_:%".contains(&c))
        {
            return Err("Invalid SSH host; enter a Host alias, hostname or IP address".into());
        }
        if (self.host.contains(':') || self.host.contains('%')) && !valid_ipv6_host(&self.host) {
            return Err("Invalid SSH IPv6 address".into());
        }
        if let Some(user) = &self.user {
            if user.is_empty()
                || user.starts_with('-')
                || !user
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            {
                return Err("Invalid SSH user".into());
            }
        }
        if self.port == Some(0) {
            return Err("SSH port must be between 1 and 65535".into());
        }
        for path in [&self.identity_file, &self.config_file]
            .into_iter()
            .flatten()
        {
            if path.as_os_str().is_empty()
                || path
                    .to_str()
                    .is_none_or(|s| s.chars().any(char::is_control))
            {
                return Err("Invalid SSH configuration or identity path".into());
            }
        }
        Ok(())
    }

    pub fn destination(&self) -> String {
        self.user
            .as_ref()
            .map_or_else(|| self.host.clone(), |user| format!("{user}@{}", self.host))
    }

    pub fn master_argv(&self, socket: &Path) -> Result<Vec<String>, String> {
        self.validate()?;
        let mut args = vec![
            "ssh".into(),
            "-M".into(),
            "-N".into(),
            "-T".into(),
            "-S".into(),
            socket_text(socket)?,
        ];
        for option in [
            "ControlMaster=yes",
            "ControlPersist=no",
            "ForkAfterAuthentication=no",
            "ClearAllForwardings=yes",
            "Tunnel=no",
            "ForwardAgent=no",
            "ForwardX11=no",
            "PermitLocalCommand=no",
            "RemoteCommand=none",
            "ExitOnForwardFailure=yes",
            "ServerAliveInterval=15",
            "ServerAliveCountMax=3",
        ] {
            args.extend(["-o".into(), option.into()]);
        }
        if let Some(user) = &self.user {
            args.extend(["-l".into(), user.clone()]);
        }
        if let Some(port) = self.port {
            args.extend(["-p".into(), port.to_string()]);
        }
        if let Some(path) = &self.identity_file {
            args.extend(["-i".into(), path.to_string_lossy().into_owned()]);
        }
        if let Some(path) = &self.config_file {
            args.extend(["-F".into(), path.to_string_lossy().into_owned()]);
        }
        args.push(self.host.clone());
        Ok(args)
    }

    pub fn control_argv(&self, socket: &Path, operation: &str) -> Result<Vec<String>, String> {
        self.validate()?;
        if !["check", "exit", "forward", "cancel"].contains(&operation) {
            return Err("Invalid SSH control operation".into());
        }
        Ok(vec![
            "ssh".into(),
            "-F".into(),
            "/dev/null".into(),
            "-S".into(),
            socket_text(socket)?,
            "-O".into(),
            operation.into(),
            self.host.clone(),
        ])
    }

    pub fn terminal_argv(
        &self,
        socket: &Path,
        cwd: Option<&str>,
        tmux: Option<&str>,
        attach: bool,
        command: &[String],
    ) -> Result<Vec<String>, String> {
        self.validate()?;
        validate_remote_cwd(cwd)?;
        if command.iter().any(|arg| arg.contains('\0')) {
            return Err("SSH command contains a NUL byte".into());
        }
        let shell = if command.is_empty() {
            "exec \"${SHELL:-/bin/sh}\" -l".to_string()
        } else {
            format!(
                "{}; exec \"${{SHELL:-/bin/sh}}\" -l",
                command
                    .iter()
                    .map(|s| shell_quote(s))
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let cd = cwd
            .map(|s| format!("cd {} || exit; ", shell_quote(s)))
            .unwrap_or_default();
        // VTE speaks UTF-8 even when the remote locale makes tmux assume ASCII.
        let bootstrap = if let Some(session) = tmux {
            if !session.starts_with("flowmux-")
                || !session
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
            {
                return Err("Invalid flowmux tmux session".into());
            }
            if attach {
                format!("exec tmux -u attach-session -t {}", shell_quote(session))
            } else {
                let dir = cwd
                    .map(|s| format!(" -c {}", shell_quote(s)))
                    .unwrap_or_default();
                format!(
                    "{cd}exec tmux -u new-session -s {}{dir} {}",
                    shell_quote(session),
                    shell_quote(&shell)
                )
            }
        } else {
            format!("{cd}{shell}")
        };
        Ok(vec![
            "ssh".into(),
            "-F".into(),
            "/dev/null".into(),
            "-S".into(),
            socket_text(socket)?,
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "ProxyCommand=/bin/false".into(),
            "-o".into(),
            "ForwardAgent=no".into(),
            "-o".into(),
            "ForwardX11=no".into(),
            "-tt".into(),
            self.host.clone(),
            bootstrap,
        ])
    }
}

fn valid_ipv6_host(host: &str) -> bool {
    let (address, zone) = host
        .split_once('%')
        .map_or((host, None), |(address, zone)| (address, Some(zone)));
    address.parse::<Ipv6Addr>().is_ok()
        && zone.is_none_or(|zone| {
            !zone.is_empty()
                && zone
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        })
}

fn socket_text(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or("SSH control socket path is not UTF-8")?;
    // OpenSSH expands percent tokens in ControlPath even when supplied by -S.
    // Require a literal absolute filename, within the portable Unix socket limit.
    if !path.is_absolute()
        || path.file_name().is_none()
        || text.contains('%')
        || text.chars().any(char::is_control)
    {
        return Err("SSH control socket must be an absolute path without control characters or percent tokens".into());
    }
    if text.len() >= 100 {
        return Err("SSH control socket path is too long".into());
    }
    Ok(text.into())
}

pub fn validate_remote_cwd(cwd: Option<&str>) -> Result<(), String> {
    if cwd.is_some_and(|s| !s.starts_with('/') || s.chars().any(char::is_control)) {
        return Err(
            "Remote directory must be an absolute POSIX path, or empty for login home".into(),
        );
    }
    Ok(())
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshWorkspaceConfig {
    pub target: SshTarget,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub tmux: bool,
    #[serde(default)]
    pub forwards: Vec<SshForwardSpec>,
}

impl SshWorkspaceConfig {
    pub fn validate(&self) -> Result<(), String> {
        self.target.validate()?;
        validate_remote_cwd(self.cwd.as_deref())?;
        for forward in &self.forwards {
            forward.validate()?;
        }
        Ok(())
    }

    pub fn terminal(&self, cwd: Option<String>) -> PaneSurface {
        let id = SurfaceId::new();
        PaneSurface {
            id,
            title: self.target.destination(),
            title_locked: false,
            kind: SurfaceKind::SshTerminal {
                cwd: cwd.or_else(|| self.cwd.clone()),
                tmux_session: self.tmux.then(|| format!("flowmux-{}", id.0.simple())),
            },
            scrollback: None,
            agent: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SshForwardSpec {
    pub id: uuid::Uuid,
    pub remote_port: u16,
    pub local_port: Option<u16>,
    #[serde(default)]
    pub https: bool,
}

impl SshForwardSpec {
    pub fn validate(&self) -> Result<(), String> {
        if self.remote_port == 0 || self.local_port == Some(0) {
            return Err("Forward ports must be between 1 and 65535".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipv6_brackets_and_socket_paths_are_validated() {
        for (input, host) in [
            ("dev@[::1]", "::1"),
            ("[fe80::1%eth0]", "fe80::1%eth0"),
            ("2001:db8::1", "2001:db8::1"),
        ] {
            assert_eq!(SshTarget::parse(input).unwrap().host, host);
        }
        for input in [
            "[::1",
            "::1]",
            "[[::1]]",
            "[devbox]",
            "[::1]:22",
            "devbox:22",
            "fe80::1%",
            "fe80::1%eth0%bad",
        ] {
            assert!(SshTarget::parse(input).is_err(), "{input}");
        }
        let target = SshTarget::parse("devbox").unwrap();
        for path in [
            "",
            "none",
            "relative.sock",
            "/",
            "/tmp/%h.sock",
            "/tmp/bad\n.sock",
            "/tmp/bad\0.sock",
        ] {
            assert!(target.master_argv(Path::new(path)).is_err(), "{path:?}");
            assert!(
                target.control_argv(Path::new(path), "check").is_err(),
                "{path:?}"
            );
            assert!(
                target
                    .terminal_argv(Path::new(path), None, None, false, &[])
                    .is_err(),
                "{path:?}"
            );
        }
        assert!(socket_text(Path::new(&format!("/{}", "x".repeat(99)))).is_err());
        assert_eq!(
            socket_text(Path::new("/tmp/with space.sock")).unwrap(),
            "/tmp/with space.sock"
        );
        let master = target.master_argv(Path::new("/tmp/master.sock")).unwrap();
        for option in [
            "ForkAfterAuthentication=no",
            "Tunnel=no",
            "ClearAllForwardings=yes",
            "ControlPersist=no",
        ] {
            assert!(master.iter().any(|arg| arg == option));
        }
    }

    #[cfg(unix)]
    #[test]
    fn remote_bootstrap_preserves_literal_arguments_and_rejects_missing_cwd() {
        let target = SshTarget::parse("devbox").unwrap();
        let socket = Path::new("/tmp/master.sock");
        let literals = [
            "a ' quoted",
            "$(printf injected)",
            "`printf injected`",
            "",
            "line\nbreak",
        ];
        let mut command = vec!["printf".into(), "<%s>".into()];
        command.extend(literals.iter().map(|s| s.to_string()));
        let argv = target
            .terminal_argv(socket, Some("/"), None, false, &command)
            .unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", argv.last().unwrap()])
            .env("SHELL", "/bin/true")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            literals
                .iter()
                .map(|s| format!("<{s}>"))
                .collect::<String>()
        );
        let missing = format!("/flowmux-missing-{}", uuid::Uuid::new_v4());
        for tmux in [None, Some("flowmux-test")] {
            let argv = target
                .terminal_argv(socket, Some(&missing), tmux, false, &command)
                .unwrap();
            let output = std::process::Command::new("/bin/sh")
                .args(["-c", argv.last().unwrap()])
                .env("SHELL", "/bin/true")
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(
                output.stdout.is_empty(),
                "command must not execute after failed cd"
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains(&missing));
        }
    }

    #[test]
    fn ssh_arguments_preserve_paths_and_never_fall_back_to_direct_connection() {
        let target = SshTarget::parse("me@devbox").unwrap();
        let argv = target
            .terminal_argv(
                Path::new("/tmp/mux.sock"),
                Some("/work/a ' $(touch nope) `id`"),
                None,
                false,
                &[],
            )
            .unwrap();
        assert!(argv.contains(&"ProxyCommand=/bin/false".into()));
        assert_eq!(
            argv.last().unwrap(),
            "cd '/work/a '\\'' $(touch nope) `id`' || exit; exec \"${SHELL:-/bin/sh}\" -l"
        );
        for host in [
            "-oProxyCommand=evil",
            "a b",
            "a\nHost *",
            "user@-x",
            "a@b@c",
        ] {
            assert!(SshTarget::parse(host).is_err(), "{host}");
        }
        assert!(target
            .terminal_argv(Path::new("/tmp/x"), Some("~/work"), None, false, &[])
            .is_err());
        let attach = target
            .terminal_argv(
                Path::new("/tmp/x"),
                None,
                Some("flowmux-test"),
                true,
                &["must-not-run".into()],
            )
            .unwrap();
        assert_eq!(
            attach.last().unwrap(),
            "exec tmux -u attach-session -t 'flowmux-test'"
        );
    }
}
