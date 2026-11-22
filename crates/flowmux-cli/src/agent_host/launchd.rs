// SPDX-License-Identifier: GPL-3.0-or-later
//! Explicit, launchd-owned startup. No GUI child or automatic restart.

use anyhow::Context;
use std::path::Path;

fn job(socket: &Path, daemon: &Path) -> anyhow::Result<serde_json::Value> {
    use std::fmt::Write;
    use std::os::unix::ffi::OsStrExt;

    anyhow::ensure!(socket.is_absolute(), "Agent Host socket must be absolute");
    let bytes = socket.as_os_str().as_bytes();
    anyhow::ensure!(bytes.len() < 104, "Agent Host socket path is too long");
    let socket = socket.to_str().context("Agent Host socket must be UTF-8")?;
    let daemon = daemon.canonicalize()?;
    let daemon = daemon.to_str().context("Agent Host path must be UTF-8")?;
    let mut label = "com.flowmux.agent-host-".to_string();
    for byte in bytes {
        write!(label, "{byte:02x}")?;
    }
    let mut environment = serde_json::Map::new();
    for name in super::HOST_ENVIRONMENT_KEYS {
        if let Some(value) = std::env::var_os(name) {
            environment.insert(
                (*name).into(),
                value
                    .to_str()
                    .with_context(|| format!("{name} must be UTF-8 for launchd"))?
                    .into(),
            );
        }
    }
    Ok(serde_json::json!({
        "Label": label,
        "ProgramArguments": [daemon, "--agent-host", "--require-agent-host-protocol",
            &flowmux_ipc::agent_host::PROTOCOL_VERSION.to_string(), "--socket", socket],
        "EnvironmentVariables": environment,
        "WorkingDirectory": "/",
        "Umask": 63,
        "ProcessType": "Interactive",
        "RunAtLoad": true,
        "KeepAlive": false,
        // Empty Hosts retire immediately; explicit relaunch must not wait ten seconds.
        "ThrottleInterval": 0,
    }))
}

/// `launchctl print` reports `state = running` for a live job.
#[cfg(any(target_os = "macos", test))]
fn job_is_running(print: &str) -> bool {
    print.lines().any(|line| line.trim() == "state = running")
}

#[cfg(any(target_os = "macos", test))]
fn job_file(job: &serde_json::Value) -> anyhow::Result<tempfile::NamedTempFile> {
    // launchctl requires a regular, privately owned .plist file.
    let file = tempfile::Builder::new()
        .prefix("flowmux-agent-host-")
        .suffix(".plist")
        .tempfile()?;
    serde_json::to_writer(file.as_file(), job)?;
    Ok(file)
}

#[cfg(target_os = "macos")]
async fn output(command: &mut tokio::process::Command) -> anyhow::Result<std::process::Output> {
    command
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null());
    tokio::time::timeout(std::time::Duration::from_secs(5), command.output())
        .await
        .context("Agent Host service startup timed out; no agent was launched")?
        .context("Cannot start Agent Host through launchd")
}

#[cfg(target_os = "macos")]
pub(super) async fn start(socket: &Path, daemon: &Path) -> anyhow::Result<std::process::Output> {
    let job = job(socket, daemon)?;
    let label = job["Label"].as_str().unwrap();
    let file = job_file(&job)?;
    let converted = output(
        tokio::process::Command::new("/usr/bin/plutil")
            .args(["-convert", "xml1"])
            .arg(file.path()),
    )
    .await?;
    anyhow::ensure!(
        converted.status.success(),
        "Cannot encode Agent Host launchd job"
    );
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let submitted = output(
        tokio::process::Command::new("/bin/launchctl")
            .arg("bootstrap")
            .arg(&domain)
            .arg(file.path()),
    )
    .await?;
    if submitted.status.success() {
        return Ok(submitted);
    }
    // launchd retains an exited job. Explicit launch may start it again, but
    // kickstart without -k never replaces a running Host, including a racer.
    if super::existing(socket).await?.is_some() {
        return Ok(submitted);
    }
    // The retained job still names the daemon path of its first submission.
    // Replace it only while it is not running, so a racer's Host survives.
    let target = format!("{domain}/{label}");
    let retained =
        output(tokio::process::Command::new("/bin/launchctl").args(["print", &target])).await?;
    if retained.status.success() && !job_is_running(&String::from_utf8_lossy(&retained.stdout)) {
        let _ =
            output(tokio::process::Command::new("/bin/launchctl").args(["bootout", &target])).await;
        let resubmitted = output(
            tokio::process::Command::new("/bin/launchctl")
                .arg("bootstrap")
                .arg(&domain)
                .arg(file.path()),
        )
        .await?;
        if resubmitted.status.success() {
            return Ok(resubmitted);
        }
    }
    let mut restarted = output(
        tokio::process::Command::new("/bin/launchctl")
            .arg("kickstart")
            .arg(format!("{domain}/{label}")),
    )
    .await
    .with_context(|| {
        format!(
            "launchctl bootstrap failed ({}): {}",
            submitted.status,
            String::from_utf8_lossy(&submitted.stderr).trim()
        )
    })?;
    if !restarted.status.success() {
        restarted.stderr = format!(
            "launchctl bootstrap ({}): {}\nlaunchctl kickstart ({}): {}",
            submitted.status,
            String::from_utf8_lossy(&submitted.stderr).trim(),
            restarted.status,
            String::from_utf8_lossy(&restarted.stderr).trim()
        )
        .into_bytes();
    }
    Ok(restarted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_running_job_is_kept() {
        assert!(job_is_running("x = {\n\tstate = running\n\tpid = 4\n}"));
        assert!(!job_is_running("x = {\n\tstate = not running\n}"));
        assert!(!job_is_running("x = {\n\tlast exit code = 0\n}"));
    }

    #[test]
    fn launchd_submission_file_has_private_permissions_and_plist_suffix() {
        use std::os::unix::fs::PermissionsExt;

        let value = job(Path::new("/tmp/agent-host.sock"), Path::new("/bin/sh")).unwrap();
        let file = job_file(&value).unwrap();
        assert_eq!(file.path().extension().unwrap(), "plist");
        assert!(!file
            .path()
            .file_name()
            .unwrap()
            .as_encoded_bytes()
            .starts_with(b"."));
        assert_eq!(
            file.as_file().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::from_reader::<_, serde_json::Value>(file.reopen().unwrap()).unwrap(),
            value
        );
        let path = file.path().to_owned();
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn launchd_job_is_literal_private_and_does_not_restart_automatically() {
        let socket = Path::new("/tmp/한글 ' $HOME & <host>.sock");
        let value = job(socket, Path::new("/bin/sh")).unwrap();
        assert_eq!(
            value["ProgramArguments"][0],
            Path::new("/bin/sh")
                .canonicalize()
                .unwrap()
                .to_str()
                .unwrap()
        );
        let arguments = value["ProgramArguments"].as_array().unwrap();
        assert_eq!(arguments.last().unwrap(), socket.to_str().unwrap());
        assert_eq!(arguments[2], "--require-agent-host-protocol");
        assert_eq!(
            arguments[3],
            flowmux_ipc::agent_host::PROTOCOL_VERSION.to_string()
        );
        assert_eq!(value["RunAtLoad"], true);
        assert_eq!(value["KeepAlive"], false);
        assert_eq!(value["ThrottleInterval"], 0);
        assert_eq!(value["Umask"], 0o077);
        assert_eq!(value["WorkingDirectory"], "/");
        assert!(value["EnvironmentVariables"]
            .as_object()
            .unwrap()
            .keys()
            .all(|key| { super::super::HOST_ENVIRONMENT_KEYS.contains(&key.as_str()) }));
        assert_eq!(
            value["Label"],
            job(socket, Path::new("/bin/sh")).unwrap()["Label"]
        );
        assert_ne!(
            value["Label"],
            job(Path::new("/tmp/other.sock"), Path::new("/bin/sh")).unwrap()["Label"]
        );
        assert!(job(Path::new("relative.sock"), Path::new("/bin/sh")).is_err());
        assert!(job(
            Path::new(&format!("/{}", "a".repeat(104))),
            Path::new("/bin/sh")
        )
        .is_err());
    }
}
