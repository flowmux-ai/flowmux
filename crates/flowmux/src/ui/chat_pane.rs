// SPDX-License-Identifier: GPL-3.0-or-later

mod transcript;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use adw::prelude::*;
use flowmux_core::PaneId;
use gtk::glib;

use super::chat_runtime::{self as runtime, Catalog, Event, Input, Menu, Provider, Settings};
use super::pane_terminal::PaneCallbacks;
use transcript::{label, Transcript};

const MAX_QUEUED: usize = 32;

struct QueuedPrompt {
    text: String,
    settings: Settings,
}

#[derive(Clone)]
pub(crate) struct ChatPane {
    pub root: gtk::Box,
    ui: Rc<ChatUi>,
}

struct ChatUi {
    pane: Cell<PaneId>,
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
    queue_paused: Cell<bool>,
    queue_button: gtk::MenuButton,
    queue_rows: gtk::Box,
    catalog: RefCell<Catalog>,
    updating_options: Cell<bool>,
    focus_cb: Rc<RefCell<dyn FnMut(PaneId)>>,
    root: glib::WeakRef<gtk::Box>,
    status: gtk::Label,
    notice: gtk::Label,
    model: Picker,
    access: Picker,
    effort: gtk::ListBox,
    effort_ids: RefCell<Vec<String>>,
    plan: gtk::ToggleButton,
    details: gtk::MenuButton,
    settings_summary: gtk::Label,
    composer: gtk::TextView,
    send: gtk::Button,
    stop: gtk::Button,
    transcript: Transcript,
}

impl ChatPane {
    pub fn new(pane: PaneId, agent: &str, cwd: PathBuf, callbacks: &PaneCallbacks) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.set_widget_name("flowmux-chat");
        root.add_css_class("view");
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // A narrow split scrolls the header and composer too, including the
        // additional controls that appear while a turn is running.
        root.append(
            &gtk::ScrolledWindow::builder()
                .child(&layout)
                .vexpand(true)
                .hscrollbar_policy(gtk::PolicyType::Automatic)
                .vscrollbar_policy(gtk::PolicyType::Never)
                .overlay_scrolling(false)
                .propagate_natural_width(true)
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

        let transcript = Transcript::new();
        layout.append(&transcript.root);
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
        let model = picker("Model");
        let access = picker("Access");
        let effort = gtk::ListBox::new();
        effort.add_css_class("boxed-list");
        effort.update_property(&[gtk::accessible::Property::Label("Reasoning intensity")]);
        let plan = gtk::ToggleButton::with_label("Plan");
        plan.set_tooltip_text(Some("Prepare a plan with read-only access for this turn"));
        let options = gtk::Box::new(gtk::Orientation::Vertical, 6);
        options.set_margin_start(6);
        options.set_margin_end(6);
        options.set_margin_top(6);
        options.set_margin_bottom(6);
        options.append(&model.widget);
        let reasoning = gtk::Box::new(gtk::Orientation::Vertical, 3);
        let reasoning_label = label("Reasoning intensity");
        reasoning_label.add_css_class("dim-label");
        reasoning.append(&reasoning_label);
        reasoning.append(&effort);
        options.append(&reasoning);
        options.append(&access.widget);
        options.append(&plan);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&options));
        let details = gtk::MenuButton::builder()
            .icon_name("view-more-symbolic")
            .tooltip_text("Chat settings")
            .popover(&popover)
            .build();
        details.add_css_class("flat");
        details.update_property(&[gtk::accessible::Property::Label("Chat settings")]);
        let settings_summary = label("");
        settings_summary.set_wrap(false);
        settings_summary.set_ellipsize(gtk::pango::EllipsizeMode::End);
        settings_summary.set_hexpand(true);
        settings_summary.add_css_class("dim-label");
        let send = gtk::Button::from_icon_name("go-up-symbolic");
        send.set_widget_name("flowmux-chat-send");
        send.add_css_class("suggested-action");
        send.add_css_class("pill");
        send.set_valign(gtk::Align::Center);
        let stop = gtk::Button::from_icon_name("media-playback-stop-symbolic");
        stop.add_css_class("flat");
        stop.set_tooltip_text(Some("Stop this turn"));
        stop.update_property(&[gtk::accessible::Property::Label("Stop")]);
        controls.append(&details);
        controls.append(&settings_summary);
        controls.append(&stop);
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
        let queue_rows = gtk::Box::new(gtk::Orientation::Vertical, 4);
        queue_rows.set_margin_start(8);
        queue_rows.set_margin_end(8);
        queue_rows.set_margin_top(8);
        queue_rows.set_margin_bottom(8);
        let queue_popover = gtk::Popover::new();
        queue_popover.set_child(Some(
            &gtk::ScrolledWindow::builder()
                .child(&queue_rows)
                .min_content_width(320)
                .max_content_height(260)
                .hscrollbar_policy(gtk::PolicyType::Never)
                .propagate_natural_height(true)
                .build(),
        ));
        let queue_button = gtk::MenuButton::builder()
            .halign(gtk::Align::Start)
            .popover(&queue_popover)
            .visible(false)
            .build();
        queue_button.add_css_class("flat");
        footer.append(&queue_button);
        footer.append(&input);
        let clamp = adw::Clamp::new();
        clamp.set_maximum_size(840);
        clamp.set_child(Some(&footer));
        layout.append(&clamp);

        let ui = Rc::new(ChatUi {
            pane: Cell::new(pane),
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
            queue_paused: Cell::new(false),
            queue_button,
            queue_rows,
            catalog: RefCell::new(Catalog::default()),
            updating_options: Cell::new(false),
            focus_cb: callbacks.on_focus.clone(),
            root: root.downgrade(),
            status,
            notice,
            model,
            access,
            effort,
            effort_ids: RefCell::new(Vec::new()),
            plan,
            details,
            settings_summary,
            composer,
            send,
            stop,
            transcript,
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
                ui.activate_send();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.stop.connect_clicked(move |_| {
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.stopping.get()) {
                ui.stop(false);
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
        let weak = Rc::downgrade(&ui);
        ui.model.widget.connect_selected_notify(move |_| {
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.updating_options.get()) {
                let effort = ui.selected_effort();
                ui.update_effort(effort.as_deref());
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.effort.connect_row_activated(move |list, row| {
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.updating_options.get()) {
                list.select_row(Some(row));
                ui.update_settings_summary();
                ui.details.popdown();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.access.widget.connect_selected_notify(move |_| {
            if let Some(ui) = weak.upgrade().filter(|ui| !ui.updating_options.get()) {
                ui.confirmed_access.borrow_mut().take();
                ui.update_access_tooltip();
            }
        });
        let weak = Rc::downgrade(&ui);
        ui.composer.buffer().connect_changed(move |buffer| {
            placeholder.set_visible(buffer.char_count() == 0);
            if let Some(ui) = weak.upgrade() {
                ui.refresh_controls();
            }
        });
        ui.set_catalog(Catalog::default(), &Settings::default());
        Self { root, ui }
    }

    pub(super) fn connect_started(
        &self,
        commands: async_channel::Sender<Input>,
        events: async_channel::Receiver<Event>,
        catalog: Catalog,
        settings: Settings,
    ) {
        self.ui.listen(commands, events);
        self.ui.ready(catalog, &settings);
    }

    pub fn set_pane_id(&self, pane: PaneId) {
        self.ui.pane.set(pane);
    }
    pub fn cwd(&self) -> &Path {
        &self.ui.cwd
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

impl ChatUi {
    fn connect(self: &Rc<Self>) {
        let (Some(runtime), Some(provider)) = (&self.runtime, self.provider) else {
            self.show_notice("The chat runtime or agent is unavailable.");
            self.refresh_controls();
            return;
        };
        self.connecting.set(true);
        self.status.set_label("Connecting…");
        self.notice.set_visible(false);
        let (tx, events) = runtime::start(runtime, provider, self.cwd.clone());
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
        if settings.confirmed_access && take_id(&self.access) == settings.access {
            *self.confirmed_access.borrow_mut() = settings.access.clone();
        }
    }

    fn set_catalog(&self, catalog: Catalog, settings: &Settings) {
        let old_model = take_id(&self.model);
        let old_access = take_id(&self.access);
        let old_effort = self.selected_effort();
        self.updating_options.set(true);
        fill(
            &self.model,
            &catalog.models,
            "Model · Default",
            settings.model.as_deref().or(old_model.as_deref()),
        );
        fill(
            &self.access,
            &catalog.access,
            "Access · Default",
            settings.access.as_deref().or(old_access.as_deref()),
        );
        *self.catalog.borrow_mut() = catalog;
        self.update_effort(settings.effort.as_deref().or(old_effort.as_deref()));
        self.update_access_tooltip();
        self.updating_options.set(false);
        self.refresh_controls();
    }

    fn update_effort(&self, retain: Option<&str>) {
        let catalog = self.catalog.borrow();
        let model = take_id(&self.model);
        let menu = catalog.efforts_for(model.as_deref());
        *self.effort_ids.borrow_mut() = menu
            .choices
            .iter()
            .map(|choice| choice.id.clone())
            .collect();
        while let Some(row) = self.effort.first_child() {
            self.effort.remove(&row);
        }
        let current = retain.or(menu.current.as_deref());
        let mut selected = 0;
        for (index, choice) in menu.choices.iter().enumerate() {
            let text = label(&choice.label);
            text.set_tooltip_text(Some(&choice.description));
            text.add_css_class("flowmux-chat-effort");
            self.effort.append(&text);
            if current == Some(choice.id.as_str()) {
                selected = index as i32;
            }
        }
        self.effort
            .select_row(self.effort.row_at_index(selected).as_ref());
        self.model
            .widget
            .set_visible(!catalog.models.choices.is_empty());
        self.access
            .widget
            .set_visible(!catalog.access.choices.is_empty());
        self.effort
            .parent()
            .unwrap()
            .set_visible(!menu.choices.is_empty());
        self.plan.set_visible(catalog.plan);
        if !catalog.plan {
            self.plan.set_active(false);
        }
        self.details.set_visible(
            !catalog.models.choices.is_empty()
                || !catalog.access.choices.is_empty()
                || !menu.choices.is_empty()
                || catalog.plan,
        );
        self.update_settings_summary();
    }

    fn update_settings_summary(&self) {
        let model = self
            .model
            .widget
            .selected_item()
            .and_downcast::<gtk::StringObject>();
        let effort = self
            .effort
            .selected_row()
            .and_then(|row| row.child().and_downcast::<gtk::Label>());
        let text = [
            model
                .filter(|_| take_id(&self.model).is_some())
                .map(|item| item.string()),
            effort.map(|item| item.label()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        self.settings_summary.set_label(&text);
        self.settings_summary
            .set_tooltip_text((!text.is_empty()).then_some(text.as_str()));
    }

    fn selected_effort(&self) -> Option<String> {
        let row = self.effort.selected_row()?;
        self.effort_ids.borrow().get(row.index() as usize).cloned()
    }

    fn update_access_tooltip(&self) {
        let active = take_id(&self.access);
        let catalog = self.catalog.borrow();
        if let Some(choice) = catalog
            .access
            .choices
            .iter()
            .find(|choice| Some(choice.id.as_str()) == active.as_deref())
        {
            self.access
                .widget
                .set_tooltip_text(Some(&format!("{} · {}", choice.label, choice.description)));
        } else {
            self.access
                .widget
                .set_tooltip_text(Some("Access · Agent default"));
        }
    }

    fn draft(&self) -> String {
        let buffer = self.composer.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn refresh_controls(&self) {
        let catalog = self.catalog.borrow();
        let ready = self.ready.get();
        self.model
            .widget
            .set_sensitive(ready && !catalog.models.choices.is_empty());
        self.access
            .widget
            .set_sensitive(ready && !catalog.access.choices.is_empty());
        self.details.set_sensitive(ready);
        self.status
            .set_visible(self.connecting.get() || self.busy.get());
        self.stop.set_visible(self.busy.get());
        self.stop.set_sensitive(!self.stopping.get());
        let (action, icon, tooltip, enabled) = if ready {
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
                !self.connecting.get() && self.provider.is_some() && self.runtime.is_some(),
            )
        };
        self.send.set_icon_name(icon);
        self.send
            .update_property(&[gtk::accessible::Property::Label(action)]);
        self.send.set_tooltip_text(Some(tooltip));
        self.send.set_sensitive(enabled);
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
        if !self.ready.get() {
            if !self.connecting.get() {
                self.connect();
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
        self.authorize(self.draft_settings(), move |ui, settings| {
            if ui.draft() == text {
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
            queue.push_back(QueuedPrompt { text, settings });
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
            take_id(&self.access)
        };
        Settings {
            model: take_id(&self.model),
            effort: self.selected_effort(),
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
            settings,
        }) {
            return false;
        }
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

    fn send_now(self: &Rc<Self>, index: usize) {
        self.queue_button.popdown();
        if !self.ready.get() {
            self.show_notice("Connect to the agent to send queued messages.");
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
            self.send_queued(index);
        }
    }

    fn send_queued(self: &Rc<Self>, index: usize) {
        let Some(settings) = self
            .queue
            .borrow()
            .get(index)
            .map(|entry| entry.settings.clone())
        else {
            return;
        };
        self.authorize(settings, move |ui, settings| {
            let text = ui.queue.borrow().get(index).map(|entry| entry.text.clone());
            if let Some(text) = text {
                if ui.send_text(&text, settings) {
                    ui.queue.borrow_mut().remove(index);
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
        self.queue_button.set_visible(count > 0);
        let title = format!(
            "{count} {}",
            if self.queue_paused.get() {
                "paused"
            } else {
                "queued"
            }
        );
        self.queue_button.set_label(&title);
        self.queue_button
            .update_property(&[gtk::accessible::Property::Label(&title)]);
        for (index, entry) in queue.iter().enumerate() {
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
            row.append(&text);
            let now = gtk::Button::with_label("Send now");
            now.add_css_class("flat");
            now.set_tooltip_text(Some("Stop the current turn and send this message next"));
            let weak = Rc::downgrade(self);
            now.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    ui.send_now(index);
                }
            });
            row.append(&now);
            let edit = gtk::Button::from_icon_name("document-edit-symbolic");
            edit.add_css_class("flat");
            edit.set_tooltip_text(Some("Move message back to the draft"));
            edit.update_property(&[gtk::accessible::Property::Label("Edit queued message")]);
            let weak = Rc::downgrade(self);
            edit.connect_clicked(move |_| {
                if let Some(ui) = weak.upgrade() {
                    let prompt = ui.queue.borrow_mut().remove(index);
                    if let Some(prompt) = prompt {
                        ui.queue_button.popdown();
                        ui.restore_draft(&prompt.text);
                        ui.refresh_queue();
                        ui.composer.grab_focus();
                    }
                }
            });
            row.append(&edit);
            self.queue_rows.append(&row);
        }
    }

    fn handle(self: &Rc<Self>, event: Event) {
        match event {
            Event::Ready(catalog) => self.ready(catalog, &Settings::default()),
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
            Event::PromptResolved { id, label } => {
                if self.transcript.resolve(&id, &label) {
                    self.status.set_label("Working…");
                }
            }
            Event::Finished { status, detail } => {
                self.transcript.finish(&status, detail.as_deref());
                self.clear_turn();
                if !self.queue_paused.get() {
                    self.send_queued(0);
                }
            }
            Event::Rejected(error) => {
                self.queue_paused.set(true);
                self.restore_unconfirmed_draft();
                self.transcript.finish("Not sent", Some(&error));
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
                    self.transcript.finish("Disconnected", Some(&error));
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
        let current = self.draft();
        let restored = if current.is_empty() {
            text.to_owned()
        } else {
            format!("{text}\n\n{current}")
        };
        self.composer.buffer().set_text(&restored);
    }

    fn clear_turn(&self) {
        self.busy.set(false);
        self.stopping.set(false);
        self.last_text.borrow_mut().take();
        self.refresh_controls();
    }
}
