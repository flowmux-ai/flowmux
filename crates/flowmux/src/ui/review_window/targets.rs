// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::bridge::{Bridge, GtkCommand};
use flowmux_core::{AgentPresence, SurfaceId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ReviewTarget {
    pub surface: SurfaceId,
    pub name: String,
    pub pid: Option<u32>,
    pub session: Option<String>,
    pub label: String,
}
impl ReviewTarget {
    pub fn matches(&self, presence: &AgentPresence) -> bool {
        self.name == presence.name
            && self.pid == presence.pid
            && self.session == presence.session_id
    }
}
pub(super) struct Targets {
    pub menu: gtk::MenuButton,
    list: gtk::Box,
    bridge: RefCell<Option<Bridge>>,
    items: RefCell<Option<Vec<ReviewTarget>>>,
}
impl Targets {
    pub fn new() -> Self {
        let menu = gtk::MenuButton::builder().label("Send · 0").build();
        menu.add_css_class("suggested-action");
        menu.set_tooltip_text(Some("Send feedback"));
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        margins(&list, 8);
        let popover = gtk::Popover::new();
        popover.set_child(Some(&list));
        menu.set_popover(Some(&popover));
        Self {
            menu,
            list,
            bridge: RefCell::new(None),
            items: RefCell::new(None),
        }
    }
    pub fn send(&self, command: GtkCommand) -> bool {
        self.bridge
            .borrow()
            .as_ref()
            .is_some_and(|b| b.tx.try_send(command).is_ok())
    }
}
impl ReviewWindow {
    #[cfg(all(test, target_os = "macos"))]
    pub(crate) fn smoke_activate_target(&self, surface: SurfaceId) {
        let index = self
            .targets
            .items
            .borrow()
            .as_ref()
            .unwrap()
            .iter()
            .position(|target| target.surface == surface)
            .unwrap();
        self.targets.menu.popup();
        self.targets
            .list
            .observe_children()
            .item(index as u32 + 1)
            .unwrap()
            .downcast::<gtk::Button>()
            .unwrap()
            .emit_clicked();
        assert!(!self.targets.menu.popover().unwrap().is_visible());
    }

    pub fn connect_targets(self: &Rc<Self>, bridge: Bridge) {
        *self.targets.bridge.borrow_mut() = Some(bridge);
        let weak = Rc::downgrade(self);
        self.targets
            .menu
            .popover()
            .unwrap()
            .connect_closed(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.targets.send(GtkCommand::RefreshReviewTargets);
                }
            });
        self.set_targets(Vec::new());
    }
    pub fn set_targets(self: &Rc<Self>, targets: Vec<ReviewTarget>) {
        // Replacing a live row destroys its focus/pressed state. Keep this
        // menu stable until it closes; delivery revalidates the chosen agent.
        if self.targets.menu.popover().unwrap().is_visible() {
            return;
        }
        if self.targets.items.borrow().as_ref() == Some(&targets) {
            return;
        }
        *self.targets.items.borrow_mut() = Some(targets.clone());
        while let Some(child) = self.targets.list.first_child() {
            self.targets.list.remove(&child);
        }
        let label = gtk::Label::new(Some(if targets.is_empty() {
            "No agent in this workspace"
        } else {
            "Send to agent"
        }));
        label.add_css_class("caption");
        self.targets.list.append(&label);
        for target in targets {
            let button = gtk::Button::new();
            button.set_child(Some(
                &gtk::Label::builder()
                    .label(&target.label)
                    .xalign(0.0)
                    .max_width_chars(32)
                    .ellipsize(pango::EllipsizeMode::End)
                    .build(),
            ));
            button.set_tooltip_text(Some(&target.label));
            let weak = Rc::downgrade(self);
            button.connect_clicked(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.send_review(Some(target.clone()));
                }
            });
            self.targets.list.append(&button);
        }
        let copy = gtk::Button::with_label("Copy feedback");
        let weak = Rc::downgrade(self);
        copy.connect_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.send_review(None);
            }
        });
        self.targets.list.append(&copy);
    }
}

#[cfg(test)]
pub(super) async fn smoke(review: &Rc<ReviewWindow>) {
    let initial = review.targets.items.borrow().clone().unwrap_or_default();
    let target = ReviewTarget {
        surface: SurfaceId::new(),
        name: "fixture".into(),
        pid: None,
        session: Some("popup-test".into()),
        label: "fixture · Working (1s)".into(),
    };
    review.set_targets(vec![target.clone()]);
    review.targets.menu.popup();
    glib::timeout_future(std::time::Duration::from_millis(50)).await;
    let button = review
        .targets
        .list
        .first_child()
        .unwrap()
        .next_sibling()
        .unwrap();
    assert!(button.grab_focus());
    assert_eq!(button.root().unwrap().focus().as_ref(), Some(&button));
    let mut updated = target;
    updated.label = "fixture · Working (2s)".into();
    review.set_targets(vec![updated.clone()]);
    assert!(
        button.parent().is_some(),
        "live target refresh detached the focused Send button"
    );
    assert_eq!(
        button.root().unwrap().focus().as_ref(),
        Some(&button),
        "live target refresh stole Send focus"
    );
    review.targets.menu.popdown();
    review.set_targets(vec![updated.clone()]);
    assert_eq!(review.targets.items.borrow().as_ref(), Some(&vec![updated]));
    review.set_targets(initial);
    println!("DIFF_REVIEW_SEND_REFRESH_FOCUS_OK");
}
