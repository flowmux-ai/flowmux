// SPDX-License-Identifier: GPL-3.0-or-later
//! User-level skill installation, sharing the CLI's paths and backup semantics.
use adw::prelude::*;
use flowmux_cli::agent::{
    self, DoctorStatus, InstallOutcome, SkillOverrides, Target, UninstallOutcome,
};
use std::{cell::Cell, path::PathBuf, rc::Rc};

pub(super) fn build() -> gtk::ScrolledWindow {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(20);
    content.set_margin_end(20);

    let description = gtk::Label::new(Some(
        "Install the FlowMux CLI skill for the agents you use. It teaches workspace and terminal control, browser automation, SSH, notifications, and GUI features such as Code Review and search.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);
    content.append(&description);

    let group = adw::PreferencesGroup::new();
    group.set_title("FlowMux CLI");
    group.set_description(Some(
        "Choose an agent below. Updates and removal back up modified content.",
    ));
    content.append(&group);
    let mut rows = Vec::new();
    match agent::resolved_home() {
        Ok(home) => {
            let overrides = SkillOverrides::from_env();
            let codex_home = agent::resolved_codex_home();
            for &target in Target::ALL {
                let path = overrides.path(target, &home, codex_home.as_deref());
                let row = SkillRow::new(target, path);
                group.add(&row.widget);
                rows.push(row);
            }
        }
        Err(error) => {
            let label = gtk::Label::new(Some(&error.to_string()));
            label.set_wrap(true);
            content.append(&label);
        }
    }
    let refresh = gtk::Button::with_label("Refresh status");
    refresh.set_widget_name("flowmux-skills-refresh");
    refresh.set_halign(gtk::Align::Start);
    refresh.connect_clicked(move |_| {
        for row in &rows {
            row.run(Action::Refresh);
        }
    });
    content.append(&refresh);
    let note = gtk::Label::new(Some(
        "After installation, check your agent's skill list or start a new session. Existing sessions are not restarted. Only the FlowMux CLI skill is managed here; agent hooks and settings are unchanged.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    content.append(&note);

    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&content)
        .build()
}

struct SkillRow {
    widget: adw::ActionRow,
    button: gtk::Button,
    remove: gtk::Button,
    path: PathBuf,
    busy: Cell<bool>,
    update: Cell<bool>,
}

#[derive(Clone, Copy)]
enum Action {
    Refresh,
    Install,
    Remove,
}

impl SkillRow {
    fn new(target: Target, path: PathBuf) -> Rc<Self> {
        let title = match target {
            Target::ClaudeCode => "Claude Code",
            Target::Codex => "Codex",
            Target::OpenCode => "OpenCode",
            Target::Antigravity => "Antigravity",
            Target::Cline => "Cline",
        };
        let widget = adw::ActionRow::builder().title(title).build();
        widget.set_use_markup(false);
        widget.set_widget_name(&format!("flowmux-skill-{}", target.slug()));
        widget.set_tooltip_text(Some(&path.display().to_string()));
        let button = gtk::Button::with_label("Checking…");
        button.set_valign(gtk::Align::Center);
        button.set_widget_name(&format!("flowmux-skill-install-{}", target.slug()));
        button.set_sensitive(false);
        widget.add_suffix(&button);
        let remove = gtk::Button::with_label("Remove");
        remove.set_valign(gtk::Align::Center);
        remove.add_css_class("destructive-action");
        remove.set_widget_name(&format!("flowmux-skill-remove-{}", target.slug()));
        remove.set_tooltip_text(Some(
            "Remove only the FlowMux skill. Modified content is backed up.",
        ));
        remove.set_sensitive(false);
        widget.add_suffix(&remove);
        let row = Rc::new(Self {
            widget,
            button,
            remove,
            path,
            busy: Cell::new(false),
            update: Cell::new(false),
        });
        let weak = Rc::downgrade(&row);
        row.button.connect_clicked(move |_| {
            if let Some(row) = weak.upgrade() {
                row.run(Action::Install);
            }
        });
        let weak = Rc::downgrade(&row);
        row.remove.connect_clicked(move |_| {
            if let Some(row) = weak.upgrade() {
                row.run(Action::Remove);
            }
        });
        row.run(Action::Refresh);
        row
    }

    fn run(self: &Rc<Self>, action: Action) {
        // Ignore repeat clicks or refreshes until this row's operation completes.
        if self.busy.replace(true) {
            return;
        }
        self.button.set_sensitive(false);
        self.remove.set_sensitive(false);
        match action {
            Action::Install => self.button.set_label("Installing…"),
            Action::Refresh => self.button.set_label("Checking…"),
            Action::Remove => self.remove.set_label("Removing…"),
        }
        let row = self.clone();
        let path = self.path.clone();
        // A file created/changed after a Missing check must not be overwritten
        // by an Install click. Only an explicit Update permits replacement.
        let force = self.update.get();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let outcome = match action {
                    Action::Refresh => Ok(None),
                    Action::Install => {
                        agent::install_one(&path, Target::payload(), force).map(|outcome| {
                            match outcome {
                                InstallOutcome::Updated { backup } => Some(format!(
                                    "Installed · Previous version saved to {}",
                                    backup.display()
                                )),
                                _ => None,
                            }
                        })
                    }
                    Action::Remove => agent::uninstall_one(&path).map(|outcome| match outcome {
                        UninstallOutcome::Preserved { backup } => Some(format!(
                            "Removed · Modified content saved to {}",
                            backup.display()
                        )),
                        _ => Some("Removed · Agent hooks and settings are unchanged".into()),
                    }),
                }
                .map_err(|error| format!("{error:#}"));
                // A file symlink can be unlinked without touching its target;
                // linked directories and non-file entries remain user-managed.
                let removable = path
                    .symlink_metadata()
                    .is_ok_and(|meta| meta.is_file() || meta.file_type().is_symlink())
                    && !path.parent().is_some_and(|parent| parent.is_symlink());
                (
                    agent::doctor_one(&path, Target::payload()),
                    outcome,
                    removable,
                )
            })
            .await;
            row.busy.set(false);
            row.remove.set_label("Remove");
            match result {
                Ok((status, outcome, removable)) => {
                    row.show_status(&status);
                    row.remove.set_sensitive(removable);
                    match outcome {
                        Ok(Some(message)) => row.widget.set_subtitle(&message),
                        Err(error) => {
                            let operation = match action {
                                Action::Remove => "Removal",
                                _ => "Installation",
                            };
                            row.widget
                                .set_subtitle(&format!("{operation} failed: {error}"));
                        }
                        _ => {}
                    }
                }
                Err(_) => {
                    row.widget
                        .set_subtitle("Could not check installation. Use Refresh status to retry.");
                    row.button.set_label("Unavailable");
                }
            }
        });
    }

    fn show_status(&self, status: &DoctorStatus) {
        self.update.set(matches!(status, DoctorStatus::Drift));
        let (subtitle, button, enabled) = match status {
            DoctorStatus::Ok => ("Installed and up to date", "Installed", false),
            DoctorStatus::Missing => ("Not installed", "Install", true),
            DoctorStatus::Drift => (
                "Different or older version · Update keeps a backup",
                "Update",
                true,
            ),
            DoctorStatus::Error(error) => {
                self.widget
                    .set_subtitle(&format!("Cannot manage this skill: {error}"));
                self.button.set_label("Unavailable");
                self.button.set_sensitive(false);
                return;
            }
        };
        self.widget.set_subtitle(subtitle);
        self.button.set_label(button);
        self.button.set_sensitive(enabled);
    }
}
