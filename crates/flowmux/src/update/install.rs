// SPDX-License-Identifier: GPL-3.0-or-later
//! Tokio side of self-update: the periodic release check and installer.
//! Local Linux installs consume the verified GitHub Release tarball;
//! platforms without a release binary retain the source installer.

use super::check::{self, Version};
use super::origin::{self, InstallOrigin, UpdateGate};
use super::{Event, Stage};
use anyhow::Context;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{
    atomic::{AtomicU8, Ordering},
    Arc,
};
use tokio::io::AsyncWriteExt;

const REPO_URL: &str = "https://github.com/flowmux-ai/flowmux.git";
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/flowmux-ai/flowmux/releases/latest";
const RELEASE_DOWNLOAD_URL: &str = "https://github.com/flowmux-ai/flowmux/releases/download";
const LINUX_RELEASE_TARGET: &str = "x86_64-unknown-linux-gnu";
const RELEASE_BINARIES: [&str; 3] = ["flowmux", "flowmuxctl", "flowmux-md-viewer"];
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

fn manual_install_guidance(script: &std::path::Path) -> String {
    format!("Run `{}` in a terminal, then retry", script.display())
}

fn release_asset_name(version: Version) -> String {
    format!("flowmux-{version}-{LINUX_RELEASE_TARGET}.tar.gz")
}

fn expected_sha256(checksum: &str) -> Option<&str> {
    let value = checksum.split_whitespace().next()?;
    (value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(value)
}

fn use_release_tarball() -> bool {
    std::env::consts::OS == "linux" && std::env::consts::ARCH == "x86_64"
}

/// Combined log of the last update attempt.
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
/// on `tx`. Offline failures are logged and stay silent — the banner
/// simply never appears.
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
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }

    let release = reqwest::Client::new()
        .get(LATEST_RELEASE_URL)
        .header(
            reqwest::header::USER_AGENT,
            concat!("flowmux/", env!("CARGO_PKG_VERSION")),
        )
        .send()
        .await
        .context("check latest GitHub Release")?
        .error_for_status()
        .context("latest GitHub Release request failed")?
        .json::<Release>()
        .await
        .context("read latest GitHub Release")?;
    let latest = Version::parse(&release.tag_name)
        .with_context(|| format!("invalid release tag {}", release.tag_name))?;
    Ok(check::update_available(env!("CARGO_PKG_VERSION"), latest).then_some(latest))
}

/// Install `version`, reporting progress and the final outcome on `tx`.
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
    let log_path = log_path().context("HOME is unset")?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).context("create cache dir")?;
    }
    let mut log = std::fs::File::create(log_path).context("create update.log")?;
    let origin = origin::install_origin();
    let gate = origin::update_gate(origin);
    let action = if gate == UpdateGate::SourceBuild && use_release_tarball() {
        "ReleaseTarball".to_string()
    } else {
        format!("{gate:?}")
    };
    writeln!(
        log,
        "install_origin={origin:?} update_action={action} version={version}"
    )
    .context("write update decision")?;
    if gate != UpdateGate::SourceBuild {
        anyhow::bail!(
            "source self-update is disabled for {origin:?} installs; use the release page"
        );
    }

    if use_release_tarball() {
        return run_release_tarball(version, tx, &mut log)
            .await
            .with_context(|| {
                format!(
                    "Download and install this release manually from {}",
                    origin::release_page_url(version)
                )
            });
    }

    run_source_install(version, tx, &mut log).await
}

async fn run_release_tarball(
    version: Version,
    tx: &async_channel::Sender<Event>,
    log: &mut std::fs::File,
) -> anyhow::Result<()> {
    let cache_dir = flowmux_config::paths::host_visible_cache_dir().context("HOME is unset")?;
    let work_dir = cache_dir.join(format!(".update-{}", std::process::id()));
    if work_dir.exists() {
        std::fs::remove_dir_all(&work_dir).context("clear previous update staging")?;
    }
    let extract_dir = work_dir.join("extracted");
    std::fs::create_dir_all(&extract_dir).context("create update staging")?;

    let asset = release_asset_name(version);
    let url = format!("{RELEASE_DOWNLOAD_URL}/{}/{asset}", version.tag());
    let client = reqwest::Client::new();

    let _ = tx.send(Event::Progress(Stage::Fetching, 0)).await;
    let fetching = Progress::new(Stage::Fetching, tx);
    let checksum_text = client
        .get(format!("{url}.sha256"))
        .send()
        .await
        .context("download release checksum")?
        .error_for_status()
        .context("release checksum request failed")?
        .text()
        .await
        .context("read release checksum")?;
    let expected = expected_sha256(&checksum_text).context("invalid release checksum")?;
    fetching.report(5);

    let mut response = client
        .get(&url)
        .send()
        .await
        .context("download release tarball")?
        .error_for_status()
        .context("release tarball request failed")?;
    let total = response.content_length().filter(|total| *total > 0);
    let archive = work_dir.join(&asset);
    let mut file = tokio::fs::File::create(&archive)
        .await
        .context("create release tarball")?;
    let mut downloaded = 0_u64;
    let mut hasher = Sha256::new();
    while let Some(chunk) = response.chunk().await.context("read release tarball")? {
        downloaded = downloaded.saturating_add(chunk.len() as u64);
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .context("write release tarball")?;
        if let Some(total) = total {
            fetching.report(5 + (downloaded.saturating_mul(90) / total).min(90) as u8);
        }
    }
    file.flush().await.context("flush release tarball")?;
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        anyhow::bail!("release checksum mismatch: expected {expected}, got {actual}");
    }
    writeln!(log, "release_asset={asset} sha256={actual}").context("write release log")?;
    fetching.report(100);

    let _ = tx.send(Event::Progress(Stage::Installing, 0)).await;
    let installing = Progress::new(Stage::Installing, tx);
    let archive_root = asset.trim_end_matches(".tar.gz");
    let mut tar_argv = vec![
        "tar".to_string(),
        "-xzf".to_string(),
        archive.display().to_string(),
        "-C".to_string(),
        extract_dir.display().to_string(),
        "--strip-components=1".to_string(),
    ];
    tar_argv.extend(
        RELEASE_BINARIES
            .iter()
            .map(|binary| format!("{archive_root}/{binary}")),
    );
    run_logged(tar_argv, None, None, &*log, installing.clone()).await?;
    installing.report(40);

    let install_dir = std::env::current_exe()
        .context("locate running flowmux")?
        .parent()
        .context("running flowmux has no parent directory")?
        .to_path_buf();
    let staged = extract_dir.clone();
    let destination = install_dir.clone();
    tokio::task::spawn_blocking(move || install_release_binaries(&staged, &destination))
        .await
        .context("join release install task")??;
    writeln!(log, "installed_release_to={}", install_dir.display()).context("write install log")?;
    installing.report(100);
    let _ = std::fs::remove_dir_all(work_dir);
    Ok(())
}

fn install_release_binaries(extracted: &Path, install_dir: &Path) -> anyhow::Result<()> {
    let mut staged = Vec::new();
    for binary in RELEASE_BINARIES {
        let source = extracted.join(binary);
        if !source.is_file() {
            anyhow::bail!("release is missing {binary}");
        }
        let pending = install_dir.join(format!(".{binary}.pending.{}", std::process::id()));
        let _ = std::fs::remove_file(&pending);
        std::fs::copy(&source, &pending).with_context(|| format!("stage {}", pending.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o755))
                .with_context(|| format!("set permissions on {}", pending.display()))?;
        }
        staged.push((pending, install_dir.join(binary)));
    }
    for (pending, destination) in staged {
        std::fs::rename(&pending, &destination)
            .with_context(|| format!("replace {}", destination.display()))?;
    }
    Ok(())
}

async fn run_source_install(
    version: Version,
    tx: &async_channel::Sender<Event>,
    log: &mut std::fs::File,
) -> anyhow::Result<()> {
    let dir = clone_dir().context("HOME is unset")?;
    let script = check::install_script(std::env::consts::OS);
    let manual_installer = dir.join(script);
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent).context("create cache dir")?;
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
        writeln!(&mut *log, "prerequisites=missing detail={message}")
            .context("write prerequisite failure")?;
        anyhow::bail!("{message}. {}", manual_install_guidance(&manual_installer));
    }
    writeln!(&mut *log, "prerequisites=ok").context("write prerequisite result")?;

    let _ = tx.send(Event::Progress(Stage::Fetching, 0)).await;
    let fetching = Progress::new(Stage::Fetching, tx);
    let tag = version.tag();
    let clone_exists = dir.join(".git").is_dir();
    if run_plan(
        check::git_plan(clone_exists, REPO_URL, &dir, &tag),
        &*log,
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
            &*log,
            fetching.clone(),
        )
        .await?;
    }
    fetching.report(100);

    let _ = tx.send(Event::Progress(Stage::Installing, 0)).await;
    let installing = Progress::new(Stage::Installing, tx);
    run_logged(
        vec!["bash".to_string(), script.to_string()],
        Some(dir),
        path.map(|p| ("PATH", p)),
        &*log,
        installing.clone(),
    )
    .await
    .with_context(|| manual_install_guidance(&manual_installer))?;
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
    use super::{
        check_prerequisites, expected_sha256, install_release_binaries, manual_install_guidance,
        output_progress, prerequisite_error, prerequisite_packages, RELEASE_BINARIES,
    };
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
    fn failed_install_guides_user_to_the_managed_script() {
        let script = std::path::Path::new("/home/u/.cache/flowmux/src/install.sh");
        assert_eq!(
            manual_install_guidance(script),
            "Run `/home/u/.cache/flowmux/src/install.sh` in a terminal, then retry"
        );
    }

    #[test]
    fn verified_release_binaries_replace_the_existing_install() {
        assert_eq!(
            expected_sha256(&format!("{}  flowmux.tar.gz\n", "a".repeat(64))),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(expected_sha256("not-a-checksum"), None);

        let temp = tempfile::tempdir().unwrap();
        let extracted = temp.path().join("extracted");
        let installed = temp.path().join("bin");
        std::fs::create_dir_all(&extracted).unwrap();
        std::fs::create_dir_all(&installed).unwrap();
        for binary in RELEASE_BINARIES {
            std::fs::write(extracted.join(binary), format!("new-{binary}")).unwrap();
            std::fs::write(installed.join(binary), format!("old-{binary}")).unwrap();
        }

        install_release_binaries(&extracted, &installed).unwrap();

        for binary in RELEASE_BINARIES {
            assert_eq!(
                std::fs::read_to_string(installed.join(binary)).unwrap(),
                format!("new-{binary}")
            );
            assert_eq!(
                std::fs::metadata(installed.join(binary))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
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
