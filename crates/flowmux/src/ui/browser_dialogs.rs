// SPDX-License-Identifier: GPL-3.0-or-later

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub(crate) enum PageDialog {
    Alert,
    Confirm,
    Prompt(String),
}

/// One-shot answer to a page dialog: `Some` for OK (with prompt text), `None`
/// for cancel. WebKit raises an exception when a panel completion handler is
/// released uncalled, so dropping an unanswered reply cancels the dialog.
pub(crate) struct PageDialogReply(pub(crate) Option<Box<dyn FnOnce(Option<String>)>>);

impl PageDialogReply {
    pub(crate) fn send(&mut self, reply: Option<String>) {
        if let Some(send) = self.0.take() {
            send(reply);
        }
    }
}

impl Drop for PageDialogReply {
    fn drop(&mut self) {
        self.send(None);
    }
}

#[derive(Clone, Default)]
pub(crate) struct PageDialogs {
    pending: Rc<RefCell<Option<PendingDialog>>>,
}

#[derive(Clone)]
struct PendingDialog {
    id: String,
    kind: &'static str,
    ready: Rc<Cell<bool>>,
    message: String,
    default_text: Option<String>,
    dialog: gtk::glib::WeakRef<adw::AlertDialog>,
    entry: Option<gtk::glib::WeakRef<gtk::Entry>>,
}

impl PageDialogs {
    pub(crate) async fn control(
        &self,
        action: &flowmux_ipc::protocol::BrowserDialogAction,
    ) -> Result<String, String> {
        use flowmux_ipc::protocol::BrowserDialogAction as Action;
        match action {
            Action::Status => {}
            Action::Accept { id, .. } | Action::Dismiss { id } => {
                let pending = self
                    .pending
                    .borrow()
                    .clone()
                    .filter(|pending| pending.id == *id)
                    .ok_or("page dialog not found or its ID is stale")?;
                let dialog = pending.dialog.upgrade().ok_or("page dialog closed")?;
                // Adwaita opens sheets after two frame ticks; closing sooner can reopen them.
                gtk::glib::future_with_timeout(std::time::Duration::from_secs(5), async {
                    while !pending.ready.get() {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .map_err(|_| "page dialog did not finish mapping")?;
                if !self
                    .pending
                    .borrow()
                    .as_ref()
                    .is_some_and(|current| current.id == *id)
                {
                    return Err("page dialog closed before the response".into());
                }
                if let Action::Accept {
                    text: Some(text), ..
                } = action
                {
                    let entry = pending
                        .entry
                        .and_then(|entry| entry.upgrade())
                        .ok_or("text is only valid for a page prompt")?;
                    entry.set_text(text);
                }
                let accepted = matches!(action, Action::Accept { .. }) || pending.kind == "alert";
                let response = if accepted { "ok" } else { "cancel" };
                dialog.set_close_response(response);
                if !dialog.close() {
                    return Err("page dialog could not be closed".into());
                }
                gtk::glib::future_with_timeout(std::time::Duration::from_secs(5), async {
                    while dialog.is_mapped() {
                        gtk::glib::timeout_future(std::time::Duration::from_millis(10)).await;
                    }
                })
                .await
                .map_err(|_| "page dialog answered but its UI did not finish closing")?;
            }
        }
        let pending = self.pending.borrow();
        let dialog = pending.as_ref().map(|pending| {
            serde_json::json!({
                "id": pending.id, "kind": pending.kind, "message": pending.message,
                "default_text": pending.default_text,
            })
        });
        Ok(serde_json::json!({"dialog": dialog}).to_string())
    }

    fn clear(&self, id: &str) {
        let mut pending = self.pending.borrow_mut();
        if pending.as_ref().is_some_and(|pending| pending.id == id) {
            pending.take();
        }
    }

    pub(crate) fn present(
        &self,
        parent: &gtk::Window,
        heading: &str,
        message: &str,
        kind: PageDialog,
        reply: PageDialogReply,
    ) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(message));
        if !matches!(kind, PageDialog::Alert) {
            dialog.add_response("cancel", "Cancel");
        }
        dialog.add_response("ok", "OK");
        dialog.set_default_response(Some("ok"));
        dialog.set_close_response(if matches!(kind, PageDialog::Alert) {
            "ok"
        } else {
            "cancel"
        });
        let kind_name = match &kind {
            PageDialog::Alert => "alert",
            PageDialog::Confirm => "confirm",
            PageDialog::Prompt(_) => "prompt",
        };
        let default_text = match &kind {
            PageDialog::Prompt(text) => Some(text.clone()),
            _ => None,
        };
        let entry = match kind {
            PageDialog::Prompt(default_text) => {
                let entry = gtk::Entry::builder()
                    .text(default_text)
                    .activates_default(true)
                    .build();
                dialog.set_extra_child(Some(&entry));
                Some(entry)
            }
            _ => None,
        };
        let id = uuid::Uuid::new_v4().to_string();
        let ready = Rc::new(Cell::new(false));
        self.pending.replace(Some(PendingDialog {
            id: id.clone(),
            kind: kind_name,
            ready: ready.clone(),
            message: message.into(),
            default_text,
            dialog: dialog.downgrade(),
            entry: entry.as_ref().map(|entry| entry.downgrade()),
        }));
        let pending = Rc::new(RefCell::new(Some(reply)));
        let pending_on_unmap = pending.clone();
        let state = self.clone();
        let id_on_unmap = id.clone();
        dialog.connect_unmap(move |_| {
            state.clear(&id_on_unmap);
            pending_on_unmap.borrow_mut().take();
        });
        let state = self.clone();
        dialog.connect_response(None, move |_, response| {
            state.clear(&id);
            if let Some(mut reply) = pending.borrow_mut().take() {
                reply.send((response == "ok").then(|| {
                    entry
                        .as_ref()
                        .map(|entry| entry.text().to_string())
                        .unwrap_or_default()
                }));
            }
        });
        dialog.present(Some(parent));
        let ticks = Cell::new(0);
        dialog.add_tick_callback(move |_, _| {
            ticks.set(ticks.get() + 1);
            if ticks.get() >= 2 {
                ready.set(true);
                gtk::glib::ControlFlow::Break
            } else {
                gtk::glib::ControlFlow::Continue
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[gtk::test]
    fn ui_response_resolves_the_same_pending_prompt() {
        use flowmux_ipc::protocol::BrowserDialogAction as Action;
        let parent = adw::Window::new();
        parent.present();
        let dialogs = PageDialogs::default();
        let replies = Rc::new(RefCell::new(Vec::new()));
        let captured = replies.clone();
        dialogs.present(
            parent.upcast_ref(),
            "This page says",
            "Name?",
            PageDialog::Prompt("initial".into()),
            PageDialogReply(Some(Box::new(move |reply| {
                captured.borrow_mut().push(reply)
            }))),
        );
        let pending = dialogs.pending.borrow().clone().unwrap();
        pending
            .entry
            .unwrap()
            .upgrade()
            .unwrap()
            .set_text("human edit");
        let dialog = pending.dialog.upgrade().unwrap();
        dialog.emit_by_name::<()>("response", &[&"ok"]);
        assert_eq!(*replies.borrow(), [Some("human edit".into())]);
        assert_eq!(
            gtk::glib::MainContext::default()
                .block_on(dialogs.control(&Action::Status))
                .unwrap(),
            "{\"dialog\":null}"
        );
        assert!(gtk::glib::MainContext::default()
            .block_on(dialogs.control(&Action::Dismiss { id: pending.id }))
            .is_err());
        parent.close();
        assert_eq!(replies.borrow().len(), 1);
    }

    #[cfg(target_os = "linux")]
    #[gtk::test]
    fn immediate_answer_waits_for_mapping_and_removes_the_modal() {
        use flowmux_ipc::protocol::BrowserDialogAction as Action;
        let parent = adw::Window::new();
        parent.present();
        let dialogs = PageDialogs::default();
        let accepted = Rc::new(Cell::new(false));
        let captured = accepted.clone();
        dialogs.present(
            parent.upcast_ref(),
            "This page says",
            "Continue?",
            PageDialog::Confirm,
            PageDialogReply(Some(Box::new(move |reply| captured.set(reply.is_some())))),
        );
        let pending = dialogs.pending.borrow().clone().unwrap();
        let dialog = pending.dialog.upgrade().unwrap();
        gtk::glib::MainContext::default()
            .block_on(dialogs.control(&Action::Accept {
                id: pending.id,
                text: None,
            }))
            .unwrap();
        assert!(accepted.get());
        assert!(!dialog.is_mapped());
        assert!(dialogs.pending.borrow().is_none());
        parent.close();
    }

    #[test]
    fn page_dialog_reply_cancels_on_drop_and_answers_only_once() {
        let replies = Rc::new(RefCell::new(Vec::new()));
        {
            let replies = replies.clone();
            let _reply = PageDialogReply(Some(Box::new(move |value| {
                replies.borrow_mut().push(value);
            })));
        }
        {
            let replies = replies.clone();
            let mut reply = PageDialogReply(Some(Box::new(move |value| {
                replies.borrow_mut().push(value);
            })));
            reply.send(Some("edited".into()));
            reply.send(None);
        }
        assert_eq!(*replies.borrow(), [None, Some("edited".into())]);
    }
}
