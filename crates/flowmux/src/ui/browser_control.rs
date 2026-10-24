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
    details: flowmux_ipc::protocol::BrowserTaskDetails,
    results: std::collections::VecDeque<serde_json::Value>,
}

impl State {
    fn control(&mut self, action: &str, task: Option<Uuid>) -> Result<(), String> {
        match action {
            "status" => return Ok(()),
            "start" if self.task.is_none() && !self.busy => {
                if task.is_some() {
                    return Err("start does not accept an existing task ID".into());
                }
                self.details = Default::default();
                self.results.clear();
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
    detail_label: gtk::Label,
    result_list: gtk::Box,
    tab_opener: Rc<RefCell<Option<Rc<dyn Fn(Uuid) -> bool>>>>,
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
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let detail_label = gtk::Label::new(None);
        detail_label.set_xalign(0.0);
        detail_label.set_wrap(true);
        detail_label.set_max_width_chars(60);
        detail_label.set_selectable(true);
        let result_list = gtk::Box::new(gtk::Orientation::Vertical, 8);
        result_list.set_margin_top(8);
        result_list.set_margin_bottom(8);
        result_list.set_margin_start(8);
        result_list.set_margin_end(8);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .max_content_height(360)
            .propagate_natural_height(true)
            .min_content_width(320)
            .child(&result_list)
            .build();
        let popover = gtk::Popover::builder().child(&scroll).build();
        let results = gtk::MenuButton::builder().label("Results").build();
        crate::ui::popover_pos::set_menu_popover(&results, &popover);
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
        text.append(&label);
        text.append(&detail_label);
        bar.append(&text);
        bar.append(&results);
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
            detail_label,
            result_list,
            tab_opener: Rc::default(),
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
            let detail_label = control.detail_label.downgrade();
            let result_list = control.result_list.downgrade();
            let tab_opener = control.tab_opener.clone();
            button.connect_clicked(move |_| {
                let (Some(bar), Some(label), Some(resume), Some(clear)) = (
                    bar.upgrade(),
                    label.upgrade(),
                    resume.upgrade(),
                    clear.upgrade(),
                ) else {
                    return;
                };
                let (Some(detail_label), Some(result_list)) =
                    (detail_label.upgrade(), result_list.upgrade())
                else {
                    return;
                };
                let control = Self {
                    state: state.clone(),
                    bar,
                    label,
                    resume,
                    clear,
                    detail_label,
                    result_list,
                    tab_opener: tab_opener.clone(),
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
                state.details = Default::default();
                state.results.clear();
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
        if state.results.is_empty() {
            while let Some(child) = self.result_list.first_child() {
                self.result_list.remove(&child);
            }
        }
        self.label.set_text(match state.phase {
            "running" => "Browser task running",
            "user_needed" => "Browser task needs your input",
            "paused" => "Browser task paused — you have control",
            "stopped" => "Browser task stopped",
            "completed" => "Browser task completed",
            _ => "",
        });
        let mut details = Vec::new();
        for (label, value) in [
            ("Task", &state.details.name),
            ("Step", &state.details.step),
            ("Waiting", &state.details.waiting),
            ("Your action", &state.details.user_action),
        ] {
            if let Some(value) = value.as_ref().filter(|value| !value.is_empty()) {
                details.push(format!("{label}: {value}"));
            }
        }
        self.detail_label.set_text(&details.join("\n"));
        self.detail_label.set_visible(!details.is_empty());
        self.resume
            .set_sensitive(!state.busy && matches!(state.phase, "paused" | "user_needed"));
        self.clear
            .set_sensitive(!state.busy && matches!(state.phase, "stopped" | "completed"));
    }

    pub fn status(&self) -> serde_json::Value {
        let state = self.state.borrow();
        serde_json::json!({"authentication": "same_os_user_ipc", "task_id_is_secret": false, "task_id":state.task,"state":if state.task.is_none(){"unmanaged"}else{state.phase},
            "action_in_progress":state.busy,"revision":state.revision,
            "resume_requires_user":true,"details":state.details,"results":state.results})
    }

    pub fn control(&self, action: &str, task: Option<Uuid>) -> Result<serde_json::Value, String> {
        self.state.borrow_mut().control(action, task)?;
        self.refresh();
        Ok(self.status())
    }

    pub fn update_task(
        &self,
        task: Option<Uuid>,
        details: flowmux_ipc::protocol::BrowserTaskDetails,
    ) -> Result<serde_json::Value, String> {
        let mut state = self.state.borrow_mut();
        if task.is_none() || task != state.task || matches!(state.phase, "stopped" | "completed") {
            return Err("an active owning task ID is required".into());
        }
        for value in [
            &details.name,
            &details.step,
            &details.waiting,
            &details.user_action,
        ]
        .into_iter()
        .flatten()
        {
            if value.chars().count() > 500 {
                return Err("task details must be at most 500 characters per field".into());
            }
        }
        if let Some(value) = details.name {
            state.details.name = Some(value);
        }
        if let Some(value) = details.step {
            state.details.step = Some(value);
        }
        if let Some(value) = details.waiting {
            state.details.waiting = Some(value);
        }
        if let Some(value) = details.user_action {
            state.details.user_action = Some(value);
        }
        drop(state);
        self.refresh();
        Ok(self.status())
    }

    pub fn set_tab_opener(&self, callback: Rc<dyn Fn(Uuid) -> bool>) {
        self.tab_opener.replace(Some(callback));
    }

    pub fn upload_observed(&self, id: &str) {
        let key = format!("upload:{id}");
        let previous = self
            .state
            .borrow()
            .results
            .iter()
            .find(|item| item["key"] == key)
            .cloned();
        if let Some(mut value) = previous {
            value["title"] = serde_json::json!("Upload condition observed");
            value["status"] = serde_json::json!("condition_observed");
            value["detail"] = serde_json::json!(format!("{}\nThe specified site condition was observed after selection; check the server result on the page.", value["detail"].as_str().unwrap_or_default()));
            self.result(value);
        }
    }

    pub fn download_result(&self, value: serde_json::Value) {
        let key = format!("download:{}", value["id"]);
        if !self
            .state
            .borrow()
            .results
            .iter()
            .any(|item| item["key"] == key)
        {
            return;
        }
        self.result(serde_json::json!({"key":key,"title":format!("Download {}: {}", value["id"], value["state"].as_str().unwrap_or("unknown")),
            "detail":value["path"].as_str().unwrap_or("Preparing file"),
            "path":if value["state"] == "complete" {value["path"].clone()} else {serde_json::Value::Null},
            "download_id":value["id"],"status":value["state"]}));
    }

    pub fn result(&self, value: serde_json::Value) {
        let mut state = self.state.borrow_mut();
        if state.task.is_none() {
            return;
        }
        if state.results.iter().any(|item| item == &value) {
            return;
        }
        state.results.retain(|item| item["key"] != value["key"]);
        if state.results.len() == 20 {
            state.results.pop_front();
        }
        state.results.push_back(value);
        let results = state.results.clone();
        drop(state);
        while let Some(child) = self.result_list.first_child() {
            self.result_list.remove(&child);
        }
        for result in results.iter().rev() {
            let card = gtk::Box::new(gtk::Orientation::Vertical, 4);
            card.add_css_class("card");
            let title = gtk::Label::new(result["title"].as_str());
            title.set_xalign(0.0);
            title.set_wrap(true);
            card.append(&title);
            let detail = gtk::Label::new(result["detail"].as_str());
            detail.set_xalign(0.0);
            detail.set_wrap(true);
            detail.set_max_width_chars(60);
            detail.set_selectable(true);
            card.append(&detail);
            if let Some(path) = result["path"].as_str() {
                let path = std::path::PathBuf::from(path);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
                for folder in [false, true] {
                    let button =
                        gtk::Button::with_label(if folder { "Open folder" } else { "Open file" });
                    let path = path.clone();
                    button.connect_clicked(move |button| {
                        if !path.is_file() {
                            button.set_sensitive(false);
                            button.set_tooltip_text(Some("File no longer exists"));
                            return;
                        }
                        if folder {
                            if let Some(parent) = path.parent() {
                                crate::ui::show_in_folder::open_directory(parent);
                            }
                        } else {
                            crate::ui::file_browser::open_file(&path);
                        }
                    });
                    row.append(&button);
                }
                card.append(&row);
            }
            if let Some(surface) = result["surface_id"]
                .as_str()
                .and_then(|id| Uuid::parse_str(id).ok())
            {
                let button = gtk::Button::with_label("Open related tab");
                let opener = self.tab_opener.clone();
                button.connect_clicked(move |button| {
                    if !opener.borrow().as_ref().is_some_and(|open| open(surface)) {
                        button.set_label("Tab no longer available");
                        button.set_sensitive(false);
                    }
                });
                card.append(&button);
            }
            self.result_list.append(&card);
        }
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
        drop(state);
        match kind {
            "popup" => self.result(serde_json::json!({"key":format!("popup:{identifier}"),"title":"Related tab","detail":"Popup created; page readiness not confirmed","surface_id":identifier})),
            "download" => self.result(serde_json::json!({"key":format!("download:{identifier}"),"title":"Download started","detail":"Waiting for the file to finish","download_id":identifier})),
            "dialog" => self.result(serde_json::json!({"key":"dialog","title":"Page dialog","detail":"Inspect the dialog or take control to respond"})),
            _ => {}
        }
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
    let details = flowmux_ipc::protocol::BrowserTaskDetails {
        name: Some("Account report".into()),
        step: Some("Download".into()),
        waiting: Some("Login".into()),
        user_action: Some("Sign in and Continue".into()),
    };
    assert!(control.update_task(None, details.clone()).is_err());
    control.update_task(Some(task), details).unwrap();
    assert!(control.detail_label.text().contains("Sign in and Continue"));
    control.result(
        serde_json::json!({"key":"test","title":"Upload unconfirmed","detail":"File selected"}),
    );
    assert_eq!(
        control.status()["results"][0]["title"],
        "Upload unconfirmed"
    );
    assert!(control.result_list.first_child().is_some());
    control.set_tab_opener(Rc::new(|_| false));
    control.record("popup", serde_json::json!(Uuid::new_v4()));
    let card = control.result_list.first_child().unwrap();
    let button = card
        .last_child()
        .unwrap()
        .downcast::<gtk::Button>()
        .unwrap();
    button.emit_clicked();
    assert!(!button.is_sensitive());
    assert_eq!(button.label().as_deref(), Some("Tab no longer available"));
    let downloads = super::browser_downloads::DownloadManager::new();
    downloads.observe(control.clone());
    let item = downloads.add(|| {});
    control.record("download", serde_json::json!(item.id()));
    let path = std::env::temp_dir().join(format!("fm-card-{}.txt", Uuid::new_v4()));
    std::fs::write(&path, "download card").unwrap();
    item.set_destination(&path);
    let before = control.status();
    let row = before["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["download_id"] == item.id())
        .unwrap();
    assert!(row["path"].is_null());
    item.finish();
    let after = control.status();
    let row = after["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["download_id"] == item.id())
        .unwrap();
    assert_eq!(row["status"], "complete");
    assert_eq!(row["path"], path.to_str().unwrap());
    std::fs::remove_file(path).unwrap();
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
    assert!(control.result_list.first_child().is_none());
    assert!(control.status()["results"].as_array().unwrap().is_empty());
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
