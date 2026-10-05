// SPDX-License-Identifier: GPL-3.0-or-later
//! User-level skill installation, sharing the CLI's paths and backup semantics.
use adw::prelude::*;
use flowmux_cli::agent::{self, DoctorStatus, InstallOutcome, SkillOverrides, Target};
use std::{cell::Cell, path::PathBuf, rc::Rc};

pub(super) fn build() -> gtk::ScrolledWindow {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(20);
    content.set_margin_end(20);

    let description = gtk::Label::new(Some(
        "Install the FlowMux user guide for the agents you use. It teaches workspace and terminal control, browser automation, SSH, notifications, and GUI features such as Code Review and search.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);
    content.append(&description);

    let group = adw::PreferencesGroup::new();
    group.set_title("FlowMux user guide");
    group.set_description(Some(
        "Choose an agent below. Updates back up existing content before replacing it.",
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
            row.run(false);
        }
    });
    content.append(&refresh);
    let note = gtk::Label::new(Some(
        "After installation, check your agent's skill list or start a new session. Existing sessions are not restarted. This installs the user guide only; agent hooks and settings are unchanged.",
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
    path: PathBuf,
    busy: Cell<bool>,
    update: Cell<bool>,
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
        let row = Rc::new(Self {
            widget,
            button,
            path,
            busy: Cell::new(false),
            update: Cell::new(false),
        });
        let weak = Rc::downgrade(&row);
        row.button.connect_clicked(move |_| {
            if let Some(row) = weak.upgrade() {
                row.run(true);
            }
        });
        row.run(false);
        row
    }

    fn run(self: &Rc<Self>, install: bool) {
        // Ignore repeat clicks or refreshes until this row's operation completes.
        if self.busy.replace(true) {
            return;
        }
        self.button.set_sensitive(false);
        self.button.set_label(if install {
            "Installing…"
        } else {
            "Checking…"
        });
        let row = self.clone();
        let path = self.path.clone();
        // A file created/changed after a Missing check must not be overwritten
        // by an Install click. Only an explicit Update permits replacement.
        let force = self.update.get();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let outcome = install.then(|| {
                    agent::install_one(&path, Target::payload(), force)
                        .map_err(|error| format!("{error:#}"))
                });
                (agent::doctor_one(&path, Target::payload()), outcome)
            })
            .await;
            row.busy.set(false);
            match result {
                Ok((status, outcome)) => {
                    row.show_status(&status);
                    match outcome {
                        Some(Ok(InstallOutcome::Updated { backup })) => row.widget.set_subtitle(
                            &format!("Installed · Previous version saved to {}", backup.display()),
                        ),
                        Some(Err(error)) => row
                            .widget
                            .set_subtitle(&format!("Installation failed: {error}")),
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
