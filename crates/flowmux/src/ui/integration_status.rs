// SPDX-License-Identifier: GPL-3.0-or-later
//! The same read-only integration report as `flowmux doctor`.
use adw::prelude::*;
use flowmux_cli::{agent, doctor};

pub(super) fn build() -> gtk::Box {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    let group = adw::PreferencesGroup::builder()
        .title("Integration status")
        .description("Check agent skills and hooks, the running Flowmux connection, browser data, and desktop installation. CLI: flowmux doctor or flowmux --json doctor.")
        .build();
    let refresh = gtk::Button::with_label("Run diagnostics");
    refresh.set_widget_name("flowmux-diagnostics-refresh");
    group.set_header_suffix(Some(&refresh));
    content.append(&group);
    let results = gtk::Box::new(gtk::Orientation::Vertical, 12);
    results.set_widget_name("flowmux-diagnostics-results");
    content.append(&results);
    refresh.connect_clicked(move |button| {
        button.set_sensitive(false);
        button.set_label("Checking…");
        let button = button.clone();
        let results = results.clone();
        gtk::glib::spawn_future_local(async move {
            // Filesystem probes and socket timeouts must not block GTK.
            let report = gtk::gio::spawn_blocking(|| -> anyhow::Result<doctor::Report> {
                let home = agent::resolved_home()?;
                let codex_home = agent::resolved_codex_home();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    Ok(doctor::collect(
                        &home,
                        codex_home.as_deref(),
                        Some(flowmux_config::paths::runtime_socket_for_pid(
                            std::process::id(),
                        )),
                    )
                    .await)
                })
            })
            .await;
            while let Some(child) = results.first_child() {
                results.remove(&child);
            }
            match report {
                Ok(Ok(report)) => render(&results, &report),
                result => {
                    let message = match result {
                        Ok(Err(error)) => format!("Could not run diagnostics: {error:#}"),
                        _ => "Diagnostics worker stopped unexpectedly. Try again.".into(),
                    };
                    let label = gtk::Label::new(Some(&message));
                    label.set_wrap(true);
                    results.append(&label);
                }
            }
            button.set_label("Refresh diagnostics");
            button.set_sensitive(true);
        });
    });
    content
}

fn render(content: &gtk::Box, report: &doctor::Report) {
    for section in &report.sections {
        let group = adw::PreferencesGroup::builder()
            .title(&section.title)
            .build();
        for entry in &section.entries {
            let row = adw::ActionRow::builder()
                .title(&entry.name)
                .subtitle(&entry.detail)
                .build();
            row.set_use_markup(false);
            row.set_subtitle_selectable(true);
            let status = gtk::Label::new(Some(entry.status.label()));
            status.add_css_class(match entry.status {
                doctor::Status::Ok => "success",
                doctor::Status::Info => "dim-label",
                doctor::Status::Warn => "warning",
                doctor::Status::NeedsFix | doctor::Status::Error => "error",
            });
            row.add_suffix(&status);
            group.add(&row);
        }
        content.append(&group);
    }
    let hint = gtk::Label::new(Some(
        "Read-only check. Run flowmux fix to repair rows tagged fix. Review error details for manual steps. Skill installation is also available in Options → Skills.",
    ));
    hint.set_wrap(true);
    hint.set_xalign(0.0);
    content.append(&hint);
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::*;

    #[gtk::test]
    fn report_renders_every_status_and_literal_paths() {
        adw::init().unwrap();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let report = doctor::Report {
            sections: vec![doctor::Section {
                title: "Test integrations".into(),
                entries: [
                    doctor::Status::Ok,
                    doctor::Status::Info,
                    doctor::Status::Warn,
                    doctor::Status::NeedsFix,
                    doctor::Status::Error,
                ]
                .into_iter()
                .map(|status| doctor::Entry {
                    name: format!("{} check", status.label()),
                    status,
                    detail: "/home/<example>/a&b".into(),
                })
                .collect(),
            }],
        };
        render(&content, &report);
        fn labels(widget: &gtk::Widget, text: &mut Vec<String>) {
            if let Some(label) = widget.downcast_ref::<gtk::Label>() {
                text.push(label.text().to_string());
            }
            let mut child = widget.first_child();
            while let Some(widget) = child {
                labels(&widget, text);
                child = widget.next_sibling();
            }
        }
        let mut text = Vec::new();
        labels(content.upcast_ref(), &mut text);
        for status in ["ok", "info", "warn", "fix", "error"] {
            assert!(text.iter().any(|label| label == status), "missing {status}");
        }
        assert_eq!(
            text.iter()
                .filter(|label| *label == "/home/<example>/a&b")
                .count(),
            5
        );
    }
}
