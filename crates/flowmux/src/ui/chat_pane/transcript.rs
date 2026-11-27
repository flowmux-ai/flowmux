// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::ui::chat_runtime::{Change, Prompt, TextKind, Work};
use flowmux_state::chat_history::Record;

mod highlight;
mod markdown;
mod media;
mod selection;

type OpenFile = dyn Fn(&str);
type Respond = dyn Fn(String, usize, Vec<String>) -> bool;

const PAGE: usize = 24;
const RAIL_LIMIT: usize = 20;
// Narrower conversations keep their width for messages; the outline still navigates.
const RAIL_MIN_WIDTH: i32 = 512;
const TEXT_LIMIT: usize = 192 * 1024;

pub(super) struct Transcript {
    pub root: gtk::Box,
    pub outline: gtk::MenuButton,
    view: Rc<View>,
}

struct View {
    root: gtk::Box,
    /// Relative media paths in answers resolve against the chat's working directory.
    base: std::path::PathBuf,
    outline: gtk::MenuButton,
    history: gtk::Box,
    scroller: gtk::ScrolledWindow,
    older: gtk::Button,
    latest: gtk::Button,
    rail: gtk::Box,
    rail_preview: gtk::Label,
    rail_fade: RefCell<Option<glib::SourceId>>,
    track: gtk::ScrolledWindow,
    turns: RefCell<Vec<Turn>>,
    first: Cell<usize>,
    follow: Cell<bool>,
    scrolling: Cell<bool>,
    jump_pending: Cell<bool>,
    jump_target: Cell<Option<usize>>,
    adjustment_scheduled: Cell<bool>,
    text_flush_scheduled: Cell<bool>,
    replaying: Cell<bool>,
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
    elapsed: Option<u64>,
    status: String,
    rows: HashMap<String, Row>,
    answer: Option<String>,
    open: Cell<bool>,
    pending: HashMap<String, gtk::Box>,
    runs: Vec<Run>,
    open_run: Option<usize>,
    explicit_open: Cell<Option<bool>>,
}

/// Consecutive tool calls; a second call moves both rows into one summary row.
#[derive(Default)]
struct Run {
    keys: Vec<String>,
    group: Option<gtk::Expander>,
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
    blocks: markdown::Blocks,
    dirty: bool,
    live: bool,
}

struct WorkRow {
    root: gtk::Expander,
    detail: gtk::Label,
    work: Work,
    run: Option<usize>,
}

pub(super) fn label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label
}

const FILE_LINK: &str = "flowmux-file:";

fn text_label(text: &str) -> gtk::Label {
    let label = label(text);
    label.set_selectable(true);
    label.connect_activate_link(|label, uri| match uri.strip_prefix(FILE_LINK) {
        Some(path) => {
            let _ = label.activate_action("chat.open-file", Some(&linked_file(path).to_variant()));
            glib::Propagation::Stop
        }
        None => glib::Propagation::Proceed,
    });
    label
}

/// Drops the `#L12` or `:12:3` position agents append to file links.
fn linked_file(target: &str) -> &str {
    let mut path = target.split('#').next().unwrap_or(target);
    for _ in 0..2 {
        if let Some((head, tail)) = path.rsplit_once(':') {
            if !tail.is_empty() && tail.bytes().all(|byte| byte.is_ascii_digit()) {
                path = head;
            }
        }
    }
    path
}

fn copy_button(action: &str) -> gtk::Button {
    let button = gtk::Button::from_icon_name("edit-copy-symbolic");
    button.set_tooltip_text(Some(action));
    button.update_property(&[gtk::accessible::Property::Label(action)]);
    button.add_css_class("flat");
    button
}

fn bubble(text: &str) -> gtk::Label {
    let bubble = text_label(text);
    bubble.set_halign(gtk::Align::End);
    bubble.set_max_width_chars(70);
    bubble.set_margin_start(28);
    bubble.set_margin_top(4);
    bubble.set_margin_bottom(4);
    bubble.set_css_classes(&["card", "flowmux-chat-prompt"]);
    bubble.set_widget_name("flowmux-chat-user");
    if super::commands::parse(text).is_some() {
        bubble.add_css_class("flowmux-chat-command-bubble");
    }
    bubble
}

/// An expander whose title ellipsizes instead of setting the transcript's minimum width.
fn expander(title: &str) -> gtk::Expander {
    let expander = gtk::Expander::new(None);
    let heading = gtk::Label::new(None);
    heading.set_xalign(0.0);
    heading.set_ellipsize(gtk::pango::EllipsizeMode::End);
    expander.set_label_widget(Some(&heading));
    set_title(&expander, title);
    expander
}

fn set_title(expander: &gtk::Expander, title: &str) {
    if let Some(heading) = expander.label_widget().and_downcast::<gtk::Label>() {
        heading.set_label(title);
        heading.set_tooltip_text(Some(title));
    }
}

impl Transcript {
    pub fn new(base: std::path::PathBuf) -> Self {
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
            .overlay_scrolling(false)
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

        let outline_search = gtk::SearchEntry::new();
        outline_search.set_placeholder_text(Some("Search messages"));
        outline_search.update_property(&[gtk::accessible::Property::Label("Search messages")]);
        let outline_list = gtk::ListBox::new();
        outline_list.set_selection_mode(gtk::SelectionMode::None);
        outline_list.update_property(&[gtk::accessible::Property::Label("Conversation outline")]);
        let outline_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
        outline_box.append(&outline_search);
        outline_box.append(
            &gtk::ScrolledWindow::builder()
                .child(&outline_list)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .max_content_height(360)
                .min_content_width(240)
                .build(),
        );
        let outline_popover = gtk::Popover::new();
        outline_popover.set_child(Some(&outline_box));
        let outline = gtk::MenuButton::builder()
            .icon_name("view-list-symbolic")
            .tooltip_text("Conversation outline")
            .popover(&outline_popover)
            .visible(false)
            .build();
        outline.add_css_class("flat");
        outline.update_property(&[gtk::accessible::Property::Label("Conversation outline")]);

        let view = Rc::new(View {
            root: root.clone(),
            base,
            outline: outline.clone(),
            history,
            scroller,
            older,
            latest,
            rail,
            rail_preview,
            rail_fade: RefCell::new(None),
            track,
            turns: RefCell::new(Vec::new()),
            first: Cell::new(0),
            follow: Cell::new(true),
            scrolling: Cell::new(false),
            jump_pending: Cell::new(false),
            jump_target: Cell::new(None),
            adjustment_scheduled: Cell::new(false),
            text_flush_scheduled: Cell::new(false),
            replaying: Cell::new(false),
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
        // File links in answers open through the chat's own file handler.
        let actions = gtk::gio::SimpleActionGroup::new();
        let open = gtk::gio::SimpleAction::new("open-file", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(&view);
        open.connect_activate(move |_, path| {
            if let (Some(view), Some(path)) = (weak.upgrade(), path.and_then(|p| p.get::<String>()))
            {
                let open_file = view.open_file.borrow().clone();
                open_file(&path);
            }
        });
        actions.add_action(&open);
        view.root.insert_action_group("chat", Some(&actions));
        // Width changes reflow messages and decide whether the rail fits.
        let weak = Rc::downgrade(&view);
        view.scroller.hadjustment().connect_changed(move |_| {
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
                        view.wake_rail();
                    }
                    view.highlight_rail();
                }
            });
        let filter = outline_search.clone();
        outline_list.set_filter_func(move |row| {
            let query = filter.text().to_lowercase();
            query.is_empty()
                || row
                    .child()
                    .and_downcast::<gtk::Label>()
                    .is_some_and(|text| text.label().to_lowercase().contains(&query))
        });
        let list = outline_list.clone();
        outline_search.connect_search_changed(move |_| list.invalidate_filter());
        let list = outline_list.clone();
        outline_search.connect_activate(move |_| {
            // Enter can arrive before the delayed search-changed signal.
            list.invalidate_filter();
            let mut index = 0;
            while let Some(row) = list.row_at_index(index) {
                if row.is_child_visible() {
                    row.emit_activate();
                    break;
                }
                index += 1;
            }
        });
        let weak = Rc::downgrade(&view);
        let popover = outline_popover.clone();
        outline_list.connect_row_activated(move |_, row| {
            popover.popdown();
            if let Some(view) = weak.upgrade() {
                let index = row.index() as usize;
                let pending = view
                    .turns
                    .borrow()
                    .get(index)
                    .is_some_and(|turn| !turn.pending.is_empty());
                view.jump(index, pending);
            }
        });
        let weak = Rc::downgrade(&view);
        outline_popover.connect_show(move |_| {
            let Some(view) = weak.upgrade() else {
                return;
            };
            outline_search.set_text("");
            outline_list.remove_all();
            for (index, turn) in view.turns.borrow().iter().enumerate() {
                let text = label(&format!(
                    "{}. {}",
                    index + 1,
                    excerpt(&turn.prompt.label(), 160)
                ));
                text.set_wrap(false);
                text.set_ellipsize(gtk::pango::EllipsizeMode::End);
                text.set_max_width_chars(48);
                text.set_margin_top(4);
                text.set_margin_bottom(4);
                if !turn.pending.is_empty() {
                    text.add_css_class("warning");
                } else if turn.failed() {
                    text.add_css_class("error");
                }
                outline_list.append(&text);
            }
            outline_search.grab_focus();
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
        Self {
            root,
            outline,
            view,
        }
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
        let bubble = bubble(prompt);
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
            elapsed: None,
            status: "Working".into(),
            rows: HashMap::new(),
            answer: None,
            open: Cell::new(true),
            pending: HashMap::new(),
            runs: Vec::new(),
            open_run: None,
            explicit_open: Cell::new(None),
        });
        self.view.history.append(&root);
        if self.view.follow.get() {
            let first = turns.len().saturating_sub(PAGE);
            let previous = self.view.first.get();
            if first > previous {
                self.view.first.set(first);
                for turn in &turns[previous..first] {
                    turn.root.set_visible(false);
                }
                self.view.older.set_visible(true);
            }
        }
        drop(turns);
        self.view.build_rail();
        self.view.schedule_adjustment();
        if self.view.replaying.get() {
            return;
        }
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

    pub fn replay(&self, records: Vec<Record>, running: bool) {
        for turn in self.view.turns.borrow_mut().drain(..) {
            self.view.history.remove(&turn.root);
        }
        self.view.first.set(0);
        self.view.follow.set(true);
        self.view.older.set_visible(false);
        self.view.replaying.set(true);
        let mut active = false;
        for record in records {
            match record {
                Record::Begin {
                    prompt,
                    steer: true,
                } if active => self.steer(&prompt),
                Record::Begin { prompt, .. } => {
                    if active {
                        self.finish("Interrupted", None, 0);
                    }
                    self.begin(&prompt);
                    active = true;
                }
                Record::Text {
                    id,
                    kind,
                    text,
                    append,
                } if active => self.text(id, kind, text, append),
                Record::Work(work) if active => self.work(work),
                Record::Prompt(prompt) if active => self.prompt(prompt),
                Record::Resolved { id, label, answers } if active => {
                    self.resolve(&id, &label, &answers);
                }
                Record::Finished {
                    status,
                    detail,
                    elapsed,
                } if active => {
                    self.finish(&status, detail.as_deref(), elapsed);
                    active = false;
                }
                _ => {}
            }
        }
        if active && !running {
            self.finish("Interrupted", None, 0);
        }
        self.view.replaying.set(false);
        self.view.build_rail();
        self.view.schedule_adjustment();
    }

    pub fn text(&self, id: String, kind: TextKind, text: String, append: bool) {
        if text.is_empty() {
            return;
        }
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return;
        };
        let key = format!("text:{id}");
        if kind == TextKind::Answer {
            turn.answer = Some(key.clone());
        }
        let row = turn.rows.entry(key).or_insert_with(|| {
            turn.open_run = None;
            let contents = gtk::Box::new(gtk::Orientation::Vertical, 6);
            let mut root = contents.clone().upcast::<gtk::Widget>();
            if kind == TextKind::Reasoning {
                let expander = expander("Reasoning summary");
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
                blocks: markdown::Blocks::default(),
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

    /// Shows a message that joined the running turn.
    pub fn steer(&self, text: &str) {
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return;
        };
        turn.flush(false, &self.view.base);
        turn.open_run = None;
        turn.entries.append(&bubble(text));
        drop(turns);
        self.view.schedule_adjustment();
    }

    pub fn flush_text(&self) {
        if self.view.text_flush_scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(&self.view);
        glib::timeout_add_local_once(Duration::from_millis(32), move || {
            if let Some(view) = weak.upgrade() {
                view.text_flush_scheduled.set(false);
                if let Some(turn) = view.turns.borrow_mut().last_mut() {
                    turn.flush(false, &view.base);
                }
                view.schedule_adjustment();
            }
        });
    }

    pub fn work(&self, work: Work) {
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return;
        };
        let key = format!("work:{}", work.id);
        if !turn.rows.contains_key(&key) {
            let root = expander("");
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
            // The plan updates in place, so it stays outside tool runs.
            let run = if work.id == "turn-plan" {
                turn.open_run = None;
                turn.entries.append(&root);
                None
            } else {
                Some(turn.attach(&key, &root))
            };
            turn.rows.insert(
                key.clone(),
                Row::Work(WorkRow {
                    root,
                    detail,
                    work: work.clone(),
                    run,
                }),
            );
        }
        let open = turn.open.get();
        let Some(Row::Work(row)) = turn.rows.get_mut(&key) else {
            return;
        };
        row.work = work;
        let run = row.run;
        let group = run.and_then(|index| turn.runs[index].group.clone());
        row.update(open, group.as_ref());
        if let (Some(index), Some(group)) = (run, group) {
            set_title(&group, &turn.run_title(index));
        }
    }

    pub fn prompt(&self, prompt: Prompt) {
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return;
        };
        turn.open_run = None;
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
        // Choices wrap onto more lines in a narrow chat.
        let actions = gtk::FlowBox::new();
        actions.set_selection_mode(gtk::SelectionMode::None);
        actions.set_halign(gtk::Align::Start);
        actions.set_column_spacing(6);
        actions.set_row_spacing(6);
        actions.set_max_children_per_line(prompt.choices.len().max(1) as u32);
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

    pub fn resolve(&self, id: &str, result: &str, answers: &[String]) -> bool {
        let mut turns = self.view.turns.borrow_mut();
        let Some(turn) = turns.last_mut() else {
            return false;
        };
        if let Some(card) = turn.pending.remove(id) {
            receipt(&card, result, answers);
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

    pub fn elapsed(&self) -> u64 {
        self.view
            .turns
            .borrow()
            .last()
            .map_or(0, |turn| turn.started.elapsed().as_secs())
    }

    pub fn finish(&self, status: &str, detail: Option<&str>, elapsed: u64) {
        let mut turns = self.view.turns.borrow_mut();
        let index = turns.len().saturating_sub(1);
        let Some(turn) = turns.last_mut() else {
            return;
        };
        turn.status = status.into();
        turn.elapsed = Some(elapsed);
        turn.update_status();
        for card in turn.pending.drain().map(|(_, card)| card) {
            receipt(&card, "Request closed", &[]);
        }
        for row in turn.rows.values_mut() {
            match row {
                Row::Work(work) if work.work.running => {
                    work.work.running = false;
                    work.work.changes.clear();
                    let group = work.run.and_then(|index| turn.runs[index].group.as_ref());
                    work.update(false, group);
                }
                _ => {}
            }
        }
        for index in 0..turn.runs.len() {
            if let Some(group) = turn.runs[index].group.clone() {
                set_title(&group, &turn.run_title(index));
            }
        }
        let expanded = turn.explicit_open.get().unwrap_or(false);
        turn.open.set(expanded);
        turn.activity.set_active(expanded);
        turn.show_process();
        turn.flush(true, &self.view.base);
        if turn.answer().is_some() {
            let copy = copy_button("Copy response");
            copy.set_halign(gtk::Align::Start);
            let weak = Rc::downgrade(&self.view);
            copy.connect_clicked(move |button| {
                if let Some(view) = weak.upgrade() {
                    if let Some(answer) = view.turns.borrow().get(index).and_then(Turn::answer) {
                        button.clipboard().set_text(&answer.text);
                    }
                }
            });
            turn.result.append(&copy);
        }
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
            let heading = expander(&title);
            let list = gtk::Box::new(gtk::Orientation::Vertical, 5);
            let open_file = self.view.open_file.borrow().clone();
            for (path, edits) in changes {
                let item = gtk::Box::new(gtk::Orientation::Vertical, 3);
                let name = gtk::Label::new(Some(path));
                name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                name.set_xalign(0.0);
                let button = gtk::Button::builder()
                    .child(&name)
                    .tooltip_text(path)
                    .halign(gtk::Align::Start)
                    .build();
                button.add_css_class("flat");
                let file = path.to_owned();
                let open_file = open_file.clone();
                button.connect_clicked(move |_| open_file(&file));
                item.append(&button);
                for edit in edits.iter().filter(|edit| !edit.diff.is_empty()) {
                    let diff = expander(&diff_label(&edit.diff));
                    diff.set_child(Some(&markdown::code("diff", &edit.diff)));
                    item.append(&diff);
                }
                list.append(&item);
            }
            heading.set_child(Some(&list));
            turn.result.append(&heading);
        }
        if !checks.is_empty() {
            checks.sort_unstable();
            let failures = checks.iter().filter(|(_, failed)| *failed).count();
            let title = if failures > 0 {
                format!("Checks · {} · {failures} failed", checks.len())
            } else {
                format!("Checks · {}", checks.len())
            };
            let heading = expander(&title);
            let list = gtk::Box::new(gtk::Orientation::Vertical, 5);
            for (check, failed) in checks {
                let check = text_label(&format!("{} {check}", if failed { "✕" } else { "✓" }));
                check.add_css_class(if failed { "error" } else { "dim-label" });
                list.append(&check);
            }
            heading.set_child(Some(&list));
            turn.result.append(&heading);
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
    fn answer(&self) -> Option<&TextRow> {
        match self.rows.get(self.answer.as_ref()?) {
            Some(Row::Text(row)) => Some(row),
            _ => None,
        }
    }

    fn update_status(&self) {
        let elapsed = self
            .elapsed
            .unwrap_or_else(|| self.started.elapsed().as_secs());
        self.activity
            .set_label(&format!("{} · {elapsed}s", self.status));
    }

    fn failed(&self) -> bool {
        matches!(self.status.as_str(), "Failed" | "Disconnected" | "Not sent")
    }

    /// Adds a tool row to the open run, grouping it with the previous row when there is one.
    fn attach(&mut self, key: &str, root: &gtk::Expander) -> usize {
        let index = *self.open_run.get_or_insert_with(|| {
            self.runs.push(Run::default());
            self.runs.len() - 1
        });
        let run = &mut self.runs[index];
        run.keys.push(key.to_owned());
        if run.keys.len() == 1 {
            self.entries.append(root);
            return index;
        }
        let group = run.group.get_or_insert_with(|| {
            let group = expander("");
            let contents = gtk::Box::new(gtk::Orientation::Vertical, 4);
            contents.set_margin_start(12);
            group.set_child(Some(&contents));
            group.set_visible(self.open.get());
            if let Some(Row::Work(first)) = self.rows.get(&run.keys[0]) {
                self.entries
                    .insert_child_after(&group, first.root.prev_sibling().as_ref());
                self.entries.remove(&first.root);
                contents.append(&first.root);
                if first.work.failed {
                    group.add_css_class("error");
                    group.set_expanded(true);
                    group.set_visible(true);
                }
            }
            group
        });
        if let Some(contents) = group.child().and_downcast::<gtk::Box>() {
            contents.append(root);
        }
        index
    }

    fn run_title(&self, index: usize) -> String {
        let works: Vec<_> = self.runs[index]
            .keys
            .iter()
            .filter_map(|key| match self.rows.get(key) {
                Some(Row::Work(row)) => Some(&row.work),
                _ => None,
            })
            .collect();
        run_title(&works)
    }

    fn show_process(&self) {
        for group in self.runs.iter().filter_map(|run| run.group.as_ref()) {
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

    fn flush(&mut self, finished: bool, base: &std::path::Path) {
        for row in self.rows.values_mut() {
            let Row::Text(text) = row else {
                continue;
            };
            if std::mem::take(&mut text.dirty) {
                text.blocks
                    .render(&text.contents, &text.text, text.live && !finished, base);
            } else if finished && text.live {
                text.blocks.settle();
            }
            if finished {
                text.live = false;
            }
        }
    }
}

impl WorkRow {
    fn update(&self, visible: bool, group: Option<&gtk::Expander>) {
        let title = if self.work.failed {
            format!("Failed · {}", self.work.title)
        } else if self.work.running {
            format!("Running · {}", self.work.title)
        } else {
            self.work.title.clone()
        };
        set_title(&self.root, &title);
        self.detail.set_label(&self.work.detail);
        self.root.set_visible(visible || self.work.failed);
        if self.work.failed {
            self.root.add_css_class("error");
            if let Some(group) = group {
                group.add_css_class("error");
                group.set_expanded(true);
            }
        }
        if let Some(group) = group {
            group.set_visible(visible || group.has_css_class("error"));
        }
    }
}

fn run_title(works: &[&Work]) -> String {
    const KINDS: [(&str, &str); 4] = [
        ("read", "reads"),
        ("command", "commands"),
        ("file change", "file changes"),
        ("tool call", "tool calls"),
    ];
    let mut counts = [0usize; KINDS.len()];
    for work in works {
        let kind = if work.read {
            0
        } else if work.title.starts_with("Command") {
            1
        } else if !work.changes.is_empty() || work.title == "File changes" {
            2
        } else {
            3
        };
        counts[kind] += 1;
    }
    let mut parts: Vec<_> = counts
        .iter()
        .zip(KINDS)
        .filter(|(count, _)| **count > 0)
        .map(|(count, (one, many))| format!("{count} {}", if *count == 1 { one } else { many }))
        .collect();
    let failed = works.iter().filter(|work| work.failed).count();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if let Some(work) = works.iter().rev().find(|work| work.running) {
        parts.push(format!("Running · {}", work.title));
    }
    parts.join(" · ")
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
        if self.replaying.get() || self.adjustment_scheduled.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(view) = weak.upgrade() else {
                return;
            };
            view.adjustment_scheduled.set(false);
            view.fit_rail();
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
        if self.replaying.get() {
            return;
        }
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
            } else if turn.failed() {
                button.add_css_class("failed");
            }
            let mark = gtk::Box::new(gtk::Orientation::Horizontal, 0);
            mark.set_halign(gtk::Align::Center);
            mark.set_valign(gtk::Align::Center);
            mark.add_css_class("flowmux-chat-rail-mark");
            button.set_child(Some(&mark));
            let motion = gtk::EventControllerMotion::new();
            let rail_preview = self.rail_preview.clone();
            let weak = Rc::downgrade(self);
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
                let Some(text) = weak.upgrade().and_then(|view| view.preview(index)) else {
                    return;
                };
                rail_preview.set_markup(&text);
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
        self.outline.set_visible(count > 0);
        *self.rail_buttons.borrow_mut() = buttons;
        drop(turns);
        self.fit_rail();
        if let Some(button) = restore_focus {
            button.grab_focus();
        }
        self.highlight_rail();
    }

    fn fit_rail(&self) {
        let show = !self.rail_buttons.borrow().is_empty() && self.root.width() >= RAIL_MIN_WIDTH;
        if self.track.is_visible() != show {
            self.track.set_visible(show);
        }
    }

    /// Shows the rail at full strength briefly after the reader scrolls.
    fn wake_rail(self: &Rc<Self>) {
        self.rail.add_css_class("active");
        if let Some(source) = self.rail_fade.take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(Duration::from_millis(1200), move || {
            if let Some(view) = weak.upgrade() {
                view.rail_fade.take();
                view.rail.remove_css_class("active");
            }
        });
        *self.rail_fade.borrow_mut() = Some(source);
    }

    fn preview(&self, index: usize) -> Option<String> {
        let turns = self.turns.try_borrow().ok()?;
        let turn = turns.get(index)?;
        let prompt = glib::markup_escape_text(&excerpt(&turn.prompt.label(), 200));
        let answer = turn.answer().map(|row| excerpt(&row.text, 240));
        Some(match answer.filter(|answer| !answer.is_empty()) {
            Some(answer) => format!("<b>{prompt}</b>\n{}", glib::markup_escape_text(&answer)),
            None => format!("<b>{prompt}</b>"),
        })
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

fn receipt(card: &gtk::Box, result: &str, answers: &[String]) {
    if let Some(actions) = card.last_child() {
        card.remove(&actions);
    }
    let mut answers = answers.iter();
    let mut child = card.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.is::<gtk::Entry>() {
            if let Some(answer) = answers.next() {
                card.insert_child_after(&text_label(answer), Some(&widget));
            }
            card.remove(&widget);
        } else if widget.is::<gtk::DropDown>() {
            card.remove(&widget);
        }
    }
    let result = text_label(result);
    result.add_css_class("dim-label");
    card.append(&result);
}

/// The text on one line, up to `limit` characters.
fn excerpt(text: &str, limit: usize) -> String {
    let mut words = String::new();
    for word in text.split_whitespace() {
        if !words.is_empty() {
            words.push(' ');
        }
        words.push_str(word);
        if words.len() > limit * 4 {
            break;
        }
    }
    match words.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &words[..end]),
        None => words,
    }
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

    fn work(id: &str, title: &str, read: bool) -> Work {
        Work {
            id: id.into(),
            title: title.into(),
            detail: String::new(),
            running: false,
            failed: false,
            read,
            changes: Vec::new(),
            check: None,
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn transcript_reflows_long_rows_instead_of_widening() {
        adw::init().unwrap();
        let transcript = Transcript::new(std::env::temp_dir());
        transcript.begin(&"word ".repeat(200));
        let long = format!("Command · {}", "x".repeat(150));
        transcript.work(work("a", &long, false));
        transcript.work(work("b", "Read · cat README.md", true));
        transcript.text(
            "answer".into(),
            TextKind::Answer,
            format!("Done.\n\n```rust\n{}\n```", "let value = 1; ".repeat(40)),
            false,
        );
        transcript.prompt(Prompt {
            id: "1".into(),
            title: "Allow the command?".into(),
            detail: long.clone(),
            choices: vec![
                "Allow once".into(),
                "Allow for this session".into(),
                "Decline".into(),
            ],
            questions: Vec::new(),
        });
        let mut changed = work("c", "File changes", false);
        changed.changes.push(Change {
            path: format!("{}/file.rs", "deeply/nested".repeat(12)),
            diff: "+added\n-removed".into(),
        });
        transcript.work(changed);
        transcript.finish("Completed", None, 3);
        let (minimum, ..) = transcript
            .view
            .history
            .measure(gtk::Orientation::Horizontal, -1);
        assert!(minimum < 240, "transcript minimum width {minimum}");

        // Both tool calls share one summary row; the edit started a new row after the prompt.
        let turns = transcript.view.turns.borrow();
        let groups = turns[0]
            .runs
            .iter()
            .filter(|run| run.group.is_some())
            .count();
        assert_eq!(groups, 1);
        assert_eq!(turns[0].run_title(0), "1 read · 1 command");
    }

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
