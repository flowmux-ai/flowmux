// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::ui::chat_runtime::{Change, Prompt, TextKind, Work};

mod markdown;
mod selection;

type OpenFile = dyn Fn(&str);
type Respond = dyn Fn(String, usize, Vec<String>) -> bool;

const PAGE: usize = 24;
const RAIL_LIMIT: usize = 17;
const TEXT_LIMIT: usize = 192 * 1024;

pub(super) struct Transcript {
    pub root: gtk::Box,
    view: Rc<View>,
}

struct View {
    history: gtk::Box,
    scroller: gtk::ScrolledWindow,
    older: gtk::Button,
    latest: gtk::Button,
    rail: gtk::Box,
    rail_preview: gtk::Label,
    track: gtk::ScrolledWindow,
    turns: RefCell<Vec<Turn>>,
    first: Cell<usize>,
    follow: Cell<bool>,
    scrolling: Cell<bool>,
    jump_pending: Cell<bool>,
    jump_target: Cell<Option<usize>>,
    adjustment_scheduled: Cell<bool>,
    rail_buttons: RefCell<Vec<(usize, gtk::Button)>>,
    open_file: RefCell<Rc<OpenFile>>,
    respond: RefCell<Rc<Respond>>,
}

struct Turn {
    root: gtk::Box,
    prompt: gtk::Label,
    activity: gtk::ToggleButton,
    entries: gtk::Box,
    result: gtk::Box,
    started: Instant,
    status: String,
    rows: HashMap<String, Row>,
    open: Cell<bool>,
    pending: HashMap<String, gtk::Box>,
    open_read_run: Option<gtk::Expander>,
    read_runs: Vec<gtk::Expander>,
    explicit_open: Cell<Option<bool>>,
}

enum Row {
    Text(TextRow),
    Work(WorkRow),
}

struct TextRow {
    root: gtk::Box,
    contents: gtk::Box,
    kind: TextKind,
    text: String,
    dirty: bool,
    live: bool,
}

struct WorkRow {
    root: gtk::Expander,
    detail: gtk::Label,
    work: Work,
    read_run: Option<gtk::Expander>,
}

pub(super) fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label
}

fn text_label(text: &str) -> gtk::Label {
    let label = label(text);
    label.set_selectable(true);
    label
}

impl Transcript {
    pub fn new() -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        root.set_vexpand(true);
        let history = gtk::Box::new(gtk::Orientation::Vertical, 16);
        history.set_margin_start(12);
        history.set_margin_end(8);
        history.set_margin_top(12);
        history.set_margin_bottom(12);
        let older = gtk::Button::with_label("Show earlier messages");
        older.set_halign(gtk::Align::Center);
        older.set_visible(false);
        history.append(&older);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(840);
        clamp.set_child(Some(&history));

        let scroller = gtk::ScrolledWindow::builder()
            .child(&clamp)
            .hexpand(true)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        selection::attach(&scroller, &history);
        let latest = gtk::Button::from_icon_name("go-bottom-symbolic");
        latest.add_css_class("circular");
        latest.set_tooltip_text(Some("Jump to latest"));
        latest.update_property(&[gtk::accessible::Property::Label("Jump to latest")]);
        latest.set_halign(gtk::Align::End);
        latest.set_valign(gtk::Align::End);
        latest.set_margin_end(10);
        latest.set_margin_bottom(8);
        latest.set_visible(false);
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&scroller));
        overlay.add_overlay(&latest);
        let rail_preview = label("");
        rail_preview.add_css_class("flowmux-chat-rail-preview");
        rail_preview.set_halign(gtk::Align::End);
        rail_preview.set_valign(gtk::Align::Start);
        rail_preview.set_margin_end(6);
        rail_preview.set_max_width_chars(48);
        rail_preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        rail_preview.set_lines(6);
        rail_preview.set_can_target(false);
        rail_preview.set_visible(false);
        overlay.add_overlay(&rail_preview);
        root.append(&overlay);

        let rail = gtk::Box::new(gtk::Orientation::Vertical, 0);
        rail.add_css_class("flowmux-chat-rail");
        rail.set_valign(gtk::Align::Center);
        let track = gtk::ScrolledWindow::builder()
            .child(&rail)
            .margin_end(5)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vscrollbar_policy(gtk::PolicyType::External)
            .visible(false)
            .build();
        root.append(&track);

        let view = Rc::new(View {
            history,
            scroller,
            older,
            latest,
            rail,
            rail_preview,
            track,
            turns: RefCell::new(Vec::new()),
            first: Cell::new(0),
            follow: Cell::new(true),
            scrolling: Cell::new(false),
            jump_pending: Cell::new(false),
            jump_target: Cell::new(None),
            adjustment_scheduled: Cell::new(false),
            rail_buttons: RefCell::new(Vec::new()),
            open_file: RefCell::new(Rc::new(|_| {})),
            respond: RefCell::new(Rc::new(|_, _, _| false)),
        });
        let weak = Rc::downgrade(&view);
        view.scroller.vadjustment().connect_changed(move |_| {
            if let Some(view) = weak.upgrade() {
                view.schedule_adjustment();
            }
        });
        let weak = Rc::downgrade(&view);
        view.scroller
            .vadjustment()
            .connect_value_changed(move |adjustment| {
                if let Some(view) = weak.upgrade() {
                    if !view.scrolling.get() && !view.jump_pending.get() {
                        view.jump_target.set(None);
                        view.follow.set(distance_to_end(adjustment) < 8.0);
                        view.latest.set_visible(!view.follow.get());
                    }
                    view.highlight_rail();
                }
            });
        let weak = Rc::downgrade(&view);
        view.latest.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                view.jump_target.set(None);
                view.follow.set(true);
                view.schedule_adjustment();
            }
        });
        let weak = Rc::downgrade(&view);
        view.older.connect_clicked(move |_| {
            if let Some(view) = weak.upgrade() {
                let previous = view.first.get();
                view.reveal(previous.saturating_sub(PAGE));
                view.jump(previous, false);
            }
        });
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(&view);
        keys.connect_key_pressed(move |_, key, _, _| {
            let Some(view) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let buttons = view.rail_buttons.borrow();
            let index = buttons
                .iter()
                .position(|(_, button)| button.has_focus())
                .unwrap_or(0);
            let next = match key {
                gtk::gdk::Key::Up => index.saturating_sub(1),
                gtk::gdk::Key::Down => index + 1,
                gtk::gdk::Key::Home => 0,
                gtk::gdk::Key::End => buttons.len().saturating_sub(1),
                _ => return glib::Propagation::Proceed,
            };
            if let Some((_, button)) = buttons.get(next) {
                button.grab_focus();
            }
            glib::Propagation::Stop
        });
        view.rail.add_controller(keys);
        Self { root, view }
    }

    pub fn set_open_file(&self, callback: impl Fn(&str) + 'static) {
        *self.view.open_file.borrow_mut() = Rc::new(callback);
    }

    pub fn set_respond(&self, callback: impl Fn(String, usize, Vec<String>) -> bool + 'static) {
        *self.view.respond.borrow_mut() = Rc::new(callback);
    }

    pub fn begin(&self, prompt: &str) {
        let mut turns = self.view.turns.borrow_mut();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 7);
        let weak = Rc::downgrade(&self.view);
        root.connect_state_flags_changed(move |root, previous| {
            let flag = gtk::StateFlags::FOCUS_WITHIN;
            if previous.contains(flag) != root.state_flags().contains(flag) {
                if let Some(view) = weak.upgrade() {
                    view.highlight_rail();
                }
            }
        });
        let bubble = text_label(prompt);
        bubble.set_halign(gtk::Align::End);
        bubble.set_max_width_chars(70);
        bubble.set_margin_start(28);
        bubble.set_margin_top(4);
        bubble.set_margin_bottom(4);
        bubble.set_css_classes(&["card", "flowmux-chat-prompt"]);
        bubble.set_widget_name("flowmux-chat-user");
        root.append(&bubble);
        let activity = gtk::ToggleButton::with_label("Working…");
        activity.set_active(true);
        activity.set_halign(gtk::Align::Start);
        activity.add_css_class("flat");
        activity.add_css_class("dim-label");
        activity.add_css_class("flowmux-chat-activity");
        activity.update_property(&[gtk::accessible::Property::Label("Show work in this turn")]);
        root.append(&activity);
        let entries = gtk::Box::new(gtk::Orientation::Vertical, 6);
        root.append(&entries);
        let result = gtk::Box::new(gtk::Orientation::Vertical, 5);
        root.append(&result);
        let index = turns.len();
        let weak = Rc::downgrade(&self.view);
        activity.connect_toggled(move |button| {
            if let Some(view) = weak.upgrade() {
                if let Ok(turns) = view.turns.try_borrow() {
                    if let Some(turn) = turns.get(index) {
                        turn.open.set(button.is_active());
                        turn.explicit_open.set(Some(button.is_active()));
                        turn.show_process();
                    }
                }
            }
        });
        turns.push(Turn {
            root: root.clone(),
            prompt: bubble,
            activity,
            entries,
            result,
            started: Instant::now(),
            status: "Working".into(),
            rows: HashMap::new(),
            open: Cell::new(true),
            pending: HashMap::new(),
            open_read_run: None,
            read_runs: Vec::new(),
            explicit_open: Cell::new(None),
        });
        self.view.history.append(&root);
        if self.view.follow.get() {
            let first = turns.len().saturating_sub(PAGE);
            if first > self.view.first.get() {
                self.view.first.set(first);
                for turn in &turns[..first] {
                    turn.root.set_visible(false);
                }
                self.view.older.set_visible(true);
            }
        }
        drop(turns);
        self.view.build_rail();
        self.view.schedule_adjustment();
        let weak = Rc::downgrade(&self.view);
        glib::timeout_add_local(Duration::from_secs(1), move || {
            let Some(view) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let turns = view.turns.borrow();
            if let Some(turn) = turns
                .get(index)
                .filter(|turn| matches!(turn.status.as_str(), "Working" | "Waiting for input"))
            {
                turn.update_status();
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }

    pub fn text(&self, id: String, kind: TextKind, text: String, append: bool) {
        if text.is_empty() {
            return;
        }
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return;
        };
        let row = turn.rows.entry(format!("text:{id}")).or_insert_with(|| {
            turn.open_read_run = None;
            let contents = gtk::Box::new(gtk::Orientation::Vertical, 6);
            let mut root = contents.clone().upcast::<gtk::Widget>();
            if kind == TextKind::Reasoning {
                let expander = gtk::Expander::new(Some("Reasoning summary"));
                expander.set_child(Some(&contents));
                expander.add_css_class("dim-label");
                root = expander.upcast();
            }
            let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
            box_.append(&root);
            box_.set_visible(kind == TextKind::Answer || turn.open.get());
            turn.entries.append(&box_);
            Row::Text(TextRow {
                root: box_,
                contents,
                kind,
                text: String::new(),
                dirty: false,
                live: true,
            })
        });
        if let Row::Text(row) = row {
            if !append {
                row.text.clear();
            }
            if row.text.len() < TEXT_LIMIT {
                let mut len = text.len().min(TEXT_LIMIT - row.text.len());
                while !text.is_char_boundary(len) {
                    len -= 1;
                }
                row.text.push_str(&text[..len]);
                if len < text.len() {
                    row.text.push_str("\n… (truncated)");
                }
                row.dirty = true;
            }
            row.live = append;
        }
    }

    pub fn flush_text(&self) {
        if let Some(turn) = self.view.turns.borrow_mut().last_mut() {
            turn.flush();
        }
        self.view.schedule_adjustment();
    }

    pub fn work(&self, work: Work) {
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return;
        };
        let row = turn
            .rows
            .entry(format!("work:{}", work.id))
            .or_insert_with(|| {
                let root = gtk::Expander::new(None);
                let detail = text_label("");
                detail.add_css_class("monospace");
                detail.set_margin_start(12);
                let scroller = gtk::ScrolledWindow::builder()
                    .child(&detail)
                    .max_content_height(180)
                    .propagate_natural_height(true)
                    .hscrollbar_policy(gtk::PolicyType::Never)
                    .build();
                root.set_child(Some(&scroller));
                let read_run = if work.read {
                    let group = turn.open_read_run.get_or_insert_with(|| {
                        let group = gtk::Expander::new(None);
                        let contents = gtk::Box::new(gtk::Orientation::Vertical, 4);
                        contents.set_margin_start(12);
                        group.set_child(Some(&contents));
                        turn.entries.append(&group);
                        turn.read_runs.push(group.clone());
                        group
                    });
                    let contents = group.child().and_downcast::<gtk::Box>().unwrap();
                    contents.append(&root);
                    group.set_label(Some(&format!(
                        "Read activity · {}",
                        contents.observe_children().n_items()
                    )));
                    Some(group.clone())
                } else {
                    turn.open_read_run = None;
                    turn.entries.append(&root);
                    None
                };
                Row::Work(WorkRow {
                    root,
                    detail,
                    work: work.clone(),
                    read_run,
                })
            });
        if let Row::Work(row) = row {
            row.work = work;
            row.update(turn.open.get());
        }
    }

    pub fn prompt(&self, prompt: Prompt) {
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return;
        };
        turn.open_read_run = None;
        let card = gtk::Box::new(gtk::Orientation::Vertical, 6);
        card.add_css_class("card");
        card.set_widget_name("flowmux-chat-approval");
        let heading = text_label(&prompt.title);
        heading.add_css_class("heading");
        card.append(&heading);
        if !prompt.detail.is_empty() {
            let detail = text_label(&prompt.detail);
            detail.add_css_class("monospace");
            let scroller = gtk::ScrolledWindow::builder()
                .child(&detail)
                .max_content_height(180)
                .propagate_natural_height(true)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .build();
            card.append(&scroller);
        }
        let mut inputs = Vec::new();
        for question in prompt.questions {
            card.append(&text_label(&question.title));
            let input = gtk::Entry::new();
            input.set_placeholder_text(Some("Your answer"));
            if !question.options.is_empty() {
                let mut labels = vec!["Choose an answer…"];
                labels.extend(question.options.iter().map(String::as_str));
                let choices = gtk::DropDown::from_strings(&labels);
                let entry = input.clone();
                choices.connect_selected_notify(move |choice| {
                    if choice.selected() != 0 {
                        if let Some(value) =
                            choice.selected_item().and_downcast::<gtk::StringObject>()
                        {
                            entry.set_text(&value.string());
                        }
                    }
                });
                card.append(&choices);
            }
            card.append(&input);
            inputs.push(input);
        }
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for (index, title) in prompt.choices.iter().enumerate() {
            let button = gtk::Button::with_label(title);
            let id = prompt.id.clone();
            let inputs = inputs.clone();
            let weak_actions = actions.downgrade();
            let respond = self.view.respond.borrow().clone();
            button.connect_clicked(move |_| {
                let answers: Vec<_> = inputs
                    .iter()
                    .map(|input| input.text().to_string())
                    .collect();
                if let Some(index) = answers.iter().position(|answer| answer.trim().is_empty()) {
                    inputs[index].grab_focus();
                    return;
                }
                if respond(id.clone(), index, answers) {
                    if let Some(actions) = weak_actions.upgrade() {
                        actions.set_sensitive(false);
                    }
                }
            });
            actions.append(&button);
        }
        card.append(&actions);
        turn.entries.append(&card);
        turn.pending.insert(prompt.id, card);
        turn.status = "Waiting for input".into();
        turn.update_status();
        drop(turns);
        self.view.build_rail();
    }

    pub fn has_pending_input(&self) -> bool {
        self.view
            .turns
            .borrow()
            .last()
            .is_some_and(|turn| !turn.pending.is_empty())
    }

    pub fn resolve(&self, id: &str, result: &str) -> bool {
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return false;
        };
        if let Some(card) = turn.pending.remove(id) {
            receipt(&card, result);
        }
        let ready = turn.pending.is_empty();
        if ready {
            turn.status = "Working".into();
            turn.update_status();
        }
        drop(turns);
        self.view.build_rail();
        ready
    }

    pub fn finish(&self, status: &str, detail: Option<&str>) {
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return;
        };
        turn.status = status.into();
        turn.update_status();
        for card in turn.pending.drain().map(|(_, card)| card) {
            receipt(&card, "Request closed");
        }
        for row in turn.rows.values_mut() {
            match row {
                Row::Text(text) => {
                    text.live = false;
                    text.dirty = true;
                }
                Row::Work(work) if work.work.running => {
                    work.work.running = false;
                    work.work.changes.clear();
                    work.update(false);
                }
                Row::Work(_) => {}
            }
        }
        let expanded = turn.explicit_open.get().unwrap_or(false);
        turn.open.set(expanded);
        turn.activity.set_active(expanded);
        turn.show_process();
        turn.flush();
        let mut changes = BTreeMap::<&str, Vec<&Change>>::new();
        let mut checks = Vec::new();
        for row in turn.rows.values() {
            if let Row::Work(row) = row {
                if !row.work.failed {
                    for change in &row.work.changes {
                        changes.entry(&change.path).or_default().push(change);
                    }
                }
                if let Some(check) = row
                    .work
                    .check
                    .as_ref()
                    .filter(|_| !row.work.read || row.work.failed)
                {
                    checks.push((check, row.work.failed));
                }
            }
        }
        if !changes.is_empty() {
            let files = changes.len();
            let totals = changes
                .values()
                .flatten()
                .try_fold((0, 0), |(added, removed), edit| {
                    let (a, r) = diff_counts(&edit.diff)?;
                    Some((added + a, removed + r))
                });
            let title = match totals {
                Some((added, removed)) => format!("Changed files · {files} · +{added} −{removed}"),
                None => format!("Changed files · {files}"),
            };
            let heading = gtk::Expander::new(Some(&title));
            let list = gtk::Box::new(gtk::Orientation::Vertical, 5);
            let open_file = self.view.open_file.borrow().clone();
            for (path, edits) in changes {
                let item = gtk::Box::new(gtk::Orientation::Vertical, 3);
                let button = gtk::Button::with_label(path);
                button.set_halign(gtk::Align::Start);
                button.add_css_class("flat");
                let file = path.to_owned();
                let open_file = open_file.clone();
                button.connect_clicked(move |_| open_file(&file));
                item.append(&button);
                for edit in edits.iter().filter(|edit| !edit.diff.is_empty()) {
                    let diff = gtk::Expander::new(Some(&diff_label(&edit.diff)));
                    diff.set_child(Some(&markdown::code("diff", &edit.diff)));
                    item.append(&diff);
                }
                list.append(&item);
            }
            heading.set_child(Some(&list));
            turn.result.append(&heading);
        }
        checks.sort_unstable();
        for (check, failed) in checks {
            let check = text_label(&format!("{} {check}", if failed { "✕" } else { "✓" }));
            check.add_css_class(if failed { "error" } else { "dim-label" });
            turn.result.append(&check);
        }
        if let Some(detail) = detail.filter(|detail| !detail.is_empty()) {
            turn.result.append(&text_label(detail));
        }
        drop(turns);
        self.view.build_rail();
        self.view.schedule_adjustment();
    }
}

impl Turn {
    fn update_status(&self) {
        let elapsed = self.started.elapsed().as_secs();
        self.activity
            .set_label(&format!("{} · {elapsed}s", self.status));
    }

    fn show_process(&self) {
        for group in &self.read_runs {
            group.set_visible(self.open.get() || group.has_css_class("error"));
        }
        for row in self.rows.values() {
            match row {
                Row::Text(row) if row.kind == TextKind::Reasoning => {
                    row.root.set_visible(self.open.get())
                }
                Row::Work(row) => row.root.set_visible(self.open.get() || row.work.failed),
                _ => {}
            }
        }
    }

    fn flush(&mut self) {
        for row in self.rows.values_mut() {
            let Row::Text(text) = row else {
                continue;
            };
            if !text.dirty {
                continue;
            }
            text.dirty = false;
            if text.live {
                if let Some(label) = text.contents.first_child().and_downcast::<gtk::Label>() {
                    label.set_label(&text.text);
                } else {
                    clear(&text.contents);
                    text.contents.append(&text_label(&text.text));
                }
            } else {
                clear(&text.contents);
                markdown::render(&text.contents, &text.text);
            }
        }
    }
}

impl WorkRow {
    fn update(&self, visible: bool) {
        let title = if self.work.failed {
            format!("Failed · {}", self.work.title)
        } else if self.work.running {
            format!("Running · {}", self.work.title)
        } else {
            self.work.title.clone()
        };
        self.root.set_label(Some(&title));
        self.detail.set_label(&self.work.detail);
        self.root.set_visible(visible || self.work.failed);
        if self.work.failed {
            self.root.add_css_class("error");
            if let Some(group) = &self.read_run {
                group.add_css_class("error");
                group.set_expanded(true);
            }
        }
        if let Some(group) = &self.read_run {
            group.set_visible(visible || group.has_css_class("error"));
        }
    }
}

impl View {
    fn reveal(&self, index: usize) {
        if index >= self.first.get() {
            return;
        }
        let turns = self.turns.borrow();
        for turn in &turns[index..self.first.get()] {
            turn.root.set_visible(true);
        }
        self.first.set(index);
        self.older.set_visible(index > 0);
    }

    fn schedule_adjustment(self: &Rc<Self>) {
        if self.adjustment_scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(view) = weak.upgrade() else {
                return;
            };
            view.adjustment_scheduled.set(false);
            if view.follow.get() && !view.jump_pending.get() {
                let adjustment = view.scroller.vadjustment();
                view.scrolling.set(true);
                adjustment.set_value(adjustment.upper() - adjustment.page_size());
                view.scrolling.set(false);
                view.latest.set_visible(false);
            }
            view.highlight_rail();
        });
    }

    fn jump(self: &Rc<Self>, index: usize, find_prompt: bool) {
        self.reveal(index);
        self.follow.set(false);
        self.jump_target.set(Some(index));
        self.jump_pending.set(true);
        self.latest.set_visible(true);
        let weak = Rc::downgrade(self);
        let attempts = Cell::new(0);
        self.history.add_tick_callback(move |_, _| {
            let Some(view) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let turns = view.turns.borrow();
            let target = turns.get(index).and_then(|turn| {
                if find_prompt {
                    turn.pending
                        .values()
                        .next()
                        .cloned()
                        .or_else(|| Some(turn.root.clone()))
                } else {
                    Some(turn.root.clone())
                }
            });
            let bounds = target.and_then(|target| target.compute_bounds(&view.history));
            attempts.set(attempts.get() + 1);
            if let Some(bounds) = bounds.filter(|bounds| bounds.height() > 0.0) {
                view.scroller
                    .vadjustment()
                    .set_value(bounds.y() as f64 + f64::from(view.history.margin_top()));
            } else if attempts.get() < 10 {
                return glib::ControlFlow::Continue;
            }
            drop(turns);
            view.jump_pending.set(false);
            view.highlight_rail();
            glib::ControlFlow::Break
        });
    }

    fn build_rail(self: &Rc<Self>) {
        let turns = self.turns.borrow();
        let count = turns.len();
        let focused = self
            .rail_buttons
            .borrow()
            .iter()
            .find(|(_, button)| button.has_focus())
            .map(|(index, _)| *index);
        let selected = self.active_turn(&turns);
        let indices = rail_indices(count, selected, focused);
        // Rebuild at turn boundaries, preserving the keyboard target when it is still present.
        self.rail_preview.set_visible(false);
        clear(&self.rail);
        let mut buttons = Vec::new();
        let mut restore_focus = None;
        for index in indices {
            let turn = &turns[index];
            let pending = !turn.pending.is_empty();
            let button = gtk::Button::new();
            button.add_css_class("flat");
            if pending {
                button.add_css_class("waiting");
            } else if matches!(turn.status.as_str(), "Failed" | "Disconnected" | "Not sent") {
                button.add_css_class("failed");
            }
            let mark = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            mark.set_halign(gtk::Align::Center);
            mark.set_valign(gtk::Align::Center);
            mark.add_css_class("flowmux-chat-rail-mark");
            button.set_child(Some(&mark));
            let motion = gtk::EventControllerMotion::new();
            let rail_preview = self.rail_preview.clone();
            let prompt = turn.prompt.clone();
            motion.connect_enter(move |motion, _, _| {
                let Some(overlay) = rail_preview.parent() else {
                    return;
                };
                let Some(bounds) = motion
                    .widget()
                    .and_then(|button| button.compute_bounds(&overlay))
                else {
                    return;
                };
                rail_preview.set_label(&prompt.label());
                rail_preview.set_margin_top(0);
                rail_preview.set_visible(true);
                let (_, width, _, _) = rail_preview.measure(gtk::Orientation::Horizontal, -1);
                let width = width.min((overlay.width() - rail_preview.margin_end()).max(1));
                let (_, height, _, _) = rail_preview.measure(gtk::Orientation::Vertical, width);
                let top = bounds.y() + (bounds.height() - height as f32) / 2.0;
                rail_preview
                    .set_margin_top((top as i32).clamp(0, (overlay.height() - height).max(0)));
            });
            let rail_preview = self.rail_preview.clone();
            motion.connect_leave(move |_| rail_preview.set_visible(false));
            button.add_controller(motion);
            let name = format!(
                "Message {} · {} · {}",
                index + 1,
                turn.status,
                preview(&turn.prompt.label(), 160)
            );
            button.update_property(&[
                gtk::accessible::Property::Label(&name),
                gtk::accessible::Property::Description(&name),
            ]);
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(view) = weak.upgrade() {
                    view.jump(index, pending);
                }
            });
            self.rail.append(&button);
            if focused == Some(index) {
                restore_focus = Some(button.clone());
            }
            buttons.push((index, button));
        }
        self.track.set_visible(count > 0);
        *self.rail_buttons.borrow_mut() = buttons;
        drop(turns);
        if let Some(button) = restore_focus {
            button.grab_focus();
        }
        self.highlight_rail();
    }

    fn active_turn(&self, turns: &[Turn]) -> Option<usize> {
        if let Some(target) = self.jump_target.get() {
            return Some(target);
        }
        if self.follow.get() {
            return turns.len().checked_sub(1);
        }
        let position = self.scroller.vadjustment().value();
        turns
            .iter()
            .enumerate()
            .skip(self.first.get())
            .rfind(|(_, turn)| {
                self.turn_bounds(turn)
                    .is_some_and(|(top, _)| top <= position + 8.0)
            })
            .map(|(index, _)| index)
            .or_else(|| (self.first.get() < turns.len()).then_some(self.first.get()))
    }

    fn turn_bounds(&self, turn: &Turn) -> Option<(f64, f64)> {
        let bounds = turn.root.compute_bounds(&self.history)?;
        let top = f64::from(bounds.y()) + f64::from(self.history.margin_top());
        Some((top, top + f64::from(bounds.height())))
    }

    fn highlight_rail(self: &Rc<Self>) {
        let Ok(turns) = self.turns.try_borrow() else {
            return;
        };
        let active = self.active_turn(&turns);
        let Ok(buttons) = self.rail_buttons.try_borrow() else {
            return;
        };
        if active.is_some_and(|active| !buttons.iter().any(|(index, _)| active == *index)) {
            drop(buttons);
            drop(turns);
            self.build_rail();
            return;
        }
        let adjustment = self.scroller.vadjustment();
        let top = adjustment.value();
        let bottom = top + adjustment.page_size();
        for (index, button) in buttons.iter() {
            let turn = &turns[*index];
            let visible = *index >= self.first.get()
                && self
                    .turn_bounds(turn)
                    .is_some_and(|(start, end)| end > top && start < bottom);
            for (class, enabled) in [
                ("current", Some(*index) == active),
                ("in-view", visible),
                (
                    "turn-focused",
                    turn.root
                        .state_flags()
                        .contains(gtk::StateFlags::FOCUS_WITHIN),
                ),
            ] {
                if enabled {
                    button.add_css_class(class);
                } else {
                    button.remove_css_class(class);
                }
            }
        }
    }
}

fn clear(box_: &gtk::Box) {
    while let Some(widget) = box_.first_child() {
        box_.remove(&widget);
    }
}

fn receipt(card: &gtk::Box, result: &str) {
    if let Some(actions) = card.last_child() {
        card.remove(&actions);
    }
    let result = text_label(result);
    result.add_css_class("dim-label");
    card.append(&result);
}

fn preview(text: &str, limit: usize) -> String {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim();
    first.chars().take(limit).collect()
}

fn diff_label(diff: &str) -> String {
    match diff_counts(diff) {
        Some((added, removed)) => format!("Diff · +{added} −{removed}"),
        None => "Partial diff".into(),
    }
}

fn diff_counts(diff: &str) -> Option<(usize, usize)> {
    if diff.is_empty() || diff.contains("\n… (truncated)") {
        return None;
    }
    let added = diff
        .lines()
        .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
        .count();
    let removed = diff
        .lines()
        .filter(|line| line.starts_with('-') && !line.starts_with("---"))
        .count();
    Some((added, removed))
}

fn distance_to_end(adjustment: &gtk::Adjustment) -> f64 {
    adjustment.upper() - adjustment.value() - adjustment.page_size()
}

fn rail_indices(count: usize, active: Option<usize>, focused: Option<usize>) -> Vec<usize> {
    if count <= RAIL_LIMIT {
        return (0..count).collect();
    }
    let mut indices: Vec<_> = (0..RAIL_LIMIT)
        .map(|slot| slot * (count - 1) / (RAIL_LIMIT - 1))
        .collect();
    for turn in [active, focused].into_iter().flatten() {
        if turn >= count || indices.contains(&turn) {
            continue;
        }
        let nearest = (1..RAIL_LIMIT - 1)
            .filter(|&slot| ![active, focused].contains(&Some(indices[slot])))
            .min_by_key(|&slot| indices[slot].abs_diff(turn))
            .unwrap();
        indices[nearest] = turn;
    }
    indices.sort_unstable();
    indices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rail_retains_endpoints_and_the_active_prompt() {
        let indices = rail_indices(200, Some(87), Some(88));
        assert_eq!(indices.len(), RAIL_LIMIT);
        assert_eq!(indices.first(), Some(&0));
        assert_eq!(indices.last(), Some(&199));
        assert!(indices.contains(&87));
        assert!(indices.contains(&88));
    }
}
