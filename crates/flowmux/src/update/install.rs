// SPDX-License-Identifier: GPL-3.0-or-later
//! Tokio side of self-update: the periodic release check and the
//! install runner executing the [`check`] command plan. This layer is
//! deliberately thin — every decision (version compare, command
//! argv, script name) comes from the unit-tested [`check`] module.

use super::check::{self, Version};
use super::origin::{self, InstallOrigin, UpdateGate};
use super::{Event, Stage};
use anyhow::Context;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};

const REPO_URL: &str = "https://github.com/flowmux-ai/flowmux-terminal.git";
const CHECK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
struct Progress {
    stage: Stage,
    percent: Arc<AtomicU8>,
    tx: async_channel::Sender<Event>,
}

impl Progress {
    fn new(stage: Stage, tx: &async_channel::Sender<Event>) -> Self {
        Self {
            stage,
            percent: Arc::new(AtomicU8::new(0)),
            tx: tx.clone(),
        }
    }

    fn report(&self, percent: u8) {
        let percent = percent.min(100);
        let mut current = self.percent.load(Ordering::Relaxed);
        loop {
            if percent <= current {
                return;
            }
            match self.percent.compare_exchange_weak(
                current,
                percent,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    let _ = self.tx.try_send(Event::Progress(self.stage, percent));
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }

    fn observe(&self, line: &str) {
        let current = self.percent.load(Ordering::Relaxed);
        if let Some(percent) = output_progress(self.stage, line, current) {
            self.report(percent);
        }
    }
}

fn parsed_percent(line: &str, marker: &str) -> Option<u8> {
    let after_marker = line.split_once(marker)?.1;
    let before_percent = after_marker.split_once('%')?.0;
    before_percent
        .split_whitespace()
        .last()?
        .parse::<u8>()
        .ok()
        .map(|percent| percent.min(100))
}

fn output_progress(stage: Stage, line: &str, current: u8) -> Option<u8> {
    match stage {
        Stage::Fetching => [
            ("Counting objects:", 0, 5),
            ("Compressing objects:", 5, 5),
            ("Receiving objects:", 10, 80),
            ("Resolving deltas:", 90, 9),
            ("Updating files:", 90, 9),
        ]
        .into_iter()
        .find_map(|(marker, start, span)| {
            parsed_percent(line, marker)
                .map(|percent| start + ((u16::from(percent) * span) / 100) as u8)
        }),
        Stage::Installing => {
            let line = line.trim_start();
            if line.contains("==> building flowmux") {
                Some(5)
            } else if line.starts_with("Compiling ") || line.starts_with("Checking ") {
                Some(current.saturating_add(2).min(74))
            } else if line.starts_with("Finished ") {
                Some(75)
            } else if line.contains("==> creating ") {
                Some(82)
            } else if line.contains("==> installed to ") {
                Some(85)
            } else if line.contains("==> installed desktop entry")
                || line.contains("==> staging app update")
                || line.contains("==> installing app")
            {
                Some(90)
            } else if line.contains("==> installed icons")
                || line.contains("==> installed CLI:")
                || line.contains("==> installed:")
            {
                Some(95)
            } else if line.contains("==> done.")
                || line.contains("==> restart FlowMux")
                || line.contains("==> launch with:")
            {
                Some(100)
            } else {
                None
            }
        }
    }
}

fn copy_output(
    mut output: impl Read,
    mut log: std::fs::File,
    progress: Progress,
) -> std::io::Result<()> {
    let mut buffer = [0_u8; 8192];
    let mut line = Vec::new();
    loop {
        let count = output.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        log.write_all(&buffer[..count])?;
        for &byte in &buffer[..count] {
            if byte == b'\n' || byte == b'\r' {
                if !line.is_empty() {
                    progress.observe(&String::from_utf8_lossy(&line));
                    line.clear();
                }
            } else {
                line.push(byte);
            }
        }
    }
    if !line.is_empty() {
        progress.observe(&String::from_utf8_lossy(&line));
    }
    Ok(())
}

/// Managed source checkout, independent of any user clone.
fn clone_dir() -> Option<PathBuf> {
    flowmux_config::paths::host_visible_cache_dir().map(|d| d.join("src"))
}

/// Combined log of the last update attempt (git + install script).
pub fn log_path() -> Option<PathBuf> {
    flowmux_config::paths::host_visible_cache_dir().map(|d| d.join("update.log"))
}

pub fn record_release_page_decision(origin: InstallOrigin, version: Version) {
    let Some(path) = log_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    if let Ok(mut log) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            log,
            "install_origin={origin:?} update_action=release_page version={version}"
        );
    }
}

/// Check for a newer release now and then every 24 h, announcing hits
/// on `tx`. Failures (offline, no git) are logged and stay silent —
/// the banner simply never appears.
pub async fn check_loop(tx: async_channel::Sender<Event>) {
    let mut tick = tokio::time::interval(CHECK_INTERVAL);
    loop {
        tick.tick().await; // first tick fires immediately = startup check
        match check_once().await {
            Ok(Some(latest)) => {
                if tx.send(Event::Available(latest)).await.is_err() {
                    return; // banner gone, window closing
                }
            }
            Ok(None) => {
                if tx.send(Event::Current).await.is_err() {
                    return; // banner gone, window closing
                }
            }
            Err(e) => tracing::warn!(error = %e, "release check failed"),
        }
    }
}

pub(crate) async fn check_once() -> anyhow::Result<Option<Version>> {
    // std::process on the blocking pool, not tokio::process — GLib's
    // child watch owns SIGCHLD in the GUI process, so tokio's child
    // wait never wakes on macOS (see flowmux-vcs `git_output`).
    let output = tokio::task::spawn_blocking(|| {
        std::process::Command::new("git")
            .args(["ls-remote", "--tags", REPO_URL])
            .stdin(Stdio::null())
            .output()
    })
    .await
    .context("join ls-remote task")?
    .context("run git ls-remote")?;
    if !output.status.success() {
        anyhow::bail!(
            "git ls-remote failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let versions = check::parse_ls_remote(&String::from_utf8_lossy(&output.stdout));
    Ok(check::latest(&versions)
        .filter(|latest| check::update_available(env!("CARGO_PKG_VERSION"), *latest)))
}

/// Bring the managed clone to `version` and run the platform install
/// script, reporting progress and the final outcome on `tx`.
pub async fn run_install(version: Version, tx: async_channel::Sender<Event>) {
    let outcome = run_install_inner(version, &tx).await;
    let event = match outcome {
        Ok(()) => Event::Done(version),
        Err(e) => {
            tracing::warn!(error = %e, "self-update failed");
            Event::Failed(format!("{e:#}"))
        }
    };
    let _ = tx.send(event).await;
}

async fn run_install_inner(
    version: Version,
    tx: &async_channel::Sender<Event>,
) -> anyhow::Result<()> {
    let dir = clone_dir().context("HOME is unset")?;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).context("create cache dir")?;
    }
    let mut log =
        std::fs::File::create(log_path().context("HOME is unset")?).context("create update.log")?;
    let origin = origin::install_origin();
    let gate = origin::update_gate(origin);
    writeln!(
        log,
        "install_origin={origin:?} update_action={gate:?} version={version}"
    )
    .context("write update decision")?;
    if gate != UpdateGate::SourceBuild {
        anyhow::bail!(
            "source self-update is disabled for {origin:?} installs; use the release page"
        );
    }

    // A launcher-started GUI has no ~/.cargo/bin on PATH, so use the same
    // adjusted PATH for prerequisite checks and the eventual install script.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = check::install_path_env(
        home.as_deref(),
        &std::env::var_os("PATH").unwrap_or_default(),
        std::env::consts::OS,
    );
    let prerequisite_path = path.clone();
    let prerequisites = tokio::task::spawn_blocking(move || {
        check_prerequisites(std::env::consts::OS, prerequisite_path.as_deref())
    })
    .await
    .context("join prerequisite check")?;
    if let Err(message) = prerequisites {
        writeln!(log, "prerequisites=missing detail={message}")
            .context("write prerequisite failure")?;
        anyhow::bail!(message);
    }
    writeln!(log, "prerequisites=ok").context("write prerequisite result")?;

    let _ = tx.send(Event::Progress(Stage::Fetching, 0)).await;
    let fetching = Progress::new(Stage::Fetching, tx);
    let tag = version.tag();
    let clone_exists = dir.join(".git").is_dir();
    if run_plan(
        check::git_plan(clone_exists, REPO_URL, &dir, &tag),
        &log,
        fetching.clone(),
    )
    .await
    .is_err()
        && clone_exists
    {
        // A stale or corrupt managed clone must not block the update:
        // wipe it and retry once from a fresh shallow clone.
        std::fs::remove_dir_all(&dir).context("reset managed clone")?;
        run_plan(
            check::git_plan(false, REPO_URL, &dir, &tag),
            &log,
            fetching.clone(),
        )
        .await?;
    }
    fetching.report(100);

    let _ = tx.send(Event::Progress(Stage::Installing, 0)).await;
    let installing = Progress::new(Stage::Installing, tx);
    let script = check::install_script(std::env::consts::OS);
    run_logged(
        vec!["bash".to_string(), script.to_string()],
        Some(dir),
        path.map(|p| ("PATH", p)),
        &log,
        installing.clone(),
    )
    .await?;
    installing.report(100);
    Ok(())
}

fn prerequisite_packages(os: &str) -> &'static [&'static str] {
    if os == "macos" {
        &["gtk4", "libadwaita-1", "vte-2.91-gtk4"]
    } else {
        &["gtk4", "libadwaita-1", "vte-2.91-gtk4", "webkitgtk-6.0"]
    }
}

fn check_prerequisites(os: &str, path: Option<&std::ffi::OsStr>) -> Result<(), String> {
    let mut missing_tools = Vec::new();
    for tool in ["cargo", "rustc"] {
        if !command_succeeds(tool, &["--version"], path) {
            missing_tools.push(tool);
        }
    }

    let mut missing_packages = Vec::new();
    if !command_succeeds("pkg-config", &["--version"], path) {
        missing_tools.push("pkg-config");
    } else {
        for &package in prerequisite_packages(os) {
            if !command_succeeds("pkg-config", &["--exists", package], path) {
                missing_packages.push(package);
            }
        }
    }
    prerequisite_error(&missing_tools, &missing_packages).map_or(Ok(()), Err)
}

fn command_succeeds(program: &str, args: &[&str], path: Option<&std::ffi::OsStr>) -> bool {
    let mut command = std::process::Command::new(program);
    command.args(args).stdin(Stdio::null());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn prerequisite_error(tools: &[&str], packages: &[&str]) -> Option<String> {
    if tools.is_empty() && packages.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !tools.is_empty() {
        parts.push(format!("tools: {}", tools.join(", ")));
    }
    if !packages.is_empty() {
        parts.push(format!("pkg-config packages: {}", packages.join(", ")));
    }
    Some(format!(
        "missing update prerequisites ({})",
        parts.join("; ")
    ))
}

async fn run_plan(
    plan: Vec<Vec<String>>,
    log: &std::fs::File,
    progress: Progress,
) -> anyhow::Result<()> {
    for argv in plan {
        run_logged(argv, None, None, log, progress.clone()).await?;
    }
    Ok(())
}

/// Run one command with stdout/stderr appended to the update log, so
/// a failure is diagnosable from `update.log` without re-running.
///
/// std::process on the blocking pool, not tokio::process — GLib's
/// child watch owns SIGCHLD in the GUI process, so tokio's child wait
/// never wakes on macOS (see flowmux-vcs `git_output`).
async fn run_logged(
    argv: Vec<String>,
    cwd: Option<std::path::PathBuf>,
    env: Option<(&'static str, std::ffi::OsString)>,
    log: &std::fs::File,
    progress: Progress,
) -> anyhow::Result<()> {
    let label = argv.join(" ");
    let stdout_log = log.try_clone().context("clone log handle")?;
    let stderr_log = log.try_clone().context("clone log handle")?;
    let run_label = label.clone();
    let status = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let mut cmd = std::process::Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        if let Some((key, value)) = env {
            cmd.env(key, value);
        }
        let mut child = cmd.spawn().with_context(|| format!("run {run_label}"))?;
        let stdout = child.stdout.take().context("capture command stdout")?;
        let stderr = child.stderr.take().context("capture command stderr")?;
        let stdout_progress = progress.clone();
        let stdout_reader =
            std::thread::spawn(move || copy_output(stdout, stdout_log, stdout_progress));
        let stderr_reader = std::thread::spawn(move || copy_output(stderr, stderr_log, progress));

        let status = child.wait();
        stdout_reader
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader panicked"))?
            .context("read command stdout")?;
        stderr_reader
            .join()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))?
            .context("read command stderr")?;
        status.with_context(|| format!("wait for {run_label}"))
    })
    .await
    .with_context(|| format!("join {label}"))??;
    if !status.success() {
        anyhow::bail!("{label} exited with {status}");
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::{check_prerequisites, output_progress, prerequisite_error, prerequisite_packages};
    use crate::update::Stage;
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn missing_prerequisite_message_names_tools_and_packages() {
        let message = prerequisite_error(&["cargo"], &["webkitgtk-6.0"]).unwrap();
        assert!(message.contains("cargo"), "{message}");
        assert!(message.contains("webkitgtk-6.0"), "{message}");
        assert_eq!(prerequisite_error(&[], &[]), None);
    }

    #[test]
    fn empty_path_reports_missing_build_tools_before_any_clone() {
        let message = check_prerequisites("linux", Some(OsStr::new(""))).unwrap_err();
        assert!(message.contains("cargo"), "{message}");
        assert!(message.contains("rustc"), "{message}");
        assert!(message.contains("pkg-config"), "{message}");
    }

    #[test]
    fn macos_prerequisites_use_native_webkit() {
        let packages = prerequisite_packages("macos");
        assert!(packages.contains(&"gtk4"));
        assert!(packages.contains(&"libadwaita-1"));
        assert!(packages.contains(&"vte-2.91-gtk4"));
        assert!(!packages.contains(&"webkitgtk-6.0"));
    }

    #[test]
    fn linux_prerequisites_require_webkitgtk() {
        assert!(prerequisite_packages("linux").contains(&"webkitgtk-6.0"));
    }

    #[test]
    fn command_output_maps_to_monotonic_stage_percentages() {
        assert_eq!(
            output_progress(Stage::Fetching, "Receiving objects: 50% (5/10)", 0),
            Some(50)
        );
        assert_eq!(
            output_progress(Stage::Fetching, "Resolving deltas: 100% (2/2)", 90),
            Some(99)
        );
        assert_eq!(
            output_progress(Stage::Installing, "   Compiling flowmux v0.8.0", 5),
            Some(7)
        );
        assert_eq!(
            output_progress(
                Stage::Installing,
                "Finished `fast` profile [optimized] target(s)",
                40,
            ),
            Some(75)
        );
        assert_eq!(
            output_progress(Stage::Installing, "==> installed icons to /tmp/icons", 90,),
            Some(95)
        );
    }

    #[test]
    fn adjusted_path_finds_macos_prerequisite_tools() {
        let temp = tempfile::tempdir().unwrap();
        let tool_dir = temp.path().join(".cargo/bin");
        std::fs::create_dir_all(&tool_dir).unwrap();
        for tool in ["cargo", "rustc", "pkg-config"] {
            let path = tool_dir.join(tool);
            std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let path = crate::update::check::install_path_env(
            Some(temp.path()),
            OsStr::new("/usr/bin:/bin"),
            "macos",
        )
        .unwrap();
        assert_eq!(check_prerequisites("macos", Some(&path)), Ok(()));
    }

    #[test]
    fn deferred_macos_swap_waits_for_the_running_app() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("FlowMux.app");
        let mut host = Command::new("sleep").arg("30").spawn().unwrap();
        let staged = temp
            .path()
            .join(format!(".FlowMux.app.pending.{}", host.id()));
        let backup = temp
            .path()
            .join(format!(".FlowMux.app.previous.{}", host.id()));
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(destination.join("version"), "old").unwrap();
        std::fs::write(staged.join("version"), "new").unwrap();

        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/deferred-macos-app-swap.sh");
        let mut swap = Command::new("sh")
            .arg(script)
            .arg(host.id().to_string())
            .arg(&staged)
            .arg(&destination)
            .arg(&backup)
            .spawn()
            .unwrap();

        thread::sleep(Duration::from_millis(300));
        assert_eq!(
            std::fs::read_to_string(destination.join("version")).unwrap(),
            "old"
        );
        assert!(staged.is_dir());
        assert!(swap.try_wait().unwrap().is_none());

        host.kill().unwrap();
        host.wait().unwrap();
        for _ in 0..100 {
            if let Some(status) = swap.try_wait().unwrap() {
                assert!(status.success());
                assert_eq!(
                    std::fs::read_to_string(destination.join("version")).unwrap(),
                    "new"
                );
                assert!(!staged.exists());
                assert!(!backup.exists());
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = swap.kill();
        panic!("deferred app swap did not finish");
    }
}
