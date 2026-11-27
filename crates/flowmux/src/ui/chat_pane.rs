// SPDX-License-Identifier: GPL-3.0-or-later

mod commands;
mod transcript;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use flowmux_core::{PaneId, SurfaceId};
use flowmux_state::chat_history::{self, History};
use gtk::glib;

use super::chat_runtime::{self as runtime, Catalog, Event, Input, Menu, Provider, Settings};
use super::pane_terminal::PaneCallbacks;
use transcript::{label, Transcript};

const MAX_QUEUED: usize = 32;

struct QueuedPrompt {
    id: u64,
    text: String,
    settings: Settings,
}

struct QueueEdit {
    id: u64,
    draft: String,
}

#[derive(Clone)]
pub(crate) struct ChatPane {
    pub root: gtk::Box,
    ui: Rc<ChatUi>,
}

struct ChatUi {
    pane: Cell<PaneId>,
    history_id: SurfaceId,
    loading_history: Cell<bool>,
    resume_pending: Cell<bool>,
    startup_settings: RefCell<Option<Settings>>,
    native_id: RefCell<Option<String>>,
    cwd: PathBuf,
    provider: Option<Provider>,
    runtime: Option<tokio::runtime::Handle>,
    commands: RefCell<Option<async_channel::Sender<Input>>>,
    connecting: Cell<bool>,
    ready: Cell<bool>,
    busy: Cell<bool>,
    stopping: Cell<bool>,
    confirmed_access: RefCell<Option<String>>,
    last_text: RefCell<Option<String>>,
    queue: RefCell<VecDeque<QueuedPrompt>>,
    next_queue_id: Cell<u64>,
    queue_paused: Cell<bool>,
    queue_edit: RefCell<Option<QueueEdit>>,
    steering: Cell<Option<u64>>,
    /// The settings the running turn started with.
    turn_settings: RefCell<Option<Settings>>,
    queue_panel: gtk::Box,
    queue_title: gtk::Label,
    queue_rows: gtk::Box,
    catalog: RefCell<Catalog>,
    updating_options: Cell<bool>,
    focus_cb: Rc<RefCell<dyn FnMut(PaneId)>>,
    restart_cb: Rc<RefCell<dyn FnMut(super::pane_terminal::ChatRestart)>>,
    root: glib::WeakRef<gtk::Box>,
    status: gtk::Label,
    notice: gtk::Label,
    model: Choices,
    access: Choices,
    effort: Choices,
    plan: gtk::ToggleButton,
    details: gtk::MenuButton,
    settings_summary: gtk::Label,
    composer: gtk::TextView,
    send: gtk::Button,
    transcript: Transcript,
    menu: commands::Menu,
    /// The draft the command menu was closed for; typing reopens it.
    menu_dismissed: RefCell<Option<String>>,
}

impl ChatPane {
    pub fn new(
        pane: PaneId,
        history_id: SurfaceId,
        agent: &str,
        cwd: PathBuf,
        callbacks: &PaneCallbacks,
    ) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("flowmux-chat");
        root.add_css_class("view");
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // The composer and queue can overflow without raising the split's minimum width.
        root.append(
            &gtk::ScrolledWindow::builder()
                .child(&layout)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .vscrollbar_policy(gtk::PolicyType::Never)
                .overlay_scrolling(false)
                .build(),
        );
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        header.add_css_class("flowmux-chat-header");
        header.set_margin_start(8);
        header.set_margin_end(8);
        header.set_margin_top(6);
        header.set_margin_bottom(6);
        let path = label(&cwd.to_string_lossy());
        path.set_wrap(false);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        path.set_hexpand(true);
        path.set_tooltip_text(Some(&cwd.to_string_lossy()));
        header.append(&path);
        let status = label("");
        status.add_css_class("dim-label");
        status.set_wrap(false);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status.set_max_width_chars(22);
        header.append(&status);
        layout.append(&header);

        let queue_title = label("");
        queue_title.add_css_class("dim-label");
        queue_title.add_css_class("caption");
        let queue_rows = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let queue_panel = gtk::Box::new(gtk::Orientation::Vertical, 2);
        queue_panel.set_visible(false);
        queue_panel.set_margin_start(8);
        queue_panel.set_margin_end(8);
        queue_panel.set_margin_top(6);
        queue_panel.append(&queue_title);
        queue_panel.append(
            &gtk::ScrolledWindow::builder()
                .child(&queue_rows)
                .max_content_height(144)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .build(),
        );
        let queue_clamp = adw::Clamp::new();
        queue_clamp.set_maximum_size(840);
        queue_clamp.set_child(Some(&queue_panel));

        let transcript = Transcript::new();
        header.append(&transcript.outline);
        // The command menu floats over the conversation, just above the composer.
        let menu = commands::Menu::new(agent);
        let stage = gtk::Overlay::new();
        stage.set_vexpand(true);
        stage.set_child(Some(&transcript.root));
        stage.add_overlay(&menu.root);
        layout.append(&stage);
        // Queued messages sit with the composer that sends them.
        layout.append(&queue_clamp);
        let composer = gtk::TextView::builder()
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .accepts_tab(false)
            .build();
        composer.set_widget_name("flowmux-chat-composer");
        composer.update_property(&[gtk::accessible::Property::Label("Chat message")]);
        let editor = gtk::ScrolledWindow::builder()
            .child(&composer)
            .vscrollbar_policy(gtk::PolicyType::Automatic)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .min_content_height(48)
            .max_content_height(144)
            .build();
        let compose = gtk::Overlay::new();
        compose.set_child(Some(&editor));
        let placeholder = label("Message…");
        placeholder.add_css_class("dim-label");
        placeholder.set_halign(gtk::Align::Start);
        placeholder.set_valign(gtk::Align::Start);
        placeholder.set_margin_start(10);
        placeholder.set_margin_top(8);
        placeholder.set_can_target(false);
        compose.add_overlay(&placeholder);
        let notice = label("");
        notice.set_visible(false);
        notice.add_css_class("error");
        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        controls.add_css_class("flowmux-chat-controls");
        let model = Choices::new("Model");
        let access = Choices::new("Access");
        let effort = Choices::new("Reasoning intensity");
        let plan = gtk::ToggleButton::with_label("Plan");
        plan.set_tooltip_text(Some("Prepare a plan with read-only access for this turn"));
        let options = gtk::Box::new(gtk::Orientation::Vertical, 6);
        options.set_margin_start(6);
        options.set_margin_end(6);
        options.set_margin_top(6);
        options.set_margin_bottom(6);
        options.append(&model.section);
        options.append(&effort.section);
        options.append(&access.section);
        options.append(&plan);
        let end = gtk::Button::with_label("End session");
        end.add_css_class("destructive-action");
        options.append(&end);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&options));
        // The current settings are the button, so the summary also opens them.
        let settings_summary = label("");
        settings_summary.set_wrap(false);
        settings_summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        settings_summary.add_css_class("dim-label");
        let details_content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        details_content.append(&gtk::Image::from_icon_name("view-more-symbolic"));
        details_content.append(&settings_summary);
        let details = gtk::MenuButton::builder()
            .child(&details_content)
            .tooltip_text("Chat settings")
            .popover(&popover)
            .build();
        details.add_css_class("flat");
        details.update_property(&[gtk::accessible::Property::Label("Chat settings")]);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let send = gtk::Button::from_icon_name("go-up-symbolic");
        send.set_widget_name("flowmux-chat-send");
        send.add_css_class("suggested-action");
        send.add_css_class("pill");
        send.set_valign(gtk::Align::Center);
        controls.append(&details);
        controls.append(&spacer);
        controls.append(&send);

        let input = gtk::Box::new(gtk::Orientation::Vertical, 0);
        input.add_css_class("flowmux-chat-input");
        input.append(&compose);
        input.append(&controls);
        let footer = gtk::Box::new(gtk::Orientation::Vertical, 6);
        footer.add_css_class("flowmux-chat-footer");
        footer.set_margin_start(8);
        footer.set_margin_end(8);
        footer.set_margin_top(8);
        footer.set_margin_bottom(8);
        footer.append(&notice);
        footer.append(&input);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(840);
        clamp.set_child(Some(&footer));
        layout.append(&clamp);

        let ui = Rc::new(ChatUi {
            pane: Cell::new(pane),
            history_id,
            loading_history: Cell::new(true),
            resume_pending: Cell::new(false),
            startup_settings: RefCell::new(None),
            native_id: RefCell::new(None),
            cwd,
            provider: Provider::parse(agent),
            runtime: callbacks.chat_runtime.clone(),
            commands: RefCell::new(None),
            connecting: Cell::new(false),
            ready: Cell::new(false),
            busy: Cell::new(false),
            stopping: Cell::new(false),
            confirmed_access: RefCell::new(None),
            last_text: RefCell::new(None),
            queue: RefCell::new(VecDeque::new()),
            next_queue_id: Cell::new(0),
            queue_paused: Cell::new(false),
            queue_edit: RefCell::new(None),
            steering: Cell::new(None),
            turn_settings: RefCell::new(None),
            queue_panel,
            queue_title,
            queue_rows,
            catalog: RefCell::new(Catalog::default()),
            updating_options: Cell::new(false),
            focus_cb: callbacks.on_focus.clone(),
            restart_cb: callbacks.on_chat_restart.clone(),
            root: root.downgrade(),
            status,
            notice,
            model,
            access,
            effort,
            plan,
            details,
            settings_summary,
            composer,
            send,
            transcript,
            menu,
            menu_dismissed: RefCell::new(None),
        });
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(&ui);
        focus.connect_enter(move |_| {
            if let Some(ui) = weak.upgrade() {
                (ui.focus_cb.borrow_mut())(ui.pane.get());
            }
        });
        root.add_controller(focus);
        let file = callbacks.on_chat_open_file.clone();
        let weak = Rc::downgrade(&ui);
        ui.transcript.set_open_file(move |path| {
            if let Some(ui) = weak.upgrade() {
                if let Some(path) = chat_file(&ui.cwd, path) {
                    (file.borrow_mut())(ui.pane.get(), path);
                } else {
                    ui.show_notice("The file is unavailable in this chat's working directory.");
                }
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.transcript.set_respond(move |id, selected, answers| {
            let Some(ui) = weak.upgrade().filter(|ui| !ui.stopping.get()) else {
                return false;
            };
            ui.try_input(Input::Respond {
                id,
                selected,
                answers,
            })
        });
        let weak = Rc::downgrade(&ui);
        ui.send.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                if ui.stop_mode() {
                    if !ui.stopping.get() {
                        ui.stop(false);
                    }
                } else {
                    ui.activate_send();
                }
            }
        });
        let weak = Rc::downgrade(&ui);
        end.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.details.popdown();
                ui.queue_paused.set(true);
                ui.try_input(Input::End);
                ui.refresh_queue();
            }
        });
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let preedit = Rc::new(Cell::new(false));
        let ime_enter = Rc::new(RefCell::new(None::<String>));
        ui.composer.connect_preedit_changed({
            let preedit = preedit.clone();
            let ime_enter = ime_enter.clone();
            let weak = Rc::downgrade(&ui);
            move |_, text| {
                preedit.set(!text.is_empty());
                if text.is_empty() {
                    if let Some(before) = ime_enter.borrow_mut().take() {
                        let weak = weak.clone();
                        let preedit = preedit.clone();
                        glib::idle_add_local_once(move || {
                            if let Some(ui) = weak.upgrade().filter(|ui| {
                                ui.composer.has_focus() && !preedit.get() && ui.draft() != before
                            }) {
                                ui.activate_send();
                            }
                        });
                    }
                }
            }
        });
        let focus = gtk::EventControllerFocus::new();
        focus.connect_leave({
            let ime_enter = ime_enter.clone();
            move |_| {
                ime_enter.borrow_mut().take();
            }
        });
        ui.composer.add_controller(focus);
        let weak = Rc::downgrade(&ui);
        keys.connect_key_pressed(move |keys, key, _, modifiers| {
            use gtk::gdk::{Key, ModifierType as Mod};
            ime_enter.borrow_mut().take();
            if let Some(ui) = weak
                .upgrade()
                .filter(|ui| !preedit.get() && ui.menu.is_open())
            {
                let plain =
                    !modifiers.intersects(Mod::SHIFT_MASK | Mod::ALT_MASK | Mod::CONTROL_MASK);
                match key {
                    Key::Up | Key::KP_Up => ui.menu.step(-1),
                    Key::Down | Key::KP_Down => ui.menu.step(1),
                    Key::Tab | Key::ISO_Left_Tab => {
                        if let Some(entry) = ui.menu.selected() {
                            ui.complete_command(&entry);
                        }
                    }
                    Key::Return | Key::KP_Enter | Key::ISO_Enter if plain => {
                        if let Some(entry) = ui.menu.selected() {
                            ui.choose_command(entry);
                        }
                    }
                    Key::Escape => {
                        *ui.menu_dismissed.borrow_mut() = Some(ui.draft());
                        ui.menu.hide();
                    }
                    _ => return glib::Propagation::Proceed,
                }
                return glib::Propagation::Stop;
            }
            if key == Key::Escape && !preedit.get() {
                if let Some(ui) = weak.upgrade().filter(|ui| ui.queue_edit.borrow().is_some()) {
                    ui.finish_queue_edit(None);
                    return glib::Propagation::Stop;
                }
            }
            if !matches!(key, Key::Return | Key::KP_Enter | Key::ISO_Enter)
                || modifiers.intersects(Mod::ALT_MASK | Mod::SHIFT_MASK)
            {
                return glib::Propagation::Proceed;
            }
            if let Some(ui) = weak.upgrade() {
                if let Some(event) = keys.current_event().filter(|_| preedit.get()) {
                    *ime_enter.borrow_mut() = Some(ui.draft());
                    ui.composer.im_context_filter_keypress(event);
                } else {
                    ui.activate_send();
                }
            }
            glib::Propagation::Stop
        });
        ui.composer.add_controller(keys);
        for choices in [&ui.model, &ui.effort, &ui.access] {
            let weak = Rc::downgrade(&ui);
            choices.list.connect_row_activated(move |list, row| {
                list.select_row(Some(row));
                if let Some(ui) = weak.upgrade() {
                    ui.details.popdown();
                    ui.composer.grab_focus();
                }
            });
        }
        let weak = Rc::downgrade(&ui);
        ui.plan.connect_toggled(move |_| {
            if let Some(ui) = weak.upgrade() {
                ui.update_settings_summary();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.model.list.connect_row_selected(move |_, _| {
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.updating_options.get()) {
                let effort = ui.effort.selected();
                ui.update_effort(effort.as_deref());
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.effort.list.connect_row_selected(move |_, _| {
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.updating_options.get()) {
                ui.update_settings_summary();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.access.list.connect_row_selected(move |_, _| {
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.updating_options.get()) {
                ui.confirmed_access.borrow_mut().take();
                ui.update_settings_summary();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.composer.buffer().connect_changed(move |buffer| {
            placeholder.set_visible(buffer.char_count() == 0);
            if let Some(ui) = weak.upgrade() {
                ui.refresh_controls();
                ui.update_menu();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.menu.connect_activate(move |entry| {
            if let Some(ui) = weak.upgrade() {
                ui.choose_command(entry);
            }
        });
        ui.set_catalog(Catalog::default(), &Settings::default());
        ui.load_history();
        Self { root, ui }
    }

    pub(super) fn connect_started(&self, catalog: Catalog, settings: Settings) {
        self.ui.loading_history.set(false);
        self.ui.set_catalog(catalog, &settings);
        *self.ui.startup_settings.borrow_mut() = Some(settings);
        self.ui.connect(true);
    }

    pub(super) fn resume(&self) {
        if self.ui.ready.get() {
            return;
        }
        if self.ui.loading_history.get() || self.ui.connecting.get() {
            self.ui.resume_pending.set(true);
        } else {
            self.ui.connect(true);
        }
    }

    pub fn set_pane_id(&self, pane: PaneId) {
        self.ui.pane.set(pane);
    }
    pub fn cwd(&self) -> &Path {
        &self.ui.cwd
    }

    pub fn history_id(&self) -> SurfaceId {
        self.ui.history_id
    }

    pub fn agent_name(&self) -> Option<&'static str> {
        self.ui.provider.map(Provider::name)
    }
    pub fn grab_focus(&self) {
        self.ui.composer.grab_focus();
    }
}

fn chat_file(cwd: &Path, name: &str) -> Option<PathBuf> {
    let root = cwd.canonicalize().ok()?;
    let candidate = cwd.join(name).canonicalize().ok()?;
    (candidate.starts_with(root) && candidate.is_file()).then_some(candidate)
}

pub(super) struct Picker {
    pub widget: gtk::DropDown,
    ids: RefCell<Vec<String>>,
}

pub(super) fn picker(name: &str) -> Picker {
    let widget = gtk::DropDown::from_strings(&[]);
    widget.update_property(&[gtk::accessible::Property::Label(name)]);
    widget.set_tooltip_text(Some(name));
    // Keep the default full-width rows when the button uses a compact label.
    widget.set_list_factory(widget.factory().as_ref());
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let text = gtk::Label::new(None);
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.set_max_width_chars(18);
        text.set_xalign(0.0);
        item.downcast_ref::<gtk::ListItem>()
            .unwrap()
            .set_child(Some(&text));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let (Some(child), Some(text)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            child.set_label(&text.string());
            child.set_tooltip_text(Some(&text.string()));
        }
    });
    widget.set_factory(Some(&factory));
    Picker {
        widget,
        ids: RefCell::new(Vec::new()),
    }
}

pub(super) fn fill(picker: &Picker, menu: &Menu, empty: &str, retain: Option<&str>) {
    let labels: Vec<_> = if menu.choices.is_empty() {
        vec![empty]
    } else {
        menu.choices
            .iter()
            .map(|option| option.label.as_str())
            .collect()
    };
    *picker.ids.borrow_mut() = menu
        .choices
        .iter()
        .map(|choice| choice.id.clone())
        .collect();
    picker
        .widget
        .set_model(Some(&gtk::StringList::new(&labels)));
    let selected = retain
        .into_iter()
        .chain(menu.current.as_deref())
        .find_map(|id| menu.choices.iter().position(|choice| choice.id == id))
        .unwrap_or(0);
    picker.widget.set_selected(selected as u32);
}

pub(super) fn take_id(picker: &Picker) -> Option<String> {
    picker
        .ids
        .borrow()
        .get(picker.widget.selected() as usize)
        .cloned()
}

/// A choice list shown directly in the chat settings popover. A
/// `gtk::DropDown` there opens a nested popover; on X11, once that list
/// closes, clicking outside no longer dismisses the settings popover.
struct Choices {
    section: gtk::Box,
    list: gtk::ListBox,
    ids: RefCell<Vec<String>>,
}

impl Choices {
    fn new(title: &str) -> Self {
        let heading = label(title);
        heading.add_css_class("dim-label");
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.update_property(&[gtk::accessible::Property::Label(title)]);
        let section = gtk::Box::new(gtk::Orientation::Vertical, 3);
        section.append(&heading);
        section.append(
            &gtk::ScrolledWindow::builder()
                .child(&list)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_width(true)
                .propagate_natural_height(true)
                .max_content_height(240)
                .build(),
        );
        Self {
            section,
            list,
            ids: RefCell::new(Vec::new()),
        }
    }

    fn fill(&self, menu: &Menu, retain: Option<&str>) {
        *self.ids.borrow_mut() = menu
            .choices
            .iter()
            .map(|choice| choice.id.clone())
            .collect();
        self.list.remove_all();
        for choice in &menu.choices {
            let text = label(&choice.label);
            text.set_tooltip_text(Some(&choice.description));
            text.add_css_class("flowmux-chat-effort");
            self.list.append(&text);
        }
        let selected = retain
            .into_iter()
            .chain(menu.current.as_deref())
            .find_map(|id| menu.choices.iter().position(|choice| choice.id == id))
            .unwrap_or(0);
        self.list
            .select_row(self.list.row_at_index(selected as i32).as_ref());
        self.section.set_visible(!menu.choices.is_empty());
    }

    fn select(&self, id: &str) {
        let index = self.ids.borrow().iter().position(|choice| choice == id);
        if let Some(row) = index.and_then(|index| self.list.row_at_index(index as i32)) {
            self.list.select_row(Some(&row));
        }
    }

    fn selected(&self) -> Option<String> {
        let row = self.list.selected_row()?;
        self.ids.borrow().get(row.index() as usize).cloned()
    }

    fn selected_label(&self) -> Option<glib::GString> {
        let row = self.list.selected_row()?;
        Some(row.child().and_downcast::<gtk::Label>()?.label())
    }
}

impl ChatUi {
    fn load_history(self: &Rc<Self>) {
        self.status.set_label("Loading history…");
        let id = self.history_id;
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let history = gtk::gio::spawn_blocking(move || {
                chat_history::Store::existing()?
                    .map_or(Ok(History::default()), |store| store.load(id))
            })
            .await;
            let Some(ui) = weak
                .upgrade()
                .filter(|ui| ui.loading_history.replace(false))
            else {
                return;
            };
            ui.status.set_label("Ready to connect");
            match history {
                Ok(Ok(History { records, native_id })) => {
                    *ui.native_id.borrow_mut() = native_id;
                    ui.transcript.replay(records, false);
                }
                _ => ui.show_notice("Could not load saved chat history."),
            }
            ui.connect(ui.resume_pending.replace(false));
        });
    }

    fn finish_turn(&self, status: &str, detail: Option<&str>) {
        let elapsed = self.transcript.elapsed();
        self.transcript.finish(status, detail, elapsed);
    }

    fn connect(self: &Rc<Self>, start: bool) {
        if self.loading_history.get() {
            return;
        }
        let (Some(runtime), Some(provider)) = (&self.runtime, self.provider) else {
            self.show_notice("The chat runtime or agent is unavailable.");
            self.refresh_controls();
            return;
        };
        self.connecting.set(true);
        self.status.set_label("Connecting…");
        self.notice.set_visible(false);
        let (tx, events) = runtime::hosted(
            runtime,
            self.history_id,
            provider,
            self.cwd.clone(),
            self.native_id.borrow().clone(),
            start,
        );
        self.listen(tx, events);
    }

    fn listen(
        self: &Rc<Self>,
        tx: async_channel::Sender<Input>,
        events: async_channel::Receiver<Event>,
    ) {
        *self.commands.borrow_mut() = Some(tx);
        self.refresh_controls();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(first) = events.recv().await {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                ui.handle(first);
                for _ in 0..127 {
                    let Ok(event) = events.try_recv() else { break };
                    ui.handle(event);
                }
                ui.transcript.flush_text();
                drop(ui);
                if !events.is_empty() {
                    glib::timeout_future(std::time::Duration::from_millis(16)).await;
                }
            }
        });
    }

    fn ready(&self, catalog: Catalog, settings: &Settings) {
        self.connecting.set(false);
        self.ready.set(true);
        self.set_catalog(catalog, settings);
        if settings.confirmed_access && self.access.selected() == settings.access {
            *self.confirmed_access.borrow_mut() = settings.access.clone();
        }
    }

    fn set_catalog(&self, catalog: Catalog, settings: &Settings) {
        let old_model = self.model.selected();
        let old_access = self.access.selected();
        let old_effort = self.effort.selected();
        self.updating_options.set(true);
        self.model.fill(
            &catalog.models,
            settings.model.as_deref().or(old_model.as_deref()),
        );
        self.access.fill(
            &catalog.access,
            settings.access.as_deref().or(old_access.as_deref()),
        );
        *self.catalog.borrow_mut() = catalog;
        self.update_effort(settings.effort.as_deref().or(old_effort.as_deref()));
        self.updating_options.set(false);
        self.refresh_controls();
    }

    fn update_effort(&self, retain: Option<&str>) {
        let catalog = self.catalog.borrow();
        let model = self.model.selected();
        let menu = catalog.efforts_for(model.as_deref());
        self.effort.fill(menu, retain);
        self.plan.set_visible(catalog.plan);
        if !catalog.plan {
            self.plan.set_active(false);
        }
        self.details.set_visible(
            self.ready.get()
                || !catalog.models.choices.is_empty()
                || !catalog.access.choices.is_empty()
                || !menu.choices.is_empty()
                || catalog.plan,
        );
        self.update_settings_summary();
    }

    fn local_command(&self, name: &str) -> Option<commands::Local> {
        let catalog = self.catalog.borrow();
        commands::LOCAL
            .iter()
            .find(|(local, ..)| *local == name)
            .map(|(_, local, _)| *local)
            .filter(|local| self.local_available(*local, &catalog))
    }

    fn local_available(&self, local: commands::Local, catalog: &Catalog) -> bool {
        use commands::Local;
        match local {
            Local::New => self.provider.is_some() && self.runtime.is_some(),
            Local::Plan => {
                catalog.plan
                    || catalog
                        .access
                        .choices
                        .iter()
                        .any(|choice| choice.id == "plan")
            }
            Local::Model => {
                !catalog.models.choices.is_empty()
                    || !catalog.efforts.choices.is_empty()
                    || !catalog.access.choices.is_empty()
            }
            Local::Stop => self.busy.get(),
        }
    }

    fn update_menu(&self) {
        let draft = self.draft();
        if self
            .menu_dismissed
            .borrow()
            .as_ref()
            .is_some_and(|dismissed| *dismissed != draft)
        {
            self.menu_dismissed.borrow_mut().take();
        }
        let Some(query) =
            commands::typed(&draft).filter(|_| self.menu_dismissed.borrow().is_none())
        else {
            self.menu.hide();
            return;
        };
        let entries = {
            let catalog = self.catalog.borrow();
            let local: Vec<_> = commands::LOCAL
                .iter()
                .copied()
                .filter(|(_, local, _)| self.local_available(*local, &catalog))
                .collect();
            commands::entries(&local, &catalog.commands)
        };
        self.menu.show(commands::filter(&entries, query), query);
    }

    fn set_draft(&self, text: &str) {
        let buffer = self.composer.buffer();
        buffer.set_text(text);
        buffer.place_cursor(&buffer.end_iter());
    }

    fn complete_command(&self, entry: &commands::Entry) {
        self.set_draft(&format!("/{} ", entry.name));
        self.composer.grab_focus();
    }

    /// Flowmux commands run at once; agent commands with arguments wait for them.
    fn choose_command(self: &Rc<Self>, entry: commands::Entry) {
        match entry.owner {
            commands::Owner::Flowmux(local) => {
                self.set_draft("");
                self.run_local(local);
            }
            _ if entry.takes_input() => self.complete_command(&entry),
            _ => {
                self.set_draft(&format!("/{}", entry.name));
                self.activate_send();
            }
        }
        self.composer.grab_focus();
    }

    fn run_local(self: &Rc<Self>, local: commands::Local) {
        use commands::Local;
        self.menu.hide();
        match local {
            Local::New => {
                let Some(provider) = self.provider else {
                    return;
                };
                // End this conversation's agent; its history stays in Agent sessions.
                if self.commands.borrow().is_some() {
                    self.queue_paused.set(true);
                    let _ = self.try_input(Input::End);
                }
                let restart = super::pane_terminal::ChatRestart {
                    pane: self.pane.get(),
                    history: self.history_id,
                    agent: provider.name().into(),
                    cwd: self.cwd.clone(),
                    catalog: self.catalog.borrow().clone(),
                    // Elevated access is confirmed again for every new connection.
                    settings: Settings {
                        confirmed_access: false,
                        ..self.draft_settings()
                    },
                };
                (self.restart_cb.borrow_mut())(restart);
            }
            Local::Plan => {
                if self.catalog.borrow().plan {
                    self.plan.set_active(!self.plan.is_active());
                } else {
                    // Claude offers Plan as an access level.
                    let planning = self.access.selected().as_deref() == Some("plan");
                    self.access
                        .select(if planning { "default" } else { "plan" });
                }
            }
            Local::Model => self.details.popup(),
            Local::Stop => {
                if self.busy.get() && !self.stopping.get() {
                    self.stop(false);
                }
            }
        }
    }

    fn update_settings_summary(&self) {
        // Plan replaces the access level with read-only for the turn.
        let access = if self.plan.is_active() {
            Some("Plan".into())
        } else {
            self.access.selected_label()
        };
        let text = [
            self.model.selected_label(),
            self.effort.selected_label(),
            access,
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        self.settings_summary.set_label(&text);
        self.settings_summary
            .set_tooltip_text((!text.is_empty()).then_some(text.as_str()));
    }

    fn draft(&self) -> String {
        let buffer = self.composer.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn refresh_controls(&self) {
        let ready = self.ready.get();
        let editing = self.queue_edit.borrow().is_some();
        self.details.set_sensitive(ready && !editing);
        self.status
            .set_visible(self.connecting.get() || self.busy.get());
        let stop = self.stop_mode();
        if stop {
            self.send.remove_css_class("suggested-action");
        } else {
            self.send.add_css_class("suggested-action");
        }
        let (action, icon, tooltip, enabled) = if stop {
            (
                "Stop",
                "media-playback-stop-symbolic",
                "Stop this turn · Type to queue another message",
                !self.stopping.get(),
            )
        } else if editing {
            (
                "Save queued message",
                "object-select-symbolic",
                "Enter to save changes · Esc to cancel · Shift+Enter for a new line",
                !self.draft().trim().is_empty(),
            )
        } else if ready {
            let enabled = !self.draft().trim().is_empty();
            if self.busy.get() {
                (
                    "Queue message",
                    "go-up-symbolic",
                    "Enter to queue · Shift+Enter for a new line",
                    enabled,
                )
            } else {
                (
                    "Send",
                    "go-up-symbolic",
                    "Enter to send · Shift+Enter for a new line",
                    enabled,
                )
            }
        } else {
            (
                "Connect",
                "network-transmit-receive-symbolic",
                "Connect to the agent",
                !self.loading_history.get()
                    && !self.connecting.get()
                    && self.provider.is_some()
                    && self.runtime.is_some(),
            )
        };
        self.send.set_icon_name(icon);
        self.send
            .update_property(&[gtk::accessible::Property::Label(action)]);
        self.send.set_tooltip_text(Some(tooltip));
        self.send.set_sensitive(enabled);
    }

    /// A running turn with an empty composer turns the send button into Stop.
    fn stop_mode(&self) -> bool {
        self.busy.get() && self.queue_edit.borrow().is_none() && self.draft().trim().is_empty()
    }

    fn show_notice(&self, message: &str) {
        self.notice.set_label(message);
        self.notice.set_visible(!message.is_empty());
    }

    fn try_input(&self, command: Input) -> bool {
        if self
            .commands
            .borrow()
            .as_ref()
            .is_some_and(|channel| channel.try_send(command).is_ok())
        {
            return true;
        }
        self.show_notice("The agent is not accepting requests. Try again.");
        false
    }

    fn activate_send(self: &Rc<Self>) {
        let editing = self.queue_edit.borrow().is_some();
        // The single routing point: Flowmux commands never reach the agent as text.
        if let Some(local) = commands::parse(&self.draft())
            .filter(|_| !editing)
            .and_then(|(name, _)| self.local_command(name))
        {
            self.set_draft("");
            self.run_local(local);
            return;
        }
        if !editing && !self.ready.get() {
            if !self.connecting.get() {
                self.connect(true);
            }
            return;
        }
        let text = self.draft();
        if text.trim().is_empty() {
            return;
        }
        if text.len() > 128 * 1024 {
            self.show_notice("The message is too long. Limit messages to 128 KiB.");
            return;
        }
        if editing {
            self.finish_queue_edit(Some(text));
            return;
        }
        self.authorize(self.draft_settings(), move |ui, settings| {
            if ui.queue_edit.borrow().is_none() && ui.draft() == text {
                ui.submit_draft(text.clone(), settings);
            }
        });
    }

    fn needs_confirmation(&self, settings: &Settings) -> bool {
        !settings.confirmed_access
            && !settings.plan
            && self.catalog.borrow().access.choices.iter().any(|choice| {
                settings.access.as_ref() == Some(&choice.id)
                    && runtime::starts_with_elevated_access(choice)
            })
    }

    fn authorize(
        self: &Rc<Self>,
        mut settings: Settings,
        run: impl Fn(&Rc<Self>, Settings) + 'static,
    ) {
        settings.confirmed_access |= settings.access.is_some()
            && self.confirmed_access.borrow().as_ref() == settings.access.as_ref();
        if self.needs_confirmation(&settings) {
            let Some(root) = self.root.upgrade() else {
                return;
            };
            let Some(connection) = self
                .commands
                .borrow()
                .as_ref()
                .map(|sender| sender.downgrade())
            else {
                return;
            };
            let dialog = adw::AlertDialog::new(Some("Allow full access?"), Some("This agent can run with broader permissions in this chat. The choice resets when the connection ends."));
            dialog.add_response("cancel", "Cancel");
            dialog.add_response("allow", "Allow for this chat");
            dialog.set_response_appearance("allow", adw::ResponseAppearance::Destructive);
            dialog.set_close_response("cancel");
            let weak = Rc::downgrade(self);
            dialog.connect_response(None, move |_, response| {
                if response == "allow" {
                    if let (Some(ui), Some(connection)) = (weak.upgrade(), connection.upgrade()) {
                        if ui
                            .commands
                            .borrow()
                            .as_ref()
                            .is_some_and(|sender| sender.same_channel(&connection))
                        {
                            let mut settings = settings.clone();
                            settings.confirmed_access = true;
                            *ui.confirmed_access.borrow_mut() = settings.access.clone();
                            run(&ui, settings);
                        }
                    }
                }
            });
            dialog.present(Some(&root));
        } else {
            run(self, settings);
        }
    }

    fn submit_draft(self: &Rc<Self>, text: String, settings: Settings) {
        if self.busy.get() {
            let mut queue = self.queue.borrow_mut();
            if queue.len() >= MAX_QUEUED {
                self.show_notice("The message queue is full.");
                return;
            }
            let id = self.next_queue_id.get();
            self.next_queue_id.set(id + 1);
            queue.push_back(QueuedPrompt { id, text, settings });
            if self.stopping.get() {
                self.queue_paused.set(false);
            }
        } else {
            if !self.send_text(&text, settings) {
                return;
            }
            if self.queue.borrow().is_empty() {
                self.queue_paused.set(false);
            }
        }
        self.composer.buffer().set_text("");
        self.refresh_queue();
        self.refresh_controls();
    }

    fn draft_settings(&self) -> Settings {
        let access = if self.plan.is_active() {
            Some("read-only".into())
        } else {
            self.access.selected()
        };
        Settings {
            model: self.model.selected(),
            effort: self.effort.selected(),
            confirmed_access: access.is_some()
                && self.confirmed_access.borrow().as_ref() == access.as_ref(),
            access,
            plan: self.plan.is_active(),
        }
    }

    fn send_text(&self, text: &str, settings: Settings) -> bool {
        if !self.ready.get() || self.busy.get() {
            return false;
        }
        if !self.try_input(Input::Send {
            text: text.to_owned(),
            settings: settings.clone(),
        }) {
            return false;
        }
        *self.turn_settings.borrow_mut() = Some(settings);
        self.show_notice("");
        self.busy.set(true);
        *self.last_text.borrow_mut() = Some(text.to_owned());
        self.transcript.begin(text);
        self.status.set_label("Sending…");
        self.refresh_controls();
        true
    }

    fn stop(self: &Rc<Self>, resume_queue: bool) -> bool {
        if !self.stopping.get() {
            if !self.try_input(Input::Stop) {
                return false;
            }
            self.stopping.set(true);
            self.status.set_label("Stopping…");
        }
        self.queue_paused.set(!resume_queue);
        self.refresh_controls();
        if !resume_queue {
            self.refresh_queue();
        }
        true
    }

    fn editing_id(&self) -> Option<u64> {
        self.queue_edit.borrow().as_ref().map(|edit| edit.id)
    }

    fn edit_queued(self: &Rc<Self>, id: u64) {
        if self.editing_id().is_some() {
            return;
        }
        let text = self
            .queue
            .borrow()
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.text.clone());
        let Some(text) = text else { return };
        *self.queue_edit.borrow_mut() = Some(QueueEdit {
            id,
            draft: self.draft(),
        });
        let buffer = self.composer.buffer();
        buffer.set_text(&text);
        buffer.place_cursor(&buffer.end_iter());
        self.refresh_controls();
        self.refresh_queue();
        self.composer.grab_focus();
    }

    fn finish_queue_edit(self: &Rc<Self>, text: Option<String>) {
        let Some(edit) = self.queue_edit.borrow_mut().take() else {
            return;
        };
        if let Some(text) = text {
            if let Some(entry) = self
                .queue
                .borrow_mut()
                .iter_mut()
                .find(|entry| entry.id == edit.id)
            {
                entry.text = text;
            }
            self.show_notice("");
        }
        let buffer = self.composer.buffer();
        buffer.set_text(&edit.draft);
        buffer.place_cursor(&buffer.end_iter());
        self.refresh_controls();
        self.refresh_queue();
        self.composer.grab_focus();
        self.send_next_queued();
    }

    fn send_now(self: &Rc<Self>, id: u64) {
        let index = self.queue.borrow().iter().position(|entry| entry.id == id);
        let Some(index) = index.filter(|_| self.editing_id() != Some(id)) else {
            return;
        };
        let steer = {
            let queue = self.queue.borrow();
            // A queued command starts its own turn; it cannot join the running one.
            commands::parse(&queue[index].text).is_none() && self.can_steer(&queue[index].settings)
        };
        if !self.ready.get() {
            self.show_notice("Connect to the agent to send queued messages.");
        } else if steer {
            let text = self.queue.borrow()[index].text.clone();
            if self.try_input(Input::Steer { text }) {
                self.steering.set(Some(id));
                self.refresh_queue();
            }
        } else if self.busy.get() {
            if self.stop(true) {
                let mut queue = self.queue.borrow_mut();
                if let Some(prompt) = queue.remove(index) {
                    queue.push_front(prompt);
                }
                drop(queue);
                self.refresh_queue();
            }
        } else {
            self.queue_paused.set(false);
            self.send_queued(id);
        }
    }

    /// Codex accepts more input in its running turn; other agents stop and send. A message
    /// queued with other settings, such as Plan, must not run under the current turn's.
    fn can_steer(&self, settings: &Settings) -> bool {
        self.provider == Some(Provider::Codex)
            && self.busy.get()
            && !self.stopping.get()
            && self.steering.get().is_none()
            && self.turn_settings.borrow().as_ref().is_some_and(|turn| {
                (&turn.model, &turn.access, &turn.effort, turn.plan)
                    == (
                        &settings.model,
                        &settings.access,
                        &settings.effort,
                        settings.plan,
                    )
            })
    }

    fn send_next_queued(self: &Rc<Self>) {
        if self.queue_paused.get() || self.busy.get() || !self.ready.get() {
            return;
        }
        let next = self.queue.borrow().front().map(|entry| entry.id);
        if let Some(id) = next {
            self.send_queued(id);
        }
    }

    fn send_queued(self: &Rc<Self>, id: u64) {
        if self.editing_id() == Some(id) {
            return;
        }
        let Some(settings) = self
            .queue
            .borrow()
            .iter()
            .find(|entry| entry.id == id)
            .map(|entry| entry.settings.clone())
        else {
            return;
        };
        self.authorize(settings, move |ui, settings| {
            if ui.editing_id() == Some(id) {
                return;
            }
            let text = ui
                .queue
                .borrow()
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.text.clone());
            if let Some(text) = text {
                if ui.send_text(&text, settings) {
                    ui.queue.borrow_mut().retain(|entry| entry.id != id);
                    ui.refresh_queue();
                } else {
                    ui.queue_paused.set(true);
                    ui.refresh_queue();
                }
            }
        });
    }

    fn refresh_queue(self: &Rc<Self>) {
        while let Some(row) = self.queue_rows.first_child() {
            self.queue_rows.remove(&row);
        }
        let queue = self.queue.borrow();
        let count = queue.len();
        let editing = self.editing_id();
        self.queue_panel.set_visible(count > 0);
        let mut title = format!("{count} queued");
        if self.queue_paused.get() {
            title.push_str(" · Paused");
        }
        if editing.is_some() {
            title.push_str(" · Editing queued message");
        }
        self.queue_title.set_label(&title);
        for entry in queue.iter() {
            let id = entry.id;
            let is_editing = editing == Some(id);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            let preview: String = entry
                .text
                .split_whitespace()
                .flat_map(|word| word.chars().chain(std::iter::once(' ')))
                .take(160)
                .collect();
            let text = label(preview.trim_end());
            text.set_wrap(false);
            text.set_ellipsize(gtk::pango::EllipsizeMode::End);
            text.set_hexpand(true);
            text.set_max_width_chars(30);
            text.set_tooltip_text(Some(preview.trim_end()));
            if is_editing {
                text.add_css_class("accent");
            }
            row.append(&text);
            let steering = self.steering.get() == Some(id);
            let (icon, action, tooltip) = if steering {
                (
                    "content-loading-symbolic",
                    "Steering",
                    "Adding this message to the running turn…",
                )
            } else if commands::parse(&entry.text).is_none() && self.can_steer(&entry.settings) {
                (
                    "mail-send-symbolic",
                    "Steer",
                    "Steer · Add this message to the running turn",
                )
            } else {
                (
                    "go-up-symbolic",
                    "Send now",
                    "Send now · Stops the current turn if running",
                )
            };
            let now = gtk::Button::from_icon_name(icon);
            now.add_css_class("flat");
            now.set_tooltip_text(Some(tooltip));
            now.update_property(&[gtk::accessible::Property::Label(action)]);
            now.set_sensitive(!is_editing && !steering);
            let weak = Rc::downgrade(self);
            now.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.send_now(id);
                }
            });
            row.append(&now);
            let (edit_icon, edit_label) = if is_editing {
                ("window-close-symbolic", "Cancel queued edit")
            } else {
                ("document-edit-symbolic", "Edit queued message")
            };
            let edit = gtk::Button::from_icon_name(edit_icon);
            edit.add_css_class("flat");
            edit.set_tooltip_text(Some(edit_label));
            edit.update_property(&[gtk::accessible::Property::Label(edit_label)]);
            edit.set_sensitive(is_editing || editing.is_none());
            let weak = Rc::downgrade(self);
            edit.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    if ui.editing_id() == Some(id) {
                        ui.finish_queue_edit(None);
                    } else {
                        ui.edit_queued(id);
                    }
                }
            });
            row.append(&edit);
            let remove = gtk::Button::from_icon_name("user-trash-symbolic");
            remove.add_css_class("flat");
            remove.set_tooltip_text(Some("Remove queued message"));
            remove.update_property(&[gtk::accessible::Property::Label("Remove queued message")]);
            remove.set_sensitive(!is_editing && !steering);
            let weak = Rc::downgrade(self);
            remove.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade().filter(|ui| ui.editing_id() != Some(id)) {
                    ui.queue.borrow_mut().retain(|entry| entry.id != id);
                    ui.refresh_queue();
                }
            });
            row.append(&remove);
            self.queue_rows.append(&row);
        }
    }

    fn handle(self: &Rc<Self>, event: Event) {
        match event {
            Event::Attached {
                records,
                busy,
                native_id,
                settings,
            } => {
                *self.native_id.borrow_mut() = native_id;
                *self.turn_settings.borrow_mut() = busy.then(|| settings.clone());
                self.startup_settings.borrow_mut().get_or_insert(settings);
                self.resume_pending.set(false);
                self.busy.set(busy);
                self.transcript.replay(records, busy);
                self.status
                    .set_label(if self.transcript.has_pending_input() {
                        "Waiting for input"
                    } else {
                        "Working…"
                    });
            }
            Event::Detached => {
                self.ready.set(false);
                self.connecting.set(false);
                self.commands.borrow_mut().take();
                if self.resume_pending.replace(false) {
                    self.connect(true);
                } else {
                    self.refresh_controls();
                }
            }
            Event::Ready(catalog) => {
                let settings = self
                    .startup_settings
                    .borrow_mut()
                    .take()
                    .unwrap_or_default();
                self.ready(catalog, &settings);
            }
            Event::Session(native_id) => *self.native_id.borrow_mut() = Some(native_id),
            Event::SettingsApplied(settings) => {
                self.last_text.borrow_mut().take();
                if !self.stopping.get() && !self.transcript.has_pending_input() {
                    self.status.set_label("Working…");
                }
                let mut applied = Vec::new();
                for value in [&settings.model, &settings.access, &settings.effort]
                    .into_iter()
                    .flatten()
                {
                    applied.push(value.as_str());
                }
                if !applied.is_empty() {
                    self.status.set_tooltip_text(Some(&applied.join(" · ")));
                }
            }
            Event::Text {
                id,
                kind,
                text,
                append,
            } => self.transcript.text(id, kind, text, append),
            Event::Work(work) => self.transcript.work(work),
            Event::Prompt(prompt) => {
                self.status.set_label("Waiting for input");
                self.transcript.prompt(prompt);
            }
            Event::PromptResolved { id, label, answers } => {
                if self.transcript.resolve(&id, &label, &answers) {
                    self.status.set_label("Working…");
                }
            }
            Event::Steered { text } => {
                if let Some(id) = self.steering.take() {
                    self.queue.borrow_mut().retain(|entry| entry.id != id);
                }
                self.transcript.steer(&text);
                self.refresh_queue();
            }
            Event::SteerRejected(error) => {
                self.steering.set(None);
                self.show_notice(&error);
                self.refresh_queue();
            }
            Event::Finished { status, detail } => {
                self.finish_turn(&status, detail.as_deref());
                self.clear_turn();
                self.send_next_queued();
                self.refresh_queue();
            }
            Event::Rejected(error) => {
                self.queue_paused.set(true);
                self.restore_unconfirmed_draft();
                self.finish_turn("Not sent", Some(&error));
                self.show_notice(&error);
                self.clear_turn();
                self.refresh_queue();
            }
            Event::Notice(notice) => self.show_notice(&notice),
            Event::StopFailed(notice) => {
                self.stopping.set(false);
                self.queue_paused.set(true);
                self.status
                    .set_label(if self.transcript.has_pending_input() {
                        "Waiting for input"
                    } else {
                        "Working…"
                    });
                self.show_notice(&notice);
                self.refresh_controls();
                self.refresh_queue();
            }
            Event::Offline(error) => {
                self.queue_paused.set(true);
                for prompt in self.queue.borrow_mut().iter_mut() {
                    prompt.settings.confirmed_access = false;
                }
                self.restore_unconfirmed_draft();
                if self.busy.get() {
                    self.finish_turn("Disconnected", Some(&error));
                }
                self.ready.set(false);
                self.connecting.set(false);
                self.commands.borrow_mut().take();
                self.confirmed_access.borrow_mut().take();
                self.set_catalog(Catalog::default(), &Settings::default());
                self.show_notice(&error);
                self.clear_turn();
                self.refresh_queue();
            }
        }
    }

    fn restore_unconfirmed_draft(&self) {
        if let Some(text) = self.last_text.borrow_mut().take() {
            self.restore_draft(&text);
        }
    }

    fn restore_draft(&self, text: &str) {
        let mut edit = self.queue_edit.borrow_mut();
        let current = edit
            .as_ref()
            .map_or_else(|| self.draft(), |edit| edit.draft.clone());
        let restored = if current.is_empty() {
            text.to_owned()
        } else {
            format!("{text}\n\n{current}")
        };
        if let Some(edit) = edit.as_mut() {
            edit.draft = restored;
        } else {
            drop(edit);
            self.composer.buffer().set_text(&restored);
        }
    }

    fn clear_turn(&self) {
        // An unconfirmed steer stays queued and can start the next turn.
        self.steering.set(None);
        self.turn_settings.borrow_mut().take();
        self.busy.set(false);
        self.stopping.set(false);
        self.last_text.borrow_mut().take();
        self.refresh_controls();
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    fn chat_minimum_width_stays_stable_with_a_running_turn_and_queue() {
        adw::init().unwrap();
        let chat = ChatPane::new(
            PaneId::new(),
            SurfaceId::new(),
            "Codex",
            std::env::temp_dir(),
            &PaneCallbacks::noop_for_test(),
        );
        let minimum = || chat.root.measure(gtk::Orientation::Horizontal, -1).0;
        let idle_width = minimum();
        chat.ui.ready.set(true);
        chat.ui.busy.set(true);
        chat.ui.set_catalog(
            Catalog {
                plan: true,
                ..Catalog::default()
            },
            &Settings::default(),
        );
        chat.ui.transcript.begin("Running prompt");
        assert_eq!(minimum(), idle_width);

        chat.ui
            .submit_draft("Queued prompt".repeat(100), Settings::default());
        assert!(chat.ui.queue_panel.is_visible());
        assert_eq!(minimum(), idle_width);
    }
}
