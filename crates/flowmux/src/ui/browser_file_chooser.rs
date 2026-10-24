// SPDX-License-Identifier: GPL-3.0-or-later

use gtk::{gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Default)]
pub(crate) struct FileChoosers {
    pending: Rc<RefCell<Option<PendingChooser>>>,
    control: Option<super::browser_control::BrowserControl>,
}

struct PendingChooser {
    id: String,
    multiple: bool,
    directories: bool,
    source: String,
    cancellable: gio::Cancellable,
    closed: Rc<Cell<bool>>,
    reply: Option<Box<dyn FnOnce(Option<Vec<PathBuf>>)>>,
}

impl PendingChooser {
    fn finish(mut self, files: Option<Vec<PathBuf>>) {
        if let Some(reply) = self.reply.take() {
            reply(files);
        }
    }
}

impl Drop for PendingChooser {
    fn drop(&mut self) {
        self.cancellable.cancel();
        if let Some(reply) = self.reply.take() {
            reply(None);
        }
    }
}

fn validate_selection(paths: &[PathBuf], multiple: bool, directories: bool) -> Result<(), String> {
    if paths.is_empty() || (!multiple && paths.len() != 1) {
        return Err("file chooser selection count does not match the input".into());
    }
    for path in paths {
        if !path.is_absolute() || path.to_str().is_none() {
            return Err("file chooser paths must be absolute UTF-8 paths".into());
        }
        let metadata = path
            .metadata()
            .map_err(|error| format!("{}: {error}", path.display()))?;
        if directories {
            if !metadata.is_dir() {
                return Err("this file chooser requires directories".into());
            }
            std::fs::read_dir(path).map_err(|error| error.to_string())?;
        } else {
            if !metadata.is_file() {
                return Err("this file chooser requires regular files".into());
            }
            std::fs::File::open(path).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn scoped_selection(
    paths: &[PathBuf],
    root: Option<&std::path::Path>,
    directories: bool,
) -> Result<Vec<PathBuf>, String> {
    if paths
        .iter()
        .any(|path| !path.is_absolute() || path.to_str().is_none())
    {
        return Err("file chooser paths must be absolute UTF-8 paths".into());
    }
    let root =
        root.ok_or("automated file selection requires an explicit --root directory scope")?;
    if !root.is_absolute() {
        return Err("file scope must be absolute".into());
    }
    let root = root.canonicalize().map_err(|error| error.to_string())?;
    if !root.is_dir() {
        return Err("file scope must be a directory".into());
    }
    paths
        .iter()
        .map(|path| {
            let path = path.canonicalize().map_err(|error| error.to_string())?;
            if !path.starts_with(&root) {
                return Err("selected file is outside the explicit directory scope".into());
            }
            if directories {
                let mut pending = vec![path.clone()];
                let mut entries = 0;
                while let Some(directory) = pending.pop() {
                    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
                        let entry = entry.map_err(|error| error.to_string())?;
                        entries += 1;
                        if entries > 10_000 {
                            return Err(
                                "folder scope exceeds 10000 entries; use the native picker".into(),
                            );
                        }
                        let kind = entry.file_type().map_err(|error| error.to_string())?;
                        if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
                            return Err(
                                "automated folder uploads cannot contain links or special files"
                                    .into(),
                            );
                        }
                        if kind.is_dir() {
                            pending.push(entry.path());
                        }
                    }
                }
            }
            Ok(path)
        })
        .collect()
}

impl FileChoosers {
    pub(crate) fn with_control(control: super::browser_control::BrowserControl) -> Self {
        Self {
            pending: Rc::default(),
            control: Some(control),
        }
    }

    pub(crate) async fn control(
        &self,
        action: &flowmux_ipc::protocol::BrowserFileChooserAction,
    ) -> Result<String, String> {
        use flowmux_ipc::protocol::BrowserFileChooserAction as Action;
        match action {
            Action::Status => {}
            Action::Select { id, .. } | Action::Cancel { id } => {
                let files = {
                    let pending = self.pending.borrow();
                    let pending = pending
                        .as_ref()
                        .filter(|pending| pending.id == *id)
                        .ok_or("file chooser not found or its ID is stale")?;
                    match action {
                        Action::Select { paths, root, .. } => {
                            let files =
                                scoped_selection(paths, root.as_deref(), pending.directories)?;
                            validate_selection(&files, pending.multiple, pending.directories)?;
                            Some(files)
                        }
                        _ => None,
                    }
                };
                let pending = self
                    .pending
                    .borrow_mut()
                    .take()
                    .expect("validated file chooser");
                pending.cancellable.cancel();
                glib::future_with_timeout(std::time::Duration::from_secs(5), async {
                    while !pending.closed.get() {
                        glib::timeout_future(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .map_err(|_| "file chooser UI did not finish closing")?;
                pending.finish(files);
            }
        }
        let pending = self.pending.borrow();
        let chooser = pending.as_ref().map(|pending| {
            serde_json::json!({
                "id": pending.id, "multiple": pending.multiple, "directories": pending.directories,
                "source": pending.source, "selection_requires_scope": true,
            })
        });
        Ok(serde_json::json!({"file_chooser": chooser}).to_string())
    }

    pub(crate) fn present(
        &self,
        parent: &gtk::Window,
        dialog: &gtk::FileDialog,
        multiple: bool,
        directories: bool,
        reply: impl FnOnce(Option<Vec<PathBuf>>) + 'static,
    ) {
        let id = uuid::Uuid::new_v4().to_string();
        let control = self.control.clone();
        let task = control
            .as_ref()
            .map(|control| control.status()["task_id"].clone());
        let result_id = id.clone();
        let source = dialog.title().to_string();
        if let Some(control) = &control {
            control.result(serde_json::json!({"key":format!("upload:{id}"),"title":"Waiting for file selection","detail":source}));
        }
        let reply = move |files: Option<Vec<PathBuf>>| {
            if let Some(control) = &control {
                if Some(control.status()["task_id"].clone()) == task {
                    let names = files
                        .as_ref()
                        .map(|paths| {
                            paths
                                .iter()
                                .take(20)
                                .map(|path| {
                                    path.file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned()
                                })
                                .collect::<Vec<_>>()
                                .join(", ")
                        })
                        .unwrap_or_default();
                    control.result(serde_json::json!({"key":format!("upload:{result_id}"),"title":if files.is_some(){"Files selected — upload unconfirmed"}else{"File selection cancelled"},"detail":format!("{source}\n{names}"),"chooser_id":result_id,"status":if files.is_some(){"selected"}else{"cancelled"}}));
                }
            }
            reply(files);
        };
        let cancellable = gio::Cancellable::new();
        let closed = Rc::new(Cell::new(false));
        self.pending.replace(Some(PendingChooser {
            id: id.clone(),
            multiple,
            directories,
            source: dialog.title().to_string(),
            cancellable: cancellable.clone(),
            closed: closed.clone(),
            reply: Some(Box::new(reply)),
        }));
        let state = Rc::downgrade(&self.pending);
        let complete = move |files: Option<Vec<PathBuf>>| {
            closed.set(true);
            if let Some(state) = state.upgrade() {
                let pending = state.borrow_mut().take_if(|pending| pending.id == id);
                if let Some(pending) = pending {
                    let files = files
                        .filter(|paths| validate_selection(paths, multiple, directories).is_ok());
                    pending.finish(files);
                }
            }
        };
        if multiple {
            let callback = move |result: Result<gio::ListModel, glib::Error>| {
                complete(result.ok().and_then(|files| {
                    files
                        .iter::<gio::File>()
                        .map(|file| file.ok()?.path())
                        .collect::<Option<Vec<_>>>()
                }));
            };
            if directories {
                dialog.select_multiple_folders(Some(parent), Some(&cancellable), callback);
            } else {
                dialog.open_multiple(Some(parent), Some(&cancellable), callback);
            }
        } else {
            let callback = move |result: Result<gio::File, glib::Error>| {
                complete(
                    result
                        .ok()
                        .and_then(|file| file.path())
                        .map(|path| vec![path]),
                );
            };
            if directories {
                dialog.select_folder(Some(parent), Some(&cancellable), callback);
            } else {
                dialog.open(Some(parent), Some(&cancellable), callback);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_a_pending_chooser_cancels_ui_and_replies_once() {
        let cancellable = gio::Cancellable::new();
        let replies = Rc::new(RefCell::new(Vec::new()));
        let captured = replies.clone();
        let pending = PendingChooser {
            id: "test".into(),
            source: "test".into(),
            multiple: false,
            directories: false,
            cancellable: cancellable.clone(),
            closed: Rc::new(Cell::new(false)),
            reply: Some(Box::new(move |files| captured.borrow_mut().push(files))),
        };
        drop(pending);
        assert!(cancellable.is_cancelled());
        assert_eq!(*replies.borrow(), [None]);
    }

    #[test]
    fn file_scope_rejects_escapes_and_implicit_grants() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = root.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(scoped_selection(&[link], Some(root.path()), false).is_err());
        assert!(scoped_selection(&[outside.path().into()], None, false).is_err());
        let file = root.path().join("safe.txt");
        std::fs::write(&file, "fixture").unwrap();
        assert_eq!(
            scoped_selection(std::slice::from_ref(&file), Some(root.path()), false).unwrap(),
            [file.canonicalize().unwrap()]
        );
        assert!(scoped_selection(&[root.path().into()], Some(root.path()), true).is_err());
        let folder = root.path().join("folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("nested.txt"), "fixture").unwrap();
        assert!(scoped_selection(&[folder], Some(root.path()), true).is_ok());
    }

    #[test]
    fn selections_reject_missing_paths_wrong_kinds_and_wrong_counts() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("한글.txt");
        std::fs::write(&file, "fixture").unwrap();
        assert!(validate_selection(std::slice::from_ref(&file), false, false).is_ok());
        assert!(validate_selection(&[file.clone(), file.clone()], true, false).is_ok());
        assert!(validate_selection(&[], true, false).is_err());
        assert!(validate_selection(&[file.clone(), file.clone()], false, false).is_err());
        assert!(validate_selection(&[PathBuf::from("relative")], false, false).is_err());
        assert!(validate_selection(&[root.path().join("missing")], false, false).is_err());
        assert!(validate_selection(&[root.path().into()], false, false).is_err());
        assert!(validate_selection(&[root.path().into()], false, true).is_ok());
        assert!(validate_selection(&[file], false, true).is_err());
    }
}
