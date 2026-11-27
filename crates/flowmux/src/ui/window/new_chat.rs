// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::ui::chat_pane::{fill, picker, take_id, Picker};
use crate::ui::chat_runtime::{self, Catalog, Event, Settings};

impl WindowController {
    pub(super) async fn show_new_chat_dialog(&self, pane: PaneId) {
        let Some(workspace) = self.pane_registry.borrow().workspace_of_pane(pane) else {
            return;
        };
        if self.pane_registry.borrow().is_ssh_pane(pane) || !self.pane_in_visible_workspace(pane) {
            return;
        }
        let window = self.workspace_window(workspace);
        if window.visible_dialog().is_some() {
            return;
        }
        let is_browser = {
            let registry = self.pane_registry.borrow();
            registry.active_browser(pane).is_some()
                || registry
                    .active_deferred_surface(pane)
                    .is_some_and(|surface| matches!(surface.kind, SurfaceKind::Browser { .. }))
        };
        let cwd = if is_browser {
            self.store
                .get_workspace(workspace)
                .await
                .and_then(|workspace| workspace.local_root().map(PathBuf::from))
        } else {
            self.file_browser_root_for_pane(pane).await
        };
        let Some(runtime) = &self.tokio_handle else {
            self.clipboard_toast
                .show_with_message("Chat runtime unavailable.");
            return;
        };
        let providers = runtime
            .spawn(chat_runtime::available())
            .await
            .unwrap_or_default();
        if window.visible_dialog().is_some()
            || !self.pane_in_visible_workspace(pane)
            || self.pane_registry.borrow().workspace_of_pane(pane) != Some(workspace)
        {
            return;
        }
        if providers.is_empty() {
            glib::spawn_future_local(async move {
                show_error_dialog(
                    &window,
                    "No chat agents available",
                    "Install or update a supported agent CLI on this machine to start a chat.",
                )
                .await;
            });
            return;
        }
        let dialog = adw::Dialog::new();
        dialog.set_title("Add Chat");
        dialog.set_follows_content_size(true);
        let fields = gtk::Box::new(gtk::Orientation::Vertical, 8);
        fields.set_margin_start(16);
        fields.set_margin_end(16);
        fields.set_margin_top(16);
        fields.set_margin_bottom(16);
        let title = gtk::Label::new(Some("Add Chat"));
        title.add_css_class("title-2");
        fields.append(&title);
        let status = gtk::Label::builder()
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(32)
            .build();
        fields.append(&status);
        let names: Vec<_> = providers.iter().map(|provider| provider.name()).collect();
        let agent = gtk::DropDown::from_strings(&names);
        agent.update_property(&[gtk::accessible::Property::Label("AI agent")]);
        fields.append(&agent);
        let path = gtk::Entry::new();
        path.set_width_chars(1);
        path.set_widget_name("flowmux-chat-directory");
        path.set_placeholder_text(Some("Working directory"));
        path.update_property(&[gtk::accessible::Property::Label("Working directory")]);
        if let Some(cwd) = &cwd {
            path.set_text(&cwd.to_string_lossy());
        }
        fields.append(&path);
        let options = gtk::Box::new(gtk::Orientation::Vertical, 8);
        fields.append(&options);
        let retry = gtk::Button::with_label("Retry loading settings");
        retry.set_visible(false);
        fields.append(&retry);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_homogeneous(true);
        let cancel = gtk::Button::with_label("Cancel");
        let start = gtk::Button::with_label("Start Chat");
        start.add_css_class("suggested-action");
        start.set_sensitive(false);
        actions.append(&cancel);
        actions.append(&start);
        fields.append(&actions);
        dialog.set_child(Some(
            &gtk::ScrolledWindow::builder()
                .child(&fields)
                .max_content_width(440)
                .max_content_height(530)
                .propagate_natural_width(true)
                .propagate_natural_height(true)
                .build(),
        ));
        let (answered, answers) = async_channel::bounded(1);
        let on_close = answered.clone();
        dialog.connect_closed(move |_| {
            let _ = on_close.try_send(false);
        });
        let weak = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                dialog.close();
            }
        });
        let weak = dialog.downgrade();
        start.connect_clicked(move |_| {
            if let Some(dialog) = weak.upgrade() {
                let _ = answered.try_send(true);
                dialog.close();
            }
        });
        let (changed, changes) = async_channel::bounded(1);
        let changed = {
            let start = start.downgrade();
            move || {
                if let Some(start) = start.upgrade() {
                    start.set_sensitive(false);
                }
                let _ = changed.try_send(());
            }
        };
        agent.connect_selected_notify({
            let changed = changed.clone();
            move |_| changed()
        });
        path.connect_changed({
            let changed = changed.clone();
            move |_| changed()
        });
        retry.connect_clicked(move |_| changed());

        let _native_browser_suspend =
            crate::ui::browser_pane::suspend_native_browser_views_for_window(window.upcast_ref());
        dialog.present(Some(&window));
        let response = answers.recv();
        tokio::pin!(response);
        'reload: loop {
            clear_options(&options);
            start.set_sensitive(false);
            status.set_visible(true);
            retry.set_visible(false);
            let selection = providers
                .get(agent.selected() as usize)
                .zip(chat_directory(path.text().as_str(), cwd.as_deref()));
            if let Some((&provider, directory)) = selection {
                status.set_label("Loading agent settings…");
                let load = async {
                    // Coalesce directory edits so typing does not start an agent for each prefix.
                    glib::timeout_future(std::time::Duration::from_millis(300)).await;
                    let (commands, events) =
                        chat_runtime::start(runtime, provider, directory.clone(), None);
                    match events.recv().await {
                        Ok(Event::Ready(catalog)) => Ok((commands, events, catalog)),
                        event => Err(setup_error(event.ok())),
                    }
                };
                let loaded = tokio::select! {
                    biased;
                    _ = changes.recv() => continue 'reload,
                    _ = &mut response => return,
                    loaded = load => loaded,
                };
                match loaded {
                    Ok((_commands, events, mut catalog)) => loop {
                        clear_options(&options);
                        let fields = ChatSetupOptions::new(&start, &status, &options, catalog);
                        tokio::select! {
                            biased;
                            _ = changes.recv() => continue 'reload,
                            answer = &mut response => {
                                if answer == Ok(true) && self.pane_in_visible_workspace(pane) {
                                    if let Some(chat) = self.add_chat_surface(
                                        pane, provider.name().into(), directory, None,
                                    ).await {
                                        chat.connect_started(fields.catalog.clone(), fields.settings());
                                    }
                                }
                                return;
                            },
                            event = events.recv() => match event {
                                Ok(Event::Ready(updated)) => catalog = updated,
                                event => {
                                    status.set_label(&setup_error(event.ok()));
                                    status.set_visible(true);
                                    break;
                                }
                            }
                        }
                    },
                    Err(error) => status.set_label(&error),
                }
                clear_options(&options);
                start.set_sensitive(false);
                retry.set_visible(true);
            } else {
                status.set_label("Choose an existing working directory.");
            }
            tokio::select! {
                _ = changes.recv() => {},
                _ = &mut response => return,
            }
        }
    }

    pub(super) async fn add_chat_surface(
        &self,
        pane: PaneId,
        agent: String,
        cwd: PathBuf,
        history: Option<SurfaceId>,
    ) -> Option<crate::ui::chat_pane::ChatPane> {
        if history.is_none() && !cwd.is_dir() {
            self.clipboard_toast
                .show_with_message("The chat working directory no longer exists.");
            return None;
        }
        let previous = self.pane_registry.borrow().active_surface(pane);
        let Some((workspace, surface)) = self
            .store
            .add_chat_surface_to_pane(pane, agent, cwd, history)
            .await
        else {
            self.clipboard_toast
                .show_with_message("The chat pane is no longer available.");
            return None;
        };
        if let Err(error) = self
            .attach_or_rerender_surface(workspace, pane, surface, previous)
            .await
        {
            self.store.close_surface(pane, surface).await;
            self.clipboard_toast.show_with_message(&error);
            return None;
        }
        self.pane_registry.borrow().chats.get(&surface).cloned()
    }
}

struct ChatSetupOptions {
    start: gtk::Button,
    catalog: Catalog,
    model: Picker,
    effort: Picker,
    effort_label: gtk::Label,
    access: Picker,
    access_detail: gtk::Label,
    confirm: gtk::CheckButton,
}

impl ChatSetupOptions {
    fn new(
        start: &gtk::Button,
        status: &gtk::Label,
        fields: &gtk::Box,
        catalog: Catalog,
    ) -> Rc<Self> {
        let model = picker("Model");
        let effort = picker("Reasoning effort");
        let access = picker("Access");
        fill(&model, &catalog.models, "Agent default", None);
        fill(&access, &catalog.access, "Agent default", None);
        let model_label = chat_field(fields, &model.widget, "Model");
        let effort_label = chat_field(fields, &effort.widget, "Reasoning effort");
        let access_label = chat_field(fields, &access.widget, "Access");
        model_label.set_visible(!catalog.models.choices.is_empty());
        model.widget.set_visible(model_label.get_visible());
        access_label.set_visible(!catalog.access.choices.is_empty());
        access.widget.set_visible(access_label.get_visible());
        let access_detail = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(30)
            .build();
        access_detail.add_css_class("dim-label");
        fields.append(&access_detail);
        let confirm = gtk::CheckButton::new();
        let caption = gtk::Label::builder()
            .label("Allow this access for this chat")
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .max_width_chars(28)
            .build();
        confirm.set_child(Some(&caption));
        confirm.update_property(&[gtk::accessible::Property::Label(
            "Allow this access for this chat",
        )]);
        fields.append(&confirm);
        let has_effort = !catalog
            .efforts_for(take_id(&model).as_deref())
            .choices
            .is_empty();
        status.set_label("This agent uses its configured defaults.");
        status
            .set_visible(!(model_label.get_visible() || access_label.get_visible() || has_effort));
        let this = Rc::new(Self {
            start: start.clone(),
            catalog,
            model,
            effort,
            effort_label,
            access,
            access_detail,
            confirm,
        });
        let weak = Rc::downgrade(&this);
        this.model.widget.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.update_effort();
            }
        });
        let weak = Rc::downgrade(&this);
        this.effort.widget.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.update_start();
            }
        });
        let weak = Rc::downgrade(&this);
        this.access.widget.connect_selected_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.update_access();
            }
        });
        let weak = Rc::downgrade(&this);
        this.confirm.connect_toggled(move |_| {
            if let Some(this) = weak.upgrade() {
                this.update_start();
            }
        });
        this.update_effort();
        this.update_access();
        this
    }

    fn update_effort(&self) {
        let menu = self.catalog.efforts_for(take_id(&self.model).as_deref());
        let required = !menu.choices.is_empty();
        fill(&self.effort, menu, "Agent default", None);
        if required {
            let labels = self
                .effort
                .widget
                .model()
                .and_downcast::<gtk::StringList>()
                .unwrap();
            let index = labels.n_items();
            labels.append("Choose reasoning effort");
            self.effort.widget.set_selected(index);
        }
        self.effort.widget.set_visible(required);
        self.effort_label.set_visible(required);
        self.update_start();
    }

    fn access_choice(&self) -> Option<&chat_runtime::Choice> {
        let id = take_id(&self.access)?;
        self.catalog
            .access
            .choices
            .iter()
            .find(|choice| choice.id == id)
    }

    fn update_access(&self) {
        let choice = self.access_choice();
        let description = choice.map_or("", |choice| choice.description.as_str());
        self.access_detail.set_label(description);
        self.access_detail.set_visible(!description.is_empty());
        self.confirm.set_active(false);
        self.confirm
            .set_visible(choice.is_some_and(chat_runtime::starts_with_elevated_access));
        self.update_start();
    }

    fn update_start(&self) {
        self.start.set_sensitive(
            (!self.effort.widget.get_visible() || take_id(&self.effort).is_some())
                && (!self.confirm.get_visible() || self.confirm.is_active()),
        );
    }

    fn settings(&self) -> Settings {
        Settings {
            model: take_id(&self.model),
            effort: take_id(&self.effort),
            access: take_id(&self.access),
            confirmed_access: self.confirm.get_visible() && self.confirm.is_active(),
            ..Settings::default()
        }
    }
}

fn clear_options(options: &gtk::Box) {
    while let Some(child) = options.first_child() {
        options.remove(&child);
    }
}

fn setup_error(event: Option<Event>) -> String {
    match event {
        Some(Event::Offline(error) | Event::Notice(error)) => error,
        _ => "The agent connection ended during setup.".into(),
    }
}

fn chat_field(fields: &gtk::Box, widget: &gtk::DropDown, title: &str) -> gtk::Label {
    let label = gtk::Label::builder().label(title).xalign(0.0).build();
    fields.append(&label);
    fields.append(widget);
    label
}

fn chat_directory(text: &str, initial: Option<&std::path::Path>) -> Option<PathBuf> {
    if let Some(initial) = initial.filter(|initial| initial.to_string_lossy() == text) {
        return initial.is_dir().then(|| initial.to_path_buf());
    }
    let path = std::path::Path::new(text);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else if !text.is_empty() {
        initial?.join(path)
    } else {
        return None;
    };
    path.canonicalize().ok().filter(|path| path.is_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_directory_resolves_paths_from_the_source_pane() {
        let root = tempfile::tempdir().unwrap();
        let subdir = root.path().join("한글 folder");
        std::fs::create_dir(&subdir).unwrap();
        assert_eq!(
            chat_directory("한글 folder", Some(root.path())),
            Some(subdir.canonicalize().unwrap())
        );
        assert_eq!(chat_directory("missing", Some(root.path())), None);
        assert_eq!(chat_directory("", Some(root.path())), None);
    }
}
