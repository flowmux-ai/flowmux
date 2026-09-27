// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded discovery records, published by atomic replacement after pipe binding.
use super::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

const MAX_RECORD_BYTES: u64 = 4096;

#[derive(Debug, Deserialize, Serialize)]
struct Record {
    pid: u32,
    pipe: String,
}

pub(super) fn pipe_pid(name: &str) -> anyhow::Result<u32> {
    let (pid, nonce) = name
        .strip_prefix(r"\\.\pipe\flowmux-")
        .and_then(|tail| tail.split_once('-'))
        .context("not a local flowmux pipe (expected a process ID and UUID)")?;
    let id = pid
        .parse::<u32>()
        .context("invalid flowmux pipe process ID")?;
    let uuid = uuid::Uuid::parse_str(nonce).context("invalid flowmux pipe UUID")?;
    anyhow::ensure!(
        id != 0 && pid == id.to_string() && nonce == uuid.to_string(),
        "noncanonical flowmux pipe name"
    );
    Ok(id)
}

fn read(path: &Path) -> Option<Record> {
    if !path.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return None;
    }
    let record: Record = serde_json::from_slice(&bytes).ok()?;
    if pipe_pid(&record.pipe).ok()? != record.pid
        || path.file_name()?.to_str()? != format!("{}.json", record.pid)
    {
        return None;
    }
    Some(record)
}

pub(super) fn candidates(directory: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries
        .sort_by_key(|entry| std::cmp::Reverse(entry.metadata().and_then(|m| m.modified()).ok()));
    entries
        .into_iter()
        .filter_map(|entry| read(&entry.path()).map(|record| record.pipe))
        .collect()
}

pub(super) fn publish(directory: &Path, name: &str) -> anyhow::Result<PathBuf> {
    let pid = pipe_pid(name)?;
    anyhow::ensure!(
        pid == std::process::id(),
        "cannot publish another process's pipe"
    );
    std::fs::create_dir_all(directory)?;
    let path = directory.join(format!("{pid}.json"));
    let temporary = directory.join(format!("{pid}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let bytes = serde_json::to_vec(&Record {
            pid,
            pipe: name.into(),
        })?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        let source = wide(&temporary);
        let destination = wide(&path);
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        loop {
            let success = unsafe {
                MoveFileExW(
                    source.as_ptr(),
                    destination.as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                )
            };
            if success != 0 {
                break;
            }
            let error = std::io::Error::last_os_error();
            // Native concurrent-reader testing also observes transient ACCESS_DENIED
            // during replacement. Retain the complete old record while retrying;
            // a persistent lock/denial still fails startup within a bounded interval.
            if !matches!(error.raw_os_error(), Some(code) if code == ERROR_ACCESS_DENIED as i32 || code == ERROR_SHARING_VIOLATION as i32)
                || std::time::Instant::now() >= deadline
            {
                return Err(error).context("replacing discovery record");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result.context("could not atomically publish Windows IPC discovery")?;
    Ok(path)
}

pub(super) fn remove_if_current(path: &Path, name: &str) {
    if read(path).is_some_and(|record| record.pipe == name) {
        let _ = std::fs::remove_file(path);
    }
}
