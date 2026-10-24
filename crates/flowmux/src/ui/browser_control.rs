// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime-only ownership of one browser tab. This coordinates trusted local clients;
//! it is not a sandbox for programs with access to the user's IPC socket.
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use uuid::Uuid;

#[derive(Default)]
struct State {
    task: Option<Uuid>,
    phase: &'static str,
    revision: u64,
    busy: bool,
    document_generation: u64,
    event_sequence: u64,
    events: std::collections::VecDeque<serde_json::Value>,
}

impl State {
    fn control(&mut self, action: &str, task: Option<Uuid>) -> Result<(), String> {
        match action {
            "status" => return Ok(()),
            "start" if self.task.is_none() && !self.busy => {
                if task.is_some() {
                    return Err("start does not accept an existing task ID".into());
                }
                self.task = Some(Uuid::new_v4());
                self.phase = "running";
            }
            "start" => return Err("this tab already has a task or an action in progress".into()),
            "handoff" | "pause" | "stop" | "finish" => {
                if self.task.is_none() || task != self.task {
                    return Err("the owning browser task ID is required".into());
                }
                if matches!(self.phase, "stopped" | "completed") {
                    return Err("this browser task has ended".into());
                }
                if action == "finish" && (self.busy || self.phase != "running") {
                    return Err("finish requires a running task with no action in progress".into());
                }
                self.phase = match action {
                    "handoff" => "user_needed",
                    "pause" => "paused",
                    "stop" => "stopped",
                    _ => "completed",
                };
            }
            _ => return Err("unknown browser control action".into()),
        }
        self.revision += 1;
        Ok(())
    }

    fn admit(&mut self, task: Option<Uuid>) -> Result<u64, String> {
        if self.task != task || (self.task.is_some() && self.phase != "running") {
            return Err("browser control is not held by this task; inspect control status and wait for the user".into());
        }
        if self.busy {
            return Err("another action is still in progress in this browser tab".into());
        }
        self.busy = true;
        Ok(self.revision)
    }
}

thread_local! {
    // Native inputs share AppKit/X11 focus even across tabs in this process.
    static NATIVE_BUSY: Cell<bool> = const { Cell::new(false) };
}

#[derive(Clone)]
pub(crate) struct BrowserControl {
    state: Rc<RefCell<State>>,
    pub bar: gtk::Box,
    label: gtk::Label,
    resume: gtk::Button,
    clear: gtk::Button,
}

pub(crate) struct Operation {
    control: BrowserControl,
    native: bool,
}

impl Drop for Operation {
    fn drop(&mut self) {
        self.control.state.borrow_mut().busy = false;
        if self.native {
            NATIVE_BUSY.with(|busy| busy.set(false));
        }
        self.control.refresh();
    }
}

impl BrowserControl {
    pub fn new() -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_start(6);
        bar.set_margin_end(6);
        let label = gtk::Label::new(None);
        label.set_hexpand(true);
        label.set_xalign(0.0);
        label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        label.set_max_width_chars(24);
        let take = gtk::Button::with_label("Take control");
        let resume = gtk::Button::with_label("Continue");
        let stop = gtk::Button::with_label("Stop task");
        let clear = gtk::Button::with_label("Clear task");
        bar.append(&label);
        for button in [&take, &resume, &stop, &clear] {
            bar.append(button);
        }
        bar.set_tooltip_text(Some("Local automation runs as your OS user. Task IDs coordinate actions; they are not authentication. Stopping prevents new actions. Already dispatched site actions may still complete. The tab is kept open."));
        let control = Self {
            state: Rc::default(),
            bar,
            label,
            resume,
            clear,
        };
        // Weak widgets prevent the button closures from retaining their own bar.
        for (button, action) in [
            (&take, "pause"),
            (&control.resume, "resume"),
            (&stop, "stop"),
            (&control.clear, "clear"),
        ] {
            let state = control.state.clone();
            let bar = control.bar.downgrade();
            let label = control.label.downgrade();
            let resume = control.resume.downgrade();
            let clear = control.clear.downgrade();
            button.connect_clicked(move |_| {
                let (Some(bar), Some(label), Some(resume), Some(clear)) = (
                    bar.upgrade(),
                    label.upgrade(),
                    resume.upgrade(),
                    clear.upgrade(),
                ) else {
                    return;
                };
                let control = Self {
                    state: state.clone(),
                    bar,
                    label,
                    resume,
                    clear,
                };
                control.user_action(action);
            });
        }
        control.refresh();
        control
    }

    fn user_action(&self, action: &str) {
        let mut state = self.state.borrow_mut();
        match action {
            "resume" if !state.busy && matches!(state.phase, "paused" | "user_needed") => {
                state.phase = "running"
            }
            "clear" if !state.busy && matches!(state.phase, "stopped" | "completed") => {
                state.task = None;
                state.phase = "";
            }
            "pause" | "stop"
                if state.task.is_some() && !matches!(state.phase, "stopped" | "completed") =>
            {
                state.phase = if action == "stop" {
                    "stopped"
                } else {
                    "paused"
                };
            }
            _ => return,
        }
        state.revision += 1;
        drop(state);
        self.refresh();
    }

    fn refresh(&self) {
        let state = self.state.borrow();
        self.bar.set_visible(state.task.is_some());
        self.label.set_text(match state.phase {
            "running" => "Browser task running",
            "user_needed" => "Browser task needs your input",
            "paused" => "Browser task paused — you have control",
            "stopped" => "Browser task stopped",
            "completed" => "Browser task completed",
            _ => "",
        });
        self.resume
            .set_sensitive(!state.busy && matches!(state.phase, "paused" | "user_needed"));
        self.clear
            .set_sensitive(!state.busy && matches!(state.phase, "stopped" | "completed"));
    }

    pub fn status(&self) -> serde_json::Value {
        let state = self.state.borrow();
        serde_json::json!({"authentication": "same_os_user_ipc", "task_id_is_secret": false, "task_id":state.task,"state":if state.task.is_none(){"unmanaged"}else{state.phase},
            "action_in_progress":state.busy,"revision":state.revision,
            "resume_requires_user":true})
    }

    pub fn control(&self, action: &str, task: Option<Uuid>) -> Result<serde_json::Value, String> {
        self.state.borrow_mut().control(action, task)?;
        self.refresh();
        Ok(self.status())
    }

    pub fn admit(&self, task: Option<Uuid>, native: bool) -> Result<(Operation, u64), String> {
        if native && NATIVE_BUSY.with(Cell::get) {
            return Err(
                "another native browser input is in progress in this window process".into(),
            );
        }
        let revision = self.state.borrow_mut().admit(task)?;
        if native {
            NATIVE_BUSY.with(|busy| busy.set(true));
        }
        self.refresh();
        Ok((
            Operation {
                control: self.clone(),
                native,
            },
            revision,
        ))
    }

    pub fn document_started(&self) {
        self.state.borrow_mut().document_generation += 1;
        self.record("navigation", serde_json::Value::Null);
    }

    pub fn record(&self, kind: &str, identifier: serde_json::Value) {
        let mut state = self.state.borrow_mut();
        state.event_sequence += 1;
        let event = serde_json::json!({"sequence":state.event_sequence,"kind":kind,
            "identifier":identifier,"document_generation":state.document_generation});
        if state.events.len() == 64 {
            state.events.pop_front();
        }
        state.events.push_back(event);
    }

    pub fn observation(&self) -> serde_json::Value {
        let state = self.state.borrow();
        serde_json::json!({"document_generation":state.document_generation,
            "event_sequence":state.event_sequence,"events":state.events})
    }

    pub fn revision(&self) -> u64 {
        self.state.borrow().revision
    }

    pub fn check_revision(&self, revision: u64) -> Result<(), String> {
        let state = self.state.borrow();
        if state.revision != revision {
            Err("browser control changed before input; input cancelled".into())
        } else {
            Ok(())
        }
    }

    pub fn close(&self) {
        let mut state = self.state.borrow_mut();
        state.phase = "stopped";
        state.revision += 1;
    }
}

#[cfg(any(test, feature = "native-smoke"))]
pub(crate) fn check_controls() {
    let control = BrowserControl::new();
    let window = gtk::Window::builder().child(&control.bar).build();
    window.present();
    let click = |name: &str| {
        let mut widget = control.bar.first_child();
        while let Some(child) = widget {
            if let Some(button) = child.downcast_ref::<gtk::Button>() {
                if button.label().as_deref() == Some(name) {
                    assert!(button.is_sensitive(), "{name} disabled");
                    button.emit_clicked();
                    return;
                }
            }
            widget = child.next_sibling();
        }
        panic!("missing {name}");
    };
    let task =
        serde_json::from_value(control.control("start", None).unwrap()["task_id"].clone()).unwrap();
    assert!(control.bar.is_visible());
    let (operation, revision) = control.admit(Some(task), true).unwrap();
    click("Take control");
    assert!(control.check_revision(revision).is_err());
    assert!(!control.resume.is_sensitive());
    drop(operation);
    click("Continue");
    assert_eq!(control.status()["state"], "running");
    drop(control.admit(Some(task), true).unwrap());
    control.control("handoff", Some(task)).unwrap();
    click("Continue");
    drop(control.admit(Some(task), true).unwrap());
    click("Stop task");
    assert!(control.admit(Some(task), true).is_err());
    click("Clear task");
    assert!(!control.bar.is_visible());
    assert!(control.admit(Some(task), true).is_err());
    drop(control.admit(None, true).unwrap());
    window.destroy();
    println!("BROWSER_CONTROL_UI_OK");
}

#[cfg(all(test, target_os = "linux"))]
#[gtk::test]
fn control_buttons_revoke_and_resume_the_lease() {
    check_controls();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_handoff_rejects_old_foreign_and_unmanaged_writers() {
        let mut state = State::default();
        state.control("start", None).unwrap();
        let owner = state.task;
        assert!(state.admit(None).is_err());
        assert!(state.admit(Some(Uuid::new_v4())).is_err());
        let revision = state.admit(owner).unwrap();
        assert!(state.admit(owner).is_err());
        state.control("handoff", owner).unwrap();
        assert!(state.revision > revision);
        state.busy = false;
        assert!(state.admit(owner).is_err());
        assert!(state.control("resume", owner).is_err());
        state.control("stop", owner).unwrap();
        assert!(state.admit(owner).is_err());
        assert!(state.control("start", None).is_err());
        assert!(state.control("finish", owner).is_err());
    }
}
