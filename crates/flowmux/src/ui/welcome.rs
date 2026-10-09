// SPDX-License-Identifier: GPL-3.0-or-later
//! First-launch tour; screenshots are embedded so packaged installs work offline.
use adw::prelude::*;
use std::{cell::Cell, path::Path, rc::Rc};

const PAGES: [(&str, &str, &[u8]); 6] = [
    ("Stay on top of notifications", "The bell collects task updates and approval requests. Select a notification to jump to its pane. Enable desktop notifications in Options → General; check agent hooks in Options → Update → Integration status.", include_bytes!("../../../../resources/welcome/notifications.png")),
    ("Split your workspace", "Use the pane menu to split right or down and work in several terminals at once. Default shortcuts: Ctrl+Shift+Page Up / Page Down (Command instead of Ctrl on macOS). Each pane can hold several tabs.", include_bytes!("../../../../resources/welcome/split.png")),
    ("Return to an agent session", "Open Sessions to browse saved conversations for the agent in the focused terminal. Search the list, preview a conversation, then resume it. Default shortcut: Ctrl+Alt+J (Command+Option+J on macOS).", include_bytes!("../../../../resources/welcome/sessions.png")),
    ("Find what you need", "Use the magnifier beside Files to search all open terminals, including SSH. Select a result to jump to it. Ctrl+Shift+F searches the focused terminal; editor focus searches editor content. On macOS, use Command instead of Ctrl.", include_bytes!("../../../../resources/welcome/search.png")),
    ("Browse and edit files", "Open Files to browse the focused pane’s local project. Double-click a file to open an editor tab, then save with Ctrl+S (Command+S on macOS). The file list follows the selected project.", include_bytes!("../../../../resources/welcome/files.png")),
    ("See every workspace", "Open workspace overview to see your workspaces and agent activity together. Select a workspace to return to it. Default shortcut: Ctrl+Alt+K (Command+Option+K on macOS). Customize shortcuts in Options → Keybindings.", include_bytes!("../../../../resources/welcome/overview.png")),
];

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
    for (index, (title, description, bytes)) in PAGES.iter().enumerate() {
        let page = gtk::Box::new(gtk::Orientation::Vertical, 12);
        page.set_margin_start(24);
        page.set_margin_end(24);
        let texture = gtk::gdk::Texture::from_bytes(&gtk::glib::Bytes::from_static(bytes))
            .expect("bundled welcome screenshot must decode");
        let picture = gtk::Picture::for_paintable(&texture);
        picture.set_can_shrink(true);
        picture.set_content_fit(gtk::ContentFit::Contain);
        picture.set_vexpand(true);
        picture.set_alternative_text(Some(title));
        page.append(&picture);
        let heading = gtk::Label::new(Some(title));
        heading.add_css_class("title-1");
        heading.set_wrap(true);
        page.append(&heading);
        let body = gtk::Label::new(Some(description));
        body.set_wrap(true);
        body.set_max_width_chars(85);
        page.append(&body);
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
        for (title, _, bytes) in PAGES {
            let image = image::load_from_memory(bytes).expect(title);
            assert!(image.width() >= 800 && image.height() >= 500, "{title}");
        }
    }
}
