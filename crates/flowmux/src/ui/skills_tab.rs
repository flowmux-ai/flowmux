// SPDX-License-Identifier: GPL-3.0-or-later
//! User-level skill installation, sharing the CLI's paths and backup semantics.
use adw::prelude::*;
use flowmux_cli::agent::{
    self, DoctorStatus, InstallOutcome, Skill, SkillOverrides, Target, UninstallOutcome,
};
use std::{cell::Cell, path::PathBuf, rc::Rc};

pub(super) fn build() -> gtk::ScrolledWindow {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(20);
    content.set_margin_end(20);

    let description = gtk::Label::new(Some(
        "Choose a skill, then install it for the agents you use. Updates and removal preserve modified files in backups.",
    ));
    description.set_wrap(true);
    description.set_xalign(0.0);
    content.append(&description);

    let mut rows = Vec::new();
    match agent::resolved_home() {
        Ok(home) => {
            let overrides = SkillOverrides::from_env();
            let codex_home = agent::resolved_codex_home();
            for &skill in Skill::ALL {
                let group = adw::PreferencesGroup::new();
                let (title, description) = match skill {
                    Skill::Browser => ("Flowmux CLI", "flowmux-browser · Workspaces, terminals, browser automation, SSH and notifications."),
                    Skill::Team => ("Flowmux Team", "flowmux-team · Work inline in ordinary workspaces, or delegate to interactive Claude Code and Codex split panes in Team workspaces. Requires Python 3.9+ and authenticated agent CLIs."),
                };
                group.set_title(title);
                group.set_description(Some(description));
                for &target in skill.targets() {
                    let path = skill.path(target, &home, codex_home.as_deref(), &overrides);
                    let row = SkillRow::new(skill, target, path);
                    group.add(&row.widget);
                    rows.push(row);
                }
                content.append(&group);
                content.append(&skill_preview(skill));
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
        "Start a new agent session to discover installed skills if needed. Existing sessions, agent hooks and settings are unchanged.",
    ));
    note.set_wrap(true);
    note.set_xalign(0.0);
    note.add_css_class("dim-label");
    content.append(&note);

    let clamp = adw::Clamp::builder()
        .maximum_size(800)
        .child(&content)
        .build();
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&clamp)
        .build()
}

fn skill_preview(skill: Skill) -> gtk::Expander {
    let manual = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    let prefix = widget_prefix(skill);
    manual.set_widget_name(&format!("{prefix}-contents"));
    manual.buffer().set_text(
        skill
            .files()
            .iter()
            .find(|(name, _)| *name == "SKILL.md")
            .unwrap()
            .1,
    );
    let manual_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(220)
        .child(&manual)
        .build();
    let preview = gtk::Expander::new(Some("View skill instructions"));
    preview.set_widget_name(&format!("{prefix}-preview"));
    preview.set_child(Some(&manual_scroll));
    preview
}

fn widget_prefix(skill: Skill) -> &'static str {
    match skill {
        Skill::Browser => "flowmux-skill",
        Skill::Team => "flowmux-team",
    }
}

struct SkillRow {
    widget: adw::ActionRow,
    button: gtk::Button,
    remove: gtk::Button,
    path: PathBuf,
    target: Target,
    skill: Skill,
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
    fn new(skill: Skill, target: Target, path: PathBuf) -> Rc<Self> {
        let title = match target {
            Target::ClaudeCode => "Claude Code",
            Target::Codex => "Codex",
            Target::OpenCode => "OpenCode",
            Target::Antigravity => "Antigravity",
            Target::Cline => "Cline",
        };
        let prefix = widget_prefix(skill);
        let widget = adw::ActionRow::builder().title(title).build();
        widget.set_use_markup(false);
        widget.set_widget_name(&format!("{prefix}-{}", target.slug()));
        widget.set_tooltip_text(Some(&path.display().to_string()));
        let button = gtk::Button::with_label("Checking…");
        button.set_valign(gtk::Align::Center);
        button.set_widget_name(&format!("{prefix}-install-{}", target.slug()));
        button.set_sensitive(false);
        widget.add_suffix(&button);
        let remove = gtk::Button::with_label("Remove");
        remove.set_valign(gtk::Align::Center);
        remove.add_css_class("destructive-action");
        remove.set_widget_name(&format!("{prefix}-remove-{}", target.slug()));
        remove.set_tooltip_text(Some(
            "Remove only the Flowmux skill. Modified content is backed up.",
        ));
        remove.set_sensitive(false);
        widget.add_suffix(&remove);
        let row = Rc::new(Self {
            widget,
            button,
            remove,
            path,
            target,
            skill,
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
        let target = self.target;
        let skill = self.skill;
        // A file created/changed after a Missing check must not be overwritten
        // by an Install click. Only an explicit Update permits replacement.
        let force = self.update.get();
        gtk::glib::spawn_future_local(async move {
            let result = gtk::gio::spawn_blocking(move || {
                let outcome = match action {
                    Action::Refresh => Ok(None),
                    Action::Install => skill.install(&path, force).map(|outcome| match outcome {
                        InstallOutcome::Updated { backup } => Some(format!(
                            "Installed · Previous version saved to {}",
                            backup.display()
                        )),
                        _ => None,
                    }),
                    Action::Remove => skill.uninstall(&path).map(|outcome| match outcome {
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
                let linked_directory = path.parent().is_some_and(|parent| parent.is_symlink());
                let duplicates = if target == Target::Codex {
                    agent::resolved_home()
                        .map(|home| {
                            skill.codex_unmanaged_paths(
                                &home,
                                agent::resolved_codex_home().as_deref(),
                            )
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let removable = skill.removable(&path);
                (
                    skill.doctor(&path),
                    outcome,
                    removable,
                    linked_directory,
                    duplicates,
                )
            })
            .await;
            row.busy.set(false);
            row.remove.set_label("Remove");
            match result {
                Ok((status, outcome, removable, linked_directory, duplicates)) => {
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
                    let mut details = row.widget.subtitle().unwrap_or_default().to_string();
                    if linked_directory {
                        details.push_str(
                            "\nManaged through a linked folder; change or remove it at its source.",
                        );
                    }
                    for duplicate in duplicates {
                        details.push_str(&format!(
                            "\nAnother copy remains at {}. It may still appear in your agent after removal here.",
                            duplicate.display()
                        ));
                    }
                    row.widget.set_subtitle(&details);
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
