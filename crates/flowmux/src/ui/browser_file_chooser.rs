// SPDX-License-Identifier: GPL-3.0-or-later

use gtk::{gio, glib, prelude::*};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Default)]
pub(crate) struct FileChoosers {
    pending: Rc<RefCell<Option<PendingChooser>>>,
}

struct PendingChooser {
    id: String,
    multiple: bool,
    directories: bool,
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

impl FileChoosers {
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
                        Action::Select { paths, .. } => {
                            validate_selection(paths, pending.multiple, pending.directories)?;
                            Some(paths.clone())
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
        let cancellable = gio::Cancellable::new();
        let closed = Rc::new(Cell::new(false));
        self.pending.replace(Some(PendingChooser {
            id: id.clone(),
            multiple,
            directories,
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
