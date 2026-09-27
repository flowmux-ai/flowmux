// SPDX-License-Identifier: GPL-3.0-or-later
//! Small Windows-only settings file, atomic updates under an OS-held writer lock.
use super::{checked, data_dir, wide};
use crate::{
    command::SettingsOp,
    settings::{Document, MAX_SETTINGS_BYTES},
};
use anyhow::Context;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::fs::OpenOptionsExt,
    path::PathBuf,
    sync::mpsc,
    time::Duration,
};
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::*;

pub struct Update {
    pub request: Option<Uuid>,
    pub result: Result<Document, String>,
}
pub struct Worker {
    pub path: Option<PathBuf>,
    sender: Option<mpsc::SyncSender<(Uuid, SettingsOp)>>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Worker {
    pub fn start(
        background: bool,
        emit: impl Fn(Update) + Send + 'static,
    ) -> anyhow::Result<(Self, Result<Document, String>)> {
        let override_dir = if cfg!(debug_assertions) {
            std::env::var_os("FLOWMUX_TEST_CONFIG_DIR").map(PathBuf::from)
        } else {
            None
        };
        let path = if let Some(dir) = override_dir {
            Some(dir.join("config.json"))
        } else if background {
            None
        } else {
            Some(data_dir()?.join("config.json"))
        };
        let store = Store { path: path.clone() };
        let initial = store.read().map_err(|e| format!("{e:#}"));
        let mut last = initial.clone();
        let mut volatile = initial.clone().unwrap_or_default();
        let (sender, receive) = mpsc::sync_channel(8);
        let thread = std::thread::Builder::new()
            .name("flowmux-settings".into())
            .spawn(move || loop {
                match receive.recv_timeout(Duration::from_secs(1)) {
                    Ok((id, op)) => {
                        let result = if store.path.is_some() {
                            store.update(&op)
                        } else {
                            change(&volatile, &op)
                        }
                        .map_err(|e| format!("{e:#}"));
                        if let Ok(doc) = &result {
                            volatile = doc.clone();
                            last = Ok(doc.clone());
                        }
                        emit(Update {
                            request: Some(id),
                            result,
                        });
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) if store.path.is_some() => {
                        let result = store.read().map_err(|e| format!("{e:#}"));
                        if result != last {
                            last = result.clone();
                            emit(Update {
                                request: None,
                                result,
                            });
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }
            })?;
        Ok((
            Self {
                path,
                sender: Some(sender),
                thread: Some(thread),
            },
            initial,
        ))
    }
    pub fn submit(&self, id: Uuid, op: SettingsOp) -> anyhow::Result<()> {
        self.sender
            .as_ref()
            .context("settings worker closed")?
            .try_send((id, op))
            .map_err(|_| anyhow::anyhow!("settings queue is full or stopped"))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
struct Store {
    path: Option<PathBuf>,
}
impl Store {
    fn bytes(&self) -> anyhow::Result<Option<Vec<u8>>> {
        let Some(path) = &self.path else {
            return Ok(None);
        };
        let file = match File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut bytes = Vec::new();
        file.take((MAX_SETTINGS_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }
    fn read(&self) -> anyhow::Result<Document> {
        self.bytes()?
            .map(|bytes| Document::decode(&bytes))
            .transpose()
            .map(|doc| doc.unwrap_or_default())
    }
    fn update(&self, op: &SettingsOp) -> anyhow::Result<Document> {
        let path = self.path.as_ref().context("settings path missing")?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        let _lease = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(path.with_extension("lock"))
            .context("another window is writing settings; try again")?;
        // Re-read under the lock: independent updates from different windows merge.
        // Only an explicit reset may replace an invalid schema; I/O errors still fail.
        let current = if matches!(op, SettingsOp::Reset) {
            self.bytes()?;
            Document::default()
        } else {
            self.read()?
        };
        let next = change(&current, op)?;
        let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| -> anyhow::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(&serde_json::to_vec_pretty(&next)?)?;
            file.sync_all()?;
            drop(file);
            unsafe {
                checked(MoveFileExW(
                    wide(&temp).as_ptr(),
                    wide(path).as_ptr(),
                    MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
                ))?;
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&temp);
        }
        result.context("could not atomically save terminal settings")?;
        Ok(next)
    }
}
fn change(current: &Document, op: &SettingsOp) -> anyhow::Result<Document> {
    let terminal = match op {
        SettingsOp::Set {
            key,
            value,
            expected,
        } => current.terminal.changed(*key, value, expected.as_deref())?,
        SettingsOp::Reset => Default::default(),
        SettingsOp::Show => anyhow::bail!("show does not write settings"),
    };
    Ok(Document {
        version: 1,
        revision: Uuid::new_v4(),
        terminal,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingKey;
    #[test]
    fn settings_lock_merge_conflict_corruption_and_failed_replace_preserve_data() {
        let dir = std::env::temp_dir().join(format!("flowmux-settings-{}", Uuid::new_v4()));
        let path = dir.join("config.json");
        let a = Store {
            path: Some(path.clone()),
        };
        let b = Store {
            path: Some(path.clone()),
        };
        let set = |key, value: &str, expected: Option<&str>| SettingsOp::Set {
            key,
            value: value.into(),
            expected: expected.map(String::from),
        };
        a.update(&set(SettingKey::FontSize, "20", None)).unwrap();
        b.update(&set(SettingKey::Theme, "light", None)).unwrap();
        assert_eq!(b.read().unwrap().terminal.font_size, 20);
        let previous = std::fs::read(&path).unwrap();
        assert!(a
            .update(&set(SettingKey::FontSize, "24", Some("14")))
            .is_err());
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path.with_extension("lock"))
            .unwrap();
        assert!(b.update(&SettingsOp::Reset).is_err());
        drop(lock);
        let guard = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();
        assert!(a.update(&SettingsOp::Reset).is_err());
        drop(guard);
        assert_eq!(std::fs::read(&path).unwrap(), previous);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::write(&path, b"invalid preserved settings").unwrap();
        assert!(a.read().is_err());
        assert!(a.update(&set(SettingKey::FontSize, "18", None)).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid preserved settings");
        assert_eq!(
            a.update(&SettingsOp::Reset).unwrap().terminal,
            Default::default()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
