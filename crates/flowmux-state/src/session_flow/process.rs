// SPDX-License-Identifier: GPL-3.0-or-later
//! Exact process-to-rollout evidence for Codex versions that defer startup hooks.
//! Never infer identity from the newest file, a title, or a shared daemon.
use crate::session_history::{list_sessions, SessionAgent};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

pub fn codex_process_session(root: u32, home: &Path) -> io::Result<Option<String>> {
    let sessions = list_sessions(SessionAgent::Codex, home)?;
    let mut ids = BTreeSet::new();
    for pid in flowmux_procmon::descendants(root).map_err(io::Error::other)? {
        if flowmux_procmon::agent_name_for_pid(pid) != Some("codex") {
            continue;
        }
        let Some(birth) = flowmux_procmon::process_start_time(pid) else {
            continue;
        };
        let files = writable_files(pid)?;
        if flowmux_procmon::process_start_time(pid) != Some(birth) {
            continue;
        }
        for session in &sessions {
            if session
                .path
                .canonicalize()
                .is_ok_and(|path| files.contains(&path))
            {
                ids.insert(session.id.clone());
            }
        }
    }
    Ok(if ids.len() == 1 {
        ids.pop_first()
    } else {
        None
    })
}

#[cfg(target_os = "macos")]
fn writable_files(pid: u32) -> io::Result<BTreeSet<PathBuf>> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    let mut child = Command::new("/usr/sbin/lsof")
        .args(["-a", "-p", &pid.to_string(), "-F0fan"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout
            .take(512 * 1024)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut timed_out = false;
    loop {
        match child.try_wait()? {
            Some(_) => break,
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                timed_out = true;
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
        }
    }
    let bytes = reader
        .join()
        .map_err(|_| io::Error::other("Process file reader failed"))??;
    if timed_out {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Process file lookup timed out",
        ));
    }
    if bytes.len() >= 512 * 1024 {
        return Err(io::Error::other("Process file list truncated"));
    }
    let mut result = BTreeSet::new();
    let mut writable = false;
    for field in bytes.split(|b| *b == 0) {
        let field = field.strip_prefix(b"\n").unwrap_or(field);
        match field.first() {
            Some(b'f') => writable = false,
            Some(b'a') => writable = matches!(field.get(1), Some(b'w' | b'u')),
            Some(b'n') if writable => {
                use std::os::unix::ffi::OsStrExt;
                let path = Path::new(std::ffi::OsStr::from_bytes(&field[1..]));
                if let Ok(path) = path.canonicalize() {
                    result.insert(path);
                }
            }
            _ => {}
        }
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
fn writable_files(pid: u32) -> io::Result<BTreeSet<PathBuf>> {
    let mut result = BTreeSet::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/fd"))?.flatten() {
        let info = std::fs::read_to_string(format!(
            "/proc/{pid}/fdinfo/{}",
            entry.file_name().to_string_lossy()
        ))
        .unwrap_or_default();
        let flags = info.lines().find_map(|line| {
            line.strip_prefix("flags:")
                .and_then(|flags| u32::from_str_radix(flags.trim(), 8).ok())
        });
        if flags.is_some_and(|flags| flags & 3 != 0) {
            if let Ok(path) = std::fs::read_link(entry.path()).and_then(|path| path.canonicalize())
            {
                result.insert(path);
            }
        }
    }
    Ok(result)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn writable_files(_pid: u32) -> io::Result<BTreeSet<PathBuf>> {
    Ok(BTreeSet::new())
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_file_evidence_includes_writers_and_excludes_readers() {
        let dir = tempfile::tempdir().unwrap();
        let writer = std::fs::File::create(dir.path().join("writer")).unwrap();
        std::fs::write(dir.path().join("reader"), b"read only").unwrap();
        let reader = std::fs::File::open(dir.path().join("reader")).unwrap();
        let paths = super::writable_files(std::process::id()).unwrap();
        assert!(paths.contains(&dir.path().join("writer").canonicalize().unwrap()));
        assert!(!paths.contains(&dir.path().join("reader").canonicalize().unwrap()));
        drop((reader, writer));
    }
}
