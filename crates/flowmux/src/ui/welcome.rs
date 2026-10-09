// SPDX-License-Identifier: GPL-3.0-or-later
//! First-launch tour; screenshots are embedded so packaged installs work offline.
use adw::prelude::*;
use flowmux_config::keybindings::ActionId;
use std::{cell::Cell, path::Path, rc::Rc};

struct Page {
    title: &'static str,
    description: &'static str,
    screenshot: &'static [u8],
    actions: &'static [ActionId],
}

const PAGES: [Page; 6] = [
    Page {
        title: "Agent completion",
        description:
            "See when agents finish in other workspaces. Look for Done in the left sidebar.",
        screenshot: include_bytes!("../../../../resources/welcome/notifications.png"),
        actions: &[],
    },
    Page {
        title: "Split panes",
        description: "Split right or down to work in several terminals side by side.",
        screenshot: include_bytes!("../../../../resources/welcome/split.png"),
        actions: &[ActionId::SplitRight, ActionId::SplitDown],
    },
    Page {
        title: "Agent sessions",
        description: "Open Sessions to find and resume a saved conversation.",
        screenshot: include_bytes!("../../../../resources/welcome/sessions.png"),
        actions: &[ActionId::ToggleSessionPanel],
    },
    Page {
        title: "Search",
        description: "Search your terminals, then select a result to jump to it.",
        screenshot: include_bytes!("../../../../resources/welcome/search.png"),
        actions: &[ActionId::SearchAllTerminals, ActionId::TerminalSearch],
    },
    Page {
        title: "Files and editor",
        description: "Open Files to browse your project. Double-click a file to edit it.",
        screenshot: include_bytes!("../../../../resources/welcome/files.png"),
        actions: &[ActionId::ToggleFileBrowser],
    },
    Page {
        title: "Workspace overview",
        description: "See all workspaces and agent activity. Select a workspace to open it.",
        screenshot: include_bytes!("../../../../resources/welcome/overview.png"),
        actions: &[ActionId::ToggleWorkspaceOverview],
    },
];

fn shortcut_text(actions: &[ActionId]) -> String {
    actions
        .iter()
        .filter_map(|&action| {
            let labels = crate::keybindings::action_accel_labels(action);
            (!labels.is_empty())
                .then(|| crate::keybindings::tooltip_with_accels(action.label(), &labels))
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

// Claim before presenting so concurrent windows do not each show the first-run tour.
fn claim_first_launch(marker: &Path, force: bool) -> std::io::Result<bool> {
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)
    {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(force),
        Err(error) => Err(error),
    }
}

pub(crate) fn present_if_needed(parent: &adw::ApplicationWindow, force: bool) {
    let show = flowmux_config::paths::state_dir()
        .ok_or_else(|| std::io::Error::other("state directory unavailable"))
        .and_then(|dir| claim_first_launch(&dir.join("welcome-seen"), force));
    match show {
        Ok(false) => return,
        Err(error) => tracing::warn!(%error, "could not persist welcome tour state"),
        Ok(true) => {}
    }
    build_dialog().present(Some(parent));
}

fn build_dialog() -> adw::Dialog {
    let dialog = adw::Dialog::builder()
        .title("Welcome to Flowmux")
        .content_width(880)
        .content_height(650)
        .build();
    dialog.set_widget_name("flowmux-welcome-dialog");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 16);
    content.append(&adw::HeaderBar::new());
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.set_transition_type(gtk::StackTransitionType::SlideLeftRight);
    for (index, info) in PAGES.iter().enumerate() {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.set_margin_start(24);
        page.set_margin_end(24);
        let texture =
            gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_static(info.screenshot))
                .expect("bundled welcome screenshot must decode");
        let picture = gtk::Picture::for_paintable(&texture);
        picture.set_can_shrink(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_vexpand(true);
        picture.set_alternative_text(Some(info.title));
        page.append(&picture);
        let heading = gtk::Label::new(Some(info.title));
        heading.add_css_class("title-1");
        heading.set_wrap(true);
        page.append(&heading);
        let body = gtk::Label::new(Some(info.description));
        body.set_wrap(true);
        body.set_max_width_chars(85);
        page.append(&body);
        let shortcuts = shortcut_text(info.actions);
        if !shortcuts.is_empty() {
            let label = gtk::Label::new(Some(&shortcuts));
            label.set_wrap(true);
            label.set_max_width_chars(85);
            page.append(&label);
        }
        stack.add_named(&page, Some(&index.to_string()));
    }
    content.append(&stack);
    let controls = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    controls.set_margin_start(24);
    controls.set_margin_end(24);
    controls.set_margin_bottom(24);
    let back = gtk::Button::with_label("Back");
    back.set_sensitive(false);
    let position = gtk::Label::new(Some("1 / 6"));
    position.set_hexpand(true);
    let next = gtk::Button::with_label("Next");
    next.add_css_class("suggested-action");
    controls.append(&back);
    controls.append(&position);
    controls.append(&next);
    content.append(&controls);
    let current = Rc::new(Cell::new(0usize));
    for (button, forward) in [(&back, false), (&next, true)] {
        let (current, stack, back, next, position, dialog) = (
            current.clone(),
            stack.clone(),
            back.downgrade(),
            next.downgrade(),
            position.clone(),
            dialog.downgrade(),
        );
        button.connect_clicked(move |_| {
            let (Some(back), Some(next)) = (back.upgrade(), next.upgrade()) else {
                return;
            };
            let index = current.get();
            if forward && index == PAGES.len() - 1 {
                if let Some(dialog) = dialog.upgrade() {
                    dialog.close();
                }
                return;
            }
            let index = if forward {
                index + 1
            } else {
                index.saturating_sub(1)
            };
            current.set(index);
            stack.set_visible_child_name(&index.to_string());
            back.set_sensitive(index > 0);
            next.set_label(if index == PAGES.len() - 1 {
                "Get started"
            } else {
                "Next"
            });
            position.set_text(&format!("{} / {}", index + 1, PAGES.len()));
        });
    }
    dialog.set_child(Some(&content));
    dialog
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn tour_navigation_reaches_every_page_and_returns() {
        adw::init().unwrap();
        let dialog = build_dialog();
        let content = dialog.child().unwrap();
        let stack = content
            .first_child()
            .unwrap()
            .next_sibling()
            .unwrap()
            .downcast::<gtk::Stack>()
            .unwrap();
        let controls = content.last_child().unwrap();
        let back = controls
            .first_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        let next = controls
            .last_child()
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap();
        assert!(!back.is_sensitive());
        for index in 1..PAGES.len() {
            next.emit_clicked();
            assert_eq!(
                stack.visible_child_name().as_deref(),
                Some(index.to_string().as_str())
            );
        }
        assert_eq!(next.label().as_deref(), Some("Get started"));
        for _ in 1..PAGES.len() {
            back.emit_clicked();
        }
        assert!(!back.is_sensitive());
        assert_eq!(next.label().as_deref(), Some("Next"));
        assert_eq!(stack.visible_child_name().as_deref(), Some("0"));
    }

    #[cfg(not(target_os = "macos"))]
    #[gtk::test]
    fn shortcuts_follow_platform_defaults_overrides_and_unbinding() {
        use crate::keybindings::{accelerator_label, install_accels};
        use flowmux_config::{keybindings::default_accels, options::Options};

        struct RestoreDefault(Option<gtk::gio::Application>);
        impl Drop for RestoreDefault {
            fn drop(&mut self) {
                if let Some(app) = &self.0 {
                    app.set_default();
                } else {
                    // The safe binding cannot restore a null default application.
                    unsafe { gtk::gio::ffi::g_application_set_default(std::ptr::null_mut()) };
                }
            }
        }
        let previous = gtk::gio::Application::default();
        let restore = RestoreDefault(previous.clone());
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("com.flowmux.App.UiTest.WelcomeShortcuts")
            .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gtk::gio::Cancellable>).unwrap();
        app.set_default();
        let mut options = Options::default();
        install_accels(&app, &options);
        for page in &PAGES {
            let text = shortcut_text(page.actions);
            for &action in page.actions {
                for accel in default_accels(action) {
                    assert!(text.contains(&accelerator_label(accel).unwrap()), "{text}");
                }
            }
        }
        options
            .keybindings
            .set(ActionId::SplitRight, vec!["<Alt>r".into(), "F6".into()]);
        options.keybindings.set(ActionId::SplitDown, vec![]);
        install_accels(&app, &options);
        assert_eq!(
            shortcut_text(PAGES[1].actions),
            format!(
                "Split pane right ({}, {})",
                accelerator_label("<Alt>r").unwrap(),
                accelerator_label("F6").unwrap()
            ),
        );
        options.keybindings.set(ActionId::SplitRight, vec![]);
        install_accels(&app, &options);
        assert!(shortcut_text(PAGES[1].actions).is_empty());
        assert!(shortcut_text(PAGES[0].actions).is_empty());
        drop(restore);
        assert_eq!(gtk::gio::Application::default(), previous);
    }

    #[test]
    fn tour_is_claimed_once_and_can_be_forced() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("state/welcome-seen");
        assert!(claim_first_launch(&marker, false).unwrap());
        assert!(!claim_first_launch(&marker, false).unwrap());
        assert!(claim_first_launch(&marker, true).unwrap());
        assert!(!claim_first_launch(&marker, false).unwrap());
        assert!(claim_first_launch(&marker.join("invalid"), false).is_err());
    }

    #[test]
    fn all_six_screenshots_decode() {
        for page in PAGES {
            let image = image::load_from_memory(page.screenshot).expect(page.title);
            assert!(
                image.width() >= 800 && image.height() >= 500,
                "{}",
                page.title
            );
        }
    }
}
